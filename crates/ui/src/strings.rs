//! Session string interner: every id, label and command name in the UI tables is a [`Sym`].
//!
//! Two regions. The **permanent** region is append-only: ids, commands, style classes and every
//! string of a plugin contribution, interned once per session. The **live** region holds the
//! free text of the app's state contribution (labels and values that change every frame, such
//! as the pointer coordinates in the status line): each [`crate::records::parse_live`] call is
//! one generation, a string still used keeps its handle, and the strings the previous
//! generation used but this one does not are freed. So text that changes every frame no longer
//! grows the table.

use std::collections::HashMap;

/// Handle to an interned string.
///
/// `Sym::NONE` is the empty string and doubles as "absent" in every column that holds a `Sym`.
/// A permanent handle is stable for the life of its [`Strings`]; a live one until the
/// generation after the last one that used its text ([`Strings::end_live`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct Sym(pub(crate) u32);

/// Handle bit of the live region. Permanent handles stay below [`Strings::CAPACITY`].
const LIVE: u32 = 1 << 31;

impl Sym {
    /// The empty string; also "no value".
    pub const NONE: Sym = Sym(0);

    /// True for [`Sym::NONE`].
    pub fn is_none(self) -> bool {
        self.0 == 0
    }

    /// Raw handle value; stable for the session for permanent strings (node ids), so it is
    /// used as the AccessKit node id.
    pub fn raw(self) -> u32 {
        self.0
    }

    /// True for a handle of the live region (state-contribution text).
    pub fn is_live(self) -> bool {
        self.0 & LIVE != 0
    }
}

/// The interner: an append-only permanent region plus a generational live region.
///
/// Capacity: [`Strings::CAPACITY`] permanent strings (session lifetime; plugin churn re-interns
/// the same names, so growth is bounded by the distinct names ever seen) and
/// [`Strings::LIVE_CAPACITY`] live strings per generation.
#[derive(Debug)]
pub struct Strings {
    names: Vec<Box<str>>,
    lookup: HashMap<Box<str>, Sym>,
    /// Live strings of the generation being built (or the last one built): text → handle.
    live: HashMap<Box<str>, Sym>,
    /// The previous generation's strings not (yet) reused by the current one.
    live_old: HashMap<Box<str>, Sym>,
    /// Handle → text for every live string of the current and the open previous generation.
    live_text: HashMap<u32, Box<str>>,
    /// Next live index to try (31 bits, wraps).
    live_next: u32,
}

impl Default for Strings {
    fn default() -> Self {
        Self::new()
    }
}

impl Strings {
    /// Maximum number of distinct permanent strings in one session.
    pub const CAPACITY: usize = 1 << 20;
    /// Maximum number of distinct live strings in one generation.
    pub const LIVE_CAPACITY: usize = 1 << 20;

    /// Creates an interner holding only the empty string at [`Sym::NONE`].
    pub fn new() -> Self {
        let mut s = Strings {
            names: Vec::with_capacity(1024),
            lookup: HashMap::with_capacity(1024),
            live: HashMap::new(),
            live_old: HashMap::new(),
            live_text: HashMap::new(),
            live_next: 0,
        };
        s.names.push("".into());
        s.lookup.insert("".into(), Sym::NONE);
        s
    }

    /// Returns the permanent handle for `s`, interning it on first sight.
    ///
    /// # Panics
    ///
    /// Panics when more than [`Strings::CAPACITY`] distinct strings are interned.
    pub fn intern(&mut self, s: &str) -> Sym {
        if let Some(&sym) = self.lookup.get(s) {
            return sym;
        }
        assert!(self.names.len() < Self::CAPACITY, "string interner full");
        let sym = Sym(self.names.len() as u32);
        self.names.push(s.into());
        self.lookup.insert(s.into(), sym);
        debug_assert_eq!(self.get(sym), s);
        debug_assert!(!sym.is_live());
        sym
    }

    /// Opens a live generation: until [`Strings::end_live`], [`Strings::intern_live`] reuses
    /// the handles of the previous generation's strings and allocates new ones for new text.
    pub fn begin_live(&mut self) {
        assert!(self.live_old.is_empty(), "live generation already open");
        self.live_old = std::mem::take(&mut self.live);
        debug_assert!(self.live.is_empty());
    }

    /// Returns a handle for `s` in the current live generation: the permanent handle when `s`
    /// is permanent, else its live handle (the previous generation's when it had `s`).
    ///
    /// # Panics
    ///
    /// Panics when one generation holds more than [`Strings::LIVE_CAPACITY`] strings.
    pub fn intern_live(&mut self, s: &str) -> Sym {
        if let Some(&sym) = self.lookup.get(s) {
            return sym;
        }
        if let Some(&sym) = self.live.get(s) {
            return sym;
        }
        if let Some((text, sym)) = self.live_old.remove_entry(s) {
            self.live.insert(text, sym);
            return sym;
        }
        assert!(
            self.live.len() < Self::LIVE_CAPACITY,
            "live string generation full"
        );
        // ponytail: indices are never reused until the 31-bit counter wraps (2^31 new live
        // strings), so a shaped-text cache keyed by handle never sees a recycled one; a
        // wrapped index skips handles still alive. A generation tag in the cache key if a
        // session ever gets near the wrap.
        let mut idx = self.live_next;
        // Bounded: at most `live_text.len()` indices are taken.
        for _ in 0..=self.live_text.len() {
            if !self.live_text.contains_key(&(LIVE | idx)) {
                break;
            }
            idx = (idx + 1) & !LIVE;
        }
        self.live_next = (idx + 1) & !LIVE;
        let sym = Sym(LIVE | idx);
        assert!(!self.live_text.contains_key(&sym.0), "live handle in use");
        self.live.insert(s.into(), sym);
        self.live_text.insert(sym.0, s.into());
        debug_assert!(sym.is_live() && !sym.is_none());
        sym
    }

    /// Closes the live generation opened by [`Strings::begin_live`]: the previous
    /// generation's strings it did not reuse are freed, and their handles die.
    pub fn end_live(&mut self) {
        // Iteration order does not matter: every entry is removed.
        for (_, sym) in self.live_old.drain() {
            self.live_text.remove(&sym.0);
        }
        assert_eq!(self.live.len(), self.live_text.len(), "live tables agree");
    }

    /// Returns the handle for `s` without interning it (permanent, or live in the current
    /// generation).
    pub fn find(&self, s: &str) -> Option<Sym> {
        self.lookup.get(s).or_else(|| self.live.get(s)).copied()
    }

    /// Returns the text of `sym`; a dead live handle reads as the empty string (and fails a
    /// debug assertion: tables must be recomposed after every live generation).
    ///
    /// # Panics
    ///
    /// Panics if a permanent `sym` came from a different interner and is out of range.
    pub fn get(&self, sym: Sym) -> &str {
        if sym.is_live() {
            let text = self.live_text.get(&sym.0);
            debug_assert!(text.is_some(), "dead live string handle");
            return text.map_or("", |t| t.as_ref());
        }
        &self.names[sym.0 as usize]
    }

    /// Number of permanent strings, including the empty string.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Number of live strings held (the current generation's).
    pub fn live_len(&self) -> usize {
        self.live_text.len()
    }

    /// Always false: the empty string is interned at construction.
    pub fn is_empty(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_is_idempotent_and_empty_is_none() {
        let mut s = Strings::new();
        assert_eq!(s.intern(""), Sym::NONE);
        let a = s.intern("file.save");
        assert_eq!(s.intern("file.save"), a);
        assert_ne!(s.intern("file.open"), a);
        assert_eq!(s.get(a), "file.save");
        assert_eq!(s.find("nope"), None);
        assert_eq!(s.len(), 3);
    }

    /// Live generations: kept text keeps its handle, changed text frees the old one, permanent
    /// text never goes live, and nothing grows across 10k generations of changing text.
    #[test]
    fn live_generations_reuse_and_free() {
        let mut s = Strings::new();
        let save = s.intern("Save");
        s.begin_live();
        let tab = s.intern_live("top.shcomp");
        let msg = s.intern_live("mouse = 0 0");
        assert_eq!(s.intern_live("Save"), save, "permanent wins");
        assert_eq!(s.intern_live("top.shcomp"), tab, "deduplicated");
        s.end_live();
        assert!(tab.is_live() && msg.is_live() && !save.is_live());
        assert_eq!((s.get(tab), s.get(msg)), ("top.shcomp", "mouse = 0 0"));
        for i in 0..10_000 {
            s.begin_live();
            assert_eq!(
                s.intern_live("top.shcomp"),
                tab,
                "kept text keeps its handle"
            );
            let m = s.intern_live(&format!("mouse = {i} {i}"));
            assert_ne!(m, msg);
            s.end_live();
            assert_eq!(s.get(m), format!("mouse = {i} {i}"));
            assert_eq!(s.live_len(), 2, "dropped text is freed");
        }
        assert_eq!(s.len(), 2, "the permanent region did not grow");
        assert_eq!(s.find("top.shcomp"), Some(tab));
        assert_eq!(s.find("mouse = 0 0"), None, "freed");
    }
}
