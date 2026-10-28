//! One definition's instance rows (netlist spec §2, §5, §6): a cell and its own-param
//! values in, every element row with its target (a child definition, built depth first,
//! or a leaf impl), connections (inherited pins included, `E011`, `E036`) and canonical
//! param texts out, handed to `defs` to share or append. `E014` bounds the recursion.
//!
//! Contents:
//! - `Elaborator::build_def` (`pub(super)`)

use super::{
    COMPUTED, DEFAULT, DEPTH_MAX, DefRows, Elaborator, InheritedPin, OVERRIDDEN, ParamEntry,
    RowTmp, Target, defs,
};
use crate::graph::OWN;
use crate::names::{Name, Names};
use crate::netexpr;
use crate::nets::{Ignore, NO_NET};
use crate::values::{Funcs, NONE, Val};
use crate::violations::{Code, Violation};
use schemify_doc::ObjId;
use std::ops::Range;

/// Canonical text of recent scalar values, (tag, bits) → name, in 64 hashed slots: a flat
/// design repeats a few values a million times; formatting and interning each was a tenth
/// of it.
struct ValTexts {
    slots: [(u8, u64, Name); 64],
    text: String,
}

impl<'project, F: Funcs> Elaborator<'project, '_, F> {
    /// Builds (or reuses) the definition of cell `ci` for own-param values `tuple` at
    /// instance path `path`. `None` when the cell cannot be elaborated here.
    pub(super) fn build_def(
        &mut self,
        ci: u32,
        own_vals: Vec<Val>,
        path: String,
        depth: u32,
    ) -> Option<u32> {
        assert!(
            self.stack.len() as u32 == depth,
            "one stack entry per level"
        );
        assert_eq!(
            own_vals.len(),
            self.ifaces[ci as usize].param_name.len(),
            "one value per own param"
        );
        let tuple = defs::tuple(&self.ifaces[ci as usize], &own_vals, &mut self.names);
        let sensitive = self.config.inst.iter().any(|rule| {
            let p = &rule.path;
            p.len() > path.len() && p.starts_with(&path) && p.as_bytes()[path.len()] == b'/'
        });
        let known = self.memo.get(&(ci, tuple.key.clone())).filter(|_| !sensitive);
        if let Some(&d) = known {
            return Some(d);
        }
        if depth >= DEPTH_MAX {
            let p = self.names.intern(&path);
            self.viol.push(Violation {
                path: p,
                ..Violation::new(
                    Code::E014,
                    self.ifaces[ci as usize].cell,
                    ObjId::NONE,
                    Name::NONE,
                    format!("hierarchy deeper than {DEPTH_MAX}"),
                )
            });
            return None;
        }
        let (vals, _unverified) = self.prepare_eval(ci, &own_vals);
        self.stack.push(ci);
        let doc = self.cells[ci as usize].doc;
        let ni = doc.instances().id.len();
        let is_top = depth == 0;
        let mut out = DefRows {
            rows: Vec::with_capacity(ni),
            conn: Vec::with_capacity(
                self.nets[ci as usize]
                    .as_ref()
                    .expect("prepared")
                    .conns
                    .len(),
            ),
            params: Vec::with_capacity(vals.len()),
            propagated: Vec::new(),
        };
        let mut texts = ValTexts {
            slots: [(0, 0, Name::NONE); 64],
            text: String::new(),
        };
        let mut child_path = String::new();
        // Per child cell, memoized for the run of instances of one cell: whether rows need
        // their path, and the last (impl → leaf row) (`leaf_impl` memoizes too, by hash).
        let mut pathed_of = (NONE, false);
        let mut leaf_of = (NONE, NONE, None);
        for i in 0..ni {
            let c = self.cells[ci as usize].inst_iface[i];
            if c == NONE {
                continue;
            }
            let first = self.cells[ci as usize].graph.inst_first[i] as usize;
            let pc = self.ifaces[c as usize].param_name.len();
            if vals[first..first + pc].iter().any(Val::is_err) {
                continue; // E008 reported; nothing sound to instantiate
            }
            if self.cells[c as usize].defaults.is_none() {
                self.defaults(c); // computed once per cell, then borrowed below
            }
            let params = self.push_params(ci, c, first, &vals, &mut texts, &mut out.params);
            let nets = self.nets[ci as usize].as_ref().expect("prepared");
            let (e0, e1) = (nets.elem_start[i] as usize, nets.elem_start[i + 1] as usize);
            let ignore = nets.ignore[i];
            // The child path is read by config `inst` rules, by schematic children (their
            // own paths, E014) and by inherited pins (E036); a plain leaf row never needs it.
            let f = &self.ifaces[c as usize];
            if pathed_of.0 != c {
                let pathed = !self.config.inst.is_empty()
                    || f.pin_inherit.iter().any(|p| *p != Name::NONE)
                    || f.impl_lang
                        .iter()
                        .any(|l| self.names.get(*l) == "schematic");
                pathed_of = (c, pathed);
            }
            let pathed = pathed_of.1;
            for k in 0..e1 - e0 {
                let elem = self.nets[ci as usize].as_ref().expect("prepared").elem_name[e0 + k];
                child_path.clear();
                if pathed {
                    child_path.push_str(&path);
                    child_path.push('/');
                    child_path.push_str(self.names.get(elem));
                }
                let Some(imp) = self.select(&child_path, c) else {
                    continue;
                };
                let lang = self
                    .names
                    .get(self.ifaces[c as usize].impl_lang[imp as usize]);
                let target = if lang == "schematic" {
                    if !self.ifaces[c as usize].open {
                        self.needs_open.push(self.ifaces[c as usize].row);
                        continue;
                    }
                    if self.stack.contains(&c) {
                        let p = self.names.intern(&child_path);
                        let detail = format!(
                            "{} instantiates itself",
                            self.names.get(self.ifaces[c as usize].cell)
                        );
                        self.viol.push(Violation {
                            path: p,
                            ..Violation::new(
                                Code::E014,
                                self.ifaces[ci as usize].cell,
                                doc.instances().id[i],
                                Name::NONE,
                                detail,
                            )
                        });
                        continue;
                    }
                    let child_tuple = vals[first..first + pc].to_vec();
                    match self.build_def(c, child_tuple, child_path.clone(), depth + 1) {
                        Some(d) => Target::Def(d),
                        None => continue,
                    }
                } else {
                    if !self.ifaces[c as usize].open && self.ifaces[c as usize].row != NONE {
                        self.needs_open.push(self.ifaces[c as usize].row);
                    }
                    if (leaf_of.0, leaf_of.1) != (c, imp) {
                        leaf_of = (c, imp, self.leaf_impl(c, imp));
                    }
                    match leaf_of.2 {
                        Some(id) => Target::Leaf(id),
                        None => continue,
                    }
                };
                let c0 = out.conn.len();
                self.instance_conns(ci, i, k, target, &child_path, &mut out);
                // The instance's own `ignore` wins; else the selected impl's.
                let ignore = match ignore {
                    Ignore::No => self.ifaces[c as usize].impl_ignore[imp as usize],
                    own => own,
                };
                if ignore == Ignore::Open {
                    out.conn[c0..].fill(Name::NONE);
                }
                out.rows.push(RowTmp {
                    name: elem,
                    prefix: name_prefix(self.names.get(elem)),
                    id: doc.instances().id[i],
                    target,
                    ignore,
                    conn: c0 as u32..out.conn.len() as u32,
                    params: params.clone(),
                });
            }
        }
        assert_eq!(self.stack.pop(), Some(ci), "the stack unwinds in order");
        let d = self.finish_def(ci, tuple, sensitive, out, is_top);
        assert!((d as usize) < self.defs.cell.len(), "finish_def returns a definition");
        Some(d)
    }
}

// ---- private ----

impl<'project, F: Funcs> Elaborator<'project, '_, F> {
    /// Appends the param entries of instance row `i`'s values (`vals[first..]`, child cell
    /// `c`, in cell `ci`) with their canonical text and flags; returns their range.
    fn push_params(
        &mut self,
        ci: u32,
        c: u32,
        first: usize,
        vals: &[Val],
        texts: &mut ValTexts,
        out: &mut Vec<ParamEntry>,
    ) -> Range<u32> {
        let p0 = out.len() as u32;
        let f = &self.ifaces[c as usize];
        let defaults = self.cells[c as usize].defaults.as_ref().expect("computed");
        let table = &self.cells[ci as usize].graph.table;
        assert!(first + f.param_name.len() <= vals.len());
        for k in 0..f.param_name.len() {
            let v = &vals[first + k];
            let t = texts.get(v, &mut self.names);
            let mut flags = 0;
            if f.param_computed[k] {
                flags |= COMPUTED;
            }
            if defaults[k] == *v {
                flags |= DEFAULT;
            }
            if table[first + k] == OWN {
                flags |= OVERRIDDEN;
            }
            out.push(ParamEntry {
                name: f.param_name[k],
                text: t,
                value: v.clone(),
                flags,
            });
        }
        assert_eq!(out.len() - p0 as usize, f.param_name.len(), "one entry per param");
        p0..out.len() as u32
    }

    /// Appends to `out.conn` the connection names of element `k` of instance row `i` in
    /// cell `ci`: ordinary pin bits, then the target's inherited pins (spec §5): a local
    /// `inherit` override, else (top) the default net, else the definition's own
    /// inherited port (recorded in `out.propagated`).
    fn instance_conns(
        &mut self,
        ci: u32,
        i: usize,
        k: usize,
        target: Target,
        path: &str,
        out: &mut DefRows,
    ) {
        // `build_def` pushed `ci`: one entry means `ci` is the top.
        assert_eq!(self.stack.last(), Some(&ci));
        let is_top = self.stack.len() == 1;
        let (conn, propagated) = (&mut out.conn, &mut out.propagated);
        let c = self.cells[ci as usize].inst_iface[i];
        assert!(c != NONE, "only resolved instances have rows");
        let nets = self.nets[ci as usize].as_ref().expect("prepared");
        let bits = self.ifaces[c as usize].bits_total as usize;
        let start = nets.conn_start[i] as usize + k * bits;
        conn.extend(nets.conns[start..start + bits].iter().map(|&b| {
            if b == NO_NET {
                Name::NONE
            } else {
                nets.bit_name[b as usize]
            }
        }));
        let none = match target {
            Target::Leaf(_) => self.ifaces[c as usize]
                .pin_inherit
                .iter()
                .all(|p| *p == Name::NONE),
            Target::Def(d) => {
                self.defs.inh_start[d as usize] == self.defs.inh_start[d as usize + 1]
            }
        };
        if none {
            return; // no inherited pins (the common case): nothing more to resolve
        }
        let inherited: Vec<InheritedPin> = match target {
            Target::Leaf(_) => {
                let f = &self.ifaces[c as usize];
                (0..f.pin_name.len())
                    .filter(|&p| f.pin_inherit[p] != Name::NONE)
                    .map(|p| InheritedPin {
                        pin: f.pin_name[p],
                        default: f.pin_inherit[p],
                    })
                    .collect()
            }
            Target::Def(d) => {
                let r = self.defs.inh_start[d as usize] as usize
                    ..self.defs.inh_start[d as usize + 1] as usize;
                r.map(|j| InheritedPin {
                    pin: self.defs.inh_pin[j],
                    default: self.defs.inh_default[j],
                })
                .collect()
            }
        };
        let mut buf = String::new();
        let mut tmp = Vec::new();
        for InheritedPin { pin, default } in inherited {
            let nets = self.nets[ci as usize].as_ref().expect("prepared");
            let local = (0..nets.inh_inst.len())
                .find(|&j| nets.inh_inst[j] == i as u32 && nets.inh_pin[j] == pin);
            if let Some(j) = local {
                let r = nets.inh_bit_start[j] as usize..nets.inh_bit_start[j + 1] as usize;
                let bits: Vec<Name> = nets.inh_bits[r]
                    .iter()
                    .map(|&b| nets.bit_name[b as usize])
                    .collect();
                tmp.clear();
                let text = self.names.get(pin).to_owned();
                let _ = netexpr::expand(&text, &mut self.names, &mut tmp, &mut buf);
                if tmp.len() != bits.len() {
                    let (cell, id) = (
                        self.ifaces[ci as usize].cell,
                        self.cells[ci as usize].doc.instances().id[i],
                    );
                    let detail = format!(
                        "inherit `{text}` ({} bits) set to {} bits",
                        tmp.len(),
                        bits.len()
                    );
                    self.viol
                        .push(Violation::new(Code::E011, cell, id, pin, detail));
                }
                conn.extend(bits);
                continue;
            }
            let via = if is_top {
                default
            } else {
                match propagated.iter().find(|p| p.pin == pin) {
                    Some(&InheritedPin { default: d, .. }) if d != default => {
                        let (cell, p) = (self.ifaces[ci as usize].cell, self.names.intern(path));
                        let detail = format!(
                            "inherited pin `{}` defaults to `{}` and `{}`",
                            self.names.get(pin),
                            self.names.get(d),
                            self.names.get(default)
                        );
                        self.viol.push(Violation {
                            path: p,
                            ..Violation::new(Code::E036, cell, ObjId::NONE, pin, detail)
                        });
                    }
                    Some(_) => {}
                    None => propagated.push(InheritedPin { pin, default }),
                }
                pin
            };
            tmp.clear();
            let text = self.names.get(via).to_owned();
            let _ = netexpr::expand(&text, &mut self.names, &mut tmp, &mut buf);
            let nets = self.nets[ci as usize].as_ref().expect("prepared");
            conn.extend(
                tmp.iter()
                    .map(|&b| nets.bit(b).map_or(b, |n| nets.bit_name[n as usize])),
            );
        }
    }
}

/// The first 8 bytes of `text` as a big-endian number (zero-padded).
fn name_prefix(text: &str) -> u64 {
    let mut b = [0u8; 8];
    let n = text.len().min(8);
    b[..n].copy_from_slice(&text.as_bytes()[..n]);
    let prefix = u64::from_be_bytes(b);
    // Big-endian keeps the text order: the first byte is the most significant.
    debug_assert_eq!(prefix >> 56, u64::from(text.bytes().next().unwrap_or(0)));
    prefix
}

impl ValTexts {
    /// The interned canonical text of `v`.
    fn get(&mut self, v: &Val, names: &mut Names) -> Name {
        let key = match v {
            Val::Int(x) => (1, *x as u64),
            Val::Num(x) => (2, x.to_bits()),
            Val::Bool(b) => (3, u64::from(*b)),
            _ => (0, 0),
        };
        let slot = (key.1.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 58) as usize;
        let hit = &self.slots[slot];
        let t = if key.0 != 0 && hit.0 == key.0 && hit.1 == key.1 {
            hit.2
        } else {
            self.text.clear();
            v.write_text(&mut self.text);
            let t = names.intern(&self.text);
            if key.0 != 0 {
                self.slots[slot] = (key.0, key.1, t);
            }
            t
        };
        debug_assert!(
            names.get(t) == {
                self.text.clear();
                v.write_text(&mut self.text);
                self.text.as_str()
            }
        );
        t
    }
}
