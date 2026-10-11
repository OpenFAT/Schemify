//! Edit gestures and commands closing xschem parity gaps, end to end through the frame spine
//! (TESTING.md: oracles, negative space): partial wire selection and vertex editing, symbol
//! text orientation, recent files of another project, the canvas options from `init.lua`,
//! make-subcircuit rewiring and join/trim of diagonal and attributed wires. Oracles: the
//! canonical text of the edited document (and undo restoring it byte for byte), elab
//! connectivity before and after (as partitions), and seeded random inputs.

mod common;
use common::{Session, U, lines};

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemify_app::editor::{Editor, Options};
use schemify_ui::InputEvents;

const SHIFT: u8 = schemify_ui::Mods::SHIFT;
const CTRL: u8 = schemify_ui::Mods::CTRL;

/// Queues command `name` (argument `arg`) and runs one frame.
fn run(s: &mut Session, name: &str, arg: &str) {
    s.ed.command(name, arg);
    s.frame(InputEvents::default());
}

fn status(s: &Session) -> String {
    s.ed.session.status.clone()
}

/// Replaces the active document's text through Properties → Edit file (one undo).
fn set_text(s: &mut Session, text: &str) {
    s.key("Alt+Q");
    s.ed.command("dialog.edit_file.ok", text);
    s.frame(InputEvents::default());
    assert_eq!(s.text(), text, "{}", status(s));
}

/// A tiny deterministic generator for the seeded property tests (no dependency).
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }
}

/// Connectivity of the active schematic: every instance pin (`i<id>.<k>`), wire (`w<id>`),
/// label (`l<id>`) and port (`p<id>`) with its component and that component's net name.
fn nets(s: &Session) -> BTreeMap<String, (u32, String)> {
    let (h, _) = s.ed.active().unwrap();
    let doc = s.ed.session.project.doc(h).unwrap();
    let cell = s.ed.cell().expect("elaborated");
    let name = |c: u32| schemify_app::checks::net_name(cell, c).to_owned();
    let mut out = BTreeMap::new();
    let inst = doc.instances();
    for i in 0..inst.id.len() {
        for (k, (c, _)) in schemify_app::checks::pins_of(doc, cell, i)
            .into_iter()
            .enumerate()
        {
            out.insert(format!("i{}.{k}", inst.id[i].0), (c, name(c)));
        }
    }
    for (r, id) in doc.wires().id.iter().enumerate() {
        let c = cell.nets.wire_net[r];
        out.insert(format!("w{}", id.0), (c, name(c)));
    }
    for (r, id) in doc.labels().id.iter().enumerate() {
        let c = cell.nets.label_net[r];
        out.insert(format!("l{}", id.0), (c, name(c)));
    }
    for (r, id) in doc.ports().id.iter().enumerate() {
        let c = cell.nets.port_net[r];
        out.insert(format!("p{}", id.0), (c, name(c)));
    }
    out
}

/// Whether two connectivity maps put the keys they share into the same partition.
fn same_partition(a: &BTreeMap<String, (u32, String)>, b: &BTreeMap<String, (u32, String)>) {
    let shared: Vec<&String> = a.keys().filter(|k| b.contains_key(*k)).collect();
    for x in &shared {
        for y in &shared {
            assert_eq!(
                a[*x].0 == a[*y].0,
                b[*x].0 == b[*y].0,
                "{x} and {y}: connected before {} / after {}",
                a[*x].0 == a[*y].0,
                b[*x].0 == b[*y].0
            );
        }
    }
}

/// Rotation (quarter turns) of every drawn text whose content is `text`.
fn text_rots(s: &Session, text: &str) -> Vec<u8> {
    let t = &s.ed.scene().prims.texts;
    let mut start = 0usize;
    let mut out = Vec::new();
    for i in 0..t.x.len() {
        let end = t.end[i] as usize;
        if &t.text[start..end] == text {
            out.push(t.rot[i]);
        }
        start = end;
    }
    out
}

// ---------------------------------------------------------------------------- partial wires

/// Ctrl+drag box around x2 selects x2 whole and the inside end of each of its four wires; a
/// plain move then moves just those ends (the wires stretch, pins stay connected), the
/// partial selection follows the move, and a plain box selects no wire ends.
#[test]
fn a_ctrl_box_selects_inside_wire_ends_and_a_move_stretches_them() {
    let mut s = Session::open("inv", "top.shcomp", "partial", true);
    let original = s.text();
    let before = nets(&s);
    s.drag((90 * U, -45 * U), (190 * U, 40 * U), CTRL);
    let sel = s.ed.selection().to_vec();
    assert_eq!(sel.len(), 1, "only x2 lies inside: {sel:?}");
    assert_eq!(sel[0].table, schemify_doc::TableId::Instances);
    s.move_to((150 * U, 0));
    s.key("m");
    s.move_to((170 * U, 0));
    s.click((170 * U, 0));
    let t = s.text();
    for want in [
        "  at 160 0 R0\n  name x2",
        "wire w00001 40 0 120 0\n",
        "wire w00003 200 0 240 0\n",
        "wire w00005 140 -70 160 -30\n",
        "wire w00008 140 70 160 30\n",
        "wire w00006 0 -70 140 -70\n",
    ] {
        assert!(t.contains(want), "{want:?} in\n{t}");
    }
    same_partition(&before, &nets(&s));
    // The partial ends moved with the wires: a second move stretches them again.
    s.move_to((170 * U, 0));
    s.key("m");
    s.move_to((190 * U, 0));
    s.click((190 * U, 0));
    assert!(
        s.text().contains("wire w00001 40 0 140 0\n"),
        "{}",
        s.text()
    );
    same_partition(&before, &nets(&s));
    s.key("u");
    s.key("u");
    assert_eq!(s.text(), original, "one undo per move");
    // Negative space: a plain box selects no ends, so a plain move leaves the wires.
    s.key("Esc");
    s.drag((90 * U, -45 * U), (190 * U, 40 * U), 0);
    s.move_to((150 * U, 0));
    s.key("m");
    s.click((170 * U, 0));
    let t = s.text();
    assert!(t.contains("  at 160 0 R0\n  name x2"), "{t}");
    assert!(t.contains("wire w00001 40 0 100 0\n"), "{t}");
}

// ---------------------------------------------------------------------------- vertices

/// Dragging an end of the one selected wire, a corner of a selected rect or a vertex of a
/// selected polygon moves that point only, one undo each; a drag that would collapse the
/// shape changes nothing.
#[test]
fn dragging_a_vertex_moves_only_that_point() {
    let mut s = Session::open("inv", "top.shcomp", "vertex", true);
    let original = s.text();
    s.click((30 * U, -70 * U)); // w00006, the VDD rail
    s.drag((140 * U, -70 * U), (140 * U, -90 * U), 0);
    let t = s.text();
    assert!(t.contains("wire w00006 0 -70 140 -90\n"), "{t}");
    let changed = t
        .lines()
        .zip(original.lines())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(changed, 1, "one record changed:\n{t}");
    s.key("u");
    assert_eq!(s.text(), original);
    // Negative space: dragging an end onto the other end is refused.
    s.click((30 * U, -70 * U));
    s.drag((140 * U, -70 * U), (0, -70 * U), 0);
    assert_eq!(s.text(), original, "{}", status(&s));
    assert!(status(&s).contains("extent"), "{}", status(&s));
    // A schematic rect: the dragged corner moves, the opposite one stays.
    s.key("Esc");
    s.move_to((300 * U, 50 * U));
    s.key("r");
    s.click((340 * U, 80 * U));
    assert!(s.text().contains(" rect 300 50 340 80\n"), "{}", s.text());
    s.drag((340 * U, 80 * U), (360 * U, 100 * U), 0);
    assert!(s.text().contains(" rect 300 50 360 100\n"), "{}", s.text());
    // A polygon vertex.
    s.key("Esc");
    s.move_to((300 * U, 120 * U));
    s.key("p");
    s.click((340 * U, 120 * U));
    s.click((340 * U, 160 * U));
    s.key("Enter");
    assert!(
        s.text().contains(" poly 300 120 340 120 340 160\n"),
        "{}",
        s.text()
    );
    s.drag((340 * U, 160 * U), (360 * U, 170 * U), 0);
    assert!(
        s.text()
            .contains(" poly 300 120 340 120 360 170\n  closed true\n"),
        "{}",
        s.text()
    );
}

// ---------------------------------------------------------------------------- stext

/// Symbol texts turn with the instance and with their own `stext` orientation (composed),
/// in instances and in the symbol view.
#[test]
fn symbol_texts_turn_with_instances_and_their_orientation() {
    let mut s = Session::open("inv", "top.shcomp", "stext", true);
    assert_eq!(text_rots(&s, "inv"), [0, 0], "x1 and x2 upright");
    s.click((10 * U, 0)); // x1
    s.key("Shift+R");
    assert!(s.text().contains("  at 0 0 R90\n  name x1"), "{}", s.text());
    let mut r = text_rots(&s, "inv");
    r.sort_unstable();
    assert_eq!(r, [0, 1], "x1's text turned a quarter, x2's did not");
    // The symbol's own text at R90: upright instances show it turned once, x1 twice.
    s.key("i");
    let sym = s.text();
    set_text(
        &mut s,
        &sym.replacen("stext -17 -5 R0 inv", "stext -17 -5 R90 inv", 1),
    );
    assert_eq!(text_rots(&s, "inv"), [1], "symbol view");
    s.key("Ctrl+E");
    let mut r = text_rots(&s, "inv");
    r.sort_unstable();
    assert_eq!(r, [1, 2], "x2: R0 ∘ R90, x1: R90 ∘ R90");
}

// ---------------------------------------------------------------------------- recent files

/// A recent file of another project switches the session to that project; unsaved edits
/// here refuse it.
#[test]
fn a_recent_file_of_another_project_switches_project() {
    let state = std::env::temp_dir().join(format!("schemify-state-switch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    let a = common::fixture("inv", "switch-a");
    let b = common::fixture("inv", "switch-b");
    let opts = |p: PathBuf| Options {
        path: Some(p),
        seed: 7,
        state_dir: Some(state.clone()),
        ..Default::default()
    };
    {
        let mut ed = Editor::new(opts(b.join("inv.shcomp"))).unwrap();
        ed.update(&InputEvents::default(), common::VIEW, 1);
    }
    let ed = Editor::new(opts(a.join("top.shcomp"))).unwrap();
    let mut s = Session {
        ed,
        now: 1,
        dir: a.clone(),
    };
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    assert_eq!(s.ed.recent(), [a.join("top.shcomp"), b.join("inv.shcomp")]);
    // Negative space: an unsaved edit here refuses the switch.
    s.click((150 * U, 0));
    s.key("Del");
    run(&mut s, "file.open_recent.1", "");
    assert!(status(&s).contains("save or undo"), "{}", status(&s));
    assert_eq!(s.ed.session.root, a);
    // Saved: the switch opens the file in project b.
    s.key("Ctrl+S");
    run(&mut s, "file.open_recent.1", "");
    assert_eq!(s.ed.session.root, b, "{}", status(&s));
    let (h, _) = s.ed.active().unwrap();
    assert_eq!(s.ed.session.path(h), Some(b.join("inv.shcomp").as_path()));
    assert_eq!(s.ed.session.tabs.len(), 1);
    assert_eq!(s.ed.recent()[0], b.join("inv.shcomp"));
    // Editing carries on in the new project: a delete and its undo.
    let t0 = s.text();
    s.click((50 * U, 0)); // wire w00002 of inv
    s.key("Del");
    assert_ne!(s.text(), t0, "{}", status(&s));
    s.key("u");
    assert_eq!(s.text(), t0);
    let _ = std::fs::remove_dir_all(&b);
    let _ = std::fs::remove_dir_all(&state);
}

// ---------------------------------------------------------------------------- config

/// `schemify.opt.layer_colors`, `dim` and `line_width` from `init.lua` reach the canvas.
#[test]
fn canvas_options_from_init_lua_reach_the_palette() {
    let dir = common::fixture("inv", "canvasopts");
    let init = dir.join("init.lua");
    std::fs::write(
        &init,
        "schemify.opt.layer_colors = { [4] = 'ff8000' }\nschemify.opt.dim = 0.25\nschemify.opt.line_width = 1.5\n",
    )
    .unwrap();
    let ed = Editor::new(Options {
        path: Some(dir.join("top.shcomp")),
        seed: 7,
        config: schemify_config::Startup::File(init),
        ..Default::default()
    })
    .unwrap();
    let mut s = Session { ed, now: 1, dir };
    s.frame(InputEvents::default());
    assert_eq!((s.ed.opts.dim, s.ed.opts.width_tenths), (25, 15));
    // Dimmed by 25% toward the background: the set color, not the theme's, is the source.
    let p = s.ed.view_palette();
    let bg = p.background >> 8;
    let mix =
        |c: u32, bg: u32| (f64::from(c) + (f64::from(bg) - f64::from(c)) * 0.25).round() as u32;
    let want = (mix(0xff, (bg >> 16) & 0xff) << 24)
        | (mix(0x80, (bg >> 8) & 0xff) << 16)
        | (mix(0x00, bg & 0xff) << 8)
        | 0xff;
    assert_eq!(p.color[4], want, "{:08x}", p.color[4]);
    // Reset colors (the session's own command) drops them for the session.
    run(&mut s, "view.reset_all_colors_to_default", "");
    assert_eq!((s.ed.opts.dim, s.ed.opts.width_tenths), (0, 10));
    assert_ne!(s.ed.view_palette().color[4], 0xff80_00ff);
}

// ---------------------------------------------------------------------------- subcircuit

/// Property: Ctrl+Shift+H on random selections keeps the parent's connectivity — objects
/// that stay are connected exactly as before — and each pin of the new instance sits on the
/// net it is named after; the new cell has a port per pin and inputs on the left; one undo.
#[test]
fn make_subcircuit_rewires_and_keeps_connectivity() {
    let picks = [
        (10 * U, 0),       // x1
        (150 * U, 0),      // x2
        (30 * U, -70 * U), // w00006, VDD rail
        (30 * U, 70 * U),  // w00009, VSS rail
        (50 * U, 0),       // w00001, mid
    ];
    let mut rng = Lcg(0x5eb5);
    for case in 0..8 {
        let mut s = Session::open("inv", "top.shcomp", &format!("subckt{case}"), true);
        let original = s.text();
        let before = nets(&s);
        let mask = 1 + rng.below(31);
        let mut first = true;
        for (k, &p) in picks.iter().enumerate() {
            if mask & (1 << k) != 0 {
                s.click_mods(p, if first { 0 } else { SHIFT });
                first = false;
            }
        }
        s.key("Ctrl+Shift+H");
        s.frame(InputEvents::default());
        let file = s.dir.join("top_sub1.shcomp");
        let sub = std::fs::read_to_string(&file)
            .unwrap_or_else(|_| panic!("case {case}: {}", status(&s)));
        let after = nets(&s);
        same_partition(&before, &after);
        // The new instance's pins, in the new cell's pin order, are on their nets.
        let pins: Vec<(String, String)> = lines(&sub, "pin ")
            .iter()
            .map(|l| {
                let w: Vec<&str> = l.split(' ').collect();
                (w[1].to_owned(), w[2].to_owned())
            })
            .collect();
        assert_eq!(lines(&sub, "port ").len(), pins.len(), "case {case}: {sub}");
        let (h, _) = s.ed.active().unwrap();
        let doc = s.ed.session.project.doc(h).unwrap();
        let st = doc.strings();
        let row = (0..doc.instances().id.len())
            .find(|&i| st.get(doc.instances().cell[i]) == "./top_sub1")
            .unwrap_or_else(|| panic!("case {case}: the instance\n{}", s.text()));
        let id = doc.instances().id[row].0;
        for (k, (name, dir)) in pins.iter().enumerate() {
            let pin = &after[&format!("i{id}.{k}")];
            assert_eq!(
                &pin.1, name,
                "case {case}: pin {k} on the net it is named after"
            );
            for (key, (_, was)) in &before {
                if was == name && after.contains_key(key) {
                    assert_eq!(after[key].0, pin.0, "case {case}: {key} joins pin {name}");
                }
            }
            assert!(["in", "out", "inout"].contains(&dir.as_str()));
        }
        s.key("u");
        assert_eq!(s.text(), original, "case {case}: one undo");
    }
}

/// Selecting x2 alone: its input on the left of the new symbol, output and supplies on the
/// right, the four nets wired to the new instance (no label added).
#[test]
fn make_subcircuit_symbol_sides_follow_pin_directions() {
    let mut s = Session::open("inv", "top.shcomp", "subcktdir", true);
    let labels = lines(&s.text(), "label ").len();
    s.click((150 * U, 0));
    s.key("Ctrl+Shift+H");
    let sub = std::fs::read_to_string(s.dir.join("top_sub1.shcomp")).unwrap();
    let pins = lines(&sub, "pin ");
    assert_eq!(pins.len(), 4, "{sub}");
    assert!(pins.iter().any(|l| l.starts_with("pin mid in -")), "{sub}");
    assert!(pins.iter().any(|l| l.starts_with("pin OUT out ")), "{sub}");
    assert!(
        pins.iter().any(|l| l.starts_with("pin VDD inout ")),
        "{sub}"
    );
    assert_eq!(
        lines(&s.text(), "label ").len(),
        labels,
        "wired, not labelled: {}",
        s.text()
    );
}

// ---------------------------------------------------------------------------- join/trim

/// Property: join/trim over random horizontal, vertical and diagonal wires, some `bus true`
/// and some with another attribute, keeps connectivity (a probe on every grid point any wire
/// covers lands in the same partition) and is idempotent.
#[test]
fn join_trim_keeps_connectivity_over_diagonal_and_attributed_wires() {
    let mut rng = Lcg(0xd1a9);
    for case in 0..8 {
        let mut s = Session::open("inv", "top.shcomp", &format!("trimdiag{case}"), true);
        let text = s.text();
        let mut wires = String::new();
        for k in 0..10 {
            let (x, y) = (300 + rng.below(6) as i32 * 10, rng.below(6) as i32 * 10);
            let len = 10 + rng.below(3) as i32 * 10;
            let (x2, y2) = match rng.below(4) {
                0 => (x + len, y),
                1 => (x, y + len),
                2 => (x + len, y + len),
                _ => (x + len, y - len),
            };
            wires.push_str(&format!("wire z{k:05} {x} {y} {x2} {y2}\n"));
            match rng.below(3) {
                0 => wires.push_str("  bus true\n"),
                1 => wires.push_str("  class power\n"),
                _ => {}
            }
        }
        let canonical = {
            let t = text.replacen("\nlabel ", &format!("\n{wires}label "), 1);
            let d = schemify_format::parse(t.as_bytes()).unwrap();
            schemify_format::write(&d)
        };
        set_text(&mut s, &canonical);
        let probes = |s: &Session| -> Vec<Vec<(i32, i32)>> {
            let (h, _) = s.ed.active().unwrap();
            let doc = s.ed.session.project.doc(h).unwrap();
            let cell = s.ed.cell().unwrap();
            let w = doc.wires();
            let mut by_net: BTreeMap<u32, Vec<(i32, i32)>> = BTreeMap::new();
            for x in (290..=400).step_by(10) {
                for y in (-40..=90).step_by(10) {
                    let p = (x * U, y * U);
                    for r in 0..w.id.len() {
                        let (a, b) = ((w.x1[r], w.y1[r]), (w.x2[r], w.y2[r]));
                        if p == a || p == b || schemify_app::wiring::on_interior(p, a, b) {
                            by_net
                                .entry(cell.nets.wire_net[r])
                                .or_default()
                                .push((x, y));
                        }
                    }
                }
            }
            let mut groups: Vec<_> = by_net
                .into_values()
                .map(|mut v| {
                    v.sort_unstable();
                    v.dedup();
                    v
                })
                .collect();
            groups.sort();
            groups
        };
        let before = probes(&s);
        s.key("&");
        assert_eq!(probes(&s), before, "case {case}: {wires}");
        let once = s.text();
        s.key("&");
        assert_eq!(s.text(), once, "case {case}: idempotent");
    }
}

/// Touching collinear diagonal wires merge, so do wires with equal attributes (kept); wires
/// whose attributes differ never merge.
#[test]
fn join_trim_merges_diagonals_and_equal_attributes_only() {
    let mut s = Session::open("inv", "top.shcomp", "trimattrs", true);
    let wires = "wire z00001 300 0 310 10\nwire z00002 310 10 320 20\n\
                 wire z00003 300 50 310 50\n  class power\nwire z00004 310 50 320 50\n  class power\n\
                 wire z00005 300 80 310 80\n  bus true\nwire z00006 310 80 320 80\n";
    let t = s
        .text()
        .replacen("\nlabel ", &format!("\n{wires}label "), 1);
    let canonical = schemify_format::write(&schemify_format::parse(t.as_bytes()).unwrap());
    set_text(&mut s, &canonical);
    s.key("&");
    let w = lines(&s.text(), "wire ")
        .iter()
        .map(|l| {
            l.split_once(' ')
                .unwrap()
                .1
                .split_once(' ')
                .unwrap()
                .1
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert!(
        w.contains(&"300 0 320 20".to_owned()),
        "diagonal merged: {w:?}"
    );
    assert!(
        w.contains(&"300 50 320 50".to_owned()),
        "equal attributes merged: {w:?}"
    );
    assert!(
        s.text().contains(" 300 50 320 50\n  class power\n"),
        "the attribute is kept: {}",
        s.text()
    );
    assert!(
        w.contains(&"300 80 310 80".to_owned()),
        "bus kept apart: {w:?}"
    );
    assert!(
        w.contains(&"310 80 320 80".to_owned()),
        "plain kept apart: {w:?}"
    );
    assert!(!w.contains(&"300 80 320 80".to_owned()));
}
