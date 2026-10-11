//! End-to-end through real wasmi: the example guests in `plugins/` (rebuilt by
//! `plugins/build.sh`) and hand-written WAT modules for load-time failures.

use schemify_plugin::abi::{Code, Event, FUEL_DEFAULT, Kind, Level, Table, col};
use schemify_plugin::{
    EvalRequest, FAULTS_MAX, Fault, IoOp, IoRequest, LOGS_MAX, LogEntry, PENDING_MAX, PLUGINS_MAX,
    PluginEvents, PluginId, Plugins, QUARANTINE_AFTER, RequestQueues, Snapshots, deliver,
};

const HELLO: &[u8] = include_bytes!("fixtures/hello.wasm");
const BAD: &[u8] = include_bytes!("fixtures/bad_actor.wasm");
const GMID: &[u8] = include_bytes!("fixtures/gmid_stub.wasm");

/// A record longer than the SDK's first `read_record` guess, to force the `E_SPACE` retry.
fn long_record() -> String {
    format!(
        "instance R1 lib:res\n  at 0 0\n  note \"{}\"",
        "x".repeat(300)
    )
}

fn snapshots(pin_counts: &[u32]) -> Snapshots {
    let mut s = Snapshots::new();
    s.fill::<u32>(Table::Docs, col::docs::HANDLE, &[1]);
    s.fill::<u8>(Table::Docs, col::docs::KIND, &[1]);
    s.fill::<u8>(Table::Docs, col::docs::DIRTY, &[0]);
    s.fill_strings(Table::Docs, col::docs::PATH, &["lib/top.shcomp"]);
    s.fill_strings(Table::Docs, col::docs::CELL, &["top"]);
    let names: Vec<String> = (0..pin_counts.len()).map(|i| format!("n{i}")).collect();
    s.fill_strings(Table::Nets, col::nets::NAME, &names);
    s.fill::<u16>(Table::Nets, col::nets::WIDTH, &vec![1; pin_counts.len()]);
    s.fill::<u8>(Table::Nets, col::nets::GLOBAL, &vec![0; pin_counts.len()]);
    s.fill::<u32>(Table::Nets, col::nets::PIN_COUNT, pin_counts);
    s.push_record(1, 42, &long_record());
    s
}

/// One host plus the app-side tables the spine would own.
struct World {
    plugins: Plugins,
    events: PluginEvents,
    snap: Snapshots,
    queues: RequestQueues,
}

impl World {
    fn new() -> World {
        World {
            plugins: Plugins::new(),
            events: PluginEvents::new(),
            snap: snapshots(&[5, 4, 1, 1]),
            queues: RequestQueues::default(),
        }
    }

    fn load(&mut self, name: &str, wasm: &[u8], grants: &[&str], fuel: u64) -> PluginId {
        let id = self.plugins.load(name, wasm, grants, fuel).unwrap();
        self.frame();
        id
    }

    fn frame(&mut self) {
        deliver(
            &mut self.plugins,
            &self.events,
            &mut self.snap,
            &mut self.queues,
        );
        self.events.clear();
    }

    fn send(&mut self, target: Option<PluginId>, event: Event<'_>) {
        self.events.push(target, &event).unwrap();
        self.frame();
    }

    fn command(&mut self, target: PluginId, name: &str) -> Vec<Fault> {
        let before = self.plugins.health().faults().len();
        self.send(Some(target), Event::Command { name, args: "" });
        self.plugins
            .health()
            .faults()
            .iter()
            .skip(before)
            .cloned()
            .collect()
    }

    fn codes(faults: &[Fault]) -> Vec<Code> {
        faults.iter().map(|f| f.code).collect()
    }
}

#[test]
fn hello_contributes_and_counts_nets() {
    let mut w = World::new();
    let hello = w.plugins.load("hello", HELLO, &[], FUEL_DEFAULT).unwrap();
    assert!(w.plugins.has_pending(), "Load waits for the next deliver");
    assert!(w.plugins.composed().records.is_empty());
    w.frame();
    assert!(!w.plugins.has_pending());

    let ui = w.plugins.composed().of(Kind::Ui);
    assert_eq!(ui.len(), 1);
    assert_eq!(&*ui[0].key, "node tools.count_nets");
    assert_eq!(ui[0].owner, hello);
    assert!(
        ui[0].text.contains("label \"Count nets (4)\""),
        "{}",
        ui[0].text
    );
    let cmd = w
        .plugins
        .composed()
        .find(Kind::Commands, "command count_nets.run")
        .unwrap();
    assert_eq!(cmd.owner, hello);

    w.send(
        Some(cmd.owner),
        Event::Command {
            name: "count_nets.run",
            args: "",
        },
    );
    let text = "2 single-connection nets".to_owned();
    assert_eq!(
        w.queues.logs,
        [LogEntry {
            plugin: hello,
            level: Level::Info,
            text
        }]
    );

    // Immediate mode: a changed document re-declares the set and the label follows.
    let epoch = w.plugins.composed_epoch();
    w.snap = snapshots(&[1, 1]);
    w.send(None, Event::DocChanged { doc: 1, epoch: 2 });
    assert_ne!(w.plugins.composed_epoch(), epoch);
    assert!(w.plugins.composed().of(Kind::Ui)[0].text.contains("(2)"));
    // Re-declaring an identical set is not a change.
    let epoch = w.plugins.composed_epoch();
    w.send(None, Event::DocChanged { doc: 1, epoch: 3 });
    assert_eq!(w.plugins.composed_epoch(), epoch);

    assert!(
        w.plugins.health().faults().is_empty(),
        "{:?}",
        w.plugins.health().faults()
    );
    assert!(w.plugins.health().overrides().is_empty());
}

#[test]
fn hello_inspect_exercises_every_read_import() {
    let mut w = World::new();
    let hello = w.load("hello", HELLO, &["read_project"], FUEL_DEFAULT);
    w.send(
        Some(hello),
        Event::Command {
            name: "hello.inspect",
            args: "",
        },
    );

    let names = Ok::<_, Code>(
        vec!["n0", "n1", "n2", "n3"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
    );
    let expected = [
        format!(
            "names={names:?} wrong_type={:?} past_end={:?}",
            Err::<(), _>(Code::Column),
            Err::<(), _>(Code::Range)
        ),
        format!(
            "record={:?} missing={:?}",
            Ok::<_, Code>(long_record()),
            Err::<(), _>(Code::Gone)
        ),
        format!(
            "eval={:?} eval_gone={:?}",
            Ok::<u32, Code>(1),
            Err::<(), _>(Code::Gone)
        ),
        format!("fs_read={:?}", Ok::<u32, Code>(2)),
    ];
    let logs: Vec<(Level, &str)> = w
        .queues
        .logs
        .iter()
        .map(|l| (l.level, l.text.as_str()))
        .collect();
    // The import log is staged during the call, so it lands before the output records.
    assert_eq!(logs[0], (Level::Debug, "inspected"));
    assert_eq!(logs[1..].iter().map(|l| l.1).collect::<Vec<_>>(), expected);
    assert_eq!(
        w.queues.evals,
        [EvalRequest {
            plugin: hello,
            handle: 1,
            doc: 1,
            expr: "w*2".into()
        }]
    );
    let read = IoRequest {
        plugin: hello,
        handle: 2,
        op: IoOp::FsRead,
        args: vec!["lib/r.shprim".into()],
        data: vec![],
    };
    assert_eq!(w.queues.io, [read]);

    // The app completes the inverted read.
    w.queues = RequestQueues::default();
    w.send(
        Some(hello),
        Event::IoDone {
            handle: 2,
            status: 0,
            bytes: b"contents",
        },
    );
    assert_eq!(w.queues.logs[0].text, "io_done 2 0 contents");
    assert!(
        w.plugins.health().faults().is_empty(),
        "{:?}",
        w.plugins.health().faults()
    );
}

#[test]
fn identical_runs_are_byte_identical() {
    let run = || {
        let mut w = World::new();
        let hello = w.load("hello", HELLO, &["read_project"], FUEL_DEFAULT);
        w.send(
            Some(hello),
            Event::Command {
                name: "hello.inspect",
                args: "",
            },
        );
        w.send(
            Some(hello),
            Event::Command {
                name: "count_nets.run",
                args: "",
            },
        );
        (w.plugins.composed().clone(), w.queues)
    };
    assert_eq!(run(), run());
}

#[test]
fn fuel_is_configurable_and_exhaustion_discards_everything() {
    let mut w = World::new();
    let starved = w.plugins.load("starved", HELLO, &[], 2_000).unwrap();
    w.frame();
    assert_eq!(
        World::codes(
            &w.plugins
                .health()
                .faults()
                .iter()
                .cloned()
                .collect::<Vec<_>>()
        ),
        [Code::Fuel]
    );
    assert!(w.plugins.contributions().is_empty());
    assert_eq!(w.plugins.health().faults()[0].plugin, starved);

    let bad = w.load("bad", BAD, &["read_project"], 1_000_000);
    let before = w.plugins.contributions().clone();
    assert_eq!(
        before.get(bad, Kind::Commands, ""),
        Some("command bad.spin \"Spin\"")
    );
    let faults = w.command(bad, "bad.spin");
    assert_eq!(World::codes(&faults), [Code::Fuel]);
    assert_eq!(
        w.queues,
        RequestQueues::default(),
        "staged fs_read and log were discarded"
    );
    assert_eq!(w.plugins.contributions(), &before);

    // The plugin survives and keeps working.
    let faults = w.command(bad, "bad.grow");
    assert!(faults.is_empty());
    assert_eq!(
        w.queues.logs[0].text, "grow -1 true",
        "the 256 MiB limiter refused 1 GiB"
    );
}

#[test]
fn bad_buffers_traps_and_memory_are_declared_errors() {
    let mut w = World::new();
    let bad = w.load("bad", BAD, &[], FUEL_DEFAULT);
    assert_eq!(
        World::codes(&w.command(bad, "bad.garbage_ptr")),
        [Code::Range]
    );
    assert_eq!(World::codes(&w.command(bad, "bad.null_ptr")), [Code::Range]);
    assert_eq!(
        World::codes(&w.command(bad, "bad.huge_output")),
        [Code::Space]
    );
    assert_eq!(World::codes(&w.command(bad, "bad.oom")), [Code::Trap]);
    assert_eq!(World::codes(&w.command(bad, "bad.recurse")), [Code::Trap]);
    assert!(w.command(bad, "bad.lie_alloc").is_empty());
    assert_eq!(
        World::codes(&w.command(bad, "bad.anything")),
        [Code::Range],
        "alloc lied"
    );
    assert_eq!(w.queues, RequestQueues::default());
    assert_eq!(
        w.command(bad, "bad.grow"),
        [],
        "still alive after every fault"
    );
}

#[test]
fn malformed_records_are_rejected_one_by_one_and_echoed_once() {
    let mut w = World::new();
    let bad = w.load("bad", BAD, &[], FUEL_DEFAULT);
    assert_eq!(
        World::codes(&w.command(bad, "bad.garbage_bytes")),
        [Code::Parse, Code::Parse]
    );
    w.frame(); // deliver those two echoes
    let faults = w.command(bad, "bad.mixed");
    use Code::{Gone, Parse, Range};
    assert_eq!(
        World::codes(&faults),
        [Parse, Gone, Parse, Parse, Range, Parse, Parse, Parse]
    );
    let logs: Vec<&str> = w.queues.logs.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        logs,
        ["ok", "also ok"],
        "valid records survive their neighbours"
    );
    assert!(w.queues.edits.is_empty() && w.queues.timers.is_empty());
    assert!(w.plugins.has_pending(), "rejections go back to the plugin");

    // The plugin answers each Rejected with garbage: recorded, never echoed again.
    let before = w.plugins.health().faults().len();
    w.frame();
    assert_eq!(w.plugins.health().faults().len() - before, 8);
    assert!(!w.plugins.has_pending());
    w.frame();
    assert_eq!(w.plugins.health().faults().len() - before, 8);
}

#[test]
fn ungranted_capabilities_are_denied_and_recorded() {
    let mut w = World::new();
    let bad = w.load("bad", BAD, &["read_project", "spawn:ngspice"], FUEL_DEFAULT);
    let faults = w.command(bad, "bad.deny");
    assert_eq!(World::codes(&faults), [Code::Denied; 4]);
    assert!(faults[0].detail.contains("Spawn") && faults[3].detail.contains("../secret"));
    let neg = |c: Code| c.neg();
    let returns = [
        neg(Code::Denied),
        neg(Code::Denied),
        neg(Code::Denied),
        neg(Code::Denied),
        neg(Code::Table),
        neg(Code::Range),
        neg(Code::Space),
    ];
    assert_eq!(w.queues.logs[0].text, format!("deny {returns:?}"));
    assert!(w.queues.io.is_empty());
}

#[test]
fn floods_stay_bounded() {
    let mut w = World::new();
    let bad = w.load("bad", BAD, &[], FUEL_DEFAULT);
    let faults = w.command(bad, "bad.flood");
    assert_eq!(w.queues.logs.len(), LOGS_MAX);
    assert!(faults.iter().all(|f| f.code == Code::Space));
    assert_eq!(w.plugins.health().faults().len(), FAULTS_MAX);
    assert!(w.plugins.health().dropped() > 0);
    w.frame();
    // Only PENDING_MAX rejections were echoed; each got one garbage answer.
    let parse = w
        .plugins
        .health()
        .faults()
        .iter()
        .filter(|f| f.code == Code::Parse)
        .count();
    assert_eq!(parse, PENDING_MAX);
    assert!(!w.plugins.has_pending());
}

#[test]
fn pure_calls_deny_imports_and_route_through_the_registry() {
    let mut w = World::new();
    let bad = w.load("bad", BAD, &[], FUEL_DEFAULT);
    let gmid = w.load("gmid", GMID, &[], FUEL_DEFAULT);
    let hello = w.load("hello", HELLO, &[], FUEL_DEFAULT);
    let mut out = String::new();
    w.plugins.call(bad, 0, "", &mut out).unwrap();
    assert_eq!(
        out,
        Code::Denied.neg().to_string(),
        "table_len is denied in a pure call"
    );

    let f = &w.plugins.composed().functions;
    let row = f.find("gmid.width").unwrap();
    assert_eq!((f.plugin[row], f.fn_id[row], f.arity[row]), (gmid, 0, 3));
    assert_eq!(f.find("gmid.length"), None);
    for (args, want) in [
        ("10 2 0.5", "25"),
        ("3 1 1", "2.25"),
        ("2 0.5 0.0001", "5e-5"),
        ("1e3 1e3 1e3", "250000000000"),
    ] {
        w.plugins.call(gmid, 0, args, &mut out).unwrap();
        assert_eq!(out, want, "{args}");
    }
    assert_eq!(w.plugins.call(gmid, 7, "1 1 1", &mut out), Err(Code::Range));
    assert_eq!(w.plugins.call(gmid, 0, "1 x 1", &mut out), Err(Code::Parse));
    assert_eq!(w.plugins.call(gmid, 0, "0 1 1", &mut out), Err(Code::Range));
    assert_eq!(
        w.plugins.call(hello, 0, "", &mut out),
        Err(Code::Range),
        "SDK default: no functions"
    );
    assert_eq!(
        w.plugins.call(PluginId(999), 0, "", &mut out),
        Err(Code::Gone)
    );
    assert!(
        w.plugins.health().faults().is_empty(),
        "function errors are not faults"
    );
}

#[test]
fn last_wins_through_wasm_and_unload_restores() {
    let mut w = World::new();
    let a = w.load("hello-a", HELLO, &[], FUEL_DEFAULT);
    let b = w.load("hello-b", HELLO, &[], FUEL_DEFAULT);
    let composed = w.plugins.composed();
    assert!(
        composed.records.iter().all(|r| r.owner == b),
        "the later plugin wins every key"
    );
    let overrides = w.plugins.health().overrides();
    assert_eq!(overrides.len(), 3, "1 ui node + 2 commands");
    assert!(overrides.iter().all(|o| o.loser == a && o.winner == b));

    w.plugins.unload(b, &mut w.snap, &mut w.queues).unwrap();
    assert!(w.plugins.composed().records.iter().all(|r| r.owner == a));
    assert!(w.plugins.health().overrides().is_empty());
    assert_eq!(w.plugins.ids(), [a]);
    assert_eq!(
        w.plugins.unload(b, &mut w.snap, &mut w.queues),
        Err(Code::Gone)
    );

    let before = w.plugins.health().faults().len();
    w.send(Some(b), Event::Timer { id: 1 });
    let fault = &w.plugins.health().faults()[before];
    assert_eq!(
        (fault.plugin, fault.code),
        (b, Code::Gone),
        "stale id: declared, not misdelivered"
    );
}

const MINIMAL: &str = r#"
    (memory (export "memory") 1)
    (func (export "schemify_alloc") (param i32) (result i32) i32.const 8)
    (func (export "schemify_on_event") (param i32 i32) (result i64) i64.const 0)
"#;

fn module(body: &str) -> Vec<u8> {
    format!("(module {body})").into_bytes()
}

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

/// Worker events (`set_worker`): the same outputs, faults and composition as on the caller's
/// thread, taken in by a later delivery, in event order; the plugin's pure calls are denied
/// while one runs; `unload` waits for it.
#[test]
fn worker_events_match_synchronous_ones() {
    fn script(w: &mut World, id: PluginId) {
        let run = Event::Command {
            name: "count_nets.run",
            args: "",
        };
        w.send(Some(id), run);
        w.send(None, Event::DocChanged { doc: 1, epoch: 3 });
        let inspect = Event::Command {
            name: "hello.inspect",
            args: "",
        };
        w.send(Some(id), inspect);
    }
    fn faults(w: &World) -> Vec<Fault> {
        w.plugins.health().faults().iter().cloned().collect()
    }
    let mut sync = World::new();
    let a = sync.load("hello", HELLO, &[], FUEL_DEFAULT);
    script(&mut sync, a);

    let mut bg = World::new();
    let b = bg.plugins.load("hello", HELLO, &[], FUEL_DEFAULT).unwrap();
    assert_eq!(a, b, "same id sequence");
    bg.plugins.set_worker(b, true).unwrap();
    bg.frame();
    assert!(bg.plugins.busy(b) && bg.plugins.running() == 1);
    assert!(
        bg.plugins.contributions().is_empty(),
        "taken in by a later delivery"
    );
    let mut out = String::new();
    assert_eq!(bg.plugins.call(b, 0, "", &mut out), Err(Code::Denied));
    script(&mut bg, b);
    for _ in 0..10_000 {
        if bg.plugins.running() == 0 && !bg.plugins.has_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        bg.frame();
    }
    assert_eq!(bg.plugins.running(), 0);
    assert_eq!(bg.plugins.jobs_done(), 4, "Load and three events");
    assert_eq!(bg.plugins.composed(), sync.plugins.composed());
    assert_eq!(bg.queues, sync.queues);
    assert_eq!(faults(&bg), faults(&sync));

    bg.send(None, Event::DocChanged { doc: 1, epoch: 4 });
    assert!(bg.plugins.busy(b));
    bg.plugins.unload(b, &mut bg.snap, &mut bg.queues).unwrap();
    assert!(bg.plugins.ids().is_empty() && bg.plugins.contributions().is_empty());
    assert_eq!(bg.plugins.running(), 0);
}
