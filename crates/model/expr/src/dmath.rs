//! Deterministic math (D32): every transcendental goes through `libm` (pure
//! Rust), so a value is bit-identical on Linux, macOS, Windows and wasm. The
//! platform libm (`f64::sin`) may differ by an ulp between targets, which
//! would make netlists differ byte-wise. `sqrt` and the four arithmetic
//! operators are IEEE correctly rounded on every target and need no wrapper.
//!
//! Hidden public module: the test-only reference evaluator shares this math
//! kernel (and nothing else), so the differential oracle checks evaluator logic.
//!
//! Contents:
//! - `pub struct Complex`, `Complex::new(re: f64, im: f64) -> Complex`,
//!   `Complex::conj(self) -> Complex`; `Add`, `Sub`, `Mul`, `Div`, `Neg`,
//!   `MulAssign`, `Mul<f64>`, `Div<f64>`
//! - `pub fn norm(z: Complex) -> f64`
//! - `pub(crate) fn real(builtin: Builtin, x: f64) -> f64`
//! - `pub(crate) fn complex(builtin: Builtin, z: Complex) -> Complex`
//! - `pub fn real_named(name: &str, x: f64) -> Option<f64>`
//! - `pub fn complex_named(name: &str, z: Complex) -> Option<Complex>`
//! - `pub fn cpow(a: Complex, b: Complex) -> Complex`

use core::ops::{Add, Div, Mul, MulAssign, Neg, Sub};

use crate::builtin::Builtin;

/// A complex number over two `f64`s: the calculator's `cnum` element and the
/// `fft` sample.
///
/// The operators use the textbook formulas (no scaling, no fused multiply-add),
/// so results are bit-identical to `num_complex::Complex64`'s (tested). `/` by
/// a zero divisor yields non-finite parts, which callers turn into `X010`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Complex {
    /// Real part.
    pub re: f64,
    /// Imaginary part.
    pub im: f64,
}

impl Complex {
    /// `re + i·im`.
    #[must_use]
    pub const fn new(re: f64, im: f64) -> Complex {
        Complex { re, im }
    }

    /// The complex conjugate `re − i·im`.
    #[must_use]
    pub fn conj(self) -> Complex {
        Complex::new(self.re, -self.im)
    }
}

impl Add for Complex {
    type Output = Complex;
    fn add(self, o: Complex) -> Complex {
        Complex::new(self.re + o.re, self.im + o.im)
    }
}

impl Sub for Complex {
    type Output = Complex;
    fn sub(self, o: Complex) -> Complex {
        Complex::new(self.re - o.re, self.im - o.im)
    }
}

impl Mul for Complex {
    type Output = Complex;
    fn mul(self, o: Complex) -> Complex {
        Complex::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }
}

impl MulAssign for Complex {
    fn mul_assign(&mut self, o: Complex) {
        *self = *self * o;
    }
}

impl Div for Complex {
    type Output = Complex;
    fn div(self, o: Complex) -> Complex {
        let d = o.re * o.re + o.im * o.im;
        Complex::new(
            (self.re * o.re + self.im * o.im) / d,
            (self.im * o.re - self.re * o.im) / d,
        )
    }
}

impl Mul<f64> for Complex {
    type Output = Complex;
    fn mul(self, k: f64) -> Complex {
        Complex::new(self.re * k, self.im * k)
    }
}

impl Div<f64> for Complex {
    type Output = Complex;
    fn div(self, k: f64) -> Complex {
        Complex::new(self.re / k, self.im / k)
    }
}

impl Neg for Complex {
    type Output = Complex;
    fn neg(self) -> Complex {
        Complex::new(-self.re, -self.im)
    }
}

/// `|z|`, through `libm::hypot` (no overflow for large parts).
#[must_use]
pub fn norm(z: Complex) -> f64 {
    let magnitude = libm::hypot(z.re, z.im);
    debug_assert!(
        magnitude >= 0.0 || magnitude.is_nan(),
        "|{z:?}| is negative"
    );
    magnitude
}

/// Real elementary function `builtin` at `x`.
///
/// # Panics
/// If `builtin` is not one of the elementary functions (a compiler bug).
pub(crate) fn real(builtin: Builtin, x: f64) -> f64 {
    match builtin {
        Builtin::Sqrt => x.sqrt(),
        Builtin::Exp => libm::exp(x),
        Builtin::Ln => libm::log(x),
        Builtin::Log10 => libm::log10(x),
        Builtin::Sin => libm::sin(x),
        Builtin::Cos => libm::cos(x),
        Builtin::Tan => libm::tan(x),
        Builtin::Asin => libm::asin(x),
        Builtin::Acos => libm::acos(x),
        Builtin::Atan => libm::atan(x),
        Builtin::Sinh => libm::sinh(x),
        Builtin::Cosh => libm::cosh(x),
        Builtin::Tanh => libm::tanh(x),
        _ => unreachable!("not an elementary function"),
    }
}

/// Complex elementary function `builtin` at `z` (principal branches).
///
/// # Panics
/// If `builtin` is not one of the elementary functions (a compiler bug).
pub(crate) fn complex(builtin: Builtin, z: Complex) -> Complex {
    match builtin {
        Builtin::Sqrt => csqrt(z),
        Builtin::Exp => cexp(z),
        Builtin::Ln => cln(z),
        Builtin::Log10 => cln(z) / core::f64::consts::LN_10,
        Builtin::Sin => Complex::new(
            libm::sin(z.re) * libm::cosh(z.im),
            libm::cos(z.re) * libm::sinh(z.im),
        ),
        Builtin::Cos => Complex::new(
            libm::cos(z.re) * libm::cosh(z.im),
            -libm::sin(z.re) * libm::sinh(z.im),
        ),
        Builtin::Tan => complex(Builtin::Sin, z) / complex(Builtin::Cos, z),
        Builtin::Sinh => Complex::new(
            libm::sinh(z.re) * libm::cos(z.im),
            libm::cosh(z.re) * libm::sin(z.im),
        ),
        Builtin::Cosh => Complex::new(
            libm::cosh(z.re) * libm::cos(z.im),
            libm::sinh(z.re) * libm::sin(z.im),
        ),
        Builtin::Tanh => complex(Builtin::Sinh, z) / complex(Builtin::Cosh, z),
        // asin z = -i ln(iz + sqrt(1 - z²))
        Builtin::Asin => {
            let i = Complex::new(0.0, 1.0);
            -i * cln(i * z + csqrt(Complex::new(1.0, 0.0) - z * z))
        }
        // acos z = π/2 - asin z
        Builtin::Acos => {
            Complex::new(core::f64::consts::FRAC_PI_2, 0.0) - complex(Builtin::Asin, z)
        }
        // atan z = (i/2) (ln(1 - iz) - ln(1 + iz))
        Builtin::Atan => {
            let i = Complex::new(0.0, 1.0);
            let one = Complex::new(1.0, 0.0);
            (i * 0.5) * (cln(one - i * z) - cln(one + i * z))
        }
        _ => unreachable!("not an elementary function"),
    }
}

/// Real elementary function by source name (`"sin"`, …); `None` if `name`
/// is not one.
#[must_use]
pub fn real_named(name: &str, x: f64) -> Option<f64> {
    by_name(name).map(|builtin| real(builtin, x))
}

/// Complex elementary function by source name; `None` if `name` is not one.
#[must_use]
pub fn complex_named(name: &str, z: Complex) -> Option<Complex> {
    by_name(name).map(|builtin| complex(builtin, z))
}

/// `a^b` for complex `a`, `b` (principal branch); `0^b` is 0 for `Re b > 0`.
#[must_use]
pub fn cpow(a: Complex, b: Complex) -> Complex {
    if a.re == 0.0 && a.im == 0.0 {
        return if b.re > 0.0 {
            Complex::new(0.0, 0.0)
        } else {
            Complex::new(f64::NAN, f64::NAN)
        };
    }
    cexp(b * cln(a))
}

// ---- private ----

/// The elementary function spelled `name`, if it is one.
fn by_name(name: &str) -> Option<Builtin> {
    Builtin::lookup(name)
        .map(|row| row.builtin)
        .filter(|builtin| builtin.elementary())
}

fn cexp(z: Complex) -> Complex {
    let e = libm::exp(z.re);
    Complex::new(e * libm::cos(z.im), e * libm::sin(z.im))
}

fn cln(z: Complex) -> Complex {
    let argument = libm::atan2(z.im, z.re);
    // The principal branch: the argument lies in [-π, π] (NaN passes through).
    debug_assert!(
        argument.abs() <= core::f64::consts::PI || argument.is_nan(),
        "arg({z:?}) = {argument} off the principal branch"
    );
    Complex::new(libm::log(norm(z)), argument)
}

/// Principal square root without cancellation (Kahan's formulation).
fn csqrt(z: Complex) -> Complex {
    if z.re == 0.0 && z.im == 0.0 {
        return Complex::new(0.0, z.im);
    }
    let t = ((z.re.abs() + norm(z)) * 0.5).sqrt();
    let root = if z.re >= 0.0 {
        Complex::new(t, z.im / (2.0 * t))
    } else {
        Complex::new(z.im.abs() / (2.0 * t), t.copysign(z.im))
    };
    // The principal root lies in the right half-plane (NaN passes through).
    debug_assert!(
        root.re >= 0.0 || root.re.is_nan(),
        "sqrt({z:?}) = {root:?} left of the axis"
    );
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    // The oracle: num-complex, reached through the rustfft dev-dependency.
    use rustfft::num_complex::Complex64 as N;

    fn n(z: Complex) -> N {
        N::new(z.re, z.im)
    }

    fn close(a: Complex, b: N) -> bool {
        (n(a) - b).norm() <= 1e-12 * (1.0 + b.norm())
    }

    #[test]
    fn operators_are_bit_identical_to_num_complex() {
        // An LCG over wide magnitudes and both signs, so failures replay.
        let mut s = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let m = (s >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
            m * libm::pow(10.0, ((s & 0xff) as f64 - 128.0) / 8.0)
        };
        let same =
            |a: Complex, b: N| a.re.to_bits() == b.re.to_bits() && a.im.to_bits() == b.im.to_bits();
        for _ in 0..10_000 {
            let (a, b, k) = (
                Complex::new(next(), next()),
                Complex::new(next(), next()),
                next(),
            );
            assert!(same(a + b, n(a) + n(b)), "{a:?} + {b:?}");
            assert!(same(a - b, n(a) - n(b)), "{a:?} - {b:?}");
            assert!(same(a * b, n(a) * n(b)), "{a:?} * {b:?}");
            assert!(same(a / b, n(a) / n(b)), "{a:?} / {b:?}");
            assert!(same(a * k, n(a) * k) && same(a / k, n(a) / k), "{a:?}, {k}");
            assert!(same(-a, -n(a)) && same(a.conj(), n(a).conj()));
            let mut m = a;
            m *= b;
            assert!(same(m, n(a) * n(b)));
        }
        // A zero divisor gives non-finite parts, never a panic.
        let q = Complex::new(1.0, 1.0) / Complex::new(0.0, 0.0);
        assert!(!q.re.is_finite() && !q.im.is_finite());
    }

    #[test]
    fn complex_matches_num_complex_within_tolerance() {
        let zs = [
            Complex::new(0.3, -1.7),
            Complex::new(-2.5, 0.4),
            Complex::new(1.0, 0.0),
            Complex::new(0.0, -3.0),
            Complex::new(-0.7, -0.2),
        ];
        type Case = (Builtin, fn(N) -> N);
        let cases: [Case; 13] = [
            (Builtin::Sqrt, N::sqrt),
            (Builtin::Exp, N::exp),
            (Builtin::Ln, N::ln),
            (Builtin::Log10, N::log10),
            (Builtin::Sin, N::sin),
            (Builtin::Cos, N::cos),
            (Builtin::Tan, N::tan),
            (Builtin::Sinh, N::sinh),
            (Builtin::Cosh, N::cosh),
            (Builtin::Tanh, N::tanh),
            (Builtin::Asin, N::asin),
            (Builtin::Acos, N::acos),
            (Builtin::Atan, N::atan),
        ];
        for z in zs {
            for (b, reference) in cases {
                let (ours, theirs) = (complex(b, z), reference(n(z)));
                assert!(
                    close(ours, theirs),
                    "{b:?}({z:?}) = {ours:?}, num_complex {theirs}"
                );
            }
        }
        assert!(close(
            cpow(Complex::new(-2.0, 0.5), Complex::new(0.5, 1.5)),
            N::new(-2.0, 0.5).powc(N::new(0.5, 1.5))
        ));
        assert_eq!(
            cpow(Complex::new(0.0, 0.0), Complex::new(2.0, 0.0)),
            Complex::new(0.0, 0.0)
        );
    }

    #[test]
    fn real_matches_std_within_one_ulp_scale() {
        for x in [0.1, 0.5, 1.0, 2.5, -0.9] {
            for (b, f) in [
                (Builtin::Exp, f64::exp as fn(f64) -> f64),
                (Builtin::Sin, f64::sin),
                (Builtin::Atan, f64::atan),
            ] {
                let (ours, theirs) = (real(b, x), f(x));
                assert!((ours - theirs).abs() <= 2.0 * f64::EPSILON * theirs.abs().max(1.0));
            }
        }
    }
}
