//! The D27 command stream: request lines → method calls → response lines, JSON-RPC 2.0 with
//! one JSON text per line on stdio or a local socket (normative spec: `docs/spec/rpc.md`).
//! The codec (`json`), the framing and responses (`wire`) and the sockets (`socket`)
//! are shared with the editor's server (`schemify-gui --rpc`, `schemify_app::serve`); this
//! file is the headless server `schemify rpc` runs: [`serve`] over [`Headless`].
//!
//! Methods are a closed table ([`METHODS`]); the CLI ones run the `schemify` binary itself as
//! a child, so a failing spine can neither hang nor kill the server (D27).
//!
//! ```
//! use schemify_cli::rpc::{Headless, serve};
//!
//! let h = Headless { exe: None, flags: Vec::new(), seed: 1 };
//! let mut input = &b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"rpc.methods\"}\n"[..];
//! let mut output = Vec::new();
//! serve(&mut input, &mut output, &h).unwrap();
//! assert!(String::from_utf8(output).unwrap().starts_with("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":["));
//! ```
//!
//! Contents:
//! - re-exported: `json::{DEPTH_MAX, Json, JsonError, parse, write}`, `socket::{Listener,
//!   Stream, listen}`, `wire::{LINE_MAX, the error codes, Request, Frame, decode, result,
//!   error, notification, read_frame}`
//! - `pub const METHODS: [&str; 7]`
//! - `pub struct Headless { exe, flags, seed }`, `Headless::call`
//! - `pub fn serve<R: BufRead, W: Write>(&mut R, &mut W, &Headless) -> io::Result<()>`
//! - `pub(crate) fn serve_with(&mut R, &mut W, impl FnMut(&str, &Json) -> Result<Json, (i32,
//!   String)>) -> io::Result<()>` (the loop [`serve`] and `mcp::serve` share)

mod json;
mod socket;
mod wire;

pub use json::{DEPTH_MAX, Json, JsonError, parse, write};
pub use socket::{Listener, Stream, listen};
pub use wire::{
    DESIGN_ERROR, Frame, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, IO_ERROR, LINE_MAX,
    METHOD_NOT_FOUND, PARSE_ERROR, Request, decode, error, notification, read_frame, result,
};

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

/// Every method the headless server answers, sorted.
pub const METHODS: [&str; 7] = [
    "check",
    "diff",
    "edit",
    "fmt",
    "netlist",
    "rpc.methods",
    "run",
];

/// What the headless methods need: the `schemify` binary that runs the CLI methods (`None`:
/// those methods answer [`INTERNAL_ERROR`]), the startup flags it gets (`--clean`, `-u`),
/// and the id seed for `edit`.
#[derive(Clone, Debug)]
pub struct Headless {
    /// The core binary (`std::env::current_exe`).
    pub exe: Option<PathBuf>,
    /// Startup flags passed before the command.
    pub flags: Vec<String>,
    /// Id-generation seed for documents `edit` opens.
    pub seed: u64,
}

/// The CLI commands the headless server runs as methods (params: their argv, as strings).
const CLI_METHODS: [&str; 5] = ["check", "diff", "fmt", "netlist", "run"];

/// Serves requests from `input` until it ends: one response line per request (none for a
/// notification), in request order, flushed after each. Blank lines are skipped; a malformed
/// line gets its error response and the stream continues.
///
/// # Errors
///
/// I/O errors of `input` or `output` only; every request-level failure is a response.
pub fn serve<R: BufRead, W: Write>(input: &mut R, output: &mut W, h: &Headless) -> io::Result<()> {
    serve_with(input, output, |method, params| h.call(method, params))
}

/// The stream loop behind [`serve`] and [`crate::mcp::serve`]: `call` answers one decoded
/// request, `Err((code, message))` becoming its error response. Same framing and errors as
/// [`serve`].
pub(crate) fn serve_with<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    mut call: impl FnMut(&str, &Json) -> Result<Json, (i32, String)>,
) -> io::Result<()> {
    let mut buf = Vec::new();
    // Bounded by the input: every pass consumes a line.
    loop {
        let reply = match read_frame(input, &mut buf)? {
            Frame::End => return Ok(()),
            Frame::TooLong => Some(error(&Json::Null, PARSE_ERROR, "line over 16 MiB")),
            Frame::Line if buf.iter().all(u8::is_ascii_whitespace) => None,
            Frame::Line => match decode(&buf) {
                Err(reply) => Some(reply),
                Ok(req) => {
                    let out = call(&req.method, &req.params);
                    req.id.map(|id| match out {
                        Ok(v) => result(&id, &v),
                        Err((code, msg)) => error(&id, code, &msg),
                    })
                }
            },
        };
        if let Some(reply) = reply {
            // `wire::frame` asserted the reply is one line.
            assert!(reply.ends_with('\n'));
            output.write_all(reply.as_bytes())?;
            output.flush()?;
        }
    }
}

impl Headless {
    /// Runs one method. `Err` is `(code, message)` for the error response: [`METHOD_NOT_FOUND`]
    /// for a name outside [`METHODS`], [`INVALID_PARAMS`] for params that do not fit, the
    /// method's own codes otherwise. A CLI method blocks until its child exits.
    pub fn call(&self, method: &str, params: &Json) -> Result<Json, (i32, String)> {
        match method {
            "rpc.methods" => Ok(Json::Arr(
                METHODS.iter().map(|m| Json::Str((*m).to_owned())).collect(),
            )),
            "edit" => edit(params, self.seed),
            m if CLI_METHODS.contains(&m) => self.cli(m, params),
            _ => Err((METHOD_NOT_FOUND, format!("unknown method `{method}`"))),
        }
    }
}

// ---- private ----

impl Headless {
    /// A CLI command as a child process: `{"code", "stdout", "stderr"}`. Its stdin is empty,
    /// so it never reads the request stream.
    fn cli(&self, method: &str, params: &Json) -> Result<Json, (i32, String)> {
        assert!(CLI_METHODS.contains(&method));
        let args: Option<Vec<&str>> = match params {
            Json::Arr(items) => items.iter().map(Json::as_str).collect(),
            _ => None,
        };
        let Some(args) = args else {
            return Err((
                INVALID_PARAMS,
                "params: an array of argument strings".into(),
            ));
        };
        let Some(exe) = &self.exe else {
            return Err((INTERNAL_ERROR, "no core binary to run CLI methods".into()));
        };
        let out = std::process::Command::new(exe)
            .args(&self.flags)
            .arg(method)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| (INTERNAL_ERROR, format!("{method}: {e}")))?;
        let text = |b: &[u8]| Json::Str(String::from_utf8_lossy(b).into_owned());
        Ok(Json::obj(vec![
            (
                "code",
                Json::Num(f64::from(out.status.code().unwrap_or(-1))),
            ),
            ("stdout", text(&out.stdout)),
            ("stderr", text(&out.stderr)),
        ]))
    }
}

/// `edit {file, ops, label?}`: opens `file` in its project, applies `ops` (edits spec §1 op
/// text) as one committed transaction and writes the canonical result atomically. Result:
/// `{"changed": bool}` (false when the ops left the file as it was).
fn edit(params: &Json, seed: u64) -> Result<Json, (i32, String)> {
    use schemify_doc::{Document, Edit, EditRequest, Mode, Outcome, Transaction, apply_edits};
    let file = params.get("file").and_then(Json::as_str);
    let ops = params.get("ops").and_then(Json::as_str);
    let (Some(file), Some(ops)) = (file, ops) else {
        return Err((
            INVALID_PARAMS,
            "params: {\"file\", \"ops\", \"label\"?}".into(),
        ));
    };
    let label = params.get("label").and_then(Json::as_str).unwrap_or("rpc");
    let (mut o, _notes) = crate::spines::open_file(Path::new(file), seed).map_err(|e| match e {
        crate::spines::OpenError::Io(m) => (IO_ERROR, format!("{file}: {m}")),
        crate::spines::OpenError::Design(m) => (DESIGN_ERROR, format!("{file}: {m}")),
    })?;
    let kind = o
        .project
        .doc(o.top)
        .map(Document::kind)
        .expect("open_file opened it");
    let ops = schemify_doc::ops_from_text(ops, kind)
        .map_err(|errs| (DESIGN_ERROR, format!("ops: {errs:?}")))?;
    let req = EditRequest {
        doc: o.top,
        edit: Edit::Tx(Transaction {
            label: label.to_owned(),
            mode: Mode::Commit,
            ops,
        }),
        unix_ms: crate::platform::unix_ms(),
    };
    let outcome = apply_edits(&mut o.project, &[req]);
    assert_eq!(outcome.len(), 1, "one outcome per request");
    if !matches!(outcome[0], Outcome::Committed { .. }) {
        return Err((DESIGN_ERROR, format!("{:?}", outcome[0])));
    }
    let save = o
        .project
        .save(o.top)
        .map_err(|c| (DESIGN_ERROR, format!("save: {c:?}")))?;
    let changed = save.is_some();
    if let Some(save) = save {
        crate::platform::write_atomic(Path::new(file), save.bytes.as_bytes())
            .map_err(|e| (IO_ERROR, e))?;
    }
    Ok(Json::obj(vec![("changed", Json::Bool(changed))]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headless() -> Headless {
        Headless {
            exe: None,
            flags: Vec::new(),
            seed: 1,
        }
    }

    /// Every method is reachable (none answers "method not found"); unknown names do.
    #[test]
    fn every_method_is_reachable() {
        let h = headless();
        let mut sorted = METHODS;
        sorted.sort_unstable();
        assert_eq!(sorted, METHODS);
        assert!(CLI_METHODS.iter().all(|m| METHODS.contains(m)));
        for m in METHODS {
            let got = h.call(m, &Json::Arr(Vec::new()));
            assert!(!matches!(got, Err((METHOD_NOT_FOUND, _))), "{m}: {got:?}");
        }
        assert_eq!(h.call("nope", &Json::Null).unwrap_err().0, METHOD_NOT_FOUND);
        assert_eq!(
            h.call("fmt", &Json::Arr(vec![Json::Null])).unwrap_err().0,
            INVALID_PARAMS
        );
        assert_eq!(
            h.call("edit", &Json::obj(vec![])).unwrap_err().0,
            INVALID_PARAMS
        );
    }

    /// The stream answers in order, skips notifications and blank lines, and survives garbage.
    #[test]
    fn serve_answers_each_request_in_order() {
        let h = headless();
        let script = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"rpc.methods\"}\n",
            "\n",
            "not json\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"rpc.methods\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"nope\"}\n",
        );
        let mut out = Vec::new();
        serve(&mut script.as_bytes(), &mut out, &h).unwrap();
        let lines: Vec<Json> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| parse(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].get("id"), Some(&Json::Num(1.0)));
        let text = |v: &Json| {
            let mut s = String::new();
            write(v, &mut s);
            s
        };
        assert_eq!(
            lines[0].get("result").map(text),
            Some(text(&h.call("rpc.methods", &Json::Null).unwrap()))
        );
        assert_eq!(lines[1].get("id"), Some(&Json::Null));
        let code = |l: &Json| l.get("error").and_then(|e| e.get("code")).cloned();
        assert_eq!(code(&lines[1]), Some(Json::Num(f64::from(PARSE_ERROR))));
        assert_eq!(
            code(&lines[2]),
            Some(Json::Num(f64::from(METHOD_NOT_FOUND)))
        );
    }
}
