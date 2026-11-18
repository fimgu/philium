#!/bin/sh
# Compile with CARGO_BUILD_JOBS=2 cargo test --no-run first, then pass its
# printed test executable. Do not invoke this from a still-running cargo test.
# The real backend launches cargo test, npm install and vite in disposable copies.
set -eu
ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
BINARY="${1:?pass the already-compiled absolute Rust test executable}"
: "${PHILIUM_SCRATCH_DIR:?set PHILIUM_SCRATCH_DIR to a disposable scratch parent}"
TASK="$(mktemp -d "$PHILIUM_SCRATCH_DIR/selfdev-backend-XXXXXX")"
cleanup() {
  if [ -d "$TASK/source/.git" ]; then
    git -C "$TASK/source" worktree remove --force "$TASK/worktree" >/dev/null 2>&1 || true
  fi
  rm -rf "$TASK"
  test ! -e "$TASK"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
mkdir "$TASK/tmp"
git clone --quiet --shared --no-hardlinks "$ROOT" "$TASK/source"
git -C "$TASK/source" worktree add --quiet --detach "$TASK/worktree" HEAD
TRIPLE="$(rustc -vV | awk '/^host: / {print $2}')"
EXT=""; case "$TRIPLE" in *windows*) EXT=".exe";; esac
mkdir -p "$TASK/source/src-tauri/binaries"
cp -p "$ROOT/src-tauri/binaries/hal-desktop-server-$TRIPLE$EXT" "$TASK/source/src-tauri/binaries/"
printf 'Real public-backend worktree HEAD: '
git -C "$TASK/worktree" rev-parse HEAD
# Reuse only an explicitly chosen, idle existing target after outer cargo exits.
# This avoids both recursive Cargo locks and rebuilding all Tauri dependencies.
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/src-tauri/target}" \
  TMPDIR="$TASK/tmp" PHILIUM_SELFDEV_TEST_SOURCE="$TASK/source" \
  PHILIUM_SELFDEV_TEST_WORKTREE="$TASK/worktree" \
  "$BINARY" --ignored --exact selfdev::tests::public_test_stages_sidecar_before_real_cargo --nocapture
