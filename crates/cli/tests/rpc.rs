//! The headless command stream end to end (`schemify rpc`, docs/spec/rpc.md): the CLI's own
//! exit codes and output are the oracle for its methods; `edit` is checked with the
//! semantic `diff`; malformed lines get declared errors and the stream goes on.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use schemify_cli::rpc::{self, Json};

const CANONICAL: &str = include_str!("../../format/tests/fixtures/ota5t.shcomp");
const MESSY: &str = include_str!("../../format/tests/fixtures/ota5t.messy.shcomp");

fn core(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_schemify"));
    c.current_dir(dir)
        .env("XDG_CONFIG_HOME", dir.join(".xdg/config"))
        .env("XDG_DATA_HOME", dir.join(".xdg/data"))
        .env_remove("SCHEMIFY_APPNAME");
    c
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("schemify-rpc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schemify.project"), "schemify 1 project\n").unwrap();
    dir
}

/// Feeds `lines` to `schemify --clean rpc` on stdin and returns every response line, parsed.
fn session(dir: &Path, lines: &[String]) -> Vec<Json> {
    let mut child = core(dir)
        .args(["--clean", "rpc"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("binary runs");
    let mut stdin = child.stdin.take().unwrap();
    for l in lines {
        writeln!(stdin, "{l}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "the stream ends cleanly at EOF");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| rpc::parse(l).expect("every response is one JSON line"))
        .collect()
}

fn request(id: u32, method: &str, params: Json) -> String {
    let mut s = String::new();
    rpc::write(
        &Json::obj(vec![
            ("jsonrpc", Json::Str("2.0".into())),
            ("id", Json::Num(f64::from(id))),
            ("method", Json::Str(method.into())),
            ("params", params),
        ]),
        &mut s,
    );
    s
}

fn strings(args: &[&str]) -> Json {
    Json::Arr(args.iter().map(|a| Json::Str((*a).into())).collect())
}

fn code_of(reply: &Json) -> Option<f64> {
    match reply.get("result")?.get("code")? {
        Json::Num(c) => Some(*c),
        _ => None,
    }
}

fn error_of(reply: &Json) -> Option<f64> {
    match reply.get("error")?.get("code")? {
        Json::Num(c) => Some(*c),
        _ => None,
    }
}

/// CLI methods answer exactly what the CLI does: same exit code and output.
#[test]
fn cli_methods_answer_what_the_cli_does() {
    let dir = scratch("cli");
    std::fs::write(dir.join("a.shcomp"), CANONICAL).unwrap();
    std::fs::write(dir.join("b.shcomp"), MESSY).unwrap();
    let direct = |args: &[&str]| {
        let out = core(&dir).arg("--clean").args(args).output().unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    let calls: [&[&str]; 3] = [
        &["fmt", "--check", "a.shcomp"],
        &["fmt", "--check", "b.shcomp"],
        &["diff", "a.shcomp", "b.shcomp"],
    ];
    let lines: Vec<String> = calls
        .iter()
        .enumerate()
        .map(|(i, c)| request(i as u32, c[0], strings(&c[1..])))
        .collect();
    let replies = session(&dir, &lines);
    assert_eq!(replies.len(), calls.len());
    for (i, c) in calls.iter().enumerate() {
        let (code, stdout) = direct(c);
        assert_eq!(replies[i].get("id"), Some(&Json::Num(i as f64)));
        assert_eq!(code_of(&replies[i]), Some(f64::from(code)), "{c:?}");
        let got = replies[i].get("result").and_then(|r| r.get("stdout"));
        assert_eq!(got.and_then(Json::as_str), Some(stdout.as_str()), "{c:?}");
    }
    assert_eq!(code_of(&replies[1]), Some(1.0), "b.shcomp is not canonical");
}

/// `edit` commits ops and saves; the semantic diff shows exactly the edited attribute. Ops
/// that do not validate change nothing and answer a design error.
#[test]
fn edit_applies_ops_and_refuses_bad_ones() {
    let dir = scratch("edit");
    std::fs::write(dir.join("a.shcomp"), CANONICAL).unwrap();
    std::fs::write(dir.join("orig.shcomp"), CANONICAL).unwrap();
    let file = dir.join("a.shcomp").to_string_lossy().into_owned();
    let edit = |ops: &str| {
        Json::obj(vec![
            ("file", Json::Str(file.clone())),
            ("ops", Json::Str(ops.into())),
            ("label", Json::Str("size".into())),
        ])
    };
    let replies = session(
        &dir,
        &[
            request(1, "edit", edit("set_attr inst 1j446r W 7")),
            request(2, "edit", edit("set_attr inst zzzzzz W 7")),
            request(
                3,
                "edit",
                Json::obj(vec![("file", Json::Str(file.clone()))]),
            ),
        ],
    );
    assert_eq!(replies.len(), 3);
    let changed = replies[0].get("result").and_then(|r| r.get("changed"));
    assert_eq!(changed, Some(&Json::Bool(true)), "{:?}", replies[0]);
    assert_eq!(error_of(&replies[1]), Some(f64::from(rpc::DESIGN_ERROR)));
    assert_eq!(error_of(&replies[2]), Some(f64::from(rpc::INVALID_PARAMS)));
    let out = core(&dir)
        .args(["--clean", "diff", "orig.shcomp", "a.shcomp"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "~ inst 1j446r [attrs: W]\n"
    );
    let text = std::fs::read_to_string(dir.join("a.shcomp")).unwrap();
    assert!(text.contains("  W 7\n"), "{text}");
}

/// Garbage and unknown methods are declared errors; the stream answers what follows.
#[test]
fn malformed_input_never_stops_the_stream() {
    let dir = scratch("garbage");
    let replies = session(
        &dir,
        &[
            "{\"jsonrpc\":\"2.0\",\"id\":1".to_owned(),
            "[1,2]".to_owned(),
            request(2, "no.such", Json::Arr(Vec::new())),
            "{\"jsonrpc\":\"2.0\",\"method\":\"rpc.methods\"}".to_owned(),
            request(3, "rpc.methods", Json::Arr(Vec::new())),
        ],
    );
    let codes: Vec<Option<f64>> = replies.iter().map(error_of).collect();
    assert_eq!(
        codes,
        [
            Some(f64::from(rpc::PARSE_ERROR)),
            Some(f64::from(rpc::INVALID_REQUEST)),
            Some(f64::from(rpc::METHOD_NOT_FOUND)),
            None
        ]
    );
    let names = replies[3].get("result").unwrap();
    let want = Json::Arr(
        rpc::METHODS
            .iter()
            .map(|m| Json::Str((*m).into()))
            .collect(),
    );
    assert_eq!(names, &want);
}

/// The same stream on a Unix-domain socket.
#[cfg(unix)]
#[test]
fn listens_on_a_unix_socket() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;
    let dir = scratch("sock");
    let sock = dir.join("s.sock");
    let mut child = core(&dir)
        .args(["--clean", "rpc", "--listen"])
        .arg(&sock)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let mut conn = None;
    for _ in 0..500 {
        if let Ok(c) = UnixStream::connect(&sock) {
            conn = Some(c);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let mut conn = conn.expect("the server listens within 5 s");
    writeln!(conn, "{}", request(7, "rpc.methods", Json::Arr(Vec::new()))).unwrap();
    let mut line = String::new();
    BufReader::new(conn.try_clone().unwrap())
        .read_line(&mut line)
        .unwrap();
    child.kill().unwrap();
    let _ = child.wait();
    let reply = rpc::parse(line.trim_end()).unwrap();
    assert_eq!(reply.get("id"), Some(&Json::Num(7.0)));
    assert!(
        matches!(reply.get("result"), Some(Json::Arr(names)) if names.len() == rpc::METHODS.len())
    );
}
