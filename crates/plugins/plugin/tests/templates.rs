//! The language templates in `examples/plugins_new/` (docs/plugins/languages.md): every
//! committed `hello.wasm` loads in the real host and behaves like the Rust one, so a
//! template the docs hand out never stops loading unnoticed.

use schemify_plugin::abi::{Event, FUEL_DEFAULT, Kind, Table, col};
use schemify_plugin::{PluginEvents, Plugins, RequestQueues, Snapshots, deliver};

const TEMPLATES: &str = concat!(env!("SCHEMIFY_ROOT"), "/examples/plugins_new");

/// Runs `wasm` through Load and `hello.count` against nets a, b, c, d with pin counts 5, 4,
/// 1, 1;
/// returns why it misbehaved, if it did.
fn misbehaves(wasm: &[u8]) -> Option<String> {
    let mut snap = Snapshots::new();
    snap.fill_strings(Table::Nets, col::nets::NAME, &["a", "b", "c", "d"]);
    snap.fill::<u16>(Table::Nets, col::nets::WIDTH, &[1; 4]);
    snap.fill::<u8>(Table::Nets, col::nets::GLOBAL, &[0; 4]);
    snap.fill::<u32>(Table::Nets, col::nets::PIN_COUNT, &[5, 4, 1, 1]);
    let mut plugins = Plugins::new();
    let mut events = PluginEvents::new();
    let mut queues = RequestQueues::default();
    let id = match plugins.load("hello", wasm, &[], FUEL_DEFAULT) {
        Ok(id) => id,
        Err(code) => return Some(format!("load: {code:?}")),
    };
    deliver(&mut plugins, &events, &mut snap, &mut queues);
    let composed = plugins.composed();
    if composed.find(Kind::Ui, "node tools.hello").is_none()
        || composed
            .find(Kind::Commands, "command hello.count")
            .is_none()
    {
        return Some(format!("contributions: {:?}", composed.records));
    }
    events
        .push(
            Some(id),
            &Event::Command {
                name: "hello.count",
                args: "",
            },
        )
        .expect("one command event fits the queue");
    deliver(&mut plugins, &events, &mut snap, &mut queues);
    let texts: Vec<&str> = queues.logs.iter().map(|l| l.text.as_str()).collect();
    if texts != ["2 single-connection nets: c, d"] {
        return Some(format!("logs: {texts:?}"));
    }
    let faults = plugins.health().faults();
    (!faults.is_empty()).then(|| format!("faults: {faults:?}"))
}

#[test]
fn every_template_loads_and_counts_nets() {
    let mut langs: Vec<_> = std::fs::read_dir(TEMPLATES)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    langs.sort();
    assert!(langs.len() >= 7, "templates: {langs:?}");
    for dir in &langs {
        let wasm = std::fs::read(dir.join("hello.wasm"))
            .unwrap_or_else(|e| panic!("{}: hello.wasm: {e}", dir.display()));
        assert!(
            wasm.len() < 200 * 1024,
            "{}: {} bytes",
            dir.display(),
            wasm.len()
        );
        assert_eq!(misbehaves(&wasm), None, "{}", dir.display());
    }
}

#[test]
fn a_misbehaving_module_is_caught() {
    // The negative space: a module that loads but contributes nothing must fail the check.
    // wasmi takes WAT text too (dev feature `wat`).
    let wat = r#"(module (memory (export "memory") 1)
             (func (export "schemify_abi") (result i32) i32.const 1)
             (func (export "schemify_alloc") (param i32) (result i32) i32.const 16)
             (func (export "schemify_on_event") (param i32 i32) (result i64) i64.const 0))"#;
    assert!(
        misbehaves(wat.as_bytes())
            .unwrap()
            .starts_with("contributions")
    );
    assert_eq!(
        misbehaves(b"not wasm").map(|m| m.starts_with("load")),
        Some(true)
    );
}
