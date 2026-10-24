//! Sweep points (feature `sweep`, run spec §2): `values <expr>…` kept verbatim, `lin`
//! and `log` expanded to canonical number text.
//!
//! `log` points use the expression language's `^` so they are bit-identical on every
//! target (D32: the platform `powf` may differ by an ulp).

use crate::plan::{SWEEP_POINTS_MAX, const_value, parse_count};
use schemify_expr::{Mode, Program, Scratch, Value, write_num};

/// Expands one sweep's `[values, lin, log]` attribute values (exactly one present).
///
/// # Errors
///
/// No shape or several; `values` entries that do not compile; `lin`/`log` bounds that
/// are not constants; a point count outside `1..=SWEEP_POINTS_MAX`; `log` bounds that
/// are zero or of different signs.
pub(crate) fn points(shapes: &[Option<Vec<&str>>; 3]) -> Result<Vec<String>, String> {
    let present = shapes.iter().filter(|s| s.is_some()).count();
    if present != 1 {
        return Err("needs exactly one of `values`, `lin`, `log`".into());
    }
    let out = match shapes {
        [Some(values), None, None] => {
            for v in values {
                Program::compile(v, Mode::Scalar).map_err(|e| format!("values: {e}"))?;
            }
            values.iter().map(|v| (*v).to_owned()).collect()
        }
        [None, Some(a), None] => grid(a, false)?,
        [None, None, Some(a)] => grid(a, true)?,
        _ => unreachable!("exactly one shape is present"),
    };
    assert!(!out.is_empty() && out.len() <= SWEEP_POINTS_MAX as usize);
    Ok(out)
}

/// `lin|log <from> <to> <n>`: `n` points, both ends included (`n = 1` is `from`).
fn grid(a: &[&str], log: bool) -> Result<Vec<String>, String> {
    assert_eq!(a.len(), 3, "the format checks the arity");
    let (from, to) = (const_value(a[0])?, const_value(a[1])?);
    let n = parse_count(a[2], SWEEP_POINTS_MAX)?;
    if log && (from == 0.0 || to == 0.0 || (from < 0.0) != (to < 0.0)) {
        return Err(format!(
            "log {from} {to}: bounds must be nonzero with one sign"
        ));
    }
    // `from * (to/from)^t`, compiled once; idents are bound in first-occurrence order.
    let geo = Program::compile("a * (b / a) ^ t", Mode::Scalar).expect("constant source");
    let mut scratch = Scratch::default();
    let mut out = Vec::with_capacity(n as usize);
    for k in 0..n {
        let t = if n == 1 {
            0.0
        } else {
            f64::from(k) / f64::from(n - 1)
        };
        let x = match (k + 1 == n && n > 1, log) {
            (true, _) => to,
            (false, false) => from + (to - from) * t,
            (false, true) => {
                let inputs = [Value::Num(from), Value::Num(to), Value::Num(t)];
                match geo.eval(&inputs, &mut scratch) {
                    Ok(Value::Num(x)) => x,
                    other => return Err(format!("log point {k}: {other:?}")),
                }
            }
        };
        let mut text = String::new();
        write_num(&mut text, x);
        out.push(text);
    }
    assert_eq!(out.len(), n as usize);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(shape: usize, a: &[&str]) -> Result<Vec<String>, String> {
        let mut s: [Option<Vec<&str>>; 3] = [None, None, None];
        s[shape] = Some(a.to_vec());
        points(&s)
    }

    #[test]
    fn lin_log_values() {
        assert_eq!(p(1, &["1k", "3k", "3"]).unwrap(), ["1000", "2000", "3000"]);
        assert_eq!(p(2, &["1", "100", "3"]).unwrap(), ["1", "10", "100"]);
        assert_eq!(p(1, &["5", "9", "1"]).unwrap(), ["5"]);
        assert_eq!(p(0, &["1k", "2*R"]).unwrap(), ["1k", "2*R"]);
    }

    #[test]
    fn rejects_bad_shapes() {
        assert!(p(2, &["0", "10", "3"]).is_err());
        assert!(p(2, &["-1", "10", "3"]).is_err());
        assert!(p(1, &["1", "2", "0"]).is_err());
        assert!(p(1, &["1", "R", "3"]).is_err(), "bounds are constants");
        assert!(p(0, &["1 +"]).is_err());
        assert!(points(&[None, None, None]).is_err());
        assert!(points(&[Some(vec!["1"]), Some(vec!["1", "2", "2"]), None]).is_err());
    }
}
