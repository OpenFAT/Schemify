//! The live checks overlay, net highlight (buses by every bit), netlisting against the CLI's
//! bytes, and the netlist text tab.

use schemify_doc::{ObjId, TableId};
use schemify_editor::checks::SOURCE_CHECKS;
use schemify_editor::editor::{Editor, Options, Panel};
use schemify_editor::scene::ObjKey;
use schemify_ui::{InputEvents, Kind, Prop};

use super::{activate, marker_rects, node_kind, open, open_with};
use crate::common::{Session, U, fixture};

/// The checks overlay mirrors elab's violations of the active level: one mark per violation
/// (source `checks`), one marker per violation whose object is drawn; an edit that adds
/// violations adds marks, undo removes them.
#[test]
fn violation_overlay_follows_edits() {
    let mut s = open("inv", "top.shcomp", "checks", None);
    let check = |s: &Session| {
        let cell = s.ed.cell().expect("every shown cell is elaborated");
        let ann = s.ed.annotations();
        let marks = ann.count_of(SOURCE_CHECKS);
        assert_eq!(marks, cell.violations.rows.len());
        let drawn = cell
            .violations
            .rows
            .iter()
            .filter(|v| v.object != ObjId::NONE)
            .count();
        assert_eq!(marker_rects(s), drawn);
        cell.violations.rows.len()
    };
    let n0 = check(&s);
    // Move x2 far away (a plain drag; Ctrl+drag would stretch its wires along): its four
    // pins dangle (W004).
    s.drag((130 * U, 10 * U), (130 * U, 10 * U), 0);
    s.drag((130 * U, 10 * U), (130 * U, 300 * U), 0);
    let n1 = check(&s);
    assert!(n1 >= n0 + 4, "{n0} -> {n1}");
    let w004 =
        s.ed.cell()
            .unwrap()
            .violations
            .rows
            .iter()
            .filter(|v| v.code.as_str() == "W004")
            .count();
    assert!(w004 >= 4);
    s.key("u");
    assert_eq!(check(&s), n0, "undo removes the copy's violations");
    // The checks panel lists them; a row click selects its object.
    activate(&mut s, "view.checks");
    assert_eq!(s.ed.panel(), Panel::Checks);
    assert_eq!(node_kind(&s, "checks.pane"), Some(Kind::Panel));
    activate(&mut s, "view.checks");
    assert_eq!(s.ed.panel(), Panel::None);
    assert_eq!(node_kind(&s, "checks.pane"), None);
}

/// `k` highlights exactly elab's net (wires, labels, ports with that component, instance
/// pins on it); descending into a sub-cell keeps it highlighted through the port; `Shift+K`
/// clears.
#[test]
fn net_highlight_covers_exactly_the_net_across_the_hierarchy() {
    let mut s = open("inv", "top.shcomp", "hilite", None);
    // w00006 runs 0 -70 → 140 -70 on VDD; click its left part (the label sits at 70).
    s.click((20 * U, -70 * U));
    assert_eq!(s.ed.selection().len(), 1);
    s.key("k");
    let oracle = |s: &Session, comp_of: &dyn Fn(&schemify_elab::CellState) -> u32| {
        let cell = s.ed.cell().unwrap();
        let (h, _) = s.ed.active().unwrap();
        let doc = s.ed.session().project.doc(h).unwrap();
        // Elab's net: every component carrying the clicked component's bit (components
        // with the same name are one net, merged by name).
        let n = &cell.nets;
        let bit = n.bits_of(comp_of(cell) as usize)[0];
        let on = |c: u32| n.bits_of(c as usize).contains(&bit);
        let mut keys = Vec::new();
        for (ids, nets, table) in [
            (&doc.wires().id, &n.wire_net, TableId::Wires),
            (&doc.labels().id, &n.label_net, TableId::Labels),
            (&doc.ports().id, &n.port_net, TableId::Ports),
        ] {
            for (id, &k) in ids.iter().zip(nets) {
                if on(k) {
                    keys.push(ObjKey { table, key: id.0 });
                }
            }
        }
        keys.sort_unstable();
        let pins: usize = (0..doc.instances().id.len())
            .map(|i| n.pins(i).iter().filter(|&&k| on(k)).count())
            .sum();
        (keys, pins)
    };
    let top_comp = |cell: &schemify_elab::CellState| {
        let doc_row = 5; // w00006 is the sixth wire
        cell.nets.wire_net[doc_row]
    };
    let (keys, pins) = oracle(&s, &top_comp);
    let hl = s.ed.highlighted();
    assert!(!keys.is_empty());
    assert_eq!(hl.keys, keys, "{}", s.status());
    assert_eq!(hl.pins.len(), pins);
    assert_eq!(pins, 2, "x1.VDD and x2.VDD");
    assert_eq!(s.ed.highlight_keys(), ["vdd"]);
    // Descend into x1: the child's VDD (port + label + wires) is the same net.
    s.click((-10 * U, 0));
    s.key("e");
    assert_eq!(s.ed.session().hier_path(), ".x1.");
    let child_comp = |cell: &schemify_elab::CellState| {
        let row = 0; // label n00001 VDD
        cell.nets.label_net[row]
    };
    let (keys, pins) = oracle(&s, &child_comp);
    assert_eq!(s.ed.highlighted().keys, keys);
    assert_eq!(s.ed.highlighted().pins.len(), pins);
    assert!(!keys.is_empty());
    s.key("Ctrl+E");
    s.key("Shift+K");
    assert!(s.ed.highlight_keys().is_empty());
    assert!(s.ed.highlighted().keys.is_empty());
}

/// `n` writes `.schemify/netlist/top.spice` with the same bytes `schemify netlist top.shcomp`
/// prints, and opens it in a read-only text tab; an expression error blocks it (D13) and
/// lists the error in the checks panel.
#[test]
fn netlist_command_writes_the_cli_bytes() {
    let mut s = open("inv", "top.shcomp", "netlist", None);
    s.key("n");
    let file = s.dir.join(".schemify/netlist/top.spice");
    let bytes = std::fs::read(&file).unwrap_or_else(|e| panic!("{e}: {}", s.status()));
    // The CLI's own load + netlist spine (`schemify netlist top.shcomp`; the binary lives in
    // schemify-cli since the D39 split, so its spine is called directly).
    let (mut o, _) = schemify_cli::spines::open_file(&s.dir.join("top.shcomp"), 1).unwrap();
    let env = schemify_cli::spines::env_pairs();
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let req = schemify_elab::Request {
        config: None,
        corner: None,
        overrides: &[],
        env: &env,
        dialect: schemify_elab::Dialect::Ngspice,
    };
    let top = o.top;
    let e = schemify_cli::spines::elaborate_all(&mut o, top, &req, &mut schemify_elab::no_funcs)
        .unwrap();
    let cli = schemify_netlist::emit(&e, schemify_elab::Dialect::Ngspice).text;
    assert_eq!(bytes, cli.as_bytes(), "editor and CLI netlists differ");
    let view =
        s.ed.session()
            .text_view()
            .expect("a text tab shows the netlist");
    assert_eq!(view.body, String::from_utf8(bytes).unwrap().trim_end());
    assert_eq!(node_kind(&s, "textview.body"), Some(Kind::Prose));
    assert_eq!(
        node_kind(&s, "canvas"),
        None,
        "the text tab replaces the canvas"
    );
    // Editing keys do nothing on a text tab.
    let before = s.text();
    s.key("Del");
    assert_eq!(s.text(), before);
    // Another dialect, another file.
    activate(&mut s, "tab.0");
    activate(&mut s, "sim.dialect.cdl");
    s.key("n");
    assert!(s.dir.join(".schemify/netlist/top.cdl").exists());
    // Gating: an expression error in an override blocks the netlist.
    let dir = fixture("inv", "netlist-gate");
    let top = std::fs::read_to_string(dir.join("top.shcomp")).unwrap();
    std::fs::write(
        dir.join("top.shcomp"),
        top.replace("  wp 4", "  wp \"1 +\""),
    )
    .unwrap();
    let ed = Editor::new(Options {
        path: Some(dir.join("top.shcomp")),
        seed: 7,
        ..Options::default()
    })
    .unwrap();
    let mut s = Session {
        ed,
        now: 1,
        dir,
        caches: Default::default(),
    };
    s.frame(InputEvents::default());
    s.key("n");
    assert!(s.status().contains("blocked"), "{}", s.status());
    assert!(!s.dir.join(".schemify/netlist/top.spice").exists());
    assert_eq!(s.ed.panel(), Panel::Checks);
    assert_eq!(node_kind(&s, "checks.blocked.0"), Some(Kind::Label));
}

/// A bus highlights by every bit: `k` on the single-bit wire `d[0]` lights the bus `d[1:0]`
/// that carries it (not `d[1]`, which only the bus shares); `k` on the bus lights both bit
/// wires. The oracle is elab's membership: components carrying any highlighted bit.
#[test]
fn bus_components_highlight_by_every_bit() {
    let bench = "schemify 1 shbench\n\nwire 0w0001 0 0 100 0\n  bus true\nwire 0w0002 0 100 100 100\n\
                 wire 0w0003 0 200 100 200\n\nlabel 0n0001 0 0 R0 d[1:0]\nlabel 0n0002 0 100 R0 d[0]\n\
                 label 0n0003 0 200 R0 d[1]\n";
    let mut s = open_with(
        "inv",
        &[("bus.shbench", bench)],
        "bus.shbench",
        "bus-hilite",
        None,
    );
    let keys = |s: &Session, rows: &[usize]| {
        let (h, _) = s.ed.active().unwrap();
        let doc = s.ed.session().project.doc(h).unwrap();
        let mut k: Vec<ObjKey> = rows
            .iter()
            .flat_map(|&r| {
                [
                    ObjKey {
                        table: TableId::Wires,
                        key: doc.wires().id[r].0,
                    },
                    ObjKey {
                        table: TableId::Labels,
                        key: doc.labels().id[r].0,
                    },
                ]
            })
            .collect();
        k.sort_unstable();
        k
    };
    // Rows follow the ids: 0 the bus, 1 `d[0]`, 2 `d[1]`.
    s.click((50 * U, 100 * U));
    assert_eq!(s.ed.selection().len(), 1);
    s.key("k");
    assert_eq!(s.ed.highlighted().keys, keys(&s, &[0, 1]), "{}", s.status());
    s.key("Shift+K");
    assert!(s.ed.highlighted().keys.is_empty());
    s.click((50 * U, 0));
    s.key("k");
    assert_eq!(s.ed.highlight_keys().len(), 2, "one key per bit");
    assert_eq!(s.ed.highlighted().keys, keys(&s, &[0, 1, 2]));
}

/// Find in the netlist tab: the numbered lines of the whole file that contain the text,
/// case-insensitively; an empty find shows the netlist again.
#[test]
fn netlist_tab_finds_lines_of_the_whole_file() {
    let mut s = open("inv", "top.shcomp", "find", None);
    s.key("n");
    let text = std::fs::read_to_string(s.dir.join(".schemify/netlist/top.spice")).unwrap();
    assert!(s.ed.session().text_view().is_some(), "{}", s.status());
    let find = |s: &mut Session, q: &str| {
        s.click_node("textview.find");
        s.key("Ctrl+A");
        s.key("Del");
        if !q.is_empty() {
            s.type_text(q);
        }
        s.key("Enter");
    };
    find(&mut s, "SUBCKT");
    let want: Vec<String> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| l.to_ascii_lowercase().contains("subckt"))
        .map(|(i, l)| format!("{:>6}: {l}", i + 1))
        .collect();
    assert!(!want.is_empty(), "{text}");
    let (count, lines) = s.ed.session().text_view().unwrap().found.clone().unwrap();
    assert_eq!(count as usize, want.len());
    assert_eq!(lines.lines().collect::<Vec<_>>(), want);
    assert_eq!(node_kind(&s, "textview.found"), Some(Kind::Label));
    find(&mut s, "no such text anywhere");
    let (count, lines) = s.ed.session().text_view().unwrap().found.clone().unwrap();
    assert_eq!((count, lines.as_str()), (0, ""));
    find(&mut s, "");
    assert!(s.ed.session().text_view().unwrap().found.is_none());
    assert_eq!(node_kind(&s, "textview.found"), None);
}

/// Children of `textview` and the source lines of its prose body.
fn text_tab_nodes(s: &Session) -> (usize, Vec<String>) {
    let sh = &s.ed.shell();
    let tv = sh.tables().nodes.find(
        sh.strings()
            .find("textview")
            .expect("the UI names the node"),
    );
    let kids = sh
        .tables()
        .nodes
        .children(tv.expect("the node is composed"))
        .count();
    let body = sh
        .strings()
        .find("textview.body")
        .expect("the UI names the node");
    let body = sh.tables().nodes.find(body).expect("the node is composed");
    let src = sh
        .strings()
        .get(sh.tables().nodes.prop(Prop::Value)[body as usize]);
    (kids, src.lines().map(str::to_owned).collect())
}

fn open_text(s: &mut Session, body: String) {
    let path = s.dir.join("big.txt");
    s.ed.open_text_tab(schemify_editor::session::TextView {
        title: "big".into(),
        path,
        body,
        find: String::new(),
        found: None,
    });
    s.frame(InputEvents::default());
}

/// A 16k-line text tab is windowed: the body node holds at most `TEXT_WINDOW` lines and the
/// tab a bounded number of nodes; its pixels equal those of a short (unwindowed) tab holding
/// the same lines from a little above the screen; a line that wraps stops the window from
/// starting below it; scrolled to the end it shows the last line.
#[test]
fn a_long_text_tab_lays_out_a_window_of_lines() {
    use schemify_editor::session::TEXT_WINDOW;
    let mut s = Session::open("inv", "top.shcomp", "textwin", true);
    let mut body: Vec<String> = (0..16_384)
        .map(|i| format!("R{i} n{i} n{} {}k", i + 1, i % 97))
        .collect();
    body.push("… (truncated; the whole netlist is in big.txt)".into());
    let n = body.len();
    let first = |lines: &[String]| body.iter().position(|l| *l == lines[1]).unwrap();
    open_text(&mut s, body.join("\n"));
    let tv =
        s.ed.shell()
            .strings()
            .find("textview")
            .expect("the text tab composed");
    let at = 10_000.0 * 12.0;
    s.ed.set_scroll(tv, at, 1e9);
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    let (kids, lines) = text_tab_nodes(&s);
    let bound = TEXT_WINDOW as usize + 2;
    assert!(lines.len() <= bound, "{} lines in the body", lines.len());
    assert!(
        (9_900..10_000).contains(&first(&lines)),
        "{}",
        first(&lines)
    );
    // find + body + spacers of at most 2^14 px each over 16k rows of 12 px.
    assert!(kids <= 2 + 2 * (n * 12 / (1 << 14) + 1), "{kids} nodes");
    let windowed = s.render();
    // Oracle: the lines from 9960 on as a short tab, scrolled by as much less.
    open_text(&mut s, body[9_960..10_200].join("\n"));
    s.ed.set_scroll(tv, at - 9_960.0 * 12.0, 1e9);
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    let (kids, lines) = text_tab_nodes(&s);
    assert_eq!(
        (kids, lines.len()),
        (2, 242),
        "the oracle is one unwindowed node"
    );
    let oracle = s.render();
    assert!(
        oracle.data() == windowed.data(),
        "the window paints what the whole text does"
    );
    // Negative space: a line that wraps (here at 5000) is never above the window.
    let mut wraps = body.clone();
    wraps[5_000] = "word ".repeat(400);
    open_text(&mut s, wraps.join("\n"));
    s.ed.set_scroll(tv, at, 1e9);
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    let (_, lines) = text_tab_nodes(&s);
    assert_eq!(
        lines[1], wraps[5_000],
        "the window starts at the wrapping line"
    );
    assert!(lines.len() > 5_000, "and reaches the screen");
    open_text(&mut s, body.join("\n"));
    // The end: the last line is in the window.
    s.ed.set_scroll(tv, 1e9, 1e9);
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    let (_, lines) = text_tab_nodes(&s);
    assert!(lines.len() <= bound);
    assert_eq!(lines[lines.len() - 2], body[n - 1]);
    assert_eq!(node_kind(&s, "textview.gap-bottom.0"), None);
    assert_eq!(node_kind(&s, "textview.gap-top.0"), Some(Kind::Panel));
}

/// A short text tab is one body node with the whole text and no spacers, as before windowing.
#[test]
fn a_short_text_tab_is_one_node() {
    let mut s = Session::open("inv", "top.shcomp", "textshort", true);
    let body = "* netlist\nR1 a b 1k\n.end".to_owned();
    open_text(&mut s, body.clone());
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    let (kids, lines) = text_tab_nodes(&s);
    assert_eq!(lines.join("\n"), format!("```\n{body}\n```"));
    assert_eq!(kids, 2, "find and body");
    assert_eq!(node_kind(&s, "textview.gap-top.0"), None);
}
