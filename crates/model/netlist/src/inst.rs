//! Instance lines (netlist spec §7.1, expr spec §6): one [`Plan`] per template, then one
//! line per instance row.
//!
//! A plan resolves each template identifier once to where its value lives ([`Src`]),
//! so a row binds by index, not by name. Templates whose holes are all bare
//! identifiers (the usual `M{name} {D} {G} {S} {B} {model} W={W}`) also get a fast form
//! that copies the canonical texts elab already wrote instead of evaluating; the
//! `schemify_expr::Template` render stays as the general path and as the oracle of the
//! fast one (`tests/metamorphic.rs`).

use crate::{is_spice, push_name};
use schemify_elab::{COMPUTED, DEFAULT, Dialect, Elab, Name, PinDir, Target, Val};
use schemify_expr::{Scratch, Segment, Template, Value};

/// Where a template identifier's value comes from (expr spec §6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Src {
    /// `name`: the instance name as emitted.
    Name,
    /// `cell`: the definition name of a schematic target, the stem of a leaf.
    Cell,
    /// Text fixed per impl (`model` and `file` attrs), verbatim.
    Text(Name),
    /// `pins`: every connection, space-joined.
    Pins,
    /// `in_pins` / `out_pins`: connections of `in` pins / of the others.
    InPins,
    /// See [`Src::InPins`].
    OutPins,
    /// `params`: `k=v` of the non-computed params that differ from the default.
    Params,
    /// A pin, by index into the cell's pins.
    Pin(u32),
    /// A param, by index into the cell's param schema.
    Param(u32),
    /// An identifier nothing defines (rendering fails with `X004`).
    Missing,
}

/// Impl langs with a default template (spec §7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lang {
    Schematic,
    Spice,
    VerilogA,
    Verilog,
    Other,
}

#[derive(Debug)]
enum Form {
    /// The lang's default template.
    Default,
    /// A `format` template.
    Template {
        tmpl: Box<Template>,
        srcs: Vec<Src>,
        /// Every hole is a bare identifier with a source: rows copy texts (see `fast_ok`).
        fast: bool,
    },
    /// The template does not parse: every line using it is this error.
    Bad(String),
}

/// How the instance lines of one impl are written.
#[derive(Debug)]
pub(crate) struct Plan {
    lang: Lang,
    form: Form,
    /// Per pin of the cell: `(first bit, bits)` in a row's connections (ordinary pins
    /// by elab's `pin_off`, then inherited pins in pin order after `bits_total`).
    slots: Vec<(u32, u32)>,
    /// The model name for default templates: `model` attr, else the `model` param,
    /// else the cell stem.
    model: Src,
    /// Per pin of the cell: written inside an XSPICE port vector (`[ … ]` of the
    /// template, or the default `verilog` template's `[{in_pins}] [{out_pins}]`).
    pub(crate) vector: Vec<bool>,
}

impl Plan {
    /// `(first bit, bits)` of pin `p` in a row's connections.
    pub(crate) fn slot(&self, p: usize) -> (u32, u32) {
        self.slots[p]
    }
}

/// Whether pin `p` of `f` is an input for `in_pins` (an ordinary `in` pin); every other
/// pin is an `out_pins` one.
fn is_input(f: &schemify_elab::Iface, p: usize) -> bool {
    f.pin_dir[p] == PinDir::In && f.pin_inherit[p] == Name::NONE
}

/// Plans for every leaf impl row and every reached cell's schematic impl.
#[derive(Debug)]
pub(crate) struct Plans {
    leaf: Vec<Plan>,
    def: Vec<Plan>,
}

impl Plans {
    /// Builds the plans. Cost: one template parse per leaf impl row and per cell with
    /// a schematic `format` for `d`.
    pub(crate) fn new(e: &Elab, d: Dialect) -> Plans {
        assert_eq!(e.impls.cell.len(), e.impls.template.len());
        let leaf: Vec<Plan> = (0..e.impls.cell.len())
            .map(|l| {
                let c = e.impls.cell[l];
                plan(e, c, e.impls.index[l], e.impls.template[l])
            })
            .collect();
        let def: Vec<Plan> = (0..e.cells.len() as u32)
            .map(|c| {
                let f = &e.cells[c as usize];
                match f.schematic_impl(&e.names) {
                    Some(k) => {
                        let fmts = f.fmt_start[k] as usize..f.fmt_start[k + 1] as usize;
                        let tmpl = d.format_words().iter().find_map(|w| {
                            fmts.clone()
                                .find(|&j| e.names.get(f.fmt_dialect[j]) == *w)
                                .map(|j| f.fmt_template[j])
                        });
                        plan(e, c, k as u32, tmpl.unwrap_or(Name::NONE))
                    }
                    None => plan(e, c, u32::MAX, Name::NONE),
                }
            })
            .collect();
        assert_eq!(def.len(), e.cells.len());
        Plans { leaf, def }
    }

    /// The plan that writes instance row `r`.
    pub(crate) fn of_row(&self, e: &Elab, r: usize) -> &Plan {
        match e.insts.target[r] {
            Target::Def(c) => &self.def[e.defs.cell[c as usize] as usize],
            Target::Leaf(l) => &self.leaf[l as usize],
        }
    }
}

/// Builds the plan of impl `k` of cell `c` (`k == u32::MAX`: a cell without one).
fn plan(e: &Elab, c: u32, k: u32, template: Name) -> Plan {
    let f = &e.cells[c as usize];
    let names = &e.names;
    let lang = match f.impl_lang.get(k as usize).map(|l| names.get(*l)) {
        Some("schematic") | None => Lang::Schematic,
        Some("spice") => Lang::Spice,
        Some("veriloga") => Lang::VerilogA,
        Some("verilog") => Lang::Verilog,
        Some(_) => Lang::Other,
    };
    let mut slots = Vec::with_capacity(f.pin_name.len());
    let mut inherited = f.bits_total;
    for p in 0..f.pin_name.len() {
        let w = f.bits(p).len() as u32;
        if f.pin_inherit[p] == Name::NONE {
            slots.push((f.pin_off[p], w));
        } else {
            slots.push((inherited, w));
            inherited += w;
        }
    }
    let param = |n: &str| f.param_name.iter().position(|p| names.get(*p) == n);
    let attr = |col: &[Name]| col.get(k as usize).copied().filter(|n| *n != Name::NONE);
    // A template names a pin by its base name (`D` for `D[3:0]`).
    let base = |p: usize| names.get(f.pin_name[p]).split('[').next().unwrap_or("");
    let model = match (attr(&f.impl_model), param("model")) {
        (Some(m), _) => Src::Text(m),
        (None, Some(p)) => Src::Param(p as u32),
        (None, None) => Src::Cell,
    };
    let n = f.pin_name.len();
    // The default `verilog` template writes `[{in_pins}] [{out_pins}]`: every pin.
    let mut vector = vec![template == Name::NONE && lang == Lang::Verilog; n];
    let form = if template == Name::NONE {
        Form::Default
    } else {
        let text = names.get(template);
        match Template::parse(text) {
            Err(err) => Form::Bad(err.to_string()),
            Ok(tmpl) => {
                let idents: Vec<&str> = tmpl.program().idents().collect();
                let srcs: Vec<Src> = idents
                    .iter()
                    .map(|&id| match id {
                        "name" => Src::Name,
                        "cell" => Src::Cell,
                        "pins" => Src::Pins,
                        "in_pins" => Src::InPins,
                        "out_pins" => Src::OutPins,
                        "params" => Src::Params,
                        "model" if model != Src::Cell => model,
                        "file" => attr(&f.impl_file).map_or(Src::Missing, Src::Text),
                        _ => match ((0..n).find(|&p| base(p) == id), param(id)) {
                            (Some(p), _) => Src::Pin(p as u32),
                            (None, Some(q)) => Src::Param(q as u32),
                            (None, None) => Src::Missing,
                        },
                    })
                    .collect();
                // Bare-identifier holes written inside `[ … ]` of the literal text
                // (an XSPICE port vector).
                let (mut in_vector, mut depth, mut fast) = (Vec::new(), 0u32, true);
                for seg in tmpl.segments() {
                    match seg {
                        Segment::Lit(s) => {
                            for c in s.bytes() {
                                match c {
                                    b'[' => depth += 1,
                                    b']' => depth = depth.saturating_sub(1),
                                    _ => {}
                                }
                            }
                        }
                        Segment::Hole { ident: Some(k) } if depth > 0 => {
                            in_vector.push(idents[k as usize]);
                        }
                        Segment::Hole { ident: Some(_) } => {}
                        Segment::Hole { ident: None } => fast = false,
                    }
                }
                for (p, v) in vector.iter_mut().enumerate() {
                    *v = in_vector.iter().any(|&id| match id {
                        "pins" => true,
                        "in_pins" => is_input(f, p),
                        "out_pins" => !is_input(f, p),
                        _ => base(p) == id,
                    });
                }
                Form::Template {
                    tmpl: Box::new(tmpl),
                    fast: fast && !srcs.contains(&Src::Missing),
                    srcs,
                }
            }
        }
    };
    assert_eq!(slots.len(), n);
    assert_eq!(vector.len(), n);
    Plan {
        lang,
        form,
        slots,
        model,
        vector,
    }
}

/// What once-only text the emitted rows need.
#[derive(Debug, Default)]
pub(crate) struct Uses {
    /// Per leaf impl row: some emitted row instantiates it (its body is emitted).
    pub(crate) impl_used: Vec<bool>,
    /// `(model, impl row)` of Verilog-A and d_cosim rows (one `.model` each).
    pub(crate) models: Vec<(Name, u32)>,
}

impl Uses {
    /// Adds what a later run of rows used (`models` keep row order; their lines are sorted
    /// and deduplicated when written).
    pub(crate) fn merge(&mut self, other: Uses) {
        if other.impl_used.len() == self.impl_used.len() {
            for (a, b) in self.impl_used.iter_mut().zip(other.impl_used) {
                *a |= b;
            }
        } else {
            assert!(other.impl_used.is_empty(), "uses of one elaboration");
        }
        self.models.extend(other.models);
    }
}

/// Per-line scratch, reused across lines.
#[derive(Debug)]
pub(crate) struct Tmp {
    buf: String,
    ranges: Vec<(u32, u32)>,
    scratch: Scratch,
    pub(crate) uses: Uses,
}

impl Tmp {
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
    /// The row's connections.
    conn: &'a [Name],
    net: F,
}

/// Appends instance row `r`'s text to `out` (no final newline unless the template
/// writes one; empty when the template renders nothing). `name` is the instance name
/// before escaping; `net` gives a connection's text (never called with
/// [`Name::NONE`], which is written `?`).
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
    let (plan, cell, leaf) = match e.insts.target[r] {
        Target::Def(c) => {
            let plan = &plans.def[e.defs.cell[c as usize] as usize];
            (plan, e.defs.name[c as usize], u32::MAX)
        }
        Target::Leaf(l) => {
            tmp.uses.impl_used[l as usize] = true;
            let stem = e.cells[e.impls.cell[l as usize] as usize].stem;
            (&plans.leaf[l as usize], stem, l)
        }
    };
    let row = Row {
        e,
        d,
        plan,
        r,
        name,
        cell,
        conn: e.conns(r),
        net,
    };
    match &plan.form {
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

/// The fast form copies param texts; a `cnum` or failed value must go through the
/// template so it reports the same error.
fn fast_ok<F>(row: &Row<'_, F>, srcs: &[Src]) -> bool {
    let p0 = row.e.insts.param_start[row.r] as usize;
    // Elab writes one param entry per schema param, so `Param(k)` indexes stay in range.
    assert_eq!(
        row.e.params_of(row.r).len(),
        row.e.cells[cell_of(row)].param_name.len()
    );
    // A plan with a `Missing` source has no fast form.
    !srcs.iter().any(|s| {
        matches!(s, Src::Param(k) if matches!(
            row.e.insts.param_val[p0 + *k as usize],
            Val::Cnum(..) | Val::Err(_) | Val::Upstream
        ))
    })
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
    tmp.buf.clear();
    tmp.ranges.clear();
    for &s in srcs {
        let start = tmp.buf.len() as u32;
        if !matches!(s, Src::Param(_) | Src::Missing) {
            write_src(row, s, &mut tmp.buf);
        }
        tmp.ranges.push((start, tmp.buf.len() as u32));
    }
    let p0 = e.insts.param_start[row.r] as usize;
    let mut small = [Value::Int(0); 16];
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
    assert_eq!(tmp.ranges.len(), srcs.len());
    tmpl.render(vals, &mut tmp.scratch, out)
        .map_err(|err| err.to_string())
}

/// Appends the text of one identifier source.
fn write_src<'n, F: Fn(Name) -> &'n str>(row: &Row<'_, F>, s: Src, out: &mut String) {
    let e = row.e;
    let conn = row.conn;
    match s {
        Src::Name => push_name(out, row.name, row.d),
        Src::Cell => push_name(out, e.names.get(row.cell), row.d),
        Src::Text(n) => out.push_str(e.names.get(n)),
        Src::Pins => push_nets(out, conn, row),
        Src::InPins | Src::OutPins => dir_nets(out, row, s == Src::InPins),
        Src::Params => {
            if let Target::Leaf(_) = e.insts.target[row.r] {
                let mut first = true;
                for k in e.params_of(row.r) {
                    if e.insts.param_flags[k] & (COMPUTED | DEFAULT) == 0 {
                        if !first {
                            out.push(' ');
                        }
                        first = false;
                        out.push_str(e.names.get(e.insts.param_name[k]));
                        out.push('=');
                        out.push_str(e.names.get(e.insts.param_text[k]));
                    }
                }
            } // a specialized subckt takes no params (spec §6)
        }
        Src::Pin(p) => {
            assert!((p as usize) < row.plan.slots.len());
            let (a, w) = row.plan.slots[p as usize];
            let bits = conn.get(a as usize..(a + w) as usize).unwrap_or(&[]);
            push_nets(out, bits, row);
        }
        Src::Param(k) => {
            let p0 = e.insts.param_start[row.r] as usize;
            out.push_str(e.names.get(e.insts.param_text[p0 + k as usize]));
        }
        Src::Missing => unreachable!("plans with unknown identifiers take the slow path"),
    }
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
    let f = &row.e.cells[cell_of(row)];
    assert_eq!(row.plan.slots.len(), f.pin_name.len());
    let conn = row.conn;
    let start = out.len();
    for p in 0..f.pin_name.len() {
        if is_input(f, p) != inputs || f.pin_alias[p] != u32::MAX {
            continue; // an alias pin is its target's connection, listed once
        }
        let (a, w) = row.plan.slots[p];
        let bits = conn.get(a as usize..(a + w) as usize).unwrap_or(&[]);
        if !bits.is_empty() && out.len() > start {
            out.push(' ');
        }
        push_nets(out, bits, row);
    }
}

/// The elab cell index of the row's target.
fn cell_of<F>(row: &Row<'_, F>) -> usize {
    match row.e.insts.target[row.r] {
        Target::Def(c) => row.e.defs.cell[c as usize] as usize,
        Target::Leaf(l) => row.e.impls.cell[l as usize] as usize,
    }
}

/// Appends `name` with the device letter `letter` in front unless it already starts
/// with it (ASCII case-insensitive): `X1` stays `X1`, `M1` becomes `XM1` (spec §7.6).
fn push_prefixed(out: &mut String, letter: u8, name: &str, d: Dialect) {
    assert!(letter.is_ascii_uppercase());
    if !name
        .as_bytes()
        .first()
        .is_some_and(|b| b.eq_ignore_ascii_case(&letter))
    {
        out.push(char::from(letter));
    }
    push_name(out, name, d);
}

/// The default templates of spec §7.1 (and §7.5 for spectre and verilog).
fn default_line<'n, F: Fn(Name) -> &'n str>(
    row: &Row<'_, F>,
    leaf: u32,
    out: &mut String,
    uses: &mut Uses,
) -> Result<(), String> {
    let (e, d) = (row.e, row.d);
    let conn = row.conn;
    assert_eq!(conn.len(), e.conns(row.r).len());
    let model = |out: &mut String| write_src(row, row.plan.model, out);
    let model_name = || match row.plan.model {
        Src::Text(n) => n,
        Src::Param(k) => e.insts.param_text[e.insts.param_start[row.r] as usize + k as usize],
        _ => row.cell,
    };
    match (row.plan.lang, d) {
        (Lang::Schematic | Lang::Spice, _) if is_spice(d) => {
            push_prefixed(out, b'X', row.name, d);
            for &n in conn {
                out.push(' ');
                push_nets(out, &[n], row);
            }
            out.push(' ');
            push_name(out, e.names.get(row.cell), d);
        }
        (Lang::Schematic, Dialect::Spectre) => {
            push_name(out, row.name, d);
            out.push_str(" (");
            push_nets(out, conn, row);
            out.push_str(") ");
            push_name(out, e.names.get(row.cell), d);
        }
        (Lang::Schematic, Dialect::Verilog) => {
            let Target::Def(c) = e.insts.target[row.r] else {
                unreachable!("a schematic plan serves definition rows")
            };
            push_name(out, e.names.get(row.cell), d);
            module_ports(out, row, e.ports(c).iter().map(|p| e.names.get(*p)));
        }
        (Lang::Verilog, Dialect::Verilog) => {
            model(out);
            let f = &e.cells[cell_of(row)];
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
            for &n in conn {
                out.push(' ');
                push_nets(out, &[n], row);
            }
            out.push(' ');
            model(out);
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
            model(out);
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

fn push_model(uses: &mut Uses, model: Name, leaf: u32) {
    assert!(leaf != u32::MAX, "only leaf impls declare models");
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
