//! The builtin function table (spec §5) and the `phys.*` constants: a name →
//! its [`Builtin`] and arity. The compiler checks names and arity against it
//! once; the evaluators dispatch on the [`Builtin`] the op carries.
//!
//! Contents:
//! - `pub(crate) enum Builtin`, `pub(crate) const VARIADIC`
//! - `Builtin::lookup(name: &str) -> Option<(Builtin, u32, u32)>`,
//!   `Builtin::from_id(id: u8) -> Builtin`, `Builtin::name(self) -> &'static str`,
//!   `Builtin::calc_only(self) -> bool`, `Builtin::elementary(self) -> bool`
//! - `pub(crate) fn phys(name: &str) -> Option<f64>`

/// Builtin functions (spec §5). Discriminants index [`BUILTINS`] and are the
/// `sub` byte of a call op; everything from `V` up to `Op` is calculator-only
/// (`Steps`, appended later, is scalar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    Abs,
    Sqrt,
    Exp,
    Ln,
    Log10,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Sinh,
    Cosh,
    Tanh,
    Floor,
    Ceil,
    Round,
    Int,
    Min,
    Max,
    Clamp,
    Sign,
    Pow,
    If,
    Snap,
    Eng,
    Str,
    Num,
    Len,
    Re,
    Im,
    Mag,
    Ph,
    Db,
    Conj,
    Cplx,
    SpectreSource,
    V,
    I,
    X,
    Deriv,
    Integ,
    Avg,
    Rms,
    Ymax,
    Ymin,
    Pp,
    At,
    Cross,
    Delay,
    Settle,
    Fft,
    GainMargin,
    PhaseMargin,
    Op,
    Steps,
}

/// Maximum-arity marker of a variadic builtin (`min`, `max`, `steps`).
pub(crate) const VARIADIC: u32 = u32::MAX;

/// `(name, builtin, min args, max args)`, in discriminant order.
const BUILTINS: [(&str, Builtin, u32, u32); 57] = [
    ("abs", Builtin::Abs, 1, 1),
    ("sqrt", Builtin::Sqrt, 1, 1),
    ("exp", Builtin::Exp, 1, 1),
    ("ln", Builtin::Ln, 1, 1),
    ("log10", Builtin::Log10, 1, 1),
    ("sin", Builtin::Sin, 1, 1),
    ("cos", Builtin::Cos, 1, 1),
    ("tan", Builtin::Tan, 1, 1),
    ("asin", Builtin::Asin, 1, 1),
    ("acos", Builtin::Acos, 1, 1),
    ("atan", Builtin::Atan, 1, 1),
    ("atan2", Builtin::Atan2, 2, 2),
    ("sinh", Builtin::Sinh, 1, 1),
    ("cosh", Builtin::Cosh, 1, 1),
    ("tanh", Builtin::Tanh, 1, 1),
    ("floor", Builtin::Floor, 1, 1),
    ("ceil", Builtin::Ceil, 1, 1),
    ("round", Builtin::Round, 1, 1),
    ("int", Builtin::Int, 1, 1),
    ("min", Builtin::Min, 2, VARIADIC),
    ("max", Builtin::Max, 2, VARIADIC),
    ("clamp", Builtin::Clamp, 3, 3),
    ("sign", Builtin::Sign, 1, 1),
    ("pow", Builtin::Pow, 2, 2),
    ("if", Builtin::If, 3, 3),
    ("snap", Builtin::Snap, 2, 2),
    ("eng", Builtin::Eng, 1, 2),
    ("str", Builtin::Str, 1, 1),
    ("num", Builtin::Num, 1, 1),
    ("len", Builtin::Len, 1, 1),
    ("re", Builtin::Re, 1, 1),
    ("im", Builtin::Im, 1, 1),
    ("mag", Builtin::Mag, 1, 1),
    ("ph", Builtin::Ph, 1, 1),
    ("db", Builtin::Db, 1, 1),
    ("conj", Builtin::Conj, 1, 1),
    ("cplx", Builtin::Cplx, 2, 2),
    ("spectre_source", Builtin::SpectreSource, 1, 1),
    ("v", Builtin::V, 1, 1),
    ("i", Builtin::I, 1, 1),
    ("x", Builtin::X, 0, 0),
    ("deriv", Builtin::Deriv, 1, 1),
    ("integ", Builtin::Integ, 1, 1),
    ("avg", Builtin::Avg, 1, 1),
    ("rms", Builtin::Rms, 1, 1),
    ("ymax", Builtin::Ymax, 1, 1),
    ("ymin", Builtin::Ymin, 1, 1),
    ("pp", Builtin::Pp, 1, 1),
    ("at", Builtin::At, 2, 2),
    ("cross", Builtin::Cross, 2, 4),
    ("delay", Builtin::Delay, 4, 4),
    ("settle", Builtin::Settle, 2, 2),
    ("fft", Builtin::Fft, 1, 1),
    ("gain_margin", Builtin::GainMargin, 1, 1),
    ("phase_margin", Builtin::PhaseMargin, 1, 1),
    ("op", Builtin::Op, 2, 2),
    ("steps", Builtin::Steps, 4, VARIADIC),
];

// The table is in discriminant order, so `from_id` indexes it directly.
const _: () = {
    let mut k = 0;
    while k < BUILTINS.len() {
        assert!(BUILTINS[k].1 as usize == k);
        assert!(BUILTINS[k].2 <= BUILTINS[k].3);
        k += 1;
    }
};

impl Builtin {
    /// The builtin spelled `name` and its `(min, max)` argument count; `V` and
    /// `I` are the only case-insensitive aliases (SPICE habit).
    pub(crate) fn lookup(name: &str) -> Option<(Builtin, u32, u32)> {
        let name = match name {
            "V" => "v",
            "I" => "i",
            n => n,
        };
        let row = BUILTINS.iter().find(|row| row.0 == name)?;
        debug_assert!(row.2 <= row.3);
        Some((row.1, row.2, row.3))
    }

    /// The builtin with discriminant `id` (a call op's `sub`).
    ///
    /// # Panics
    /// If `id` is not a discriminant (a compiler bug).
    pub(crate) fn from_id(id: u8) -> Builtin {
        let b = BUILTINS[id as usize].1;
        debug_assert_eq!(b as u8, id);
        b
    }

    /// The source spelling, for error messages.
    pub(crate) fn name(self) -> &'static str {
        BUILTINS[self as usize].0
    }

    /// True for functions that need columns (spec §5 calculator-only).
    pub(crate) fn calc_only(self) -> bool {
        // `steps` was appended after the calculator block (discriminants are stable).
        self as u8 >= Builtin::V as u8 && self != Builtin::Steps
    }

    /// True for the real/complex elementary functions of `dmath`.
    pub(crate) fn elementary(self) -> bool {
        use Builtin::*;
        matches!(
            self,
            Sqrt | Exp | Ln | Log10 | Sin | Cos | Tan | Asin | Acos | Atan | Sinh | Cosh | Tanh
        )
    }
}

/// The `phys.*` constant `name` (spec §5; SI 2019 exact values, CODATA 2022
/// for eps0), or `None` (`X004` at the caller).
pub(crate) fn phys(name: &str) -> Option<f64> {
    Some(match name {
        "k" => 1.380_649e-23,
        "q" => 1.602_176_634e-19,
        "h" => 6.626_070_15e-34,
        "c" => 299_792_458.0,
        "eps0" => 8.854_187_818_8e-12,
        "kelvin0" => 273.15,
        _ => return None,
    })
}
