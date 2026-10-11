//! Values for the names a reference netlist uses but never defines
//! ([`crate::Options::bind_free`]).
//!
//! xschem netlisting a component as the top level keeps its parameters as
//! names with no `.param` (`M2 … p w=WP`), while Schemify evaluates them to
//! the component defaults (`w=4.5e-5`). Binding `WP` to the value the other
//! netlist uses and then comparing *everything* again asks the honest
//! question: is there one assignment of the reference's free names under
//! which the two netlists are equivalent? Inference only proposes values;
//! the verdict is the full comparison under them, so an inconsistent or
//! wrong proposal still compares different.

use std::collections::HashMap;

use crate::eval::{Lex, eval, is_key, lex, parse_number};
use crate::flat::Flat;
use crate::parse::Netlist;

/// Words that are SPICE syntax in source specs, never parameters.
const KEYWORDS: [&str; 6] = ["dc", "ac", "pi", "temper", "time", "hertz"];

/// Proposes values for `a`'s free names (used in a device, `.param` or
/// `.model` value but not defined by a top-level `.param` of `a`), in this
/// order: `b`'s top-level `.param` of the same name; then, over `a`'s
/// devices in flat order, the same-named device of `b` (same letter and
/// model) whose value in the same slot matches `a`'s text with the free name
/// read as a number, or solves `a`'s expression when it is affine in one free
/// name. The first proposal per name wins. Cost: O(devices × slot length).
pub(crate) fn infer(na: &Netlist, fa: &Flat, fb: &Flat) -> Vec<(String, f64)> {
    debug_assert_eq!(fa.dev_name.len(), fa.dev_text.len());
    debug_assert_eq!(fb.dev_name.len(), fb.dev_text.len());
    let defined: Vec<&str> = na.top.params.iter().map(|(k, _)| k.as_str()).collect();
    let free = |n: &str| !defined.contains(&n) && !KEYWORDS.contains(&n);

    // Free names `a` actually uses: only those may be bound from `b`'s params.
    // Slots skip the letter and model of a device, the name and kind of a card.
    let mut used: Vec<&str> = Vec::new();
    let slots = (fa.params.iter().map(|p| (p, 0)))
        .chain(fa.models.iter().map(|m| (m, 3)))
        .chain(fa.dev_text.iter().map(|t| (t, 2)))
        .flat_map(|(text, skip)| text.split(' ').skip(skip));
    for s in slots {
        let l = lex(s);
        for (i, x) in l.iter().enumerate() {
            let key = is_key(&l, i);
            if let Lex::Name(n) = *x
                && !key
                && free(n)
                && !used.contains(&n)
            {
                used.push(n);
            }
        }
    }
    let mut bound: Vec<(String, f64)> = Vec::new();
    let bind = |n: &str, v: f64, bound: &mut Vec<(String, f64)>| {
        if v.is_finite() && !bound.iter().any(|(k, _)| k == n) {
            bound.push((n.to_string(), v));
        }
    };
    for p in &fb.params {
        if let Some((k, v)) = p.split_once('=')
            && used.contains(&k)
            && let Some(x) = parse_number(v)
        {
            bind(k, x, &mut bound);
        }
    }

    let b_index: HashMap<&str, usize> = fb
        .dev_name
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    for (d, name) in fa.dev_name.iter().enumerate() {
        let Some(&e) = b_index.get(name.as_str()) else {
            continue;
        };
        let ta: Vec<&str> = fa.dev_text[d].split(' ').collect();
        let tb: Vec<&str> = fb.dev_text[e].split(' ').collect();
        // Letter and model must agree; the slots must line up one to one.
        if ta.len() != tb.len() || ta.len() < 2 || ta[..2] != tb[..2] {
            continue;
        }
        for (sa, sb) in ta[2..].iter().zip(&tb[2..]) {
            if sa == sb {
                continue;
            }
            let (va, vb) = match (sa.split_once('='), sb.split_once('=')) {
                (Some((ka, va)), Some((kb, vb))) if ka == kb => (va, vb),
                (None, None) => (*sa, *sb),
                _ => continue,
            };
            for (n, v) in slot(va, vb, &free) {
                bind(n, v, &mut bound);
            }
        }
    }
    debug_assert!(bound.iter().all(|(k, _)| free(k)));
    bound
}

/// Proposals from one slot: `a`'s text `va` against `b`'s `vb`.
fn slot<'a>(va: &'a str, vb: &str, free: &impl Fn(&str) -> bool) -> Vec<(&'a str, f64)> {
    let (la, lb) = (lex(va), lex(vb));
    // Same shape: every differing lexeme is a free name of `a` against a number.
    if la.len() == lb.len() {
        let mut out = Vec::new();
        for (x, y) in la.iter().zip(&lb) {
            match (x, y) {
                _ if x == y => {}
                (Lex::Name(n), Lex::Num(v)) if free(n) => out.push((*n, *v)),
                _ => return Vec::new(),
            }
        }
        return out;
    }
    // `res/2` against `5e3`: solve when affine in its one free name.
    let Some(target) = parse_number(vb) else {
        return Vec::new();
    };
    let mut names = la.iter().filter_map(|l| match l {
        Lex::Name(n) => Some(*n),
        _ => None,
    });
    let Some(n) = names.next() else {
        return Vec::new();
    };
    if !free(n) || names.any(|m| m != n) {
        return Vec::new();
    }
    let at = |t: f64| eval(va, &|m: &str| (m == n).then_some(t));
    let (Some(f0), Some(f1), Some(f2)) = (at(0.0), at(1.0), at(2.0)) else {
        return Vec::new();
    };
    let slope = f1 - f0;
    let affine = (f2 - 2.0 * f1 + f0).abs() <= 1e-9 * (f0.abs() + f1.abs() + f2.abs());
    if !affine || slope == 0.0 {
        return Vec::new();
    }
    let x = (target - f0) / slope;
    // Accept only a solution that reproduces the target (rounding included).
    match at(x) {
        Some(y) if (y - target).abs() <= 1e-9 * target.abs().max(1e-30) => vec![(n, x)],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any(_: &str) -> bool {
        true
    }

    #[test]
    fn slots_bind_names_against_numbers() {
        assert_eq!(slot("wp", "4.5e-5", &any), [("wp", 4.5e-5)]);
        assert_eq!(slot("v(a)/ron", "v(a)/1e-1", &any), [("ron", 0.1)]);
        let solved = slot("res/2", "5e3", &any);
        assert_eq!(solved.len(), 1);
        assert!((solved[0].1 - 1e4).abs() < 1e-6, "{solved:?}");
        // Not affine, two names, or a differing non-name lexeme: no proposal.
        assert!(slot("w*w", "4e0", &any).is_empty());
        assert!(slot("a+b", "3e0", &any).is_empty());
        assert!(slot("v(a)/ron", "v(b)/1e-1", &any).is_empty());
        assert!(slot("wp", "4.5e-5", &|_| false).is_empty());
    }
}
