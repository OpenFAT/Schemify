#!/usr/bin/env bash
# Browser smoke test of the web bundle (D2, D32): serves <web-dir> (index.html + wasm-bindgen
# `pkg/`) and loads it in headless Chrome twice: once dumping the DOM, once taking a screenshot
# (<out-dir>/web.png) with the console logged. Passes when the editor reached its window (winit
# appended its canvas to the page) and the console shows no trap or panic. WebGPU is requested
# through SwiftShader; a missing adapter (`gpu: ...`, blank canvas) is reported, not failed:
# headless runners have no GPU, and the renderer has no CPU fallback on the web yet.
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

# shellcheck disable=SC2054 # "1280,800" is one argument
flags=(--headless=new --no-sandbox --no-first-run --window-size=1280,800
  --enable-unsafe-webgpu --enable-features=Vulkan --use-angle=swiftshader
  --use-webgpu-adapter=swiftshader --enable-logging=stderr --v=0 --virtual-time-budget=20000)
url="http://127.0.0.1:$port/"
timeout 120 "$chrome" "${flags[@]}" --dump-dom "$url" >"$out/dom.html" 2>"$out/chrome-dom.log" || true
timeout 120 "$chrome" "${flags[@]}" --screenshot="$out/web.png" "$url" 2>"$out/chrome.log" || true

echo "--- console"
grep -ah 'CONSOLE' "$out/chrome-dom.log" "$out/chrome.log" || echo "(no console output)"
echo "--- page"
grep -ao '<canvas[^>]*>' "$out/dom.html" || echo "(no canvas)"

status=0
if ! grep -aq '<canvas' "$out/dom.html"; then
  echo "::error::the editor never opened its window (no canvas in the page)"
  status=1
fi
if grep -ahiE 'RuntimeError|panicked|Uncaught' "$out/chrome-dom.log" "$out/chrome.log"; then
  echo "::error::the web build trapped or panicked"
  status=1
fi
if grep -aq 'gpu: ' "$out/chrome.log"; then
  echo "::warning::no WebGPU device in this browser: the editor runs but draws nothing"
fi
exit $status
