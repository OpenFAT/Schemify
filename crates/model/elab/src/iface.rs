//! One cell document in, its interface digest out ([`Iface`]): pins with their bit
//! expansion, the param schema with compiled defaults/computed/bounds, the cell's vars,
//! and its impls. Built once per reached cell from whatever `doc` holds for it (the open
//! document, else the indexed ranks 0–1). Cites format spec §4 rank 1 (pins, params,
//! impls), D13 (bounds and the `valid` rule), D20 (`vderived`, `vinputs`), D29
//! (inherited pins, domains).
//!
//! Six questions: in, one `Document`; out, SoA columns. Pins ≤ 2^12, params ≤ 2^8,
//! impls ≤ 2^4, vars ≤ 2^12 per cell (doc capacities). Read per instance during
//! connectivity (pin columns) and per graph node during binding (program refs).
//! Lifetime: one elaboration or one editor cell state.
//!
//! Contents:
//! - `pub enum Domain`, `pub enum ParamKind` (`pub(crate) syntax`)
//! - `pub struct Iface`: `bits`, `param`, `schematic_impl`, `choices_of`,
//!   `pub(crate)` `from_doc`, `bound_konst`
//! - `pub(crate) const MIN`, `MAX`, `STEP`, `GRID`, `VALID`, `BOUNDS`
//! - `pub(crate) fn own_ref(names, params, vars, name) -> Option<Ref>`
//! - `pub(crate) fn cached(t: &Records, r: usize, s: &Strings, doc: &Document) -> Option<Val>`

use crate::names::{Name, Names};
use crate::netexpr;
use crate::nets::Ignore;
use crate::values::{NONE, Progs, Ref, Syntax, VAR, Val};
use schemify_doc::{Document, PinDir, Records, Strings, Sym};
use schemify_format::RecordKind;

/// Signal domain of a pin (format spec §4 rank 1, D29).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Domain {
    /// `analog` (the default).
    Analog,
    /// `digital`.
    Digital,
    /// `real`.
    Real,
}

/// Value kind of a param (format spec §4 rank 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ParamKind {
    /// `number` (default): expression.
    Number,
    /// `int`: expression.
    Int,
    /// `bool`: expression.
    Bool,
    /// `string`: template.
    Str,
    /// `choice`: template, gated by `choices`.
    Choice,
}

/// One cell's interface digest. Pins keep file order (= port order).
#[derive(Clone, Debug, Default)]
pub struct Iface {
    /// Display name (`lib:cell`, or the cell stem for project cells).
    pub cell: Name,
    /// The file stem: the subckt / module base name.
    pub stem: Name,
    /// Project cell row, or `u32::MAX` for an unindexed top document.
    pub row: u32,
    /// Whether the full document was available (schematic, vars, bodies), not only the
    /// indexed interface.
    pub(crate) open: bool,

    /// Pin name (may be a bus).
    pub pin_name: Vec<Name>,
    /// Symbol-local pin x (milli-units).
    pub pin_x: Vec<i32>,
    /// Symbol-local pin y.
    pub pin_y: Vec<i32>,
    /// Direction.
    pub pin_dir: Vec<PinDir>,
    /// Signal domain.
    pub(crate) pin_domain: Vec<Domain>,
    /// Default net of an inherited pin (D29), or [`Name::NONE`] for ordinary pins.
    pub pin_inherit: Vec<Name>,
    /// CSR offsets (`pins + 1`) into [`Iface::pin_bits`].
    pub(crate) pin_bit_start: Vec<u32>,
    /// Bit names of each pin name, in written order.
    pub(crate) pin_bits: Vec<Name>,
    /// Offset of each ordinary pin's first bit among the ordinary pin bits; `u32::MAX`
    /// for inherited pins. An alias pin shares its target's offset.
    pub pin_off: Vec<u32>,
    /// For an alias pin (`alias <pin>`, a second position of an earlier pin: xschem's
    /// same-named pins), the index of that earlier pin; else `u32::MAX`. An alias pin
    /// carries its target's bits, is a geometric node joined to its target's in every
    /// instance, and is no port or connection of its own.
    pub pin_alias: Vec<u32>,
    /// Bits of all ordinary pins (the width of one instance element's connection row).
    pub bits_total: u32,

    /// Param name.
    pub param_name: Vec<Name>,
    /// Param kind.
    pub(crate) param_kind: Vec<ParamKind>,
    /// Whether the param is `computed` (then `param_prog` is the computed expression).
    pub(crate) param_computed: Vec<bool>,
    /// `choices` as a CSR (offsets `params + 1`) into [`Iface::choices`].
    pub(crate) choice_start: Vec<u32>,
    /// Choice texts.
    pub(crate) choices: Vec<Name>,
    /// Program (in [`Iface::progs`]) of the default, or of the computed expression.
    pub(crate) param_prog: Vec<u32>,
    /// Programs of `min max step grid valid`, `u32::MAX` where absent.
    pub(crate) param_bound: Vec<[u32; BOUNDS]>,
    /// Whether the param is `vderived true`: a value only Virtuoso's callbacks compute
    /// (no rule explains it), carried by an instance's `cached` attribute (D20 as amended
    /// 2026-10-15) and stale once the inputs in [`Iface::vinputs`] change.
    pub(crate) param_vderived: Vec<bool>,
    /// The params a `vderived` value depends on (header `vinputs`), in digest order.
    pub(crate) vinputs: Vec<Name>,

    /// Var names (rank 4; present only when the document is open).
    pub(crate) var_name: Vec<Name>,
    /// Program of each var's expression.
    pub(crate) var_prog: Vec<u32>,

    /// Impl name.
    pub impl_name: Vec<Name>,
    /// Impl lang (`schematic`, `spice`, `veriloga`, `verilog`, or a plugin tag).
    pub impl_lang: Vec<Name>,
    /// `model` attr, or [`Name::NONE`].
    pub impl_model: Vec<Name>,
    /// `file` attr, or [`Name::NONE`].
    pub impl_file: Vec<Name>,
    /// CSR offsets (`impls + 1`) into the format columns.
    pub fmt_start: Vec<u32>,
    /// `format <dialect>` word.
    pub fmt_dialect: Vec<Name>,
    /// `format` template text.
    pub fmt_template: Vec<Name>,
    /// `ignore` attr of each impl (format spec §4: same meaning as the instance attr).
    pub(crate) impl_ignore: Vec<Ignore>,
    /// Index of the default impl (`default` header attr, else the only impl), or
    /// `u32::MAX` when the cell has none.
    pub(crate) default_impl: u32,
    /// Bus pin names that expand to more than [`netexpr::WIDTH_MAX`] bits.
    pub(crate) too_wide: Vec<Name>,

    /// Every program above: defaults, computed expressions, bounds, vars.
    pub(crate) progs: Progs,
}

/// Index of a bound in [`Iface::param_bound`].
pub(crate) const MIN: usize = 0;

pub(crate) const MAX: usize = 1;

pub(crate) const STEP: usize = 2;

pub(crate) const GRID: usize = 3;

/// A param's legality rule (`valid <bool expr>`, D13 as amended 2026-10-15).
pub(crate) const VALID: usize = 4;

/// Bounds per param: `min max step grid valid`.
pub(crate) const BOUNDS: usize = 5;

impl ParamKind {
    pub(crate) fn syntax(self) -> Syntax {
        match self {
            ParamKind::Str | ParamKind::Choice => Syntax::Template,
            _ => Syntax::Expr,
        }
    }
}

impl Iface {
    /// Digests `doc`. `cell` is the display name; `stem` the subckt base name; `row` the
    /// project cell row or `u32::MAX`; `open` whether the full document is at hand.
    ///
    /// Cost: one compile per param default, bound and var; O(pins + bits) otherwise.
    pub(crate) fn from_doc(
        doc: &Document,
        cell: &str,
        stem: &str,
        row: u32,
        open: bool,
        names: &mut Names,
    ) -> Iface {
        let mut f = Iface {
            cell: names.intern(cell),
            stem: names.intern(stem),
            row,
            open,
            ..Iface::default()
        };
        // The interning order is part of the output (handles number in it): pins, params
        // and vars, new choice texts, impls, `vinputs`.
        f.read_pins(doc, names);
        f.read_params(doc, names);
        f.read_impls(doc, names);
        f.vinputs = doc
            .header_values("vinputs")
            .into_iter()
            .map(|n| names.intern(n))
            .collect();
        f.default_impl = match doc.header_value("default") {
            Some(d) => f
                .impl_name
                .iter()
                .position(|n| names.get(*n) == d)
                .map_or(NONE, |i| i as u32),
            None if f.impl_name.len() == 1 => 0,
            None => NONE,
        };
        assert_eq!(f.pin_bit_start.len(), f.pin_name.len() + 1);
        assert_eq!(f.param_prog.len(), f.param_name.len());
        assert_eq!(f.fmt_start.len(), f.impl_name.len() + 1);
        f
    }

    /// Returns the bit names of pin `p`.
    pub fn bits(&self, p: usize) -> &[Name] {
        &self.pin_bits[self.pin_bit_start[p] as usize..self.pin_bit_start[p + 1] as usize]
    }

    /// Returns the index of param `name`.
    pub fn param(&self, name: Name) -> Option<usize> {
        self.param_name.iter().position(|n| *n == name)
    }

    /// Returns the index of the schematic impl, if the cell has one.
    pub fn schematic_impl(&self, names: &Names) -> Option<usize> {
        self.impl_lang
            .iter()
            .position(|l| names.get(*l) == "schematic")
    }

    /// The value of bound `w` of param `k` when it reads nothing (the usual `min 0.15`:
    /// evaluated once when compiled, `Progs::konst`); `None` when absent or not constant.
    pub(crate) fn bound_konst(&self, k: usize, w: usize) -> Option<&Val> {
        assert!(w < BOUNDS);
        let b = self.param_bound[k][w];
        (b != NONE).then(|| self.progs.konst[b as usize].as_ref())?
    }

    /// Returns the choices of param `k`.
    pub fn choices_of(&self, k: usize) -> &[Name] {
        &self.choices[self.choice_start[k] as usize..self.choice_start[k + 1] as usize]
    }
}

/// Resolves `name` in a cell's own scope: params first, then vars (expr spec §4).
pub(crate) fn own_ref(names: &Names, params: &[Name], vars: &[Name], name: &str) -> Option<Ref> {
    let n = names.find(name)?;
    if let Some(k) = params.iter().position(|p| *p == n) {
        return Some(k as Ref);
    }
    vars.iter().position(|v| *v == n).map(|v| VAR | v as Ref)
}

/// The D20 `cached` value of record `r` of table `t`, parsed ([`Val::parse_cached`]).
pub(crate) fn cached(t: &Records, r: usize, s: &Strings, doc: &Document) -> Option<Val> {
    let v = attr_vals(t, r, s, doc, "cached")?;
    v.first().map(|v| Val::parse_cached(s.get(*v)))
}

// ---- private ----

impl Iface {
    /// Pin columns, each pin's bits, aliases and the ordinary-pin bit offsets.
    fn read_pins(&mut self, doc: &Document, names: &mut Names) {
        let (s, pool, pins) = (doc.strings(), doc.pool(), doc.pins());
        let f = self;
        let mut buf = String::new();
        f.pin_bit_start.push(0);
        for p in 0..pins.name.len() {
            let text = s.get(pins.name[p]);
            f.pin_name.push(names.intern(text));
            f.pin_x.push(pins.x[p]);
            f.pin_y.push(pins.y[p]);
            f.pin_dir.push(pins.dir[p]);
            let attr = |k| {
                pool.find_attr(s, pins.attrs[p], k)
                    .and_then(|v| v.first())
                    .map(|v| s.get(*v))
            };
            f.pin_domain.push(match attr("domain") {
                Some("digital") => Domain::Digital,
                Some("real") => Domain::Real,
                _ => Domain::Analog,
            });
            let inherit = attr("inherit").map_or(Name::NONE, |n| names.intern(n));
            f.pin_inherit.push(inherit);
            // An alias names an earlier ordinary pin; anything else is an ordinary pin.
            let alias = attr("alias")
                .and_then(|a| names.find(a))
                .and_then(|a| {
                    (0..p).find(|&q| f.pin_name[q] == a && f.pin_inherit[q] == Name::NONE)
                })
                .filter(|_| inherit == Name::NONE);
            f.pin_alias.push(alias.map_or(NONE, |q| q as u32));
            if let Some(q) = alias {
                let (a, b) = (f.pin_bit_start[q] as usize, f.pin_bit_start[q + 1] as usize);
                f.pin_bits.extend_from_within(a..b);
                f.pin_bit_start.push(f.pin_bits.len() as u32);
                f.pin_off.push(f.pin_off[q]);
                continue;
            }
            if netexpr::expand(text, names, &mut f.pin_bits, &mut buf).is_err() {
                f.too_wide.push(f.pin_name[p]);
            }
            f.pin_bit_start.push(f.pin_bits.len() as u32);
            let w = f.pin_bit_start[p + 1] - f.pin_bit_start[p];
            if inherit == Name::NONE {
                f.pin_off.push(f.bits_total);
                f.bits_total += w;
            } else {
                f.pin_off.push(NONE);
            }
        }
        assert_eq!(f.pin_off.len(), f.pin_name.len());
    }

    /// Param and var columns with their compiled programs: names first, so programs can
    /// resolve against both; then the choice texts the programs did not intern.
    fn read_params(&mut self, doc: &Document, names: &mut Names) {
        let s = doc.strings();
        let (params, blocks) = (doc.params(), doc.doc_blocks());
        let f = self;
        let vars: Vec<usize> = (0..blocks.kind.len())
            .filter(|&r| blocks.kind[r] == RecordKind::Var)
            .collect();
        for r in 0..params.kind.len() {
            f.param_name.push(names.intern(s.get(params.fields[r][0])));
        }
        for &r in &vars {
            f.var_name.push(names.intern(s.get(blocks.fields[r][0])));
        }
        let scope = |name: &str| own_ref(names, &f.param_name, &f.var_name, name);
        let mut progs = Progs::default();
        f.choice_start.push(0);
        for r in 0..params.kind.len() {
            let a = |k: &str| attr_vals(params, r, s, doc, k);
            let kind = match a("kind").and_then(|v| v.first().copied()).map(|v| s.get(v)) {
                Some("int") => ParamKind::Int,
                Some("bool") => ParamKind::Bool,
                Some("string") => ParamKind::Str,
                Some("choice") => ParamKind::Choice,
                _ => ParamKind::Number,
            };
            let computed = a("computed").and_then(|v| v.first().copied());
            let text = computed.unwrap_or(params.fields[r][1]);
            let syntax = if computed.is_some() {
                Syntax::Expr
            } else {
                kind.syntax()
            };
            f.param_kind.push(kind);
            f.param_computed.push(computed.is_some());
            let vderived = a("vderived").and_then(|v| v.first().copied());
            f.param_vderived
                .push(vderived.is_some_and(|v| s.get(v) == "true"));
            let prog = progs.push(s.get(text), syntax, cached(params, r, s, doc), scope);
            f.param_prog.push(prog);
            let mut bound = [NONE; BOUNDS];
            for (slot, key) in [
                (MIN, "min"),
                (MAX, "max"),
                (STEP, "step"),
                (GRID, "grid"),
                (VALID, "valid"),
            ] {
                if let Some(v) = a(key).and_then(|v| v.first().copied()) {
                    bound[slot] = progs.push(s.get(v), Syntax::Expr, None, scope);
                }
            }
            f.param_bound.push(bound);
            // `scope` borrows `names`: choices are looked up now, the new ones interned
            // below.
            for &c in a("choices").unwrap_or_default() {
                f.choices.push(names.find(s.get(c)).unwrap_or(Name::NONE));
            }
            f.choice_start.push(f.choices.len() as u32);
        }
        for &r in &vars {
            let c = cached(blocks, r, s, doc);
            f.var_prog
                .push(progs.push(s.get(blocks.fields[r][1]), Syntax::Expr, c, scope));
        }
        f.progs = progs;
        let mut k = 0;
        for r in 0..params.kind.len() {
            for &c in attr_vals(params, r, s, doc, "choices").unwrap_or_default() {
                if f.choices[k] == Name::NONE {
                    f.choices[k] = names.intern(s.get(c));
                }
                k += 1;
            }
        }
        assert_eq!(k, f.choices.len());
        assert_eq!(f.var_prog.len(), f.var_name.len());
    }

    /// Impl columns and their `format` lines.
    fn read_impls(&mut self, doc: &Document, names: &mut Names) {
        let (s, pool, impls) = (doc.strings(), doc.pool(), doc.impls());
        let f = self;
        f.fmt_start.push(0);
        for r in 0..impls.kind.len() {
            f.impl_name.push(names.intern(s.get(impls.fields[r][0])));
            f.impl_lang.push(names.intern(s.get(impls.fields[r][1])));
            let one = |k| attr_vals(impls, r, s, doc, k).and_then(|v| v.first().copied());
            f.impl_model
                .push(one("model").map_or(Name::NONE, |v| names.intern(s.get(v))));
            f.impl_file
                .push(one("file").map_or(Name::NONE, |v| names.intern(s.get(v))));
            f.impl_ignore
                .push(Ignore::parse(one("ignore").map(|v| s.get(v))));
            for (key, vals) in pool.attrs(impls.attrs[r]) {
                if s.get(key) == "format" && vals.len() == 2 {
                    f.fmt_dialect.push(names.intern(s.get(vals[0])));
                    f.fmt_template.push(names.intern(s.get(vals[1])));
                }
            }
            f.fmt_start.push(f.fmt_dialect.len() as u32);
        }
        assert_eq!(f.impl_ignore.len(), f.impl_name.len());
    }
}

fn attr_vals<'d>(
    t: &Records,
    r: usize,
    s: &Strings,
    doc: &'d Document,
    key: &str,
) -> Option<&'d [Sym]> {
    doc.pool().find_attr(s, t.attrs[r], key)
}
