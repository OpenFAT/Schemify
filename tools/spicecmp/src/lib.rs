//! Semantic SPICE/CDL netlist comparison: the `netlist` oracle of TESTING.md.
//!
//! Two netlists are **equivalent** when all six sections agree:
//!
//! 1. **Connectivity** — after flattening every subckt defined in the text,
//!    the device/net graphs are isomorphic under 1-dimensional
//!    Weisfeiler–Lehman colour refinement. A device's starting colour is its
//!    signature (letter, model, evaluated positional values and spec, sorted
//!    evaluated params, `m=1` dropped); terminal order is significant. Net
//!    names are ignored except for anchors: `0`/`gnd`, `.global` names, names
//!    ending in `!`, and the ports of root subckts (a CDL top cell).
//!    Instance names, line order and subckt order never matter.
//! 2. **Params** — top-level `.param` values (evaluated when numeric).
//! 3. **Models** — `.model` cards (evaluated params, any order).
//! 4. **Includes** — `.include` / `.lib` lines, paths compared verbatim
//!    after [`Options::path_aliases`].
//! 5. **Directives** — every other dot-card, case-folded, whitespace-collapsed.
//!    `.save` vectors are a set; with matching connectivity a saved net or
//!    device is spelled by its final colour, once per expansion of its subckt.
//! 6. **Control** — `.control` blocks as opaque text (line order kept, block
//!    order ignored).
//!
//! Numbers compare after evaluation (SI suffixes, arithmetic over params in
//! scope) rounded to 10 significant digits; an expression that does not
//! evaluate compares as canonical text (numbers canonical, params in scope
//! replaced by their values, whitespace dropped). Control nodes of B/E/G
//! sources (`v(a,b)` arguments, the nodes after `poly(N)`/`pwl(N)`/`and(N)`…)
//! and extra bare nodes of CDL R/C/D lines are terminals.
//!
//! Known blind spots: 1-WL cannot tell apart some highly regular graphs
//! (e.g. two disjoint triangles vs one hexagon of identical devices, or a save
//! of one of two automorphic nets); F/H/K references keep instance names as
//! text.
//!
//! Data: per side, one flat SoA device table (signature hash, CSR terminal →
//! net ids `u32`) and a net table; lifetime is one `compare` call.

#![forbid(unsafe_code)]

mod bind;
mod eval;
mod flat;
mod parse;

use std::fmt;

use flat::Flat;

/// Comparison knobs. `Default` is the TESTING.md oracle.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// `(from, to)` literal replacements applied to both texts before
    /// parsing, so absolute paths from different checkouts compare equal
    /// (e.g. `("/home/ci/corpus/.cache", "$CORPUS")`).
    pub path_aliases: Vec<(String, String)>,
    /// Treats names the first netlist (the reference) uses but never defines
    /// with a top-level `.param` as free parameters, bound to the values the
    /// second netlist uses (its same-named `.param`, else the same-named
    /// device's value in the same slot), then compares everything under that
    /// binding; `b`'s `.param`s of bound names leave the Params section.
    /// Off for the oracle: a pair equal only with it on means "the reference
    /// leaves these parameters unevaluated", reported apart from agreement.
    pub bind_free: bool,
}

/// Which equivalence section a difference belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    /// Device signatures or the net partition over device terminals.
    Connectivity,
    /// Top-level `.param` values.
    Params,
    /// `.model` cards.
    Models,
    /// `.include` / `.lib` lines.
    Includes,
    /// Other dot-cards and unclassified lines.
    Directives,
    /// `.control` blocks.
    Control,
}

/// One difference: `-` lines exist only in the first netlist, `+` only in
/// the second, `~` pairs a same-named device whose signature changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff {
    /// Section the difference belongs to.
    pub section: Section,
    /// Human-readable line.
    pub text: String,
}

/// Result of [`compare`].
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Differences, grouped by section in [`Section`] order; empty iff the
    /// netlists are equivalent. Long lists are truncated with a count line.
    pub diffs: Vec<Diff>,
    /// Parser/flattener diagnostics, prefixed `a:` / `b:`. They never make
    /// netlists unequal by themselves: unclassified lines are compared as
    /// directives.
    pub warnings: Vec<String>,
    /// Flat device counts `[a, b]`.
    pub devices: [usize; 2],
    /// Flat net counts `[a, b]`.
    pub nets: [usize; 2],
    /// With [`Options::bind_free`]: the values assumed for `a`'s free names,
    /// `name=value` sorted by name; empty otherwise.
    pub bindings: Vec<String>,
}

impl Report {
    /// True when no section differs.
    pub fn is_equal(&self) -> bool {
        self.diffs.is_empty()
    }
}

/// Lines printed per side and section before truncating.
const SHOWN_MAX: usize = 25;

/// Compares two netlist texts under the equivalence in the crate docs.
///
/// Never panics on any input: unparseable lines become directives plus a
/// warning. Cost: O(n log n) per refinement round, at most `devices + nets`
/// rounds (in practice the circuit's diameter).
pub fn compare(a: &str, b: &str, opts: &Options) -> Report {
    let (a, b) = (alias(a, opts), alias(b, opts));
    let (na, nb) = (parse::parse(&a), parse::parse(&b));
    let (mut fa, mut fb) = (flat::flatten(&na, &[]), flat::flatten(&nb, &[]));
    let mut bound = Vec::new();
    if opts.bind_free {
        bound = bind::infer(&na, &fa, &fb);
        if !bound.is_empty() {
            fa = flat::flatten(&na, &bound);
            fb.params.retain(|p| {
                p.split_once('=')
                    .is_none_or(|(k, _)| bound.iter().all(|(n, _)| n != k))
            });
        }
    }
    let mut bindings: Vec<String> = bound
        .iter()
        .map(|(k, v)| format!("{k}={}", eval::canon_number(*v)))
        .collect();
    bindings.sort();

    let mut report = Report {
        devices: [fa.dev_sig.len(), fb.dev_sig.len()],
        nets: [fa.net_name.len(), fb.net_name.len()],
        bindings,
        ..Report::default()
    };
    for (side, nl, f) in [("a", &na, &fa), ("b", &nb, &fb)] {
        report.warnings.extend(
            nl.warnings
                .iter()
                .chain(&f.warnings)
                .map(|w| format!("{side}: {w}")),
        );
    }
    let colours = connectivity(&fa, &fb, &mut report.diffs);
    let (mut da, mut db) = (na.directives.clone(), nb.directives.clone());
    match &colours {
        Some([(dca, nca), (dcb, ncb)]) => {
            da.extend(saves(&na, &fa, Some((dca, nca))));
            db.extend(saves(&nb, &fb, Some((dcb, ncb))));
        }
        None => {
            da.extend(saves(&na, &fa, None));
            db.extend(saves(&nb, &fb, None));
        }
    }
    multiset(Section::Params, &fa.params, &fb.params, &mut report.diffs);
    multiset(Section::Models, &fa.models, &fb.models, &mut report.diffs);
    multiset(
        Section::Includes,
        &na.includes,
        &nb.includes,
        &mut report.diffs,
    );
    multiset(Section::Directives, &da, &db, &mut report.diffs);
    multiset(
        Section::Control,
        &na.controls,
        &nb.controls,
        &mut report.diffs,
    );
    report
}

/// `.save` directives as a set. With the final WL colours (connectivity
/// matched), every vector naming nets or devices is spelled by their colours,
/// once per expansion of its scope: names of unanchored nets do not matter
/// there either, and a save inside a subckt is the same save whatever the
/// subckt is called or however its instances are split between definitions.
/// Without colours, or for vectors naming nothing, `scope: .save vector` text.
fn saves(nl: &parse::Netlist, f: &Flat, colours: Option<(&Vec<u64>, &Vec<u64>)>) -> Vec<String> {
    let dev_index: std::collections::HashMap<&str, u32> = f
        .dev_name
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i as u32))
        .collect();
    let mut out = Vec::with_capacity(nl.saves.len());
    for (scope, vector) in &nl.saves {
        let resolved = colours.and_then(|(d, n)| f.save_vectors(scope, vector, &dev_index, d, n));
        match resolved {
            Some(list) => out.extend(list.into_iter().map(|v| format!(".save {v}"))),
            None if scope.is_empty() => out.push(format!(".save {vector}")),
            None => out.push(format!("{scope}: .save {vector}")),
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn alias(text: &str, opts: &Options) -> String {
    let mut out = text.to_string();
    for (from, to) in &opts.path_aliases {
        if !from.is_empty() {
            out = out.replace(from.as_str(), to);
        }
    }
    out
}

/// Multiset difference of normalized lines.
fn multiset(section: Section, a: &[String], b: &[String], diffs: &mut Vec<Diff>) {
    let mut a: Vec<&String> = a.iter().collect();
    let mut b: Vec<&String> = b.iter().collect();
    a.sort();
    b.sort();
    let (mut only_a, mut only_b) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) if x == y => {
                i += 1;
                j += 1;
            }
            (Some(x), Some(y)) if x < y => {
                only_a.push(format!("- {x}"));
                i += 1;
            }
            (Some(x), None) => {
                only_a.push(format!("- {x}"));
                i += 1;
            }
            (_, Some(y)) => {
                only_b.push(format!("+ {y}"));
                j += 1;
            }
            (None, None) => unreachable!("loop condition"),
        }
    }
    push_capped(section, only_a, diffs);
    push_capped(section, only_b, diffs);
}

fn push_capped(section: Section, lines: Vec<String>, diffs: &mut Vec<Diff>) {
    let total = lines.len();
    diffs.extend(
        lines
            .into_iter()
            .take(SHOWN_MAX)
            .map(|text| Diff { section, text }),
    );
    if total > SHOWN_MAX {
        diffs.push(Diff {
            section,
            text: format!("  … {} more", total - SHOWN_MAX),
        });
    }
}

/// Net → incident (device colour slot, terminal index) lists, CSR.
struct Incidence {
    start: Vec<u32>,
    dev: Vec<u32>,
    term: Vec<u32>,
}

fn incidence(f: &Flat) -> Incidence {
    debug_assert_eq!(f.term_start.len(), f.dev_sig.len() + 1);
    debug_assert_eq!(f.term_start.last().copied(), Some(f.term_net.len() as u32));
    let nets = f.net_name.len();
    let mut count = vec![0u32; nets + 1];
    for &n in &f.term_net {
        count[n as usize + 1] += 1;
    }
    for i in 0..nets {
        count[i + 1] += count[i];
    }
    let mut fill = count.clone();
    let mut dev = vec![0u32; f.term_net.len()];
    let mut term = vec![0u32; f.term_net.len()];
    for d in 0..f.dev_sig.len() {
        let (lo, hi) = (f.term_start[d] as usize, f.term_start[d + 1] as usize);
        for (t, &n) in f.term_net[lo..hi].iter().enumerate() {
            let slot = fill[n as usize] as usize;
            dev[slot] = d as u32;
            term[slot] = t as u32;
            fill[n as usize] += 1;
        }
    }
    debug_assert!((0..nets).all(|n| fill[n] == count[n + 1]));
    Incidence {
        start: count,
        dev,
        term,
    }
}

/// One WL round: nets absorb sorted (device colour, terminal) multisets,
/// then devices absorb their terminals' new net colours in order.
fn refine(f: &Flat, inc: &Incidence, dev: &[u64], net: &[u64]) -> (Vec<u64>, Vec<u64>) {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut scratch: Vec<(u64, u32)> = Vec::new();
    let mut new_net = Vec::with_capacity(net.len());
    for (n, &colour) in net.iter().enumerate() {
        scratch.clear();
        for k in inc.start[n] as usize..inc.start[n + 1] as usize {
            scratch.push((dev[inc.dev[k] as usize], inc.term[k]));
        }
        scratch.sort_unstable();
        let mut h = DefaultHasher::new();
        (colour, &scratch).hash(&mut h);
        new_net.push(h.finish());
    }
    let mut new_dev = Vec::with_capacity(dev.len());
    for (d, &colour) in dev.iter().enumerate() {
        let mut h = DefaultHasher::new();
        colour.hash(&mut h);
        for &n in &f.term_net[f.term_start[d] as usize..f.term_start[d + 1] as usize] {
            new_net[n as usize].hash(&mut h);
        }
        new_dev.push(h.finish());
    }
    debug_assert_eq!((new_dev.len(), new_net.len()), (dev.len(), net.len()));
    (new_dev, new_net)
}

fn distinct(a: &[u64], b: &[u64]) -> usize {
    let mut all: Vec<u64> = a.iter().chain(b).copied().collect();
    all.sort_unstable();
    all.dedup();
    all.len()
}

/// Indices of `a`'s elements not matched (as a multiset) in `b`, and vice versa.
fn unmatched(a: &[u64], b: &[u64]) -> (Vec<usize>, Vec<usize>) {
    let mut ia: Vec<usize> = (0..a.len()).collect();
    let mut ib: Vec<usize> = (0..b.len()).collect();
    ia.sort_by_key(|&i| (a[i], i));
    ib.sort_by_key(|&i| (b[i], i));
    let (mut oa, mut ob) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < ia.len() || j < ib.len() {
        match (ia.get(i), ib.get(j)) {
            (Some(&x), Some(&y)) if a[x] == b[y] => {
                i += 1;
                j += 1;
            }
            (Some(&x), Some(&y)) if a[x] < b[y] => {
                oa.push(x);
                i += 1;
            }
            (Some(&x), None) => {
                oa.push(x);
                i += 1;
            }
            (_, Some(&y)) => {
                ob.push(y);
                j += 1;
            }
            (None, None) => unreachable!("loop condition"),
        }
    }
    oa.sort_unstable();
    ob.sort_unstable();
    (oa, ob)
}

/// Final colours `(devices, nets)` of both sides, when they match.
type Colours = [(Vec<u64>, Vec<u64>); 2];

/// Runs WL on both graphs in lockstep and reports the devices that differ
/// at the first round where the colour multisets diverge: round 0 means
/// signatures (params/models), later rounds mean wiring. Returns the stable
/// colours when nothing differs.
fn connectivity(fa: &Flat, fb: &Flat, diffs: &mut Vec<Diff>) -> Option<Colours> {
    let (ia, ib) = (incidence(fa), incidence(fb));
    let (mut da, mut na) = (fa.dev_sig.clone(), fa.net_anchor.clone());
    let (mut db, mut nb) = (fb.dev_sig.clone(), fb.net_anchor.clone());
    let mut classes = [distinct(&da, &na), distinct(&db, &nb)];
    // Each round that is not stable splits at least one class.
    let rounds_max = da.len() + na.len() + db.len() + nb.len() + 1;
    for round in 0..=rounds_max {
        let (oa, ob) = unmatched(&da, &db);
        if !oa.is_empty() || !ob.is_empty() {
            report_devices(fa, fb, &oa, &ob, round, diffs);
            return None;
        }
        (da, na) = refine(fa, &ia, &da, &na);
        (db, nb) = refine(fb, &ib, &db, &nb);
        let now = [distinct(&da, &na), distinct(&db, &nb)];
        if now != classes {
            classes = now;
            continue;
        }
        // Both partitions stable: further rounds cannot separate anything.
        let (oa, ob) = unmatched(&da, &db);
        if !oa.is_empty() || !ob.is_empty() {
            report_devices(fa, fb, &oa, &ob, round + 1, diffs);
            return None;
        }
        let (ona, onb) = unmatched(&na, &nb);
        if ona.is_empty() && onb.is_empty() {
            return Some([(da, na), (db, nb)]);
        }
        let list = |f: &Flat, ids: &[usize], sign: char| -> Vec<String> {
            ids.iter()
                .map(|&n| format!("{sign} net {}", f.net_name[n]))
                .collect()
        };
        push_capped(Section::Connectivity, list(fa, &ona, '-'), diffs);
        push_capped(Section::Connectivity, list(fb, &onb, '+'), diffs);
        return None;
    }
    debug_assert!(
        false,
        "WL refinement must stabilize within {rounds_max} rounds"
    );
    None
}

fn report_devices(
    fa: &Flat,
    fb: &Flat,
    oa: &[usize],
    ob: &[usize],
    round: usize,
    diffs: &mut Vec<Diff>,
) {
    let pins = |f: &Flat, d: usize| -> String {
        let nets = &f.term_net[f.term_start[d] as usize..f.term_start[d + 1] as usize];
        nets.iter()
            .map(|&n| f.net_name[n as usize].as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut changed = Vec::new();
    let (mut only_a, mut only_b) = (Vec::new(), Vec::new());
    let mut used_b = vec![false; ob.len()];
    for &x in oa {
        // Same flat name on both sides at round 0: show it as a change.
        let twin = (round == 0)
            .then(|| ob.iter().position(|&y| fb.dev_name[y] == fa.dev_name[x]))
            .flatten();
        match twin {
            Some(k) if !used_b[k] => {
                used_b[k] = true;
                changed.push(format!(
                    "~ {}: {}  →  {}",
                    fa.dev_name[x], fa.dev_text[x], fb.dev_text[ob[k]]
                ));
            }
            _ => only_a.push(format!(
                "- {} ({})  pins: {}",
                fa.dev_name[x],
                fa.dev_text[x],
                pins(fa, x)
            )),
        }
    }
    for (k, &y) in ob.iter().enumerate() {
        if !used_b[k] {
            only_b.push(format!(
                "+ {} ({})  pins: {}",
                fb.dev_name[y],
                fb.dev_text[y],
                pins(fb, y)
            ));
        }
    }
    let what = if round == 0 {
        "device signatures differ"
    } else {
        "wiring differs"
    };
    diffs.push(Diff {
        section: Section::Connectivity,
        text: format!(
            "{what} (WL round {round}): {} devices only in a, {} only in b",
            oa.len(),
            ob.len()
        ),
    });
    push_capped(Section::Connectivity, changed, diffs);
    push_capped(Section::Connectivity, only_a, diffs);
    push_capped(Section::Connectivity, only_b, diffs);
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verdict = if self.is_equal() {
            "equivalent"
        } else {
            "DIFFERENT"
        };
        writeln!(
            f,
            "{verdict}: devices {} vs {}, nets {} vs {}",
            self.devices[0], self.devices[1], self.nets[0], self.nets[1]
        )?;
        let mut last = None;
        for d in &self.diffs {
            if last != Some(d.section) {
                writeln!(f, "[{:?}]", d.section)?;
                last = Some(d.section);
            }
            writeln!(f, "  {}", d.text)?;
        }
        if !self.bindings.is_empty() {
            writeln!(f, "assuming {}", self.bindings.join(" "))?;
        }
        for w in &self.warnings {
            writeln!(f, "warning: {w}")?;
        }
        Ok(())
    }
}
