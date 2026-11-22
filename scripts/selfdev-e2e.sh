#!/bin/sh
# End to end "Improve Philium" through the real backend: a disposable clone of
# this checkout, the real HAL sidecar, gateway, MCP and terminals, and a
# scripted stand-in model that changes a tooltip and commits.
#   cd src-tauri && cargo test --no-run   # prints the test executable
#   PHILIUM_SCRATCH_DIR=/tmp/x scripts/selfdev-e2e.sh /abs/path/to/philium-<hash>
set -eu
ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
BINARY="${1:?pass the already-compiled absolute Rust test executable}"
: "${PHILIUM_SCRATCH_DIR:?set PHILIUM_SCRATCH_DIR to a disposable scratch parent}"
TASK="$(mktemp -d "$PHILIUM_SCRATCH_DIR/selfdev-e2e-XXXXXX")"
cleanup() {
  if [ -d "$TASK/source/.git" ]; then
    for w in $(git -C "$TASK/source" worktree list --porcelain | awk '/^worktree / {print $2}' | tail -n +2); do
      git -C "$TASK/source" worktree remove --force "$w" >/dev/null 2>&1 || true
    done
  fi
  rm -rf "$TASK"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
mkdir "$TASK/tmp" "$TASK/scratch"
git clone --quiet --no-hardlinks "$ROOT" "$TASK/source"
git -C "$TASK/source" -c user.name=Fixture -c user.email=fixture@example.invalid commit -q --allow-empty -m fixture
TRIPLE="$(rustc -vV | awk '/^host: / {print $2}')"
mkdir -p "$TASK/source/src-tauri/binaries"
cp -p "$ROOT/src-tauri/binaries/hal-desktop-server-$TRIPLE" "$TASK/source/src-tauri/binaries/"
# The source checkout's node_modules keeps npm install offline and fast.
[ -d "$ROOT/node_modules" ] && ln -s "$ROOT/node_modules" "$TASK/source/node_modules"
TMPDIR="$TASK/tmp" PHILIUM_SELFDEV_TEST_SOURCE="$TASK/source" PHILIUM_SELFDEV_TEST_SCRATCH="$TASK/scratch" \
  "$BINARY" --ignored --exact selfdev::tests::improver_end_to_end_tooltip_change --nocapture
