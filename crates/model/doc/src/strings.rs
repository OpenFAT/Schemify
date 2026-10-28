//! Record texts → `Sym` handles (one arena per document), and record id text → `ObjId`.
//! Cites: D33 (ids and coordinates as narrow integers), format spec §3 (id alphabet).
//!
//! Six questions:
//! 1. In: record texts (names, cell references, values, template strings). Out: `Sym`
//!    handles every table column stores instead of text.
//! 2. One arena per document; a 100k-object cell interns ~100k distinct strings.
//! 3. `Sym` is `u32` (capacity 2^24 strings, 2^31 bytes). `ObjId` is `u32`: 30 bits for
//!    the 6-char ids the writer generates, bit 31 for any other id length.
//! 4. Interning is a hash probe on load and edit; reads are `ends[sym]` slices.
//! 5. Lifetime: the document. Strings are never freed; what edits intern is counted
//!    (garbage once its row is replaced) and `Document::compact` rebuilds.
//! 6. Not parallel: one writer (`doc`), many readers through `&Strings`.
//!
//! Contents:
//! - `pub struct Sym`, `Sym::NONE`, `pub(crate) const SYMS_MAX`
//! - `pub struct Strings`: `len`, `is_empty`, `get`, `find`; crate: `intern`, `settle`,
//!   `outgrown`
//! - `pub struct ObjId`: `NONE`, `decode`, `find`, `is_alias`, `write`, `text`; crate:
//!   `intern`
//! - `pub(crate) struct MixHasher`, `pub(crate) type FastMap`

use schemify_format::{ID_ALPHABET, ID_LEN_NEW};
use std::hash::{BuildHasherDefault, Hasher};

/// Handle of an interned string; valid only for the arena (document) that interned it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sym(pub u32);

/// Most strings one document interns.
pub(crate) const SYMS_MAX: u32 = 1 << 24;

/// Interned strings of one document: one byte arena plus an open-addressing table.
#[derive(Clone, Debug, Default)]
pub struct Strings {
    bytes: String,
    /// End offset of symbol `i`; it starts where symbol `i - 1` ends.
    ends: Vec<u32>,
    /// Open addressing, linear probing: 0 is empty, else `sym + 1`. Power-of-two length.
    table: Vec<u32>,
    /// `(strings, bytes)` at the last load or rebuild: everything after is edit output.
    settled: (u32, u32),
}

/// A record id as stored in tables.
///
/// A 6-char id (every id this crate or the importer generates) is its base32 value,
/// most significant digit first, so it needs no arena, survives reloads, and orders
/// like its text. Ids of any other length (hand-written `0abc`) are an alias bit plus a
/// [`Sym`] of their text: valid only for the document that interned them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjId(pub u32);

/// Hasher for maps keyed by values that are already well mixed (random object ids).
/// Deterministic; no random state, so it also works on `wasm32-unknown-unknown`.
#[derive(Default)]
pub(crate) struct MixHasher(u64);

/// `HashMap` with [`MixHasher`]; never iterated on an output path.
pub(crate) type FastMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<MixHasher>>;

/// Largest arena in bytes.
const BYTES_MAX: usize = 1 << 31;
/// Bit 31 of an [`ObjId`]: the low bits are a [`Sym`] of the id text.
const ALIAS: u32 = 1 << 31;
const NOT_DIGIT: u8 = u8::MAX;
/// Byte → Crockford digit value, built from [`ID_ALPHABET`] at compile time.
const DIGIT: [u8; 256] = {
    let mut t = [NOT_DIGIT; 256];
    let mut i = 0;
    while i < 32 {
        t[ID_ALPHABET[i] as usize] = i as u8;
        i += 1;
    }
    t
};

impl Sym {
    /// "No string" in optional columns (an arc's template, a stext's angles).
    pub const NONE: Sym = Sym(u32::MAX);
}

impl Strings {
    /// Returns the number of interned strings.
    #[must_use]
    pub fn len(&self) -> u32 {
        self.ends.len() as u32
    }

    /// Returns whether nothing is interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Returns the text of `sym`.
    ///
    /// # Panics
    ///
    /// Panics if `sym` was not interned by this arena (including [`Sym::NONE`]).
    #[must_use]
    pub fn get(&self, sym: Sym) -> &str {
        let i = sym.0 as usize;
        assert!(i < self.ends.len(), "symbol from another arena or NONE");
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        &self.bytes[start..self.ends[i] as usize]
    }

    /// Returns the symbol of `text` if it was interned, without interning it.
    /// Cost: one hash and a short probe.
    #[must_use]
    pub fn find(&self, text: &str) -> Option<Sym> {
        if self.table.is_empty() {
            return None;
        }
        self.probe(text).ok()
    }

    /// Interns `text`, returning the existing symbol when already present. Cost: one
    /// hash and a short probe.
    ///
    /// # Panics
    ///
    /// Panics past [`SYMS_MAX`] strings or 2 GiB of text.
    pub(crate) fn intern(&mut self, text: &str) -> Sym {
        if (self.ends.len() + 1) * 2 > self.table.len() {
            self.grow();
        }
        let slot = match self.probe(text) {
            Ok(s) => return s,
            Err(slot) => slot,
        };
        assert!(self.len() < SYMS_MAX, "string arena holds 2^24 strings");
        assert!(
            self.bytes.len() + text.len() < BYTES_MAX,
            "string arena exceeds 2 GiB"
        );
        let sym = Sym(self.len());
        self.bytes.push_str(text);
        self.ends.push(self.bytes.len() as u32);
        self.table[slot] = sym.0 + 1;
        assert_eq!(self.get(sym), text);
        sym
    }

    /// Marks what is interned now as loaded (not edit output).
    pub(crate) fn settle(&mut self) {
        self.settled = (self.len(), self.bytes.len() as u32);
        assert!(!self.outgrown(1));
    }

    /// Returns whether edits interned `k` times as many strings or bytes as were loaded
    /// (and at least `k`·2^16 strings or `k` MiB): each may be garbage of a replaced row,
    /// and nothing tells which, so the document is rebuilt then (amortized O(1) per
    /// interned byte).
    pub(crate) fn outgrown(&self, k: u32) -> bool {
        assert!(k >= 1);
        let (n, b) = self.settled;
        assert!(
            n <= self.len(),
            "strings are freed only by a rebuild, which settles"
        );
        assert!(
            b as usize <= self.bytes.len(),
            "bytes are freed only by a rebuild"
        );
        let dn = u64::from(self.len() - n);
        let db = u64::from(self.bytes.len() as u32 - b);
        let k = u64::from(k);
        (dn >= k << 16 && dn >= u64::from(n) * k) || (db >= k << 20 && db >= u64::from(b) * k)
    }
}

impl ObjId {
    /// "No id" (rows of kinds that have none).
    pub const NONE: ObjId = ObjId(u32::MAX);

    /// Decodes a 6-char id to its 30-bit value; `None` for any other text.
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: the asserts check that the decoded value fits 30 bits
    /// and that the text is an id by the format's rules.
    #[must_use]
    pub fn decode(text: &str) -> Option<ObjId> {
        if text.len() != ID_LEN_NEW {
            return None;
        }
        let mut v: u32 = 0;
        for b in text.bytes() {
            let d = DIGIT[usize::from(b)];
            if d == NOT_DIGIT {
                return None;
            }
            v = v << 5 | u32::from(d);
        }
        assert!(v < 1 << 30);
        assert!(schemify_format::is_id(text));
        Some(ObjId(v))
    }

    /// Returns the id of `text` in `strings` without interning; `None` when `text` is
    /// not an id or is an alias this arena never saw (so no record can carry it).
    ///
    /// # Panics
    ///
    /// Only on a bug in this crate: an id never equals [`ObjId::NONE`].
    #[must_use]
    pub fn find(text: &str, strings: &Strings) -> Option<ObjId> {
        if !schemify_format::is_id(text) {
            return None;
        }
        let id = ObjId::decode(text).or_else(|| strings.find(text).map(|s| ObjId(ALIAS | s.0)))?;
        assert!(id != ObjId::NONE);
        Some(id)
    }

    /// Returns the id of `text`, interning an alias when it is not 6 chars long.
    ///
    /// # Panics
    ///
    /// Panics if `text` is not an id (callers validate first).
    pub(crate) fn intern(text: &str, strings: &mut Strings) -> ObjId {
        assert!(schemify_format::is_id(text), "{text:?} is not an id");
        let id = ObjId::decode(text).unwrap_or_else(|| ObjId(ALIAS | strings.intern(text).0));
        assert!(id != ObjId::NONE);
        id
    }

    /// Returns whether this id's text lives in the document arena (it is not 6 chars).
    #[must_use]
    pub fn is_alias(self) -> bool {
        self.0 & ALIAS != 0
    }

    /// Appends the id text to `out`.
    ///
    /// # Panics
    ///
    /// Panics on [`ObjId::NONE`] or an alias from another arena.
    pub fn write(self, strings: &Strings, out: &mut String) {
        assert!(self != ObjId::NONE, "NONE has no text");
        if self.is_alias() {
            out.push_str(strings.get(Sym(self.0 & !ALIAS)));
            return;
        }
        assert!(self.0 < 1 << 30, "a 6-char id is a 30-bit value");
        for i in (0..ID_LEN_NEW).rev() {
            out.push(char::from(ID_ALPHABET[(self.0 >> (5 * i) & 31) as usize]));
        }
    }

    /// Returns the id text as a new string (see [`ObjId::write`]; allocates).
    ///
    /// # Panics
    ///
    /// As [`ObjId::write`].
    #[must_use]
    pub fn text(self, strings: &Strings) -> String {
        let mut s = String::with_capacity(ID_LEN_NEW);
        self.write(strings, &mut s);
        assert!(schemify_format::is_id(&s));
        s
    }
}

impl Hasher for MixHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn write_u32(&mut self, n: u32) {
        self.0 = (self.0 ^ u64::from(n)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

// ---- private ----

/// Multiply-rotate hash over 8-byte words (FxHash style): deterministic on every
/// platform, so nothing depends on a per-process random seed. Interning is the hot loop
/// of loading (~12 strings per instance); byte-at-a-time FNV cost twice as much.
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

impl Strings {
    /// Finds `text`'s symbol, or the empty slot where it belongs. Bounded: the load
    /// factor stays below 1/2, so an empty slot exists.
    fn probe(&self, text: &str) -> Result<Sym, usize> {
        assert!(self.table.len().is_power_of_two());
        assert!(self.ends.len() < self.table.len(), "an empty slot exists");
        let mask = self.table.len() - 1;
        let mut slot = hash(text) as usize & mask;
        for _ in 0..self.table.len() {
            match self.table[slot] {
                0 => return Err(slot),
                e if self.get(Sym(e - 1)) == text => return Ok(Sym(e - 1)),
                _ => slot = (slot + 1) & mask,
            }
        }
        unreachable!("the load factor stays below 1/2, so a probe meets an empty slot")
    }

    fn grow(&mut self) {
        let len = (self.table.len() * 2).max(64);
        assert!(len.is_power_of_two());
        self.table = vec![0; len];
        for i in 0..self.len() {
            let slot = self
                .probe(self.get(Sym(i)))
                .expect_err("strings are distinct");
            self.table[slot] = i + 1;
        }
        assert!(self.ends.len() * 2 < self.table.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_dedups_and_survives_growth() {
        let mut s = Strings::default();
        let syms: Vec<Sym> = (0..1000).map(|i| s.intern(&format!("n{i}"))).collect();
        for (i, sym) in syms.iter().enumerate() {
            assert_eq!(s.get(*sym), format!("n{i}"));
            assert_eq!(s.intern(&format!("n{i}")), *sym);
            assert_eq!(s.find(&format!("n{i}")), Some(*sym));
        }
        assert_eq!(s.find("missing"), None);
        assert_eq!(s.len(), 1000);
        let e = s.intern("");
        assert_eq!(s.get(e), "");
    }

    #[test]
    fn ids_round_trip_and_order_like_text() {
        let mut s = Strings::default();
        for text in ["000000", "0gsrp0", "zzzzzz", "1j446r"] {
            let id = ObjId::intern(text, &mut s);
            assert!(!id.is_alias());
            assert_eq!(id.text(&s), text);
        }
        assert!(ObjId::decode("0gsrp0") < ObjId::decode("1j446r"));
        let short = ObjId::intern("0abc", &mut s);
        assert!(short.is_alias());
        assert_eq!(short.text(&s), "0abc");
        assert_eq!(ObjId::find("0abc", &s), Some(short));
        assert_eq!(ObjId::find("0abd", &s), None);
        assert_eq!(ObjId::find("ABC", &s), None);
        assert_eq!(ObjId::decode("0gsrpi"), None);
    }
}
