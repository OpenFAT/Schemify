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
use schemify_doc::PinDir;
use schemify_elab::{COMPUTED, DEFAULT, Dialect, Elab, Name, Target, Val};
use schemify_expr::{Scratch, Template, Value};

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

#[derive(Clone, Copy, Debug)]
enum Seg {
    /// Byte range of [`Fast::lits`].
    Lit(u32, u32),
    /// Identifier index (into the plan's sources).
    Hole(u32),
}

/// The fast form of a template whose holes are all bare identifiers.
#[derive(Debug)]
struct Fast {
    segs: Vec<Seg>,
    lits: String,
}

#[derive(Debug)]
enum Form {
    /// The lang's default template.
    Default,
    /// A `format` template.
    Template {
        tmpl: Box<Template>,
        srcs: Vec<Src>,
        fast: Option<Fast>,
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
    let model = match (attr(&f.impl_model), param("model")) {
        (Some(m), _) => Src::Text(m),
        (None, Some(p)) => Src::Param(p as u32),
        (None, None) => Src::Cell,
    };
    let form = if template == Name::NONE {
        Form::Default
    } else {
        let text = names.get(template);
        match Template::parse(text) {
            Err(err) => Form::Bad(err.to_string()),
            Ok(tmpl) => {
                let srcs: Vec<Src> = tmpl
                    .program()
                    .idents()
                    .map(|id| match id {
                        "name" => Src::Name,
                        "cell" => Src::Cell,
                        "pins" => Src::Pins,
                        "in_pins" => Src::InPins,
                        "out_pins" => Src::OutPins,
                        "params" => Src::Params,
                        "model" if model != Src::Cell => model,
                        "file" => attr(&f.impl_file).map_or(Src::Missing, Src::Text),
                        _ => {
                            let pin = (0..f.pin_name.len()).find(|&p| {
                                let n = names.get(f.pin_name[p]);
                                n.split('[').next() == Some(id)
                            });
                            match (pin, param(id)) {
                                (Some(p), _) => Src::Pin(p as u32),
                                (None, Some(q)) => Src::Param(q as u32),
                                (None, None) => Src::Missing,
                            }
                        }
                    })
                    .collect();
                let fast = if srcs.contains(&Src::Missing) {
                    None
                } else {
                    fast_form(text, &tmpl)
                };
                Form::Template {
                    tmpl: Box::new(tmpl),
                    srcs,
                    fast,
                }
            }
        }
    };
    assert_eq!(slots.len(), f.pin_name.len());
    Plan {
        lang,
        form,
        slots,
        model,
    }
}

/// The fast form of `src` when every hole is a bare identifier (`[A-Za-z_][A-Za-z0-9_]*`,
/// not `true`/`false`/`pi`), else `None`. Same lexical rules as `Template::parse`:
/// `{{`/`}}` are literal braces, a hole ends at the first `}`.
fn fast_form(src: &str, tmpl: &Template) -> Option<Fast> {
    let idents: Vec<&str> = tmpl.program().idents().collect();
    let (mut segs, mut lits) = (Vec::new(), String::new());
    let mut lit_start = 0usize;
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1)) {
            (b'{', Some(b'{')) | (b'}', Some(b'}')) => {
                lits.push(b[i] as char);
                i += 2;
            }
            (b'{', _) => {
                let end = i + 1 + src[i + 1..].find('}')?;
                let id = src[i + 1..end].trim();
                let bare = id.bytes().enumerate().all(|(k, c)| {
                    c == b'_' || c.is_ascii_alphabetic() || (k > 0 && c.is_ascii_digit())
                }) && !id.is_empty()
                    && !matches!(id, "true" | "false" | "pi");
                if !bare {
                    return None;
                }
                if lits.len() > lit_start {
                    segs.push(Seg::Lit(lit_start as u32, lits.len() as u32));
                }
                lit_start = lits.len();
                let k = idents.iter().position(|x| *x == id)?;
                segs.push(Seg::Hole(k as u32));
                i = end + 1;
            }
            (b'}', _) => return None,
            _ => {
                let next = src[i..].find(['{', '}']).map_or(src.len(), |k| i + k);
                lits.push_str(&src[i..next]);
                i = next;
            }
        }
    }
    if lits.len() > lit_start {
        segs.push(Seg::Lit(lit_start as u32, lits.len() as u32));
    }
    assert!(lits.len() <= src.len());
    assert!(
        segs.iter()
            .all(|s| !matches!(s, Seg::Hole(k) if *k as usize >= idents.len()))
    );
    Some(Fast { segs, lits })
}

/// What once-only text the emitted rows need.
#[derive(Debug, Default)]
pub(crate) struct Uses {
    /// Per leaf impl row: some emitted row instantiates it (its body is emitted).
    pub(crate) impl_used: Vec<bool>,
    /// `(model, impl row)` of Verilog-A and d_cosim rows (one `.model` each).
    pub(crate) models: Vec<(Name, u32)>,
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
        Form::Template { tmpl, srcs, fast } => match fast {
            Some(f) if fast_ok(&row, srcs) => {
                for seg in &f.segs {
                    match *seg {
                        Seg::Lit(a, b) => out.push_str(&f.lits[a as usize..b as usize]),
                        Seg::Hole(i) => write_src(&row, srcs[i as usize], out),
                    }
                }
                Ok(())
            }
            _ => slow_line(&row, tmpl, srcs, out, tmp),
        },
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
    srcs.iter().all(|s| match s {
        Src::Param(k) => !matches!(
            row.e.insts.param_val[p0 + *k as usize],
            Val::Cnum(..) | Val::Err(_) | Val::Upstream
        ),
        Src::Text(_) => true,
        _ => !matches!(s, Src::Missing),
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
/// (ordinary `out`/`inout` pins, then inherited pins), space-separated.
fn dir_nets<'n, F: Fn(Name) -> &'n str>(out: &mut String, row: &Row<'_, F>, inputs: bool) {
    let f = &row.e.cells[cell_of(row)];
    assert_eq!(row.plan.slots.len(), f.pin_name.len());
    let conn = row.conn;
    let start = out.len();
    for p in 0..f.pin_name.len() {
        let is_in = f.pin_dir[p] == PinDir::In && f.pin_inherit[p] == Name::NONE;
        if is_in != inputs {
            continue;
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
            let ordinary = (0..f.pin_name.len()).filter(|&p| f.pin_inherit[p] == Name::NONE);
            let inherited = (0..f.pin_name.len()).filter(|&p| f.pin_inherit[p] != Name::NONE);
            let bits = ordinary
                .chain(inherited)
                .flat_map(|p| f.bits(p).iter().map(|b| e.names.get(*b)));
            module_ports(out, row, bits);
        }
        (Lang::VerilogA, Dialect::Ngspice) => {
            push_prefixed(out, b'N', row.name, d);
            for &n in conn {
                out.push(' ');
                push_nets(out, &[n], row);
            }
            out.push(' ');
            model(out);
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
