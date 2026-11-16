#!/bin/sh
# build + render: scripts/render/run.sh <outDir> [--only=a,b] [--sets=empty]
cd "$(dirname "$0")/../.." && npx vite build >/dev/null && node scripts/render/render.mjs "$@"
