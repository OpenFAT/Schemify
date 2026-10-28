//! Impl selection and leaf impl rows (netlist spec §2, §7.1, §7.6): a cell and an
//! instance path in, the selected impl, its [`Impls`](super::Impls) row with the dialect's
//! template and `body` lines, and a component's `code` lines out; `E013`, `E031`, `E008`
//! (template errors) and `E033` on the way.
//!
//! Contents:
//! - `Elaborator::{select, leaf_impl, def_body, push_def_code}` (`pub(super)`)

use super::{Dialect, Elaborator};
use crate::names::{Name, Names};
use crate::values::{Funcs, NONE};
use crate::violations::{Code, Violation};
use schemify_doc::ObjId;
use schemify_format::FileKind;

impl<'project, F: Funcs> Elaborator<'project, '_, F> {
    /// Impl selection (spec §2) for the instance at `path` of cell `c`. Reports `E031`
    /// for config names the cell lacks and `E013` when nothing is selectable.
    pub(super) fn select(&mut self, path: &str, c: u32) -> Option<u32> {
        let f = &self.ifaces[c as usize];
        let f_impls = f.impl_name.len();
        let by_name =
            |name: &str, names: &Names| f.impl_name.iter().position(|n| names.get(*n) == name);
        let mut finding = None;
        let mut chosen = None;
        let rule = self.config.inst.iter().position(|r| r.path == path);
        if let Some(r) = rule {
            self.config.inst_hit[r] = true;
        }
        if let Some(imp) = rule.map(|r| &self.config.inst[r].imp) {
            match by_name(imp, &self.names) {
                Some(k) => chosen = Some(k),
                None => finding = Some((imp.clone(), "inst")),
            }
        }
        let cell_rule = self.config.cell.iter().find(|r| r.row as u32 == f.row);
        if let Some(imp) = cell_rule.filter(|_| chosen.is_none()).map(|r| &r.imp) {
            match by_name(imp, &self.names) {
                Some(k) => chosen = Some(k),
                None => finding = finding.or(Some((imp.clone(), "cell"))),
            }
        }
        if chosen.is_none() {
            chosen = self
                .config
                .order
                .iter()
                .find_map(|imp| by_name(imp, &self.names));
        }
        if chosen.is_none() && f.default_impl != NONE {
            chosen = Some(f.default_impl as usize);
        }
        let cell = f.cell;
        if let Some((imp, rule)) = finding {
            let param = self.names.intern(&imp);
            // A `cell` rule is wrong for every path; an `inst` rule for its own.
            let path = if rule == "inst" {
                self.names.intern(path)
            } else {
                Name::NONE
            };
            self.viol.push(Violation {
                path,
                ..Violation::new(
                    Code::E031,
                    cell,
                    ObjId::NONE,
                    param,
                    format!("config `{rule}` rule names impl `{imp}`, which the cell lacks"),
                )
            });
        }
        if chosen.is_none() {
            self.viol.push(Violation::new(
                Code::E013,
                cell,
                ObjId::NONE,
                Name::NONE,
                "the cell has no selectable impl".to_owned(),
            ));
        }
        assert!(chosen.is_none_or(|k| k < f_impls), "the chosen impl is the cell's");
        chosen.map(|k| k as u32)
    }

    /// The leaf impl row for (cell `c`, impl `k`), checking dialect support (`E013`).
    pub(super) fn leaf_impl(&mut self, c: u32, k: u32) -> Option<u32> {
        assert!((k as usize) < self.ifaces[c as usize].impl_name.len(), "impl {k} of cell {c}");
        if let Some(&id) = self.impl_ids.get(&(c, k)) {
            return (id != NONE).then_some(id);
        }
        let f = &self.ifaces[c as usize];
        let d = self.req.dialect;
        let lang = self.names.get(f.impl_lang[k as usize]);
        let fmts = f.fmt_start[k as usize] as usize..f.fmt_start[k as usize + 1] as usize;
        let template = d.format_words().iter().find_map(|w| {
            fmts.clone()
                .find(|&j| self.names.get(f.fmt_dialect[j]) == *w)
                .map(|j| f.fmt_template[j])
        });
        // Spec §7.1 default templates: which langs have one for which dialect.
        let default_ok = matches!(
            (lang, d),
            (
                "spice",
                Dialect::Ngspice
                    | Dialect::Xyce
                    | Dialect::Cdl
                    | Dialect::Espice
                    | Dialect::Hspice
                    | Dialect::Ltspice
            ) | ("veriloga", Dialect::Ngspice | Dialect::Espice)
                | ("verilog", Dialect::Ngspice | Dialect::Verilog)
        );
        if template.is_none() && !default_ok {
            let (cell, imp) = (f.cell, f.impl_name[k as usize]);
            let detail = format!(
                "impl `{}` ({lang}) has no format for {}",
                self.names.get(imp),
                d.as_str()
            );
            self.viol
                .push(Violation::new(Code::E013, cell, ObjId::NONE, imp, detail));
            self.impl_ids.insert((c, k), NONE);
            return None;
        }
        if let Some(t) = template
            && let Some(detail) = self.template_error(c, k, self.names.get(t))
        {
            // The netlister would write `* error in …` for every row: gate it here.
            let (cell, imp) = (f.cell, f.impl_name[k as usize]);
            self.viol
                .push(Violation::new(Code::E008, cell, ObjId::NONE, imp, detail));
        }
        let id = self.impls.cell.len() as u32;
        self.impls.cell.push(c);
        self.impls.index.push(k);
        self.impls.template.push(template.unwrap_or(Name::NONE));
        self.push_body(c, k);
        self.impl_ids.insert((c, k), id);
        assert_eq!(self.impls.template.len(), self.impls.cell.len(), "impl columns in step");
        Some(id)
    }

    /// The [`Impls`] row carrying the body of cell `c`'s schematic impl, `NONE` when no
    /// schematic impl has one (spec §7.6.8). Memoized like leaf rows.
    pub(super) fn def_body(&mut self, c: u32) -> u32 {
        let f = &self.ifaces[c as usize];
        let doc = self.cells[c as usize].doc;
        let (s, bodies) = (doc.strings(), doc.bodies());
        let k = (0..f.impl_name.len()).find(|&k| {
            self.names.get(f.impl_lang[k]) == "schematic"
                && (0..bodies.kind.len())
                    .any(|r| s.get(bodies.fields[r][0]) == self.names.get(f.impl_name[k]))
        });
        let Some(k) = k.map(|k| k as u32) else {
            return NONE;
        };
        if let Some(&id) = self.impl_ids.get(&(c, k)) {
            return id;
        }
        let id = self.impls.cell.len() as u32;
        self.impls.cell.push(c);
        self.impls.index.push(k);
        self.impls.template.push(Name::NONE);
        self.push_body(c, k);
        self.impl_ids.insert((c, k), id);
        assert_eq!(self.impls.index.len(), self.impls.cell.len(), "impl columns in step");
        id
    }

    /// Closes the next [`Defs`] row's range of [`Defs::code`] with the `code` lines of cell
    /// `ci` for the dialect. Only a `.shcomp` contributes (D17 as amended): a bench top's
    /// code is [`Bench`]'s, and a primitive has none.
    pub(super) fn push_def_code(&mut self, ci: u32) {
        assert_eq!(self.defs.code_start.len(), self.defs.cell.len());
        let doc = self.cells[ci as usize].doc;
        if doc.kind() == FileKind::Shcomp {
            let (s, codes) = (doc.strings(), doc.codes());
            let cell = self.ifaces[ci as usize].cell;
            for r in self.code_rows(doc, cell) {
                for &l in &doc.pool().lines[codes.verbatim[r].range()] {
                    let line = self.expand_vars(cell, s.get(l));
                    self.defs.code.push(line);
                }
            }
        }
        self.defs.code_start.push(self.defs.code.len() as u32);
        assert_eq!(self.defs.code_start.len(), self.defs.cell.len() + 1);
    }
}

// ---- private ----

impl<'project, F: Funcs> Elaborator<'project, '_, F> {
    /// Why impl `k` of cell `c` cannot render the template `src`: its parse error, or
    /// the first identifier that no builtin, pin or param provides, resolved as the
    /// netlister resolves them (spec §7.6.6). `None` when every row can render.
    fn template_error(&self, c: u32, k: u32, src: &str) -> Option<String> {
        assert!((k as usize) < self.ifaces[c as usize].impl_name.len(), "impl {k} of cell {c}");
        let tmpl = match schemify_expr::Template::parse(src) {
            Err(err) => return Some(err.to_string()),
            Ok(t) => t,
        };
        let f = &self.ifaces[c as usize];
        let attr = |col: &[Name]| col.get(k as usize).is_some_and(|n| *n != Name::NONE);
        let param = |id: &str| f.param_name.iter().any(|p| self.names.get(*p) == id);
        let pin = |id: &str| {
            f.pin_name
                .iter()
                .any(|p| self.names.get(*p).split('[').next() == Some(id))
        };
        let missing = tmpl.program().idents().find(|&id| match id {
            "name" | "cell" | "pins" | "in_pins" | "out_pins" | "params" => false,
            "model" => !(attr(&f.impl_model) || param(id) || pin(id)),
            "file" => !attr(&f.impl_file),
            _ => !(pin(id) || param(id)),
        })?;
        Some(format!(
            "X004: unknown identifier `{missing}` in the template"
        ))
    }

    /// Appends impl `k`'s `body` lines of cell `c` (rank 5, so only in an open
    /// document) and closes the row's CSR range.
    fn push_body(&mut self, c: u32, k: u32) {
        assert_eq!(self.impls.body_start.len(), self.impls.cell.len());
        let doc = self.cells[c as usize].doc;
        let s = doc.strings();
        let f = &self.ifaces[c as usize];
        let imp_text = self.names.get(f.impl_name[k as usize]).to_owned();
        let bodies = doc.bodies();
        if let Some(r) = (0..bodies.kind.len()).find(|&r| s.get(bodies.fields[r][0]) == imp_text) {
            let cell = self.ifaces[c as usize].cell;
            for &l in &doc.pool().lines[bodies.verbatim[r].range()] {
                let line = self.expand_vars(cell, s.get(l));
                self.impls.body.push(line);
            }
        }
        self.impls.body_start.push(self.impls.body.len() as u32);
        assert_eq!(self.impls.body_start.len(), self.impls.cell.len() + 1);
    }
}
