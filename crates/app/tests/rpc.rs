//! The editor's command stream (`schemify-gui --rpc`, docs/spec/rpc.md) through the real
//! frame spine: every registered command is reachable by name, unknown names and bad params
//! are declared errors, and a client on a socket gets its answers after the frame that ran
//! its commands plus the editor's events as notifications.

mod common;

use std::io::{BufRead, BufReader, Write};

use common::Session;
use schemify_app::commands::CORE;
use schemify_app::rpc::{self, Pump};
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
        assert_eq!(rpc::dispatch(&mut s.ed, name, &none), None, "{name}");
    }
    assert_eq!(
        rpc::dispatch(&mut s.ed, "layers.set_current.3", &none),
        None
    );
    let err = |r: Option<Result<Json, (i32, String)>>| r.and_then(|r| r.err()).map(|e| e.0);
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
    assert_eq!(listed, Some(Ok(rpc::methods(&s.ed))));
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
        s.ed.session.project.doc(h).unwrap().instances().id.len()
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
        pump.answer(&s.ed);
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
