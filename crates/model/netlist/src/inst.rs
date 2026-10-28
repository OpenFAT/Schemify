//! Instance lines (netlist spec §7.1, §7.6.1, §7.6.5–7, expr spec §6): one instance row
//! and its [`Plan`] in, its element text out, plus the impls and models it uses.
//!
//! A `fast` template copies the canonical texts elab already wrote; any other goes
//! through `schemify_expr::Template`, which stays the general path and the oracle of the
//! fast one (`tests/metamorphic.rs`). Default templates are the table of §7.1; a
//! subcircuit row is spelled by SpiceRack's `push_instance`.
//!
//! Contents:
//! - `pub(crate) struct Context`
//! - `pub(crate) struct Uses`, `Uses::merge(&mut self, other: Uses)`
//! - `pub(crate) struct ModelUse`
//! - `pub(crate) struct RowScratch`, `RowScratch::new(elab: &Elab) -> RowScratch`
//! - `pub(crate) fn line<'net, F: Fn(Name) -> &'net str>(context, index, name, net, out, scratch) -> Result<(), String>`

use crate::plan;
use crate::plan::{Form, Lang, Plan, Plans, Source};
use schemify_elab::{COMPUTED, DEFAULT, Dialect, Elab, Iface, Name, Target, Val};
use schemify_expr::{Scratch, Segment, Template, Value};
use spicerack::codegen::xschem;
use std::ops::Range;

/// What every row of one lowering reads: the elaboration, the dialect and its template
/// plans.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Context<'elab> {
    pub(crate) elab: &'elab Elab,
    pub(crate) dialect: Dialect,
    /// Built from `elab` for `dialect`.
    pub(crate) plans: &'elab Plans,
}

/// What once-only text the emitted rows need.
#[derive(Debug)]
pub(crate) struct Uses {
    /// Per leaf impl row: some emitted row instantiates it (its body is emitted).
    pub(crate) impl_used: Vec<bool>,
    /// The models of Verilog-A and d_cosim rows (one `.model` each).
    pub(crate) models: Vec<ModelUse>,
}

/// One `.model` a row needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModelUse {
    pub(crate) model: Name,
    /// The leaf impl row the model belongs to; always a used one.
    pub(crate) leaf: u32,
}

/// Per-line scratch, reused across the lines of one thread.
#[derive(Debug)]
pub(crate) struct RowScratch {
    /// Identifier texts of the slow path, one range each in `text_ranges`.
    texts: String,
    text_ranges: Vec<Range<u32>>,
    template: Scratch,
    pub(crate) uses: Uses,
}

/// One row as the renderers see it.
struct Row<'row, F> {
    elab: &'row Elab,
    dialect: Dialect,
    plan: &'row Plan,
    /// The instance row.
    index: usize,
    /// The instance name before dialect escaping.
    name: &'row str,
    /// `cell` (definition name or leaf stem).
    cell: Name,
    /// The cell's interface.
    iface: &'row Iface,
    /// The row's connections.
    conn: &'row [Name],
    net: F,
}

/// Which pins `in_pins` / `out_pins` list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PinGroup {
    /// Ordinary `in` pins.
    Inputs,
    /// Every other pin: `out`, `inout`, inherited.
    Others,
}

/// Template identifiers held inline by the slow path before it allocates.
const INLINE_VALUES: usize = 16;

impl Uses {
    /// Adds what a later run of rows used (`models` keep row order; their lines are sorted
    /// and deduplicated when written).
    pub(crate) fn merge(&mut self, other: Uses) {
        assert_eq!(
            other.impl_used.len(),
            self.impl_used.len(),
            "uses of one elaboration"
        );
        for model in &other.models {
            assert!(
                other.impl_used[model.leaf as usize],
                "a model's impl is used"
            );
        }
        for (a, b) in self.impl_used.iter_mut().zip(other.impl_used) {
            *a |= b;
        }
        self.models.extend(other.models);
    }
}

impl RowScratch {
    /// Empty scratch for the rows of `elab`.
    pub(crate) fn new(elab: &Elab) -> RowScratch {
        assert_eq!(elab.impls.cell.len(), elab.impls.index.len());
        RowScratch {
            texts: String::new(),
            text_ranges: Vec::new(),
            template: Scratch::default(),
            uses: Uses {
                impl_used: vec![false; elab.impls.cell.len()],
                models: Vec::new(),
            },
        }
    }
}

/// Appends instance row `index`'s text to `out` (no final newline unless the template
/// writes one; empty when the template renders nothing). `name` is the instance name
/// before escaping; `net` gives a connection's text (never called with [`Name::NONE`],
/// which is `?`).
///
/// # Errors
///
/// The message of a failed render (`X0xx` from the template, `E008` for a param
/// without a value, `E013` for a lang without a default template); `out` is then
/// unspecified past its length at entry (the caller truncates).
pub(crate) fn line<'net, F: Fn(Name) -> &'net str>(
    context: Context<'_>,
    index: usize,
    name: &str,
    net: F,
    out: &mut String,
    scratch: &mut RowScratch,
) -> Result<(), String> {
    let Context {
        elab,
        dialect,
        plans,
    } = context;
    assert!(
        index < elab.insts.name.len(),
        "row {index} is an instance row"
    );
    assert_eq!(elab.insts.param_start.len(), elab.insts.name.len() + 1);
    assert_eq!(
        scratch.uses.impl_used.len(),
        elab.impls.cell.len(),
        "scratch of this elaboration"
    );
    let (cell, iface, leaf) = match elab.insts.target[index] {
        Target::Def(def) => {
            let iface = &elab.cells[elab.defs.cell[def as usize] as usize];
            (elab.defs.name[def as usize], iface, u32::MAX)
        }
        Target::Leaf(leaf) => {
            scratch.uses.impl_used[leaf as usize] = true;
            let iface = &elab.cells[elab.impls.cell[leaf as usize] as usize];
            (iface.stem, iface, leaf)
        }
    };
    let row = Row {
        elab,
        dialect,
        plan: plans.of_row(elab, index),
        index,
        name,
        cell,
        iface,
        conn: elab.conns(index),
        net,
    };
    assert_eq!(row.plan.slots.len(), iface.pin_name.len());
    match &row.plan.form {
        Form::Bad(message) => Err(message.clone()),
        Form::Default => default_line(&row, leaf, out, &mut scratch.uses),
        Form::Template {
            template,
            sources,
            fast,
        } if *fast && fast_ok(&row, sources) => {
            for segment in template.segments() {
                match segment {
                    Segment::Lit(s) => out.push_str(s),
                    Segment::Hole { ident: Some(i) } => {
                        write_source(&row, sources[i as usize], out);
                    }
                    Segment::Hole { ident: None } => unreachable!("a fast plan has bare holes"),
                }
            }
            Ok(())
        }
        Form::Template {
            template, sources, ..
        } => slow_line(&row, template, sources, out, scratch),
    }
}

// ---- private ----

/// The fast form copies param texts; a `cnum` or failed value must go through the
/// template so it reports the same error.
fn fast_ok<F>(row: &Row<'_, F>, sources: &[Source]) -> bool {
    let param_start = row.elab.insts.param_start[row.index] as usize;
    // Elab writes one param entry per schema param, so `Param(k)` indexes stay in range.
    assert_eq!(
        row.elab.params_of(row.index).len(),
        row.iface.param_name.len()
    );
    !sources.iter().any(|s| {
        matches!(s, Source::Param(k) if matches!(
            row.elab.insts.param_val[param_start + *k as usize],
            Val::Cnum(..) | Val::Err(_) | Val::Upstream
        ))
    })
}

/// Appends the text of one identifier source (`Param` and `Missing` are the slow path's).
fn write_source<'net, F: Fn(Name) -> &'net str>(
    row: &Row<'_, F>,
    source: Source,
    out: &mut String,
) {
    let elab = row.elab;
    match source {
        Source::Name => crate::push_name(out, row.name, row.dialect),
        Source::Cell => crate::push_name(out, elab.names.get(row.cell), row.dialect),
        Source::Text(n) => out.push_str(elab.names.get(n)),
        Source::Pins => push_nets(out, row.conn, row),
        Source::InPins => dir_nets(out, row, PinGroup::Inputs),
        Source::OutPins => dir_nets(out, row, PinGroup::Others),
        // A specialized subckt takes no params (spec §6).
        Source::Params if matches!(elab.insts.target[row.index], Target::Def(_)) => {}
        Source::Params => {
            let start = out.len();
            for k in elab.params_of(row.index) {
                if elab.insts.param_flags[k] & (COMPUTED | DEFAULT) == 0 {
                    if out.len() > start {
                        out.push(' ');
                    }
                    out.push_str(elab.names.get(elab.insts.param_name[k]));
                    out.push('=');
                    out.push_str(elab.names.get(elab.insts.param_text[k]));
                }
            }
        }
        Source::Pin(p) => push_nets(out, pin_bits(row, p as usize), row),
        Source::Param(k) => {
            let params = elab.params_of(row.index);
            debug_assert!((k as usize) < params.len(), "param {k} of the row's schema");
            out.push_str(
                elab.names
                    .get(elab.insts.param_text[params.start + k as usize]),
            );
        }
        Source::Missing => unreachable!("plans with unknown identifiers take the slow path"),
    }
}

/// Renders through `schemify_expr::Template` (any hole expression).
fn slow_line<'net, F: Fn(Name) -> &'net str>(
    row: &Row<'_, F>,
    template: &Template,
    sources: &[Source],
    out: &mut String,
    scratch: &mut RowScratch,
) -> Result<(), String> {
    let elab = row.elab;
    assert_eq!(sources.len(), template.program().idents().len());
    // The texts of every non-param source, written once into `texts`.
    scratch.texts.clear();
    scratch.text_ranges.clear();
    for &source in sources {
        let start = scratch.texts.len() as u32;
        if !matches!(source, Source::Param(_) | Source::Missing) {
            write_source(row, source, &mut scratch.texts);
        }
        scratch.text_ranges.push(start..scratch.texts.len() as u32);
    }
    assert!(
        scratch.texts.len() < u32::MAX as usize,
        "one row's identifier texts stay below 4 GiB"
    );
    assert_eq!(scratch.text_ranges.len(), sources.len());
    let param_start = elab.insts.param_start[row.index] as usize;
    let mut small = [Value::Int(0); INLINE_VALUES];
    let mut big = Vec::new();
    let values: &mut [Value<'_>] = if sources.len() <= small.len() {
        &mut small[..sources.len()]
    } else {
        big.resize(sources.len(), Value::Int(0));
        &mut big
    };
    for (i, source) in sources.iter().enumerate() {
        values[i] = match *source {
            Source::Param(k) => {
                let value = &elab.insts.param_val[param_start + k as usize];
                value.value().ok_or_else(|| {
                    let name = elab
                        .names
                        .get(elab.insts.param_name[param_start + k as usize]);
                    format!("E008: param `{name}` has no value")
                })?
            }
            Source::Missing => {
                let id = template.program().idents().nth(i).unwrap_or("?");
                return Err(format!("X004: unknown identifier `{id}`"));
            }
            _ => {
                let range = scratch.text_ranges[i].clone();
                Value::Str(&scratch.texts[range.start as usize..range.end as usize])
            }
        };
    }
    template
        .render(values, &mut scratch.template, out)
        .map_err(|err| err.to_string())
}

/// The connection bits of pin `pin` (empty when the row is short of them, `E012`).
fn pin_bits<'row, F>(row: &Row<'row, F>, pin: usize) -> &'row [Name] {
    row.conn.get(row.plan.slots[pin].bits()).unwrap_or(&[])
}

/// Appends `nets` space-separated in the row's dialect; an unconnected bit is `?`.
fn push_nets<'net, F: Fn(Name) -> &'net str>(out: &mut String, nets: &[Name], row: &Row<'_, F>) {
    for (i, &net) in nets.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if net == Name::NONE {
            out.push('?');
        } else {
            crate::push_name(out, (row.net)(net), row.dialect);
        }
    }
}

/// Appends the connections of the cell's pins in `group`, in pin order, space-separated;
/// alias pins are skipped.
fn dir_nets<'net, F: Fn(Name) -> &'net str>(out: &mut String, row: &Row<'_, F>, group: PinGroup) {
    let iface = row.iface;
    debug_assert_eq!(iface.pin_alias.len(), iface.pin_name.len());
    let start = out.len();
    for p in 0..iface.pin_name.len() {
        let in_group = plan::is_input(iface, p) == (group == PinGroup::Inputs);
        if !in_group || iface.pin_alias[p] != u32::MAX {
            continue; // an alias pin is its target's connection, listed once
        }
        let bits = pin_bits(row, p);
        if !bits.is_empty() && out.len() > start {
            out.push(' ');
        }
        push_nets(out, bits, row);
    }
}

/// The default templates of spec §7.1 (and §7.5 for spectre and verilog): a subcircuit
/// row is spelled by SpiceRack (`X{name} {pins} {cell}`, Spectre `{name} ({pins})
/// {cell}`, Verilog `{cell} {name} (.{port}({net}), …);`).
fn default_line<'net, F: Fn(Name) -> &'net str>(
    row: &Row<'_, F>,
    leaf: u32,
    out: &mut String,
    uses: &mut Uses,
) -> Result<(), String> {
    let (elab, dialect, iface) = (row.elab, row.dialect, row.iface);
    assert_eq!(
        leaf == u32::MAX,
        matches!(elab.insts.target[row.index], Target::Def(_)),
        "a leaf impl row exactly when the row targets a leaf"
    );
    let start = out.len();
    let model_name = || match row.plan.model {
        Source::Text(n) => n,
        Source::Param(k) => {
            elab.insts.param_text[elab.insts.param_start[row.index] as usize + k as usize]
        }
        _ => row.cell,
    };
    match (row.plan.lang, dialect) {
        (Lang::Schematic | Lang::Spice, _) if crate::is_spice(dialect) => instance(row, out),
        (Lang::Schematic, Dialect::Spectre | Dialect::Verilog) => {
            assert!(
                matches!(elab.insts.target[row.index], Target::Def(_)),
                "a definition row"
            );
            instance(row, out);
        }
        (Lang::Verilog, Dialect::Verilog) => {
            write_source(row, row.plan.model, out);
            let pins = 0..iface.pin_name.len();
            let ordinary = pins
                .clone()
                .filter(|&p| iface.pin_inherit[p] == Name::NONE && iface.pin_alias[p] == u32::MAX);
            let inherited = pins.filter(|&p| iface.pin_inherit[p] != Name::NONE);
            let bits = ordinary
                .chain(inherited)
                .flat_map(|p| iface.bits(p).iter().map(|b| elab.names.get(*b)));
            module_ports(out, row, bits);
        }
        (Lang::VerilogA, Dialect::Ngspice | Dialect::Espice) => {
            push_prefixed(out, b'N', row.name, dialect);
            for &net in row.conn {
                out.push(' ');
                push_nets(out, &[net], row);
            }
            out.push(' ');
            write_source(row, row.plan.model, out);
            if dialect == Dialect::Espice {
                va_params(row, out);
            }
            push_model(uses, model_name(), leaf);
        }
        (Lang::Verilog, Dialect::Ngspice) => {
            push_prefixed(out, b'A', row.name, dialect);
            out.push_str(" [ ");
            dir_nets(out, row, PinGroup::Inputs);
            out.push_str(" ] [ ");
            dir_nets(out, row, PinGroup::Others);
            out.push_str(" ] ");
            write_source(row, row.plan.model, out);
            push_model(uses, model_name(), leaf);
        }
        (lang, _) => {
            return Err(format!(
                "E013: no default template for a {lang:?} impl in {}",
                dialect.as_str()
            ));
        }
    }
    assert!(out.len() > start, "a default template writes the element");
    Ok(())
}

/// Appends the subcircuit instance of a default-template row as SpiceRack spells an
/// IR `Instance` (`xschem::push_instance`): its name, its cell, its connections in the
/// row's spelling (`?` unconnected); Verilog pairs them with the definition's ports.
fn instance<'net, F: Fn(Name) -> &'net str>(row: &Row<'_, F>, out: &mut String) {
    let elab = row.elab;
    let nets = row
        .conn
        .iter()
        .map(|&n| if n == Name::NONE { "?" } else { (row.net)(n) });
    let ports: &[Name] = match (elab.insts.target[row.index], row.dialect) {
        (Target::Def(def), Dialect::Verilog) => elab.ports(def),
        _ => &[],
    };
    let start = out.len();
    xschem::push_instance(
        out,
        row.name,
        elab.names.get(row.cell),
        nets,
        ports.iter().map(|&p| crate::net_text(elab, p)),
        [],
        crate::spicerack_dialect(row.dialect),
    );
    assert!(out.len() > start);
}

/// Appends `name` with the device letter `letter` in front unless it already starts
/// with it (ASCII case-insensitive): `X1` stays `X1`, `M1` becomes `XM1` (spec §7.6.5).
fn push_prefixed(out: &mut String, letter: u8, name: &str, dialect: Dialect) {
    assert!(letter.is_ascii_uppercase());
    let start = out.len();
    if !name
        .as_bytes()
        .first()
        .is_some_and(|b| b.eq_ignore_ascii_case(&letter))
    {
        out.push(char::from(letter));
    }
    crate::push_name(out, name, dialect);
    assert!(out.len() > start);
}

/// Appends ` k=v` for every param of a Verilog-A leaf row that differs from its default
/// (spec §7.2a: ESPice reads instance parameters from the `N` line), except computed params
/// and the `model` param the line already names; `m` is written `mfactor`, the LRM name
/// ESPice's Verilog-A instances take (`m=` is its `UnknownParameter`).
fn va_params<'net, F: Fn(Name) -> &'net str>(row: &Row<'_, F>, out: &mut String) {
    let elab = row.elab;
    assert_eq!(row.plan.lang, Lang::VerilogA, "a Verilog-A row");
    let params = elab.params_of(row.index);
    let model = match row.plan.model {
        Source::Param(k) => params.start + k as usize,
        _ => usize::MAX,
    };
    assert!(model == usize::MAX || params.contains(&model));
    for k in params {
        if elab.insts.param_flags[k] & (COMPUTED | DEFAULT) != 0 || k == model {
            continue;
        }
        let name = elab.names.get(elab.insts.param_name[k]);
        out.push(' ');
        out.push_str(if name.eq_ignore_ascii_case("m") {
            "mfactor"
        } else {
            name
        });
        out.push('=');
        out.push_str(elab.names.get(elab.insts.param_text[k]));
    }
}

/// Records that leaf impl row `leaf` needs a `.model` named `model` (consecutive
/// duplicates folded; the rest are folded when written).
fn push_model(uses: &mut Uses, model: Name, leaf: u32) {
    assert!(leaf != u32::MAX, "only leaf impls declare models");
    assert!(
        uses.impl_used[leaf as usize],
        "the row marked its impl used"
    );
    let model_use = ModelUse { model, leaf };
    if uses.models.last() != Some(&model_use) {
        uses.models.push(model_use);
    }
}

/// Appends ` <name> (.<port>(<net>), …);` for a verilog module instance.
fn module_ports<'port, 'net, F: Fn(Name) -> &'net str>(
    out: &mut String,
    row: &Row<'_, F>,
    ports: impl Iterator<Item = &'port str>,
) {
    assert!(!out.is_empty(), "the module name precedes the instance");
    assert_eq!(row.dialect, Dialect::Verilog);
    out.push(' ');
    crate::push_name(out, row.name, row.dialect);
    out.push_str(" (");
    let connections = ports.zip(row.conn);
    for (i, (port, &net)) in connections.enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push('.');
        crate::push_name(out, port, row.dialect);
        out.push('(');
        push_nets(out, &[net], row);
        out.push(')');
    }
    out.push_str(");");
}
