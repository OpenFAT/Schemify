//! Capability decisions (plugin-abi §5, D7) as a property: `authorize` allows an I/O
//! request exactly when the spec's rule, restated here independently of the host, says so,
//! and refuses every other request with a declared code.

use proptest::prelude::*;
use schemify_plugin as host;
use schemify_plugin::abi::Code;
use schemify_plugin::{Grants, IoOp};

// ---- private ----

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn capability_decisions_follow_the_grants(
        grants in proptest::sample::subsequence(
            vec!["read_project", "write_project", "network", "clipboard", "spawn:ngspice", "spawn:xyce", "read:PDK_ROOT"], 0..=7),
        op in proptest::sample::select(vec![
            IoOp::FsRead, IoOp::FsList, IoOp::FsWrite, IoOp::FsRemove, IoOp::Spawn, IoOp::Fetch, IoOp::ClipboardGet,
            IoOp::ClipboardSet,
        ]),
        args in proptest::collection::vec(prop_oneof![
            proptest::sample::select(vec![
                "lib/a.shcomp", "../x", "/abs", "a/./b", "a//b", "c:x", "ngspice", "xyce", "sh", "", "https://x",
                ".schemify/pdks/a", ".schemify", ".schemify/../x", ".schemify/schemify.lock", "schemify.project",
                "${PDK_ROOT}", "${PDK_ROOT}/a/b.sym", "${PDK_ROOT}/../x", "${PDK_ROOT}x", "${HOME}/x",
                "a/${PDK_ROOT}/b", "${pdk_root}/x",
            ]).prop_map(String::from),
            "\\PC{0,8}",
        ], 0..3),
    ) {
        let granted = Grants::parse(&grants).unwrap();
        let got = host::authorize(&granted, op, &args);
        prop_assert!(matches!(got, Ok(()) | Err(Code::Denied) | Err(Code::Parse)));
        let arity = match op {
            IoOp::FsRead | IoOp::FsList | IoOp::FsWrite | IoOp::FsRemove | IoOp::Fetch => args.len() == 1,
            IoOp::Spawn => !args.is_empty(),
            IoOp::ClipboardGet | IoOp::ClipboardSet => args.is_empty(),
        };
        if !arity {
            prop_assert_eq!(got, Err(Code::Parse));
            return Ok(());
        }
        // Positive and negative space: allowed exactly when the spec's rule says so.
        let clean = |p: &str| {
            !p.is_empty() && !p.starts_with('/') && !p.contains(['\\', ':', '\0'])
                && p.split('/').all(|s| !s.is_empty() && s != "." && s != ".." && !s.starts_with("${"))
        };
        // `${NAME}` or `${NAME}/<clean>` reads need `read:<NAME>`; NAME is upper snake case.
        let env_read = |p: &str| -> Option<bool> {
            let rest = p.strip_prefix("${")?;
            let (name, tail) = rest.split_once('}')?;
            let named = !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            let tail_ok = tail.is_empty() || tail.strip_prefix('/').is_some_and(|t| clean(t) && t.len() <= 4096);
            (named && tail_ok).then(|| grants.contains(&format!("read:{name}").as_str()))
        };
        let allowed = match op {
            IoOp::FsRead | IoOp::FsList => match env_read(&args[0]) {
                Some(granted) => granted,
                None => granted.has(op.cap()) && clean(&args[0]) && args[0].len() <= 4096,
            },
            _ => granted.has(op.cap()) && match op {
            IoOp::FsRead | IoOp::FsList => unreachable!(),
            IoOp::FsWrite => clean(&args[0]) && args[0].len() <= 4096,
            IoOp::FsRemove => clean(&args[0]) && args[0].len() <= 4096
                && args[0].split('/').count() >= 2 && args[0].split('/').next() == Some(".schemify")
                && !matches!(args[0].rsplit('/').next(), Some("schemify.project" | "schemify.lock")),
            IoOp::Spawn => grants.iter().any(|grant| grant.strip_prefix("spawn:") == Some(args[0].as_str())),
            IoOp::Fetch => !args[0].is_empty(),
            IoOp::ClipboardGet | IoOp::ClipboardSet => true,
        }};
        prop_assert_eq!(got.is_ok(), allowed, "{:?} {:?} {:?}", grants, op, args);
    }
}
