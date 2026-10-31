#!/usr/bin/env bash
# Browser smoke test of the web bundle (D2, D32): serves <web-dir> (index.html + wasm-bindgen
# `pkg/`) and drives the editor in headless Chrome through the DevTools protocol
# (`drive.py`, web spec §8): the editor opens its window (winit's canvas) without a trap or
# panic, a wire drawn in a project and saved reaches OPFS and survives a reload, Ctrl+C reaches the
# system clipboard and Ctrl+V pastes from it. Console output, the Chrome log and a screenshot
# land in <out-dir>. WebGPU is disabled (SwiftShader's headless WebGPU shows no frames), so the
# editor draws with WebGL2 through ANGLE on SwiftShader (web spec §7), and the page screenshot is
# compared with the native golden under a loose tolerance. The page mounts OPFS
# before the editor starts, so the checks wait in real time (Chrome's virtual-time budget skips
# past storage I/O).
#
#   tests/web/smoke.sh <web-dir> [<out-dir>] [<chrome-binary>]
set -euo pipefail

dir="$1"
out="${2:-.}"
chrome="${3:-$(command -v google-chrome || command -v chromium || command -v chromium-browser)}"
port=8765
mkdir -p "$out"

python3 -m http.server "$port" --bind 127.0.0.1 --directory "$dir" >/dev/null 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
for _ in $(seq 50); do
  curl -sf "http://127.0.0.1:$port/" >/dev/null && break
  sleep 0.1
done

status=0
python3 "$(dirname "$0")/drive.py" "$chrome" "http://127.0.0.1:$port/" "$out" || status=1
exit $status
