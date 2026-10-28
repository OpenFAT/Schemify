//! The elaboration string arena: text in, [`Name`] handles out. Every name elab derives
//! (net and bit names, instance element names, cell names, parameter value text) is a
//! [`Name`] into one [`Names`]; plus the text helpers that build names (decimal, FNV-1a,
//! base32) and the one key sort every sorted output shares.
//!
//! `doc` interns per document; elab output spans many documents, so it owns one arena per
//! output ([`crate::Elab`], [`crate::CellState`]). Same shape as `doc`'s arena: one byte
//! buffer plus an open-addressing table with a deterministic hash (no random state, so it
//! behaves the same on `wasm32-unknown-unknown`). Cites netlist spec §6 (FNV-1a 64 and
//! base32 specialization names).
//!
//! Contents:
//! - `pub struct Name`, `Name::NONE`
//! - `pub const NAMES_MAX`
//! - `pub struct Names`: `len`, `is_empty`, `get`, `find`, `cmp`, `sort_by_text`, `sort`,
//!   `pub(crate)` `reserve`, `intern`, `intern_batch`
//! - `pub(crate) fn fnv1a64(bytes: &[u8]) -> u64`
//! - `pub(crate) fn push_base32(h: u64, chars: u32, out: &mut String)`
//! - `pub(crate) fn push_decimal(v: u32, out: &mut String)`
//! - `pub(crate) fn radix_sort(v: Vec<(u64, u32)>) -> Vec<(u64, u32)>`

use std::cmp::Ordering;

/// Handle of a string in a [`Names`] arena; valid only for the arena that made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(pub u32);

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

/// Batches below this intern one by one: the slot-order sweep pays off only once the
/// table outgrows the cache.
const BATCH_MIN: usize = 1 << 12;

/// Inputs below this [`radix_sort`] hands to `sort_unstable`.
const RADIX_MIN: usize = 1 << 12;

/// Bits per radix digit of [`radix_sort`] (6 passes cover a 64-bit key).
const KEY_DIGIT_BITS: u32 = 11;

/// Bits per radix digit of the batch's home-slot sort: two passes cover 2^28 slots, more
/// than [`NAMES_MAX`] at load 1/2 needs.
const SLOT_DIGIT_BITS: u32 = 14;
const _: () = assert!((1u64 << (2 * SLOT_DIGIT_BITS)) >= 2 * NAMES_MAX as u64);

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

impl Names {
    /// Returns the number of interned names.
    #[must_use]
    pub fn len(&self) -> u32 {
        self.ends.len() as u32
    }

    /// Returns whether nothing is interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Returns the text of `n`.
    ///
    /// # Panics
    ///
    /// Panics on [`Name::NONE`] or a name from another arena that is out of range.
    #[must_use]
    pub fn get(&self, n: Name) -> &str {
        let i = n.0 as usize;
        assert!(i < self.ends.len(), "name from another arena or NONE");
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        &self.bytes[start..self.ends[i] as usize]
    }

    /// Returns the name of `text` without interning it.
    #[must_use]
    pub fn find(&self, text: &str) -> Option<Name> {
        if self.table.is_empty() {
            return None;
        }
        self.probe(text, hash(text)).ok()
    }

    /// Compares two names bytewise by text (the order every sorted output uses).
    /// [`Name::NONE`] sorts first.
    #[must_use]
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

    /// Sorts `rows` by the text of the name ([`Names::cmp`]), then by the tag. With
    /// distinct tags the order is total, so tagging each row with its position gives a
    /// stable sort by text.
    ///
    /// Why one routine: text sorts of names share this one sort instead of instantiating
    /// a sort per call-site closure (smaller code, one place that defines the order).
    ///
    /// Cost: O(n log n) text compares, no allocation.
    pub fn sort_by_text(&self, rows: &mut [(Name, u32)]) {
        rows.sort_unstable_by(|a, b| self.cmp(a.0, b.0).then(a.1.cmp(&b.1)));
        debug_assert!(rows.windows(2).all(|w| self.cmp(w[0].0, w[1].0).is_le()));
    }

    /// Sorts `v` by text ([`Names::cmp`]); equal names are equal handles, so the order is
    /// total.
    ///
    /// Cost: [`Names::sort_by_text`] plus one scratch copy of `v`.
    pub fn sort(&self, v: &mut [Name]) {
        let mut rows: Vec<(Name, u32)> = v.iter().map(|&n| (n, 0)).collect();
        self.sort_by_text(&mut rows);
        for (d, r) in v.iter_mut().zip(&rows) {
            *d = r.0;
        }
        debug_assert!(v.windows(2).all(|w| self.cmp(w[0], w[1]).is_le()));
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

    /// Interns `text`; equal text always yields the same name.
    ///
    /// Cost: one hash and one probe sequence (absent text is placed where the lookup
    /// ended); amortized O(1).
    ///
    /// # Panics
    ///
    /// Panics past [`NAMES_MAX`] names or 4 GiB of text.
    pub(crate) fn intern(&mut self, text: &str) -> Name {
        if (self.ends.len() + 1) * 2 > self.table.len() {
            self.grow();
        }
        let h = hash(text);
        let slot = match self.probe(text, h) {
            Ok(n) => return n,
            Err(slot) => slot,
        };
        let n = self.append(text);
        self.table[slot] = entry(h, n);
        debug_assert_eq!(self.get(n), text);
        n
    }

    /// Interns every text of `texts` and appends their names to `out`: the same names, and
    /// new ones numbered in the same order, as interning them one by one.
    ///
    /// Why: a batch of mostly new texts (a flat design's element and generated net names,
    /// 10^6 each) misses the cache once per text when probed in text order, the table being
    /// tens of MB. Here the probes run in table-slot order (a radix sort of the batch by
    /// home slot), so the table is swept; small batches just intern one by one.
    ///
    /// Cost: O(n) passes over the batch plus the probe sequences.
    pub(crate) fn intern_batch(&mut self, texts: &[&str], out: &mut Vec<Name>) {
        let n = texts.len();
        if n < BATCH_MIN {
            out.extend(texts.iter().map(|t| self.intern(t)));
            return;
        }
        assert!(n < NAMES_MAX as usize, "a batch fits the arena");
        self.reserve(n); // the table grows once, here
        let mask = self.table.len() - 1;
        let slot = |item: u64| (item >> 32) as usize & mask;
        let items = slot_order(texts, mask);
        // Pass 1, slot order: an existing name, NEW, or DUP of the first occurrence of the
        // same text in the batch (equal texts share a home slot: one run, index order).
        const NEW: u32 = u32::MAX;
        const DUP: u32 = 1 << 31;
        let mut res = vec![NEW; n]; // by item position
        let mut run = 0;
        for at in 0..n {
            let (it, k) = (items[at], items[at] as u32 as usize);
            if at > 0 && slot(items[at - 1]) != slot(it) {
                run = at;
            }
            if let Ok(name) = self.probe_from(texts[k], (it >> 32) as u32, slot(it)) {
                res[at] = name.0;
                continue;
            }
            let earlier = (run..at).find(|&j| {
                res[j] == NEW
                    && items[j] >> 32 == it >> 32
                    && texts[items[j] as u32 as usize] == texts[k]
            });
            if let Some(j) = earlier {
                res[at] = DUP | items[j] as u32;
            }
        }
        // Pass 2, batch order: number the new names as one-by-one interning would.
        let mut by_k = vec![NEW; n];
        for at in 0..n {
            by_k[items[at] as u32 as usize] = res[at];
        }
        // An upper bound read in text order (the exact sum would read texts in slot order).
        self.bytes.reserve(texts.iter().map(|t| t.len()).sum());
        for k in 0..n {
            let r = by_k[k];
            by_k[k] = if r == NEW {
                self.append(texts[k]).0
            } else if r & DUP != 0 {
                by_k[(r & !DUP) as usize] // an earlier index: already numbered
            } else {
                r
            };
        }
        // Pass 3, slot order: place the new names (linear probing from the home slot keeps
        // every entry reachable whatever the insertion order).
        for at in (0..n).filter(|&at| res[at] == NEW) {
            let it = items[at];
            let mut sl = slot(it);
            while self.table[sl] != 0 {
                sl = (sl + 1) & mask;
            }
            self.table[sl] = entry((it >> 32) as u32, Name(by_k[it as u32 as usize]));
        }
        out.extend(by_k.into_iter().map(Name));
        assert!(
            self.ends.len() * 2 <= self.table.len(),
            "load stays below 1/2"
        );
    }
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
///
/// # Panics
///
/// Panics unless `chars` is 1..=13 (13 digits hold 64 bits).
pub(crate) fn push_base32(h: u64, chars: u32, out: &mut String) {
    assert!((1..=13).contains(&chars));
    let before = out.len();
    for i in 0..chars {
        let shift = 64i32 - 5 * (i as i32 + 1);
        let digit = if shift >= 0 {
            (h >> shift) & 31
        } else {
            (h << -shift) & 31
        };
        out.push(char::from(schemify_format::ID_ALPHABET[digit as usize]));
    }
    assert_eq!(out.len() - before, chars as usize);
}

/// Appends `v` in decimal (what `to_string` writes, without its allocation: generated net
/// names are built once per unnamed bit).
pub(crate) fn push_decimal(mut v: u32, out: &mut String) {
    let mut digits = [0u8; 10];
    let mut n = 0;
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    assert!(n <= digits.len());
    out.extend(digits[..n].iter().rev().map(|&d| char::from(d)));
}

/// Sorts `(key, index)` pairs whose indices are ascending, so ties keep index order as a
/// `(key, index)` sort would: LSD radix on the key for large inputs, `sort_unstable` below
/// 2^12 pairs.
///
/// Why: about a third of `sort_unstable`'s time at 10^6 nodes (the two node sorts were a
/// quarter of a flat 1M-instance elaboration).
pub(crate) fn radix_sort(mut v: Vec<(u64, u32)>) -> Vec<(u64, u32)> {
    if v.len() < RADIX_MIN {
        v.sort_unstable();
        return v;
    }
    const DIGITS: usize = 64usize.div_ceil(KEY_DIGIT_BITS as usize);
    let digit = |k: u64, d: usize| {
        ((k >> (d as u32 * KEY_DIGIT_BITS)) & ((1 << KEY_DIGIT_BITS) - 1)) as usize
    };
    let mut counts = vec![[0u32; 1 << KEY_DIGIT_BITS]; DIGITS];
    for &(k, _) in &v {
        for (d, c) in counts.iter_mut().enumerate() {
            c[digit(k, d)] += 1;
        }
    }
    let mut tmp = vec![(0u64, 0u32); v.len()];
    for (d, c) in counts.iter_mut().enumerate() {
        // A digit every key shares (the high bits of a narrow coordinate range) is skipped.
        if c.iter().any(|&n| n as usize == v.len()) {
            continue;
        }
        let mut sum = 0;
        for n in c.iter_mut() {
            (*n, sum) = (sum, sum + *n);
        }
        for &e in &v {
            let slot = &mut c[digit(e.0, d)];
            tmp[*slot as usize] = e;
            *slot += 1;
        }
        std::mem::swap(&mut v, &mut tmp);
    }
    debug_assert!(v.windows(2).all(|w| w[0] < w[1]));
    v
}

// ---- private ----

impl Names {
    /// Where `text` (hashing to `h`) is: `Ok(name)`, or `Err(empty slot)` to put it in.
    fn probe(&self, text: &str, h: u32) -> Result<Name, usize> {
        self.probe_from(text, h, h as usize & (self.table.len() - 1))
    }

    /// [`Names::probe`] starting at `slot`. The table is never full (load factor below
    /// 1/2), so the probe ends within one lap.
    fn probe_from(&self, text: &str, h: u32, mut slot: usize) -> Result<Name, usize> {
        let mask = self.table.len() - 1;
        assert!(self.table.len().is_power_of_two());
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

    /// Appends `text` as the next name (not yet in the table).
    fn append(&mut self, text: &str) -> Name {
        assert!(self.len() < NAMES_MAX, "name arena holds 2^26 names");
        assert!(
            self.bytes.len() + text.len() < u32::MAX as usize,
            "name arena exceeds 4 GiB"
        );
        self.bytes.push_str(text);
        self.ends.push(self.bytes.len() as u32);
        Name(self.len() - 1)
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
        assert!(self.ends.len() * 2 <= self.table.len());
        assert!(self.table.len().is_power_of_two());
    }
}

/// A table entry: `(hash, name + 1)`.
fn entry(h: u32, n: Name) -> u64 {
    (u64::from(h) << 32) | u64::from(n.0 + 1)
}

/// Items `(hash << 32) | index` of `texts`, sorted by (home slot under `mask`, index): LSD
/// radix on the slot. Each item carries its hash, so the slot-order passes read the items
/// and the table sequentially and touch a text only on a hash match.
fn slot_order(texts: &[&str], mask: usize) -> Vec<u64> {
    let n = texts.len();
    let mut items: Vec<u64> = texts
        .iter()
        .enumerate()
        .map(|(k, t)| u64::from(hash(t)) << 32 | k as u64)
        .collect();
    let slot = |item: u64| (item >> 32) as usize & mask;
    let mut tmp = vec![0u64; n];
    for shift in [0, SLOT_DIGIT_BITS] {
        if (mask >> shift) == 0 {
            break;
        }
        let digit = |item: u64| slot(item) >> shift & ((1 << SLOT_DIGIT_BITS) - 1);
        let mut count = vec![0u32; (1 << SLOT_DIGIT_BITS) + 1];
        for &it in &items {
            count[digit(it) + 1] += 1;
        }
        for d in 0..1 << SLOT_DIGIT_BITS {
            count[d + 1] += count[d];
        }
        for &it in &items {
            let d = digit(it);
            tmp[count[d] as usize] = it;
            count[d] += 1;
        }
        std::mem::swap(&mut items, &mut tmp);
    }
    debug_assert!(
        items
            .windows(2)
            .all(|w| (slot(w[0]), w[0] as u32) < (slot(w[1]), w[1] as u32))
    );
    assert_eq!(items.len(), n);
    items
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: a stable `sort_by(Names::cmp)`, over names with duplicates, NONE and "".
    #[test]
    fn text_sorts_equal_a_stable_sort_by_cmp() {
        let mut n = Names::default();
        let pool: Vec<Name> = ["b", "a10", "", "a", "a1", "B", "a"]
            .iter()
            .map(|t| n.intern(t))
            .chain([Name::NONE])
            .collect();
        let mut seed = 0x9e37_79b9_u32;
        for len in 0..40 {
            let v: Vec<Name> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    pool[(seed >> 16) as usize % pool.len()]
                })
                .collect();
            let mut want: Vec<(Name, u32)> = (0..len).map(|i| (v[i], i as u32)).collect();
            want.sort_by(|a, b| n.cmp(a.0, b.0));
            let mut got: Vec<(Name, u32)> = (0..len).map(|i| (v[i], i as u32)).collect();
            n.sort_by_text(&mut got);
            assert_eq!(got, want);
            let mut names = v.clone();
            n.sort(&mut names);
            assert_eq!(names, want.iter().map(|w| w.0).collect::<Vec<_>>());
        }
    }

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

    /// Oracle: one-by-one interning. New, existing and repeated texts, below and above the
    /// batch threshold, across table growth.
    #[test]
    fn a_batch_interns_like_one_by_one() {
        for (n, pre) in [
            (0usize, 0usize),
            (100, 50),
            (5000, 0),
            (20_000, 7000),
            (70_000, 30_000),
        ] {
            let text = |k: usize| format!("t{}", (k * 7919) % (n.max(1) * 2 / 3 + 1));
            let mut seq = Names::default();
            for k in 0..pre {
                seq.intern(&format!("t{}", k * 3));
            }
            let mut batch = seq.clone();
            let texts: Vec<String> = (0..n).map(text).collect();
            let want: Vec<Name> = texts.iter().map(|t| seq.intern(t)).collect();
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            let mut got = vec![Name(7)];
            batch.intern_batch(&refs, &mut got);
            assert_eq!(&got[1..], &want[..], "n {n}");
            assert_eq!(batch.len(), seq.len());
            for i in 0..seq.len() {
                assert_eq!(batch.get(Name(i)), seq.get(Name(i)));
                assert_eq!(batch.find(seq.get(Name(i))), Some(Name(i)), "findable");
            }
            assert_eq!(batch.find("absent"), None);
        }
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

    /// Oracle: `to_string`, at the digit-count boundaries.
    #[test]
    fn decimal_equals_to_string() {
        for v in [0, 9, 10, 99, 100, 65_535, 1 << 31, u32::MAX] {
            let mut s = String::from("x");
            push_decimal(v, &mut s);
            assert_eq!(s, format!("x{v}"));
        }
    }
}
