//! Diagnostics (spec §7): the error and warning codes this crate reports, with positions.

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

impl Code {
    /// Returns the code as written in diagnostics and the spec (`"E022"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Code::E021 => "E021",
            Code::E022 => "E022",
            Code::E023 => "E023",
            Code::E024 => "E024",
            Code::E025 => "E025",
            Code::E026 => "E026",
            Code::E027 => "E027",
            Code::E028 => "E028",
            Code::E029 => "E029",
            Code::W001 => "W001",
        }
    }

    /// Returns the spec §7 meaning.
    pub fn meaning(self) -> &'static str {
        match self {
            Code::E021 => "file too large",
            Code::E022 => "coordinate not representable",
            Code::E023 => "record out of rank order",
            Code::E024 => "duplicate name within a kind",
            Code::E025 => "kind rule violated",
            Code::E026 => "zero-length wire",
            Code::E027 => "unknown record",
            Code::E028 => "malformed line",
            Code::E029 => "wrong arity or value form",
            Code::W001 => "duplicate id",
        }
    }

    /// Returns whether the code is a warning; warnings never fail a parse.
    pub fn is_warning(self) -> bool {
        let warning = self == Code::W001;
        assert_eq!(warning, self.as_str().starts_with('W'));
        warning
    }
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

/// Why a file failed to parse: every error found, in line order (capped at 64).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// Errors only; warnings of a failed parse are dropped.
    pub diagnostics: Vec<Diagnostic>,
}

impl ParseError {
    /// Returns whether any diagnostic has `code`.
    pub fn has(&self, code: Code) -> bool {
        self.diagnostics.iter().any(|d| d.code == code)
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
