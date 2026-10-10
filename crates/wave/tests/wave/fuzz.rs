//! The reader is total (TESTING.md: fuzz the reader, assertions are the oracle): random bytes
//! and mutated real files give `Ok` or a declared [`RawError`], never a panic or a hang, and
//! every `Ok` satisfies the column-store invariants (which `RawPlot`'s constructor asserts).

use proptest::prelude::*;
use schemify_wave::{RawError, RawPlot, parse_raw};

const SEEDS: [&str; 8] = [
    "rc_tran.bin.raw",
    "rc_tran.ascii.raw",
    "rlc_ac.bin.raw",
    "rlc_ac.ascii.raw",
    "dc_step.ascii.raw",
    "rc_tran.xyce.bin.raw",
    "rlc_ac.xyce.ascii.raw",
    "dc_step.xyce.bin.raw",
];

fn seed(k: usize) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), SEEDS[k]);
    std::fs::read(path).expect("fixture")
}

fn check(bytes: &[u8]) {
    match parse_raw(bytes) {
        Ok(plots) => {
            assert!(!plots.is_empty());
            for p in &plots {
                invariants(p);
            }
        }
        Err(
            RawError::Empty
            | RawError::Header(_)
            | RawError::Truncated { .. }
            | RawError::Values(_)
            | RawError::NoPlots,
        ) => {}
    }
}

fn invariants(p: &RawPlot) {
    let n = p.len();
    assert!(n > 0 && !p.vars().is_empty());
    for v in 0..p.vars().len() {
        assert_eq!(p.col(v).len(), n);
        assert!(p.col_im(v).is_empty() != p.is_complex());
    }
    let mut at = 0;
    for s in p.steps() {
        assert_eq!(s.start, at);
        assert!(s.end > s.start);
        at = s.end;
    }
    assert_eq!(at as usize, n);
}

#[derive(Debug, Clone)]
enum Mutation {
    Flip { at: usize, byte: u8 },
    Truncate { at: usize },
    Insert { at: usize, bytes: Vec<u8> },
    Remove { at: usize, len: usize },
}

fn mutation() -> impl Strategy<Value = Mutation> {
    prop_oneof![
        (any::<usize>(), any::<u8>()).prop_map(|(at, byte)| Mutation::Flip { at, byte }),
        any::<usize>().prop_map(|at| Mutation::Truncate { at }),
        (
            any::<usize>(),
            prop::collection::vec(
                prop_oneof![
                    any::<u8>(),
                    Just(b'\n'),
                    Just(b','),
                    Just(b'9'),
                    Just(b'-'),
                    Just(b'e')
                ],
                1..12
            )
        )
            .prop_map(|(at, bytes)| Mutation::Insert { at, bytes }),
        (any::<usize>(), 1..64usize).prop_map(|(at, len)| Mutation::Remove { at, len }),
    ]
}

fn apply(mut b: Vec<u8>, muts: &[Mutation]) -> Vec<u8> {
    for m in muts {
        if b.is_empty() {
            break;
        }
        match m {
            Mutation::Flip { at, byte } => {
                let i = at % b.len();
                b[i] = *byte;
            }
            Mutation::Truncate { at } => b.truncate(at % b.len()),
            Mutation::Insert { at, bytes } => {
                let i = at % b.len();
                b.splice(i..i, bytes.iter().copied());
            }
            Mutation::Remove { at, len } => {
                let i = at % b.len();
                let j = (i + len).min(b.len());
                b.drain(i..j);
            }
        }
    }
    b
}

/// Header-only text where the numbers are random: exercises the size guards.
fn header() -> impl Strategy<Value = Vec<u8>> {
    (
        any::<u32>(),
        any::<u32>(),
        prop::bool::ANY,
        prop::bool::ANY,
        prop::collection::vec(any::<u8>(), 0..256),
    )
        .prop_map(|(nv, np, complex, binary, tail)| {
            let mut s = format!(
                "Title: x\nPlotname: p\nFlags: {}\nNo. Variables: {nv}\nNo. Points: {np}\nVariables:\n",
                if complex { "complex" } else { "real" }
            );
            for k in 0..nv.min(4) {
                s.push_str(&format!("\t{k}\tv{k}\tvoltage\n"));
            }
            s.push_str(if binary { "Binary:\n" } else { "Values:\n" });
            let mut b = s.into_bytes();
            b.extend(tail);
            b
        })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn random_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        check(&bytes);
    }

    #[test]
    fn mutated_real_files_never_panic(
        k in 0..SEEDS.len(),
        muts in prop::collection::vec(mutation(), 1..6),
    ) {
        check(&apply(seed(k), &muts));
    }

    #[test]
    fn random_headers_never_panic(bytes in header()) {
        check(&bytes);
    }
}
