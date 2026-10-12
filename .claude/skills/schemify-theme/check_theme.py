#!/usr/bin/env python3
"""Check a Schemify ui.records theme against the schemify-theme rules. Stdlib only.

Usage: check_theme.py [path/to/ui.records ...]   (default: plugins/default-ui/ui.records)
Later files override earlier ones per (selector, prop), like D8 contributions. Exit 1 on any FAIL.
"""
import re
import sys
from itertools import combinations

# Canvas layer roles (xschem numbering, see docs/research/xschem-feature-inventory.md § layers).
WIRE, TEXT, SYMBOL, PIN = 1, 3, 4, 5
# Must match HILITE_LAYERS in crates/app/src/editor/parity.rs.
HILITE = [8, 9, 10, 12, 13, 15, 17, 21]
# Chrome selectors whose colour is meaning (severity, pass/fail, diff state), exempt from the accent budget.
STATUS = re.compile(r"^(check-|results-(pass|fail|yield)|docs-(value|unverified|staged))")
# Containers that never hold text: their fg is not read.
NO_TEXT = {"window", "canvas"}
COLOR_PROPS = {"bg", "fg", "hover-bg", "hover-fg", "active-bg", "active-fg", "accel-fg", "border-color"}

# Machado et al. 2009, severity 1.0, applied to linear RGB.
CVD = {
    "deutan": ((0.367322, 0.860646, -0.227968), (0.280085, 0.672501, 0.047413), (-0.011820, 0.042940, 0.968881)),
    "protan": ((0.152286, 1.052583, -0.204868), (0.114503, 0.786281, 0.099216), (-0.003882, -0.048116, 1.051998)),
}


def parse(paths):
    """{selector: {prop: 'rrggbb'}} for style blocks; plus colour values on any other record."""
    styles, other = {}, []
    for path in paths:
        sel = None
        for line in open(path, encoding="utf-8"):
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            if not line[0].isspace():
                words = line.split()
                sel = words[1] if words[0] == "style" and len(words) > 1 else None
                continue
            words = line.split()
            if len(words) < 2 or words[0] not in COLOR_PROPS:
                continue
            hexv = words[1].strip('"').lstrip("#")[:6]
            if not re.fullmatch(r"[0-9a-fA-F]{6}", hexv):
                continue
            if sel is not None:
                styles.setdefault(sel, {})[words[0]] = hexv.lower()
            else:
                other.append(hexv.lower())
    return styles, other


def lin(c):
    c /= 255
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def rgb_lin(h):
    return [lin(int(h[i:i + 2], 16)) for i in (0, 2, 4)]


def luminance(h):
    r, g, b = rgb_lin(h)
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def contrast(a, b):
    la, lb = sorted((luminance(a), luminance(b)), reverse=True)
    return (la + 0.05) / (lb + 0.05)


def lab_from_lin(rgb):
    r, g, b = rgb
    x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047
    y = 0.2126 * r + 0.7152 * g + 0.0722 * b
    z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883
    f = lambda t: t ** (1 / 3) if t > 0.008856 else 7.787 * t + 16 / 116
    fx, fy, fz = f(x), f(y), f(z)
    return 116 * fy - 16, 500 * (fx - fy), 200 * (fy - fz)


def lab(h, cvd=None):
    rgb = rgb_lin(h)
    if cvd:
        rgb = [max(0.0, min(1.0, sum(m * c for m, c in zip(row, rgb)))) for row in CVD[cvd]]
    return lab_from_lin(rgb)


def delta_e(a, b, cvd=None):
    # ponytail: CIE76, not CIEDE2000; good enough to flag near-duplicates. Upgrade if thresholds mislead.
    return sum((p - q) ** 2 for p, q in zip(lab(a, cvd), lab(b, cvd))) ** 0.5


def chroma(h):
    _, a, b = lab(h)
    return (a * a + b * b) ** 0.5


fails = 0


def report(ok, msg):
    global fails
    fails += not ok
    print(("PASS " if ok else "FAIL ") + msg)


def check_canvas(styles, theme):
    layer = lambda n: styles.get(f"canvas.{theme}.{n}", {}).get("fg")
    bg, grid, sel = layer(0), layer("grid"), layer("sel")
    if bg is None:
        print(f"SKIP canvas.{theme}: no background")
        return
    named = {"wire": layer(WIRE), "text": layer(TEXT), "symbol": layer(SYMBOL), "pin": layer(PIN), "sel": sel}
    named.update({f"hl{n}": layer(n) for n in HILITE})
    named = {k: v for k, v in named.items() if v}
    for k, v in named.items():
        need = 4.5 if k == "text" else 3.0
        c = contrast(v, bg)
        report(c >= need, f"{theme}: {k} #{v} vs bg contrast {c:.2f} (>= {need})")
    if grid:
        g = contrast(grid, bg)
        floor = min(contrast(named[k], bg) for k in ("wire", "symbol", "pin") if k in named)
        report(1.3 <= g < floor, f"{theme}: grid contrast {g:.2f} in [1.3, {floor:.2f}) (visible, dimmer than circuit)")
    # Highlights must separate from each other and from wire/selection, also for colour-blind users.
    group = [k for k in named if k.startswith("hl")] + ["wire", "sel"]
    for cvd, need in ((None, 20), ("deutan", 10), ("protan", 10)):
        bad = [
            f"{a}/{b} {delta_e(named[a], named[b], cvd):.0f}"
            for a, b in combinations([k for k in group if k in named], 2)
            if delta_e(named[a], named[b], cvd) < need
        ]
        report(not bad, f"{theme}: highlight separation dE76 >= {need} ({cvd or 'normal'})" + (": " + ", ".join(bad) if bad else ""))


def check_chrome(styles, other):
    base = styles.get("*", {})
    for sel, props in sorted(styles.items()):
        if sel.startswith("canvas.") or sel in NO_TEXT or "bg" not in props:
            continue
        fg = props.get("fg", base.get("fg"))
        if fg:
            c = contrast(fg, props["bg"])
            report(c >= 4.5, f"chrome {sel}: text contrast {c:.2f} (>= 4.5)")
    decor = {s: p for s, p in styles.items() if not s.startswith("canvas.") and not STATUS.match(s)}
    loud = {}
    for s, p in decor.items():
        for v in p.values():
            if chroma(v) > 30:
                loud.setdefault(v, []).append(s)
    for v in other:
        if chroma(v) > 30:
            loud.setdefault(v, []).append("(node)")
    detail = "; ".join(f"{v} {','.join(sorted(set(s)))}" for v, s in sorted(loud.items()))
    report(len(loud) <= 2, f"chrome decorative saturated colours {len(loud)} (<= 2 accents): {detail}")


def main():
    paths = sys.argv[1:] or ["plugins/default-ui/ui.records"]
    styles, other = parse(paths)
    assert styles, f"no style records in {paths}"
    for theme in ("dark", "light"):
        check_canvas(styles, theme)
    check_chrome(styles, other)
    print(f"{fails} failing check(s)")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
