//! Findings in, the sorted violations table out (netlist spec §8): the check code
//! table, a finding's key and its order, and the [`Violations`] table every check pass
//! writes. Cites netlist spec §8 (codes, severities), D13 (waivable gating codes).
//!
//! Contents:
//! - `pub enum Code` (`as_str`, `severity`, `waivable`, `meaning`), `pub enum Severity`
//! - `pub struct Violation`: `key_cmp`, `pub(crate) new`
//! - `pub const VIOLATIONS_MAX`
//! - `pub struct Violations`: `diff`, `blocking`, `with`, `pub(crate) from_rows`
//! - `pub(crate) fn cmp_key(a: &Violation, b: &Violation, names: &Names) -> Ordering`

use crate::names::{Name, Names};
use schemify_doc::ObjId;
use std::cmp::Ordering;

/// Check codes (netlist spec §8; `E035`, `E036`, `W037` are elab amendments).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(missing_docs)] // meanings: `Code::meaning`
pub enum Code {
    E001,
    E002,
    E003,
    W004,
    E005,
    E006,
    W007,
    E008,
    E009,
    W010,
    E011,
    E012,
    E013,
    E014,
    W016,
    E030,
    E031,
    E032,
    E033,
    E034,
    E035,
    E036,
    W037,
    E038,
    E039,
    I005,
}

/// How much a violation matters (spec §8): `Error` blocks Run and netlisting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Severity {
    /// Blocks.
    Error,
    /// Reported.
    Warning,
    /// Informational (waived violations).
    Info,
}

/// One finding. Keyed by (code, cell, object, path, param).
#[derive(Clone, Debug, PartialEq)]
pub struct Violation {
    /// What.
    pub code: Code,
    /// Display name of the cell whose document holds the object (`lib:cell`).
    pub cell: Name,
    /// The object id in that document, or [`ObjId::NONE`].
    pub object: ObjId,
    /// Hierarchical instance path (`/X1/X3`), or [`Name::NONE`] when the finding holds
    /// for every path through the object.
    pub path: Name,
    /// Param, pin, net or impl name, or [`Name::NONE`].
    pub param: Name,
    /// Human-readable detail (cold).
    pub detail: String,
}

/// Most violations one table holds (ARCHITECTURE capacity).
pub const VIOLATIONS_MAX: usize = 1 << 20;

/// Violations, sorted by key and unique per key.
#[derive(Clone, Debug, Default)]
pub struct Violations {
    /// Rows, sorted by (code text, cell text, object, path text, param text).
    pub rows: Vec<Violation>,
}

impl Code {
    /// Returns the code as written in reports (`"E005"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        const S: [&str; 26] = [
            "E001", "E002", "E003", "W004", "E005", "E006", "W007", "E008", "E009", "W010", "E011",
            "E012", "E013", "E014", "W016", "E030", "E031", "E032", "E033", "E034", "E035", "E036",
            "W037", "E038", "E039", "I005",
        ];
        S[self as usize]
    }

    /// Returns the severity, from the code's letter.
    #[must_use]
    pub fn severity(self) -> Severity {
        match self.as_str().as_bytes()[0] {
            b'E' => Severity::Error,
            b'W' => Severity::Warning,
            _ => Severity::Info,
        }
    }

    /// Returns whether a `waive` record can downgrade it to `I005` (D13).
    #[must_use]
    pub fn waivable(self) -> bool {
        matches!(
            self,
            Code::E005 | Code::E006 | Code::E032 | Code::E038 | Code::E039
        )
    }

    /// Returns the spec meaning.
    #[must_use]
    pub fn meaning(self) -> &'static str {
        match self {
            Code::E001 => "missing lib/cell, or bench instantiated",
            Code::E002 => "impl pins differ from symbol pins",
            Code::E003 => "shorted names on one net",
            Code::W004 => "instance pin unconnected",
            Code::E005 => "param outside min/max",
            Code::E006 => "param not in choices",
            Code::W007 => "override of unknown param",
            Code::E008 => "expression error",
            Code::E009 => "duplicate instance name in a cell",
            Code::W010 => "real-domain pin on an analog/digital net",
            Code::E011 => "bus width mismatch",
            Code::E012 => "port without interface pin, or interface pin without port",
            Code::E013 => "dialect unsupported by selected impl",
            Code::E014 => "hierarchy recursion",
            Code::W016 => "net with a single connection",
            Code::E030 => "override of computed param",
            Code::E031 => "config names an impl the cell lacks",
            Code::E032 => "param off grid/step",
            Code::E033 => ".control in a code block for xyce, hspice or ltspice",
            Code::E034 => "undefined ${NAME} in code, body or corner path",
            Code::E035 => "unknown corner or config",
            Code::E036 => "inherited pins of one name with different default nets",
            Code::W037 => "config inst rule names a path no instance has",
            Code::E038 => "param value fails its valid rule",
            Code::E039 => "Virtuoso-derived value is stale",
            Code::I005 => "waived violation",
        }
    }
}

impl Violation {
    /// A finding on every path through `object` ([`Violation::path`] is [`Name::NONE`]).
    pub(crate) fn new(
        code: Code,
        cell: Name,
        object: ObjId,
        param: Name,
        detail: String,
    ) -> Violation {
        Violation {
            code,
            cell,
            object,
            path: Name::NONE,
            param,
            detail,
        }
    }

    /// The order [`Violations::rows`] are sorted in (unique per table): code text, cell,
    /// object, path, param (texts through `names`, the arena both rows' names are from).
    #[must_use]
    pub fn key_cmp(&self, other: &Violation, names: &Names) -> Ordering {
        cmp_key(self, other, names)
    }
}

impl Violations {
    /// Sorts `rows` by key and keeps the first row of each key. Beyond
    /// [`VIOLATIONS_MAX`] rows the tail is dropped (the count stays visible as the cap).
    pub(crate) fn from_rows(mut rows: Vec<Violation>, names: &Names) -> Violations {
        rows.sort_by(|a, b| cmp_key(a, b, names));
        rows.dedup_by(|b, a| cmp_key(a, b, names) == Ordering::Equal);
        rows.truncate(VIOLATIONS_MAX);
        // Room for a patch's insertions: the first one after a build would otherwise
        // reallocate the whole table in an edit frame.
        rows.reserve(rows.len() / 8 + 64);
        debug_assert!(
            rows.windows(2)
                .all(|w| cmp_key(&w[0], &w[1], names) == Ordering::Less)
        );
        Violations { rows }
    }

    /// What changed from `old` to `self` (both from the same names arena): `(gone, new)`,
    /// the rows only `old` has and the rows only `self` has, each in key order. A row whose
    /// key stayed but whose detail changed is in both.
    ///
    /// Cost: one merge pass, O(rows) row compares; key comparisons (string compares through
    /// `names`) only where the two lists differ.
    ///
    /// # Panics
    ///
    /// Panics only on a broken internal invariant: a side listing more rows than it has.
    #[must_use]
    pub fn diff<'a>(
        &'a self,
        old: &'a Violations,
        names: &Names,
    ) -> (Vec<&'a Violation>, Vec<&'a Violation>) {
        let (mut gone, mut new) = (Vec::new(), Vec::new());
        let (a, b) = (&old.rows, &self.rows);
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            if a[i] == b[j] {
                (i, j) = (i + 1, j + 1);
                continue;
            }
            match cmp_key(&a[i], &b[j], names) {
                Ordering::Less => {
                    gone.push(&a[i]);
                    i += 1;
                }
                Ordering::Greater => {
                    new.push(&b[j]);
                    j += 1;
                }
                Ordering::Equal => {
                    gone.push(&a[i]);
                    new.push(&b[j]);
                    (i, j) = (i + 1, j + 1);
                }
            }
        }
        gone.extend(&a[i..]);
        new.extend(&b[j..]);
        assert!(gone.len() <= a.len(), "gone rows come from old");
        assert!(new.len() <= b.len(), "new rows come from self");
        (gone, new)
    }

    /// Returns whether any row blocks (severity `Error`).
    #[must_use]
    pub fn blocking(&self) -> bool {
        self.rows
            .iter()
            .any(|v| v.code.severity() == Severity::Error)
    }

    /// Returns the rows with `code`.
    pub fn with(&self, code: Code) -> impl Iterator<Item = &Violation> + '_ {
        self.rows.iter().filter(move |v| v.code == code)
    }
}

/// Orders violations by their key (code text, cell, object, path, param).
pub(crate) fn cmp_key(a: &Violation, b: &Violation, names: &Names) -> Ordering {
    let code = if a.code == b.code {
        Ordering::Equal
    } else {
        a.code.as_str().cmp(b.code.as_str())
    };
    code.then_with(|| names.cmp(a.cell, b.cell))
        .then(a.object.cmp(&b.object))
        .then_with(|| names.cmp(a.path, b.path))
        .then_with(|| names.cmp(a.param, b.param))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_have_severity_by_letter_and_only_gating_is_waivable() {
        assert_eq!(Code::E005.severity(), Severity::Error);
        assert_eq!(Code::W016.severity(), Severity::Warning);
        assert_eq!(Code::I005.severity(), Severity::Info);
        let waivable: Vec<Code> = [Code::E001, Code::E005, Code::E006, Code::E032, Code::E030]
            .into_iter()
            .filter(|c| c.waivable())
            .collect();
        assert_eq!(waivable, [Code::E005, Code::E006, Code::E032]);
        assert!(Code::E039.waivable() && Code::E038.waivable());
        assert_eq!(Code::I005.as_str(), "I005");
        assert_eq!(Code::E035.as_str(), "E035");
    }

    /// Oracle: a set difference by key and detail over rows of a few keys, after the
    /// table's own sort and dedup (one row per key, the first kept).
    #[test]
    fn diff_equals_a_set_difference_of_sorted_unique_rows() {
        let mut names = Names::default();
        let cell = names.intern("c");
        let params: Vec<Name> = ["a", "b", ""].iter().map(|t| names.intern(t)).collect();
        let mut seed = 0x2545_f491_u64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..300 {
            let make = |rnd: &mut dyn FnMut(u64) -> u64| -> Vec<Violation> {
                (0..rnd(8))
                    .map(|_| {
                        let code = [Code::E005, Code::W016, Code::E011][rnd(3) as usize];
                        let param = params[rnd(3) as usize];
                        let object = ObjId(rnd(2) as u32);
                        Violation::new(code, cell, object, param, format!("d{}", rnd(2)))
                    })
                    .collect()
            };
            let (a, b) = (make(&mut rnd), make(&mut rnd));
            let (old, new) = (
                Violations::from_rows(a, &names),
                Violations::from_rows(b, &names),
            );
            for t in [&old, &new] {
                assert!(
                    t.rows
                        .windows(2)
                        .all(|w| w[0].key_cmp(&w[1], &names).is_lt())
                );
            }
            let (gone, came) = new.diff(&old, &names);
            let want_gone: Vec<&Violation> =
                old.rows.iter().filter(|r| !new.rows.contains(r)).collect();
            let want_came: Vec<&Violation> =
                new.rows.iter().filter(|r| !old.rows.contains(r)).collect();
            assert_eq!(gone, want_gone);
            assert_eq!(came, want_came);
        }
    }
}
