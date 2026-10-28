//! The panel as UI records: header, pins, params, impls (with bodies), the pending port
//! sync's diff, and the bench sections, each field a node whose id names its target.
//!
//! Contents:
//! - `pub fn contribute`
//! - `pub(super) fn sync_rows`
//! - `pub(super) fn section`
//! - `pub(super) fn note`
//! - `pub(super) fn block`
//! - `pub(super) fn input`
//! - `pub(super) fn dropdown`
//! - `pub(super) fn button`

use schemify_doc::{Document, Loc, PinDir, TableId};
use schemify_format::FileKind;

use crate::docs::{attr, node};

use super::bench_panel::bench_sections;
use super::ops::{add_impl_ops, sync_ops};
use super::{
    ADD_LANGS, DIALECTS, PARAM_ATTRS, PARAM_KINDS, PIN_DIRS, PIN_DOMAINS, PortSync, SyncPlan,
    attr_of, body_row, body_text, names,
};

/// Projects `doc` into the panel: rows under the plugin-declared `cell.page` node. A file
/// kind without an interface (a `.shbench`) shows only its header. `sync`, a pending
/// "Sync pins from ports" of `doc`, shows its diff with Apply / Cancel under its impl.
pub fn contribute(t: &mut String, doc: &Document, sync: Option<&PortSync>) {
    let kind = doc.kind();
    let cellish = matches!(kind, FileKind::Shprim | FileKind::Shcomp);
    let impls = names(doc, TableId::Impls, doc.impls().kind.len());

    section(t, "cell.s.hdr", "Cell");
    let mut keys = vec!["doc", "category"];
    if cellish {
        keys.push("prefix");
    } else if kind == FileKind::Shbench {
        keys.extend(["corner", "config"]);
    }
    for key in keys {
        let id = format!("cell.hdr.{key}");
        let v = doc.header_value(key).unwrap_or("");
        input(t, &id, key, v, "cell.page");
    }
    if cellish && impls.len() > 1 {
        let v = doc.header_value("default").unwrap_or("");
        let choices: Vec<&str> = impls.iter().map(String::as_str).collect();
        dropdown(t, "cell.hdr.default", "default", v, &choices, "cell.page");
    }
    if kind == FileKind::Shbench {
        bench_sections(t, doc);
    }
    if !cellish {
        return;
    }

    section(t, "cell.s.pins", "Pins (file order = port order)");
    let n_pins = doc.pins().name.len();
    for r in 0..n_pins {
        let rec = doc.record(Loc {
            table: TableId::Pins,
            row: r as u32,
        });
        let blk = block(t, &format!("cell.pin.{r}"));
        input(
            t,
            &format!("cell.pin.{r}.name"),
            "name",
            &rec.fields[0],
            &blk,
        );
        dropdown(
            t,
            &format!("cell.pin.{r}.dir"),
            "dir",
            &rec.fields[1],
            &PIN_DIRS,
            &blk,
        );
        let dom = attr_of(&rec, "domain").unwrap_or("analog");
        dropdown(
            t,
            &format!("cell.pin.{r}.domain"),
            "domain",
            dom,
            &PIN_DOMAINS,
            &blk,
        );
        let bar = format!("cell.pin.{r}.bar");
        node(t, &bar, "panel", &blk);
        attr(t, "style", "cell-bar");
        button(
            t,
            &format!("cell.pin.{r}.up"),
            "↑",
            "cell.pin_up",
            "Earlier port",
            &bar,
        );
        button(
            t,
            &format!("cell.pin.{r}.down"),
            "↓",
            "cell.pin_down",
            "Later port",
            &bar,
        );
    }
    if n_pins == 0 {
        note(
            t,
            "cell.pins.none",
            "No pins: draw them in the Symbol view.",
        );
    }

    section(t, "cell.s.params", "Parameters");
    for r in 0..doc.params().kind.len() {
        let rec = doc.record(Loc {
            table: TableId::Params,
            row: r as u32,
        });
        let blk = block(t, &format!("cell.param.{r}"));
        input(
            t,
            &format!("cell.param.{r}.name"),
            "name",
            &rec.fields[0],
            &blk,
        );
        let default = rec.fields.get(1).map_or("", String::as_str);
        input(
            t,
            &format!("cell.param.{r}.default"),
            "default",
            default,
            &blk,
        );
        let k = attr_of(&rec, "kind").unwrap_or("number");
        dropdown(
            t,
            &format!("cell.param.{r}.kind"),
            "kind",
            k,
            &PARAM_KINDS,
            &blk,
        );
        for a in PARAM_ATTRS {
            let v = match rec.find_attr(a) {
                Some(at) => at.values.join(" "),
                None => String::new(),
            };
            input(t, &format!("cell.param.{r}.{a}"), a, &v, &blk);
        }
        button(
            t,
            &format!("cell.param.{r}.del"),
            "Delete parameter",
            "cell.delete",
            "",
            &blk,
        );
    }
    button(
        t,
        "cell.add_param",
        "+ Parameter",
        "cell.add_param",
        "Add a parameter",
        "cell.page",
    );

    section(t, "cell.s.impls", "Implementations");
    for (r, name) in impls.iter().enumerate() {
        let rec = doc.record(Loc {
            table: TableId::Impls,
            row: r as u32,
        });
        let lang = rec.fields.get(1).map_or("", String::as_str);
        let blk = block(t, &format!("cell.impl.{r}"));
        node(t, &format!("cell.impl.{r}.title"), "label", &blk);
        attr(t, "label", &format!("{name} · {lang}"));
        attr(t, "style", "cell-title");
        if lang != "schematic" {
            input(
                t,
                &format!("cell.impl.{r}.model"),
                "model",
                attr_of(&rec, "model").unwrap_or(""),
                &blk,
            );
            input(
                t,
                &format!("cell.impl.{r}.file"),
                "file",
                attr_of(&rec, "file").unwrap_or(""),
                &blk,
            );
            for d in DIALECTS {
                let v = rec
                    .attrs
                    .iter()
                    .find(|a| a.key == "format" && a.values.first().is_some_and(|f| f == d))
                    .and_then(|a| a.values.get(1))
                    .map_or("", String::as_str);
                input(
                    t,
                    &format!("cell.impl.{r}.format.{d}"),
                    &format!("format {d}"),
                    v,
                    &blk,
                );
            }
            let id = format!("cell.impl.{r}.body");
            node(t, &id, "text_area", &blk);
            attr(t, "value", &body_text(doc, name));
            attr(t, "command", "cell.set");
            attr(
                t,
                "tooltip",
                "Definition text (Verilog-A, Verilog, SPICE); Ctrl+Enter applies",
            );
            attr(t, "style", "cell-body");
            if body_row(doc, name).is_some() {
                let bar = format!("cell.impl.{r}.ports");
                node(t, &bar, "panel", &blk);
                attr(t, "style", "cell-bar");
                button(
                    t,
                    &format!("cell.impl.{r}.sync"),
                    "Sync pins from ports",
                    "cell.sync_ports",
                    "Make the pins match the body's port list (shows the changes first)",
                    &bar,
                );
                button(
                    t,
                    &format!("cell.impl.{r}.symbol"),
                    "Symbol from ports",
                    "cell.symbol_from_ports",
                    "Regenerate the symbol (pins, box, labels) from the body's ports",
                    &bar,
                );
            }
            if let Some(p) = sync.filter(|p| p.impl_row == r as u32) {
                sync_rows(t, &sync_ops(doc, &p.ports).0, &p.ports, &blk);
            }
        }
        button(
            t,
            &format!("cell.impl.{r}.del"),
            "Delete implementation",
            "cell.delete",
            "",
            &blk,
        );
    }
    let bar = "cell.add_impl";
    node(t, bar, "panel", "cell.page");
    attr(t, "style", "cell-bar");
    for (lang, _) in ADD_LANGS {
        if add_impl_ops(doc, lang).is_ok() {
            let label = match lang {
                "schematic" => "+ Schematic",
                "spice" => "+ SPICE",
                "veriloga" => "+ Verilog-A",
                _ => "+ Verilog",
            };
            button(
                t,
                &format!("cell.add_impl.{lang}"),
                label,
                "cell.add_impl",
                "",
                bar,
            );
        }
    }
}

/// The pending sync's diff rows and its Apply / Cancel buttons, under `parent`.
pub(super) fn sync_rows(t: &mut String, plan: &SyncPlan, ports: &[(String, PinDir)], parent: &str) {
    let mut rows: Vec<(String, &str)> = Vec::new();
    if plan.is_empty() {
        rows.push(("Pins already match the ports.".into(), "docs-hint"));
    }
    for (name, dir) in &plan.add {
        rows.push((format!("+ pin {name} {}", dir.as_str()), "check-info"));
    }
    for name in &plan.remove {
        rows.push((format!("− pin {name}"), "check-error"));
    }
    for (name, from, to) in &plan.redir {
        let text = format!("pin {name}: {} → {}", from.as_str(), to.as_str());
        rows.push((text, "check-warning"));
    }
    if plan.reorder {
        let order: Vec<&str> = ports.iter().map(|p| p.0.as_str()).collect();
        rows.push((format!("order: {}", order.join(" ")), "check-warning"));
    }
    for (k, (text, style)) in rows.iter().enumerate() {
        node(t, &format!("cell.sync.{k}"), "label", parent);
        attr(t, "label", text);
        attr(t, "style", style);
    }
    let bar = "cell.sync.bar";
    node(t, bar, "panel", parent);
    attr(t, "style", "cell-bar");
    if !plan.is_empty() {
        button(
            t,
            "cell.sync.apply",
            "Apply",
            "cell.sync_apply",
            "Apply these pin changes (one undo)",
            bar,
        );
    }
    button(
        t,
        "cell.sync.cancel",
        "Cancel",
        "cell.sync_cancel",
        "Leave the pins as they are",
        bar,
    );
}

pub(super) fn section(t: &mut String, id: &str, title: &str) {
    node(t, id, "label", "cell.page");
    attr(t, "label", title);
    attr(t, "style", "cell-section");
}

pub(super) fn note(t: &mut String, id: &str, text: &str) {
    node(t, id, "label", "cell.page");
    attr(t, "label", text);
    attr(t, "style", "docs-hint");
}

pub(super) fn block(t: &mut String, id: &str) -> String {
    node(t, id, "panel", "cell.page");
    attr(t, "style", "cell-block");
    id.to_owned()
}

/// A `key  [value]` row: Enter submits `cell.set` with the typed text.
pub(super) fn input(t: &mut String, id: &str, key: &str, value: &str, parent: &str) {
    let row = format!("{id}.row");
    node(t, &row, "panel", parent);
    attr(t, "style", "cell-row");
    node(t, &format!("{id}.key"), "label", &row);
    attr(t, "label", key);
    attr(t, "style", "cell-key");
    node(t, id, "text_input", &row);
    attr(t, "value", value);
    attr(t, "command", "cell.set");
    attr(t, "style", "cell-input");
}

/// A `key  [choice v]` row: each choice item runs `cell.set` from id `<id>=<choice>`.
pub(super) fn dropdown(
    t: &mut String,
    id: &str,
    key: &str,
    value: &str,
    choices: &[&str],
    parent: &str,
) {
    let row = format!("{id}.row");
    node(t, &row, "panel", parent);
    attr(t, "style", "cell-row");
    node(t, &format!("{id}.key"), "label", &row);
    attr(t, "label", key);
    attr(t, "style", "cell-key");
    node(t, id, "dropdown", &row);
    attr(t, "label", if value.is_empty() { "(none)" } else { value });
    attr(t, "style", "cell-input");
    for c in choices {
        node(t, &format!("{id}={c}"), "item", id);
        attr(t, "label", c);
        attr(t, "command", "cell.set");
        attr(t, "checked", if *c == value { "1" } else { "0" });
    }
}

pub(super) fn button(
    t: &mut String,
    id: &str,
    label: &str,
    command: &str,
    tooltip: &str,
    parent: &str,
) {
    node(t, id, "button", parent);
    attr(t, "label", label);
    attr(t, "command", command);
    if !tooltip.is_empty() {
        attr(t, "tooltip", tooltip);
    }
}
