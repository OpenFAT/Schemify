//! Fuzz target: any bytes as one request line, and as a whole stream through the headless
//! server. Oracles: no panic (the decoder's and the server's asserts fire otherwise); every
//! decode error is one JSON response line with a declared code; a value that parses
//! re-encodes to text that parses back to the same value; the server answers with whole
//! JSON lines only.
//!
//! Run (nightly): `cargo fuzz run rpc_decode` from `crates/cli`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use schemify_cli::rpc::{self, Headless, Json};

fuzz_target!(|data: &[u8]| {
    if let Err(reply) = rpc::decode(data) {
        assert!(reply.ends_with('\n') && !reply[..reply.len() - 1].contains('\n'));
        let v = rpc::parse(reply.trim_end()).expect("error responses are JSON");
        let code = v.get("error").and_then(|e| e.get("code")).cloned();
        assert!(matches!(code, Some(Json::Num(c)) if c == -32700.0 || c == -32600.0));
    }
    if let Ok(text) = std::str::from_utf8(data)
        && let Ok(v) = rpc::parse(text)
    {
        let mut again = String::new();
        rpc::write(&v, &mut again);
        assert_eq!(rpc::parse(&again).as_ref(), Ok(&v));
    }
    // The server without a core binary: CLI methods answer an internal error; `edit` could
    // touch files the input names, so inputs that could spell it (also through `\u`
    // escapes) are left out.
    let h = Headless {
        exe: None,
        flags: Vec::new(),
        seed: 1,
    };
    if !data.windows(4).any(|w| w == b"edit") && !data.contains(&b'\\') {
        let mut out = Vec::new();
        rpc::serve(&mut &data[..], &mut out, &h).expect("in-memory I/O");
        for line in String::from_utf8(out).expect("responses are UTF-8").lines() {
            assert!(rpc::parse(line).is_ok(), "{line}");
        }
    }
});
