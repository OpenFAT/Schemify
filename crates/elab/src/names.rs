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
        assert_eq!(rows.len(), v.len());
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
        if n < 1 << 12 {
            out.extend(texts.iter().map(|t| self.intern(t)));
            return;
        }
        assert!(n < NAMES_MAX as usize, "a batch fits the arena");
        self.reserve(n); // the table grows once, here
        let mask = self.table.len() - 1;
        // Items `(hash << 32) | index`, sorted by (home slot, index): LSD radix on the slot,
        // 14-bit digits (≤ 2^27 slots). Each item carries its hash, so the slot-order passes
        // read the items and the table sequentially and touch a text only on a hash match.
        let mut items: Vec<u64> = texts
            .iter()
            .enumerate()
            .map(|(k, t)| u64::from(hash(t)) << 32 | k as u64)
            .collect();
        let slot = |item: u64| (item >> 32) as usize & mask;
        let mut tmp = vec![0u64; n];
        for shift in [0u32, 14] {
            if (mask >> shift) == 0 {
                break;
            }
            let digit = |item: u64| slot(item) >> shift & 0x3fff;
            let mut count = vec![0u32; (1 << 14) + 1];
            for &it in &items {
                count[digit(it) + 1] += 1;
            }
            for d in 0..1 << 14 {
                count[d + 1] += count[d];
            }
            for &it in &items {
                let d = digit(it);
                tmp[count[d] as usize] = it;
                count[d] += 1;
            }
            std::mem::swap(&mut items, &mut tmp);
        }
        drop(tmp);
        debug_assert!(
            items
                .windows(2)
                .all(|w| (slot(w[0]), w[0] as u32) < (slot(w[1]), w[1] as u32))
        );
        // Pass 1, slot order: an existing name, NEW, or DUP of the first occurrence of the
        // same text in the batch (equal texts share a home slot: one run, index order).
        const NEW: u32 = u32::MAX;
        const DUP: u32 = 1 << 31;
        let mut res = vec![NEW; n]; // by item position
        let mut run = 0;
        for at in 0..n {
            let (it, k) = (items[at], items[at] as u32 as usize);
            let h = (it >> 32) as u32;
            if at > 0 && slot(items[at - 1]) != slot(it) {
                run = at;
            }
            let mut sl = slot(it);
            loop {
                let e = self.table[sl];
                if e == 0 {
                    break;
                }
                let name = Name((e as u32) - 1);
                if (e >> 32) as u32 == h && self.get(name) == texts[k] {
                    res[at] = name.0;
                    break;
                }
                sl = (sl + 1) & mask;
            }
            if res[at] != NEW {
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
                assert!(self.len() < NAMES_MAX, "name arena holds 2^26 names");
                assert!(
                    self.bytes.len() + texts[k].len() < u32::MAX as usize,
                    "name arena exceeds 4 GiB"
                );
                self.bytes.push_str(texts[k]);
                self.ends.push(self.bytes.len() as u32);
                self.len() - 1
            } else if r & DUP != 0 && r != NEW {
                by_k[(r & !DUP) as usize] // an earlier index: already numbered
            } else {
                r
            };
        }
        // Pass 3, slot order: place the new names (linear probing from the home slot keeps
        // every entry reachable whatever the insertion order).
        for at in 0..n {
            if res[at] != NEW {
                continue;
            }
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
}
