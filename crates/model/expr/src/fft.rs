//! The calculator's discrete Fourier transform (D23): iterative radix-2 for power-of-two
//! lengths, Bluestein's chirp-z (a power-of-two circular convolution) for every other length.
//!
//! Twiddles and chirps come from `libm` (pure Rust), so a spectrum is bit-identical on every
//! target (D32); `rustfft` used the platform `sin`/`cos`. Lengths are bounded by the column
//! (a step of one raw plot), so scratch is allocated per call.
//!
//! Contents:
//! - `pub(crate) fn forward(buf: &mut [Complex])`

use core::f64::consts::PI;

use crate::dmath::Complex;

/// Transforms `buf` in place: `X[k] = Σₙ x[n]·e^(−2πi·kn/N)`, unscaled.
/// Cost O(N log N) for every N; any N that is not a power of two allocates two scratch
/// buffers of the next power of two ≥ 2N − 1.
pub(crate) fn forward(buf: &mut [Complex]) {
    let n = buf.len();
    let energy_in = if cfg!(debug_assertions) {
        energy(buf)
    } else {
        0.0
    };
    if n.is_power_of_two() || n == 0 {
        radix2(buf, Direction::Forward);
    } else {
        bluestein(buf);
    }
    // Parseval: the unscaled transform multiplies the energy by N.
    let expected = energy_in * n as f64;
    if cfg!(debug_assertions) && expected.is_finite() {
        let got = energy(buf);
        debug_assert!(
            (got - expected).abs() <= 1e-6 * expected.max(f64::MIN_POSITIVE),
            "Parseval violated for N = {n}: {got} vs {expected}"
        );
    }
}

// ---- private ----

/// Which way [`radix2`] turns: the sign of the twiddle exponent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    /// `e^(−2πi·kn/N)`.
    Forward,
    /// `e^(+2πi·kn/N)`, unscaled.
    Inverse,
}

/// `e^(i·angle)` through `libm`.
fn cis(angle: f64) -> Complex {
    Complex::new(libm::cos(angle), libm::sin(angle))
}

/// In-place iterative Cooley–Tukey, unscaled in both directions.
fn radix2(buf: &mut [Complex], direction: Direction) {
    let n = buf.len();
    assert!(
        n == 0 || n.is_power_of_two(),
        "radix-2 needs a power-of-two length, got {n}"
    );
    let sign = match direction {
        Direction::Forward => -1.0,
        Direction::Inverse => 1.0,
    };
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
    let twiddles: Vec<Complex> = (0..n / 2)
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
    debug_assert_eq!(len, 2 * n, "every stage up to N ran");
}

/// Bluestein: `X[k] = w[k]·Σₙ (x[n]·w[n])·conj(w[k−n])` with `w[k] = e^(−iπk²/N)`, the sum a
/// circular convolution done with three radix-2 transforms of length M ≥ 2N − 1.
fn bluestein(buf: &mut [Complex]) {
    let n = buf.len();
    assert!(n >= 3, "Bluestein serves lengths 3 and up, got {n}");
    assert!(!n.is_power_of_two(), "{n} takes the radix-2 path");
    let m = (2 * n - 1).next_power_of_two();
    // k² mod 2N keeps the chirp angle small, so it stays exact for long columns.
    let two_n = 2 * n as u128;
    let chirp: Vec<Complex> = (0..n)
        .map(|k| {
            let k2 = (k as u128 * k as u128 % two_n) as f64;
            cis(-PI * k2 / n as f64)
        })
        .collect();
    let mut a = vec![Complex::new(0.0, 0.0); m];
    let mut b = vec![Complex::new(0.0, 0.0); m];
    for k in 0..n {
        a[k] = buf[k] * chirp[k];
        b[k] = chirp[k].conj();
        if k > 0 {
            b[m - k] = chirp[k].conj();
        }
    }
    radix2(&mut a, Direction::Forward);
    radix2(&mut b, Direction::Forward);
    for (x, y) in a.iter_mut().zip(&b) {
        *x *= *y;
    }
    radix2(&mut a, Direction::Inverse);
    let scale = 1.0 / m as f64;
    for k in 0..n {
        buf[k] = a[k] * scale * chirp[k];
    }
}

/// `Σ |z|²`, for the Parseval check.
fn energy(buf: &[Complex]) -> f64 {
    buf.iter().map(|z| z.re * z.re + z.im * z.im).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmath::norm;

    /// Deterministic inputs (an LCG), so failures replay.
    fn signal(n: usize, seed: u64) -> Vec<Complex> {
        let mut s = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let mut next = || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (s >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
        };
        (0..n).map(|_| Complex::new(next(), next())).collect()
    }

    /// The oracle: the O(N²) definition.
    fn naive(x: &[Complex]) -> Vec<Complex> {
        let n = x.len();
        (0..n)
            .map(|k| {
                (0..n)
                    .map(|j| x[j] * cis(-2.0 * PI * ((k * j) % n) as f64 / n as f64))
                    .fold(Complex::default(), |acc, t| acc + t)
            })
            .collect()
    }

    fn assert_close(ours: &[Complex], theirs: &[Complex], tol: f64) {
        assert_eq!(ours.len(), theirs.len());
        let scale = theirs.iter().map(|&z| norm(z)).fold(1.0, f64::max);
        for (k, (&a, &b)) in ours.iter().zip(theirs).enumerate() {
            assert!(
                norm(a - b) <= tol * scale,
                "bin {k} of {}: {a:?} vs {b:?}",
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
            let mut theirs: Vec<_> = x
                .iter()
                .map(|z| rustfft::num_complex::Complex64::new(z.re, z.im))
                .collect();
            planner.plan_fft_forward(n).process(&mut theirs);
            let theirs: Vec<Complex> = theirs.iter().map(|z| Complex::new(z.re, z.im)).collect();
            assert_close(&ours, &theirs, 1e-10);
        }
    }

    #[test]
    fn known_spectra_and_linearity() {
        // A unit impulse is flat; a constant is one DC bin.
        let mut imp = vec![Complex::new(0.0, 0.0); 12];
        imp[0] = Complex::new(1.0, 0.0);
        forward(&mut imp);
        assert!(
            imp.iter()
                .all(|&z| norm(z - Complex::new(1.0, 0.0)) < 1e-14)
        );
        let mut dc = vec![Complex::new(2.0, 0.0); 8];
        forward(&mut dc);
        assert_eq!(dc[0], Complex::new(16.0, 0.0));
        assert!(dc[1..].iter().all(|&z| norm(z) < 1e-14));
        // F(a + b) = F(a) + F(b), on a Bluestein length.
        let (a, b) = (signal(45, 1), signal(45, 2));
        let mut sum: Vec<Complex> = a.iter().zip(&b).map(|(&x, &y)| x + y).collect();
        let (mut fa, mut fb) = (a, b);
        forward(&mut fa);
        forward(&mut fb);
        forward(&mut sum);
        let both: Vec<Complex> = fa.iter().zip(&fb).map(|(&x, &y)| x + y).collect();
        assert_close(&sum, &both, 1e-12);
    }
}
