//! End-to-end through real wasmi with hand-written WAT modules: load-time failures, the
//! plugin table's capacity, host work charged as fuel, and quarantine. Cites plugin-abi §2
//! (exports, ABI version, fuel, quarantine) and §3 (imports outside an event).

mod common;

use common::{World, snapshots};
use schemify_plugin::abi::{Code, Event, FUEL_DEFAULT};
use schemify_plugin::{Fault, PLUGINS_MAX, Plugins, QUARANTINE_AFTER};

const BAD: &[u8] = include_bytes!("fixtures/bad_actor.wasm");

/// The exports every module needs besides `schemify_abi`, which each case varies.
const MINIMAL: &str = r#"
    (memory (export "memory") 1)
    (func (export "schemify_alloc") (param i32) (result i32) i32.const 8)
    (func (export "schemify_on_event") (param i32 i32) (result i64) i64.const 0)
"#;

// ---- private ----

#[test]
fn load_failures_are_declared() {
    let abi = |v: u32| format!(r#"(func (export "schemify_abi") (result i32) i32.const {v})"#);
    let cases: Vec<(Vec<u8>, &[&str], Code)> = vec![
        (b"not wasm".to_vec(), &[], Code::Module),
        (module(&format!("{MINIMAL} {}", abi(2))), &[], Code::Version),
        (module(&abi(1)), &[], Code::Module),
        (
            module(&format!(
                r#"{MINIMAL} (func (export "schemify_abi") (result i64) i64.const 1)"#
            )),
            &[],
            Code::Module,
        ),
        (
            module(&format!(
                r#"(import "env" "x" (func)) {MINIMAL} {}"#,
                abi(1)
            )),
            &[],
            Code::Module,
        ),
        (
            module(&format!(r#"(memory (export "memory") 5000) {}"#, abi(1))),
            &[],
            Code::Module,
        ),
        (
            module(&format!(
                r#"(func $s (loop br 0)) (start $s) {MINIMAL} {}"#,
                abi(1)
            )),
            &[],
            Code::Fuel,
        ),
        (
            module(&format!(
                r#"(func $s unreachable) (start $s) {MINIMAL} {}"#,
                abi(1)
            )),
            &[],
            Code::Trap,
        ),
        (
            module(&format!("{MINIMAL} {}", abi(1))),
            &["root"],
            Code::Parse,
        ),
    ];
    let mut plugins = Plugins::new();
    for (i, (wasm, grants, code)) in cases.iter().enumerate() {
        assert_eq!(
            plugins.load("x", wasm, grants, 1_000_000),
            Err(*code),
            "case {i}"
        );
        assert_eq!(plugins.health().faults()[i].code, *code);
    }
    assert!(plugins.ids().is_empty());

    // Imports called from a start function are denied, never a host panic.
    let start = r#"
        (import "schemify" "table_len" (func $len (param i32) (result i32)))
        (import "schemify" "read_column" (func $col (param i32 i32 i32 i32 i32 i32) (result i32)))
        (import "schemify" "fs_read" (func $fs (param i32 i32) (result i32)))
        (func $s (drop (call $len (i32.const 0)))
                 (drop (call $col (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 0) (i32.const 4)))
                 (drop (call $fs (i32.const 0) (i32.const 1))))
        (start $s)"#;
    let ok = module(&format!("{start} {MINIMAL} {}", abi(1)));
    assert!(
        plugins
            .load("start", &ok, &["read_project"], 1_000_000)
            .is_ok()
    );
}

#[test]
fn plugin_table_is_bounded() {
    let wasm = module(&format!(
        r#"{MINIMAL} (func (export "schemify_abi") (result i32) i32.const 1)"#
    ));
    let mut w = World::new();
    for _ in 0..PLUGINS_MAX {
        w.plugins.load("tiny", &wasm, &[], 1_000).unwrap();
    }
    assert_eq!(
        w.plugins.load("one-too-many", &wasm, &[], 1_000),
        Err(Code::Space)
    );
    w.frame();
    assert_eq!(w.plugins.ids().len(), PLUGINS_MAX);
    assert!(
        w.plugins
            .health()
            .faults()
            .iter()
            .all(|f| f.code == Code::Space)
    );
}

#[test]
fn host_work_is_charged_as_fuel() {
    // One 400 KB read costs 50k fuel: on a 20k budget the guest dies at its next fuel
    // check, so a loop of cheap-looking imports cannot keep the host copying forever.
    let wat = r#"
        (import "schemify" "read_column" (func $col (param i32 i32 i32 i32 i32 i32) (result i32)))
        (memory (export "memory") 16)
        (data (i32.const 2048) "log info done\n")
        (func $nop)
        (func (export "schemify_abi") (result i32) i32.const 1)
        (func (export "schemify_alloc") (param i32) (result i32) i32.const 1024)
        (func (export "schemify_on_event") (param i32 i32) (result i64)
            (drop (call $col (i32.const 4) (i32.const 3) (i32.const 0) (i32.const 100000)
                             (i32.const 65536) (i32.const 400000)))
            (call $nop)
            (i64.or (i64.shl (i64.const 2048) (i64.const 32)) (i64.const 14)))"#;
    for (fuel, codes, logs) in [(20_000, vec![Code::Fuel], 0), (1_000_000, vec![], 1)] {
        let mut w = World::new();
        w.snap = snapshots(&vec![1; 100_000]);
        w.load("reader", &module(wat), &[], fuel);
        let faults: Vec<Fault> = w.plugins.health().faults().iter().cloned().collect();
        assert_eq!(World::codes(&faults), codes, "fuel {fuel}");
        assert_eq!(w.queues.logs.len(), logs, "fuel {fuel}");
    }
}

/// Quarantine (§2): a plugin whose calls abort `QUARANTINE_AFTER` times in a row is dropped
/// with a health entry and its id stays gone; one good event in between resets the count.
#[test]
fn a_plugin_that_keeps_aborting_is_quarantined() {
    let trap = r#"
        (memory (export "memory") 1)
        (func (export "schemify_abi") (result i32) i32.const 1)
        (func (export "schemify_alloc") (param i32) (result i32) i32.const 8)
        (func (export "schemify_on_event") (param i32 i32) (result i64) unreachable)"#;
    let mut w = World::new();
    let bad = w.load("bad", BAD, &[], FUEL_DEFAULT);
    let trapper = w.load("trap", &module(trap), &[], FUEL_DEFAULT); // `Load` aborts: 1
    for _ in 2..QUARANTINE_AFTER {
        w.send(Some(trapper), Event::Timer { id: 1 });
    }
    assert_eq!(
        w.plugins.ids(),
        [bad, trapper],
        "one abort short: still loaded"
    );
    w.send(Some(trapper), Event::Timer { id: 1 });
    assert_eq!(w.plugins.ids(), [bad]);
    let last = w.plugins.health().faults().back().cloned().unwrap();
    assert_eq!((last.plugin, last.code), (trapper, Code::Gone));
    assert!(last.detail.contains("quarantined"), "{}", last.detail);
    w.send(Some(trapper), Event::Timer { id: 1 });
    let last = w.plugins.health().faults().back().cloned().unwrap();
    assert_eq!(
        (last.plugin, last.code),
        (trapper, Code::Gone),
        "stays gone"
    );

    for _ in 0..2 {
        for _ in 1..QUARANTINE_AFTER {
            assert_eq!(World::codes(&w.command(bad, "bad.recurse")), [Code::Trap]);
        }
        assert!(
            w.command(bad, "bad.grow").is_empty(),
            "a good event resets the count"
        );
    }
    assert_eq!(w.plugins.ids(), [bad]);
}

/// A WAT module of `body`; wasmi compiles the text (dev feature `wat`).
fn module(body: &str) -> Vec<u8> {
    format!("(module {body})").into_bytes()
}
