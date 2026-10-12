#!/usr/bin/env python3
"""Drives the web editor in headless Chrome through the DevTools protocol (over a pipe,
Python stdlib only) and checks what the web spec promises end to end:

0. pixels (web spec §7): without a WebGPU adapter the editor draws on the CPU, and the
   frame of the `inv` project matches the native CPU golden
   `crates/app/tests/golden/editor_dark.png` (≤ 0.1% of pixels off by more than 2 levels);
1. storage (web spec §2): the `inv` fixture, written into OPFS by the browser's own API,
   opens (the mount reads files it did not write); a wire drawn and saved (Ctrl+S) reaches
   OPFS and is still there after a reload;
2. clipboard (§4): Ctrl+C of that wire puts the Schemify fragment on the system clipboard;
   after the reload (the editor's own clipboard is empty) Ctrl+V pastes it from the system
   clipboard, and the next save holds the reloaded file plus the pasted wire (so the editor
   read the saved file back);
3. no trap, panic or uncaught error on the console.

Test-only JavaScript below (OPFS reads, clipboard reads) runs through `Runtime.evaluate`;
the product itself ships none (D2).

    tests/web/drive.py <chrome> <url> <out-dir>
"""

import base64
import json
import os
import select
import subprocess
import sys
import struct
import time
import zlib

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
FIXTURE = os.path.join(REPO, "crates/app/tests/fixtures/inv")
GOLDEN = os.path.join(REPO, "crates/app/tests/golden/editor_dark.png")
CPU_CANVAS = "schemify-cpu"  # render::cpu_view::ID


class Browser:
    """One Chrome with `--remote-debugging-pipe`: commands on fd 3, replies on fd 4."""

    def __init__(self, chrome, flags, log):
        cmd_r, self.cmd_w = os.pipe()
        self.res_r, res_w = os.pipe()

        def child():
            # Move the pipe ends out of the way first, then onto 3 and 4.
            a, b = os.dup(cmd_r), os.dup(res_w)
            os.dup2(a, 3)
            os.dup2(b, 4)

        self.proc = subprocess.Popen(
            [chrome, *flags, "--remote-debugging-pipe"],
            preexec_fn=child,
            pass_fds=(3, 4),
            stdout=log,
            stderr=log,
        )
        os.close(cmd_r)
        os.close(res_w)
        self.buf = b""
        self.next_id = 0
        self.events = []

    def _read(self, deadline):
        while b"\0" not in self.buf:
            left = deadline - time.time()
            if left <= 0:
                raise TimeoutError("no reply from Chrome")
            ready, _, _ = select.select([self.res_r], [], [], left)
            if ready:
                chunk = os.read(self.res_r, 1 << 20)
                if not chunk:
                    raise RuntimeError("Chrome closed the pipe")
                self.buf += chunk
        msg, self.buf = self.buf.split(b"\0", 1)
        return json.loads(msg)

    def call(self, method, params=None, session=None, timeout=60):
        self.next_id += 1
        msg = {"id": self.next_id, "method": method, "params": params or {}}
        if session:
            msg["sessionId"] = session
        os.write(self.cmd_w, json.dumps(msg).encode() + b"\0")
        deadline = time.time() + timeout
        while True:
            m = self._read(deadline)
            if m.get("id") == self.next_id:
                if "error" in m:
                    raise RuntimeError(f"{method}: {m['error']}")
                return m.get("result", {})
            self.events.append(m)

    def close(self):
        try:
            self.call("Browser.close", timeout=10)
        except Exception:  # noqa: BLE001 - closing is best effort
            pass
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


class Page:
    def __init__(self, browser, origin):
        self.b = browser
        target = browser.call("Target.createTarget", {"url": "about:blank"})["targetId"]
        self.s = browser.call(
            "Target.attachToTarget", {"targetId": target, "flatten": True}
        )["sessionId"]
        browser.call(
            "Browser.grantPermissions",
            {"origin": origin, "permissions": ["clipboardReadWrite", "clipboardSanitizedWrite"]},
        )
        for domain in ("Runtime", "Page", "Log"):
            self.call(f"{domain}.enable")
        # Headless pages are never focused; the Clipboard API needs focus.
        self.call("Emulation.setFocusEmulationEnabled", {"enabled": True})

    def call(self, method, params=None, timeout=60):
        return self.b.call(method, params, self.s, timeout)

    def eval(self, expr, timeout=30):
        r = self.call(
            "Runtime.evaluate",
            {"expression": expr, "awaitPromise": True, "returnByValue": True},
            timeout,
        )
        if "exceptionDetails" in r:
            raise RuntimeError(f"{expr[:60]}: {r['exceptionDetails']}")
        return r["result"].get("value")

    def wait(self, what, expr, timeout=30):
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                if self.eval(expr):
                    return
            except RuntimeError:
                pass  # the page is navigating: its context is gone for a moment
            time.sleep(0.2)
        raise TimeoutError(f"timed out waiting for {what}")

    def open(self, url):
        self.call("Page.navigate", {"url": url})
        self.wait("the editor's canvas", "!!document.querySelector('canvas')")
        time.sleep(1.0)  # the first frames: plugins load, the UI composes

    def load(self, url):
        """Loads `url` afresh (a changed fragment alone would not restart the editor)."""
        before = self.eval("performance.timeOrigin")
        self.call("Page.navigate", {"url": url})
        self.call("Page.reload", {"ignoreCache": False})
        self.wait(
            f"the editor's canvas at {url}",
            f"performance.timeOrigin !== {before} && !!document.querySelector('canvas')",
        )
        time.sleep(1.0)

    def write_opfs(self, prefix, src):
        """Copies the non-hidden files under directory `src` into OPFS at `prefix`."""
        files = []
        for top, dirs, names in os.walk(src):
            dirs[:] = sorted(d for d in dirs if not d.startswith("."))
            for n in sorted(names):
                if n.startswith("."):
                    continue
                path = os.path.join(top, n)
                rel = os.path.relpath(path, src).replace(os.sep, "/")
                with open(path, "rb") as f:
                    files.append([f"{prefix}/{rel}", base64.b64encode(f.read()).decode()])
        js = (
            "(async () => { const root = await navigator.storage.getDirectory();"
            f" for (const [path, b64] of {json.dumps(files)}) {{"
            " const segs = path.split('/').filter(s => s); let d = root;"
            " for (const s of segs.slice(0, -1)) d = await d.getDirectoryHandle(s, {create: true});"
            " const w = await (await d.getFileHandle(segs[segs.length - 1], {create: true}))"
            ".createWritable();"
            " await w.write(Uint8Array.from(atob(b64), c => c.charCodeAt(0))); await w.close(); }"
            " return true; })()"
        )
        self.eval(js)
        return len(files)

    def cpu_png(self):
        """The CPU overlay's pixels as PNG bytes, or None when the page draws on a GPU."""
        url = self.eval(
            f"(() => {{ const c = document.getElementById('{CPU_CANVAS}');"
            " return c ? c.toDataURL('image/png') : null; })()"
        )
        return base64.b64decode(url.split(",", 1)[1]) if url else None

    def write_text(self, path, text):
        """Writes OPFS file `path` (test-only JS, as the browser's own API would)."""
        segs = [s for s in path.split("/") if s]
        self.eval(
            "(async () => { let d = await navigator.storage.getDirectory();"
            f" for (const s of {json.dumps(segs[:-1])})"
            " d = await d.getDirectoryHandle(s, {create: true});"
            f" const w = await (await d.getFileHandle({json.dumps(segs[-1])}, {{create: true}}))"
            ".createWritable();"
            f" await w.write({json.dumps(text)}); await w.close(); return true; }})()"
        )

    def wait_file(self, path, ok, timeout=20):
        """The text of OPFS file `path` once `ok(text)` holds (or the last text read)."""
        text, deadline = None, time.time() + timeout
        while time.time() < deadline:
            text = self.opfs(path)
            if text is not None and ok(text):
                break
            time.sleep(0.2)
        return text

    def reload(self):
        before = self.eval("performance.timeOrigin")
        self.call("Page.reload", {"ignoreCache": False})
        self.wait(
            "the editor's canvas after the reload",
            f"performance.timeOrigin !== {before} && !!document.querySelector('canvas')",
        )
        time.sleep(1.0)

    def mouse(self, kind, x, y):
        params = {"type": kind, "x": x, "y": y, "button": "left", "clickCount": 1}
        if kind == "mouseMoved":
            params["button"] = "none"
        self.call("Input.dispatchMouseEvent", params)

    def move(self, x, y):
        self.mouse("mouseMoved", x, y)
        time.sleep(0.05)

    def click(self, x, y):
        self.move(x, y)
        self.mouse("mousePressed", x, y)
        self.mouse("mouseReleased", x, y)
        time.sleep(0.15)

    def key(self, key, ctrl=False):
        """A printable key (`w`), a named one (`Escape`), or Ctrl + a letter."""
        named = {"Escape": ("Escape", 27), "F9": ("F9", 120)}
        if key in named:
            code, vk = named[key]
            text = None
        else:
            code, vk, text = f"Key{key.upper()}", ord(key.upper()), key
        mods = 2 if ctrl else 0
        if ctrl:
            self.call(
                "Input.dispatchKeyEvent",
                {"type": "rawKeyDown", "key": "Control", "code": "ControlLeft",
                 "windowsVirtualKeyCode": 17, "modifiers": 2},
            )
        down = {"type": "keyDown" if text and not ctrl else "rawKeyDown", "key": key,
                "code": code, "windowsVirtualKeyCode": vk, "modifiers": mods}
        if text and not ctrl:
            down["text"] = text
        self.call("Input.dispatchKeyEvent", down)
        self.call("Input.dispatchKeyEvent", {**down, "type": "keyUp", "text": ""})
        if ctrl:
            self.call(
                "Input.dispatchKeyEvent",
                {"type": "keyUp", "key": "Control", "code": "ControlLeft",
                 "windowsVirtualKeyCode": 17, "modifiers": 0},
            )
        time.sleep(0.15)

    def opfs(self, path):
        """The text of OPFS file `path`, or None."""
        segs = [s for s in path.split("/") if s]
        js = (
            "(async () => { try { let d = await navigator.storage.getDirectory();"
            f" for (const s of {json.dumps(segs[:-1])}) d = await d.getDirectoryHandle(s);"
            f" const f = await (await d.getFileHandle({json.dumps(segs[-1])})).getFile();"
            " return await f.text(); } catch (e) { return null; } })()"
        )
        return self.eval(js)

    def clipboard(self):
        return self.eval("navigator.clipboard.readText().catch(e => 'ERROR ' + e)")

    def opfs_tree(self):
        """Every OPFS path with its size (diagnostics when a save does not show up)."""
        return self.eval(
            "(async () => { const out = []; const walk = async (d, p) => {"
            " for await (const [n, h] of d.entries()) {"
            " if (h.kind === 'directory') await walk(h, p + '/' + n);"
            " else out.push(p + '/' + n + ' ' + (await h.getFile()).size); } };"
            " await walk(await navigator.storage.getDirectory(), ''); return out; })()"
        )

    def opfs_probe(self):
        """Whether this browser's OPFS takes a `createWritable` write (what the flusher does)."""
        return self.eval(
            "(async () => { try { const d = await navigator.storage.getDirectory();"
            " const h = await d.getFileHandle('probe.txt', {create: true});"
            " const w = await h.createWritable(); await w.write(new Uint8Array([111, 107]));"
            " await w.close(); const t = await (await h.getFile()).text();"
            " await d.removeEntry('probe.txt'); return t; } catch (e) { return 'ERROR ' + e; } })()"
        )

    def screenshot(self, path):
        data = self.call("Page.captureScreenshot", {"format": "png"})["data"]
        with open(path, "wb") as f:
            f.write(base64.b64decode(data))


def png_rgba(data):
    """(width, height, RGBA bytes) of an 8-bit RGB or RGBA, non-interlaced PNG."""
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, idat = 8, b""
    w = h = bpp = 0
    while pos < len(data):
        n = int.from_bytes(data[pos:pos + 4], "big")
        kind, body = data[pos + 4:pos + 8], data[pos + 8:pos + 8 + n]
        pos += 12 + n
        if kind == b"IHDR":
            w, h, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", body)
            assert depth == 8 and color in (2, 6) and interlace == 0, (depth, color, interlace)
            bpp = 4 if color == 6 else 3
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    raw, stride = zlib.decompress(idat), w * bpp
    out, prev, i = bytearray(), bytearray(stride), 0
    for _ in range(h):
        f, line = raw[i], bytearray(raw[i + 1:i + 1 + stride])
        i += 1 + stride
        for x in range(stride):
            a = line[x - bpp] if x >= bpp else 0
            b, c = prev[x], prev[x - bpp] if x >= bpp else 0
            if f == 1:
                line[x] = (line[x] + a) & 255
            elif f == 2:
                line[x] = (line[x] + b) & 255
            elif f == 3:
                line[x] = (line[x] + ((a + b) >> 1)) & 255
            elif f == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        out += line
        prev = line
    if bpp == 3:
        out = bytearray(b for k in range(0, len(out), 3) for b in (*out[k:k + 3], 255))
    return w, h, bytes(out)


def differing_pixels(a, b):
    """Pixels where some channel differs by more than 2 levels (the native golden rule)."""
    return sum(
        1
        for k in range(0, len(a), 4)
        if any(abs(a[k + j] - b[k + j]) > 2 for j in range(4))
    )


def console_lines(events):
    out = []
    for e in events:
        m, p = e.get("method"), e.get("params", {})
        if m == "Runtime.consoleAPICalled":
            args = " ".join(str(a.get("value", a.get("description", ""))) for a in p.get("args", []))
            out.append(f"console.{p.get('type')}: {args}")
        elif m == "Runtime.exceptionThrown":
            d = p.get("exceptionDetails", {})
            out.append(f"exception: {d.get('text')} {d.get('exception', {}).get('description', '')}")
        elif m == "Log.entryAdded":
            entry = p.get("entry", {})
            out.append(f"log.{entry.get('level')}: {entry.get('text')}")
    return out


def wires(text):
    return [line for line in (text or "").splitlines() if line.startswith("wire ")]


def main():
    chrome, url, out = sys.argv[1], sys.argv[2], sys.argv[3]
    os.makedirs(out, exist_ok=True)
    origin = url.split("/", 3)[:3]
    origin = "/".join(origin)
    flags = ["--headless=new", "--no-sandbox", "--no-first-run", "--window-size=1280,800",
             "--enable-unsafe-webgpu", "--enable-features=Vulkan", "--use-angle=swiftshader",
             "--use-webgpu-adapter=swiftshader", "--enable-logging=stderr", "--v=0"]
    failures = []
    with open(os.path.join(out, "drive-chrome.log"), "wb") as log:
        b = Browser(chrome, flags, log)
        try:
            p = Page(b, origin)
            try:
                p.open(url)
            except TimeoutError:
                failures.append("the editor never opened its window (no canvas in the page)")
                raise
            p.screenshot(os.path.join(out, "web.png"))

            # 1. A project the browser itself puts into OPFS (the mount reads files it did
            #    not write): draw a wire in an empty area (xschem: `w` from the pointer,
            #    click its end), save it, select just that wire and copy it.
            with open(os.path.join(FIXTURE, "top.shcomp")) as f:
                n0 = len(wires(f.read()))
            n = p.write_opfs("/projects/inv", FIXTURE)
            # The user config is OPFS `/config/init.lua` (web spec §5): F9 saves, so the first
            # save below works only when the config ran.
            p.write_text("/config/init.lua", 'schemify.keymap.set("F9", "file.save")\n')
            top = "/projects/inv/top.shcomp"
            p.load(f"{url}#{top}")
            # 0. Pixels (web spec §7): without a WebGPU adapter the editor draws on the CPU;
            #    before any synthetic input moves the pointer (the status line shows it), the
            #    frame must match the native CPU golden under the native test's tolerance.
            png, deadline = None, time.time() + 15
            while time.time() < deadline and png is None:
                png = p.cpu_png()
                time.sleep(0.25)
            if png is None:
                print("::notice::the browser has a WebGPU adapter: no CPU frame to compare")
            else:
                time.sleep(2.0)  # the editor settles (plugin load, compose, a tick)
                png = p.cpu_png()
                with open(os.path.join(out, "web_cpu.png"), "wb") as f:
                    f.write(png)
                got = png_rgba(png)
                with open(GOLDEN, "rb") as f:
                    want = png_rgba(f.read())
                if got[:2] != want[:2]:
                    failures.append(f"CPU frame {got[:2]}, golden {want[:2]}")
                else:
                    bad, limit = differing_pixels(got[2], want[2]), got[0] * got[1] // 1000
                    print(f"CPU frame vs editor_dark.png: {bad} pixels differ (limit {limit})")
                    if bad > limit:
                        failures.append(f"the web CPU frame differs from the native golden in "
                                        f"{bad} pixels (limit {limit}): see web_cpu.png")
            p.click(250, 700)  # focus the canvas (winit takes keys only when focused)
            p.key("w")
            p.click(500, 700)
            p.key("Escape")
            p.key("F9")
            first = p.wait_file(top, lambda t: len(wires(t)) == n0 + 1)
            print(f"inv ({n} files via OPFS): top.shcomp {n0} -> {len(wires(first))} wires")
            if len(wires(first)) != n0 + 1:
                failures.append(f"the save (F9, bound by /config/init.lua) did not reach OPFS "
                                f"with {n0 + 1} wires")
                print(f"OPFS now: {p.opfs_tree()}")
            p.click(375, 700)
            p.key("c", ctrl=True)
            clip = None
            deadline = time.time() + 10
            while time.time() < deadline and "schemify 1" not in (clip or ""):
                clip = p.clipboard()
                time.sleep(0.2)
            print(f"system clipboard after Ctrl+C: {len(wires(clip))} wire(s)")
            if len(wires(clip)) != 1:
                failures.append(f"Ctrl+C did not put the wire on the system clipboard: {clip!r}")

            # 2. Reload: the saved file persists and the editor reads it back; the paste
            #    comes from the system clipboard (the editor's own clipboard is empty now).
            p.reload()
            if len(wires(p.opfs(top))) != n0 + 1:
                failures.append("top.shcomp changed or vanished across the reload")
            p.click(400, 160)  # focus the canvas again (a fresh page)
            p.key("v", ctrl=True)
            time.sleep(1.0)  # the browser answers the clipboard read; the paste follows
            p.click(400, 160)
            p.key("Escape")
            p.key("s", ctrl=True)
            second = p.wait_file(top, lambda t: len(wires(t)) == n0 + 2)
            print(f"after the reload, paste and save: {len(wires(second))} wires")
            if len(wires(second)) != n0 + 2:
                failures.append(
                    f"expected {n0 + 2} wires (the reloaded file plus the pasted wire), got "
                    f"{len(wires(second))}: the editor did not read the saved file back, or "
                    "Ctrl+V did not paste from the system clipboard"
                )
            p.screenshot(os.path.join(out, "drive.png"))
        except Exception as e:  # noqa: BLE001 - report and fail
            failures.append(f"driver: {type(e).__name__}: {e}")
        finally:
            lines = console_lines(b.events)
            b.close()
    with open(os.path.join(out, "drive-console.txt"), "w") as f:
        f.write("\n".join(lines) + "\n")
    print("--- console (driven session)")
    print("\n".join(lines) or "(none)")
    bad = [line for line in lines if any(w in line for w in ("RuntimeError", "panicked", "exception:"))]
    if bad:
        failures.append("trap, panic or uncaught exception: " + bad[0])
    for f in failures:
        print(f"::error::{f}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
