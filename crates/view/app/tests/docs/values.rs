//! The OTA page's blocks with the `gmid` plugin's values, var edits through the frame spine
//! (one undo), bounds colouring, and the plugin functions programs name.

use schemify_elab::{CellState, Val, no_funcs};
use schemify_expr::{Mode, Program};

use super::{
    cached, edit_var, eng, inst_param, label_of, label_of_opt, num, open, page_nodes, plugin_hash,
    row, var, var_in,
};

#[test]
fn ota_page_shows_blocks_with_gmid_values_beside_the_schematic() {
    let s = open("page");
    assert!(
        s.ed.docs_shown(),
        "a cell with doc blocks opens with its page"
    );
    let canvas = s.ed.canvas_rect();
    assert!(
        canvas.x1 < 800.0 && canvas.height() > 600.0,
        "schematic | docs side by side: {canvas:?}"
    );
    // Values come from the plugin, exactly (the same LUT query the plugin's own tests pin).
    let w_in = num(&s, "W_in");
    assert!((w_in - 3.881e-6).abs() < 2e-9, "W_in = {w_in}");
    assert!(!var(&s, "W_in").1, "plugin loaded: verified");
    assert!((num(&s, "Av_in") - 112.2).abs() < 0.1);
    assert!((num(&s, "GBW") - 150e-6 / (2.0 * std::f64::consts::PI * 1e-12)).abs() < 1.0);
    // Instance overrides `W = W_in` evaluate to the var; the symbol label shows it.
    assert_eq!(inst_param(&s, "M1", "W"), (Val::Num(w_in), false));
    assert_eq!(inst_param(&s, "M5", "L"), (Val::Num(1e-6), false));
    let texts = &s.ed.scene().prims.texts.text;
    assert!(
        texts.contains(&format!("W={}", eng(w_in))),
        "evaluated W on the canvas"
    );
    // The page: prose segments, var rows with values, the plot placeholder, in file order.
    let ids = page_nodes(&s);
    assert!(ids.contains(&"docs.b.0.0".to_owned()), "{ids:?}");
    let r = row(&s, "W_in");
    assert_eq!(
        label_of(&s, &format!("docs.b.{r}.val")),
        format!("= {}m", schemify_wave::format_si(w_in, 4))
    );
    let g = row(&s, "gm_id_in");
    assert_eq!(label_of(&s, &format!("docs.b.{g}.val")), "= 15 1/V");
    let plot = ids.iter().find(|i| {
        i.ends_with(".expr") && label_of_opt(&s, i).is_some_and(|l| l.starts_with("plot"))
    });
    assert!(
        plot.is_some(),
        "the ```plot fence becomes a graph slot: {ids:?}"
    );
    // Caches carry this plugin's hash; opening wrote only when they were stale.
    let text = s.text();
    for v in ["W_in", "gm_in", "Av_in", "ft_in", "W_load", "W_tail"] {
        let (value, hash) = cached(&text, v).unwrap_or_else(|| panic!("{v} cached"));
        assert_eq!(hash, plugin_hash(), "{v}");
        assert_eq!(value, num(&s, v), "{v}");
    }
    let (h, _) = s.ed.active().unwrap();
    let fixture = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/ota/ota.shcomp"
    ))
    .unwrap();
    let fresh = cached(&fixture, "W_in").is_some_and(|c| c.1 == plugin_hash());
    assert_eq!(
        s.ed.session().project.is_dirty(h),
        !fresh,
        "clean iff the caches matched"
    );
}

#[test]
fn a_var_edit_recomputes_page_and_instances_in_one_undo() {
    let mut s = open("edit");
    let (h, _) = s.ed.active().unwrap();
    let before = s.text();
    let undo_before = s.ed.session().project.history_len(h).0;
    let w_before = num(&s, "W_in");
    let journal = s
        .dir
        .join(".schemify")
        .join("journal")
        .join("ota.shcomp.journal");
    let records = |word: &str| {
        let t = std::fs::read_to_string(&journal).unwrap_or_default();
        let first = |l: &&str| l.split_whitespace().next() == Some(word);
        t.lines().filter(first).count()
    };
    let txs_before = records("tx");
    edit_var(&mut s, "gm_id_in", "10");
    assert_eq!(var(&s, "gm_id_in").0, Val::Int(10));
    let w = num(&s, "W_in");
    assert!(
        w < w_before,
        "stronger inversion: higher current density, narrower device ({w} vs {w_before})"
    );
    assert_eq!(
        inst_param(&s, "M2", "W"),
        (Val::Num(w), false),
        "M2 resized"
    );
    let texts = &s.ed.scene().prims.texts.text;
    assert!(
        texts.contains(&format!("W={}", eng(w))),
        "label updated in the same frame"
    );
    // D20: the cache followed the value inside the same undo entry.
    let text = s.text();
    assert_eq!(cached(&text, "W_in"), Some((w, plugin_hash())));
    assert!(text.contains("var gm_id_in 10\n"), "{text}");
    assert_eq!(
        s.ed.session().project.history_len(h).0,
        undo_before + 1,
        "one edit, one undo entry"
    );
    // The cache ops extended the edit's own journal record (`Edit::Extend`): one more `tx`,
    // no undo/redo records, and the record holds them.
    assert_eq!(records("tx"), txs_before + 1);
    assert_eq!((records("undo"), records("redo")), (0, 0));
    let jtext = std::fs::read_to_string(&journal).unwrap();
    assert!(jtext.contains("set_attr var W_in cached"), "{jtext}");
    assert_eq!(
        s.ed.session().project.undo_labels(h).last().copied(),
        Some("edit variable")
    );
    s.key("u");
    assert_eq!(
        s.text(),
        before,
        "undo restores expression and cache together"
    );
    assert_eq!(num(&s, "W_in"), w_before);
    assert_eq!(inst_param(&s, "M1", "W").0, Val::Num(w_before));
    s.key("Shift+U");
    assert_eq!(s.text(), text, "redo replays both");
    // Oracle: the state recomputed over undo's and redo's applied ops equals a fresh
    // elaboration (plugin-less: it reads the D20 caches, so compare values only).
    let fresh = CellState::build(&s.ed.session().project, h, &mut no_funcs).unwrap();
    for name in ["gm_id_in", "W_in", "W_tail"] {
        assert_eq!(var_in(&fresh, name).0, var(&s, name).0, "{name}");
    }
    // Oracle from the plugin's own sky130 sample checks: gmid.w(nfet, 10, 1u, 20u) = 4.930µm.
    edit_var(&mut s, "L_in", "1u");
    edit_var(&mut s, "Id_tail", "40u");
    assert!(
        (num(&s, "W_in") - 4.930e-6).abs() < 1e-9,
        "{}",
        num(&s, "W_in")
    );
    // A bad expression is kept and shown, never a crash (D13: errors are live findings).
    edit_var(&mut s, "L_in", "0.5u +");
    let r = row(&s, "L_in");
    assert!(
        label_of(&s, &format!("docs.b.{r}.val")).starts_with("X009"),
        "{}",
        label_of(&s, &format!("docs.b.{r}.val"))
    );
    assert!(matches!(var(&s, "W_in").0, Val::Upstream));
    // Out of the LUT's reach: the plugin's range error is the value's error.
    edit_var(&mut s, "L_in", "0.1u");
    let r = row(&s, "W_in");
    assert!(label_of(&s, &format!("docs.b.{r}.val")).starts_with("X010"));
}

#[test]
fn bounds_colour_the_value_red() {
    let mut s = open("bounds");
    edit_var(&mut s, "gm_id_in", "26");
    let r = row(&s, "gm_id_in");
    let label = label_of(&s, &format!("docs.b.{r}.val"));
    assert!(label.contains("above max 25"), "{label}");
    let sh = &s.ed.shell();
    let n = &sh.tables().nodes;
    let i = n
        .find(sh.strings().find(&format!("docs.b.{r}.val")).unwrap())
        .unwrap() as usize;
    assert_eq!(
        sh.strings().get(n.prop(schemify_ui::Prop::Style)[i]),
        "docs-value-bad"
    );
}

#[test]
fn programs_name_their_plugin_functions() {
    // The cache hash covers exactly the functions an expression calls (docs::cache_ops).
    let p = Program::compile(
        "gmid.w(\"n\", 1, 2, 3) + gmid.jd(\"n\", 1, 2) * x",
        Mode::Scalar,
    )
    .unwrap();
    let f: Vec<(&str, &str)> = p.plugin_fns().collect();
    assert_eq!(f, [("gmid", "w"), ("gmid", "jd")]);
}
