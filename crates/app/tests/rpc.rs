//! The editor's command stream (`schemify-gui --rpc`, docs/spec/rpc.md) through the real
//! frame spine: every registered command is reachable by name, unknown names and bad params
//! are declared errors, and a client on a socket gets its answers after the frame that ran
//! its commands plus the editor's events as notifications.

mod common;

use std::io::{BufRead, BufReader, Write};

use common::Session;
use schemify_app::commands::CORE;
use schemify_app::rpc::{self, Dispatched, Pump};
use schemify_cli::rpc::{INVALID_PARAMS, Json, Listener, METHOD_NOT_FOUND};
use schemify_ui::InputEvents;

/// Oracle: the command table itself. Every core and plugin command `rpc.methods` lists is
/// queued (not refused), suffix arguments included; anything else is "method not found".
#[test]
fn every_registered_command_is_reachable() {
    let mut s = Session::open("inv", "top.shcomp", "rpc-reach", true);
    let Json::Arr(names) = rpc::methods(&s.ed) else {
        panic!("rpc.methods is a list")
    };
    let names: Vec<String> = names
        .iter()
        .map(|n| n.as_str().unwrap().to_owned())
        .collect();
    assert!(CORE.iter().all(|(n, _)| names.iter().any(|m| m == n)));
    assert!(
        names.len() > CORE.len() + 1,
        "the default UI's own commands are listed too"
    );
    let none = Json::Arr(Vec::new());
    for name in names.iter().filter(|n| *n != "rpc.methods") {
        assert!(
            matches!(rpc::dispatch(&mut s.ed, name, &none), Dispatched::Queued(_)),
            "{name}"
        );
    }
    assert!(matches!(
        rpc::dispatch(&mut s.ed, "layers.set_current.3", &none),
        Dispatched::Queued(_)
    ));
    let err = |r: Dispatched| match r {
        Dispatched::Ready(Err(e)) => Some(e.0),
        _ => None,
    };
    assert_eq!(
        err(rpc::dispatch(&mut s.ed, "no.such_command", &none)),
        Some(METHOD_NOT_FOUND)
    );
    assert_eq!(
        err(rpc::dispatch(&mut s.ed, "edit.undo.x", &none)),
        Some(METHOD_NOT_FOUND)
    );
    let bad = Json::Arr(vec![Json::Num(1.0)]);
    assert_eq!(
        err(rpc::dispatch(&mut s.ed, "edit.undo", &bad)),
        Some(INVALID_PARAMS)
    );
    let listed = rpc::dispatch(&mut s.ed, "rpc.methods", &none);
    assert_eq!(listed, Dispatched::Ready(Ok(rpc::methods(&s.ed))));
}

/// A client on a loopback socket: its commands run in the next frame (select all, delete:
/// the document changes), each request gets `{"status"}` back, then the `doc_changed`
/// event arrives as a notification; a malformed line gets its error in line with the rest.
#[test]
fn a_socket_client_drives_the_editor() {
    let mut s = Session::open("inv", "top.shcomp", "rpc-sock", true);
    let listener = schemify_cli::rpc::listen("127.0.0.1:0").unwrap();
    #[allow(irrefutable_let_patterns)] // TCP is the only kind on Windows
    let Listener::Tcp(tcp) = &listener else {
        panic!("an address listens on TCP")
    };
    let addr = tcp.local_addr().unwrap();
    let mut pump = Pump::start(listener, &s.ed);
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(20)))
        .unwrap();
    let instances = |s: &Session| {
        let h = s.ed.active().unwrap().0;
        s.ed.session().project.doc(h).unwrap().instances().id.len()
    };
    assert!(instances(&s) > 0);
    for line in [
        r#"{"jsonrpc":"2.0","id":1,"method":"edit.select_all"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"edit.delete"}"#,
        "not json",
    ] {
        writeln!(client, "{line}").unwrap();
    }
    let mut reader = BufReader::new(client.try_clone().unwrap());
    let mut got: Vec<Json> = Vec::new();
    let done = |got: &[Json]| {
        got.iter().filter(|j| j.get("id").is_some()).count() == 3
            && got.iter().any(|j| {
                j.get("params").and_then(|p| p.get("kind"))
                    == Some(&Json::Str("doc_changed".into()))
            })
    };
    // A line cut by the read timeout stays in `line` until its end arrives.
    let mut line = String::new();
    for _ in 0..500 {
        pump.take(&mut s.ed);
        s.frame(InputEvents::default());
        pump.answer(&mut s.ed);
        while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
            if line.ends_with('\n') {
                got.push(schemify_cli::rpc::parse(line.trim_end()).unwrap());
                line.clear();
            }
        }
        if done(&got) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(done(&got), "{got:?}");
    assert!(
        !pump.waiting(),
        "every answer sent: the served window sleeps (D39)"
    );
    pump.take(&mut s.ed);
    assert!(!pump.waiting(), "no traffic, nothing queued");
    let answers: Vec<&Json> = got.iter().filter(|j| j.get("id").is_some()).collect();
    let ids: Vec<Option<&Json>> = answers.iter().map(|j| j.get("id")).collect();
    assert!(
        ids.contains(&Some(&Json::Null)),
        "the malformed line's error"
    );
    for id in [1.0, 2.0] {
        let a = answers
            .iter()
            .find(|j| j.get("id") == Some(&Json::Num(id)))
            .unwrap();
        assert!(
            matches!(
                a.get("result").and_then(|r| r.get("status")),
                Some(Json::Str(_))
            ),
            "{a:?}"
        );
    }
    assert_eq!(instances(&s), 0, "select all, then delete");
}

/// A loopback client of `s`'s editor, one request at a time: each call pumps frames until
/// its answer arrives (at most 500 frames).
struct Client {
    pump: Pump,
    out: std::net::TcpStream,
    reader: BufReader<std::net::TcpStream>,
    next: f64,
}

impl Client {
    fn connect(s: &Session) -> Client {
        let listener = schemify_cli::rpc::listen("127.0.0.1:0").unwrap();
        #[allow(irrefutable_let_patterns)] // TCP is the only kind on Windows
        let Listener::Tcp(tcp) = &listener else {
            panic!("an address listens on TCP")
        };
        let addr = tcp.local_addr().unwrap();
        let pump = Pump::start(listener, &s.ed);
        let out = std::net::TcpStream::connect(addr).unwrap();
        out.set_read_timeout(Some(std::time::Duration::from_millis(20)))
            .unwrap();
        let reader = BufReader::new(out.try_clone().unwrap());
        Client {
            pump,
            out,
            reader,
            next: 1.0,
        }
    }

    /// The `status` of the answer to `method` with string `params`.
    fn status(&mut self, s: &mut Session, method: &str, params: &[&str]) -> String {
        let id = self.next;
        self.next += 1.0;
        let params = Json::Arr(params.iter().map(|p| Json::Str((*p).into())).collect());
        let req = Json::obj(vec![
            ("jsonrpc", Json::Str("2.0".into())),
            ("id", Json::Num(id)),
            ("method", Json::Str(method.into())),
            ("params", params),
        ]);
        let mut text = String::new();
        schemify_cli::rpc::write(&req, &mut text);
        writeln!(self.out, "{text}").unwrap();
        let mut line = String::new();
        for _ in 0..500 {
            self.pump.take(&mut s.ed);
            s.frame(InputEvents::default());
            self.pump.answer(&mut s.ed);
            while self.reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                if !line.ends_with('\n') {
                    continue;
                }
                let j = schemify_cli::rpc::parse(line.trim_end()).unwrap();
                line.clear();
                if j.get("id") == Some(&Json::Num(id)) {
                    let r = j.get("result").and_then(|r| r.get("status"));
                    let Some(status) = r.and_then(Json::as_str) else {
                        panic!("{method}: {j:?} (status line: {})", s.ed.session().status)
                    };
                    return status.to_owned();
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!(
            "no answer to {method} (status line: {})",
            s.ed.session().status
        );
    }
}

/// Oracle: the status line the same command leaves when typed (rpc spec §3.2). Each answer
/// carries its own command's message, not the one before it, and a command that says
/// nothing answers an empty status rather than the previous command's.
#[test]
fn each_answer_carries_its_own_status() {
    let mut s = Session::open("inv", "top.shcomp", "rpc-status", true);
    let mut c = Client::connect(&s);
    assert_eq!(
        c.status(&mut s, "edit.pop", &[]),
        "already at the top level"
    );
    // Says nothing: an empty answer, while the status line keeps the previous message.
    assert_eq!(
        c.status(&mut s, "view.zoom_full", &[]),
        "",
        "lags one command"
    );
    assert_eq!(s.ed.session().status, "already at the top level");
    assert_eq!(
        c.status(&mut s, "edit.pop", &[]),
        "already at the top level"
    );
}

/// Opens the `inv` fixture with the `bad_actor` test guest loaded from its manifest at
/// `fuel` per event (above the default: its events run on a worker).
fn with_bad_actor(tag: &str, fuel: u64) -> Session {
    let dir = common::fixture("inv", tag);
    let wasm = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../plugin/tests/fixtures/bad_actor.wasm");
    let manifest = dir.join("schemify.project");
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    text.push_str(&format!(
        "\nplugin bad path {}\n  grant read_project\n  fuel {fuel}\n",
        // Quoted with the format's escapes: a Windows path holds `\`.
        schemify_app::docs::quote(&wasm.display().to_string())
    ));
    std::fs::write(&manifest, text).unwrap();
    let ed = schemify_app::editor::Editor::new(schemify_app::editor::Options {
        path: Some(dir.join("top.shcomp")),
        seed: 7,
        ..Default::default()
    })
    .expect("fixture opens");
    let mut s = Session { ed, now: 1, dir };
    s.frame(InputEvents::default());
    s.frame(InputEvents::default());
    s
}

/// Oracle: plugin-abi §2 (an exhausted budget aborts the event and is reported). A plugin
/// command that runs out of fuel answers, and leaves in the status line, the plugin, the
/// event and the budget; on a worker (fuel above the default) the answer waits for the
/// event instead of carrying the status from before it.
#[test]
fn out_of_fuel_is_reported_not_silent() {
    for (tag, fuel) in [("rpc-fuel", 1_000_000), ("rpc-fuel-worker", 100_000_001)] {
        let mut s = with_bad_actor(tag, fuel);
        let mut c = Client::connect(&s);
        assert_eq!(
            c.status(&mut s, "edit.pop", &[]),
            "already at the top level"
        );
        let want = format!("bad: command bad.spin: out of fuel (budget {fuel}), outputs discarded");
        assert_eq!(c.status(&mut s, "bad.spin", &[]), want, "{tag}");
        assert_eq!(s.ed.session().status, want, "{tag}: the status line");
        let faults = s.ed.host().plugins.health().faults();
        assert_eq!(
            faults.back().map(|f| f.detail.as_str()),
            Some(want.as_str())
        );
        // The next command answers its own message, not the fault.
        assert_eq!(
            c.status(&mut s, "edit.pop", &[]),
            "already at the top level"
        );
    }
}

/// `file.open <cell>` (rpc spec §3.2, the case a script needs): with an argument it opens
/// that cell (`lib:cell`, a project cell's stem, or a project-relative path) without the
/// chooser; a name that is no cell is an error in the status line and changes nothing;
/// without an argument it opens the chooser as before.
#[test]
fn file_open_with_an_argument_opens_that_cell() {
    let mut s = Session::open("inv", "top.shcomp", "rpc-open", true);
    let mut c = Client::connect(&s);
    let active = |s: &Session| s.ed.session().name(s.ed.active().unwrap().0);
    assert_eq!(active(&s), "top.shcomp");
    for (arg, want) in [
        ("inv", "inv.shcomp"),
        ("dev:nmos", "nmos.shprim"),
        ("top.shcomp", "top.shcomp"),
        ("dev/pmos.shprim", "pmos.shprim"),
    ] {
        let said = c.status(&mut s, "file.open", &[arg]);
        assert_eq!(active(&s), want, "{arg}: {said}");
        assert_eq!(said, format!("opened {want}"), "{arg}");
        assert!(s.ed.shell().ui().dialog().is_none(), "{arg}: no chooser");
    }
    for bad in ["nope", "dev:nope", "../outside.shcomp"] {
        let said = c.status(&mut s, "file.open", &[bad]);
        assert!(
            said.starts_with(&format!("file.open {bad}: ")),
            "{bad}: {said}"
        );
        assert_eq!(active(&s), "pmos.shprim", "{bad}: nothing opened");
        assert!(s.ed.shell().ui().dialog().is_none(), "{bad}: no chooser");
    }
    c.status(&mut s, "file.open", &[]);
    assert!(
        !s.ed.shell().ui().dialog().is_none(),
        "no argument: the chooser"
    );
}
