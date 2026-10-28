//! Instance lines (netlist spec §7.1, §7.6.1, §7.6.5–7, expr spec §6): one instance row
//! and its [`Plan`] in, its element text out, plus the impls and models it uses.
//!
//! A `fast` template copies the canonical texts elab already wrote; any other goes
//! through `schemify_expr::Template`, which stays the general path and the oracle of the
//! fast one (`tests/metamorphic.rs`). Default templates are the table of §7.1; a
//! subcircuit row is spelled by SpiceRack's `push_instance`.
//!
//! Contents:
//! - `pub(crate) struct Uses`, `Uses::merge(&mut self, other: Uses)`
//! - `pub(crate) struct Tmp`, `Tmp::new(e: &Elab) -> Tmp`
//! - `pub(crate) fn line<'n, F: Fn(Name) -> &'n str>(e, d, plans, r, name, net, out, tmp) -> Result<(), String>`

use crate::plan::{Form, Lang, Plan, Plans, Src, is_input};
use crate::{is_spice, net_text, push_name, sr};
use schemify_elab::{COMPUTED, DEFAULT, Dialect, Elab, Iface, Name, Target, Val};
use schemify_expr::{Scratch, Segment, Template, Value};
use spicerack::codegen::xschem;

/// What once-only text the emitted rows need.
#[derive(Debug)]
pub(crate) struct Uses {
    /// Per leaf impl row: some emitted row instantiates it (its body is emitted).
    pub(crate) impl_used: Vec<bool>,
    /// `(model, impl row)` of Verilog-A and d_cosim rows (one `.model` each).
    pub(crate) models: Vec<(Name, u32)>,
}

/// Per-line scratch, reused across the lines of one thread.
#[derive(Debug)]
pub(crate) struct Tmp {
    /// Identifier texts of the slow path, one range each in `ranges`.
    buf: String,
    ranges: Vec<(u32, u32)>,
    scratch: Scratch,
    pub(crate) uses: Uses,
}

/// One row as the renderers see it.
struct Row<'a, F> {
    e: &'a Elab,
    d: Dialect,
    plan: &'a Plan,
    r: usize,
    /// The instance name before dialect escaping.
    name: &'a str,
    /// `cell` (definition name or leaf stem).
    cell: Name,
    /// The cell's interface.
    f: &'a Iface,
    /// The row's connections.
    conn: &'a [Name],
    net: F,
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
        for (a, b) in self.impl_used.iter_mut().zip(other.impl_used) {
            *a |= b;
        }
        self.models.extend(other.models);
    }
}

impl Tmp {
    /// Empty scratch for the rows of `e`.
    pub(crate) fn new(e: &Elab) -> Tmp {
        assert_eq!(e.impls.cell.len(), e.impls.index.len());
        Tmp {
            buf: String::new(),
            ranges: Vec::new(),
            scratch: Scratch::default(),
            uses: Uses {
                impl_used: vec![false; e.impls.cell.len()],
                models: Vec::new(),
            },
        }
    }
}

/// Appends instance row `r`'s text to `out` (no final newline unless the template
/// writes one; empty when the template renders nothing). `name` is the instance name
/// before escaping; `net` gives a connection's text (never called with [`Name::NONE`],
/// which is `?`).
///
/// # Errors
///
/// The message of a failed render (`X0xx` from the template, `E008` for a param
/// without a value, `E013` for a lang without a default template); `out` is then
/// unspecified past its length at entry (the caller truncates).
#[allow(clippy::too_many_arguments)]
pub(crate) fn line<'n, F: Fn(Name) -> &'n str>(
    e: &Elab,
    d: Dialect,
    plans: &Plans,
    r: usize,
    name: &str,
    net: F,
    out: &mut String,
    tmp: &mut Tmp,
) -> Result<(), String> {
    assert!(r < e.insts.name.len());
    assert_eq!(e.insts.param_start.len(), e.insts.name.len() + 1);
    let (cell, f, leaf) = match e.insts.target[r] {
        Target::Def(c) => {
            let f = &e.cells[e.defs.cell[c as usize] as usize];
            (e.defs.name[c as usize], f, u32::MAX)
        }
        Target::Leaf(l) => {
            tmp.uses.impl_used[l as usize] = true;
            let f = &e.cells[e.impls.cell[l as usize] as usize];
            (f.stem, f, l)
        }
    };
    let row = Row {
        e,
        d,
        plan: plans.of_row(e, r),
        r,
        name,
        cell,
        f,
        conn: e.conns(r),
        net,
    };
    assert_eq!(row.plan.slots.len(), f.pin_name.len());
    match &row.plan.form {
        Form::Bad(msg) => Err(msg.clone()),
        Form::Default => default_line(&row, leaf, out, &mut tmp.uses),
        Form::Template { tmpl, srcs, fast } if *fast && fast_ok(&row, srcs) => {
            for seg in tmpl.segments() {
                match seg {
                    Segment::Lit(s) => out.push_str(s),
                    Segment::Hole { ident: Some(i) } => write_src(&row, srcs[i as usize], out),
                    Segment::Hole { ident: None } => unreachable!("a fast plan has bare holes"),
                }
            }
            Ok(())
        }
        Form::Template { tmpl, srcs, .. } => slow_line(&row, tmpl, srcs, out, tmp),
    }
}

// ---- private ----

/// The fast form copies param texts; a `cnum` or failed value must go through the
/// template so it reports the same error.
fn fast_ok<F>(row: &Row<'_, F>, srcs: &[Src]) -> bool {
    let p0 = row.e.insts.param_start[row.r] as usize;
    // Elab writes one param entry per schema param, so `Param(k)` indexes stay in range.
    assert_eq!(row.e.params_of(row.r).len(), row.f.param_name.len());
    !srcs.iter().any(|s| {
        matches!(s, Src::Param(k) if matches!(
            row.e.insts.param_val[p0 + *k as usize],
            Val::Cnum(..) | Val::Err(_) | Val::Upstream
        ))
    })
}

/// Appends the text of one identifier source (`Param` and `Missing` are the slow path's).
fn write_src<'n, F: Fn(Name) -> &'n str>(row: &Row<'_, F>, s: Src, out: &mut String) {
    let e = row.e;
    match s {
        Src::Name => push_name(out, row.name, row.d),
        Src::Cell => push_name(out, e.names.get(row.cell), row.d),
        Src::Text(n) => out.push_str(e.names.get(n)),
        Src::Pins => push_nets(out, row.conn, row),
        Src::InPins | Src::OutPins => dir_nets(out, row, s == Src::InPins),
        // A specialized subckt takes no params (spec §6).
        Src::Params if matches!(e.insts.target[row.r], Target::Def(_)) => {}
        Src::Params => {
            let start = out.len();
            for k in e.params_of(row.r) {
                if e.insts.param_flags[k] & (COMPUTED | DEFAULT) == 0 {
                    if out.len() > start {
                        out.push(' ');
                    }
                    out.push_str(e.names.get(e.insts.param_name[k]));
                    out.push('=');
                    out.push_str(e.names.get(e.insts.param_text[k]));
                }
            }
        }
        Src::Pin(p) => push_nets(out, pin_bits(row, p as usize), row),
        Src::Param(k) => {
            let p0 = e.insts.param_start[row.r] as usize;
            out.push_str(e.names.get(e.insts.param_text[p0 + k as usize]));
        }
        Src::Missing => unreachable!("plans with unknown identifiers take the slow path"),
    }
}

/// Renders through `schemify_expr::Template` (any hole expression).
fn slow_line<'n, F: Fn(Name) -> &'n str>(
    row: &Row<'_, F>,
    tmpl: &Template,
    srcs: &[Src],
    out: &mut String,
    tmp: &mut Tmp,
) -> Result<(), String> {
    let e = row.e;
    assert_eq!(srcs.len(), tmpl.program().idents().len());
    // The texts of every non-param source, written once into `buf`.
    tmp.buf.clear();
    tmp.ranges.clear();
    for &s in srcs {
        let start = tmp.buf.len() as u32;
        if !matches!(s, Src::Param(_) | Src::Missing) {
            write_src(row, s, &mut tmp.buf);
        }
        tmp.ranges.push((start, tmp.buf.len() as u32));
    }
    assert_eq!(tmp.ranges.len(), srcs.len());
    let p0 = e.insts.param_start[row.r] as usize;
    let mut small = [Value::Int(0); INLINE_VALUES];
    let mut big = Vec::new();
    let vals: &mut [Value<'_>] = if srcs.len() <= small.len() {
        &mut small[..srcs.len()]
    } else {
        big.resize(srcs.len(), Value::Int(0));
        &mut big
    };
    for (i, s) in srcs.iter().enumerate() {
        vals[i] = match *s {
            Src::Param(k) => {
                let v = &e.insts.param_val[p0 + k as usize];
                v.value().ok_or_else(|| {
                    let n = e.names.get(e.insts.param_name[p0 + k as usize]);
                    format!("E008: param `{n}` has no value")
                })?
            }
            Src::Missing => {
                let id = tmpl.program().idents().nth(i).unwrap_or("?");
                return Err(format!("X004: unknown identifier `{id}`"));
            }
            _ => {
                let (a, b) = tmp.ranges[i];
                Value::Str(&tmp.buf[a as usize..b as usize])
            }
        };
    }
    tmpl.render(vals, &mut tmp.scratch, out)
        .map_err(|err| err.to_string())
}

/// The connection bits of pin `p` (empty when the row is short of them, `E012`).
fn pin_bits<'a, F>(row: &Row<'a, F>, p: usize) -> &'a [Name] {
    let (a, w) = row.plan.slots[p];
    row.conn.get(a as usize..(a + w) as usize).unwrap_or(&[])
}

/// Appends `nets` space-separated in the row's dialect; an unconnected bit is `?`.
fn push_nets<'n, F: Fn(Name) -> &'n str>(out: &mut String, nets: &[Name], row: &Row<'_, F>) {
    for (i, &n) in nets.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if n == Name::NONE {
            out.push('?');
        } else {
            push_name(out, (row.net)(n), row.d);
        }
    }
}

/// Appends the connections of the cell's `in` pins (`inputs`), or of all other pins
/// (`out`, `inout`, inherited), in pin order, space-separated; alias pins are skipped.
fn dir_nets<'n, F: Fn(Name) -> &'n str>(out: &mut String, row: &Row<'_, F>, inputs: bool) {
    let f = row.f;
    let start = out.len();
    for p in 0..f.pin_name.len() {
        if is_input(f, p) != inputs || f.pin_alias[p] != u32::MAX {
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
fn default_line<'n, F: Fn(Name) -> &'n str>(
    row: &Row<'_, F>,
    leaf: u32,
    out: &mut String,
    uses: &mut Uses,
) -> Result<(), String> {
    let (e, d, f) = (row.e, row.d, row.f);
    let model_name = || match row.plan.model {
        Src::Text(n) => n,
        Src::Param(k) => e.insts.param_text[e.insts.param_start[row.r] as usize + k as usize],
        _ => row.cell,
    };
    match (row.plan.lang, d) {
        (Lang::Schematic | Lang::Spice, _) if is_spice(d) => instance(row, out),
        (Lang::Schematic, Dialect::Spectre | Dialect::Verilog) => {
            assert!(
                matches!(e.insts.target[row.r], Target::Def(_)),
                "a definition row"
            );
            instance(row, out);
        }
        (Lang::Verilog, Dialect::Verilog) => {
            write_src(row, row.plan.model, out);
            let ordinary = (0..f.pin_name.len())
                .filter(|&p| f.pin_inherit[p] == Name::NONE && f.pin_alias[p] == u32::MAX);
            let inherited = (0..f.pin_name.len()).filter(|&p| f.pin_inherit[p] != Name::NONE);
            let bits = ordinary
                .chain(inherited)
                .flat_map(|p| f.bits(p).iter().map(|b| e.names.get(*b)));
            module_ports(out, row, bits);
        }
        (Lang::VerilogA, Dialect::Ngspice | Dialect::Espice) => {
            push_prefixed(out, b'N', row.name, d);
            for &n in row.conn {
                out.push(' ');
                push_nets(out, &[n], row);
            }
            out.push(' ');
            write_src(row, row.plan.model, out);
            if d == Dialect::Espice {
                va_params(row, out);
            }
            push_model(uses, model_name(), leaf);
        }
        (Lang::Verilog, Dialect::Ngspice) => {
            push_prefixed(out, b'A', row.name, d);
            out.push_str(" [ ");
            dir_nets(out, row, true);
            out.push_str(" ] [ ");
            dir_nets(out, row, false);
            out.push_str(" ] ");
            write_src(row, row.plan.model, out);
            push_model(uses, model_name(), leaf);
        }
        (lang, _) => {
            return Err(format!(
                "E013: no default template for a {lang:?} impl in {}",
                d.as_str()
            ));
        }
    }
    Ok(())
}

/// Appends the subcircuit instance of a default-template row as SpiceRack spells an
/// IR `Instance` (`xschem::push_instance`): its name, its cell, its connections in the
/// row's spelling (`?` unconnected); Verilog pairs them with the definition's ports.
fn instance<'n, F: Fn(Name) -> &'n str>(row: &Row<'_, F>, out: &mut String) {
    let e = row.e;
    let nets = row
        .conn
        .iter()
        .map(|&n| if n == Name::NONE { "?" } else { (row.net)(n) });
    let ports: &[Name] = match (e.insts.target[row.r], row.d) {
        (Target::Def(c), Dialect::Verilog) => e.ports(c),
        _ => &[],
    };
    let start = out.len();
    xschem::push_instance(
        out,
        row.name,
        e.names.get(row.cell),
        nets,
        ports.iter().map(|&p| net_text(e, p)),
        [],
        sr(row.d),
    );
    assert!(out.len() > start);
}

/// Appends `name` with the device letter `letter` in front unless it already starts
/// with it (ASCII case-insensitive): `X1` stays `X1`, `M1` becomes `XM1` (spec §7.6.5).
fn push_prefixed(out: &mut String, letter: u8, name: &str, d: Dialect) {
    assert!(letter.is_ascii_uppercase());
    let start = out.len();
    if !name
        .as_bytes()
        .first()
        .is_some_and(|b| b.eq_ignore_ascii_case(&letter))
    {
        out.push(char::from(letter));
    }
    push_name(out, name, d);
    assert!(out.len() > start);
}

/// Appends ` k=v` for every param of a Verilog-A leaf row that differs from its default
/// (spec §7.2a: ESPice reads instance parameters from the `N` line), except computed params
/// and the `model` param the line already names; `m` is written `mfactor`, the LRM name
/// ESPice's Verilog-A instances take (`m=` is its `UnknownParameter`).
fn va_params<'n, F: Fn(Name) -> &'n str>(row: &Row<'_, F>, out: &mut String) {
    let e = row.e;
    let params = e.params_of(row.r);
    let model = match row.plan.model {
        Src::Param(k) => params.start + k as usize,
        _ => usize::MAX,
    };
    assert!(model == usize::MAX || params.contains(&model));
    for k in params {
        if e.insts.param_flags[k] & (COMPUTED | DEFAULT) != 0 || k == model {
            continue;
        }
        let name = e.names.get(e.insts.param_name[k]);
        out.push(' ');
        out.push_str(if name.eq_ignore_ascii_case("m") {
            "mfactor"
        } else {
            name
        });
        out.push('=');
        out.push_str(e.names.get(e.insts.param_text[k]));
    }
}

/// Records that leaf impl row `leaf` needs a `.model` named `model` (consecutive
/// duplicates folded; the rest are folded when written).
fn push_model(uses: &mut Uses, model: Name, leaf: u32) {
    assert!(leaf != u32::MAX, "only leaf impls declare models");
    assert!((leaf as usize) < uses.impl_used.len());
    if uses.models.last() != Some(&(model, leaf)) {
        uses.models.push((model, leaf));
    }
}

/// Appends ` <name> (.<port>(<net>), …);` for a verilog module instance.
fn module_ports<'p, 'n, F: Fn(Name) -> &'n str>(
    out: &mut String,
    row: &Row<'_, F>,
    ports: impl Iterator<Item = &'p str>,
) {
    assert!(!out.is_empty(), "the module name precedes the instance");
    out.push(' ');
    push_name(out, row.name, row.d);
    out.push_str(" (");
    for (i, (port, &n)) in ports.zip(row.conn).enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push('.');
        push_name(out, port, row.d);
        out.push('(');
        push_nets(out, &[n], row);
        out.push(')');
    }
    out.push_str(");");
}
