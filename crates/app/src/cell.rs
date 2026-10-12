//! `cell`: the cell settings panel (`view.cell`): the active document's header attributes,
//! pins, params and impls (with their bodies) projected as UI nodes into the plugin-declared
//! `cell.page` scroll pane beside the canvas, and the `doc` ops each submitted field makes
//! (one transaction per field, so one undo). It is the structured editor for every rank-0,
//! rank-1 interface and rank-5 record the netlister reads, so none of them needs the raw
//! Edit-file dialog (`docs/research/editability-audit.md`).
//!
//! Node ids carry their target, so the panel holds no state: `cell.hdr.<key>`,
//! `cell.pin.<r>.<attr>`, `cell.param.<r>.<attr>`, `cell.impl.<r>.<attr>`,
//! `cell.impl.<r>.format.<dialect>`, `cell.impl.<r>.body`; a dropdown choice is the field id
//! plus `=<value>`; buttons are `cell.pin.<r>.up|down`, `cell.param.<r>.del`,
//! `cell.impl.<r>.del`, `cell.add_impl.<lang>`.
//!
//! Data design:
//! 1. In: one document's header, `pins`, `params`, `impls` and `bodies`. Out: state
//!    contribution records (UI nodes) and `doc` ops.
//! 2. How many: a cell's interface (tens of pins and params, a few impls); the panel shows
//!    all of them. Rows are `u32` (doc tables hold < 2^16).
//! 3. Access: every row once per frame while the panel is shown, through owned
//!    [`RecordBuf`] copies (cold path: the contribution is reparsed only when it changed).
//! 4. Lifetime: nothing persists here; whether the panel is shown is `Tab::cell`.

use schemify_doc::{Document, Loc, Op, RecordBuf, RecordKind, TableId, Target};
use schemify_format::FileKind;

use crate::docs::{attr, node};

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
}

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
/// kind without an interface (a `.shbench`) shows only its header.
pub fn contribute(t: &mut String, doc: &Document) {
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
