//! The calculator's discrete Fourier transform (D23): iterative radix-2 for power-of-two
//! lengths, Bluestein's chirp-z (a power-of-two circular convolution) for every other length.
//!
//! Twiddles and chirps come from `libm` (pure Rust), so a spectrum is bit-identical on every
//! target (D32); `rustfft` used the platform `sin`/`cos`. Lengths are bounded by the column
//! (a step of one raw plot), so scratch is allocated per call.

use core::f64::consts::PI;

use num_complex::Complex64 as C;

/// Transforms `buf` in place: `X[k] = Σₙ x[n]·e^(−2πi·kn/N)`, unscaled.
/// Cost O(N log N) for every N; any N that is not a power of two allocates two scratch
/// buffers of the next power of two ≥ 2N − 1.
pub(crate) fn forward(buf: &mut [C]) {
    let n = buf.len();
    if n.is_power_of_two() || n == 0 {
        radix2(buf, -1.0);
    } else {
        bluestein(buf);
    }
    assert_eq!(buf.len(), n);
}

/// `e^(i·angle)` through `libm`.
fn cis(angle: f64) -> C {
    C::new(libm::cos(angle), libm::sin(angle))
}

/// In-place iterative Cooley–Tukey; `sign` −1 forward, +1 inverse (unscaled).
fn radix2(buf: &mut [C], sign: f64) {
    let n = buf.len();
    assert!(
        n == 0 || n.is_power_of_two(),
        "radix-2 needs a power-of-two length"
    );
    assert!(sign == 1.0 || sign == -1.0);
    if n <= 1 {
        return;
    }
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            buf.swap(i, j);
        }
    }
    // One table for the last stage; stage `len` reads every (n / len)-th entry.
    let twiddles: Vec<C> = (0..n / 2)
        .map(|k| cis(sign * 2.0 * PI * k as f64 / n as f64))
        .collect();
    let mut len = 2;
    while len <= n {
        let (half, stride) = (len / 2, n / len);
        for start in (0..n).step_by(len) {
            for k in 0..half {
                let t = buf[start + k + half] * twiddles[k * stride];
                let u = buf[start + k];
                buf[start + k] = u + t;
                buf[start + k + half] = u - t;
            }
        }
        len <<= 1;
    }
}

/// Bluestein: `X[k] = w[k]·Σₙ (x[n]·w[n])·conj(w[k−n])` with `w[k] = e^(−iπk²/N)`, the sum a
/// circular convolution done with three radix-2 transforms of length M ≥ 2N − 1.
fn bluestein(buf: &mut [C]) {
    let n = buf.len();
    assert!(n >= 3 && !n.is_power_of_two());
    let m = (2 * n - 1).next_power_of_two();
    // k² mod 2N keeps the chirp angle small, so it stays exact for long columns.
    let two_n = 2 * n as u128;
    let chirp: Vec<C> = (0..n)
        .map(|k| {
            let k2 = (k as u128 * k as u128 % two_n) as f64;
            cis(-PI * k2 / n as f64)
        })
        .collect();
    let mut a = vec![C::new(0.0, 0.0); m];
    let mut b = vec![C::new(0.0, 0.0); m];
    for k in 0..n {
        a[k] = buf[k] * chirp[k];
        b[k] = chirp[k].conj();
        if k > 0 {
            b[m - k] = chirp[k].conj();
        }
    }
    radix2(&mut a, -1.0);
    radix2(&mut b, -1.0);
    for (x, y) in a.iter_mut().zip(&b) {
        *x *= *y;
    }
    radix2(&mut a, 1.0);
    let scale = 1.0 / m as f64;
    for k in 0..n {
        buf[k] = a[k] * scale * chirp[k];
    }
    assert!(m >= 2 * n - 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic inputs (an LCG), so failures replay.
    fn signal(n: usize, seed: u64) -> Vec<C> {
        let mut s = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let mut next = || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (s >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
        };
        (0..n).map(|_| C::new(next(), next())).collect()
    }

    /// The oracle: the O(N²) definition.
    fn naive(x: &[C]) -> Vec<C> {
        let n = x.len();
        (0..n)
            .map(|k| {
                (0..n)
                    .map(|j| x[j] * cis(-2.0 * PI * ((k * j) % n) as f64 / n as f64))
                    .sum()
            })
            .collect()
    }

    fn assert_close(ours: &[C], theirs: &[C], tol: f64) {
        assert_eq!(ours.len(), theirs.len());
        let scale = theirs.iter().map(|z| z.norm()).fold(1.0, f64::max);
        for (k, (a, b)) in ours.iter().zip(theirs).enumerate() {
            assert!(
                (a - b).norm() <= tol * scale,
                "bin {k} of {}: {a} vs {b}",
                ours.len()
            );
        }
    }

    #[test]
    fn matches_the_naive_dft_for_every_small_length() {
        for n in 0..=96 {
            let x = signal(n, n as u64);
            let mut ours = x.clone();
            forward(&mut ours);
            assert_close(&ours, &naive(&x), 1e-12 * (n.max(1) as f64));
        }
    }

    #[test]
    fn matches_rustfft_on_long_and_odd_lengths() {
        let mut planner = rustfft::FftPlanner::new();
        for n in [128, 1000, 1024, 4093, 4096, 6000, 65536] {
            let x = signal(n, 7 + n as u64);
            let mut ours = x.clone();
            forward(&mut ours);
            let mut theirs = x;
            planner.plan_fft_forward(n).process(&mut theirs);
            assert_close(&ours, &theirs, 1e-10);
        }
    }

    #[test]
    fn known_spectra_and_linearity() {
        // A unit impulse is flat; a constant is one DC bin.
        let mut imp = vec![C::new(0.0, 0.0); 12];
        imp[0] = C::new(1.0, 0.0);
        forward(&mut imp);
        assert!(imp.iter().all(|z| (z - C::new(1.0, 0.0)).norm() < 1e-14));
        let mut dc = vec![C::new(2.0, 0.0); 8];
        forward(&mut dc);
        assert_eq!(dc[0], C::new(16.0, 0.0));
        assert!(dc[1..].iter().all(|z| z.norm() < 1e-14));
        // F(a + b) = F(a) + F(b), on a Bluestein length.
        let (a, b) = (signal(45, 1), signal(45, 2));
        let mut sum: Vec<C> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
        let (mut fa, mut fb) = (a, b);
        forward(&mut fa);
        forward(&mut fb);
        forward(&mut sum);
        let both: Vec<C> = fa.iter().zip(&fb).map(|(x, y)| x + y).collect();
        assert_close(&sum, &both, 1e-12);
    }
}
