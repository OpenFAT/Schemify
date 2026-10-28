//! End-to-end through real wasmi with the example guests in `tests/guests/` (rebuilt by
//! `ui/build.sh`): contributions, every import, fuel, faults, rejections, capabilities,
//! pure calls, composition order and worker events, each checked on the request tables
//! and the health report the app reads. Cites plugin-abi §1–§8.

mod common;

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
    let recorded = w.plugins.health().recorded();
    let faults = w.command(bad, "bad.spin");
    assert_eq!(World::codes(&faults), [Code::Fuel]);
    // The report names the plugin, the event and the budget (plugin-abi §2).
    assert_eq!(
        faults[0].detail,
        "bad: command bad.spin: out of fuel (budget 1000000), outputs discarded"
    );
    assert_eq!(w.plugins.health().recorded(), recorded + 1);
    // The aborted event counts as finished: nothing of `bad` is left to wait for.
    let (given, finished) = w.plugins.event_counts(bad).unwrap();
    assert!(given >= 2, "load and the command");
    assert_eq!(given, finished);
    assert_eq!(w.plugins.event_counts(PluginId(u32::MAX - 1)), None);
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
