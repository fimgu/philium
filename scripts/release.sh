#!/bin/sh
# Build and publish a Philium release with in-app update support.
#
#   sh scripts/release.sh 0.1.1 ["release notes"]
#
# Bumps the version, refreshes HAL, builds the DMG plus the signed update bundle,
# writes latest.json and publishes a GitHub release. Installed copies find the update
# through https://github.com/fimgu/philium/releases/latest/download/latest.json.
#
# Signing key: ~/.tauri/philium-updater.key (or TAURI_SIGNING_PRIVATE_KEY). Back it up.
# Losing it means installed copies can no longer verify, and so never get, new updates.
set -eu
VERSION="${1:?usage: release.sh <version> [notes]}"
NOTES="${2:-Philium $VERSION}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO="fimgu/philium"
cd "$ROOT"

if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
  KEY="${PHILIUM_UPDATER_KEY:-$HOME/.tauri/philium-updater.key}"
  [ -f "$KEY" ] || { echo "missing updater signing key $KEY" >&2; exit 1; }
  TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY")"
fi
export TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"

[ -z "$(git status --porcelain)" ] || { echo "working tree is not clean" >&2; exit 1; }
git fetch -q origin main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || { echo "HEAD is not origin/main" >&2; exit 1; }
gh release view "v$VERSION" -R "$REPO" >/dev/null 2>&1 && { echo "v$VERSION already exists" >&2; exit 1; }

# Version lives in tauri.conf.json, Cargo.toml and package.json.
node -e '
const fs=require("fs"),v=process.argv[1];
for (const f of ["src-tauri/tauri.conf.json","package.json"]) { const j=JSON.parse(fs.readFileSync(f)); j.version=v; fs.writeFileSync(f, JSON.stringify(j,null,2)+"\n"); }
const t="src-tauri/Cargo.toml"; fs.writeFileSync(t, fs.readFileSync(t,"utf8").replace(/^version = ".*"$/m, `version = "${v}"`));
' "$VERSION"
(cd src-tauri && cargo update -q -p philium --offline 2>/dev/null || cargo update -q -p philium)

npm run sidecar
npx tauri build --bundles app,dmg

ARCH="$(uname -m)"; case "$ARCH" in arm64) ARCH=aarch64;; esac
B="src-tauri/target/release/bundle"
DMG="$B/dmg/Philium_${VERSION}_${ARCH}.dmg"
TGZ="$B/macos/Philium.app.tar.gz"
for f in "$DMG" "$TGZ" "$TGZ.sig"; do [ -f "$f" ] || { echo "missing $f" >&2; exit 1; }; done
# HAL must be inside the app, or agents cannot start.
[ -x "$B/macos/Philium.app/Contents/MacOS/hal-desktop-server" ] || { echo "HAL missing from bundle" >&2; exit 1; }

STAGE="$(mktemp -d)"; trap 'rm -rf "$STAGE"' EXIT
UPDATE="Philium_${VERSION}_${ARCH}.app.tar.gz"
cp "$TGZ" "$STAGE/$UPDATE"
node -e '
const [v,notes,sig,url,arch]=process.argv.slice(1);
const p={}; p[`darwin-${arch}`]={signature:sig,url};
process.stdout.write(JSON.stringify({version:v,notes,pub_date:new Date().toISOString(),platforms:p},null,2)+"\n");
' "$VERSION" "$NOTES" "$(cat "$TGZ.sig")" "https://github.com/$REPO/releases/download/v$VERSION/$UPDATE" "$ARCH" > "$STAGE/latest.json"

git add src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock package.json
[ -f package-lock.json ] && npm install --package-lock-only -s >/dev/null 2>&1 && git add package-lock.json
git commit -qm "Release $VERSION"
git tag "v$VERSION"
git push -q origin main "v$VERSION"

gh release create "v$VERSION" -R "$REPO" --title "Philium $VERSION" --notes "$NOTES" --latest \
  "$DMG" "$STAGE/$UPDATE" "$STAGE/latest.json"
echo "published https://github.com/$REPO/releases/tag/v$VERSION"
