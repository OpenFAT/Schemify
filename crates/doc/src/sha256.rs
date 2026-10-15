//! SHA-256 (FIPS 180-4) for lockfile and journal hashes.
//!
//! In-crate rather than the `sha2` crate (docs/dev/contributing.md rule 7: a few lines of our own
//! before a new dependency): ~60 lines, verified against the NIST CAVP vectors in the
//! tests, deterministic, no feature detection, builds for wasm32. `ponytail:` scalar
//! code runs ~300 MB/s; switch to `sha2` (SHA-NI) if lockfile hashing of large PDKs
//! shows up in a profile.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// A SHA-256 digest.
pub type Digest = [u8; 32];

/// Incremental SHA-256, for hashing a sequence of parts without concatenating them.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    fill: usize,
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Sha256 {
            state: H0,
            block: [0; 64],
            fill: 0,
            total: 0,
        }
    }
}

impl Sha256 {
    /// Absorbs `bytes`.
    pub fn update(&mut self, mut bytes: &[u8]) {
        let before = self.total;
        self.total += bytes.len() as u64;
        while !bytes.is_empty() {
            let take = (64 - self.fill).min(bytes.len());
            self.block[self.fill..self.fill + take].copy_from_slice(&bytes[..take]);
            self.fill += take;
            bytes = &bytes[take..];
            if self.fill == 64 {
                compress(&mut self.state, &self.block);
                self.fill = 0;
            }
        }
        assert!(self.fill < 64 && self.total >= before);
    }

    /// Returns the digest of everything absorbed.
    pub fn finish(mut self) -> Digest {
        let bits = self.total.wrapping_mul(8);
        self.update(&[0x80]);
        while self.fill != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        assert_eq!(self.fill, 0);
        let mut out = [0u8; 32];
        for (chunk, word) in out.chunks_exact_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, chunk) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let mut v = *state;
    for i in 0..64 {
        let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
        let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
        let t1 = v[7]
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
        let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
        let t2 = s0.wrapping_add(maj);
        v = [
            t1.wrapping_add(t2),
            v[0],
            v[1],
            v[2],
            v[3].wrapping_add(t1),
            v[4],
            v[5],
            v[6],
        ];
    }
    for (s, x) in state.iter_mut().zip(v) {
        *s = s.wrapping_add(x);
    }
}

/// Returns the SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> Digest {
    let mut h = Sha256::default();
    h.update(bytes);
    h.finish()
}

/// Returns `digest` as 64 lowercase hex characters (the lockfile and journal form).
pub fn hex(digest: &Digest) -> String {
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push(char::from(b"0123456789abcdef"[usize::from(b >> 4)]));
        s.push(char::from(b"0123456789abcdef"[usize::from(b & 15)]));
    }
    assert_eq!(s.len(), 64);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    // NIST CAVP / FIPS 180-4 example vectors.
    #[test]
    fn nist_vectors() {
        let cases: [(&[u8], &str); 3] = [
            (
                b"",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
        ];
        for (msg, want) in cases {
            assert_eq!(hex(&sha256(msg)), want);
        }
        let million = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(&sha256(&million)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        let long = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
        assert_eq!(
            hex(&sha256(long)),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    #[test]
    fn incremental_equals_one_shot_at_every_split() {
        let msg: Vec<u8> = (0..200u8).collect();
        let whole = sha256(&msg);
        for split in 0..msg.len() {
            let mut h = Sha256::default();
            h.update(&msg[..split]);
            h.update(&msg[split..]);
            assert_eq!(h.finish(), whole, "split {split}");
        }
    }
}
