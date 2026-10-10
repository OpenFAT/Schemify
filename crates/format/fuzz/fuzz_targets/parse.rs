//! Reader oracle: never panics, reports only declared codes, and anything it accepts
//! writes to a canonical text that re-parses and is a fixed point of parse → write.
//! The crate's internal asserts are the rest of the oracle.

#![no_main]

use libfuzzer_sys::fuzz_target;
use schemify_format::{parse, parse_prefix, write};

fuzz_target!(|data: &[u8]| {
    match parse(data) {
        Ok(doc) => {
            let text = write(&doc);
            let again = parse(text.as_bytes()).expect("canonical output parses");
            assert_eq!(write(&again), text, "write is not idempotent");
        }
        Err(e) => {
            assert!(!e.diagnostics.is_empty() && e.diagnostics.len() <= 64);
            assert!(e.diagnostics.iter().all(|d| !d.code.is_warning()));
        }
    }
    let _ = parse_prefix(data, 1);
});
