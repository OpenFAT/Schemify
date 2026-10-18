//! Exports and the user config in the editor, through the real frame spine: the PNG scale
//! and whole-drawing PNG (image sizes as the oracle, negative space: a refused size writes
//! nothing), the export page of PDF and EPS (media boxes), PDF/EPS text in the embedded UI
//! font, the packages view (the CLI's own `pack status` lines as the oracle), the user
//! config's compilers in the editor's runs (the runner's error names the configured program),
//! and the harness's fixture copies (no `.schemify/` state reaches a test).

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Session, U, copy_dir, fixture};
use schemify_app::editor::{Editor, Options};
use schemify_config::Startup;
use schemify_ui::InputEvents;

/// Queues command `name` (argument `arg`) and runs one frame.
fn run(s: &mut Session, name: &str, arg: &str) {
    s.ed.command(name, arg);
    s.frame(InputEvents::default());
}

fn png_size(path: &Path) -> (u16, u16) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let px = schemify_render::Pixmap::from_png(std::io::Cursor::new(bytes)).expect("a PNG");
    (px.width(), px.height())
}

fn open_with_config(dir: &Path, file: &str, init: &str) -> Session {
    let path = dir.join("init.lua");
    std::fs::write(&path, init).unwrap();
    let ed = Editor::new(Options {
        path: Some(dir.join(file)),
        seed: 7,
        config: Startup::File(path),
        ..Default::default()
    })
    .expect("opens");
    let mut s = Session {
        ed,
        now: 1,
        dir: dir.to_path_buf(),
    };
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    s
}

#[test]
fn png_exports_scale_and_cover_the_whole_drawing() {
    let mut s = Session::open("inv", "top.shcomp", "png-scale", true);
    let one = s.dir.join("one.png");
    run(
        &mut s,
        "file.image_export.png_export",
        one.to_str().unwrap(),
    );
    let (w, h) = png_size(&one);
    let r = s.ed.canvas_rect();
    assert_eq!(
        (w, h),
        (r.width().round() as u16, r.height().round() as u16)
    );
    // Scale 2 (192 dpi): the same view, twice the pixels a side.
    run(&mut s, "options.png_export_scale", "2");
    assert_eq!(s.ed.opts().png_scale, 200);
    let two = s.dir.join("two.png");
    run(
        &mut s,
        "file.image_export.png_export",
        two.to_str().unwrap(),
    );
    assert_eq!(png_size(&two), (2 * w, 2 * h));
    // The whole drawing, zoomed in or not: its bounds plus 10 units, 1 px per unit × 2.
    for _ in 0..4 {
        run(&mut s, "view.zoom_in", "");
    }
    let b = schemify_app::export::bounds(&s.ed.scene().prims).expect("a drawing");
    let b = schemify_app::export::padded(b, 10 * U);
    let side = |a: i32, z: i32| ((f64::from(z - a) / f64::from(U)).ceil() * 2.0).round() as u16;
    let full = s.dir.join("full.png");
    run(
        &mut s,
        "file.image_export.png_export_full",
        full.to_str().unwrap(),
    );
    assert_eq!(png_size(&full), (side(b[0], b[2]), side(b[1], b[3])));
    // Negative space: an out-of-range scale is refused and changes nothing; an image over
    // 16384 px a side is refused and writes no file.
    run(&mut s, "options.png_export_scale", "100");
    assert_eq!(s.ed.opts().png_scale, 200);
    run(&mut s, "options.png_export_scale", "16");
    assert!(r.width() * 16.0 > 16384.0, "the canvas overflows at 16×");
    let huge = s.dir.join("huge.png");
    run(
        &mut s,
        "file.image_export.png_export",
        huge.to_str().unwrap(),
    );
    assert!(!huge.exists());
    assert!(
        s.ed.session().status.contains("lower the PNG scale"),
        "{}",
        s.ed.session().status
    );
}

#[test]
fn pdf_and_eps_fit_the_export_page_and_embed_the_ui_font() {
    let mut s = Session::open("inv", "top.shcomp", "export-page", true);
    let pdf = s.dir.join("a.pdf");
    run(
        &mut s,
        "file.image_export.pdf_ps_export_full",
        pdf.to_str().unwrap(),
    );
    let t = std::fs::read_to_string(&pdf).unwrap();
    assert!(
        t.contains("/MediaBox [0 0 842 595]"),
        "A4 landscape by default"
    );
    // The drawing's texts print in the embedded UI font, not base-14 Helvetica.
    assert!(t.contains("/FontFile2 ") && t.contains("/ToUnicode "));
    assert!(!t.contains("Helvetica"));
    run(&mut s, "options.export_page_size", "letter portrait");
    assert_eq!(s.ed.opts().export_page.name(), "letter portrait");
    run(
        &mut s,
        "file.image_export.pdf_ps_export_full",
        pdf.to_str().unwrap(),
    );
    let t = std::fs::read_to_string(&pdf).unwrap();
    assert!(t.contains("/MediaBox [0 0 612 792]"), "letter portrait");
    let eps = s.dir.join("a.eps");
    run(
        &mut s,
        "file.image_export.eps_selection_export",
        eps.to_str().unwrap(),
    );
    let e = std::fs::read_to_string(&eps).unwrap();
    assert!(e.contains("%%BoundingBox: 0 0 612 792\n"));
    assert!(e.contains("/FontType 42 def") && e.contains("glyphshow"));
    // Negative space: an unknown page is refused and the page stays.
    run(&mut s, "options.export_page_size", "a9");
    assert!(s.ed.session().status.contains("not a page"));
    assert_eq!(s.ed.opts().export_page.name(), "letter portrait");
}

/// Tools → Package status shows what `schemify pack status` prints for the same config.
#[test]
fn packages_view_shows_pack_status() {
    let dir = fixture("inv", "packages-view");
    let pkg = dir.join("hello-pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    let wasm = Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugin/tests/fixtures/hello.wasm");
    std::fs::copy(&wasm, pkg.join("hello.wasm")).unwrap();
    let init = format!(
        "schemify.pack.add {{ {{ path = [==[{}]==], name = 'hello', lazy = {{ cmd = 'count_nets.run' }} }}, {{ path = [==[{}]==], name = 'ghost' }} }}",
        pkg.display(),
        dir.join("nope").display()
    );
    let mut s = open_with_config(&dir, "top.shcomp", &init);
    run(&mut s, "tools.package_status", "");
    let v = s.ed.session().text_view().expect("a packages tab");
    assert_eq!(v.title, "packages");
    let body = v.body.clone();
    // Oracle: the core's `schemify pack status` report for the same config.
    let (config, _) = schemify_config::Config::load(&Startup::File(dir.join("init.lua")));
    let paths = schemify_cli::pack::Paths::of(&config).unwrap();
    let report = schemify_cli::pack::status(&config.table().packs, &paths);
    assert_eq!(report.lines.len(), 2);
    for line in &report.lines {
        assert!(body.contains(line.as_str()), "{line:?} in\n{body}");
    }
    assert!(body.contains("ghost: path - missing"), "{body}");
    assert!(body.contains("run `schemify pack sync`"), "{body}");
    // Negative space: without a user config there is nothing to list.
    let mut clean = Session::open("inv", "top.shcomp", "packages-clean", true);
    run(&mut clean, "tools.package_status", "");
    let v = clean.ed.session().text_view().expect("a packages tab");
    assert!(v.body.contains("no user config"), "{}", v.body);
}

const VRES: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nimpl va veriloga\n  model vres\n  file vres.va\n";
const VA_BENCH: &str = "schemify 1 shbench\n\ninst 0aaaaa ./vres\n  at 0 0 R0\n  name N1\n\n\
label 0bbbba 0 0 R0 out\nlabel 0bbbbb 0 20 R0 0\n\nvar R 1\n";

/// Simulates the active bench to idle and returns the runner's error messages.
fn simulate_errors(s: &mut Session) -> Vec<String> {
    run(s, "sim.simulate", "");
    let t = Instant::now();
    while s.ed.sim().busy() {
        assert!(t.elapsed() < Duration::from_secs(60), "run did not finish");
        std::thread::sleep(Duration::from_millis(10));
        s.frame(InputEvents::default());
    }
    s.frame(InputEvents::default());
    let (_, _, results) = s.ed.sim().tables().expect("a run");
    results.errors().map(|e| e.2.to_owned()).collect()
}

/// The editor's Simulate uses the user config's `sim openvaf` (as the CLI's runner does):
/// the compile step of a Verilog-A `file` source runs the configured program.
#[test]
fn simulate_uses_the_configured_compilers() {
    let dir = std::env::temp_dir().join(format!("schemify-editor-vacomp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schemify.project"), "schemify 1 project\n").unwrap();
    std::fs::write(dir.join("vres.shprim"), VRES).unwrap();
    std::fs::write(dir.join("tb.shbench"), VA_BENCH).unwrap();
    std::fs::write(dir.join("vres.va"), "module vres; endmodule\n").unwrap();
    let init = "schemify.sim('openvaf', { path = 'openvaf-from-init' })\n\
                schemify.sim('iverilog', { path = 'iverilog-from-init' })\n";
    let mut s = open_with_config(&dir, "tb.shbench", init);
    let errors = simulate_errors(&mut s);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("`openvaf-from-init`") && e.contains("vres.va")),
        "{errors:?}"
    );
    assert!(!errors.iter().any(|e| e.contains("iverilog-from-init")));
    drop(s);
    // Negative space: without the config line the runner's default program runs.
    let dir = std::env::temp_dir().join(format!("schemify-editor-vacomp0-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schemify.project"), "schemify 1 project\n").unwrap();
    std::fs::write(dir.join("vres.shprim"), VRES).unwrap();
    std::fs::write(dir.join("tb.shbench"), VA_BENCH).unwrap();
    std::fs::write(dir.join("vres.va"), "module vres; endmodule\n").unwrap();
    let mut s = open_with_config(&dir, "tb.shbench", "-- no programs\n");
    let errors = simulate_errors(&mut s);
    assert!(
        !errors.iter().any(|e| e.contains("from-init")),
        "{errors:?}"
    );
}

/// A `.schemify/` directory in a fixture (a GUI session's journal, BACKLOG "tests open
/// fixtures in place") never reaches a test's copy, so no crash-recovery prompt appears.
#[test]
fn fixture_copies_leave_editor_state_behind() {
    let src = fixture("inv", "stale-src");
    let journal = src.join(".schemify/journal/top.shcomp.journal");
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(&journal, "not a journal\n").unwrap();
    let dst =
        std::env::temp_dir().join(format!("schemify-editor-stale-dst-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dst);
    copy_dir(&src, &dst);
    let _ = std::fs::remove_dir_all(&src);
    assert!(dst.join("top.shcomp").exists() && dst.join("inv.shcomp").exists());
    assert!(
        !dst.join(".schemify").exists(),
        "derived state is not copied"
    );
    let ed = Editor::new(Options {
        path: Some(dst.join("top.shcomp")),
        seed: 7,
        ..Default::default()
    })
    .expect("opens");
    let mut s = Session {
        ed,
        now: 1,
        dir: dst,
    };
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    assert!(
        s.ed.shell().ui().dialog().is_none(),
        "no crash-recovery prompt"
    );
}
