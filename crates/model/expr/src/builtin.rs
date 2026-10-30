//! The builtin function table (spec §5) and the `phys.*` constants: a name →
//! its [`Builtin`] and arity. The compiler checks names and arity against it
//! once; the evaluators dispatch on the [`Builtin`] the op carries.
//!
//! Contents:
//! - `pub(crate) enum Builtin`, `pub(crate) struct BuiltinRow`, `pub(crate) const VARIADIC`
//! - `Builtin::lookup(name: &str) -> Option<&'static BuiltinRow>`,
//!   `Builtin::from_id(id: u8) -> Builtin`, `Builtin::name(self) -> &'static str`,
//!   `Builtin::calc_only(self) -> bool`, `Builtin::elementary(self) -> bool`
//! - `BuiltinRow::check_call(&self, mode, name, argc, span) -> Result<(), Error>`
//! - `pub(crate) fn phys(name: &str) -> Option<f64>`

use crate::program::Mode;
use crate::{Code, Error, Span};

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

/// One row of the builtin table: the source spelling and the accepted
/// argument counts (`max_args` is [`VARIADIC`] for `min`, `max`, `steps`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct BuiltinRow {
    pub name: &'static str,
    pub builtin: Builtin,
    pub min_args: u32,
    pub max_args: u32,
}

/// Maximum-arity marker of a variadic builtin (`min`, `max`, `steps`).
pub(crate) const VARIADIC: u32 = u32::MAX;

/// The builtin table, in discriminant order.
const BUILTINS: [BuiltinRow; 57] = [
    row("abs", Builtin::Abs, 1, 1),
    row("sqrt", Builtin::Sqrt, 1, 1),
    row("exp", Builtin::Exp, 1, 1),
    row("ln", Builtin::Ln, 1, 1),
    row("log10", Builtin::Log10, 1, 1),
    row("sin", Builtin::Sin, 1, 1),
    row("cos", Builtin::Cos, 1, 1),
    row("tan", Builtin::Tan, 1, 1),
    row("asin", Builtin::Asin, 1, 1),
    row("acos", Builtin::Acos, 1, 1),
    row("atan", Builtin::Atan, 1, 1),
    row("atan2", Builtin::Atan2, 2, 2),
    row("sinh", Builtin::Sinh, 1, 1),
    row("cosh", Builtin::Cosh, 1, 1),
    row("tanh", Builtin::Tanh, 1, 1),
    row("floor", Builtin::Floor, 1, 1),
    row("ceil", Builtin::Ceil, 1, 1),
    row("round", Builtin::Round, 1, 1),
    row("int", Builtin::Int, 1, 1),
    row("min", Builtin::Min, 2, VARIADIC),
    row("max", Builtin::Max, 2, VARIADIC),
    row("clamp", Builtin::Clamp, 3, 3),
    row("sign", Builtin::Sign, 1, 1),
    row("pow", Builtin::Pow, 2, 2),
    row("if", Builtin::If, 3, 3),
    row("snap", Builtin::Snap, 2, 2),
    row("eng", Builtin::Eng, 1, 2),
    row("str", Builtin::Str, 1, 1),
    row("num", Builtin::Num, 1, 1),
    row("len", Builtin::Len, 1, 1),
    row("re", Builtin::Re, 1, 1),
    row("im", Builtin::Im, 1, 1),
    row("mag", Builtin::Mag, 1, 1),
    row("ph", Builtin::Ph, 1, 1),
    row("db", Builtin::Db, 1, 1),
    row("conj", Builtin::Conj, 1, 1),
    row("cplx", Builtin::Cplx, 2, 2),
    row("spectre_source", Builtin::SpectreSource, 1, 1),
    row("v", Builtin::V, 1, 1),
    row("i", Builtin::I, 1, 1),
    row("x", Builtin::X, 0, 0),
    row("deriv", Builtin::Deriv, 1, 1),
    row("integ", Builtin::Integ, 1, 1),
    row("avg", Builtin::Avg, 1, 1),
    row("rms", Builtin::Rms, 1, 1),
    row("ymax", Builtin::Ymax, 1, 1),
    row("ymin", Builtin::Ymin, 1, 1),
    row("pp", Builtin::Pp, 1, 1),
    row("at", Builtin::At, 2, 2),
    row("cross", Builtin::Cross, 2, 4),
    row("delay", Builtin::Delay, 4, 4),
    row("settle", Builtin::Settle, 2, 2),
    row("fft", Builtin::Fft, 1, 1),
    row("gain_margin", Builtin::GainMargin, 1, 1),
    row("phase_margin", Builtin::PhaseMargin, 1, 1),
    row("op", Builtin::Op, 2, 2),
    row("steps", Builtin::Steps, 4, VARIADIC),
];

// The table is in discriminant order, so `from_id` indexes it directly, and
// every row's arity range is non-empty.
const _: () = {
    let mut k = 0;
    while k < BUILTINS.len() {
        assert!(BUILTINS[k].builtin as usize == k);
        assert!(BUILTINS[k].min_args <= BUILTINS[k].max_args);
        k += 1;
    }
};
// A call op's `sub` byte holds the discriminant.
const _: () = assert!(BUILTINS.len() <= u8::MAX as usize + 1);

impl Builtin {
    /// The row of the builtin spelled `name`; `V` and `I` are the only
    /// case-insensitive aliases (SPICE habit).
    pub(crate) fn lookup(name: &str) -> Option<&'static BuiltinRow> {
        let name = match name {
            "V" => "v",
            "I" => "i",
            other => other,
        };
        let row = BUILTINS.iter().find(|row| row.name == name)?;
        debug_assert!(row.min_args <= row.max_args);
        debug_assert_eq!(BUILTINS[row.builtin as usize].name, name);
        Some(row)
    }

    /// The builtin with discriminant `id` (a call op's `sub`).
    ///
    /// # Panics
    /// If `id` is not a discriminant (a compiler bug).
    pub(crate) fn from_id(id: u8) -> Builtin {
        assert!(
            usize::from(id) < BUILTINS.len(),
            "call op names builtin {id}, past the table"
        );
        let builtin = BUILTINS[usize::from(id)].builtin;
        debug_assert_eq!(builtin as u8, id);
        builtin
    }

    /// The source spelling, for error messages.
    pub(crate) fn name(self) -> &'static str {
        let row = &BUILTINS[self as usize];
        debug_assert_eq!(row.builtin, self);
        row.name
    }

    /// True for functions that need columns (spec §5 calculator-only).
    pub(crate) fn calc_only(self) -> bool {
        // `steps` was appended after the calculator block (discriminants are stable).
        let calc_only = self as u8 >= Builtin::V as u8 && self != Builtin::Steps;
        // An elementary function is never calculator-only.
        if calc_only {
            debug_assert!(!self.elementary());
        }
        calc_only
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

impl BuiltinRow {
    /// Checks a call of this builtin, spelled `name`, with `argc` arguments in `mode`.
    ///
    /// # Errors
    /// `X008` when `argc` is outside the row's arity; `X006` for a calculator-only
    /// builtin in [`Mode::Scalar`].
    pub(crate) fn check_call(
        &self,
        mode: Mode,
        name: &str,
        argc: u32,
        span: Span,
    ) -> Result<(), Error> {
        let (lo, hi) = (self.min_args, self.max_args);
        debug_assert!(lo <= hi);
        debug_assert!(
            name.eq_ignore_ascii_case(self.name),
            "`{name}` checked against `{}`",
            self.name
        );
        if argc < lo || argc > hi {
            let want = match (lo, hi) {
                (lo, hi) if lo == hi => format!("{lo}"),
                (lo, VARIADIC) => format!("at least {lo}"),
                (lo, hi) => format!("{lo} to {hi}"),
            };
            return Err(Error::new(
                Code::X008,
                span,
                format!("`{name}` takes {want} arguments, got {argc}"),
            ));
        }
        if self.builtin.calc_only() && mode == Mode::Scalar {
            return Err(Error::new(
                Code::X006,
                span,
                format!("`{name}` is only available in calculator mode"),
            ));
        }
        Ok(())
    }
}

/// The `phys.*` constant `name` (spec §5; SI 2019 exact values, CODATA 2022
/// for eps0), or `None` (`X004` at the caller).
pub(crate) fn phys(name: &str) -> Option<f64> {
    let value: f64 = match name {
        "k" => 1.380_649e-23,
        "q" => 1.602_176_634e-19,
        "h" => 6.626_070_15e-34,
        "c" => 299_792_458.0,
        "eps0" => 8.854_187_818_8e-12,
        "kelvin0" => 273.15,
        _ => return None,
    };
    // Every physical constant is a positive finite number.
    debug_assert!(value > 0.0);
    debug_assert!(value.is_finite());
    Some(value)
}

// ---- private ----

/// `(name, builtin, min args, max args)` as a table row.
const fn row(name: &'static str, builtin: Builtin, min_args: u32, max_args: u32) -> BuiltinRow {
    BuiltinRow {
        name,
        builtin,
        min_args,
        max_args,
    }
}
