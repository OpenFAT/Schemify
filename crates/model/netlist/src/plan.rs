//! Template plans (netlist spec §7.1, §7.6.6–7, expr spec §6): elab's leaf impls and
//! cells in, one [`Plan`] per leaf impl row and per cell's schematic impl out.
//!
//! A plan resolves each template identifier once to where its value lives ([`Source`]),
//! so a row binds by index, not by name. Templates whose holes are all bare identifiers
//! (the usual `M{name} {D} {G} {S} {B} {model} W={W}`) are marked `fast`: rows copy the
//! canonical texts elab already wrote instead of evaluating (`inst.rs`).
//!
//! Contents:
//! - `pub(crate) enum Source`, `pub(crate) enum Lang`, `pub(crate) enum Form`
//! - `pub(crate) struct Slot`, `Slot::bits(self) -> Range<usize>`
//! - `pub(crate) struct Plan`, `pub(crate) struct Plans`
//! - `pub(crate) fn is_input(iface: &Iface, pin: usize) -> bool`
//! - `Plans::new(elab: &Elab, dialect: Dialect) -> Plans`,
//!   `Plans::of_row(&self, elab: &Elab, row: usize) -> &Plan`

use schemify_elab::{Dialect, Elab, Iface, Name, Names, PinDir, Target};
use schemify_expr::{Segment, Template};
use std::ops::Range;

/// Where a template identifier's value comes from (expr spec §6, spec §7.6.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
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
    /// See [`Source::InPins`].
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
        template: Box<Template>,
        /// Per template identifier (in `template.program().idents()` order): its source.
        sources: Vec<Source>,
        /// Every hole is a bare identifier with a source: rows may copy texts.
        fast: bool,
    },
    /// The template does not parse: every line using it is this error.
    Bad(String),
}

/// Where one pin's bits sit in a row's connections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Slot {
    /// The first bit: elab's `pin_off` for an ordinary pin, after `bits_total` for an
    /// inherited one.
    pub(crate) first: u32,
    /// The pin's width in bits.
    pub(crate) width: u32,
}

/// How the instance lines of one impl are written. `slots` and `vector` have one entry
/// per pin of the cell.
#[derive(Debug)]
pub(crate) struct Plan {
    pub(crate) lang: Lang,
    pub(crate) form: Form,
    /// Per pin: its bits (ordinary pins by elab's `pin_off`, then inherited pins in pin
    /// order after `bits_total`).
    pub(crate) slots: Vec<Slot>,
    /// The model name for default templates: `model` attr, else the `model` param, else
    /// the cell stem.
    pub(crate) model: Source,
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

/// Whether pin `pin` of `iface` is an input for `in_pins` (an ordinary `in` pin); every
/// other pin is an `out_pins` one.
pub(crate) fn is_input(iface: &Iface, pin: usize) -> bool {
    debug_assert_eq!(iface.pin_dir.len(), iface.pin_inherit.len());
    iface.pin_dir[pin] == PinDir::In && iface.pin_inherit[pin] == Name::NONE
}

impl Slot {
    /// The slot's bit range in a row's connections.
    pub(crate) fn bits(self) -> Range<usize> {
        self.first as usize..(self.first + self.width) as usize
    }
}

impl Plans {
    /// Builds the plans for `dialect`. Cost: one template parse per leaf impl row and
    /// per cell with a schematic `format` for `dialect`.
    pub(crate) fn new(elab: &Elab, dialect: Dialect) -> Plans {
        assert_eq!(elab.impls.cell.len(), elab.impls.template.len());
        assert_eq!(elab.impls.cell.len(), elab.impls.index.len());
        let leaf: Vec<Plan> = (0..elab.impls.cell.len())
            .map(|l| {
                let impls = &elab.impls;
                plan(elab, impls.cell[l], impls.index[l], impls.template[l])
            })
            .collect();
        let mut def: Vec<Plan> = Vec::with_capacity(elab.cells.len());
        for cell in 0..elab.cells.len() as u32 {
            let iface = &elab.cells[cell as usize];
            let Some(k) = iface.schematic_impl(&elab.names) else {
                def.push(plan(elab, cell, u32::MAX, Name::NONE));
                continue;
            };
            // The first of the dialect's format words the impl has a template for.
            let formats = iface.fmt_start[k] as usize..iface.fmt_start[k + 1] as usize;
            let template = dialect.format_words().iter().find_map(|word| {
                formats
                    .clone()
                    .find(|&j| elab.names.get(iface.fmt_dialect[j]) == *word)
                    .map(|j| iface.fmt_template[j])
            });
            def.push(plan(elab, cell, k as u32, template.unwrap_or(Name::NONE)));
        }
        assert_eq!(leaf.len(), elab.impls.cell.len());
        assert_eq!(def.len(), elab.cells.len());
        Plans { leaf, def }
    }

    /// The plan that writes instance row `row`.
    pub(crate) fn of_row(&self, elab: &Elab, row: usize) -> &Plan {
        let plan = match elab.insts.target[row] {
            Target::Def(def) => {
                let plan = &self.def[elab.defs.cell[def as usize] as usize];
                // `Plans::new` plans a cell's schematic impl, or none, for its definitions.
                debug_assert_eq!(plan.lang, Lang::Schematic);
                plan
            }
            Target::Leaf(leaf) => &self.leaf[leaf as usize],
        };
        debug_assert_eq!(plan.slots.len(), plan.vector.len(), "one entry per pin");
        plan
    }
}

// ---- private ----

/// Builds the plan of impl `impl_index` of cell `cell` (`u32::MAX`: a cell without one).
fn plan(elab: &Elab, cell: u32, impl_index: u32, template: Name) -> Plan {
    assert!(
        (cell as usize) < elab.cells.len(),
        "cell {cell} is a cell row"
    );
    let iface = &elab.cells[cell as usize];
    let names = &elab.names;
    let pins = iface.pin_name.len();
    assert!(
        impl_index == u32::MAX || (impl_index as usize) < iface.impl_lang.len(),
        "impl {impl_index} of cell {cell}"
    );
    assert_eq!(iface.pin_off.len(), pins);
    assert_eq!(iface.pin_inherit.len(), pins);
    let lang = match iface
        .impl_lang
        .get(impl_index as usize)
        .map(|l| names.get(*l))
    {
        Some("schematic") | None => Lang::Schematic,
        Some("spice") => Lang::Spice,
        Some("veriloga") => Lang::VerilogA,
        Some("verilog") => Lang::Verilog,
        Some(_) => Lang::Other,
    };
    // Bit slots: ordinary pins where elab put them, inherited pins appended.
    let mut slots = Vec::with_capacity(pins);
    let mut inherited = iface.bits_total;
    for p in 0..pins {
        let width = iface.bits(p).len() as u32;
        if iface.pin_inherit[p] == Name::NONE {
            let slot = Slot {
                first: iface.pin_off[p],
                width,
            };
            assert!(
                slot.first + width <= iface.bits_total,
                "an ordinary pin's bits lie within the ordinary bits"
            );
            slots.push(slot);
        } else {
            slots.push(Slot {
                first: inherited,
                width,
            });
            inherited += width;
        }
    }
    let model = match (
        attr(&iface.impl_model, impl_index),
        param(names, iface, "model"),
    ) {
        (Some(m), _) => Source::Text(m),
        (None, Some(p)) => Source::Param(p),
        (None, None) => Source::Cell,
    };
    let (form, vector) = if template == Name::NONE {
        // The default `verilog` template writes `[{in_pins}] [{out_pins}]`: every pin.
        (Form::Default, vec![lang == Lang::Verilog; pins])
    } else {
        match Template::parse(names.get(template)) {
            Err(err) => (Form::Bad(err.to_string()), vec![false; pins]),
            Ok(parsed) => template_form(names, iface, impl_index, model, parsed),
        }
    };
    assert_eq!(slots.len(), pins);
    assert_eq!(vector.len(), pins);
    Plan {
        lang,
        form,
        slots,
        model,
        vector,
    }
}

/// The plan form and port-vector pins of a parsed `format` template of impl
/// `impl_index` of `iface`.
fn template_form(
    names: &Names,
    iface: &Iface,
    impl_index: u32,
    model: Source,
    template: Template,
) -> (Form, Vec<bool>) {
    let pins = iface.pin_name.len();
    let idents: Vec<&str> = template.program().idents().collect();
    // Builtins shadow pins, pins shadow params (spec §7.6.6).
    let sources: Vec<Source> = idents
        .iter()
        .map(|&id| match id {
            "name" => Source::Name,
            "cell" => Source::Cell,
            "pins" => Source::Pins,
            "in_pins" => Source::InPins,
            "out_pins" => Source::OutPins,
            "params" => Source::Params,
            "model" if model != Source::Cell => model,
            "file" => attr(&iface.impl_file, impl_index).map_or(Source::Missing, Source::Text),
            _ => match (
                (0..pins).find(|&p| base(names, iface, p) == id),
                param(names, iface, id),
            ) {
                (Some(p), _) => Source::Pin(p as u32),
                (None, Some(q)) => Source::Param(q),
                (None, None) => Source::Missing,
            },
        })
        .collect();
    // Bare-identifier holes written inside `[ … ]` of the literal text (an XSPICE port
    // vector); any other hole is an expression, which has no fast form.
    let (mut in_vector, mut depth, mut fast) = (Vec::new(), 0u32, true);
    for segment in template.segments() {
        match segment {
            Segment::Lit(s) => {
                for c in s.bytes() {
                    match c {
                        b'[' => depth += 1,
                        b']' => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                }
            }
            Segment::Hole { ident: Some(i) } => {
                assert!(
                    (i as usize) < idents.len(),
                    "a hole names one of the program's identifiers"
                );
                if depth > 0 {
                    in_vector.push(idents[i as usize]);
                }
            }
            Segment::Hole { ident: None } => fast = false,
        }
    }
    let vector: Vec<bool> = (0..pins)
        .map(|p| {
            in_vector.iter().any(|&id| match id {
                "pins" => true,
                "in_pins" => is_input(iface, p),
                "out_pins" => !is_input(iface, p),
                _ => base(names, iface, p) == id,
            })
        })
        .collect();
    assert_eq!(sources.len(), idents.len());
    assert_eq!(vector.len(), pins);
    let form = Form::Template {
        fast: fast && !sources.contains(&Source::Missing),
        template: Box::new(template),
        sources,
    };
    (form, vector)
}

/// Impl `impl_index`'s value in the attr column `column`, unless unset.
fn attr(column: &[Name], impl_index: u32) -> Option<Name> {
    column
        .get(impl_index as usize)
        .copied()
        .filter(|n| *n != Name::NONE)
}

/// The schema index of `iface`'s param named `name`.
fn param(names: &Names, iface: &Iface, name: &str) -> Option<u32> {
    iface
        .param_name
        .iter()
        .position(|p| names.get(*p) == name)
        .map(|p| p as u32)
}

/// Pin `pin`'s base name: a template names a pin by it (`D` for `D[3:0]`).
fn base<'names>(names: &'names Names, iface: &Iface, pin: usize) -> &'names str {
    names
        .get(iface.pin_name[pin])
        .split('[')
        .next()
        .unwrap_or("")
}
