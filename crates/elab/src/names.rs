//! The elaboration string arena: every name elab derives (net and bit names, instance
//! element names, cell names, parameter value text) is a [`Name`] into one [`Names`].
//!
//! `doc` interns per document; elab output spans many documents, so it owns one arena
//! per output ([`crate::Elab`], [`crate::CellState`]). Same shape as `doc`'s arena: one
//! byte buffer plus an open-addressing table with a deterministic hash (no random
//! state, so it behaves the same on `wasm32-unknown-unknown`).

use std::cmp::Ordering;

/// Handle of a string in a [`Names`] arena; valid only for the arena that made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(pub u32);

impl Name {
    /// "No name" in optional columns.
    pub const NONE: Name = Name(u32::MAX);
}

impl Default for Name {
    /// [`Name::NONE`].
    fn default() -> Name {
        Name::NONE
    }
}

/// Most strings one arena holds (2^26: a 1M-instance flat cell interns ~4 per instance).
pub const NAMES_MAX: u32 = 1 << 26;

/// Interned strings: `ends[i]` is where name `i` ends in `bytes`.
#[derive(Clone, Debug, Default)]
pub struct Names {
    bytes: String,
    ends: Vec<u32>,
    /// Open addressing, linear probing: 0 empty, else `name + 1`. Power-of-two length,
    /// load factor below 1/2.
    table: Vec<u32>,
}

impl Names {
    /// Returns the number of interned names.
    pub fn len(&self) -> u32 {
        self.ends.len() as u32
    }

    /// Returns whether nothing is interned.
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Returns the text of `n`.
    ///
    /// # Panics
    ///
    /// Panics on [`Name::NONE`] or a name from another arena that is out of range.
    pub fn get(&self, n: Name) -> &str {
        let i = n.0 as usize;
        assert!(i < self.ends.len(), "name from another arena or NONE");
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        &self.bytes[start..self.ends[i] as usize]
    }

    /// Returns the name of `text` without interning it.
    pub fn find(&self, text: &str) -> Option<Name> {
        if self.table.is_empty() {
            return None;
        }
        let mask = self.table.len() - 1;
        let mut slot = hash(text) as usize & mask;
        for _ in 0..self.table.len() {
            let e = self.table[slot];
            if e == 0 {
                return None;
            }
            if self.get(Name(e - 1)) == text {
                return Some(Name(e - 1));
            }
            slot = (slot + 1) & mask;
        }
        unreachable!("the table is never full");
    }

    /// Compares two names bytewise by text (the order every sorted output uses).
    /// [`Name::NONE`] sorts first.
    pub fn cmp(&self, a: Name, b: Name) -> Ordering {
        match (a == Name::NONE, b == Name::NONE) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => self.get(a).cmp(self.get(b)),
        }
    }

    /// Interns `text`; equal text always yields the same name.
    pub(crate) fn intern(&mut self, text: &str) -> Name {
        if let Some(n) = self.find(text) {
            return n;
        }
        assert!(self.len() < NAMES_MAX, "name arena holds 2^26 names");
        assert!(
            self.bytes.len() + text.len() < u32::MAX as usize,
            "name arena exceeds 4 GiB"
        );
        if (self.ends.len() + 1) * 2 > self.table.len() {
            self.grow();
        }
        let n = Name(self.len());
        self.bytes.push_str(text);
        self.ends.push(self.bytes.len() as u32);
        self.insert_slot(n);
        debug_assert_eq!(self.get(n), text);
        n
    }

    fn insert_slot(&mut self, n: Name) {
        assert!(self.table.len().is_power_of_two() && (n.0 as usize) < self.table.len() / 2);
        let mask = self.table.len() - 1;
        let mut slot = hash(self.get(n)) as usize & mask;
        while self.table[slot] != 0 {
            slot = (slot + 1) & mask;
        }
        self.table[slot] = n.0 + 1;
    }

    fn grow(&mut self) {
        let len = (self.table.len() * 2).max(64);
        self.table = vec![0; len];
        for i in 0..self.len() {
            self.insert_slot(Name(i));
        }
        assert!(self.ends.len() * 2 <= self.table.len());
    }
}

/// Multiply-rotate hash over 8-byte words; deterministic on every target.
fn hash(text: &str) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let b = text.as_bytes();
    let mut h = b.len() as u64;
    let mut chunks = b.chunks_exact(8);
    for c in &mut chunks {
        let w = u64::from_le_bytes(c.try_into().expect("8-byte chunk"));
        h = (h.rotate_left(5) ^ w).wrapping_mul(K);
    }
    let mut tail = [0u8; 8];
    tail[..chunks.remainder().len()].copy_from_slice(chunks.remainder());
    h = (h.rotate_left(5) ^ u64::from_le_bytes(tail)).wrapping_mul(K);
    h ^ (h >> 29)
}

/// FNV-1a 64 (netlist spec §6 specialization names).
pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Appends the first `chars` base32 digits of `h`, most significant first, in the
/// Crockford lowercase alphabet the format uses for ids.
pub(crate) fn push_base32(h: u64, chars: u32, out: &mut String) {
    assert!((1..=13).contains(&chars));
    for i in 0..chars {
        let shift = 64i32 - 5 * (i as i32 + 1);
        let digit = if shift >= 0 {
            (h >> shift) & 31
        } else {
            (h << -shift) & 31
        };
        out.push(char::from(schemify_format::ID_ALPHABET[digit as usize]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_dedups_and_survives_growth() {
        let mut n = Names::default();
        let ids: Vec<Name> = (0..2000).map(|i| n.intern(&format!("n{i}"))).collect();
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(n.get(*id), format!("n{i}"));
            assert_eq!(n.intern(&format!("n{i}")), *id);
        }
        assert_eq!(n.find("absent"), None);
        assert_eq!(n.cmp(ids[1], ids[10]), "n1".cmp("n10"));
        assert_eq!(n.cmp(Name::NONE, ids[0]), Ordering::Less);
    }

    #[test]
    fn fnv_and_base32_are_the_reference_values() {
        // FNV-1a 64 of "" and "a" are published constants.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        let mut s = String::new();
        push_base32(u64::MAX, 6, &mut s);
        assert_eq!(s, "zzzzzz");
        s.clear();
        push_base32(0, 13, &mut s);
        assert_eq!(s, "0000000000000");
        s.clear();
        push_base32(1 << 59, 2, &mut s);
        assert_eq!(s, "10");
    }
}
