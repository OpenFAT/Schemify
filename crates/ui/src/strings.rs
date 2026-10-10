//! Session string interner: every id, label and command name in the UI tables is a [`Sym`].

use std::collections::HashMap;

/// Handle to an interned string, stable for the life of its [`Strings`].
///
/// `Sym::NONE` is the empty string and doubles as "absent" in every column that holds a `Sym`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct Sym(pub(crate) u32);

impl Sym {
    /// The empty string; also "no value".
    pub const NONE: Sym = Sym(0);

    /// True for [`Sym::NONE`].
    pub fn is_none(self) -> bool {
        self.0 == 0
    }

    /// Raw handle value; stable for the session, so it is used as the AccessKit node id.
    pub fn raw(self) -> u32 {
        self.0
    }
}

// ponytail: append-only, never collected. Fine for ids, labels and commands; text that changes
// every frame (cursor coordinates in a status field) must not be interned, or the session grows
// until CAPACITY. Add a per-frame "live text" side table when the status bar needs it.

/// Append-only interner. Handles never move or die, so tables may store them freely.
///
/// Capacity: [`Strings::CAPACITY`] strings (session lifetime; plugin churn re-interns the same
/// names, so growth is bounded by the distinct names ever seen).
#[derive(Debug)]
pub struct Strings {
    names: Vec<Box<str>>,
    lookup: HashMap<Box<str>, Sym>,
}

impl Default for Strings {
    fn default() -> Self {
        Self::new()
    }
}

impl Strings {
    /// Maximum number of distinct strings in one session.
    pub const CAPACITY: usize = 1 << 20;

    /// Creates an interner holding only the empty string at [`Sym::NONE`].
    pub fn new() -> Self {
        let mut s = Strings {
            names: Vec::with_capacity(1024),
            lookup: HashMap::with_capacity(1024),
        };
        s.names.push("".into());
        s.lookup.insert("".into(), Sym::NONE);
        s
    }

    /// Returns the handle for `s`, interning it on first sight.
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
        sym
    }

    /// Returns the handle for `s` without interning it.
    pub fn find(&self, s: &str) -> Option<Sym> {
        self.lookup.get(s).copied()
    }

    /// Returns the text of `sym`.
    ///
    /// # Panics
    ///
    /// Panics if `sym` came from a different interner and is out of range.
    pub fn get(&self, sym: Sym) -> &str {
        &self.names[sym.0 as usize]
    }

    /// Number of interned strings, including the empty string.
    pub fn len(&self) -> usize {
        self.names.len()
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
}
