//! Object ids (spec §4, research §3.3): 1–13 chars of lowercase Crockford base32,
//! unique per file across all id-bearing kinds. The writer generates 6.

/// Crockford base32 digits in value order. Byte order equals value order, so ids sort
/// bytewise and numerically alike.
pub const ID_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
/// Longest id the reader accepts.
pub const ID_LEN_MAX: usize = 13;
/// Length of every id this crate generates (30 random bits).
pub const ID_LEN_NEW: usize = 6;
/// Draws [`fresh_id`] makes before concluding the caller's id set is full.
const DRAWS_MAX: u32 = 64;

fn digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'h' => Some(b - b'a' + 10),
        b'j' | b'k' => Some(b - b'j' + 18),
        b'm' | b'n' => Some(b - b'm' + 20),
        b'p'..=b't' => Some(b - b'p' + 22),
        b'v'..=b'z' => Some(b - b'v' + 27),
        _ => None,
    }
}

/// Returns whether `s` is a legal id: 1–13 chars from [`ID_ALPHABET`].
pub fn is_id(s: &str) -> bool {
    (1..=ID_LEN_MAX).contains(&s.len()) && s.bytes().all(|b| digit(b).is_some())
}

/// Packs an id into an integer whose order equals the bytewise order of the text:
/// 13 five-bit digits left-aligned (pad 0, the smallest digit), then the length.
/// `None` when `s` is not a legal id.
pub(crate) fn id_sort_key(s: &str) -> Option<u128> {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > ID_LEN_MAX {
        return None;
    }
    let mut key: u128 = 0;
    for i in 0..ID_LEN_MAX {
        let d = match b.get(i) {
            Some(&c) => digit(c)?,
            None => 0,
        };
        key = key << 5 | u128::from(d);
    }
    let key = key << 4 | b.len() as u128;
    assert!(
        key & 0xf != 0 && key >> 69 == 0,
        "length nibble set, 69 bits used"
    );
    Some(key)
}

/// 64-bit FNV-1a, folded over `bytes` starting from `hash` (pass [`FNV_OFFSET`] to start).
pub(crate) fn fnv1a64(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// FNV-1a 64-bit offset basis.
pub(crate) const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// Caller-owned random state for id generation (splitmix64). There is no global RNG:
/// the same seed always yields the same id sequence.
#[derive(Clone, Debug)]
pub struct IdRng {
    state: u64,
}

impl IdRng {
    /// Creates a generator from a seed (e.g. OS entropy in the editor, a fixed value in tests).
    pub fn new(seed: u64) -> IdRng {
        IdRng { state: seed }
    }

    /// Creates the deterministic generator an importer uses for one source record, so
    /// re-importing an unchanged library yields the same ids (research §3.3). The seed
    /// is FNV-1a over the path bytes followed by `ordinal` little-endian. Draw with
    /// [`fresh_id`] so collisions between ordinals are redrawn deterministically.
    pub fn for_import(source_path: &str, ordinal: u32) -> IdRng {
        let hash = fnv1a64(FNV_OFFSET, source_path.as_bytes());
        IdRng::new(fnv1a64(hash, &ordinal.to_le_bytes()))
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// A generated 6-char id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NewId([u8; ID_LEN_NEW]);

impl NewId {
    /// Returns the id text.
    pub fn as_str(&self) -> &str {
        let s = std::str::from_utf8(&self.0).expect("id bytes are ASCII");
        assert_eq!(s.len(), ID_LEN_NEW);
        s
    }
}

/// Draws 6-char ids from `rng` until `taken` reports one unused (the editor's
/// redraw-on-collision rule, research §3.3).
///
/// # Panics
///
/// Panics after 64 consecutive taken draws. With 2^30 ids and at most 2^22 objects
/// per file that has probability below 2^-512; reaching it means `taken` is wrong.
pub fn fresh_id(rng: &mut IdRng, mut taken: impl FnMut(&str) -> bool) -> NewId {
    for _ in 0..DRAWS_MAX {
        let bits = rng.next_u64();
        let mut id = [0u8; ID_LEN_NEW];
        for (i, c) in id.iter_mut().enumerate() {
            *c = ID_ALPHABET[((bits >> (5 * i)) & 31) as usize];
        }
        let id = NewId(id);
        if !taken(id.as_str()) {
            assert!(is_id(id.as_str()));
            return id;
        }
    }
    panic!("fresh_id: {DRAWS_MAX} consecutive draws were taken; the id set predicate is broken");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_syntax() {
        for good in ["0", "0gsrp0", "zzzzzzzzzzzzz", "abcdefghjkmnp"] {
            assert!(is_id(good), "{good}");
        }
        for bad in [
            "",
            "0gsrpi",
            "ABC123",
            "abcdefghjkmnpq",
            "a-b",
            "l",
            "o",
            "u",
            "é",
        ] {
            assert!(!is_id(bad), "{bad}");
            assert_eq!(id_sort_key(bad), None);
        }
    }

    #[test]
    fn sort_key_order_matches_bytewise_order() {
        let mut rng = IdRng::new(7);
        let mut ids: Vec<String> = ["a", "a0", "a00", "a1", "0a", "z", "zzzzzzzzzzzzz", "0"]
            .map(String::from)
            .to_vec();
        for n in 0..200u64 {
            let len = (rng.next_u64() % 13 + 1) as usize;
            let bits = rng.next_u64() ^ n;
            ids.push(
                (0..len)
                    .map(|i| ID_ALPHABET[((bits >> (i * 4)) % 32) as usize] as char)
                    .collect(),
            );
        }
        for x in &ids {
            for y in &ids {
                assert_eq!(
                    x.cmp(y),
                    id_sort_key(x).unwrap().cmp(&id_sort_key(y).unwrap()),
                    "{x} vs {y}"
                );
            }
        }
    }

    #[test]
    fn generation_is_deterministic_and_avoids_taken() {
        let a = fresh_id(&mut IdRng::new(42), |_| false);
        let b = fresh_id(&mut IdRng::new(42), |_| false);
        assert_eq!(a, b);
        assert_eq!(a.as_str().len(), ID_LEN_NEW);
        let c = fresh_id(&mut IdRng::new(42), |s| s == a.as_str());
        assert_ne!(a, c);
        let i1 = fresh_id(&mut IdRng::for_import("lib/a.sch", 3), |_| false);
        let i2 = fresh_id(&mut IdRng::for_import("lib/a.sch", 3), |_| false);
        let i3 = fresh_id(&mut IdRng::for_import("lib/a.sch", 4), |_| false);
        assert_eq!(i1, i2);
        assert_ne!(i1, i3);
    }

    #[test]
    #[should_panic(expected = "consecutive draws")]
    fn generation_panics_when_everything_is_taken() {
        fresh_id(&mut IdRng::new(1), |_| true);
    }
}
