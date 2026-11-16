#!/bin/sh
# Build HAL's desktop server from ../hal and stage it as a Tauri sidecar.
set -eu
HAL_DIR="${HAL_DIR:-$(cd "$(dirname "$0")/../../hal" && pwd)}"
TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
EXT=""; case "$TRIPLE" in *windows*) EXT=".exe";; esac
(cd "$HAL_DIR" && cargo build --release --bin hal-desktop-server)
mkdir -p "$(dirname "$0")/../src-tauri/binaries"
cp "$HAL_DIR/target/release/hal-desktop-server$EXT" \
   "$(dirname "$0")/../src-tauri/binaries/hal-desktop-server-$TRIPLE$EXT"
echo "staged hal-desktop-server for $TRIPLE ($(cd "$HAL_DIR" && git rev-parse --short HEAD))"
