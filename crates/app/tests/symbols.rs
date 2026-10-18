//! The `crates/doc/devices/` contact sheet (`schemify-symbols` skill): every core cell placed with pin
//! stubs plus a 5-transistor OTA, rendered under the Modern preset in the dark and light
//! canvas, at fit zoom and at a 4× crop. Re-bless with `SCHEMIFY_BLESS=1`; look at every
//! image before committing it.

mod common;

use common::{Session, U};
use schemify_app::scene::LAYER_MISSING;
use schemify_ui::{InputEvent, InputEvents};

fn check_golden(name: &str, pixmap: schemify_render::Pixmap) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");
    let path = format!("{dir}/{name}.png");
    let png = pixmap.clone().into_png().expect("encode png");
    if std::env::var_os("SCHEMIFY_BLESS").is_some() {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(&path, &png).unwrap();
    }
    let want = std::fs::read(&path)
        .unwrap_or_else(|_| panic!("{path} missing; run with SCHEMIFY_BLESS=1"));
    let want =
        schemify_render::Pixmap::from_png(std::io::Cursor::new(want)).expect("decode golden");
    assert_eq!(
        (want.width(), want.height()),
        (pixmap.width(), pixmap.height())
    );
    let bad = want
        .data()
        .iter()
        .zip(pixmap.data())
        .filter(|(a, b)| {
            let (a, b) = ([a.r, a.g, a.b, a.a], [b.r, b.g, b.b, b.a]);
            a.iter().zip(b).any(|(x, y)| x.abs_diff(y) > 2)
        })
        .count();
    let limit = want.data().len() / 1000;
    if bad > limit {
        let out = format!("{}/{name}.actual.png", env!("CARGO_TARGET_TMPDIR"));
        std::fs::write(&out, &png).unwrap();
        panic!("{name}: {bad} pixels differ (limit {limit}); actual written to {out}");
    }
}

/// The sheet open under the Modern preset; every placed cell resolves.
fn sheet(tag: &str) -> Session {
    let mut s = Session::open("symbols", "sheet.shcomp", tag, true);
    s.ed.command("view.theme_modern", "");
    s.frame(InputEvents::default());
    let missing = s.ed.scene().prims.rects.layer.iter();
    assert!(
        !missing.clone().any(|&l| l == LAYER_MISSING),
        "every core cell resolves"
    );
    assert!(s.ed.scene().prims.len() > 300, "the sheet drew");
    // The first paint fits the camera to the sheet; zooms start from there.
    let _ = s.render();
    s
}

/// Zooms `k`× so that world point `c` (xschem units) ends at the canvas centre: one wheel
/// turn of `log(k)/log(1.2)` steps at the screen point the zoom keeps fixed for that.
fn zoom_centred(s: &mut Session, c: (i32, i32), k: f64) {
    assert!(k > 1.0, "zoom in only");
    let before = s.ed.camera().scale;
    let c = (c.0 * U, c.1 * U);
    let (cx, cy) = s.ed.to_screen(c);
    let mid = s.ed.canvas_rect().center();
    let at = |cs: f32, m: f64| ((k * f64::from(cs) - m) / (k - 1.0)) as f32;
    let (x, y) = (at(cx, mid.x), at(cy, mid.y));
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerMove { x, y });
    let dy = (k.ln() / 1.2f64.ln()) as f32;
    ev.push(InputEvent::Wheel { x, y, dx: 0.0, dy });
    s.frame(ev);
    let got = s.ed.camera().scale / before;
    assert!((got - k).abs() < 1e-3 * k, "zoomed {k}×, not {got}");
    let (x2, y2) = s.ed.to_screen(c);
    let off = (f64::from(x2) - mid.x).abs() + (f64::from(y2) - mid.y).abs();
    assert!(off < 2.0, "{c:?} lands {off} px off the centre");
}

#[test]
fn golden_core_symbols() {
    let mut s = sheet("core-symbols");
    check_golden("core_symbols_dark", s.render());
    s.key("Shift+O");
    assert!(s.ed.opts().light);
    check_golden("core_symbols_light", s.render());
    zoom_centred(&mut s, (150, 55), 4.0);
    check_golden("core_symbols_light_4x", s.render());
    s.key("Shift+O");
    assert!(!s.ed.opts().light);
    check_golden("core_symbols_dark_4x", s.render());
}

/// Review aid for the symbol loop (not a check): with `SCHEMIFY_SHOTS=<dir>`, writes an 8×
/// crop centred on every cell of the sheet and on the OTA, dark canvas.
#[test]
#[ignore]
fn symbol_crops() {
    let Some(dir) = std::env::var_os("SCHEMIFY_SHOTS") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let mut centres: Vec<(String, (i32, i32))> = (0..23)
        .map(|k| (format!("cell{k:02}"), ((k % 6) * 160, (k / 6) * 140)))
        .collect();
    centres.push(("ota".into(), (1220, 130)));
    for (name, at) in centres {
        let mut s = sheet(&format!("crop-{name}"));
        zoom_centred(&mut s, at, if name == "ota" { 2.5 } else { 8.0 });
        common::save_png(&s.render(), &dir.join(format!("{name}.png")));
    }
}
