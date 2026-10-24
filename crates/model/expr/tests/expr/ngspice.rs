//! Differential vs ngspice `.param` evaluation on the SPICE-compatible subset
//! (arithmetic, `%`, `^`, SI suffixes except `mil`, common functions).
//!
//! Ignored by default; needs `ngspice` on PATH and skips (passing) without it:
//! `cargo test -p schemify-expr -- --ignored ngspice`.
//! Expressions are printed fully parenthesized because the two grammars
//! differ on `^` associativity (ngspice is left-associative). Values pass
//! through ngspice's operating-point solve, which adds about one ulp, hence
//! the relative tolerance.

use std::fmt::Write;
use std::process::Command;

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

const SUFFIXES: [&str; 15] = [
    "", "", "", "f", "p", "n", "u", "m", "k", "meg", "g", "t", "K", "MEG", "Meg",
];
const FUNCS: [&str; 15] = [
    "abs", "sqrt", "exp", "ln", "log10", "sin", "cos", "tan", "atan", "sinh", "cosh", "tanh",
    "floor", "ceil", "int",
];

fn literal(r: &mut Rng) -> String {
    let m = r.below(100_000);
    let mant = match r.below(4) {
        0 => m.to_string(),
        1 => format!("{}.{:02}", m / 100, m % 100),
        2 => format!(".{m}"),
        _ => format!("{}e{}", m % 1000, r.below(9) as i64 - 4),
    };
    format!(
        "{mant}{}",
        SUFFIXES[r.below(SUFFIXES.len() as u64) as usize]
    )
}

/// A fully parenthesized expression, valid in both languages.
fn random_expr(r: &mut Rng, depth: u32) -> String {
    if depth == 0 || r.below(4) == 0 {
        return literal(r);
    }
    match r.below(10) {
        0 => format!("(-{})", random_expr(r, depth - 1)),
        1 => format!(
            "{}({})",
            FUNCS[r.below(FUNCS.len() as u64) as usize],
            random_expr(r, depth - 1)
        ),
        2 => format!(
            "{}({}, {})",
            ["min", "max"][r.below(2) as usize],
            random_expr(r, depth - 1),
            random_expr(r, depth - 1)
        ),
        // ngspice's `.param` `^` computes |x|^y (so `(-2)^3` is 8 there); ours
        // is the real power. The shared subset keeps the base non-negative.
        3 => format!("((abs({}))^{})", random_expr(r, depth - 1), r.below(4)),
        // ngspice's `%` loses precision on large operands (ours is exact
        // fmod), so the shared subset only uses it on small plain literals.
        4 => format!(
            "({}.{} % {}.5)",
            1 + r.below(999),
            r.below(10),
            1 + r.below(99)
        ),
        _ => {
            let op = ["+", "-", "*", "/"][r.below(4) as usize];
            format!(
                "(({}) {op} ({}))",
                random_expr(r, depth - 1),
                random_expr(r, depth - 1)
            )
        }
    }
}

#[test]
#[ignore = "needs ngspice on PATH; run with --ignored"]
fn params_agree_with_ngspice() {
    if Command::new("ngspice").arg("--version").output().is_err() {
        eprintln!("ngspice not on PATH; skipped");
        return;
    }
    let mut r = Rng(0x5eed_1234_abcd_0001);
    let mut cases = Vec::new();
    while cases.len() < 400 {
        let src = random_expr(&mut r, 4);
        if let Ok(v) = crate::eval(&src) {
            let v = match v {
                crate::Owned::Int(i) => i as f64,
                crate::Owned::Num(x) => x,
                o => panic!("{src}: {o:?}"),
            };
            if v.abs() < 1e200 && (v == 0.0 || v.abs() > 1e-200) {
                cases.push((src, v));
            }
        }
    }
    let mut net = String::from("* schemify-expr differential\n");
    for (i, (src, _)) in cases.iter().enumerate() {
        writeln!(net, ".param p{i} = {{{src}}}\nV{i} n{i} 0 {{p{i}}}").unwrap();
    }
    net.push_str(".control\nset numdgt=17\nop\n");
    for i in 0..cases.len() {
        writeln!(net, "print v(n{i})").unwrap();
    }
    net.push_str(".endc\n.end\n");
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("expr_ngspice_diff.cir");
    std::fs::write(&path, net).unwrap();
    let out = Command::new("ngspice")
        .arg("-b")
        .arg(&path)
        .output()
        .expect("ngspice runs");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut got = vec![None; cases.len()];
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("v(n") else {
            continue;
        };
        let Some((idx, val)) = rest.split_once(") = ") else {
            continue;
        };
        got[idx.parse::<usize>().unwrap()] = val.trim().parse::<f64>().ok();
    }
    let mut bad = Vec::new();
    for (i, (src, want)) in cases.iter().enumerate() {
        match got[i] {
            Some(g) if (g - want).abs() <= 1e-9 * g.abs().max(want.abs()) + 1e-300 => {}
            other => bad.push(format!("{src}: ours {want:e}, ngspice {other:?}")),
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} disagree:\n{}\n--- ngspice stderr ---\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n"),
        String::from_utf8_lossy(&out.stderr)
    );
}
