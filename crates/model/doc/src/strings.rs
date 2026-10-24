//! The per-document string arena and object ids.
//!
//! Six questions:
//! 1. In: record texts (names, cell references, values, template strings). Out: `Sym`
//!    handles every table column stores instead of text.
//! 2. One arena per document; a 100k-object cell interns ~100k distinct strings.
//! 3. `Sym` is `u32` (capacity 2^24 strings, 2^31 bytes). `ObjId` is `u32`: 30 bits for
//!    the 6-char ids the writer generates, bit 31 for any other id length.
//! 4. Interning is a hash probe on load and edit; reads are `ends[sym]` slices.
//! 5. Lifetime: the document. Strings are never freed; `Document::compact` rebuilds.
//! 6. Not parallel: one writer (`doc`), many readers through `&Strings`.

use schemify_format::{ID_ALPHABET, ID_LEN_NEW, is_id};
use std::hash::{BuildHasherDefault, Hasher};

/// Handle of an interned string; valid for the document that interned it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sym(pub u32);

impl Sym {
    /// "No string" in optional columns (an arc's template, a stext's angles).
    pub const NONE: Sym = Sym(u32::MAX);
}

/// Most strings one document interns.
pub const SYMS_MAX: u32 = 1 << 24;
/// Largest arena in bytes.
const BYTES_MAX: usize = 1 << 31;

/// Interned strings of one document: one byte arena plus an open-addressing table.
#[derive(Clone, Debug, Default)]
pub struct Strings {
    bytes: String,
    /// End offset of symbol `i`; it starts where symbol `i - 1` ends.
    ends: Vec<u32>,
    /// Open addressing, linear probing: 0 is empty, else `sym + 1`. Power-of-two length.
    table: Vec<u32>,
}

impl Strings {
    /// Returns the number of interned strings.
    pub fn len(&self) -> u32 {
        self.ends.len() as u32
    }

    /// Returns whether nothing is interned.
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Returns the text of `sym`.
    ///
    /// # Panics
    ///
    /// Panics if `sym` was not interned by this arena (including [`Sym::NONE`]).
    pub fn get(&self, sym: Sym) -> &str {
        let i = sym.0 as usize;
        assert!(i < self.ends.len(), "symbol from another arena or NONE");
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        &self.bytes[start..self.ends[i] as usize]
    }

    /// Returns the symbol of `text` if it was interned, without interning it.
    /// Cost: one hash and a short probe.
    pub fn find(&self, text: &str) -> Option<Sym> {
        if self.table.is_empty() {
            return None;
        }
        let mask = self.table.len() - 1;
        let mut slot = hash(text) as usize & mask;
        for _ in 0..self.table.len() {
            let entry = self.table[slot];
            if entry == 0 {
                return None;
            }
            if self.get(Sym(entry - 1)) == text {
                return Some(Sym(entry - 1));
            }
            slot = (slot + 1) & mask;
        }
        unreachable!("the table is never full: load factor stays below 1/2");
    }

    /// Interns `text`, returning the existing symbol when already present.
    ///
    /// # Panics
    ///
    /// Panics past [`SYMS_MAX`] strings or 2 GiB of text.
    pub(crate) fn intern(&mut self, text: &str) -> Sym {
        if let Some(s) = self.find(text) {
            return s;
        }
        assert!(self.len() < SYMS_MAX, "string arena holds 2^24 strings");
        assert!(
            self.bytes.len() + text.len() < BYTES_MAX,
            "string arena exceeds 2 GiB"
        );
        if (self.ends.len() + 1) * 2 > self.table.len() {
            self.grow();
        }
        let sym = Sym(self.len());
        self.bytes.push_str(text);
        self.ends.push(self.bytes.len() as u32);
        self.insert_slot(sym);
        assert_eq!(self.get(sym), text);
        sym
    }

    fn insert_slot(&mut self, sym: Sym) {
        assert!(self.table.len().is_power_of_two() && (sym.0 as usize) < self.table.len() / 2);
        let mask = self.table.len() - 1;
        let mut slot = hash(self.get(sym)) as usize & mask;
        // Bounded: the load factor stays below 1/2, so an empty slot exists.
        while self.table[slot] != 0 {
            slot = (slot + 1) & mask;
        }
        self.table[slot] = sym.0 + 1;
        assert!(self.table[slot] != 0);
    }

    fn grow(&mut self) {
        let len = (self.table.len() * 2).max(64);
        assert!(len.is_power_of_two());
        self.table = vec![0; len];
        for i in 0..self.len() {
            self.insert_slot(Sym(i));
        }
        assert!(self.ends.len() * 2 <= self.table.len());
    }
}

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

/// Hasher for maps keyed by values that are already well mixed (random object ids).
/// Deterministic; no random state, so it also works on `wasm32-unknown-unknown`.
#[derive(Default)]
pub(crate) struct MixHasher(u64);

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

/// `HashMap` with [`MixHasher`]; never iterated on an output path.
pub(crate) type FastMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<MixHasher>>;

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

/// A record id as stored in tables.
///
/// A 6-char id (every id this crate or the importer generates) is its base32 value,
/// most significant digit first, so it needs no arena, survives reloads, and orders
/// like its text. Ids of any other length (hand-written `0abc`) are `ALIAS | sym`:
/// valid only for the document that interned them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjId(pub u32);

impl ObjId {
    /// "No id" (rows of kinds that have none).
    pub const NONE: ObjId = ObjId(u32::MAX);
    /// Bit 31: the low bits are a [`Sym`] of the id text.
    pub const ALIAS: u32 = 1 << 31;

    /// Decodes a 6-char id to its 30-bit value; `None` for any other text.
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
        assert!(v < 1 << 30 && is_id(text));
        Some(ObjId(v))
    }

    /// Returns the id of `text` in `strings` without interning; `None` when `text` is
    /// not an id or is an alias this arena never saw (so no record can carry it).
    pub fn find(text: &str, strings: &Strings) -> Option<ObjId> {
        if !is_id(text) {
            return None;
        }
        let id = ObjId::decode(text)
            .or_else(|| strings.find(text).map(|s| ObjId(ObjId::ALIAS | s.0)))?;
        assert!(id != ObjId::NONE);
        Some(id)
    }

    /// Returns the id of `text`, interning an alias when it is not 6 chars long.
    ///
    /// # Panics
    ///
    /// Panics if `text` is not an id (callers validate first).
    pub(crate) fn intern(text: &str, strings: &mut Strings) -> ObjId {
        assert!(is_id(text), "{text:?} is not an id");
        let id =
            ObjId::decode(text).unwrap_or_else(|| ObjId(ObjId::ALIAS | strings.intern(text).0));
        assert!(id != ObjId::NONE);
        id
    }

    /// Returns whether this id's text lives in the document arena.
    pub fn is_alias(self) -> bool {
        self.0 & ObjId::ALIAS != 0
    }

    /// Appends the id text to `out`.
    ///
    /// # Panics
    ///
    /// Panics on [`ObjId::NONE`] or an alias from another arena.
    pub fn write(self, strings: &Strings, out: &mut String) {
        assert!(self != ObjId::NONE, "NONE has no text");
        if self.is_alias() {
            out.push_str(strings.get(Sym(self.0 & !ObjId::ALIAS)));
            return;
        }
        for i in (0..ID_LEN_NEW).rev() {
            out.push(char::from(ID_ALPHABET[(self.0 >> (5 * i) & 31) as usize]));
        }
    }

    /// Returns the id text as a new string (see [`ObjId::write`]).
    pub fn text(self, strings: &Strings) -> String {
        let mut s = String::with_capacity(ID_LEN_NEW);
        self.write(strings, &mut s);
        assert!(is_id(&s));
        s
    }
}

/// Appends milli-units as the shortest decimal (format spec §3) without the `fmt`
/// machinery or UTF-8 re-validation: the save path formats ~3 coordinates per object.
/// Equal to `schemify_format::format_coord` (tested over a sweep).
pub(crate) fn push_coord(milli: i32, out: &mut String) {
    let before = out.len();
    if milli < 0 {
        out.push('-');
    }
    let mag = i64::from(milli).unsigned_abs();
    let (whole, mut frac) = (mag / 1000, (mag % 1000) as u32);
    let mut digits = [0u8; 10];
    let (mut n, mut w) = (0, whole);
    loop {
        digits[n] = (w % 10) as u8;
        n += 1;
        w /= 10;
        if w == 0 {
            break;
        }
    }
    for &d in digits[..n].iter().rev() {
        out.push(char::from(b'0' + d));
    }
    if frac != 0 {
        out.push('.');
        let mut scale = 100;
        while frac != 0 {
            out.push(char::from(b'0' + (frac / scale) as u8));
            frac %= scale;
            scale /= 10;
        }
    }
    assert!(out.len() > before && !out.ends_with('.'));
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
    fn fast_coordinates_equal_the_format_writer() {
        let mut m: i64 = i64::from(i32::MIN);
        let check = |v: i32| {
            let (mut want, mut got) = (String::new(), String::from("x"));
            schemify_format::format_coord(v, &mut want);
            push_coord(v, &mut got);
            assert_eq!(&got[1..], want, "{v}");
        };
        while m <= i64::from(i32::MAX) {
            check(m as i32);
            m += 7_919_993;
        }
        for v in [
            0,
            1,
            -1,
            10,
            100,
            999,
            1000,
            -1000,
            1001,
            1010,
            12_500,
            -60_000,
            156,
            50,
            5,
            i32::MAX,
            i32::MIN,
        ] {
            check(v);
        }
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
