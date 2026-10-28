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
//!
//! Contents:
//! - `pub struct PortSync`
//! - `pub struct SyncPlan`, `SyncPlan::is_empty`
//! - `pub const PIN_DIRS`
//! - `pub const PIN_DOMAINS`
//! - `pub const PARAM_KINDS`
//! - `pub const PARAM_ATTRS`
//! - `pub const DIALECTS`
//! - `pub const ADD_LANGS`
//! - `pub enum Field`
//! - `pub enum Bench`, `Bench::word`, `Bench::ALL`
//! - `pub const ALGOS`
//! - `pub fn parse_field`
//! - `pub fn button_row`

mod bench;
mod bench_panel;
mod ops;
mod panel;

use schemify_doc::{Document, Loc, PinDir, RecordBuf, RecordKind, TableId};

pub use bench::{add_bench_ops, delete_bench_ops};
pub use ops::{
    add_impl_ops, add_param_ops, delete_impl_ops, delete_param_ops, impl_ports, move_pin_ops,
    set_ops, sync_ops,
};
pub use panel::contribute;

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

/// Optimizer algorithms (format rank 6).
pub const ALGOS: [&str; 2] = ["nelder-mead", "random"];

impl SyncPlan {
    /// Whether the pins already match the ports.
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty() && self.redir.is_empty() && !self.reorder
    }
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

// ---- private ----

// ------------------------------------------------------------------- projection

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
fn body_text(doc: &Document, name: &str) -> String {
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

// ------------------------------------------------------------------- bench tables

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
