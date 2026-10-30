//! Keymap coverage against the xschem inventory (D25) and commands greyed out on the wrong
//! file kind.

use schemify_editor::commands::{CORE, Commands, Target};
use schemify_ui::Chord;

use super::node_prop;
use crate::common::{Session, Ui};

/// Expected chords from xschem's canvas key table (`fixtures/xschem_keys.md`, captured by
/// running xschem 3.4.7): every row except graph-only, cadence-only, placeholder and Tk-dialog rows.
fn inventory_chords() -> Vec<(String, String)> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/xschem_keys.md");
    let text = std::fs::read_to_string(path).expect("inventory");
    let start = text.find("### 2.1").expect("§2.1");
    let end = text[start..].find("### 2.2").expect("§2.2") + start;
    let mut out = Vec::new();
    let table_rows = text[start..end].lines().filter(|l| l.starts_with("| "));
    for row in table_rows {
        let cols: Vec<&str> = row
            .split(" | ")
            .map(|c| c.trim_matches(['|', ' ']))
            .collect();
        if cols.len() < 4 || cols[0] == "Key" || cols[0].starts_with("---") {
            continue;
        }
        let (key, m, ctx) = (cols[0], cols[1], cols[2]);
        if ctx == "G only" || ctx.contains("cad=1") || key.contains(';') || key == "Ctrl+Enter" {
            continue;
        }
        let mods = match m {
            "Ctrl" => "Ctrl+",
            "Alt" => "Alt+",
            "Ctrl+Alt" => "Ctrl+Alt+",
            "Shift" => "Shift+",
            _ => "", // –, (Shift), any
        };
        let keys: Vec<String> = if key == "0-9" {
            (0..10).map(|d| d.to_string()).collect()
        } else {
            vec![key.trim_matches('`').to_owned()]
        };
        for k in keys {
            let k = match k.as_str() {
                "Return" => "Enter".to_owned(),
                "Escape" => "Esc".to_owned(),
                "Delete" => "Del".to_owned(),
                "Insert" => "Ins".to_owned(),
                "\\\\" => "\\".to_owned(),
                s if s.len() == 1 && s.chars().all(|c| c.is_ascii_uppercase()) => {
                    format!("Shift+{s}")
                }
                s => s.to_owned(),
            };
            let chord = format!("{mods}{k}");
            out.push((chord, row.to_owned()));
        }
    }
    out
}

#[test]
fn keymap_covers_the_xschem_inventory() {
    let s = Session::open("inv", "top.shcomp", "keys", Ui::Default);
    let keymap = &s.ed.shell().tables().keymap;
    let expected = inventory_chords();
    assert!(
        expected.len() >= 150,
        "parsed {} inventory combos",
        expected.len()
    );
    // xschem functions with no meaning here keep no binding (docs/dev/backlog.md "Parity:
    // intentionally dropped"): the logic simulator (0-4), the launcher (Ctrl+h), insertion
    // order (Shift+S), floaters (Ctrl+u), X11 drawing modes ($, Ctrl+$).
    const DROPPED: [&str; 10] = [
        "0", "1", "2", "3", "4", "Ctrl+h", "Shift+S", "Ctrl+u", "$", "Ctrl+$",
    ];
    let mut missing = Vec::new();
    for (chord, row) in &expected {
        let c = Chord::parse(chord).unwrap_or_else(|| panic!("{chord} parses ({row})"));
        let dropped = DROPPED.contains(&chord.as_str());
        if keymap.lookup(c).is_none() != dropped {
            missing.push(chord.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "unbound (or dropped but bound) inventory chords: {missing:?}"
    );
    // How many bindings reach an implementation (core command or plugin command).
    let commands = Commands::default();
    let strings = &s.ed.shell().strings();
    let bound = keymap.commands();
    let implemented = bound
        .iter()
        .filter(|&&c| {
            let name = strings.get(c);
            !matches!(commands.resolve(name), Target::Unknown) || name.starts_with("help.")
        })
        .count();
    eprintln!(
        "keymap: {} inventory combos, {} bindings, {} reach an implementation, {} core commands",
        expected.len(),
        bound.len(),
        implemented,
        CORE.len()
    );
    // The editing, view and file keys of the M2 scope are all implemented.
    let required = [
        "w",
        "Shift+W",
        "m",
        "Ctrl+M",
        "Shift+M",
        "c",
        "Del",
        "Shift+R",
        "Shift+F",
        "Shift+V",
        "Alt+R",
        "Alt+F",
        "Alt+V",
        "u",
        "Shift+U",
        "Ctrl+S",
        "Ctrl+N",
        "Ctrl+Shift+N",
        "Ctrl+O",
        "Ctrl+W",
        "Ctrl+Q",
        "Alt+S",
        "z",
        "Shift+Z",
        "Ctrl+Z",
        "f",
        "Ctrl+Shift+F",
        "q",
        "Shift+Q",
        "Ctrl+Shift+Q",
        "Ins",
        "Shift+I",
        "Shift+Ins",
        "Ctrl+I",
        "e",
        "i",
        "Ctrl+E",
        "BackSpace",
        "Alt+E",
        "Alt+I",
        "Ctrl+A",
        "Ctrl+C",
        "Ctrl+X",
        "Ctrl+V",
        "Left",
        "Right",
        "Up",
        "Down",
        "Ctrl+Left",
        "Ctrl+Right",
        "Shift+P",
        "Esc",
        "Alt+L",
        "Alt+Shift+L",
        "Ctrl+P",
        "Ctrl+Shift+P",
        "Alt+P",
        "t",
        "l",
        "r",
        "g",
        "Shift+G",
        "%",
        "Shift+O",
        "Space",
        "h",
        "v",
        "d",
        "Shift+L",
        "n",
        "Shift+N",
        "s",
        "k",
        "Shift+K",
    ];
    for chord in required {
        let c = Chord::parse(chord).unwrap();
        let cmd = keymap
            .lookup(c)
            .unwrap_or_else(|| panic!("{chord} unbound"));
        let name = strings.get(cmd);
        assert!(
            matches!(commands.resolve(name), Target::Core(_)),
            "{chord} → {name} has no core implementation"
        );
    }
    assert!(implemented >= required.len());
}

/// Commands that act on other file kinds are greyed out (`disabled`, with a tooltip saying
/// what they need) in every menu, toolbar and bar that binds them, and refused from a key or
/// script with the same message; on the right file kind they are enabled.
#[test]
fn commands_grey_out_on_the_wrong_file_kind() {
    use schemify_ui::Prop;
    let disabled = |s: &Session, id: &str| node_prop(s, id, Prop::Disabled) == "1";
    // A component: no simulation, no optimizer; symbol generation available.
    let mut c = Session::open("inv", "top.shcomp", "grey-comp", Ui::Default);
    for id in [
        "sim.simulate",
        "sim.optimize",
        "menubar.simulate",
        "tb.simulate",
    ] {
        assert!(disabled(&c, id), "{id} greyed in a .shcomp");
    }
    assert!(node_prop(&c, "sim.simulate", Prop::Tooltip).contains(".shbench"));
    assert!(!disabled(&c, "symbol.make_symbol_from_schematic"));
    assert!(!disabled(&c, "sim.new_bench"));
    c.click_node("tb.simulate");
    assert!(!c.ed.sim().busy());
    c.run("sim.simulate", "");
    assert!(c.status().contains("needs a .shbench"), "{}", c.status());
    // A bench: simulation on, symbol generation and new-bench off.
    let b = Session::open("rc", "tb.shbench", "grey-bench", Ui::Default);
    for id in ["sim.simulate", "sim.optimize", "tb.simulate"] {
        assert!(!disabled(&b, id), "{id} enabled in a .shbench");
    }
    assert!(disabled(&b, "symbol.make_symbol_from_schematic"));
    assert!(disabled(&b, "sim.new_bench"));
    // A primitive: no schematic-to-symbol either.
    let p = Session::open("rc", "res.shprim", "grey-prim", Ui::Default);
    assert!(disabled(&p, "symbol.make_symbol_from_schematic"));
    assert!(disabled(&p, "sim.simulate"));
}
