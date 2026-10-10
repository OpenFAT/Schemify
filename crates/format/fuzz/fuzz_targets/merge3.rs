//! Merge oracle on three NUL-separated inputs: never panics, a side equal to the base
//! yields the other side's canonical text, and the conflict count matches the markers
//! (asserted inside `merge3`).

#![no_main]

use libfuzzer_sys::fuzz_target;
use schemify_format::{diff, merge3, parse, write};

fuzz_target!(|data: &[u8]| {
    let mut parts = data.splitn(3, |&b| b == 0);
    let (Some(o), Some(a), Some(b)) = (parts.next(), parts.next(), parts.next()) else { return };
    let (Ok(o), Ok(a), Ok(b)) = (parse(o), parse(a), parse(b)) else { return };
    let _ = merge3(&o, &a, &b);
    assert_eq!(merge3(&o, &a, &o).text, write(&a));
    assert_eq!(merge3(&o, &o, &b).text, write(&b));
    assert!(diff(&a, &a).changes.is_empty());
    let _ = diff(&o, &b);
});
