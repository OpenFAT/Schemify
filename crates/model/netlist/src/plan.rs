//! Template plans (netlist spec §7.1, §7.6.6–7, expr spec §6): elab's leaf impls and
//! cells in, one [`Plan`] per leaf impl row and per cell's schematic impl out.
//!
//! A plan resolves each template identifier once to where its value lives ([`Src`]),
//! so a row binds by index, not by name. Templates whose holes are all bare identifiers
//! (the usual `M{name} {D} {G} {S} {B} {model} W={W}`) are marked `fast`: rows copy the
//! canonical texts elab already wrote instead of evaluating (`inst.rs`).
//!
//! Contents:
//! - `pub(crate) enum Src`, `pub(crate) enum Lang`, `pub(crate) enum Form`
//! - `pub(crate) struct Plan`, `pub(crate) struct Plans`
//! - `pub(crate) fn is_input(f: &Iface, p: usize) -> bool`
//! - `Plans::new(e: &Elab, d: Dialect) -> Plans`, `Plans::of_row(&self, e: &Elab, r: usize) -> &Plan`

use schemify_elab::{Dialect, Elab, Iface, Name, Names, PinDir, Target};
use schemify_expr::{Segment, Template};

/// Where a template identifier's value comes from (expr spec §6, spec §7.6.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Src {
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
pub(crate) enum Lang {
    Schematic,
    Spice,
    VerilogA,
    Verilog,
    Other,
}

/// How a plan writes a row.
#[derive(Debug)]
pub(crate) enum Form {
    /// The lang's default template.
    Default,
    /// A `format` template.
    Template {
        tmpl: Box<Template>,
        /// Per template identifier (in `tmpl.program().idents()` order): its source.
        srcs: Vec<Src>,
        /// Every hole is a bare identifier with a source: rows may copy texts.
        fast: bool,
    },
    /// The template does not parse: every line using it is this error.
    Bad(String),
}

/// How the instance lines of one impl are written. `slots` and `vector` have one entry
/// per pin of the cell.
#[derive(Debug)]
pub(crate) struct Plan {
    pub(crate) lang: Lang,
    pub(crate) form: Form,
    /// Per pin: `(first bit, bits)` in a row's connections (ordinary pins by elab's
    /// `pin_off`, then inherited pins in pin order after `bits_total`).
    pub(crate) slots: Vec<(u32, u32)>,
    /// The model name for default templates: `model` attr, else the `model` param, else
    /// the cell stem.
    pub(crate) model: Src,
    /// Per pin: written inside an XSPICE port vector (`[ … ]` of the template, or the
    /// default `verilog` template's `[{in_pins}] [{out_pins}]`), spec §7.2.
    pub(crate) vector: Vec<bool>,
}

/// Plans for every leaf impl row (`leaf`, by row) and every cell's schematic impl
/// (`def`, by cell).
#[derive(Debug)]
pub(crate) struct Plans {
    leaf: Vec<Plan>,
    def: Vec<Plan>,
}

/// Whether pin `p` of `f` is an input for `in_pins` (an ordinary `in` pin); every other
/// pin is an `out_pins` one.
pub(crate) fn is_input(f: &Iface, p: usize) -> bool {
    f.pin_dir[p] == PinDir::In && f.pin_inherit[p] == Name::NONE
}

impl Plans {
    /// Builds the plans for dialect `d`. Cost: one template parse per leaf impl row and
    /// per cell with a schematic `format` for `d`.
    pub(crate) fn new(e: &Elab, d: Dialect) -> Plans {
        assert_eq!(e.impls.cell.len(), e.impls.template.len());
        let leaf: Vec<Plan> = (0..e.impls.cell.len())
            .map(|l| plan(e, e.impls.cell[l], e.impls.index[l], e.impls.template[l]))
            .collect();
        let def: Vec<Plan> = (0..e.cells.len() as u32)
            .map(|c| {
                let f = &e.cells[c as usize];
                let Some(k) = f.schematic_impl(&e.names) else {
                    return plan(e, c, u32::MAX, Name::NONE);
                };
                // The first of the dialect's format words the impl has a template for.
                let fmts = f.fmt_start[k] as usize..f.fmt_start[k + 1] as usize;
                let tmpl = d.format_words().iter().find_map(|w| {
                    fmts.clone()
                        .find(|&j| e.names.get(f.fmt_dialect[j]) == *w)
                        .map(|j| f.fmt_template[j])
                });
                plan(e, c, k as u32, tmpl.unwrap_or(Name::NONE))
            })
            .collect();
        assert_eq!(leaf.len(), e.impls.cell.len());
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

// ---- private ----

/// Builds the plan of impl `k` of cell `c` (`k == u32::MAX`: a cell without one).
fn plan(e: &Elab, c: u32, k: u32, template: Name) -> Plan {
    let f = &e.cells[c as usize];
    let names = &e.names;
    let n = f.pin_name.len();
    let lang = match f.impl_lang.get(k as usize).map(|l| names.get(*l)) {
        Some("schematic") | None => Lang::Schematic,
        Some("spice") => Lang::Spice,
        Some("veriloga") => Lang::VerilogA,
        Some("verilog") => Lang::Verilog,
        Some(_) => Lang::Other,
    };
    // Bit slots: ordinary pins where elab put them, inherited pins appended.
    let mut slots = Vec::with_capacity(n);
    let mut inherited = f.bits_total;
    for p in 0..n {
        let w = f.bits(p).len() as u32;
        if f.pin_inherit[p] == Name::NONE {
            slots.push((f.pin_off[p], w));
        } else {
            slots.push((inherited, w));
            inherited += w;
        }
    }
    let model = match (attr(&f.impl_model, k), param(names, f, "model")) {
        (Some(m), _) => Src::Text(m),
        (None, Some(p)) => Src::Param(p),
        (None, None) => Src::Cell,
    };
    let (form, vector) = if template == Name::NONE {
        // The default `verilog` template writes `[{in_pins}] [{out_pins}]`: every pin.
        (Form::Default, vec![lang == Lang::Verilog; n])
    } else {
        match Template::parse(names.get(template)) {
            Err(err) => (Form::Bad(err.to_string()), vec![false; n]),
            Ok(tmpl) => template_form(names, f, k, model, tmpl),
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

/// The plan form and port-vector pins of a parsed `format` template of impl `k` of `f`.
fn template_form(
    names: &Names,
    f: &Iface,
    k: u32,
    model: Src,
    tmpl: Template,
) -> (Form, Vec<bool>) {
    let n = f.pin_name.len();
    let idents: Vec<&str> = tmpl.program().idents().collect();
    // Builtins shadow pins, pins shadow params (spec §7.6.6).
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
            "file" => attr(&f.impl_file, k).map_or(Src::Missing, Src::Text),
            _ => match ((0..n).find(|&p| base(names, f, p) == id), param(names, f, id)) {
                (Some(p), _) => Src::Pin(p as u32),
                (None, Some(q)) => Src::Param(q),
                (None, None) => Src::Missing,
            },
        })
        .collect();
    // Bare-identifier holes written inside `[ … ]` of the literal text (an XSPICE port
    // vector); any other hole is an expression, which has no fast form.
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
            Segment::Hole { ident: Some(i) } if depth > 0 => in_vector.push(idents[i as usize]),
            Segment::Hole { ident: Some(_) } => {}
            Segment::Hole { ident: None } => fast = false,
        }
    }
    let vector: Vec<bool> = (0..n)
        .map(|p| {
            in_vector.iter().any(|&id| match id {
                "pins" => true,
                "in_pins" => is_input(f, p),
                "out_pins" => !is_input(f, p),
                _ => base(names, f, p) == id,
            })
        })
        .collect();
    assert_eq!(srcs.len(), idents.len());
    let form = Form::Template {
        fast: fast && !srcs.contains(&Src::Missing),
        tmpl: Box::new(tmpl),
        srcs,
    };
    (form, vector)
}

/// Impl `k`'s value in the attr column `col`, unless unset.
fn attr(col: &[Name], k: u32) -> Option<Name> {
    col.get(k as usize).copied().filter(|n| *n != Name::NONE)
}

/// The schema index of `f`'s param named `n`.
fn param(names: &Names, f: &Iface, n: &str) -> Option<u32> {
    f.param_name
        .iter()
        .position(|p| names.get(*p) == n)
        .map(|p| p as u32)
}

/// Pin `p`'s base name: a template names a pin by it (`D` for `D[3:0]`).
fn base<'a>(names: &'a Names, f: &Iface, p: usize) -> &'a str {
    names.get(f.pin_name[p]).split('[').next().unwrap_or("")
}
