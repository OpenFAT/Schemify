//! The bench top in, its header data ([`Bench`]) out (netlist spec §7.1, run spec §5):
//! bench vars after corner `set` and overrides, corner lines with `${NAME}` expanded
//! (`E034`), evaluated `temp`, and the dialect's code blocks (`E033` for `.control` where
//! nothing runs it). Nothing is executed: `${NAME}` reads the project's `set` entries and
//! the request's environment only.
//!
//! Contents:
//! - `Elaborator::{bench_lines, code_rows, expand_vars}` (`pub(super)`)

use super::{Bench, CornerLine, Dialect, Elaborator};
use crate::names::{Name, Names};
use crate::values::{Funcs, Val};
use crate::violations::{Code, Violation};
use schemify_doc::{Document, ObjId};
use schemify_expr::{Mode, Program, Scratch, Value};

/// Bound on nested `${NAME}` expansion; deeper (or cyclic) definitions are `E034`.
const EXPAND_DEPTH_MAX: u8 = 16;

impl<'project, F: Funcs> Elaborator<'project, '_, F> {
    /// The header data of bench `doc` (the top, cell 0) under corner row `corner`.
    pub(super) fn bench_lines(&mut self, doc: &Document, corner: Option<usize>) -> Bench {
        assert!(!self.cells.is_empty(), "the top cell is prepared first");
        assert!(
            corner.is_none_or(|r| r < doc.corners().attrs.len()),
            "corner row in range"
        );
        let mut b = Bench::default();
        let s = doc.strings();
        // Vars: the top graph's own nodes in the default context.
        let g = &self.cells[0].graph;
        let (mut vals, mut unv) = (Vec::new(), Vec::new());
        g.eval_own(&mut vals, &mut unv, &self.ifaces, self.funcs, &mut self.ws);
        let mut vars: Vec<(Name, Name)> = Vec::new();
        for v in 0..g.vars as usize {
            let val = &vals[g.params as usize + v];
            if val.is_err() {
                continue;
            }
            let mut text = String::new();
            val.write_text(&mut text);
            vars.push((g.var_name[v], self.names.intern(&text)));
        }
        let mut order: Vec<(Name, u32)> = (0..vars.len()).map(|i| (vars[i].0, i as u32)).collect();
        self.names.sort_by_text(&mut order);
        b.var_name = order.iter().map(|o| o.0).collect();
        b.var_text = order.iter().map(|o| vars[o.1 as usize].1).collect();
        // Corner lines in file order.
        b.corner_start.push(0);
        if let Some(r) = corner {
            for (k, v) in doc.pool().attrs(doc.corners().attrs[r]) {
                let kind = match s.get(k) {
                    "lib" => CornerLine::Lib,
                    "include" => CornerLine::Include,
                    "temp" => CornerLine::Temp,
                    _ => continue,
                };
                if kind == CornerLine::Temp {
                    let text = s.get(v[0]);
                    let value = eval_in_bench(text, &b, &self.names);
                    match value {
                        Ok(t) => b.corner_args.push(self.names.intern(&t)),
                        Err(e) => {
                            self.viol.push(Violation::new(
                                Code::E008,
                                self.ifaces[0].cell,
                                ObjId::NONE,
                                self.names.intern("temp"),
                                format!("{}: {}", e.code.as_str(), e.message),
                            ));
                            continue;
                        }
                    }
                    b.corner_project.push(false);
                } else {
                    // The path expands `${NAME}`; a `lib` section word is literal. A path that
                    // starts with a project variable is the project's (run spec §5).
                    let mut from_project = false;
                    for (j, &x) in v.iter().enumerate() {
                        let n = if j == 0 {
                            let raw = s.get(x);
                            // The leading `${NAME}` is one of the project's `set` entries
                            // (not an environment variable).
                            from_project = raw
                                .strip_prefix("${")
                                .and_then(|r| r.split_once('}'))
                                .is_some_and(|(name, _)| {
                                    self.project.settings().iter().any(|(k, _)| k == name)
                                });
                            self.expand_vars(self.ifaces[0].cell, raw)
                        } else {
                            self.names.intern(s.get(x))
                        };
                        b.corner_args.push(n);
                    }
                    b.corner_project.push(from_project);
                }
                b.corner_kind.push(kind);
                b.corner_start.push(b.corner_args.len() as u32);
            }
        }
        // Code blocks for the dialect, sorted by name; E033 for `.control` where nothing runs it.
        let (codes, top) = (doc.codes(), self.ifaces[0].cell);
        b.code_start.push(0);
        for r in self.code_rows(doc, top) {
            let lines = &doc.pool().lines[codes.verbatim[r].range()];
            b.code_name
                .push(self.names.intern(s.get(codes.fields[r][0])));
            b.code_lang
                .push(self.names.intern(s.get(codes.fields[r][1])));
            for &l in lines {
                let n = self.expand_vars(top, s.get(l));
                b.code_lines.push(n);
            }
            b.code_start.push(b.code_lines.len() as u32);
        }
        assert_eq!(b.var_name.len(), b.var_text.len());
        assert_eq!(b.corner_start.len(), b.corner_kind.len() + 1);
        assert_eq!(b.corner_project.len(), b.corner_kind.len());
        assert_eq!(b.code_start.len(), b.code_name.len() + 1);
        b
    }

    /// Rows of `doc`'s `code` table the dialect takes (spec §9.13), sorted by name. A
    /// block holding `.control` is left out for xyce, hspice and ltspice and reported as
    /// `E033` on `cell`.
    pub(super) fn code_rows(&mut self, doc: &Document, cell: Name) -> Vec<usize> {
        let (s, codes) = (doc.strings(), doc.codes());
        let mut rows: Vec<usize> = (0..codes.kind.len())
            .filter(|&r| self.req.dialect.takes_code(s.get(codes.fields[r][1])))
            .collect();
        rows.sort_by(|a, c| s.get(codes.fields[*a][0]).cmp(s.get(codes.fields[*c][0])));
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            // Only ngspice and ESPice run `.control` blocks.
            let control = matches!(
                self.req.dialect,
                Dialect::Xyce | Dialect::Hspice | Dialect::Ltspice
            ) && doc.pool().lines[codes.verbatim[r].range()].iter().any(|l| {
                s.get(*l)
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with(".control")
            });
            if !control {
                out.push(r);
                continue;
            }
            let name = self.names.intern(s.get(codes.fields[r][0]));
            self.viol.push(Violation::new(
                Code::E033,
                cell,
                ObjId::NONE,
                name,
                format!("{} has no .control blocks", self.req.dialect.as_str()),
            ));
        }
        assert!(out.len() <= codes.kind.len());
        debug_assert!(
            out.windows(2)
                .all(|w| s.get(codes.fields[w[0]][0]) <= s.get(codes.fields[w[1]][0])),
            "rows stay sorted by name"
        );
        out
    }

    /// Expands `${NAME}` in `text` (project `set` entries, then [`Request::env`];
    /// `$$` is a literal `$`) and interns the result. An undefined or unclosed name is
    /// `E034` on `cell`, and the text stays as written.
    pub(super) fn expand_vars(&mut self, cell: Name, text: &str) -> Name {
        if !text.contains('$') {
            return self.names.intern(text);
        }
        let sets = self.project.settings();
        let lookup = |n: &str| {
            sets.iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| v.as_str())
                .or_else(|| self.req.env.iter().find(|(k, _)| *k == n).map(|(_, v)| *v))
        };
        match expand_dollars(text, &lookup, 0) {
            Ok(out) => self.names.intern(&out),
            Err(name) => {
                let param = self.names.intern(&name);
                self.viol.push(Violation::new(
                    Code::E034,
                    cell,
                    ObjId::NONE,
                    param,
                    format!("`${{{name}}}` is not defined or expands cyclically"),
                ));
                self.names.intern(text)
            }
        }
    }
}

// ---- private ----

/// `${NAME}` expansion (spec §7.1): `$$` → `$`, `${NAME}` → `lookup(NAME)`, any other
/// `$` literal, recursively expanding the values it substitutes (project `set` values
/// such as `${PDK_ROOT}/${PDK}/libs.tech` are themselves templates). Nothing is executed.
/// Call with `depth` 0.
///
/// Errors: the first undefined name, the unclosed `${…` text, or the name at which
/// nesting passes [`EXPAND_DEPTH_MAX`] (a cycle).
fn expand_dollars<'value>(
    text: &str,
    lookup: &impl Fn(&str) -> Option<&'value str>,
    depth: u8,
) -> Result<String, String> {
    assert!(depth <= EXPAND_DEPTH_MAX);
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("$$") {
            out.push('$');
            rest = after;
        } else if let Some(after) = tail.strip_prefix("${") {
            let end = after.find('}').ok_or_else(|| tail.to_owned())?;
            let name = &after[..end];
            let value = lookup(name).ok_or_else(|| name.to_owned())?;
            if depth >= EXPAND_DEPTH_MAX {
                return Err(name.to_owned());
            }
            out.push_str(&expand_dollars(value, lookup, depth + 1)?);
            rest = &after[end + 1..];
        } else {
            out.push('$');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Evaluates a corner `temp` expression with the bench vars in scope.
fn eval_in_bench(text: &str, b: &Bench, names: &Names) -> Result<String, schemify_expr::Error> {
    assert_eq!(
        b.var_name.len(),
        b.var_text.len(),
        "bench var columns in step"
    );
    let p = Program::compile(text, Mode::Scalar)?;
    let vals: Vec<Val> = b
        .var_text
        .iter()
        .map(|t| Val::parse_cached(names.get(*t)))
        .collect();
    let mut inputs = Vec::new();
    p.bind(&mut inputs, |n| {
        let i = b.var_name.iter().position(|v| names.get(*v) == n)?;
        vals[i].value()
    })?;
    let mut scratch = Scratch::default();
    let v: Value<'_> = p.eval(&inputs, &mut scratch)?;
    let mut out = String::new();
    Val::from(v).write_text(&mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{EXPAND_DEPTH_MAX, expand_dollars};

    /// Spec §7.1 cases, each answer written by hand from the rule.
    #[test]
    fn expand_dollars_follows_the_spec() {
        let sets = [
            ("A", "a"),
            ("ROOT", "/pdk"),
            ("LIB", "${ROOT}/lib"),
            ("X", "${Y}"),
            ("Y", "${X}"),
        ];
        let lookup = |n: &str| sets.iter().find(|(k, _)| *k == n).map(|(_, v)| *v);
        let ok = |t: &str| expand_dollars(t, &lookup, 0);
        assert_eq!(ok("plain"), Ok("plain".to_owned()));
        assert_eq!(ok("$$A ${A} $A"), Ok("$A a $A".to_owned()));
        assert_eq!(ok("${LIB}/models"), Ok("/pdk/lib/models".to_owned()));
        // Negative space: undefined, unclosed, cyclic.
        assert_eq!(ok("${NOPE}"), Err("NOPE".to_owned()));
        assert_eq!(ok("x ${A"), Err("${A".to_owned()));
        assert!(ok("${X}").is_err());
        const { assert!(EXPAND_DEPTH_MAX > 2, "the nested case above fits") };
    }
}
