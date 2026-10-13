//! Property oracles for every reader (TESTING.md, `import` row):
//! - totality: arbitrary and grammar-shaped input never panics, and each reader's
//!   count-conservation invariant holds (the readers assert it on exit; these tests
//!   re-check it from outside);
//! - round trip: records and attribute strings written with xschem's documented writer
//!   rules read back to the same values;
//! - mutation: real corpus files with random byte edits never panic (needs
//!   `SCHEMIFY_CORPUS`, skipped otherwise).

use std::path::{Path, PathBuf};

use interop::attr::{Attrs, at_tokens, classify};
use interop::xschem::{self, XschemFile};
use interop::{cdf, spice, symbol, tech};
use proptest::prelude::*;

/// Characters that drive every branch of the xschem, attribute and SPICE grammars.
const ALPHABET: &[char] = &[
    'v', 'G', 'K', 'V', 'S', 'E', 'F', '#', 'L', 'B', 'P', 'A', 'T', 'N', 'C', '[', ']', '{', '}',
    '\\', ' ', '\n', '\r', '\t', '0', '1', '5', '.', '-', 'e', '@', '#', '%', '=', '"', '\'', ';',
    '$', '(', ')', '?', '+', '*', 'x', 'µ', '\0',
];

fn grammar_text() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(ALPHABET), 0..600).prop_map(String::from_iter)
}

fn any_text() -> impl Strategy<Value = String> {
    prop_oneof![grammar_text(), any::<String>()]
}

/// Checks the tables of one file level from outside: conservation, span validity and
/// parallel-column lengths.
fn check_level(f: &XschemFile) {
    assert_eq!(
        f.records_read,
        f.records_kept() + f.comments + f.issues.len() as u32
    );
    let spans = f
        .globals
        .text
        .iter()
        .chain(&f.lines.props)
        .chain(&f.boxes.props)
        .chain(&f.polygons.props);
    let spans = spans
        .chain(&f.arcs.props)
        .chain(&f.texts.text)
        .chain(&f.texts.props)
        .chain(&f.wires.props);
    for s in spans.chain(&f.instances.symref).chain(&f.instances.props) {
        let _ = f.str(*s);
    }
    assert_eq!(
        f.polygons.count.iter().map(|c| *c as usize).sum::<usize>(),
        f.points.len()
    );
    assert!(f.issues.windows(2).all(|w| w[0].line <= w[1].line));
    for e in &f.embedded {
        assert!((e.instance as usize) < f.instances.symref.len());
        check_level(&e.body);
    }
}

fn check_xschem(text: &str) {
    let f = xschem::read(text).expect("test inputs are small");
    check_level(&f);
    let s = symbol::extract(&f);
    assert!(s.pins.name.len() <= f.boxes.layer.len());
    for i in 0..s.pins.name.len() {
        let _ = s.pin_name(i);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn xschem_reader_is_total(text in any_text()) {
        check_xschem(&text);
    }

    #[test]
    fn attribute_tokenizer_is_total(text in any_text()) {
        let a = Attrs::parse(&text);
        prop_assert!(a.len() <= text.len());
        for i in 0..a.len() {
            let raw = a.raw(i);
            prop_assert!((raw.start + raw.len) as usize <= text.len());
            prop_assert!(a.key(i).len() <= text.len() && a.value(i).len() <= text.len());
        }
        let mut last = None;
        for t in at_tokens(&text) {
            prop_assert!(!t.name.is_empty());
            prop_assert!(last.is_none_or(|l| t.offset > l));
            last = Some(t.offset);
        }
        let _ = classify(&text);
    }

    #[test]
    fn spice_scanner_is_total(text in any_text()) {
        // Every include resolves to the same text, so cycles and the depth bound get hit.
        let s = spice::scan(Path::new("a.lib"), &spice::ScanOptions { section: Some("tt"), search_dirs: &[] }, |_| Some(text.clone()));
        prop_assert_eq!(s.line_counts.iter().sum::<u32>(), s.lines_read);
        let s = spice::scan(Path::new("a.lib"), &spice::ScanOptions::default(), |_| Some(text.clone()));
        prop_assert!(s.files.len() <= spice::MAX_FILES);
        for m in 0..s.models.name.len() {
            let _ = s.backing(m);
        }
        let _ = spice::union(&s.bins_of("x"));
    }

    #[test]
    fn tech_and_cdf_readers_are_total(text in any_text()) {
        let _ = tech::read_lyt(&text);
        let _ = tech::read_lyt(&format!("<technology>{text}</technology>"));
        let _ = tech::read_magic(&text).manufacturing_grid_pm();
        let d = cdf::read(&text);
        prop_assert!(d.params.name.len() as u32 <= d.param_calls);
        let d = cdf::read(&format!("cdfCreateParam( cdfId ?name \"p\" {text}"));
        prop_assert_eq!(d.param_calls, 1);
    }
}

/// One generated record and the values the reader must return for it.
#[derive(Clone, Debug)]
enum Rec {
    Line(u8, [i32; 4], String),
    Box(u8, [i32; 4], String),
    Wire([i32; 4], String),
    Arc(u8, [i32; 5], String),
    Poly(u8, Vec<[i32; 2]>, String),
    Text(String, [i32; 2], i16, i16, [i32; 2], String),
    Inst(String, [i32; 2], i16, i16, String),
    Global(char, String),
}

/// A coordinate in milli-units, written exactly as xschem units.
fn milli() -> impl Strategy<Value = i32> + Clone {
    prop_oneof![
        -2_000_000_000i32..2_000_000_000,
        -100_000i32..100_000,
        Just(0)
    ]
}

fn write_milli(v: i32) -> String {
    let sign = if v < 0 { "-" } else { "" };
    let a = v.unsigned_abs();
    format!("{sign}{}.{:03}", a / 1000, a % 1000)
}

/// Brace-string content: anything but `\r`, which xschem drops.
fn brace_text() -> impl Strategy<Value = String> {
    any::<String>().prop_map(|s| s.replace('\r', ""))
}

/// xschem's brace writer: `\`, `{`, `}` get a backslash.
fn brace(s: &str) -> String {
    let mut out = String::from("{");
    for c in s.chars() {
        if matches!(c, '\\' | '{' | '}') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('}');
    out
}

fn rec() -> impl Strategy<Value = Rec> {
    let quad = [milli(), milli(), milli(), milli()];
    prop_oneof![
        (any::<u8>(), quad.clone(), brace_text()).prop_map(|(l, q, p)| Rec::Line(l, q, p)),
        (any::<u8>(), quad.clone(), brace_text()).prop_map(|(l, q, p)| Rec::Box(l, q, p)),
        (quad, brace_text()).prop_map(|(q, p)| Rec::Wire(q, p)),
        (
            any::<u8>(),
            [milli(), milli(), milli(), milli(), milli()],
            brace_text()
        )
            .prop_map(|(l, a, p)| Rec::Arc(l, a, p)),
        (
            any::<u8>(),
            prop::collection::vec([milli(), milli()], 1..20),
            brace_text()
        )
            .prop_map(|(l, v, p)| Rec::Poly(l, v, p)),
        (
            brace_text(),
            [milli(), milli()],
            any::<i16>(),
            any::<i16>(),
            [milli(), milli()],
            brace_text()
        )
            .prop_map(|(t, xy, r, f, s, p)| Rec::Text(t, xy, r, f, s, p)),
        (
            brace_text(),
            [milli(), milli()],
            0i16..4,
            0i16..2,
            brace_text()
        )
            .prop_map(|(t, xy, r, f, p)| Rec::Inst(t, xy, r, f, p)),
        (
            prop::sample::select(&['v', 'G', 'K', 'V', 'S', 'E'][..]),
            brace_text()
        )
            .prop_map(|(c, t)| Rec::Global(c, t)),
    ]
}

fn write_rec(r: &Rec) -> String {
    let m = |v: &[i32]| {
        v.iter()
            .map(|x| write_milli(*x))
            .collect::<Vec<_>>()
            .join(" ")
    };
    match r {
        Rec::Line(l, q, p) => format!("L {l} {} {}\n", m(q), brace(p)),
        Rec::Box(l, q, p) => format!("B {l} {} {}\n", m(q), brace(p)),
        Rec::Wire(q, p) => format!("N {} {}\n", m(q), brace(p)),
        Rec::Arc(l, a, p) => format!("A {l} {} {}\n", m(a), brace(p)),
        Rec::Poly(l, v, p) => format!("P {l} {} {} {}\n", v.len(), m(&v.concat()), brace(p)),
        Rec::Text(t, xy, r, f, s, p) => {
            format!("T {} {} {r} {f} {} {}\n", brace(t), m(xy), m(s), brace(p))
        }
        Rec::Inst(t, xy, r, f, p) => format!("C {} {} {r} {f} {}\n", brace(t), m(xy), brace(p)),
        Rec::Global(c, t) => format!("{c} {}\n", brace(t)),
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn written_records_read_back_exactly(recs in prop::collection::vec(rec(), 0..24)) {
        let text: String = recs.iter().map(write_rec).collect();
        let f = xschem::read(&text).expect("small");
        prop_assert!(f.issues.is_empty(), "{:?}", f.issues);
        prop_assert!(f.roundings.is_empty());
        prop_assert_eq!(f.records_read as usize, recs.len());
        let (mut l, mut b, mut w, mut a, mut p, mut t, mut c, mut g) = (0, 0, 0, 0, 0, 0, 0, 0);
        for r in &recs {
            match r {
                Rec::Line(layer, q, s) => {
                    prop_assert_eq!((f.lines.layer[l], f.lines.xy[l], f.str(f.lines.props[l])), (*layer, *q, s.as_str()));
                    l += 1;
                }
                Rec::Box(layer, q, s) => {
                    prop_assert_eq!((f.boxes.layer[b], f.boxes.xy[b], f.str(f.boxes.props[b])), (*layer, *q, s.as_str()));
                    b += 1;
                }
                Rec::Wire(q, s) => {
                    prop_assert_eq!((f.wires.xy[w], f.str(f.wires.props[w])), (*q, s.as_str()));
                    w += 1;
                }
                Rec::Arc(layer, v, s) => {
                    let got = (f.arcs.layer[a], f.arcs.circle[a], f.arcs.angles[a], f.str(f.arcs.props[a]));
                    prop_assert_eq!(got, (*layer, [v[0], v[1], v[2]], [v[3], v[4]], s.as_str()));
                    a += 1;
                }
                Rec::Poly(layer, v, s) => {
                    let (first, count) = (f.polygons.first[p] as usize, f.polygons.count[p] as usize);
                    prop_assert_eq!(&f.points[first..first + count], v.as_slice());
                    prop_assert_eq!((f.polygons.layer[p], f.str(f.polygons.props[p])), (*layer, s.as_str()));
                    p += 1;
                }
                Rec::Text(s, xy, rot, flip, scale, props) => {
                    let x = &f.texts;
                    prop_assert_eq!((f.str(x.text[t]), x.xy[t], x.rot[t], x.flip[t], x.scale[t], f.str(x.props[t])),
                        (s.as_str(), *xy, *rot, *flip, *scale, props.as_str()));
                    t += 1;
                }
                Rec::Inst(s, xy, rot, flip, props) => {
                    let x = &f.instances;
                    prop_assert_eq!((f.str(x.symref[c]), x.xy[c], x.rot[c], x.flip[c], f.str(x.props[c])),
                        (s.as_str(), *xy, *rot, *flip, props.as_str()));
                    c += 1;
                }
                Rec::Global(_, s) => {
                    prop_assert_eq!(f.str(f.globals.text[g]), s.as_str());
                    g += 1;
                }
            }
        }
    }

    #[test]
    fn written_attributes_read_back_exactly(
        pairs in prop::collection::btree_map("[a-z_][a-z0-9_]{0,8}", any::<String>(), 0..12)
    ) {
        // xschem's writer rules (research §5.4): quote values holding a separator or that
        // are empty; escape `"` and `\` with a backslash.
        let mut s = String::new();
        for (k, v) in &pairs {
            let mut esc = String::new();
            for ch in v.chars() {
                if ch == '"' || ch == '\\' {
                    esc.push('\\');
                }
                esc.push(ch);
            }
            let quote = v.is_empty() || v.contains([' ', '\t', '\n', ';', '\0']);
            if quote {
                s += &format!("{k}=\"{esc}\"\n");
            } else {
                s += &format!("{k}={esc} ");
            }
        }
        let a = Attrs::parse(&s);
        prop_assert_eq!(a.len(), pairs.len());
        for (k, v) in &pairs {
            prop_assert_eq!(a.get(k), Some(v.as_str()));
        }
        // And through the file layer: a brace string holding the attribute string.
        let f = xschem::read(&format!("K {}\n", brace(&s))).expect("small");
        let a2 = Attrs::parse(f.global(xschem::GlobalTag::K).unwrap_or(""));
        prop_assert_eq!(a2, a);
    }
}

/// Real files from the corpus, for mutation fuzzing.
fn corpus_files() -> Vec<PathBuf> {
    let Some(root) = std::env::var_os("SCHEMIFY_CORPUS").map(PathBuf::from) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| {
                ["sym", "sch", "lib", "spice", "lyt", "tech"]
                    .iter()
                    .any(|e| x == *e)
            }) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn mutated_corpus_files_never_panic() {
    let files = corpus_files();
    if files.is_empty() {
        eprintln!("SCHEMIFY_CORPUS unset: skipping mutation test");
        return;
    }
    let texts: Vec<Vec<u8>> = files
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .filter(|t| t.len() < 200_000)
        .collect();
    let edits = prop::collection::vec((any::<prop::sample::Index>(), 0u8..3, any::<u8>()), 1..16);
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig {
        cases: 2048,
        ..ProptestConfig::default()
    });
    runner
        .run(&(any::<prop::sample::Index>(), edits), |(file, edits)| {
            let mut bytes = file.get(&texts).clone();
            for (at, op, byte) in edits {
                let i = at.index(bytes.len() + 1);
                match op {
                    0 => bytes.insert(i, byte),
                    1 if i < bytes.len() => {
                        bytes.remove(i);
                    }
                    _ if i < bytes.len() => bytes[i] = byte,
                    _ => {}
                }
            }
            let text = String::from_utf8_lossy(&bytes);
            check_xschem(&text);
            let _ = Attrs::parse(&text);
            let s = spice::scan(Path::new("m.lib"), &spice::ScanOptions::default(), |_| {
                Some(text.to_string())
            });
            assert_eq!(s.line_counts.iter().sum::<u32>(), s.lines_read);
            let _ = tech::read_lyt(&text);
            let _ = tech::read_magic(&text);
            let _ = cdf::read(&text);
            Ok(())
        })
        .expect("no panics on mutated corpus files");
}
