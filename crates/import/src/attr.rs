//! xschem attribute strings: `key=value` tokenizing, value unescaping, `@token` scanning
//! and idiom classification (research: `xschem-feature-inventory.md` §5.4, §4A.5;
//! `pdk-xschem-devices.md` §1, §5.5–§5.6).
//!
//! An attribute string is the content of a `{…}` brace string after the file-level
//! escapes are gone ([`crate::xschem`] removes them). This module removes the second
//! layer: separators are space, TAB, newline, `;` and NUL outside quotes; `"` toggles
//! quoting unless escaped; `\` escapes the next char; the value handed out drops every
//! unescaped `"` and `\` (xschem's normal lookup). Nothing is evaluated: `tcleval(…)`,
//! `expr(…)`, `@tokens` stay as text, and [`Idioms`] records which of them a value uses so
//! the translator can count and route them.

use std::ops::{BitOr, BitOrAssign};

use crate::{Span, push_span};

/// One attribute string, tokenized: a table with one row per `key[=value]` occurrence,
/// in source order. Duplicates are kept; lookups return the first (xschem's rule).
///
/// Capacity: at most one row per input byte. [`Attrs::parse_into`] reuses the buffers, so
/// a caller looping over many strings allocates only when one is larger than any before.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attrs {
    arena: String,
    key: Vec<Span>,
    value: Vec<Span>,
    raw: Vec<Span>,
    flag: Vec<bool>,
    idioms: Vec<Idioms>,
}

impl Attrs {
    /// Tokenizes `s` into a fresh table.
    ///
    /// # Examples
    ///
    /// ```
    /// use schemify_import::attr::{Attrs, ValueClass};
    /// let a = Attrs::parse("name=M1 ;  W = 1\nad=\"expr('@W * 0.29')\" lock");
    /// assert_eq!(a.get("W"), Some("1"));
    /// assert_eq!(a.get("ad"), Some("expr('@W * 0.29')"));
    /// assert_eq!(a.class(a.find("ad").unwrap()), ValueClass::Expr);
    /// assert!(a.is_flag(a.find("lock").unwrap()));
    /// ```
    pub fn parse(s: &str) -> Attrs {
        let mut a = Attrs::default();
        a.parse_into(s);
        a
    }

    /// Clears the table and tokenizes `s` into it. Total: any input yields some table.
    ///
    /// # Panics
    ///
    /// Panics if `s` is longer than `u32::MAX` bytes (spans are `u32`).
    ///
    /// # Time complexity
    ///
    /// O(len) to tokenize plus O(len) per value to classify.
    pub fn parse_into(&mut self, s: &str) {
        self.arena.clear();
        self.key.clear();
        self.value.clear();
        self.raw.clear();
        self.flag.clear();
        self.idioms.clear();
        let b = s.as_bytes();
        let n = b.len();
        let blank = |c: u8| matches!(c, b' ' | b'\t' | b'\n');
        let mut i = 0;
        while i < n {
            while i < n && is_separator(b[i]) {
                i += 1;
            }
            if i == n {
                break;
            }
            let key_start = i;
            while i < n && !is_separator(b[i]) && b[i] != b'=' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i = i.min(n);
            let key_end = i;
            let mut j = i;
            while j < n && blank(b[j]) {
                j += 1;
            }
            if j < n && b[j] == b'=' {
                i = j + 1;
                while i < n && blank(b[i]) {
                    i += 1;
                }
                let value_start = i;
                let mut quoted = false;
                while i < n {
                    match b[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            quoted = !quoted;
                            i += 1;
                        }
                        c if !quoted && is_separator(c) => break,
                        _ => i += 1,
                    }
                }
                i = i.min(n);
                self.push(s, key_start..key_end, Some(value_start..i));
            } else {
                assert!(
                    key_end > key_start,
                    "a bare flag consumes at least one byte"
                );
                self.push(s, key_start..key_end, None);
            }
        }
        assert!(self.key.len() <= n);
        debug_assert!(self.key.len() == self.value.len() && self.key.len() == self.idioms.len());
    }

    fn push(
        &mut self,
        s: &str,
        key: std::ops::Range<usize>,
        value: Option<std::ops::Range<usize>>,
    ) {
        // ponytail: keys and values are copied (unescaped) into one reused arena instead of
        // borrowing `s`; strings are tens of bytes, so a borrow-when-unescaped variant
        // would buy nothing measurable.
        let k_start = self.arena.len();
        unescape(&s[key.clone()], &mut self.arena);
        self.key.push(Span::new(k_start, self.arena.len()));
        let raw = value.clone().unwrap_or(key.end..key.end);
        let v_start = self.arena.len();
        unescape(&s[raw.clone()], &mut self.arena);
        let value_span = Span::new(v_start, self.arena.len());
        self.idioms.push(classify(value_span.of(&self.arena)));
        self.value.push(value_span);
        self.raw.push(Span::new(raw.start, raw.end));
        self.flag.push(value.is_none());
    }

    /// Returns the number of rows.
    pub fn len(&self) -> usize {
        self.key.len()
    }

    /// Returns true when the string held no tokens.
    pub fn is_empty(&self) -> bool {
        self.key.is_empty()
    }

    /// Returns row `i`'s key (escapes removed).
    pub fn key(&self, i: usize) -> &str {
        self.key[i].of(&self.arena)
    }

    /// Returns row `i`'s value with quotes and escapes removed; `""` for a bare flag.
    pub fn value(&self, i: usize) -> &str {
        self.value[i].of(&self.arena)
    }

    /// Returns row `i`'s value as written, as a byte range of the parsed input string
    /// (empty, at the key's end, for a bare flag). Use it for source locations.
    pub fn raw(&self, i: usize) -> Span {
        self.raw[i]
    }

    /// Returns true when row `i` is a bare `key` with no `=`.
    pub fn is_flag(&self, i: usize) -> bool {
        self.flag[i]
    }

    /// Returns the idioms row `i`'s value uses.
    pub fn idioms(&self, i: usize) -> Idioms {
        self.idioms[i]
    }

    /// Returns the routing class of row `i`'s value.
    pub fn class(&self, i: usize) -> ValueClass {
        self.idioms[i].class()
    }

    /// Returns the row of the first occurrence of `key` (exact, case-sensitive match).
    pub fn find(&self, key: &str) -> Option<usize> {
        (0..self.len()).find(|&i| self.key(i) == key)
    }

    /// Returns the value of the first occurrence of `key`; `Some("")` for a bare flag.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.find(key).map(|i| self.value(i))
    }

    /// Inserts `key=value` (already unescaped) as the first row, so every lookup sees it
    /// before an existing row of that key: how an instance attribute overrides its
    /// symbol's. The row has no source location ([`Attrs::raw`] is empty at 0). Shifts
    /// every row index by one.
    pub fn prepend(&mut self, key: &str, value: &str) {
        let n = self.len();
        let k = push_span(&mut self.arena, key);
        let v = push_span(&mut self.arena, value);
        self.key.insert(0, k);
        self.value.insert(0, v);
        self.raw.insert(0, Span::new(0, 0));
        self.flag.insert(0, false);
        self.idioms.insert(0, classify(value));
        assert_eq!(self.len(), n + 1);
        assert_eq!(self.get(key), Some(value));
    }
}

fn is_separator(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b';' | 0)
}

/// Appends `raw` to `out` minus every unescaped `"` and `\` (`\x` keeps `x` literally).
///
/// This is xschem's normal attribute lookup unescaping: `\"` becomes `"`, `\\` becomes
/// `\`, a lone trailing `\` disappears.
pub fn unescape(raw: &str, out: &mut String) {
    let b = raw.as_bytes();
    let mut seg = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                out.push_str(&raw[seg..i]);
                seg = i + 1;
                i += 2;
            }
            b'"' => {
                out.push_str(&raw[seg..i]);
                i += 1;
                seg = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&raw[seg.min(b.len())..]);
}

/// What an `@`/`%` token refers to (research §4A.5 tokenizer rules).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtKind {
    /// `@name`: an attribute or a built-in (`@pinlist`, `@symname`, `@spice_get_*`, …).
    Attr,
    /// `@@pin`: the net on the named pin.
    PinNet,
    /// `@#n[:attr]` / `@#pin[:attr]`: pin net or pin attribute by index or name.
    PinAttr,
    /// `%attr`: like `@attr` but yields the literal name when undefined.
    Percent,
}

/// One substitution token inside a value or text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtToken<'a> {
    /// What the token refers to.
    pub kind: AtKind,
    /// Byte offset of the leading `@`/`%` in the scanned text.
    pub offset: u32,
    /// The name after the sigil(s): everything up to whitespace, `@`, `%` or `\`.
    pub name: &'a str,
    /// True when a `\`-escaped char ends the token (`@prefix\a21o_1`): the backslash
    /// is a terminator, emitted as nothing.
    pub terminated: bool,
}

/// Iterator over the `@`/`%` tokens of an unescaped value or text, in order.
///
/// Rules: a token starts at an unescaped `@` or `%` and needs at least one name char; it
/// ends at whitespace, the next `@`/`%`, or a backslash. `\@` is a literal `@`.
#[derive(Clone, Debug)]
pub struct AtTokens<'a> {
    text: &'a str,
    pos: usize,
}

/// Returns the substitution tokens of `text` (an unescaped value or a `T` text).
///
/// # Examples
///
/// ```
/// use schemify_import::attr::{at_tokens, AtKind};
/// let t: Vec<_> = at_tokens("@spiceprefix@name @@D \\@lit @prefix\\a21").collect();
/// assert_eq!(t.iter().map(|t| t.name).collect::<Vec<_>>(), ["spiceprefix", "name", "D", "prefix"]);
/// assert_eq!(t[2].kind, AtKind::PinNet);
/// assert!(t[3].terminated);
/// ```
pub fn at_tokens(text: &str) -> AtTokens<'_> {
    AtTokens { text, pos: 0 }
}

impl<'a> Iterator for AtTokens<'a> {
    type Item = AtToken<'a>;

    fn next(&mut self) -> Option<AtToken<'a>> {
        let b = self.text.as_bytes();
        while self.pos < b.len() {
            let c = b[self.pos];
            if c == b'\\' {
                self.pos += 2;
                continue;
            }
            if c != b'@' && c != b'%' {
                self.pos += 1;
                continue;
            }
            let start = self.pos;
            self.pos += 1;
            let kind = match (c, b.get(self.pos)) {
                (b'%', _) => AtKind::Percent,
                (_, Some(b'@')) => AtKind::PinNet,
                (_, Some(b'#')) => AtKind::PinAttr,
                _ => AtKind::Attr,
            };
            if matches!(kind, AtKind::PinNet | AtKind::PinAttr) {
                self.pos += 1;
            }
            let name_start = self.pos;
            while self.pos < b.len()
                && !matches!(
                    b[self.pos],
                    b' ' | b'\t' | b'\n' | b'\r' | b'@' | b'%' | b'\\'
                )
            {
                self.pos += 1;
            }
            if self.pos == name_start {
                if kind == AtKind::PinNet {
                    // `@@` followed by a terminator: re-scan the second `@` on its own.
                    self.pos = start + 1;
                }
                continue;
            }
            return Some(AtToken {
                kind,
                offset: start as u32,
                name: &self.text[name_start..self.pos],
                terminated: b.get(self.pos) == Some(&b'\\'),
            });
        }
        None
    }
}

/// Set of idioms one value uses (a bitset; see the associated constants).
///
/// The `TCL_*` bits are only looked for after a `tcleval(` in the same value, because a
/// `$` or `[` anywhere else is literal text to xschem.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Idioms(pub u32);

impl Idioms {
    /// `@attr` substitution of a plain attribute (including `@name`, `@model`).
    pub const AT_ATTR: Idioms = Idioms(1 << 0);
    /// `@pinlist`.
    pub const AT_PINLIST: Idioms = Idioms(1 << 1);
    /// `@@pin`.
    pub const AT_PIN_NET: Idioms = Idioms(1 << 2);
    /// `@#n…` / `@#pin…`.
    pub const AT_PIN_ATTR: Idioms = Idioms(1 << 3);
    /// `@spiceprefix`.
    pub const AT_SPICEPREFIX: Idioms = Idioms(1 << 4);
    /// `@symname`, `@symname_ext`, `@symref`.
    pub const AT_SYMNAME: Idioms = Idioms(1 << 5);
    /// `@path`.
    pub const AT_PATH: Idioms = Idioms(1 << 6);
    /// `@spice_get_*` or `@#n:spice_get_*` (simulation back-annotation, display only).
    pub const AT_SIM_VALUE: Idioms = Idioms(1 << 7);
    /// A token ended by a backslash (`@prefix\cell`).
    pub const AT_TERMINATOR: Idioms = Idioms(1 << 8);
    /// `%attr`.
    pub const AT_PERCENT: Idioms = Idioms(1 << 9);
    /// `expr(…)` (xschem's C arithmetic evaluator).
    pub const EXPR: Idioms = Idioms(1 << 10);
    /// `expr_eng(…)` / `expr_eng4(…)`.
    pub const EXPR_ENG: Idioms = Idioms(1 << 11);
    /// A single-quoted SPICE expression with `*`, `/` or `+`, outside `expr('…')`.
    pub const SPICE_QUOTED: Idioms = Idioms(1 << 12);
    /// `tcleval(` anywhere in the value.
    pub const TCLEVAL: Idioms = Idioms(1 << 13);
    /// Tcl `$var`, `$::var`, `${var}` after `tcleval(`.
    pub const TCL_VAR: Idioms = Idioms(1 << 14);
    /// `[ev …]` / `[ev7 …]`.
    pub const TCL_EV: Idioms = Idioms(1 << 15);
    /// Tcl `[expr …]`.
    pub const TCL_EXPR: Idioms = Idioms(1 << 16);
    /// `[abs_sym_path …]`.
    pub const TCL_ABS_SYM_PATH: Idioms = Idioms(1 << 17);
    /// `[symtype]`.
    pub const TCL_SYMTYPE: Idioms = Idioms(1 << 18);
    /// `[hierarchy_config …]`.
    pub const TCL_HIER_CONFIG: Idioms = Idioms(1 << 19);
    /// `[sim_is_* …]`.
    pub const TCL_SIM_IS: Idioms = Idioms(1 << 20);
    /// `[ngspice::…]` (display only).
    pub const TCL_NGSPICE: Idioms = Idioms(1 << 21);
    /// `[subst …]`, `[to_eng …]` (display only).
    pub const TCL_DISPLAY: Idioms = Idioms(1 << 22);
    /// Any other `[command …]`: unknown Tcl, goes to `needs_review`.
    pub const TCL_OTHER: Idioms = Idioms(1 << 23);
    /// `pre_osdi` (per-netlist Verilog-A model load).
    pub const PRE_OSDI: Idioms = Idioms(1 << 24);

    /// Every `@`/`%` token bit.
    pub const AT_ANY: Idioms = Idioms((1 << 10) - 1);
    /// Every bit set by a `[command]` after `tcleval(`.
    pub const TCL_COMMAND: Idioms = Idioms(((1 << 24) - 1) & !((1 << 15) - 1));

    /// Returns true when every bit of `other` is set.
    pub fn contains(self, other: Idioms) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns true when any bit of `other` is set.
    pub fn intersects(self, other: Idioms) -> bool {
        self.0 & other.0 != 0
    }

    /// Returns the routing class: the most demanding idiom present wins
    /// (`Tcleval` > `Expr` > `SpiceQuoted` > `AtSubst` > `Plain`).
    pub fn class(self) -> ValueClass {
        if self.intersects(Idioms::TCLEVAL) {
            ValueClass::Tcleval
        } else if self.intersects(Idioms(Idioms::EXPR.0 | Idioms::EXPR_ENG.0)) {
            ValueClass::Expr
        } else if self.intersects(Idioms::SPICE_QUOTED) {
            ValueClass::SpiceQuoted
        } else if self.intersects(Idioms::AT_ANY) {
            ValueClass::AtSubst
        } else {
            ValueClass::Plain
        }
    }
}

impl BitOr for Idioms {
    type Output = Idioms;
    fn bitor(self, rhs: Idioms) -> Idioms {
        Idioms(self.0 | rhs.0)
    }
}

impl BitOrAssign for Idioms {
    fn bitor_assign(&mut self, rhs: Idioms) {
        self.0 |= rhs.0;
    }
}

/// How the translator must treat a value, derived from its [`Idioms`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValueClass {
    /// Literal text.
    Plain,
    /// Contains `@`/`%` substitutions only: template conversion.
    AtSubst,
    /// Contains a SPICE-side quoted expression: emitted verbatim.
    SpiceQuoted,
    /// Contains `expr(…)`/`expr_eng(…)`: needs the expression evaluator.
    Expr,
    /// Contains `tcleval(…)`: needs pattern rewriting or review.
    Tcleval,
}

/// Known Tcl commands after `tcleval(`, by exact name (PDK doc §5.5 classes a–d).
const TCL_COMMANDS: &[(&str, Idioms)] = &[
    ("ev", Idioms::TCL_EV),
    ("ev7", Idioms::TCL_EV),
    ("expr", Idioms::TCL_EXPR),
    ("abs_sym_path", Idioms::TCL_ABS_SYM_PATH),
    ("symtype", Idioms::TCL_SYMTYPE),
    ("hierarchy_config", Idioms::TCL_HIER_CONFIG),
    ("subst", Idioms::TCL_DISPLAY),
    ("to_eng", Idioms::TCL_DISPLAY),
];

/// Returns the idioms `text` uses. `text` is an unescaped value or a `T` text.
///
/// # Examples
///
/// ```
/// use schemify_import::attr::{classify, Idioms, ValueClass};
/// let i = classify("tcleval(Q@name le=[ev { @El * 1.0e-6 } ])");
/// assert!(i.contains(Idioms::TCLEVAL | Idioms::TCL_EV | Idioms::AT_ATTR));
/// assert_eq!(i.class(), ValueClass::Tcleval);
/// assert_eq!(classify("$[@model\\]").class(), ValueClass::AtSubst);
/// ```
pub fn classify(text: &str) -> Idioms {
    let mut bits = Idioms::default();
    for t in at_tokens(text) {
        bits |= match t.kind {
            AtKind::PinNet => Idioms::AT_PIN_NET,
            AtKind::Percent => Idioms::AT_PERCENT,
            AtKind::PinAttr if t.name.contains(":spice_get_") => {
                Idioms::AT_PIN_ATTR | Idioms::AT_SIM_VALUE
            }
            AtKind::PinAttr => Idioms::AT_PIN_ATTR,
            // Built-ins match on the leading identifier: `schematic=` substitutes
            // `@symname` as a substring (research §4A.4), as in `[hierarchy_config @symname]`.
            AtKind::Attr => match t
                .name
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .next()
                .unwrap_or("")
            {
                "pinlist" => Idioms::AT_PINLIST,
                "spiceprefix" => Idioms::AT_SPICEPREFIX,
                "symname" | "symname_ext" | "symref" => Idioms::AT_SYMNAME,
                "path" => Idioms::AT_PATH,
                n if n.starts_with("spice_get_") => Idioms::AT_SIM_VALUE,
                "tcleval" => Idioms::TCLEVAL,
                _ => Idioms::AT_ATTR,
            },
        };
        if t.terminated {
            bits |= Idioms::AT_TERMINATOR;
        }
    }
    let b = text.as_bytes();
    let mut i = 0;
    let mut tcl_from = None;
    while i < b.len() {
        let rest = &b[i..];
        let word_start = i == 0 || !is_word(b[i - 1]);
        let call = if !word_start {
            None
        } else if rest.starts_with(b"expr(") {
            Some((Idioms::EXPR, 5))
        } else if rest.starts_with(b"expr_eng(") {
            Some((Idioms::EXPR_ENG, 9))
        } else if rest.starts_with(b"expr_eng4(") {
            Some((Idioms::EXPR_ENG, 10))
        } else if rest.starts_with(b"tcleval(") {
            tcl_from.get_or_insert(i + 8);
            Some((Idioms::TCLEVAL, 8))
        } else {
            None
        };
        if let Some((bit, len)) = call {
            bits |= bit;
            i += len;
            // `expr('…')`: the quotes belong to xschem, not to SPICE.
            if bit != Idioms::TCLEVAL && b.get(i) == Some(&b'\'') {
                i = text[i + 1..].find('\'').map_or(b.len(), |q| i + 1 + q + 1);
            }
            continue;
        }
        if b[i] == b'\'' {
            let Some(q) = text[i + 1..].find('\'') else {
                break;
            };
            if text[i + 1..i + 1 + q].contains(['*', '/', '+']) {
                bits |= Idioms::SPICE_QUOTED;
            }
            i += q + 2;
            continue;
        }
        i += 1;
    }
    if text.contains("pre_osdi") {
        bits |= Idioms::PRE_OSDI;
    }
    if let Some(from) = tcl_from {
        bits |= classify_tcl(&text[from..]);
    }
    bits
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Classifies the Tcl text after a `tcleval(`: variables and bracketed commands.
fn classify_tcl(tcl: &str) -> Idioms {
    let b = tcl.as_bytes();
    let mut bits = Idioms::default();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'$' => {
                let next = b.get(i + 1).copied().unwrap_or(b' ');
                if next == b'{' || next == b':' || next.is_ascii_alphabetic() || next == b'_' {
                    bits |= Idioms::TCL_VAR;
                }
                i += 1;
            }
            b'[' => {
                i += 1;
                while i < b.len() && matches!(b[i], b' ' | b'\t') {
                    i += 1;
                }
                let start = i;
                while i < b.len() && (is_word(b[i]) || b[i] == b':') {
                    i += 1;
                }
                let name = &tcl[start..i];
                if !name.is_empty() {
                    bits |= TCL_COMMANDS.iter().find(|(n, _)| *n == name).map_or_else(
                        || {
                            if name.starts_with("sim_is_") {
                                Idioms::TCL_SIM_IS
                            } else if name.starts_with("ngspice::") {
                                Idioms::TCL_NGSPICE
                            } else if name.bytes().all(|c| c.is_ascii_digit()) {
                                Idioms::default() // `[3]`: an index, not a command
                            } else {
                                Idioms::TCL_OTHER
                            }
                        },
                        |(_, bit)| *bit,
                    );
                }
            }
            _ => i += 1,
        }
    }
    debug_assert!(!bits.intersects(Idioms::AT_ANY));
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_escape_layers_from_research_examples() {
        // res.sym: file `verilog_format="tran @name (@@P\\\\, @@M\\\\);"` → brace string
        // `…@@P\\, …` → value `…@@P\, …` (§5.4).
        let a = Attrs::parse("verilog_format=\"tran @name (@@P\\\\, @@M\\\\);\"");
        assert_eq!(a.get("verilog_format"), Some("tran @name (@@P\\, @@M\\);"));
        let i = a.idioms(0);
        assert!(i.contains(Idioms::AT_ATTR | Idioms::AT_PIN_NET | Idioms::AT_TERMINATOR));
        // sky130 template: nested quoted values survive as text.
        let k = Attrs::parse(
            "template=\"name=M1\nad=\\\"expr('int((@nf + 1)/2) * @W / @nf * 0.29')\\\"\n\"",
        );
        let t = Attrs::parse(k.get("template").unwrap());
        assert_eq!(
            t.get("ad"),
            Some("expr('int((@nf + 1)/2) * @W / @nf * 0.29')")
        );
        assert_eq!(t.class(t.find("ad").unwrap()), ValueClass::Expr);
        assert!(!t.idioms(1).contains(Idioms::SPICE_QUOTED));
    }

    #[test]
    fn separators_flags_duplicates() {
        let a = Attrs::parse(" a=1;b = \"x y\";c d=\"\" a=2 \t=e ");
        let rows: Vec<_> = (0..a.len())
            .map(|i| (a.key(i), a.value(i), a.is_flag(i)))
            .collect();
        assert_eq!(
            rows,
            [
                ("a", "1", false),
                ("b", "x y", false),
                ("c", "", true),
                ("d", "", false),
                ("a", "2", false),
                ("", "e", false)
            ]
        );
        assert_eq!(a.get("a"), Some("1"));
        assert_eq!(a.get("zz"), None);
        assert_eq!(
            a.raw(1).of(" a=1;b = \"x y\";c d=\"\" a=2 \t=e "),
            "\"x y\""
        );
    }

    #[test]
    fn idiom_bits() {
        assert_eq!(classify("plain text 10% off"), Idioms::default());
        assert_eq!(classify("W='@W * @nf '").class(), ValueClass::SpiceQuoted);
        assert!(!classify("'abc' 'd'").intersects(Idioms::SPICE_QUOTED));
        assert!(classify("expr_eng( 70.0e-6 / @w )").contains(Idioms::EXPR_ENG | Idioms::AT_ATTR));
        assert!(!classify("myexpr(1)").intersects(Idioms::EXPR));
        assert!(
            classify("@#1:spice_get_voltage").contains(Idioms::AT_PIN_ATTR | Idioms::AT_SIM_VALUE)
        );
        let corner = classify("tcleval(.lib $::SKYWATER_MODELS/sky130.lib.spice @corner )");
        assert!(corner.contains(Idioms::TCLEVAL | Idioms::TCL_VAR | Idioms::AT_ATTR));
        let std = classify("tcleval([hierarchy_config @symname])");
        assert!(std.contains(Idioms::TCL_HIER_CONFIG | Idioms::AT_SYMNAME));
        let disp = classify("tcleval(gm=[to_eng [ngspice::get_node [subst -nocommand {x}]]])");
        assert!(disp.contains(Idioms::TCL_DISPLAY | Idioms::TCL_NGSPICE));
        assert!(!disp.intersects(Idioms::TCL_OTHER));
        assert!(classify("tcleval([my_proc 1])").contains(Idioms::TCL_OTHER));
        // `$` and `[` outside tcleval are literal.
        assert!(
            !classify("$SUB=@body $[@model\\] [ @@clk ]")
                .intersects(Idioms::TCL_COMMAND | Idioms::TCL_VAR)
        );
        assert!(classify(".control\npre_osdi x.osdi\n.endc").contains(Idioms::PRE_OSDI));
    }

    #[test]
    fn at_token_edges() {
        fn names(s: &str) -> Vec<(AtKind, &str)> {
            at_tokens(s).map(|t| (t.kind, t.name)).collect()
        }
        assert_eq!(names("@ @@ x@"), []);
        assert_eq!(names("@@@a"), [(AtKind::PinNet, "a")]);
        assert_eq!(names("µ@a\\µ"), [(AtKind::Attr, "a")]);
        assert_eq!(
            names("@#0:net_name %lab"),
            [(AtKind::PinAttr, "0:net_name"), (AtKind::Percent, "lab")]
        );
    }
}
