//! Merge-tier replay of the checked-in fuzz corpus (TESTING.md tiers): every input of
//! `fuzz/corpus/xschem_read/` through the fuzz target's own oracle.

include!("../../../tests/fuzz/replay.rs");
include!("../fuzz/oracles/xschem_read.rs");

#[test]
fn xschem_read_corpus_passes_the_reader_oracle() {
    replay("xschem_read", xschem_read_oracle);
}

include!("../fuzz/oracles/sfyx_import.rs");

#[test]
fn sfyx_import_corpus_passes_the_import_oracle() {
    replay("sfyx_import", sfyx_import_oracle);
}

/// Seeded mutations of every `.sfyx` corpus input (truncations, byte flips, dropped,
/// duplicated and swapped lines) through the import oracle: the merge tier's stand-in for
/// the nightly fuzzer on the inputs most likely to reach deep into the importer.
#[test]
fn mutated_sfyx_inputs_never_panic_the_import() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus/sfyx_import");
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move |n: usize| -> usize {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n.max(1) as u64) as usize
    };
    let mut inputs: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    inputs.sort();
    for p in inputs {
        let base = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = base.lines().collect();
        for k in 0..150 {
            let m: String = match k % 5 {
                0 => base[..base
                    .char_indices()
                    .nth(next(base.chars().count()))
                    .map_or(0, |c| c.0)]
                    .to_owned(),
                1 => {
                    let mut b = base.clone().into_bytes();
                    let i = next(b.len());
                    b[i] = b"()\" 0-9a\\\n"[next(10)];
                    String::from_utf8_lossy(&b).into_owned()
                }
                2 => {
                    let mut l = lines.clone();
                    l.remove(next(l.len()));
                    l.join("\n") + "\n"
                }
                3 => {
                    let mut l = lines.clone();
                    let i = next(l.len());
                    l.insert(i, lines[next(lines.len())]);
                    l.join("\n") + "\n"
                }
                _ => {
                    let mut l = lines.clone();
                    let (a, b) = (next(l.len()), next(l.len()));
                    l.swap(a, b);
                    l.join("\n") + "\n"
                }
            };
            let run = std::panic::catch_unwind(|| sfyx_import_oracle(m.as_bytes()));
            assert!(
                run.is_ok(),
                "{} mutation {k} panics the import:\n{m}",
                p.display()
            );
        }
    }
}
