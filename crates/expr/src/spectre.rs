//! `spectre_source(s)` (spec §5): a SPICE independent-source specification
//! (`dc 1 ac 1 0 pulse(0 1.8 1n …)`) as Spectre `vsource`/`isource` parameters
//! (`dc=1 mag=1 phase=0 type=pulse val0=0 val1=1.8 delay=1e-9 …`).
//!
//! Numbers are rewritten in canonical text (§7) because the SI suffixes of the
//! two languages disagree (`1M` is milli in SPICE, mega in Spectre; Spectre has
//! no `meg`). Parameter names: docs/research/spectre-netlist.md.

use core::fmt::Write;

use crate::text::{fmt_num, parse_num};
use crate::{Code, Error, Span};

/// Transient functions: SPICE name, Spectre `type`, Spectre names of the
/// positional arguments (at most this many; at least two).
const FUNCS: [(&str, &str, &[&str]); 4] = [
    (
        "pulse",
        "pulse",
        &["val0", "val1", "delay", "rise", "fall", "width", "period"],
    ),
    (
        "sin",
        "sine",
        &["sinedc", "ampl", "freq", "delay", "damp", "sinephase"],
    ),
    (
        "exp",
        "exp",
        &["val0", "val1", "td1", "tau1", "td2", "tau2"],
    ),
    ("pwl", "pwl", &[]),
];

/// Most tokens one specification may hold (a PWL table's points included).
const TOKENS_MAX: usize = 1 << 16;

fn bad(spec: &str, why: impl core::fmt::Display) -> Error {
    Error::new(
        Code::X009,
        Span::default(),
        format!("spectre_source: cannot translate {spec:?}: {why}"),
    )
}

/// Appends one value: a number in canonical text, or an identifier (a bench
/// parameter) verbatim.
fn value(spec: &str, tok: &str, out: &mut String) -> Result<(), Error> {
    if let Ok(x) = parse_num(tok) {
        return fmt_num(out, x).map_err(|_| bad(spec, "write"));
    }
    let ident = tok
        .bytes()
        .enumerate()
        .all(|(k, c)| c == b'_' || c.is_ascii_alphabetic() || (k > 0 && c.is_ascii_digit()));
    if !ident || tok.is_empty() {
        return Err(bad(spec, format_args!("`{tok}` is not a number")));
    }
    out.push_str(tok);
    Ok(())
}

fn is_value(tok: &str) -> bool {
    tok != "(" && tok != ")" && !keyword(tok)
}

fn keyword(tok: &str) -> bool {
    tok.eq_ignore_ascii_case("dc")
        || tok.eq_ignore_ascii_case("ac")
        || FUNCS.iter().any(|f| tok.eq_ignore_ascii_case(f.0))
}

/// Translates `spec` (grammar: `[[dc] v] [ac mag [phase]] [fn(args)]`, the
/// parts in any order, each at most once, keywords case-insensitive, args
/// separated by blanks or commas) and appends the Spectre parameters to `out`:
/// `dc=`, `mag=`, `phase=`, then `type=` and the function's parameters. An
/// empty `spec` appends nothing (Spectre's defaults: 0).
///
/// # Errors
/// `X009` naming the spec for anything else: an unknown function (`sffm`,
/// `am`, …), a PWL option, more arguments than the function has, fewer than
/// two, an odd PWL table, a repeated part, a token that is neither a number
/// nor an identifier. `out` is then unspecified past its length at entry.
pub(crate) fn source(spec: &str, out: &mut String) -> Result<(), Error> {
    let mut toks: Vec<&str> = Vec::new();
    for word in spec.split(|c: char| c.is_whitespace() || c == ',') {
        let mut rest = word;
        while !rest.is_empty() {
            let cut = rest.find(['(', ')']).unwrap_or(rest.len());
            let (head, tail) = rest.split_at(if cut == 0 { 1 } else { cut });
            toks.push(head);
            rest = tail;
        }
        if toks.len() > TOKENS_MAX {
            return Err(bad(spec, "too long"));
        }
    }
    let start = out.len();
    let (mut dc, mut ac, mut tran) = (false, false, false);
    let mut i = 0;
    let sep = |out: &mut String| {
        if out.len() > start {
            out.push(' ');
        }
    };
    while i < toks.len() {
        let t = toks[i];
        if t.eq_ignore_ascii_case("dc") || (is_value(t) && !dc && i == 0) {
            let v = if t.eq_ignore_ascii_case("dc") {
                i + 1
            } else {
                i
            };
            let Some(tok) = toks.get(v).copied().filter(|t| is_value(t)) else {
                return Err(bad(spec, "dc needs a value"));
            };
            if dc {
                return Err(bad(spec, "two dc values"));
            }
            dc = true;
            sep(out);
            out.push_str("dc=");
            value(spec, tok, out)?;
            i = v + 1;
        } else if t.eq_ignore_ascii_case("ac") {
            if ac {
                return Err(bad(spec, "two ac parts"));
            }
            ac = true;
            let Some(mag) = toks.get(i + 1).copied().filter(|t| is_value(t)) else {
                return Err(bad(spec, "ac needs a magnitude"));
            };
            sep(out);
            out.push_str("mag=");
            value(spec, mag, out)?;
            i += 2;
            if let Some(ph) = toks.get(i).copied().filter(|t| is_value(t)) {
                out.push_str(" phase=");
                value(spec, ph, out)?;
                i += 1;
            }
        } else if let Some(&(_, ty, names)) = FUNCS.iter().find(|f| t.eq_ignore_ascii_case(f.0)) {
            if tran {
                return Err(bad(spec, "two transient functions"));
            }
            tran = true;
            if toks.get(i + 1) != Some(&"(") {
                return Err(bad(
                    spec,
                    format_args!("`{t}` needs its arguments in parentheses"),
                ));
            }
            let close = toks[i + 2..]
                .iter()
                .position(|t| *t == ")")
                .map(|k| i + 2 + k)
                .ok_or_else(|| bad(spec, "unclosed `(`"))?;
            let args = &toks[i + 2..close];
            if let Some(a) = args.iter().find(|a| !is_value(a)) {
                return Err(bad(spec, format_args!("`{a}` inside `{t}(…)`")));
            }
            let n = args.len();
            if n < 2 || (ty == "pwl" && !n.is_multiple_of(2)) || (!names.is_empty() && n > names.len()) {
                let want = if ty == "pwl" {
                    "time-value pairs".to_owned()
                } else {
                    format!("2 to {} arguments", names.len())
                };
                return Err(bad(spec, format_args!("`{t}` takes {want}, got {n}")));
            }
            sep(out);
            write!(out, "type={ty}").map_err(|_| bad(spec, "write"))?;
            if ty == "pwl" {
                out.push_str(" wave=[");
                for (k, a) in args.iter().enumerate() {
                    if k > 0 {
                        out.push(' ');
                    }
                    value(spec, a, out)?;
                }
                out.push(']');
            } else {
                for (name, a) in names.iter().zip(args) {
                    write!(out, " {name}=").map_err(|_| bad(spec, "write"))?;
                    value(spec, a, out)?;
                }
            }
            i = close + 1;
        } else {
            return Err(bad(spec, format_args!("unexpected `{t}`")));
        }
    }
    assert!(i == toks.len());
    debug_assert!(!out[start..].contains("  "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::source;

    fn ok(spec: &str) -> String {
        let mut s = String::new();
        source(spec, &mut s).unwrap_or_else(|e| panic!("{spec}: {e}"));
        s
    }

    #[test]
    fn translates_every_part() {
        assert_eq!(ok(""), "");
        assert_eq!(ok("0.9"), "dc=0.9");
        assert_eq!(ok("DC 1.8 AC 1"), "dc=1.8 mag=1");
        assert_eq!(ok("dc 0 ac 1 90"), "dc=0 mag=1 phase=90");
        assert_eq!(ok("ac 1"), "mag=1");
        assert_eq!(ok("vdd"), "dc=vdd");
        assert_eq!(
            ok("pulse(0 1.8 1n 10p 10p 5n 10n)"),
            "type=pulse val0=0 val1=1.8 delay=1e-9 rise=1e-11 fall=1e-11 width=5e-9 period=1e-8"
        );
        assert_eq!(
            ok("dc 0.9 sin(0.9, 1m, 1meg)"),
            "dc=0.9 type=sine sinedc=0.9 ampl=0.001 freq=1000000"
        );
        assert_eq!(
            ok("exp(0 1 1n 2n 5n 1n)"),
            "type=exp val0=0 val1=1 td1=1e-9 tau1=2e-9 td2=5e-9 tau2=1e-9"
        );
        assert_eq!(ok("PWL (0 0 1n 1.8)"), "type=pwl wave=[0 0 1e-9 1.8]");
        // SPICE `M` is milli: canonical text keeps it so (Spectre `M` is mega).
        assert_eq!(ok("dc 2M"), "dc=0.002");
    }

    #[test]
    fn refuses_what_spectre_cannot_say() {
        for spec in [
            "sffm(0 1 1k 5 100)",
            "pwl(0 0 1n)",
            "pwl(0 0 1n 1 r=0)",
            "pulse(0 1 0 1n 1n 5n 10n 3)",
            "sin(0)",
            "dc 1 dc 2",
            "ac",
            "pulse 0 1",
            "pulse(0 1",
            "dc 1 'a*2'",
            "1 2",
        ] {
            let mut s = String::new();
            let e = source(spec, &mut s).expect_err(spec);
            assert_eq!(e.code, crate::Code::X009, "{spec}");
            assert!(e.msg.contains("spectre_source"), "{spec}: {e}");
        }
    }
}
