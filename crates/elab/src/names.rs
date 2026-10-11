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
    /// Open addressing, linear probing: 0 empty, else `(hash << 32) | (name + 1)` with the
    /// name's 32-bit hash, which picks the slot and is compared before the text (the table
    /// outgrows the cache at 10^5 names: the text would be a second miss) and lets growth
    /// re-place entries without reading any text. Power-of-two length, load below 1/2.
    table: Vec<u64>,
}

/// A table entry: `(hash, name + 1)`.
#[inline]
fn entry(h: u32, n: Name) -> u64 {
    (u64::from(h) << 32) | u64::from(n.0 + 1)
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
        self.probe(text, hash(text)).ok()
    }

    /// Where `text` (hashing to `h`) is: `Ok(name)`, or `Err(empty slot)` to put it in.
    /// The table is never full (load factor below 1/2), so the probe ends.
    fn probe(&self, text: &str, h: u32) -> Result<Name, usize> {
        let mask = self.table.len() - 1;
        let mut slot = h as usize & mask;
        for _ in 0..self.table.len() {
            let e = self.table[slot];
            if e == 0 {
                return Err(slot);
            }
            let n = Name((e as u32) - 1);
            if (e >> 32) as u32 == h && self.get(n) == text {
                return Ok(n);
            }
            slot = (slot + 1) & mask;
        }
        unreachable!("the table is never full");
    }

    /// Makes room for `additional` more names without growing the table on the way (a
    /// caller about to intern a known volume, e.g. one name per instance).
    pub(crate) fn reserve(&mut self, additional: usize) {
        let want = ((self.ends.len() + additional) * 2).next_power_of_two();
        while self.table.len() < want {
            self.grow();
        }
        self.ends.reserve(additional);
        assert!((self.ends.len() + additional) * 2 <= self.table.len());
    }

    /// Compares two names bytewise by text (the order every sorted output uses).
    /// [`Name::NONE`] sorts first.
    pub fn cmp(&self, a: Name, b: Name) -> Ordering {
        if a == b {
            return Ordering::Equal; // one arena: equal handles are equal text
        }
        match (a == Name::NONE, b == Name::NONE) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => self.get(a).cmp(self.get(b)),
        }
    }

    /// Interns `text`; equal text always yields the same name.
    ///
    /// Cost: one hash and one probe sequence (absent text is placed where the lookup
    /// ended); amortized O(1).
    pub(crate) fn intern(&mut self, text: &str) -> Name {
        if (self.ends.len() + 1) * 2 > self.table.len() {
            self.grow();
        }
        let h = hash(text);
        let slot = match self.probe(text, h) {
            Ok(n) => return n,
            Err(slot) => slot,
        };
        assert!(self.len() < NAMES_MAX, "name arena holds 2^26 names");
        assert!(
            self.bytes.len() + text.len() < u32::MAX as usize,
            "name arena exceeds 4 GiB"
        );
        let n = Name(self.len());
        self.bytes.push_str(text);
        self.ends.push(self.bytes.len() as u32);
        self.table[slot] = entry(h, n);
        debug_assert_eq!(self.get(n), text);
        n
    }

    /// Doubles the table, re-placing entries by their stored hashes (no text is read).
    fn grow(&mut self) {
        let len = (self.table.len() * 2).max(64);
        let old = std::mem::replace(&mut self.table, vec![0; len]);
        let mask = len - 1;
        for e in old.into_iter().filter(|&e| e != 0) {
            let mut slot = (e >> 32) as usize & mask;
            while self.table[slot] != 0 {
                slot = (slot + 1) & mask;
            }
            self.table[slot] = e;
        }
        assert!(self.ends.len() * 2 <= self.table.len() && self.table.len().is_power_of_two());
    }
}

/// Multiply-rotate hash over 8-byte words, folded to 32 bits; deterministic on every target.
fn hash(text: &str) -> u32 {
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
    // The product's high bits are the well-mixed ones.
    ((h ^ (h >> 29)) >> 32) as u32 ^ (h as u32)
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
