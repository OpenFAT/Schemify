//! Composition (D8): last wins with health rows, immediate-mode recomposition, live strings
//! that do not grow the table, and `patch_last` equal to a full `compose`.
//!
//! Contents: no `pub` items.

use schemify_ui::{Chord, HealthKind, NO_NODE, Prop, UiTables, compose, records};

use crate::fixture;

// ---- private ----

#[test]
fn default_ui_composes_without_health_rows() {
    let f = fixture(&[]);
    assert!(f.tables.health.is_empty(), "{:?}", f.tables.health);
    let n = &f.tables.nodes;
    assert!(n.len() > 300, "xschem shell has {} nodes", n.len());
    // Pre-order invariants: parents precede children; subtree ends nest.
    for i in 0..n.len() {
        let p = n.parent()[i];
        assert!(p == NO_NODE || (p as usize) < i);
        assert!(n.end()[i] as usize > i && n.end()[i] as usize <= n.len());
        if p != NO_NODE {
            assert!(n.end()[i] <= n.end()[p as usize]);
        }
    }
    let titles: Vec<&str> = n
        .children(f.node("menubar"))
        .map(|c| f.strings.get(n.prop(Prop::Label)[c as usize]))
        .collect();
    assert_eq!(
        titles,
        [
            "File",
            "Edit",
            "Options",
            "View",
            "Properties",
            "Layers",
            "Tools",
            "Symbol",
            "Highlight",
            "Simulation",
            "Help",
            "-",
            "Netlist",
            "Simulate",
            "Waves"
        ]
    );
    assert_eq!(
        f.tables.keymap.lookup(Chord::parse("Ctrl+S").unwrap()),
        Some(f.sym("file.save"))
    );
    assert_eq!(
        f.tables.keymap.lookup(Chord::parse("shift+z").unwrap()),
        Some(f.sym("view.zoom_in"))
    );
    assert_eq!(
        f.tables.keymap.lookup(Chord::parse("%").unwrap()),
        Some(f.sym("options.draw_grid"))
    );
}

#[test]
fn plugins_override_last_wins_with_health_report() {
    let p1 = "set file.save label \"Save!\"\nremove menu.help\nnode tools.count_nets item\n  parent menu.tools\n  label \"Count nets\"\n  command count_nets.run\n  order -1\nstyle item\n  pad 9\nkey Ctrl+S count_nets.run\nset ghost label x\nnode stray item\n  parent nowhere\n";
    let p2 = "set file.save label \"Save!!\"\n";
    let f = fixture(&[p1, p2]);
    let h = &f.tables.health;
    let rows: Vec<(HealthKind, &str, u8, u8)> = (0..h.len())
        .map(|i| (h.kind[i], f.strings.get(h.target[i]), h.by[i], h.over[i]))
        .collect();
    assert_eq!(
        rows,
        vec![
            (HealthKind::PropOverridden, "file.save", 1, 0),
            (HealthKind::NodeRemoved, "menu.help", 1, 0),
            (HealthKind::StyleOverridden, "item", 1, 0),
            (HealthKind::KeyOverridden, "file.save", 1, 0),
            (HealthKind::UnknownTarget, "ghost", 1, 1),
            (HealthKind::PropOverridden, "file.save", 2, 0),
            (HealthKind::Orphaned, "stray", 1, 1),
        ]
    );
    let n = &f.tables.nodes;
    assert_eq!(
        f.strings
            .get(n.prop(Prop::Label)[f.node("file.save") as usize]),
        "Save!!",
        "last wins"
    );
    assert!(
        n.find(f.sym("menu.help")).is_none() && n.find(f.sym("help.about")).is_none(),
        "subtree removed"
    );
    let first_tool = n.children(f.node("menu.tools")).next().unwrap();
    assert_eq!(
        n.id()[first_tool as usize],
        f.sym("tools.count_nets"),
        "order -1 sorts first"
    );
    assert_eq!(
        f.tables.keymap.lookup(Chord::parse("Ctrl+S").unwrap()),
        Some(f.sym("count_nets.run"))
    );
    assert_eq!(n.owner()[f.node("tools.count_nets") as usize], 1);
}

#[test]
fn recomposition_is_immediate_mode() {
    let with = fixture(&["remove menu.file\n"]);
    assert!(with.tables.nodes.find(with.sym("file.save")).is_none());
    let without = fixture(&[]);
    assert!(
        without
            .tables
            .nodes
            .find(without.sym("file.save"))
            .is_some(),
        "dropping a plugin restores the base"
    );
}

/// Live strings: a state contribution whose text changes every frame keeps the permanent
/// table fixed (plain `parse` grows it by one string per frame: the negative space), and the
/// recomposed tables read each frame's text.
#[test]
fn state_text_that_changes_every_frame_does_not_grow_strings() {
    let mut f = fixture(&[]);
    let (base, _) = records::parse(schemify_ui::DEFAULT_UI, &mut f.strings);
    let name = f.strings.intern("app.state");
    let frame = |i: u32| {
        format!(
            "set status.msg label \"mouse = {i} {}\"\nset tab.0 label top.shcomp\n",
            i * 7
        )
    };
    let len = f.strings.len();
    for i in 0..2000 {
        let (c, rej) = records::parse_live(&frame(i), &mut f.strings);
        assert!(rej.is_empty());
        f.tables = compose(&f.strings, &base, &[(name, &c)]);
        let label = f.tables.nodes.prop(Prop::Label)[f.node("status.msg") as usize];
        assert!(label.is_live());
        assert_eq!(f.strings.get(label), format!("mouse = {i} {}", i * 7));
        assert_eq!(
            f.strings.live_len(),
            2,
            "one generation: the message and the tab"
        );
    }
    assert_eq!(f.strings.len(), len, "the permanent table did not grow");
    for i in 0..10 {
        records::parse(&frame(5000 + i), &mut f.strings);
    }
    assert_eq!(
        f.strings.len(),
        len + 11,
        "permanent parsing grows per new text: ten messages and the tab"
    );
}

/// Every column a frame reads of two composed tables, compared (the node index is a hash map;
/// `find` of every id stands for it).
fn same_tables(a: &UiTables, b: &UiTables) -> bool {
    let (n, m) = (&a.nodes, &b.nodes);
    n.id() == m.id()
        && n.kind() == m.kind()
        && n.parent() == m.parent()
        && n.end() == m.end()
        && n.owner() == m.owner()
        && Prop::ALL.iter().all(|&p| n.prop(p) == m.prop(p))
        && n.id().iter().all(|&id| n.find(id) == m.find(id))
        && a.health == b.health
        && a.keymap.chords() == b.keymap.chords()
        && a.keymap.commands() == b.keymap.commands()
        && a.owners == b.owners
}

/// `patch_last`: a state contribution that differs from the last one only in `set` values
/// (the pointer coordinates, a tab title, a dynamic row's label, a duplicate set, a set of an
/// unknown node) patches the composed tables to exactly what `compose` builds; any other
/// difference (a new node, a changed target, a `style` set, a re-declaration after the set)
/// is refused and leaves the tables untouched.
#[test]
fn patch_last_equals_compose() {
    let mut f = fixture(&[]);
    let (base, _) = records::parse(schemify_ui::DEFAULT_UI, &mut f.strings);
    let name = f.strings.intern("app.state");
    let state = |i: u32, title: &str| {
        format!(
            "set tab.0 label {title}\nnode row.x label\n  parent status\n  label \"r{}\"\n\
             set status.msg label \"mouse = {i} {}\"\nset nosuch.node label v{i}\n\
             set status.msg label \"last {}\"\n",
            i % 3,
            i * 7,
            i % 5
        )
    };
    let mut old = records::parse_live(&state(0, "a"), &mut f.strings).0;
    let mut tables = compose(&f.strings, &base, &[(name, &old)]);
    let mut patched = 0;
    for i in 1..300u32 {
        let title = ["a", "b.shcomp", "c*"][(i % 3) as usize];
        let (new, rej) = records::parse_live(&state(i, title), &mut f.strings);
        assert!(rej.is_empty());
        assert!(
            schemify_ui::patch_last(&mut tables, &old, &new),
            "frame {i}: values only"
        );
        let full = compose(&f.strings, &base, &[(name, &new)]);
        assert!(
            same_tables(&tables, &full),
            "frame {i}: patched == composed"
        );
        patched += usize::from(old != new);
        old = new;
    }
    assert!(patched > 250, "most frames changed a value: {patched}");
    // Negative space: each of these is refused and changes nothing.
    let redeclared = |v: u32| format!("set row.y label v{v}\nnode row.y label\n  parent status\n");
    let refused = [
        (
            state(1, "a"),
            format!("{}node extra label\n  parent status\n", state(1, "a")),
        ),
        (
            state(1, "a"),
            state(1, "a").replace("set nosuch.node", "set status.grid"),
        ),
        (
            state(1, "a"),
            state(1, "a").replace("set tab.0 label a", "set tab.0 style a"),
        ),
        (
            state(1, "a"),
            state(1, "a").replace("set status.msg label \"last 1\"", "node status.msg label"),
        ),
        (redeclared(1), redeclared(2)),
    ];
    for (was, text) in refused {
        let (old, _) = records::parse_live(&was, &mut f.strings);
        let before = compose(&f.strings, &base, &[(name, &old)]);
        let (new, _) = records::parse_live(&text, &mut f.strings);
        let mut t = before.clone();
        assert!(!schemify_ui::patch_last(&mut t, &old, &new), "{text}");
        assert!(same_tables(&t, &before), "a refusal changes nothing");
    }
}
