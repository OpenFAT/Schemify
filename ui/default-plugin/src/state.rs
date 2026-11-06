//! The session-state projection: the `Tabs`, `Tool`, `Status`, `Options` and `Dialog` /
//! `DialogRows` snapshot tables in → the `state` record body that fills this UI's tabs,
//! status bar, option check marks and dialogs out (plugin-abi §4, §7 `state`). The words,
//! marks and titles are this plugin's; the core supplies only data, so another UI plugin
//! reading the same tables can show them its own way.
//!
//! Contents:
//! - `pub(crate) fn render(host: &Host) -> String`

use std::fmt::Write as _;

use schemify_abi::guest::Host;
use schemify_abi::text::push_value;
use schemify_abi::{Scalar, Table, col};

/// The projection of the current session state (an empty table renders nothing).
pub(crate) fn render(host: &Host) -> String {
    let mut t = String::with_capacity(2048);
    tabs(host, &mut t);
    status(host, &mut t);
    dialog(host, &mut t);
    debug_assert!(t.is_empty() || t.ends_with('\n'), "whole record lines");
    t
}

// ---- private ----

/// One column of `table`, every row; empty when the read fails (an older host).
fn ints<T: Scalar>(host: &Host, table: Table, column: u32) -> Vec<T> {
    let n = host.len(table).unwrap_or(0);
    host.column::<T>(table, column, 0..n).unwrap_or_default()
}

/// One string column of `table`, every row; empty when the read fails.
fn strs(host: &Host, table: Table, column: u32) -> Vec<String> {
    let n = host.len(table).unwrap_or(0);
    host.strings(table, column, 0..n).unwrap_or_default()
}

/// `<prefix><value>\n` with `value` quoted as needed.
fn line(t: &mut String, prefix: &str, value: &str) {
    t.push_str(prefix);
    push_value(t, value);
    t.push('\n');
}

/// The tab bar: `tab.0` is this UI's own first tab, the rest are added under `tabs`.
fn tabs(host: &Host, t: &mut String) {
    let title = strs(host, Table::Tabs, col::tabs::TITLE);
    let active = ints::<u8>(host, Table::Tabs, col::tabs::ACTIVE);
    let dirty = ints::<u8>(host, Table::Tabs, col::tabs::DIRTY);
    let symbol = ints::<u8>(host, Table::Tabs, col::tabs::SYMBOL);
    if active.len() != title.len() || dirty.len() != title.len() || symbol.len() != title.len() {
        return; // a torn read: keep the last projection's tabs
    }
    for (i, name) in title.iter().enumerate() {
        let mut name = name.clone();
        if symbol[i] == 1 {
            name.push_str(" (symbol)");
        }
        if dirty[i] == 1 {
            name.push('*');
        }
        let checked = active[i];
        if i == 0 {
            line(t, "set tab.0 label ", &name);
            let _ = writeln!(t, "set tab.0 checked {checked}\nset tab.0 order -1");
        } else {
            let _ = writeln!(t, "node tab.{i} tab\n  parent tabs");
            line(t, "  label ", &name);
            let _ = writeln!(t, "  command tab.select\n  checked {checked}\n  order -1");
        }
    }
}

/// The status bar (snap, grid, the message line) and the option check marks.
fn status(host: &Host, t: &mut String) {
    let names = strs(host, Table::Options, col::options::NAME);
    let values = strs(host, Table::Options, col::options::VALUE);
    let opt = |name: &str| {
        let i = names.iter().position(|n| n == name)?;
        values.get(i).map(String::as_str)
    };
    let num = |name: &str| opt(name).and_then(|v| v.parse::<i32>().ok());
    if let Some(snap) = num("snap") {
        line(t, "set status.snap value ", &coord(snap));
    }
    if let Some(grid) = num("grid") {
        line(t, "set status.grid value ", &coord(grid));
    }
    let tool = strs(host, Table::Tool, col::tool::NAME);
    let banner = match tool.first().map(String::as_str) {
        Some("wire") => "DRAW WIRE! ",
        Some("rect") => "DRAW RECTANGLE! ",
        Some("line") => "DRAW LINE! ",
        Some("poly") => "DRAW POLYGON! ",
        Some("arc") => "DRAW ARC! ",
        Some("circle") => "DRAW CIRCLE! ",
        Some("unselect") => "UNSELECT AREA! ",
        Some("copy") => "COPY! ",
        Some("move") => "MOVE! ",
        Some("place") => "PLACE! ",
        Some("zoom") => "ZOOM BOX! ",
        _ => "",
    };
    let x = ints::<i32>(host, Table::Status, col::status::X);
    let y = ints::<i32>(host, Table::Status, col::status::Y);
    let selected = ints::<u32>(host, Table::Status, col::status::SELECTED);
    let path = strs(host, Table::Status, col::status::PATH);
    let message = strs(host, Table::Status, col::status::MESSAGE);
    if let ([x], [y], [n], [path], [message]) =
        (&x[..], &y[..], &selected[..], &path[..], &message[..])
    {
        let msg = format!(
            "{banner}mouse = {} {}  selected: {n}  path: {path}  {message}",
            coord(*x),
            coord(*y)
        );
        line(t, "set status.msg label ", &msg);
    }
    let flags = [
        ("options.draw_grid", opt("show_grid") == Some("1")),
        (
            "options.enable_orthogonal_wiring",
            opt("orthogonal") == Some("1"),
        ),
        ("edit.unconstrained_move", opt("constrain") == Some("0")),
        (
            "edit.constrained_horizontal_move",
            opt("constrain") == Some("1"),
        ),
        (
            "edit.constrained_vertical_move",
            opt("constrain") == Some("2"),
        ),
    ];
    if names.is_empty() {
        return;
    }
    for (id, on) in flags {
        let _ = writeln!(t, "set {id} checked {}", u8::from(on));
    }
}

/// The open dialog's title, field and rows (enter text, chooser, properties).
fn dialog(host: &Host, t: &mut String) {
    let kind = strs(host, Table::Dialog, col::dialog::KIND);
    let Some(kind) = kind.first() else {
        return;
    };
    let one = |column| {
        strs(host, Table::Dialog, column)
            .into_iter()
            .next()
            .unwrap_or_default()
    };
    let (purpose, title, text) = (
        one(col::dialog::PURPOSE),
        one(col::dialog::TITLE),
        one(col::dialog::TEXT),
    );
    let label = strs(host, Table::DialogRows, col::dialog_rows::LABEL);
    match kind.as_str() {
        "text" => {
            line(t, "set dialog.enter_text.title label ", &title);
            line(t, "set dialog.enter_text.input value ", &text);
            t.push_str("set dialog.enter_text.input input field.typed\n");
        }
        "choose" => {
            let note = one(col::dialog::NOTE);
            let title = match purpose.as_str() {
                "insert" => "Insert symbol (type to filter, click to place)".to_owned(),
                "open" => "Open cell".to_owned(),
                "waves" if !note.is_empty() => {
                    format!("Open waveform file for {note} (.raw; or type a path + Enter)")
                }
                "waves" => "Open waveform file (.raw; or type a path + Enter)".to_owned(),
                "merge" => "Merge: insert a cell's schematic at the pointer".to_owned(),
                "delete" => "Delete a cell file (pick twice to confirm)".to_owned(),
                "compare" => "Compare the active schematic with".to_owned(),
                _ => title,
            };
            line(t, "set dialog.choose.title label ", &title);
            line(t, "set dialog.choose.filter value ", &text);
            t.push_str("set dialog.choose.filter input field.typed\n");
            for (i, l) in label.iter().enumerate() {
                let _ = writeln!(
                    t,
                    "node dialog.choose.row.{i} item\n  parent dialog.choose.list"
                );
                line(t, "  label ", l);
                t.push_str("  command choose.pick\n  style list-row\n");
            }
        }
        "props" => {
            let value = strs(host, Table::DialogRows, col::dialog_rows::VALUE);
            let editable = ints::<u8>(host, Table::DialogRows, col::dialog_rows::EDITABLE);
            if value.len() != label.len() || editable.len() != label.len() {
                return;
            }
            line(t, "set dialog.props.title label ", &title);
            for (i, l) in label.iter().enumerate() {
                let _ = writeln!(
                    t,
                    "node dialog.props.row.{i} panel\n  parent dialog.props.fields\n  style dialog-row"
                );
                let _ = writeln!(
                    t,
                    "node dialog.props.key.{i} label\n  parent dialog.props.row.{i}"
                );
                line(t, "  label ", l);
                t.push_str("  style dialog-key\n");
                let kind = if editable[i] == 1 {
                    "text_input"
                } else {
                    "label"
                };
                let _ = writeln!(
                    t,
                    "node dialog.props.val.{i} {kind}\n  parent dialog.props.row.{i}"
                );
                if editable[i] == 1 {
                    line(t, "  value ", &value[i]);
                    t.push_str(
                        "  command props.field\n  input field.typed\n  style dialog-input\n",
                    );
                } else {
                    t.push_str("  label \"(computed)\"\n");
                }
            }
        }
        _ => {}
    }
}

/// `milli` milli-units as the file format writes a coordinate: `10`, `-2.5`, `0.001`.
fn coord(milli: i32) -> String {
    let mag = i64::from(milli).unsigned_abs();
    let (whole, frac) = (mag / 1000, mag % 1000);
    let sign = if milli < 0 { "-" } else { "" };
    let mut s = if frac == 0 {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{frac:03}")
    };
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    debug_assert!(!s.is_empty() && !s.ends_with('.'));
    s
}
