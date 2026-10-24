//! `cell`: the cell settings panel (`view.cell`): the active document's header attributes,
//! pins, params and impls (with their bodies) projected as UI nodes into the plugin-declared
//! `cell.page` scroll pane beside the canvas, and the `doc` ops each submitted field makes
//! (one transaction per field, so one undo). It is the structured editor for every rank-0,
//! rank-1 interface and rank-5 record the netlister reads, so none of them needs the raw
//! Edit-file dialog.
//!
//! Node ids carry their target, so the panel holds no state: `cell.hdr.<key>`,
//! `cell.pin.<r>.<attr>`, `cell.param.<r>.<attr>`, `cell.impl.<r>.<attr>`,
//! `cell.impl.<r>.format.<dialect>`, `cell.impl.<r>.body`; a dropdown choice is the field id
//! plus `=<value>`; buttons are `cell.pin.<r>.up|down`, `cell.param.<r>.del`,
//! `cell.impl.<r>.del`, `cell.add_impl.<lang>`, `cell.impl.<r>.sync` (sync pins from the
//! body's ports: shows a [`PortSync`] diff first, then `cell.sync.apply` /
//! `cell.sync.cancel`), `cell.impl.<r>.symbol` (regenerate the symbol from the body's ports).
//!
//! Data design:
//! 1. In: one document's header, `pins`, `params`, `impls` and `bodies`. Out: state
//!    contribution records (UI nodes) and `doc` ops.
//! 2. How many: a cell's interface (tens of pins and params, a few impls); the panel shows
//!    all of them. Rows are `u32` (doc tables hold < 2^16).
//! 3. Access: every row once per frame while the panel is shown, through owned
//!    [`RecordBuf`] copies (cold path: the contribution is reparsed only when it changed).
//! 4. Lifetime: nothing persists here; whether the panel is shown is `Tab::cell`, and a
//!    pending sync is the editor's ([`PortSync`], passed in).

use schemify_doc::{Document, Loc, Op, PinDir, RecordBuf, RecordKind, TableId, Target};
use schemify_format::FileKind;

use crate::docs::{attr, node};
use crate::scene::UNIT;

/// A "Sync pins from ports" waiting for Apply or Cancel: the ports parsed from impl
/// `impl_row`'s body when the user asked. Editor state (it never reaches the document);
/// valid only for the document and edit epoch it was made at, so any edit drops it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortSync {
    /// Impl row whose body was read.
    pub impl_row: u32,
    /// Its ports in port order (unique names, ≤ `schemify_elab::PORTS_MAX`).
    pub ports: Vec<(String, PinDir)>,
}

/// What syncing the pins to a port list changes (shown before it is applied).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncPlan {
    /// Ports with no pin, in port order: added.
    pub add: Vec<(String, PinDir)>,
    /// Pins with no port, in file order: deleted.
    pub remove: Vec<String>,
    /// Pins whose dir differs from their port's: (name, now, then).
    pub redir: Vec<(String, PinDir, PinDir)>,
    /// The pins end up reordered to port order.
    pub reorder: bool,
}

impl SyncPlan {
    /// Whether the pins already match the ports.
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty() && self.redir.is_empty() && !self.reorder
    }
}

/// Pin directions (format rank 1).
pub const PIN_DIRS: [&str; 3] = ["in", "out", "inout"];
/// Pin signal domains; `analog` is the default and is written as no attribute.
pub const PIN_DOMAINS: [&str; 3] = ["analog", "digital", "real"];
/// Param kinds; `number` is the default and is written as no attribute.
pub const PARAM_KINDS: [&str; 5] = ["number", "int", "bool", "string", "choice"];
/// Param attributes the panel edits, in display order (`name` and `default` are fields).
pub const PARAM_ATTRS: [&str; 8] = [
    "unit", "min", "max", "step", "grid", "choices", "computed", "doc",
];
/// Netlist dialects an impl can carry a `format` template for.
pub const DIALECTS: [&str; 5] = ["spice", "xyce", "cdl", "spectre", "verilog"];
/// Impl languages the panel can add, with the default impl name of each.
pub const ADD_LANGS: [(&str, &str); 4] = [
    ("schematic", "sch"),
    ("spice", "spice"),
    ("veriloga", "va"),
    ("verilog", "verilog"),
];

/// A field of the panel, decoded from its node id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field<'a> {
    /// Header attribute `key`.
    Header(&'a str),
    /// Pin row, attribute (`name`, `dir`, `domain`).
    Pin(u32, &'a str),
    /// Param row, attribute (`name`, `default`, `kind` or one of [`PARAM_ATTRS`]).
    Param(u32, &'a str),
    /// Impl row, attribute (`model`, `file`).
    Impl(u32, &'a str),
    /// Impl row, `format` template of a dialect.
    Format(u32, &'a str),
    /// Impl row's `body` text.
    Body(u32),
    /// A bench record (`.shbench` rank 6): table, row, attribute.
    Bench(Bench, u32, &'a str),
}

/// The bench tables the panel edits (format rank 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bench {
    /// `output <name> <expr>`: `expr`, `unit`.
    Output,
    /// `spec <output>`: `min`, `max`.
    Spec,
    /// `optimize <name>`: `algo`, `goals`, `vary`, `corners`, `configs`, `budget`, `seed`.
    Optimize,
}

impl Bench {
    /// The node id word of the table.
    pub fn word(self) -> &'static str {
        match self {
            Bench::Output => "output",
            Bench::Spec => "spec",
            Bench::Optimize => "opt",
        }
    }

    fn table(self) -> TableId {
        match self {
            Bench::Output => TableId::Outputs,
            Bench::Spec => TableId::Specs,
            Bench::Optimize => TableId::Optimizes,
        }
    }

    fn kind(self) -> RecordKind {
        match self {
            Bench::Output => RecordKind::Output,
            Bench::Spec => RecordKind::Spec,
            Bench::Optimize => RecordKind::Optimize,
        }
    }

    /// Every bench table, in panel order.
    pub const ALL: [Bench; 3] = [Bench::Output, Bench::Spec, Bench::Optimize];
}

/// Optimizer algorithms (format rank 6).
pub const ALGOS: [&str; 2] = ["nelder-mead", "random"];

/// Decodes a field node id; a dropdown choice id (`<field>=<value>`) also returns the value.
/// `None` for an id that is not a panel field.
pub fn parse_field(node: &str) -> Option<(Field<'_>, Option<&str>)> {
    let (id, choice) = match node.split_once('=') {
        Some((id, v)) => (id, Some(v)),
        None => (node, None),
    };
    let rest = id.strip_prefix("cell.")?;
    let mut parts = rest.splitn(3, '.');
    let section = parts.next()?;
    if section == "hdr" {
        let key = parts.next()?;
        return (parts.next().is_none() && !key.is_empty()).then_some((Field::Header(key), choice));
    }
    let row: u32 = parts.next()?.parse().ok()?;
    let what = parts.next()?;
    let field = match (section, what) {
        ("pin", a) => Field::Pin(row, a),
        ("param", a) => Field::Param(row, a),
        ("impl", "body") => Field::Body(row),
        ("impl", a) => match a.strip_prefix("format.") {
            Some(d) => Field::Format(row, d),
            None => Field::Impl(row, a),
        },
        ("output", a) => Field::Bench(Bench::Output, row, a),
        ("spec", a) => Field::Bench(Bench::Spec, row, a),
        ("opt", a) => Field::Bench(Bench::Optimize, row, a),
        _ => return None,
    };
    Some((field, choice))
}

/// Row number of a button id `cell.<section>.<r>.<action>`, if it is one.
pub fn button_row(node: &str, section: &str, action: &str) -> Option<u32> {
    let rest = node
        .strip_prefix("cell.")?
        .strip_prefix(section)?
        .strip_prefix('.')?;
    let (row, act) = rest.split_once('.')?;
    (act == action).then(|| row.parse().ok()).flatten()
}

// ------------------------------------------------------------------- projection

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
fn sync_rows(t: &mut String, plan: &SyncPlan, ports: &[(String, PinDir)], parent: &str) {
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

fn section(t: &mut String, id: &str, title: &str) {
    node(t, id, "label", "cell.page");
    attr(t, "label", title);
    attr(t, "style", "cell-section");
}

fn note(t: &mut String, id: &str, text: &str) {
    node(t, id, "label", "cell.page");
    attr(t, "label", text);
    attr(t, "style", "docs-hint");
}

fn block(t: &mut String, id: &str) -> String {
    node(t, id, "panel", "cell.page");
    attr(t, "style", "cell-block");
    id.to_owned()
}

/// A `key  [value]` row: Enter submits `cell.set` with the typed text.
fn input(t: &mut String, id: &str, key: &str, value: &str, parent: &str) {
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
fn dropdown(t: &mut String, id: &str, key: &str, value: &str, choices: &[&str], parent: &str) {
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

fn button(t: &mut String, id: &str, label: &str, command: &str, tooltip: &str, parent: &str) {
    node(t, id, "button", parent);
    attr(t, "label", label);
    attr(t, "command", command);
    if !tooltip.is_empty() {
        attr(t, "tooltip", tooltip);
    }
}

// ------------------------------------------------------------------------ reads

fn names(doc: &Document, table: TableId, n: usize) -> Vec<String> {
    (0..n as u32)
        .map(|row| doc.record(Loc { table, row }).fields[0].clone())
        .collect()
}

fn attr_of<'r>(rec: &'r RecordBuf, key: &str) -> Option<&'r str> {
    rec.find_attr(key)
        .and_then(|a| a.values.first())
        .map(String::as_str)
}

/// Row of the `body` record of impl `name`, if it has one.
fn body_row(doc: &Document, name: &str) -> Option<u32> {
    let n = doc.bodies().kind.len() as u32;
    (0..n).find(|&row| {
        doc.record(Loc {
            table: TableId::Bodies,
            row,
        })
        .fields[0]
            == name
    })
}

/// The `body` text of impl `name` (empty when it has none).
pub fn body_text(doc: &Document, name: &str) -> String {
    body_row(doc, name).map_or_else(String::new, |row| {
        doc.record(Loc {
            table: TableId::Bodies,
            row,
        })
        .verbatim
        .join("\n")
    })
}

fn record(doc: &Document, table: TableId, row: u32) -> Option<RecordBuf> {
    let n = match table {
        TableId::Pins => doc.pins().name.len(),
        TableId::Params => doc.params().kind.len(),
        TableId::Impls => doc.impls().kind.len(),
        TableId::Outputs => doc.outputs().kind.len(),
        TableId::Specs => doc.specs().kind.len(),
        TableId::Optimizes => doc.optimizes().kind.len(),
        _ => 0,
    };
    ((row as usize) < n).then(|| doc.record(Loc { table, row }))
}

// ------------------------------------------------------------------------ edits

/// Set `key` to `value` on `target` (`values` split for multi-value attributes); an empty
/// value removes the line when present. No op when nothing changes.
fn attr_ops(
    target: Target,
    current: Option<&str>,
    key: &str,
    value: &str,
    values: Vec<String>,
) -> Vec<Op> {
    match (current, value.is_empty()) {
        (Some(c), _) if c == value => Vec::new(),
        (None, true) => Vec::new(),
        (Some(_), true) => vec![Op::RemoveAttr {
            target,
            key: key.to_owned(),
            first: None,
        }],
        (_, false) => vec![Op::SetAttr {
            target,
            key: key.to_owned(),
            values,
        }],
    }
}

fn one_of(value: &str, allowed: &[&str], what: &str) -> Result<(), String> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!("{what} must be one of {}", allowed.join(", ")))
    }
}

fn word(value: &str, what: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        Err(format!("{what} must be one word"))
    } else {
        Ok(())
    }
}

/// Ops setting `field` of `doc` to `value` (trimmed, except a body). `Ok(empty)` when
/// nothing changes; `Err` names what is wrong with the value (the document is untouched).
pub fn set_ops(doc: &Document, field: Field<'_>, value: &str) -> Result<Vec<Op>, String> {
    let v = if matches!(field, Field::Body(_)) {
        value.trim_end()
    } else {
        value.trim()
    };
    let ops = match field {
        Field::Header(key) => {
            let allowed: &[&str] = match doc.kind() {
                FileKind::Shprim | FileKind::Shcomp => &["doc", "category", "prefix", "default"],
                FileKind::Shbench => &["doc", "category", "corner", "config"],
                _ => &[],
            };
            one_of(key, allowed, "header attribute")?;
            if key == "default" && !v.is_empty() {
                let impls = names(doc, TableId::Impls, doc.impls().kind.len());
                if !impls.iter().any(|n| n == v) {
                    return Err(format!("no impl named {v}"));
                }
            }
            if key == "prefix" && v.chars().count() > 1 {
                return Err("prefix is one letter".into());
            }
            attr_ops(
                Target::header(),
                doc.header_value(key),
                key,
                v,
                vec![v.to_owned()],
            )
        }
        Field::Pin(row, a) => {
            let rec = record(doc, TableId::Pins, row).ok_or("no such pin")?;
            let target = Target::new(RecordKind::Pin, &rec.fields[0]);
            match a {
                "name" => {
                    word(v, "a pin name")?;
                    set_field(target, &rec, 0, v)
                }
                "dir" => {
                    one_of(v, &PIN_DIRS, "dir")?;
                    set_field(target, &rec, 1, v)
                }
                "domain" => {
                    one_of(v, &PIN_DOMAINS, "domain")?;
                    // `analog` is the default: written as no attribute.
                    let v = if v == "analog" { "" } else { v };
                    attr_ops(
                        target,
                        attr_of(&rec, "domain"),
                        "domain",
                        v,
                        vec![v.to_owned()],
                    )
                }
                _ => return Err(format!("pin has no field {a}")),
            }
        }
        Field::Param(row, a) => {
            let rec = record(doc, TableId::Params, row).ok_or("no such parameter")?;
            let target = Target::new(RecordKind::Param, &rec.fields[0]);
            match a {
                "name" => {
                    word(v, "a parameter name")?;
                    set_field(target, &rec, 0, v)
                }
                "default" => {
                    if v.is_empty() {
                        return Err("a parameter needs a default".into());
                    }
                    set_field(target, &rec, 1, v)
                }
                "kind" => {
                    one_of(v, &PARAM_KINDS, "kind")?;
                    let v = if v == "number" { "" } else { v };
                    attr_ops(target, attr_of(&rec, "kind"), "kind", v, vec![v.to_owned()])
                }
                "choices" => {
                    let cur = rec.find_attr("choices").map(|at| at.values.join(" "));
                    let vals = v.split_whitespace().map(str::to_owned).collect();
                    attr_ops(target, cur.as_deref(), "choices", v, vals)
                }
                a if PARAM_ATTRS.contains(&a) => {
                    attr_ops(target, attr_of(&rec, a), a, v, vec![v.to_owned()])
                }
                _ => return Err(format!("parameter has no field {a}")),
            }
        }
        Field::Impl(row, a) => {
            let rec = record(doc, TableId::Impls, row).ok_or("no such impl")?;
            one_of(a, &["model", "file"], "impl field")?;
            let target = Target::new(RecordKind::Impl, &rec.fields[0]);
            attr_ops(target, attr_of(&rec, a), a, v, vec![v.to_owned()])
        }
        Field::Format(row, d) => {
            let rec = record(doc, TableId::Impls, row).ok_or("no such impl")?;
            one_of(d, &DIALECTS, "dialect")?;
            let target = Target::new(RecordKind::Impl, &rec.fields[0]);
            let cur = rec
                .attrs
                .iter()
                .find(|at| at.key == "format" && at.values.first().is_some_and(|f| f == d))
                .and_then(|at| at.values.get(1))
                .map(String::as_str);
            match (cur, v.is_empty()) {
                (Some(c), _) if c == v => Vec::new(),
                (None, true) => Vec::new(),
                (Some(_), true) => vec![Op::RemoveAttr {
                    target,
                    key: "format".into(),
                    first: Some(d.to_owned()),
                }],
                (_, false) => vec![Op::SetAttr {
                    target,
                    key: "format".into(),
                    values: vec![d.to_owned(), v.to_owned()],
                }],
            }
        }
        Field::Body(row) => {
            let rec = record(doc, TableId::Impls, row).ok_or("no such impl")?;
            let name = &rec.fields[0];
            if rec.fields.get(1).is_some_and(|l| l == "schematic") {
                return Err("a schematic impl has no body".into());
            }
            body_ops(doc, name, v)
        }
        Field::Bench(b, row, a) => {
            let rec = record(doc, b.table(), row).ok_or_else(|| format!("no such {}", b.word()))?;
            return bench_ops(b, &rec, a, v);
        }
    };
    debug_assert!(ops.len() <= 2);
    Ok(ops)
}

fn set_field(target: Target, rec: &RecordBuf, index: u32, v: &str) -> Vec<Op> {
    if rec.fields.get(index as usize).is_some_and(|f| f == v) {
        return Vec::new();
    }
    vec![Op::SetField {
        target,
        index,
        value: v.to_owned(),
    }]
}

/// Ops replacing impl `name`'s body with `text` (blank deletes the body record).
fn body_ops(doc: &Document, name: &str, text: &str) -> Vec<Op> {
    let row = body_row(doc, name);
    if row.is_some_and(|_| body_text(doc, name) == text)
        || (row.is_none() && text.trim().is_empty())
    {
        return Vec::new();
    }
    let target = Target::new(RecordKind::Body, name);
    if text.trim().is_empty() {
        return vec![Op::Delete(target)];
    }
    let rec = text.split('\n').fold(
        RecordBuf::new(RecordKind::Body).field(name),
        RecordBuf::verbatim,
    );
    match row {
        Some(_) => vec![Op::Put { target, rec }],
        None => vec![Op::Insert { rec, at: None }],
    }
}

/// The ports impl `row`'s body declares (`schemify_elab::impl_ports`). `Err` in words when
/// the row is no impl, the impl is a schematic or has no body, or the body does not parse
/// (with its line).
pub fn impl_ports(doc: &Document, row: u32) -> Result<Vec<(String, PinDir)>, String> {
    let rec = record(doc, TableId::Impls, row).ok_or("no such impl")?;
    let (name, lang) = (&rec.fields[0], rec.fields.get(1).map_or("", String::as_str));
    if lang == "schematic" {
        return Err("a schematic impl's ports are its schematic's".into());
    }
    if body_row(doc, name).is_none() {
        return Err(format!("impl {name} has no body to read ports from"));
    }
    let ports = schemify_elab::impl_ports(lang, &body_text(doc, name))
        .map_err(|e| format!("impl {name} body {e}"))?;
    debug_assert!(ports.len() <= schemify_elab::PORTS_MAX);
    Ok(ports)
}

/// What syncing `doc`'s pins to `ports` (unique names, port order) changes, and the ops
/// that do it in one transaction: delete pins with no port, set dirs, add a pin per new
/// port, reorder to port order. Existing pins keep their coordinates and attributes; new
/// pins stack below the lowest pin on the 10-unit grid, inputs in the leftmost pin column,
/// the rest in the rightmost. No ops when the pins already match.
pub fn sync_ops(doc: &Document, ports: &[(String, PinDir)]) -> (SyncPlan, Vec<Op>) {
    assert!(ports.len() <= schemify_elab::PORTS_MAX);
    let (s, pins) = (doc.strings(), doc.pins());
    let have: Vec<&str> = pins.name.iter().map(|&n| s.get(n)).collect();
    let mut plan = SyncPlan::default();
    let mut ops = Vec::new();
    for name in &have {
        if !ports.iter().any(|p| p.0 == *name) {
            plan.remove.push((*name).to_owned());
            ops.push(Op::Delete(Target::new(RecordKind::Pin, name)));
        }
    }
    for (name, dir) in ports {
        match have.iter().position(|h| h == name) {
            Some(i) if pins.dir[i] != *dir => {
                plan.redir.push((name.clone(), pins.dir[i], *dir));
                ops.push(Op::SetField {
                    target: Target::new(RecordKind::Pin, name),
                    index: 1,
                    value: dir.as_str().to_owned(),
                });
            }
            Some(_) => {}
            None => plan.add.push((name.clone(), *dir)),
        }
    }
    // ponytail: new pins go below every pin, not outside the drawn body's bbox; a symbol
    // whose graphics reach below its pins may overlap them. "Symbol from ports" redraws
    // cleanly; read the graphics' extent here if that bites.
    let grid = 10 * UNIT;
    let (left, right, bottom) = match (pins.x.iter().min(), pins.x.iter().max()) {
        (Some(&l), Some(&r)) => {
            let r = if r == l { l + 4 * grid } else { r };
            (l, r, pins.y.iter().copied().max().unwrap_or(0))
        }
        _ => (-2 * grid, 2 * grid, 0),
    };
    let c = |v: i32| {
        let mut t = String::new();
        schemify_format::format_coord(v, &mut t);
        t
    };
    for (k, (name, dir)) in plan.add.iter().enumerate() {
        let x = if *dir == PinDir::In { left } else { right };
        let y = bottom + (k as i32 + 1) * grid;
        let rec = RecordBuf::new(RecordKind::Pin)
            .field(name)
            .field(dir.as_str())
            .field(&c(x))
            .field(&c(y));
        ops.push(Op::Insert { rec, at: None });
    }
    // Pin order after the deletes and appends, then moved into port order one by one.
    let mut order: Vec<&str> = have
        .iter()
        .copied()
        .filter(|h| ports.iter().any(|p| p.0 == *h))
        .chain(plan.add.iter().map(|a| a.0.as_str()))
        .collect();
    assert_eq!(order.len(), ports.len());
    for (k, (name, _)) in ports.iter().enumerate() {
        if order[k] != name {
            let from = order[k..]
                .iter()
                .position(|o| o == name)
                .expect("every port has a pin")
                + k;
            let moved = order.remove(from);
            order.insert(k, moved);
            plan.reorder = true;
            ops.push(Op::Reorder {
                target: Target::new(RecordKind::Pin, name),
                to: k as u32,
            });
        }
    }
    debug_assert!(order.iter().zip(ports).all(|(o, p)| *o == p.0));
    debug_assert_eq!(plan.is_empty(), ops.is_empty());
    (plan, ops)
}

/// Ops adding a parameter `p<k>` (the first free `k`) with default `0`.
pub fn add_param_ops(doc: &Document) -> Vec<Op> {
    let names = names(doc, TableId::Params, doc.params().kind.len());
    let name = (1..=names.len() + 1)
        .map(|k| format!("p{k}"))
        .find(|n| !names.contains(n))
        .expect("one of n + 1 names is free");
    let rec = RecordBuf::new(RecordKind::Param).field(&name).field("0");
    vec![Op::Insert { rec, at: None }]
}

/// Ops adding an impl of `lang` (named per [`ADD_LANGS`], uniquified). A second impl also
/// sets the header `default` to the first (E025: more than one impl needs a default). `Err`
/// when the file kind does not allow it: a `.shprim` has no schematic impl, a `.shcomp`
/// exactly one, a `.shbench` none.
pub fn add_impl_ops(doc: &Document, lang: &str) -> Result<Vec<Op>, String> {
    let (_, base) = ADD_LANGS
        .iter()
        .find(|(l, _)| *l == lang)
        .ok_or_else(|| format!("unknown impl language {lang}"))?;
    let n = doc.impls().kind.len();
    let langs: Vec<String> = (0..n as u32)
        .map(|row| {
            let r = doc.record(Loc {
                table: TableId::Impls,
                row,
            });
            r.fields.get(1).cloned().unwrap_or_default()
        })
        .collect();
    let has_sch = langs.iter().any(|l| l == "schematic");
    match (doc.kind(), lang) {
        (FileKind::Shprim, "schematic") => return Err("a primitive has no schematic".into()),
        (FileKind::Shcomp, "schematic") if has_sch => {
            return Err("a component has exactly one schematic".into());
        }
        (FileKind::Shprim | FileKind::Shcomp, _) => {}
        _ => return Err("this file kind has no impls".into()),
    }
    let names = names(doc, TableId::Impls, n);
    let name = std::iter::once(base.to_string())
        .chain((2..=n + 2).map(|k| format!("{base}{k}")))
        .find(|c| !names.contains(c))
        .expect("one of n + 2 names is free");
    let mut ops = vec![Op::Insert {
        rec: RecordBuf::new(RecordKind::Impl).field(&name).field(lang),
        at: None,
    }];
    if n == 1 && doc.header_value("default").is_none() {
        ops.push(Op::SetAttr {
            target: Target::header(),
            key: "default".into(),
            values: vec![names[0].clone()],
        });
    }
    Ok(ops)
}

/// Ops deleting param `row`.
pub fn delete_param_ops(doc: &Document, row: u32) -> Vec<Op> {
    record(doc, TableId::Params, row).map_or_else(Vec::new, |rec| {
        vec![Op::Delete(Target::new(RecordKind::Param, &rec.fields[0]))]
    })
}

/// Ops deleting impl `row` with its body; a header `default` naming it moves to the first
/// remaining impl, or goes when at most one impl remains.
pub fn delete_impl_ops(doc: &Document, row: u32) -> Vec<Op> {
    let Some(rec) = record(doc, TableId::Impls, row) else {
        return Vec::new();
    };
    let name = rec.fields[0].clone();
    let mut ops = vec![Op::Delete(Target::new(RecordKind::Impl, &name))];
    if body_row(doc, &name).is_some() {
        ops.push(Op::Delete(Target::new(RecordKind::Body, &name)));
    }
    let rest: Vec<String> = names(doc, TableId::Impls, doc.impls().kind.len())
        .into_iter()
        .filter(|n| *n != name)
        .collect();
    let default = doc.header_value("default");
    if default.is_some() && (rest.len() <= 1 || default == Some(name.as_str())) {
        if rest.len() > 1 {
            ops.push(Op::SetAttr {
                target: Target::header(),
                key: "default".into(),
                values: vec![rest[0].clone()],
            });
        } else {
            ops.push(Op::RemoveAttr {
                target: Target::header(),
                key: "default".into(),
                first: None,
            });
        }
    }
    ops
}

/// Ops moving pin `row` one place earlier (`up`) or later in port order; none at an end.
pub fn move_pin_ops(doc: &Document, row: u32, up: bool) -> Vec<Op> {
    let n = doc.pins().name.len() as u32;
    let to = match (up, row) {
        (true, 0) => return Vec::new(),
        (true, r) => r - 1,
        (false, r) if r + 1 >= n => return Vec::new(),
        (false, r) => r + 1,
    };
    let Some(rec) = record(doc, TableId::Pins, row) else {
        return Vec::new();
    };
    vec![Op::Reorder {
        target: Target::new(RecordKind::Pin, &rec.fields[0]),
        to,
    }]
}

// ------------------------------------------------------------------- bench tables

/// The Outputs, Specs and Optimizers sections of a `.shbench`.
fn bench_sections(t: &mut String, doc: &Document) {
    let rows = |b: Bench| record_count(doc, b.table());
    section(t, "cell.s.output", "Outputs (one number per job)");
    for r in 0..rows(Bench::Output) {
        let rec = doc.record(Loc {
            table: TableId::Outputs,
            row: r as u32,
        });
        let blk = block(t, &format!("cell.output.{r}"));
        input(
            t,
            &format!("cell.output.{r}.name"),
            "name",
            &rec.fields[0],
            &blk,
        );
        let expr = rec.fields.get(1).map_or("", String::as_str);
        input(t, &format!("cell.output.{r}.expr"), "expr", expr, &blk);
        input(
            t,
            &format!("cell.output.{r}.unit"),
            "unit",
            attr_of(&rec, "unit").unwrap_or(""),
            &blk,
        );
        button(
            t,
            &format!("cell.output.{r}.del"),
            "Delete output",
            "cell.delete",
            "",
            &blk,
        );
    }
    button(
        t,
        "cell.add.output",
        "+ Output",
        "cell.add_bench",
        "Add an output expression",
        "cell.page",
    );

    section(t, "cell.s.spec", "Specs");
    for r in 0..rows(Bench::Spec) {
        let rec = doc.record(Loc {
            table: TableId::Specs,
            row: r as u32,
        });
        let blk = block(t, &format!("cell.spec.{r}"));
        node(t, &format!("cell.spec.{r}.title"), "label", &blk);
        attr(t, "label", &rec.fields[0]);
        attr(t, "style", "cell-title");
        for a in ["min", "max"] {
            input(
                t,
                &format!("cell.spec.{r}.{a}"),
                a,
                attr_of(&rec, a).unwrap_or(""),
                &blk,
            );
        }
        button(
            t,
            &format!("cell.spec.{r}.del"),
            "Delete spec",
            "cell.delete",
            "",
            &blk,
        );
    }
    button(
        t,
        "cell.add.spec",
        "+ Spec",
        "cell.add_bench",
        "Add a spec for an output without one",
        "cell.page",
    );

    section(t, "cell.s.opt", "Optimizers");
    for r in 0..rows(Bench::Optimize) {
        let rec = doc.record(Loc {
            table: TableId::Optimizes,
            row: r as u32,
        });
        let blk = block(t, &format!("cell.opt.{r}"));
        node(t, &format!("cell.opt.{r}.title"), "label", &blk);
        attr(t, "label", &format!("optimize {}", rec.fields[0]));
        attr(t, "style", "cell-title");
        let algo = attr_of(&rec, "algo").unwrap_or("nelder-mead");
        dropdown(t, &format!("cell.opt.{r}.algo"), "algo", algo, &ALGOS, &blk);
        let goals = joined(&rec, "goal", "; ");
        input(t, &format!("cell.opt.{r}.goals"), "goals", &goals, &blk);
        input(
            t,
            &format!("cell.opt.{r}.vary"),
            "vary",
            &joined(&rec, "vary", " "),
            &blk,
        );
        input(
            t,
            &format!("cell.opt.{r}.corners"),
            "corners",
            &joined(&rec, "corner", " "),
            &blk,
        );
        input(
            t,
            &format!("cell.opt.{r}.configs"),
            "configs",
            &joined(&rec, "config", " "),
            &blk,
        );
        for a in ["budget", "seed"] {
            input(
                t,
                &format!("cell.opt.{r}.{a}"),
                a,
                attr_of(&rec, a).unwrap_or(""),
                &blk,
            );
        }
        note(
            t,
            &format!("cell.opt.{r}.hint"),
            "goals: `<output> min|max|target <expr> [weight <w>]`, separated by `;`. vary: docs variables (their bounds apply).",
        );
        button(
            t,
            &format!("cell.opt.{r}.del"),
            "Delete optimizer",
            "cell.delete",
            "",
            &blk,
        );
    }
    button(
        t,
        "cell.add.opt",
        "+ Optimizer",
        "cell.add_bench",
        "Add an optimizer setup",
        "cell.page",
    );
}

fn record_count(doc: &Document, table: TableId) -> usize {
    match table {
        TableId::Outputs => doc.outputs().kind.len(),
        TableId::Specs => doc.specs().kind.len(),
        TableId::Optimizes => doc.optimizes().kind.len(),
        _ => 0,
    }
}

/// The values of every `key` line of `rec`, one line per item, joined by `sep`.
fn joined(rec: &RecordBuf, key: &str, sep: &str) -> String {
    rec.attrs
        .iter()
        .filter(|a| a.key == key)
        .map(|a| a.values.join(" "))
        .collect::<Vec<_>>()
        .join(sep)
}

/// Ops replacing every `key` line of `target` by one line per item of `items`.
fn lines_ops(target: &Target, rec: &RecordBuf, key: &str, items: Vec<Vec<String>>) -> Vec<Op> {
    let current: Vec<Vec<String>> = rec
        .attrs
        .iter()
        .filter(|a| a.key == key)
        .map(|a| a.values.clone())
        .collect();
    if current == items {
        return Vec::new();
    }
    let mut ops = Vec::with_capacity(items.len() + 1);
    if !current.is_empty() {
        ops.push(Op::RemoveAttr {
            target: target.clone(),
            key: key.to_owned(),
            first: None,
        });
    }
    // ponytail: SetAttr replaces a repeatable line by its first value, so items sharing a
    // first value (two goals on one output) keep only the last; split the record if needed.
    for values in items {
        ops.push(Op::SetAttr {
            target: target.clone(),
            key: key.to_owned(),
            values,
        });
    }
    ops
}

/// Parses an optimizer goal list `<output> min|max|target <expr> [weight <w>]; …`.
fn parse_goals(v: &str) -> Result<Vec<Vec<String>>, String> {
    let mut out = Vec::new();
    for g in v.split(';').map(str::trim).filter(|g| !g.is_empty()) {
        let w: Vec<String> = g.split_whitespace().map(str::to_owned).collect();
        let ok = match w.get(1).map(String::as_str) {
            Some("min" | "max") => w.len() == 2 || (w.len() == 4 && w[2] == "weight"),
            Some("target") => w.len() == 3 || (w.len() == 5 && w[3] == "weight"),
            _ => false,
        };
        if !ok {
            return Err(format!(
                "goal `{g}`: expected `<output> min|max|target <expr> [weight <w>]`"
            ));
        }
        out.push(w);
    }
    Ok(out)
}

fn bench_ops(b: Bench, rec: &RecordBuf, a: &str, v: &str) -> Result<Vec<Op>, String> {
    let target = Target::new(b.kind(), &rec.fields[0]);
    let words = |v: &str| {
        v.split_whitespace()
            .map(|w| vec![w.to_owned()])
            .collect::<Vec<_>>()
    };
    let ops = match (b, a) {
        (Bench::Output, "name") => {
            word(v, "an output name")?;
            set_field(target, rec, 0, v)
        }
        (Bench::Output, "expr") => {
            if v.is_empty() {
                return Err("an output needs an expression".into());
            }
            set_field(target, rec, 1, v)
        }
        (Bench::Output, "unit") | (Bench::Spec, "min" | "max") => {
            attr_ops(target, attr_of(rec, a), a, v, vec![v.to_owned()])
        }
        (Bench::Optimize, "algo") => {
            one_of(v, &ALGOS, "algo")?;
            let v = if v == "nelder-mead" { "" } else { v };
            attr_ops(target, attr_of(rec, "algo"), "algo", v, vec![v.to_owned()])
        }
        (Bench::Optimize, "budget" | "seed") => {
            if !v.is_empty() && v.parse::<u64>().is_err() {
                return Err(format!("{a} must be a whole number"));
            }
            attr_ops(target, attr_of(rec, a), a, v, vec![v.to_owned()])
        }
        (Bench::Optimize, "goals") => lines_ops(&target, rec, "goal", parse_goals(v)?),
        (Bench::Optimize, "vary") => lines_ops(&target, rec, "vary", words(v)),
        (Bench::Optimize, "corners") => lines_ops(&target, rec, "corner", words(v)),
        (Bench::Optimize, "configs") => lines_ops(&target, rec, "config", words(v)),
        _ => return Err(format!("{} has no field {a}", b.word())),
    };
    Ok(ops)
}

/// Ops adding a bench record: an output `out<k>` = `0`, a spec for the first output without
/// one, or an optimizer `opt<k>` (defaults: nelder-mead, budget 100).
pub fn add_bench_ops(doc: &Document, b: Bench) -> Result<Vec<Op>, String> {
    if doc.kind() != FileKind::Shbench {
        return Err("only a testbench has outputs, specs and optimizers".into());
    }
    let n = record_count(doc, b.table());
    let have = names(doc, b.table(), n);
    let free = |base: &str| {
        (1..=n + 1)
            .map(|k| format!("{base}{k}"))
            .find(|c| !have.contains(c))
            .expect("one of n + 1 names is free")
    };
    let rec = match b {
        Bench::Output => RecordBuf::new(RecordKind::Output)
            .field(&free("out"))
            .field("0"),
        Bench::Spec => {
            let outs = names(doc, TableId::Outputs, record_count(doc, TableId::Outputs));
            let out = outs
                .into_iter()
                .find(|o| !have.contains(o))
                .ok_or("every output already has a spec (add an output first)")?;
            RecordBuf::new(RecordKind::Spec).field(&out)
        }
        Bench::Optimize => RecordBuf::new(RecordKind::Optimize).field(&free("opt")),
    };
    Ok(vec![Op::Insert { rec, at: None }])
}

/// Ops deleting bench record `row` of table `b`.
pub fn delete_bench_ops(doc: &Document, b: Bench, row: u32) -> Vec<Op> {
    record(doc, b.table(), row).map_or_else(Vec::new, |rec| {
        vec![Op::Delete(Target::new(b.kind(), &rec.fields[0]))]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_ids_round_trip() {
        assert_eq!(
            parse_field("cell.hdr.prefix"),
            Some((Field::Header("prefix"), None))
        );
        assert_eq!(
            parse_field("cell.pin.3.dir=out"),
            Some((Field::Pin(3, "dir"), Some("out")))
        );
        assert_eq!(
            parse_field("cell.param.0.min"),
            Some((Field::Param(0, "min"), None))
        );
        assert_eq!(
            parse_field("cell.impl.1.model"),
            Some((Field::Impl(1, "model"), None))
        );
        assert_eq!(
            parse_field("cell.impl.1.format.spectre"),
            Some((Field::Format(1, "spectre"), None))
        );
        assert_eq!(
            parse_field("cell.impl.2.body"),
            Some((Field::Body(2), None))
        );
        // Negative space: other ids are not fields.
        for bad in [
            "docs.b.1",
            "cell.pin.x.dir",
            "cell.hdr.",
            "cell.hdr.a.b",
            "cell.foo.1.x",
            "cell.pin.1",
        ] {
            assert_eq!(parse_field(bad), None, "{bad}");
        }
        assert_eq!(button_row("cell.pin.4.up", "pin", "up"), Some(4));
        assert_eq!(button_row("cell.pin.4.up", "pin", "down"), None);
        assert_eq!(button_row("cell.param.7.del", "param", "del"), Some(7));
    }
}
