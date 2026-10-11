#!/usr/bin/env bash
# Browser smoke test of the web bundle (D2, D32): serves <web-dir> (index.html + wasm-bindgen
# `pkg/`) and loads it in headless Chrome. Passes when the editor reached its window (winit
# appended its canvas to the page) and the console shows no trap or panic. WebGPU is requested
# through SwiftShader; a missing adapter is reported (`gpu: ...`, blank canvas), not failed:
# CI runners have no GPU.
#
#   tests/web/smoke.sh <web-dir> [chrome-binary]
set -euo pipefail

dir="$1"
chrome="${2:-$(command -v google-chrome || command -v chromium || command -v chromium-browser)}"
port=8765
log="$(mktemp)"
dom="$(mktemp)"

python3 -m http.server "$port" --bind 127.0.0.1 --directory "$dir" >/dev/null 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
for _ in $(seq 50); do
  curl -sf "http://127.0.0.1:$port/" >/dev/null && break
  sleep 0.1
done

timeout 120 "$chrome" --headless=new --no-sandbox --no-first-run \
  --enable-unsafe-webgpu --enable-features=Vulkan --use-angle=swiftshader \
  --use-webgpu-adapter=swiftshader --enable-logging=stderr --v=0 \
  --virtual-time-budget=20000 --dump-dom "http://127.0.0.1:$port/" >"$dom" 2>"$log" || true

echo "--- console"
grep -a 'CONSOLE' "$log" || echo "(no console output)"
echo "--- page"
grep -ao '<canvas[^>]*>' "$dom" || echo "(no canvas)"

status=0
if ! grep -aq '<canvas' "$dom"; then
  echo "::error::the editor never opened its window (no canvas in the page)"
  status=1
fi
if grep -aiE 'RuntimeError|panicked|Uncaught' "$log"; then
  echo "::error::the web build trapped or panicked"
  status=1
fi
exit $status
