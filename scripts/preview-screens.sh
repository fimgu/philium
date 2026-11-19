#!/bin/zsh
# Look at screens without the app: the built frontend with sample data, drawn by headless Chrome.
#
#   scripts/preview-screens.sh OUT_DIR ['screen=Review&state=bad' 'screen=Workday' ...]
#
# Each argument is a query for scripts/preview/mock.js: screen (the nav label), and for Review
# state=incomplete|ready|bad. Nothing here talks to a backend, opens a window or takes focus.
# It needs `npx vite build` to have run, and Google Chrome.
set -eu
HERE="${0:A:h}"; ROOT="${HERE:h}"; OUT="${1:?output folder}"; shift
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
[ -x "$CHROME" ] || { echo "Google Chrome is not installed"; exit 1; }
[ -f "$ROOT/dist/index.html" ] || { echo "Run: npx vite build"; exit 1; }
mkdir -p "$OUT"; WEB="$(mktemp -d "$OUT/.web.XXXXXX")"
cp -R "$ROOT/dist/." "$WEB/"; cp "$HERE/preview/mock.js" "$WEB/mock.js"
python3 - "$WEB/index.html" <<'PY'
import sys
p = sys.argv[1]; s = open(p).read()
open(p, 'w').write(s.replace('<head>', '<head><script src="./mock.js"></script>', 1))
PY
PORT=$(python3 -c "import socket; s=socket.socket(); s.bind(('127.0.0.1',0)); print(s.getsockname()[1]); s.close()")
(cd "$WEB" && exec python3 -u -m http.server "$PORT" --bind 127.0.0.1 >/dev/null 2>&1) &
SERVER=$!
trap 'kill $SERVER 2>/dev/null; rm -rf "$WEB"' EXIT
for i in {1..30}; do curl -s -o /dev/null "http://127.0.0.1:$PORT/index.html" && break; python3 -c "import time; time.sleep(0.2)"; done
[ $# -gt 0 ] || set -- 'screen=Review&state=incomplete' 'screen=Review&state=ready' 'screen=Review&state=bad' 'screen=Workday' 'screen=Environments&machine=1'
for q in "$@"; do
  name=$(print -r -- "$q" | tr -c 'A-Za-z0-9' '-' | sed 's/-*$//')
  "$CHROME" --headless=new --disable-gpu --hide-scrollbars --no-first-run --no-default-browser-check --user-data-dir="$WEB/.chrome" \
    --window-size=1280,1300 --virtual-time-budget=9000 --screenshot="$OUT/$name.png" "http://127.0.0.1:$PORT/index.html?$q" >/dev/null 2>&1
  echo "$OUT/$name.png"
done
