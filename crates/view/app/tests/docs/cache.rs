//! The D20 cache paths: cached values without the plugin, a changed plugin hash, and param
//! defaults cached like vars.

use schemify_elab::{CellState, Val, no_funcs};

use super::{cached, cached_of, edit_var, inst_param, label_of, plugin_hash, row, var, var_in};
use crate::common::{self, Session};

/// Copies the fixture and edits one of its files before opening.
fn open_edited(tag: &str, file: &str, edit: impl Fn(String) -> String) -> Session {
    let dir = common::fixture("ota", tag);
    let p = dir.join(file);
    let text = std::fs::read_to_string(&p).expect("the fixture is readable");
    std::fs::write(&p, edit(text)).expect("the test's temp dir is writable");
    let ed = schemify_editor::editor::Editor::new(schemify_editor::editor::Options {
        no_default_ui: false,
        path: Some(dir.join("ota.shcomp")),
        seed: 7,
        ..Default::default()
    })
    .expect("opens");
    let mut s = Session {
        ed,
        now: 1,
        dir,
        caches: Default::default(),
    };
    for _ in 0..3 {
        s.frame(Default::default());
    }
    s
}

#[test]
fn d20_without_the_plugin_files_open_on_cached_values_marked_unverified() {
    let mut s = open_edited("noplugin", "schemify.project", |_| {
        "schemify 1 project\n\nlib dev path dev\n".to_owned()
    });
    let (h, _) = s.ed.active().unwrap();
    let text = s.text();
    let (w_cached, _) = cached(&text, "W_in").unwrap();
    assert_eq!(var(&s, "W_in"), (Val::Num(w_cached), true));
    assert_eq!(
        inst_param(&s, "M1", "W"),
        (Val::Num(w_cached), true),
        "dependents inherit the marker"
    );
    assert!(!var(&s, "id_in").1, "plain vars stay verified");
    let r = row(&s, "W_in");
    assert!(label_of(&s, &format!("docs.b.{r}.unverified")).starts_with("unverified"));
    assert!(
        !s.ed.session().project.is_dirty(h),
        "no plugin, nothing written"
    );
    // The oracle: the same values as a plugin-less elaboration of the file.
    let fresh = CellState::build(&s.ed.session().project, h, &mut no_funcs).unwrap();
    assert_eq!(var_in(&fresh, "W_tail"), var(&s, "W_tail"));
    // A new expression has no cache: X006 until the plugin is back, and the stale cache goes.
    edit_var(&mut s, "W_in", "gmid.w(\"nfet_01v8\", 12, L_in, id_in)");
    assert!(matches!(var(&s, "W_in").0, Val::Err(e) if e.code == schemify_expr::Code::X006));
    assert_eq!(cached(&s.text(), "W_in"), None);
}

#[test]
fn d20_a_changed_plugin_hash_recomputes_and_rewrites_the_cache() {
    let s = open_edited("rehash", "ota.shcomp", |t| {
        t.replace(&format!(" {}\n", fixture_hash(&t)), " 00000000deadbeef\n")
            .replace("cached 112.20605997990423", "cached 1")
    });
    let (h, _) = s.ed.active().unwrap();
    let text = s.text();
    assert!(!text.contains("deadbeef"), "every stale cache rewritten");
    assert_eq!(cached(&text, "Av_in").unwrap().1, plugin_hash());
    assert!(
        (cached(&text, "Av_in").unwrap().0 - 112.2).abs() < 0.1,
        "recomputed value"
    );
    assert!(s.ed.session().project.is_dirty(h));
    assert_eq!(
        s.ed.session().project.undo_labels(h).last().copied(),
        Some("update plugin cache")
    );
    assert!(!var(&s, "Av_in").1);
}

/// The OTA with an own param whose default calls the plugin (and, when given, its cache).
fn with_wref(text: &str, cache: Option<&str>) -> String {
    let mut p = String::from(
        "pin VSS inout 20 40\n\nparam wref \"gmid.w(\\\"nfet_01v8\\\", 10, 1u, 20u)\"\n",
    );
    if let Some(c) = cache {
        p.push_str(&format!("  cached {c}\n"));
    }
    text.replacen("pin VSS inout 20 40\n", &p, 1)
}

/// Replaces the document's text through Edit file (the canonical text with `from` → `to`).
fn edit_file(s: &mut Session, from: &str, to: &str) {
    let edited = s.text().replace(from, to);
    s.key("Alt+Q");
    s.ed.command("dialog.edit_file.ok", &edited);
    s.frame(Default::default());
}

/// D20 for the cell's own param defaults: a default that calls a plugin function gets its
/// `cached` on open (the plugin's answer and hash); a new default through Edit file, with the
/// stale `cached` line kept, has it stripped in the same transaction and the plugin's new
/// answer written after.
#[test]
fn d20_param_defaults_are_cached_like_vars() {
    let mut s = open_edited("param", "ota.shcomp", |t| with_wref(&t, None));
    let (w, hash) = cached_of(&s.text(), "param", "wref").expect("written on open");
    // Oracle: the plugin's own sample check, gmid.w(nfet, 10, 1u, 20u) = 4.930µm.
    assert!((w - 4.930e-6).abs() < 2e-9, "{w}");
    assert_eq!(hash, plugin_hash());
    let g = &s.ed.cell().unwrap().graph;
    assert_eq!((g.vals[0].clone(), g.unverified[0]), (Val::Num(w), false));
    edit_file(&mut s, "10, 1u, 20u", "12, 1u, 20u");
    let (w12, _) = cached_of(&s.text(), "param", "wref").expect("rewritten");
    assert!(w12 > w, "weaker inversion, wider: {w12} vs {w}");
    assert_eq!(s.ed.cell().unwrap().graph.vals[0], Val::Num(w12));
}

/// Without the plugin a param's cache stands in, unverified; a new default drops it (the
/// cache belongs to the old expression), leaving `X006` until the plugin is back.
#[test]
fn d20_param_cache_without_the_plugin_goes_with_its_default() {
    let dir = common::fixture("ota", "param-noplugin");
    let manifest = "schemify 1 project\n\nlib dev path dev\n";
    std::fs::write(dir.join("schemify.project"), manifest).unwrap();
    let p = dir.join("ota.shcomp");
    let text = std::fs::read_to_string(&p).unwrap();
    let cache = format!("4.93e-6 {}", fixture_hash(&text));
    std::fs::write(&p, with_wref(&text, Some(&cache))).unwrap();
    let ed = schemify_editor::editor::Editor::new(schemify_editor::editor::Options {
        path: Some(p),
        seed: 7,
        ..Default::default()
    })
    .expect("opens");
    let mut s = Session {
        ed,
        now: 1,
        dir,
        caches: Default::default(),
    };
    for _ in 0..3 {
        s.frame(Default::default());
    }
    let g = &s.ed.cell().unwrap().graph;
    assert_eq!(
        (g.vals[0].clone(), g.unverified[0]),
        (Val::Num(4.93e-6), true)
    );
    edit_file(&mut s, "10, 1u, 20u", "12, 1u, 20u");
    assert_eq!(cached_of(&s.text(), "param", "wref"), None);
    let v = &s.ed.cell().unwrap().graph.vals[0];
    assert!(
        matches!(v, Val::Err(e) if e.code == schemify_expr::Code::X006),
        "{v:?}"
    );
}

/// The hash the fixture file was cached with.
fn fixture_hash(text: &str) -> String {
    cached(text, "W_in").expect("the value is cached").1
}
