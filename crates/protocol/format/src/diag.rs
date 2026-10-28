//! Diagnostics: reader findings → codes with positions (spec §7).
//!
//! Contents:
//! - `pub enum Code`, `Code::as_str`, `Code::meaning`, `Code::is_warning`
//! - `pub const NO_RECORD: u32`
//! - `pub struct Diagnostic`
//! - `pub struct ParseError`, `ParseError::has`

use std::fmt;

/// Format-level codes this crate produces. `E020` (two kinds for one cell stem) and
/// `E030` (override of a computed param) need more than one file and are reported by
/// the `doc` and checker layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(missing_docs)] // meanings are in `Code::meaning` and spec §7
pub enum Code {
    E021,
    E022,
    E023,
    E024,
    E025,
    E026,
    E027,
    E028,
    E029,
    W001,
}

/// Sentinel for [`Diagnostic::record`] when no record is involved (lexical errors).
pub const NO_RECORD: u32 = u32::MAX;

/// One error or warning with its position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// What went wrong.
    pub code: Code,
    /// 1-based line.
    pub line: u32,
    /// 1-based byte column within the line; 0 when the position is the whole line
    /// (a value that was unescaped from a quoted string has no single column).
    pub col: u32,
    /// Index of the record involved, or [`NO_RECORD`]. For `W001` this is the later
    /// duplicate, the one the `doc` layer re-ids.
    pub record: u32,
    /// Which rule was broken, beyond the code's meaning.
    pub detail: &'static str,
}

/// Why a file failed to parse: every error found, in line order (capped at 64).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// Errors only; warnings of a failed parse are dropped.
    pub diagnostics: Vec<Diagnostic>,
}

/// Code text and spec §7 meaning, in `Code` declaration order.
const CODES: [(&str, &str); 10] = [
    ("E021", "file too large"),
    ("E022", "coordinate not representable"),
    ("E023", "record out of rank order"),
    ("E024", "duplicate name within a kind"),
    ("E025", "kind rule violated"),
    ("E026", "zero-length wire"),
    ("E027", "unknown record"),
    ("E028", "malformed line"),
    ("E029", "wrong arity or value form"),
    ("W001", "duplicate id"),
];

impl Code {
    /// Returns the code as written in diagnostics and the spec (`"E022"`).
    pub fn as_str(self) -> &'static str {
        CODES[self as usize].0
    }

    /// Returns the spec §7 meaning.
    pub fn meaning(self) -> &'static str {
        CODES[self as usize].1
    }

    /// Returns whether the code is a warning; warnings never fail a parse.
    pub fn is_warning(self) -> bool {
        let warning = self == Code::W001;
        assert_eq!(warning, self.as_str().starts_with('W'));
        warning
    }
}

impl ParseError {
    /// Returns whether any diagnostic has `code`.
    pub fn has(&self, code: Code) -> bool {
        self.diagnostics.iter().any(|d| d.code == code)
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (code, meaning) = (self.code.as_str(), self.code.meaning());
        write!(
            f,
            "{}:{}: {code} {meaning}: {}",
            self.line, self.col, self.detail
        )
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, d) in self.diagnostics.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{d}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: spec §7's table, typed independently of `CODES`.
    #[test]
    fn codes_match_the_spec_table() {
        let all = [
            (Code::E021, "E021", "file too large"),
            (Code::E022, "E022", "coordinate not representable"),
            (Code::E026, "E026", "zero-length wire"),
            (Code::E029, "E029", "wrong arity or value form"),
            (Code::W001, "W001", "duplicate id"),
        ];
        for (code, text, meaning) in all {
            assert_eq!((code.as_str(), code.meaning()), (text, meaning));
            assert_eq!(code.is_warning(), code == Code::W001);
        }
    }
}
