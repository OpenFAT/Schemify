//! Deterministic math. Every transcendental goes through `libm` (pure Rust), so
//! a value is bit-identical on Linux, macOS, Windows and wasm (D32). The
//! platform libm (`f64::sin`, `num_complex`'s methods) may differ by an ulp
//! between targets, which would make netlists differ byte-wise.
//!
//! `sqrt` and the four arithmetic operators are IEEE correctly rounded on every
//! target, so they need no wrapper.
//!
//! Hidden public module: the test-only reference evaluator shares this math
//! kernel (and nothing else) so the differential oracle checks evaluator logic.

use num_complex::Complex64 as C;

use crate::compile::Builtin;

pub fn pow(x: f64, y: f64) -> f64 {
    libm::pow(x, y)
}

pub fn hypot(x: f64, y: f64) -> f64 {
    libm::hypot(x, y)
}

pub fn atan2(y: f64, x: f64) -> f64 {
    libm::atan2(y, x)
}

pub fn log10(x: f64) -> f64 {
    libm::log10(x)
}

/// `|z|`.
pub fn norm(z: C) -> f64 {
    hypot(z.re, z.im)
}

/// Real elementary function `b` at `x`.
///
/// Panics if `b` is not one of the elementary functions (a compiler bug).
pub(crate) fn real(b: Builtin, x: f64) -> f64 {
    match b {
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

/// Complex elementary function `b` at `z` (principal branches).
///
/// Panics if `b` is not one of the elementary functions (a compiler bug).
pub(crate) fn complex(b: Builtin, z: C) -> C {
    match b {
        Builtin::Sqrt => csqrt(z),
        Builtin::Exp => cexp(z),
        Builtin::Ln => cln(z),
        Builtin::Log10 => cln(z) / core::f64::consts::LN_10,
        Builtin::Sin => C::new(
            libm::sin(z.re) * libm::cosh(z.im),
            libm::cos(z.re) * libm::sinh(z.im),
        ),
        Builtin::Cos => C::new(
            libm::cos(z.re) * libm::cosh(z.im),
            -libm::sin(z.re) * libm::sinh(z.im),
        ),
        Builtin::Tan => complex(Builtin::Sin, z) / complex(Builtin::Cos, z),
        Builtin::Sinh => C::new(
            libm::sinh(z.re) * libm::cos(z.im),
            libm::cosh(z.re) * libm::sin(z.im),
        ),
        Builtin::Cosh => C::new(
            libm::cosh(z.re) * libm::cos(z.im),
            libm::sinh(z.re) * libm::sin(z.im),
        ),
        Builtin::Tanh => complex(Builtin::Sinh, z) / complex(Builtin::Cosh, z),
        // asin z = -i ln(iz + sqrt(1 - z²))
        Builtin::Asin => {
            let i = C::new(0.0, 1.0);
            -i * cln(i * z + csqrt(C::new(1.0, 0.0) - z * z))
        }
        // acos z = π/2 - asin z
        Builtin::Acos => C::new(core::f64::consts::FRAC_PI_2, 0.0) - complex(Builtin::Asin, z),
        // atan z = (i/2) (ln(1 - iz) - ln(1 + iz))
        Builtin::Atan => {
            let i = C::new(0.0, 1.0);
            let one = C::new(1.0, 0.0);
            (i * 0.5) * (cln(one - i * z) - cln(one + i * z))
        }
        _ => unreachable!("not an elementary function"),
    }
}

fn by_name(name: &str) -> Option<Builtin> {
    Some(match name {
        "sqrt" => Builtin::Sqrt,
        "exp" => Builtin::Exp,
        "ln" => Builtin::Ln,
        "log10" => Builtin::Log10,
        "sin" => Builtin::Sin,
        "cos" => Builtin::Cos,
        "tan" => Builtin::Tan,
        "asin" => Builtin::Asin,
        "acos" => Builtin::Acos,
        "atan" => Builtin::Atan,
        "sinh" => Builtin::Sinh,
        "cosh" => Builtin::Cosh,
        "tanh" => Builtin::Tanh,
        _ => return None,
    })
}

/// Real elementary function by source name (`"sin"`, …); `None` if `name`
/// is not one.
pub fn real_named(name: &str, x: f64) -> Option<f64> {
    by_name(name).map(|b| real(b, x))
}

/// Complex elementary function by source name; `None` if `name` is not one.
pub fn complex_named(name: &str, z: C) -> Option<C> {
    by_name(name).map(|b| complex(b, z))
}

/// `a^b` for complex `a`, `b` (principal branch); `0^b` is 0 for `Re b > 0`.
pub fn cpow(a: C, b: C) -> C {
    if a.re == 0.0 && a.im == 0.0 {
        return if b.re > 0.0 {
            C::new(0.0, 0.0)
        } else {
            C::new(f64::NAN, f64::NAN)
        };
    }
    cexp(b * cln(a))
}

fn cexp(z: C) -> C {
    let e = libm::exp(z.re);
    C::new(e * libm::cos(z.im), e * libm::sin(z.im))
}

fn cln(z: C) -> C {
    C::new(libm::log(norm(z)), atan2(z.im, z.re))
}

/// Principal square root without cancellation (Kahan's formulation).
fn csqrt(z: C) -> C {
    if z.re == 0.0 && z.im == 0.0 {
        return C::new(0.0, z.im);
    }
    let t = ((z.re.abs() + norm(z)) * 0.5).sqrt();
    if z.re >= 0.0 {
        C::new(t, z.im / (2.0 * t))
    } else {
        C::new(z.im.abs() / (2.0 * t), t.copysign(z.im))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Case = (Builtin, fn(C) -> C);

    fn close(a: C, b: C) -> bool {
        (a - b).norm() <= 1e-12 * (1.0 + b.norm())
    }

    #[test]
    fn complex_matches_num_complex_within_tolerance() {
        let zs = [
            C::new(0.3, -1.7),
            C::new(-2.5, 0.4),
            C::new(1.0, 0.0),
            C::new(0.0, -3.0),
            C::new(-0.7, -0.2),
        ];
        let cases: [Case; 13] = [
            (Builtin::Sqrt, C::sqrt),
            (Builtin::Exp, C::exp),
            (Builtin::Ln, C::ln),
            (Builtin::Log10, C::log10),
            (Builtin::Sin, C::sin),
            (Builtin::Cos, C::cos),
            (Builtin::Tan, C::tan),
            (Builtin::Sinh, C::sinh),
            (Builtin::Cosh, C::cosh),
            (Builtin::Tanh, C::tanh),
            (Builtin::Asin, C::asin),
            (Builtin::Acos, C::acos),
            (Builtin::Atan, C::atan),
        ];
        for z in zs {
            for (b, reference) in cases {
                let (ours, theirs) = (complex(b, z), reference(z));
                assert!(
                    close(ours, theirs),
                    "{b:?}({z}) = {ours}, num_complex {theirs}"
                );
            }
        }
        assert!(close(
            cpow(C::new(-2.0, 0.5), C::new(0.5, 1.5)),
            C::new(-2.0, 0.5).powc(C::new(0.5, 1.5))
        ));
        assert_eq!(cpow(C::new(0.0, 0.0), C::new(2.0, 0.0)), C::new(0.0, 0.0));
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
