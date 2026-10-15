Embedded fonts, SIL Open Font License 1.1 (redistributable with MIT code):

- NotoSans-UI.ttf: Noto Sans 2.015 (variable), cut down for the UI (D39 binary size): width
  axis pinned at 100, weight axis limited to 400–700 (regular to bold), glyphs subset to Basic
  Latin, Latin-1, Latin Extended-A, Greek, general punctuation, super/subscripts, currency,
  letterlike symbols, arrows, math operators, misc technical and geometric shapes (µ Ω ± × ° →
  ≤ ≥ − … all included). 150 KB instead of 1.5 MB. Text outside the subset falls back to system
  fonts (fontique), so it never decides layout or golden images. License: NotoSans-OFL.txt
- KaTeX_*.ttf: KaTeX math fonts (copied from ratex-katex-fonts 0.1.14), math widget. License: KaTeX-OFL.txt

Regenerating NotoSans-UI.ttf from the full variable font (`NotoSans.ttf`, Noto Sans 2.015:
the notofonts/latin-greek-cyrillic release, or this directory before 2026-10-11 in git):

```
nix shell nixpkgs#python3Packages.fonttools -c fonttools varLib.instancer -q \
  -o NotoSans-inst.ttf NotoSans.ttf wdth=100 wght=400:700
nix shell nixpkgs#python3Packages.fonttools -c pyftsubset NotoSans-inst.ttf \
  --unicodes=U+0020-007E,U+00A0-017F,U+0370-03FF,U+2000-206F,U+2070-209F,U+20A0-20CF,U+2100-214F,U+2190-21FF,U+2200-22FF,U+2300-23FF,U+25A0-25FF \
  '--layout-features=*' --output-file=NotoSans-UI.ttf
```

`crates/ui` tests check that every non-ASCII character the UI sources use is in the subset.
