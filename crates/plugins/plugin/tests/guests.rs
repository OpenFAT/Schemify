//! End-to-end through real wasmi with the example guests in `tests/guests/` (rebuilt by
//! `ui/build.sh`): contributions, every import, fuel, faults, rejections, capabilities,
//! pure calls, composition order and worker events, each checked on the request tables
//! and the health report the app reads. Cites plugin-abi §1–§8.

mod common;

use std::thread;
use std::time::Duration;

use common::{World, long_name, snapshots};
use schemify_plugin::abi::{Code, Event, FUEL_DEFAULT, Kind, Level};
use schemify_plugin::{
    EvalRequest, FAULTS_MAX, Fault, IoOp, IoRequest, LOGS_MAX, LogEntry, PENDING_MAX, PluginId,
    RequestQueues,
};

const HELLO: &[u8] = include_bytes!("fixtures/hello.wasm");
const BAD: &[u8] = include_bytes!("fixtures/bad_actor.wasm");
const GMID: &[u8] = include_bytes!("fixtures/gmid_stub.wasm");

// ---- private ----

#[test]
fn hello_contributes_and_counts_nets() {
    let mut world = World::new();
    let hello = world
        .plugins
        .load("hello", HELLO, &[], FUEL_DEFAULT)
        .unwrap();
    assert!(
        world.plugins.has_pending(),
        "Load waits for the next deliver"
    );
    assert!(world.plugins.composed().records.is_empty());
    world.frame();
    assert!(!world.plugins.has_pending());

    let ui = world.plugins.composed().of(Kind::Ui);
    assert_eq!(ui.len(), 1);
    assert_eq!(&*ui[0].key, "node tools.count_nets");
    assert_eq!(ui[0].owner, hello);
    assert!(
        ui[0].text.contains("label \"Count nets (4)\""),
        "{}",
        ui[0].text
    );
    let cmd = world
        .plugins
        .composed()
        .find(Kind::Commands, "command count_nets.run")
        .unwrap();
    assert_eq!(cmd.owner, hello);

    world.send(
        Some(cmd.owner),
        Event::Command {
            name: "count_nets.run",
            args: "",
        },
    );
    let text = "2 single-connection nets".to_owned();
    assert_eq!(
        world.queues.logs,
        [LogEntry {
            plugin: hello,
            level: Level::Info,
            text
        }]
    );

    // Immediate mode: a changed document re-declares the set and the label follows.
    let epoch = world.plugins.composed_epoch();
    world.snapshots = snapshots(&[1, 1]);
    world.send(None, Event::DocChanged { doc: 1, epoch: 2 });
    assert_ne!(world.plugins.composed_epoch(), epoch);
    assert!(
        world.plugins.composed().of(Kind::Ui)[0]
            .text
            .contains("(2)")
    );
    // Re-declaring an identical set is not a change.
    let epoch = world.plugins.composed_epoch();
    world.send(None, Event::DocChanged { doc: 1, epoch: 3 });
    assert_eq!(world.plugins.composed_epoch(), epoch);

    assert!(
        world.plugins.health().faults().is_empty(),
        "{:?}",
        world.plugins.health().faults()
    );
    assert!(world.plugins.health().overrides().is_empty());
}

#[test]
fn hello_inspect_exercises_every_read_import() {
    let mut world = World::new();
    let hello = world.load("hello", HELLO, &["read_project"], FUEL_DEFAULT);
    world.send(
        Some(hello),
        Event::Command {
            name: "hello.inspect",
            args: "",
        },
    );

    let names = Ok::<_, Code>(vec![long_name(), "n1".into(), "n2".into(), "n3".into()]);
    let expected = [
        format!(
            "names={names:?} wrong_type={:?} past_end={:?}",
            Err::<(), _>(Code::Column),
            Err::<(), _>(Code::Range)
        ),
        format!(
            "eval={:?} eval_gone={:?}",
            Ok::<u32, Code>(1),
            Err::<(), _>(Code::Gone)
        ),
        format!("fs_read={:?}", Ok::<u32, Code>(2)),
    ];
    let logs = &world.queues.logs;
    // The import log is staged during the call, so it lands before the output records.
    assert_eq!(logs[0].level, Level::Debug);
    assert_eq!(logs[0].text, "inspected");
    let texts: Vec<&str> = logs[1..].iter().map(|log| log.text.as_str()).collect();
    assert_eq!(texts, expected);
    assert_eq!(
        world.queues.evals,
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
    assert_eq!(world.queues.io, [read]);

    // The app completes the inverted read.
    world.queues = RequestQueues::default();
    world.send(
        Some(hello),
        Event::IoDone {
            handle: 2,
            status: 0,
            bytes: b"contents",
        },
    );
    assert_eq!(world.queues.logs[0].text, "io_done 2 0 contents");
    assert!(
        world.plugins.health().faults().is_empty(),
        "{:?}",
        world.plugins.health().faults()
    );
}

#[test]
fn identical_runs_are_byte_identical() {
    let run = || {
        let mut world = World::new();
        let hello = world.load("hello", HELLO, &["read_project"], FUEL_DEFAULT);
        world.send(
            Some(hello),
            Event::Command {
                name: "hello.inspect",
                args: "",
            },
        );
        world.send(
            Some(hello),
            Event::Command {
                name: "count_nets.run",
                args: "",
            },
        );
        (world.plugins.composed().clone(), world.queues)
    };
    assert_eq!(run(), run());
}

#[test]
fn fuel_is_configurable_and_exhaustion_discards_everything() {
    let mut world = World::new();
    let starved = world.plugins.load("starved", HELLO, &[], 2_000).unwrap();
    world.frame();
    let faults: Vec<Fault> = world.plugins.health().faults().iter().cloned().collect();
    assert_eq!(World::codes(&faults), [Code::Fuel]);
    assert!(world.plugins.contributions().is_empty());
    assert_eq!(world.plugins.health().faults()[0].plugin, starved);

    let bad = world.load("bad", BAD, &["read_project"], 1_000_000);
    let before = world.plugins.contributions().clone();
    assert_eq!(
        before.get(bad, Kind::Commands, ""),
        Some("command bad.spin \"Spin\"")
    );
    let recorded = world.plugins.health().recorded();
    let faults = world.command(bad, "bad.spin");
    assert_eq!(World::codes(&faults), [Code::Fuel]);
    // The report names the plugin, the event and the budget (plugin-abi §2).
    assert_eq!(
        faults[0].detail,
        "bad: command bad.spin: out of fuel (budget 1000000), outputs discarded"
    );
    assert_eq!(world.plugins.health().recorded(), recorded + 1);
    // The aborted event counts as finished: nothing of `bad` is left to wait for.
    let (given, finished) = world.plugins.event_counts(bad).unwrap();
    assert!(given >= 2, "load and the command");
    assert_eq!(given, finished);
    assert_eq!(world.plugins.event_counts(PluginId(u32::MAX - 1)), None);
    assert_eq!(
        world.queues,
        RequestQueues::default(),
        "staged fs_read and log were discarded"
    );
    assert_eq!(world.plugins.contributions(), &before);

    // The plugin survives and keeps working.
    let faults = world.command(bad, "bad.grow");
    assert!(faults.is_empty());
    assert_eq!(
        world.queues.logs[0].text, "grow -1 true",
        "the 256 MiB limiter refused 1 GiB"
    );
}

#[test]
fn bad_buffers_traps_and_memory_are_declared_errors() {
    let mut world = World::new();
    let bad = world.load("bad", BAD, &[], FUEL_DEFAULT);
    assert_eq!(
        World::codes(&world.command(bad, "bad.garbage_ptr")),
        [Code::Range]
    );
    assert_eq!(
        World::codes(&world.command(bad, "bad.null_ptr")),
        [Code::Range]
    );
    assert_eq!(
        World::codes(&world.command(bad, "bad.huge_output")),
        [Code::Space]
    );
    assert_eq!(World::codes(&world.command(bad, "bad.oom")), [Code::Trap]);
    assert_eq!(
        World::codes(&world.command(bad, "bad.recurse")),
        [Code::Trap]
    );
    assert!(world.command(bad, "bad.lie_alloc").is_empty());
    assert_eq!(
        World::codes(&world.command(bad, "bad.anything")),
        [Code::Range],
        "alloc lied"
    );
    assert_eq!(world.queues, RequestQueues::default());
    assert_eq!(
        world.command(bad, "bad.grow"),
        [],
        "still alive after every fault"
    );
}

#[test]
fn malformed_records_are_rejected_one_by_one_and_echoed_once() {
    let mut world = World::new();
    let bad = world.load("bad", BAD, &[], FUEL_DEFAULT);
    assert_eq!(
        World::codes(&world.command(bad, "bad.garbage_bytes")),
        [Code::Parse, Code::Parse]
    );
    world.frame(); // deliver those two echoes
    let faults = world.command(bad, "bad.mixed");
    use Code::{Gone, Parse, Range};
    assert_eq!(
        World::codes(&faults),
        [Parse, Gone, Parse, Parse, Range, Parse, Parse, Parse]
    );
    let logs: Vec<&str> = world
        .queues
        .logs
        .iter()
        .map(|log| log.text.as_str())
        .collect();
    assert_eq!(
        logs,
        ["ok", "also ok"],
        "valid records survive their neighbours"
    );
    assert!(world.queues.edits.is_empty() && world.queues.timers.is_empty());
    assert!(
        world.plugins.has_pending(),
        "rejections go back to the plugin"
    );

    // The plugin answers each Rejected with garbage: recorded, never echoed again.
    let before = world.plugins.health().faults().len();
    world.frame();
    assert_eq!(world.plugins.health().faults().len() - before, 8);
    assert!(!world.plugins.has_pending());
    world.frame();
    assert_eq!(world.plugins.health().faults().len() - before, 8);
}

#[test]
fn ungranted_capabilities_are_denied_and_recorded() {
    let mut world = World::new();
    let bad = world.load("bad", BAD, &["read_project", "spawn:ngspice"], FUEL_DEFAULT);
    let faults = world.command(bad, "bad.deny");
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
    assert_eq!(world.queues.logs[0].text, format!("deny {returns:?}"));
    assert!(world.queues.io.is_empty());
}

#[test]
fn floods_stay_bounded() {
    let mut world = World::new();
    let bad = world.load("bad", BAD, &[], FUEL_DEFAULT);
    let faults = world.command(bad, "bad.flood");
    assert_eq!(world.queues.logs.len(), LOGS_MAX);
    assert!(faults.iter().all(|f| f.code == Code::Space));
    assert_eq!(world.plugins.health().faults().len(), FAULTS_MAX);
    assert!(world.plugins.health().dropped() > 0);
    world.frame();
    // Only PENDING_MAX rejections were echoed; each got one garbage answer.
    let parse = world
        .plugins
        .health()
        .faults()
        .iter()
        .filter(|f| f.code == Code::Parse)
        .count();
    assert_eq!(parse, PENDING_MAX);
    assert!(!world.plugins.has_pending());
}

#[test]
fn pure_calls_deny_imports_and_route_through_the_registry() {
    let mut world = World::new();
    let bad = world.load("bad", BAD, &[], FUEL_DEFAULT);
    let gmid = world.load("gmid", GMID, &[], FUEL_DEFAULT);
    let hello = world.load("hello", HELLO, &[], FUEL_DEFAULT);
    let mut out = String::new();
    world.plugins.call(bad, 0, "", &mut out).unwrap();
    assert_eq!(
        out,
        Code::Denied.neg().to_string(),
        "table_len is denied in a pure call"
    );

    let f = &world.plugins.composed().functions;
    let row = f.find("gmid.width").unwrap();
    assert_eq!((f.plugin[row], f.fn_id[row], f.arity[row]), (gmid, 0, 3));
    assert_eq!(f.find("gmid.length"), None);
    for (args, want) in [
        ("10 2 0.5", "25"),
        ("3 1 1", "2.25"),
        ("2 0.5 0.0001", "5e-5"),
        ("1e3 1e3 1e3", "250000000000"),
    ] {
        world.plugins.call(gmid, 0, args, &mut out).unwrap();
        assert_eq!(out, want, "{args}");
    }
    assert_eq!(
        world.plugins.call(gmid, 7, "1 1 1", &mut out),
        Err(Code::Range)
    );
    assert_eq!(
        world.plugins.call(gmid, 0, "1 x 1", &mut out),
        Err(Code::Parse)
    );
    assert_eq!(
        world.plugins.call(gmid, 0, "0 1 1", &mut out),
        Err(Code::Range)
    );
    assert_eq!(
        world.plugins.call(hello, 0, "", &mut out),
        Err(Code::Range),
        "SDK default: no functions"
    );
    assert_eq!(
        world.plugins.call(PluginId(999), 0, "", &mut out),
        Err(Code::Gone)
    );
    assert!(
        world.plugins.health().faults().is_empty(),
        "function errors are not faults"
    );
}

#[test]
fn last_wins_through_wasm_and_unload_restores() {
    let mut world = World::new();
    let a = world.load("hello-a", HELLO, &[], FUEL_DEFAULT);
    let b = world.load("hello-b", HELLO, &[], FUEL_DEFAULT);
    let composed = world.plugins.composed();
    assert!(
        composed.records.iter().all(|r| r.owner == b),
        "the later plugin wins every key"
    );
    let overrides = world.plugins.health().overrides();
    assert_eq!(overrides.len(), 3, "1 ui node + 2 commands");
    assert!(overrides.iter().all(|o| o.loser == a && o.winner == b));

    world
        .plugins
        .unload(b, &mut world.snapshots, &mut world.queues)
        .unwrap();
    assert!(
        world
            .plugins
            .composed()
            .records
            .iter()
            .all(|r| r.owner == a)
    );
    assert!(world.plugins.health().overrides().is_empty());
    assert_eq!(world.plugins.ids(), [a]);
    assert_eq!(
        world
            .plugins
            .unload(b, &mut world.snapshots, &mut world.queues),
        Err(Code::Gone)
    );

    let before = world.plugins.health().faults().len();
    world.send(Some(b), Event::Timer { id: 1 });
    let fault = &world.plugins.health().faults()[before];
    assert_eq!(
        (fault.plugin, fault.code),
        (b, Code::Gone),
        "stale id: declared, not misdelivered"
    );
}

/// Worker events (`set_worker`): the same outputs, faults and composition as on the caller's
/// thread, taken in by a later delivery, in event order; the plugin's pure calls are denied
/// while one runs; `unload` waits for it.
#[test]
fn worker_events_match_synchronous_ones() {
    fn script(world: &mut World, id: PluginId) {
        let run = Event::Command {
            name: "count_nets.run",
            args: "",
        };
        world.send(Some(id), run);
        world.send(None, Event::DocChanged { doc: 1, epoch: 3 });
        let inspect = Event::Command {
            name: "hello.inspect",
            args: "",
        };
        world.send(Some(id), inspect);
    }
    fn faults(world: &World) -> Vec<Fault> {
        world.plugins.health().faults().iter().cloned().collect()
    }
    let mut sync = World::new();
    let a = sync.load("hello", HELLO, &[], FUEL_DEFAULT);
    script(&mut sync, a);

    let mut background = World::new();
    let b = background
        .plugins
        .load("hello", HELLO, &[], FUEL_DEFAULT)
        .unwrap();
    assert_eq!(a, b, "same id sequence");
    background.plugins.set_worker(b, true).unwrap();
    background.frame();
    assert!(background.plugins.busy(b) && background.plugins.running() == 1);
    assert!(
        background.plugins.contributions().is_empty(),
        "taken in by a later delivery"
    );
    let mut out = String::new();
    assert_eq!(
        background.plugins.call(b, 0, "", &mut out),
        Err(Code::Denied)
    );
    script(&mut background, b);
    for _ in 0..10_000 {
        if background.plugins.running() == 0 && !background.plugins.has_pending() {
            break;
        }
        thread::sleep(Duration::from_millis(1));
        background.frame();
    }
    assert_eq!(background.plugins.running(), 0);
    assert_eq!(background.plugins.jobs_done(), 4, "Load and three events");
    assert_eq!(background.plugins.composed(), sync.plugins.composed());
    assert_eq!(background.queues, sync.queues);
    assert_eq!(faults(&background), faults(&sync));

    background.send(None, Event::DocChanged { doc: 1, epoch: 4 });
    assert!(background.plugins.busy(b));
    background
        .plugins
        .unload(b, &mut background.snapshots, &mut background.queues)
        .unwrap();
    assert!(background.plugins.ids().is_empty() && background.plugins.contributions().is_empty());
    assert_eq!(background.plugins.running(), 0);
}
