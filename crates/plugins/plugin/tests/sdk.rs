//! The guest libraries for other languages (`crates/protocol/abi/sdk/`): each library's
//! conformance guest (`sdk/CONFORMANCE.md`) runs in the real host, and every event it
//! parses, value it unescapes and record it writes must come out as the Rust SDK's would.

use schemify_plugin::abi::{Code, EditMode, Event, FUEL_DEFAULT, Kind, Level, Table, col};
use schemify_plugin::{PluginEvents, PluginId, Plugins, RequestQueues, Snapshots, deliver};

const SDK: &str = concat!(env!("SCHEMIFY_ROOT"), "/crates/protocol/abi/sdk");

/// Every library under `sdk/`, by directory name; each must have a conformance guest.
fn libs() -> Vec<String> {
    let mut libs: Vec<String> = std::fs::read_dir(SDK)
        .expect("the sdk directory is in the repository")
        .map(|e| e.expect("a readable sdk entry"))
        .filter(|e| e.path().is_dir())
        .map(|e| {
            e.file_name()
                .into_string()
                .expect("sdk directory names are UTF-8")
        })
        .collect();
    libs.sort();
    assert!(libs.len() >= 5, "libraries: {libs:?}");
    libs
}

fn guest(lib: &str) -> Vec<u8> {
    let path = format!("{SDK}/{lib}/conformance/conformance.wasm");
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

struct World {
    plugins: Plugins,
    id: PluginId,
    snap: Snapshots,
    queues: RequestQueues,
}

impl World {
    fn new(lib: &str) -> World {
        let mut snap = Snapshots::new();
        snap.fill::<u32>(Table::Docs, col::docs::HANDLE, &[1]);
        snap.fill::<u8>(Table::Docs, col::docs::KIND, &[1]);
        snap.fill::<u8>(Table::Docs, col::docs::DIRTY, &[0]);
        snap.fill_strings(Table::Docs, col::docs::PATH, &["lib/top.shcomp"]);
        snap.fill_strings(Table::Docs, col::docs::CELL, &["top"]);
        snap.fill_strings(Table::Nets, col::nets::NAME, &["a", "b c", "d", "é"]);
        snap.fill::<u16>(Table::Nets, col::nets::WIDTH, &[1; 4]);
        snap.fill::<u8>(Table::Nets, col::nets::GLOBAL, &[0; 4]);
        snap.fill::<u32>(Table::Nets, col::nets::PIN_COUNT, &[5, 4, 1, 1]);
        let mut plugins = Plugins::new();
        let id = plugins
            .load(lib, &guest(lib), &[], FUEL_DEFAULT)
            .unwrap_or_else(|code| panic!("{lib}: load: {code:?}"));
        let mut w = World {
            plugins,
            id,
            snap,
            queues: RequestQueues::default(),
        };
        w.send(&[]);
        w
    }

    fn send(&mut self, events: &[Event<'_>]) {
        let mut queue = PluginEvents::new();
        for e in events {
            queue
                .push(Some(self.id), e)
                .expect("a test event fits the queue");
        }
        deliver(&mut self.plugins, &queue, &mut self.snap, &mut self.queues);
    }

    fn logs(&mut self) -> Vec<(Level, String)> {
        let logs = std::mem::take(&mut self.queues.logs);
        logs.into_iter().map(|l| (l.level, l.text)).collect()
    }

    fn faults(&self) -> Vec<String> {
        let faults = self.plugins.health().faults();
        faults
            .iter()
            .map(|f| format!("{:?} {}", f.code, f.detail))
            .collect()
    }
}

fn info(texts: &[&str]) -> Vec<(Level, String)> {
    texts
        .iter()
        .map(|t| (Level::Info, (*t).to_owned()))
        .collect()
}

#[test]
fn events_parse_like_the_rust_sdk() {
    for lib in &libs() {
        let mut w = World::new(lib);
        let composed = w.plugins.composed();
        for key in ["command conf.echo", "command conf.out", "command conf.read"] {
            assert!(composed.find(Kind::Commands, key).is_some(), "{lib}: {key}");
        }
        assert_eq!(composed.of(Kind::Functions).len(), 2, "{lib}");
        w.send(&[
            Event::DocOpened { doc: 3 },
            Event::DocClosed { doc: 3 },
            Event::DocChanged { doc: 1, epoch: 9 },
            Event::Saved { doc: 2 },
            Event::SelectionChanged,
            Event::NetlistRequest {
                dialect: "spice",
                bench: "lib:tb",
            },
            Event::RunJob {
                job: 5,
                lang: "python",
            },
            Event::RunDone { job: 5 },
            Event::Timer { id: 42 },
            Event::IoDone {
                handle: 7,
                status: -2,
                bytes: "contents ü".as_bytes(),
            },
            Event::Rejected {
                code: Code::Parse,
                record: b"bogus record",
            },
            Event::Command {
                name: "conf.echo",
                args: r#"a "b c" "q\"\\\n\t" "\u{e9}" "#,
            },
            Event::Command {
                name: "x.y",
                args: "",
            },
            Event::Unload,
        ]);
        assert_eq!(
            w.logs(),
            info(&[
                "load",
                "doc_opened 3",
                "doc_closed 3",
                "doc_changed 1 9",
                "saved 2",
                "selection_changed",
                "netlist_request spice lib:tb",
                "run_job 5 python",
                "run_done 5",
                "timer 42",
                "io_done 7 -2 contents ü",
                "rejected 7 bogus record",
                "command conf.echo [a] [b c] [q\"\\\n\t] [é]",
                "command x.y",
                "unload",
            ]),
            "{lib}"
        );
        assert_eq!(w.faults(), Vec::<String>::new(), "{lib}");
    }
}

#[test]
fn records_are_written_like_the_rust_sdk() {
    for lib in &libs() {
        let mut w = World::new(lib);
        w.logs();
        w.send(&[Event::Command {
            name: "conf.out",
            args: "",
        }]);
        let marks = w.plugins.composed().of(Kind::Annotations);
        assert_eq!(marks.len(), 1, "{lib}");
        assert_eq!(
            (&*marks[0].scope, &*marks[0].text),
            ("erc pass", "mark R1 warn \"hi\"")
        );
        let edits: Vec<_> = w
            .queues
            .edits
            .iter()
            .map(|e| (e.doc, e.mode, e.label.as_str(), e.ops.as_str()))
            .collect();
        assert_eq!(
            edits,
            [
                (1, EditMode::Stage, "size it", "set_attr M1 w 2u"),
                (
                    1,
                    EditMode::Commit,
                    "move",
                    "set_attr R1 at 10 20\nset_attr R2 at 0 0"
                ),
            ],
            "{lib}"
        );
        let run = &w.queues.runs[..];
        assert_eq!(run.len(), 1, "{lib}");
        assert_eq!(
            (&*run[0].bench, &*run[0].config, &*run[0].corner),
            ("lib:tb", "default", "tt")
        );
        let vary = [("vdd", "1.8"), ("w", "two words")].map(|(a, b)| (a.into(), b.into()));
        assert_eq!(run[0].vary, vary, "{lib}");
        assert_eq!(
            w.queues
                .timers
                .iter()
                .map(|t| (t.id, t.ms))
                .collect::<Vec<_>>(),
            [(7, 250)],
            "{lib}"
        );
        assert_eq!(
            w.logs(),
            [
                (Level::Warn, "q\"\\\tx\ny\u{1}".to_owned()),
                (Level::Info, "done".to_owned()),
            ],
            "{lib}"
        );
        assert_eq!(w.faults(), Vec::<String>::new(), "{lib}");
    }
}

#[test]
fn tables_read_like_the_rust_sdk() {
    for lib in &libs() {
        let mut w = World::new(lib);
        w.logs();
        w.send(&[Event::Command {
            name: "conf.read",
            args: "",
        }]);
        assert_eq!(
            w.logs(),
            info(&["nets 4: a=5 b c=4 d=1 é=1", "errors 4 5 6"]),
            "{lib}"
        );
    }
}

#[test]
fn functions_answer_like_the_rust_sdk() {
    for lib in &libs() {
        let mut w = World::new(lib);
        let mut call = |fn_id, args| {
            let mut result = String::new();
            w.plugins
                .call(w.id, fn_id, args, &mut result)
                .map(|()| result)
        };
        assert_eq!(call(0, "3 4"), Ok("7".into()), "{lib}");
        assert_eq!(call(0, "-10 4"), Ok("-6".into()), "{lib}");
        assert_eq!(call(1, r#""x y""#), Ok(r#""hi x y""#.into()), "{lib}");
        assert_eq!(call(1, "z"), Ok(r#""hi z""#.into()), "{lib}");
        assert_eq!(call(0, "a b"), Err(Code::Parse), "{lib}");
        assert_eq!(call(0, "1"), Err(Code::Parse), "{lib}");
        assert_eq!(call(2, ""), Err(Code::Range), "{lib}");
    }
}
