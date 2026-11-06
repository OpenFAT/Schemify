//! Contribution sets (§1, D8) and record text: a plugin's `contribute` body in → a
//! validated, keyed set row; a `spawn` argv line in → its values. Sets and argv are lexed by
//! `schemify-format` (format spec §2, the same reader as documents), so a set the host
//! accepts is exactly what the format accepts. Cites plugin-abi §1 (kinds, record words,
//! keys), §5 (`spawn` argv), §7 (`contribute`), D20 (`function` records).
//!
//! Contents:
//! - `pub struct Contributions`: `new`, `len`, `is_empty`, `epoch`, `get`, `rows`,
//!   `remove_plugin`; `pub(crate) fn set`
//! - `pub struct ContributionRow<'table>`
//! - `pub(crate) struct SetRecord<'body>`
//! - `pub(crate) struct FunctionRow`
//! - `pub(crate) fn set_records(kind: Kind, body: &str) -> Result<Vec<SetRecord<'_>>, Code>`
//! - `pub(crate) fn function_row(document: &FormatDoc<'_>, record: u32) -> Result<FunctionRow, Code>`
//! - `pub(crate) fn lex<T>(body: &str, read: impl FnOnce(&FormatDoc<'_>) -> T) -> Result<T, Code>`
//! - `pub(crate) fn line_values(line: &str) -> Result<Vec<String>, Code>`
//! - `pub(crate) fn truncate(s: &str, max: usize) -> &str`

use schemify_abi::{Code, Kind, text};
use schemify_format::Document as FormatDoc;

use crate::{CONTRIBUTIONS_MAX, PluginId};

/// Every plugin's current contribution set per (kind, scope); an empty set has no row
/// (existence-based). Columns in insertion order; capacity [`CONTRIBUTIONS_MAX`] rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contributions {
    plugin: Vec<PluginId>,
    kind: Vec<Kind>,
    scope: Vec<Box<str>>,
    body: Vec<Box<str>>,
    epoch: u32,
}

/// One row of [`Contributions`], borrowed from the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContributionRow<'table> {
    /// The contributing plugin.
    pub plugin: PluginId,
    /// Contribution kind.
    pub kind: Kind,
    /// Annotation scope; empty for other kinds.
    pub scope: &'table str,
    /// The set's line-record text, never empty.
    pub body: &'table str,
}

/// One top-level record of a contribution set: its composition key and its full text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetRecord<'body> {
    /// Record word and first value (`set` adds the property; `remove X` is `node X`).
    pub key: String,
    /// The record line plus its indented lines.
    pub text: &'body str,
}

/// The values of a `function <name: word> <fn_id: u32> <arity: u8>` record (D20).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FunctionRow {
    pub name: Box<str>,
    pub fn_id: u32,
    pub arity: u8,
}

/// What sets and argv lines are lexed as: a lockfile of a newer format version, whose only
/// known word (`locked`) no plugin record uses, so every record is kept as an unknown record
/// with its fields unescaped (the `doc` journal does the same).
const LEX_HEADER: &str = "schemify 65535 lock\n";

impl Contributions {
    /// Returns an empty table.
    #[must_use]
    pub fn new() -> Contributions {
        Contributions::default()
    }

    /// Returns the number of non-empty sets.
    #[must_use]
    pub fn len(&self) -> usize {
        debug_assert_eq!(self.plugin.len(), self.kind.len());
        debug_assert_eq!(self.plugin.len(), self.scope.len());
        debug_assert_eq!(self.plugin.len(), self.body.len());
        self.plugin.len()
    }

    /// Returns whether no plugin contributes anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a value that changes (wrapping) whenever any set changes.
    #[must_use]
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Returns the current set of `plugin` for (`kind`, `scope`), or `None` when empty.
    #[must_use]
    pub fn get(&self, plugin: PluginId, kind: Kind, scope: &str) -> Option<&str> {
        let body = self.find(plugin, kind, scope).map(|i| &*self.body[i]);
        debug_assert!(
            body.is_none_or(|body| !body.is_empty()),
            "an empty set has no row"
        );
        body
    }

    /// Iterates every row, in insertion order.
    pub fn rows(&self) -> impl Iterator<Item = ContributionRow<'_>> {
        (0..self.len()).map(|i| ContributionRow {
            plugin: self.plugin[i],
            kind: self.kind[i],
            scope: &self.scope[i],
            body: &self.body[i],
        })
    }

    /// Drops every set of `plugin` (unload); advances the epoch if it had any.
    pub fn remove_plugin(&mut self, plugin: PluginId) {
        let before = self.len();
        for i in (0..before).rev() {
            if self.plugin[i] == plugin {
                self.remove(i);
            }
        }
        if self.len() != before {
            self.epoch = self.epoch.wrapping_add(1);
        }
        debug_assert!(self.plugin.iter().all(|&owner| owner != plugin));
    }

    /// Replaces a set (an identical body is no change); an empty `body` removes the row.
    /// The caller validated `body` with [`set_records`].
    ///
    /// # Errors
    ///
    /// `E_SPACE` (table unchanged) for a new row at [`CONTRIBUTIONS_MAX`].
    pub(crate) fn set(
        &mut self,
        plugin: PluginId,
        kind: Kind,
        scope: &str,
        body: &str,
    ) -> Result<(), Code> {
        debug_assert!(kind.scoped() || scope.is_empty());
        match (self.find(plugin, kind, scope), body.is_empty()) {
            (Some(i), true) => self.remove(i),
            (Some(i), false) if *self.body[i] == *body => return Ok(()),
            (Some(i), false) => self.body[i] = body.into(),
            (None, true) => return Ok(()),
            (None, false) if self.len() >= CONTRIBUTIONS_MAX => return Err(Code::Space),
            (None, false) => {
                self.plugin.push(plugin);
                self.kind.push(kind);
                self.scope.push(scope.into());
                self.body.push(body.into());
            }
        }
        self.epoch = self.epoch.wrapping_add(1);
        debug_assert!(self.len() <= CONTRIBUTIONS_MAX);
        debug_assert_eq!(
            self.get(plugin, kind, scope),
            (!body.is_empty()).then_some(body)
        );
        Ok(())
    }
}

/// Splits a contribution body into keyed records, validating it against the kind: legal
/// record words only, well-formed values and attribute lines, and for
/// [`Kind::Functions`] the exact form `function <name: word> <fn_id: u32> <arity: u8>`.
/// [`text::chunks`] only cuts the body into each record's own text.
///
/// # Errors
///
/// `E_PARSE` on the first violation.
pub(crate) fn set_records(kind: Kind, body: &str) -> Result<Vec<SetRecord<'_>>, Code> {
    let chunks: Vec<&str> = text::chunks(body.as_bytes())
        .map(|c| std::str::from_utf8(c).expect("chunks of a str split at LF stay UTF-8"))
        .collect();
    let keys = lex(body, |document| -> Result<Vec<String>, Code> {
        // An indented line before the first record attaches to the header: an orphan.
        let orphan = !document.attrs(0).is_empty() || document.verbatim(0).next().is_some();
        if orphan || document.record_count() as usize != chunks.len() + 1 {
            return Err(Code::Parse);
        }
        (1..document.record_count())
            .map(|record| record_key(kind, document, record))
            .collect()
    })??;
    debug_assert_eq!(keys.len(), chunks.len());
    let out: Vec<SetRecord<'_>> = keys
        .into_iter()
        .zip(chunks)
        .map(|(key, text)| SetRecord { key, text })
        .collect();
    debug_assert!(out.iter().all(|r| !r.key.is_empty()));
    Ok(out)
}

/// The values of `record`, a `function <name: word> <fn_id: u32> <arity: u8>`.
///
/// # Errors
///
/// `E_PARSE` for any other shape.
pub(crate) fn function_row(document: &FormatDoc<'_>, record: u32) -> Result<FunctionRow, Code> {
    debug_assert!(record > 0, "record 0 is the lex header");
    debug_assert!(record < document.record_count());
    let name = document
        .field(record, 0)
        .filter(|name| text::is_word(name))
        .ok_or(Code::Parse)?;
    let fn_id: u32 = document
        .field(record, 1)
        .and_then(|field| field.parse().ok())
        .ok_or(Code::Parse)?;
    let arity: u8 = document
        .field(record, 2)
        .and_then(|field| field.parse().ok())
        .ok_or(Code::Parse)?;
    if document.field(record, 3).is_some() {
        return Err(Code::Parse);
    }
    Ok(FunctionRow {
        name: Box::from(name),
        fn_id,
        arity,
    })
}

/// Lexes `body` with the format reader and hands the document to `read`; record 0 is the
/// header, the body's records start at 1.
///
/// # Errors
///
/// `E_PARSE` when the format reader rejects the text.
pub(crate) fn lex<T>(body: &str, read: impl FnOnce(&FormatDoc<'_>) -> T) -> Result<T, Code> {
    let source = format!("{LEX_HEADER}{body}");
    let document = schemify_format::parse(source.as_bytes()).map_err(|_| Code::Parse)?;
    assert!(
        document.version() > schemify_format::VERSION,
        "forward-compatible mode"
    );
    assert!(document.record_count() >= 1, "the header is record 0");
    Ok(read(&document))
}

/// The values of one line (a `spawn` argv), lexed by the format reader.
///
/// # Errors
///
/// `E_PARSE` for a malformed value or a line break (the text must be exactly one line).
pub(crate) fn line_values(line: &str) -> Result<Vec<String>, Code> {
    if line.contains(['\n', '\r']) {
        return Err(Code::Parse);
    }
    let source = format!("argv {line}");
    let values = lex(&source, |document| match document.record_count() {
        2 => Ok(document.fields(1).map(str::to_owned).collect()),
        _ => Err(Code::Parse),
    })?;
    // Negative space: no text, no values.
    debug_assert!(!line.is_empty() || values.as_ref().is_ok_and(Vec::is_empty));
    values
}

/// Returns the longest prefix of `s` of at most `max` bytes that ends on a char boundary.
pub(crate) fn truncate(s: &str, max: usize) -> &str {
    let end = s.floor_char_boundary(max);
    debug_assert!(end <= max);
    debug_assert!(
        end == s.len() || max - end < 4,
        "cuts at most one partial character"
    );
    &s[..end]
}

// ---- private ----

impl Contributions {
    fn find(&self, plugin: PluginId, kind: Kind, scope: &str) -> Option<usize> {
        let matches =
            |i: usize| self.plugin[i] == plugin && self.kind[i] == kind && *self.scope[i] == *scope;
        let found = (0..self.len()).find(|&i| matches(i));
        debug_assert!(
            found.is_none_or(|i| !(i + 1..self.len()).any(matches)),
            "one row per (plugin, kind, scope)"
        );
        found
    }

    fn remove(&mut self, i: usize) {
        let rows = self.len();
        assert!(i < rows);
        self.plugin.remove(i);
        self.kind.remove(i);
        self.scope.remove(i);
        self.body.remove(i);
        debug_assert_eq!(self.len(), rows - 1);
    }
}

/// The composition key of record `r` of a lexed set, after checking its word against
/// `kind` and the exact `function` form.
fn record_key(kind: Kind, document: &FormatDoc<'_>, record: u32) -> Result<String, Code> {
    let word = document.record_word(record);
    if !kind.words().contains(&word) {
        return Err(Code::Parse);
    }
    // `remove X` shares `node X`'s key (D8).
    let base = if word == "remove" { "node" } else { word };
    let first = document.field(record, 0).ok_or(Code::Parse)?;
    let key = if word == "set" {
        // `set <node> <prop> <value>` is keyed by two values.
        let property = document.field(record, 1).ok_or(Code::Parse)?;
        format!("{base} {first} {property}")
    } else {
        format!("{base} {first}")
    };
    if kind == Kind::Functions {
        function_row(document, record)?;
    }
    debug_assert!(key.starts_with(base));
    debug_assert!(key.len() > base.len());
    Ok(key)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    #[test]
    fn set_records_key_and_validate() {
        let body =
            "node a item\n  parent menu\nremove b\nset c label \"x y\"\nstyle s\n  color red";
        let records = set_records(Kind::Ui, body).unwrap();
        let keys: Vec<&str> = records.iter().map(|record| record.key.as_str()).collect();
        assert_eq!(keys, ["node a", "node b", "set c label", "style s"]);
        assert_eq!(records[0].text, "node a item\n  parent menu");
        assert_eq!(set_records(Kind::Ui, "command x y"), Err(Code::Parse));
        assert_eq!(set_records(Kind::Ui, "node"), Err(Code::Parse));
        assert_eq!(set_records(Kind::Ui, "  orphan x"), Err(Code::Parse));
        assert_eq!(
            set_records(Kind::Ui, "node a\n  attr \"open"),
            Err(Code::Parse)
        );
        assert!(set_records(Kind::Functions, "function gmid.width 0 3").is_ok());
        assert!(set_records(Kind::Functions, "function \"g.w\" 1 2\n  doc x").is_ok());
        for bad in [
            "function f",
            "function f x 3",
            "function f 0 300",
            "function f 0 3 extra",
            "function \"a b\" 0 1",
        ] {
            assert_eq!(set_records(Kind::Functions, bad), Err(Code::Parse), "{bad}");
        }
        assert_eq!(set_records(Kind::Commands, ""), Ok(Vec::new()));
        let blank = set_records(Kind::Ui, "node a x\n\n# c\nnode b y\n").unwrap();
        assert_eq!(blank.len(), 2);
        assert_eq!(set_records(Kind::Ui, "| x"), Err(Code::Parse));
    }

    #[test]
    fn spawn_argv_lexes_one_line() {
        let argv = line_values(r#"ngspice -b "a b.cir""#).unwrap();
        assert_eq!(argv, ["ngspice", "-b", "a b.cir"]);
        assert_eq!(line_values(""), Ok(Vec::new()));
        for bad in ["a\nb", "a\r", "\"open", "#x", "a\"b"] {
            assert_eq!(line_values(bad), Err(Code::Parse), "{bad:?}");
        }
    }

    proptest::proptest! {
        /// The guests' lexer (`abi::text`) and the format reader the host uses agree on
        /// every line: the same values, or both reject it.
        #[test]
        fn abi_and_format_lexers_agree(line in "[ a-z0-9\"\\\\#|:.{}u\\-]{0,24}") {
            let abi: Result<Vec<String>, Code> = text::values(&line)
                .map(|value| value.map(Cow::into_owned))
                .collect();
            proptest::prop_assert_eq!(line_values(&line), abi);
        }
    }

    #[test]
    fn contributions_replace_and_clear() {
        let mut table = Contributions::new();
        let plugin = PluginId(1);
        table.set(plugin, Kind::Ui, "", "node a x").unwrap();
        let epoch = table.epoch();
        table.set(plugin, Kind::Ui, "", "node a x").unwrap();
        assert_eq!(table.epoch(), epoch, "identical set is not a change");
        table.set(plugin, Kind::Ui, "", "node b x").unwrap();
        assert_eq!(table.get(plugin, Kind::Ui, ""), Some("node b x"));
        table
            .set(plugin, Kind::Annotations, "erc", "mark a info b")
            .unwrap();
        assert_eq!(table.len(), 2);
        table.set(plugin, Kind::Ui, "", "").unwrap();
        assert_eq!(table.get(plugin, Kind::Ui, ""), None);
        let epoch = table.epoch();
        table.remove_plugin(PluginId(2));
        assert_eq!(table.epoch(), epoch, "removing nothing is not a change");
        table.remove_plugin(plugin);
        assert!(table.is_empty());
        assert_ne!(table.epoch(), epoch);
    }

    #[test]
    fn contributions_are_bounded() {
        let mut table = Contributions::new();
        for i in 0..CONTRIBUTIONS_MAX {
            table
                .set(
                    PluginId(1),
                    Kind::Annotations,
                    &format!("s{i}"),
                    "mark a info b",
                )
                .unwrap();
        }
        let full = table.set(PluginId(1), Kind::Annotations, "over", "mark a info b");
        assert_eq!(full, Err(Code::Space));
        assert_eq!(table.len(), CONTRIBUTIONS_MAX);
        assert_eq!(table.get(PluginId(1), Kind::Annotations, "over"), None);
    }

    #[test]
    fn truncate_keeps_char_boundaries() {
        assert_eq!(truncate("héllo", 2), "h");
        assert_eq!(truncate("héllo", 3), "hé");
        assert_eq!(truncate("ab", 9), "ab");
    }
}
