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
//!   Stream, listen}`, `wire::{LINE_MAX, the error codes, Request, MethodError, Frame, decode,
//!   result, error, notification, read_frame}`
//! - `pub const METHODS: [&str; 7]`
//! - `pub struct Headless { exe, flags, seed }`, `Headless::call`
//! - `pub fn serve<R: BufRead, W: Write>(&mut R, &mut W, &Headless) -> io::Result<()>`
//! - `pub(crate) fn serve_with(&mut R, &mut W, impl FnMut(&str, &Json) -> Result<Json,
//!   MethodError>) -> io::Result<()>` (the loop [`serve`] and `mcp::serve` share)

mod json;
mod socket;
mod wire;

pub use json::{DEPTH_MAX, Json, JsonError, parse, write};
pub use socket::{Listener, Stream, listen};
pub use wire::{
    DESIGN_ERROR, Frame, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, IO_ERROR, LINE_MAX,
    METHOD_NOT_FOUND, MethodError, PARSE_ERROR, Request, decode, error, notification, read_frame,
    result,
};

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::platform;
use crate::spines::{self, OpenError};

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
#[derive(Clone, Debug, Default)]
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
/// request, an `Err` becoming its error response. Same framing and errors as [`serve`].
pub(crate) fn serve_with<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    mut call: impl FnMut(&str, &Json) -> Result<Json, MethodError>,
) -> io::Result<()> {
    let mut buf = Vec::new();
    // Bounded by the input: every pass consumes a line.
    loop {
        assert!(buf.len() <= LINE_MAX);
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
                        Err(e) => error(&id, e.code, &e.message),
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
    /// Runs one method. A CLI method blocks until its child exits.
    ///
    /// # Errors
    ///
    /// The error response's code and message: [`METHOD_NOT_FOUND`] for a name outside
    /// [`METHODS`], [`INVALID_PARAMS`] for params that do not fit, [`INTERNAL_ERROR`] when a
    /// CLI method has no binary to run or its child cannot start, and `edit`'s
    /// [`IO_ERROR`] or [`DESIGN_ERROR`].
    ///
    /// # Panics
    ///
    /// Panics if a method of [`METHODS`] has no route (a bug here).
    pub fn call(&self, method: &str, params: &Json) -> Result<Json, MethodError> {
        let answer = match method {
            "rpc.methods" => Ok(Json::Arr(
                METHODS.iter().map(|m| Json::Str((*m).to_owned())).collect(),
            )),
            "edit" => edit(params, self.seed),
            m if CLI_METHODS.contains(&m) => self.cli(m, params),
            _ => Err(MethodError::new(
                METHOD_NOT_FOUND,
                format!("unknown method `{method}`"),
            )),
        };
        if let Err(e) = &answer
            && e.code == METHOD_NOT_FOUND
        {
            assert!(
                !METHODS.contains(&method),
                "{method}: listed but not routed"
            );
        }
        answer
    }
}

// ---- private ----

impl Headless {
    /// A CLI command as a child process: `{"code", "stdout", "stderr"}`. Its stdin is empty,
    /// so it never reads the request stream.
    fn cli(&self, method: &str, params: &Json) -> Result<Json, MethodError> {
        assert!(CLI_METHODS.contains(&method));
        assert!(METHODS.contains(&method));
        let args: Option<Vec<&str>> = match params {
            Json::Arr(items) => items.iter().map(Json::as_str).collect(),
            _ => None,
        };
        let Some(args) = args else {
            return Err(MethodError::new(
                INVALID_PARAMS,
                "params: an array of argument strings".to_owned(),
            ));
        };
        let Some(exe) = &self.exe else {
            return Err(MethodError::new(
                INTERNAL_ERROR,
                "no core binary to run CLI methods".to_owned(),
            ));
        };
        let out = Command::new(exe)
            .args(&self.flags)
            .arg(method)
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| MethodError::new(INTERNAL_ERROR, format!("{method}: {e}")))?;
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
fn edit(params: &Json, seed: u64) -> Result<Json, MethodError> {
    use schemify_doc::{Document, Edit, EditRequest, Mode, Outcome, Transaction};
    let file = params.get("file").and_then(Json::as_str);
    let ops = params.get("ops").and_then(Json::as_str);
    let (Some(file), Some(ops)) = (file, ops) else {
        return Err(MethodError::new(
            INVALID_PARAMS,
            "params: {\"file\", \"ops\", \"label\"?}".to_owned(),
        ));
    };
    let label = params.get("label").and_then(Json::as_str).unwrap_or("rpc");
    let mut o = spines::open_file(Path::new(file), seed).map_err(|e| match e {
        OpenError::Io(m) => MethodError::new(IO_ERROR, format!("{file}: {m}")),
        OpenError::Design(m) => MethodError::new(DESIGN_ERROR, format!("{file}: {m}")),
    })?;
    let kind = o
        .project
        .doc(o.top)
        .map(Document::kind)
        .expect("open_file opened it");
    let ops = schemify_doc::ops_from_text(ops, kind)
        .map_err(|errs| MethodError::new(DESIGN_ERROR, format!("ops: {errs:?}")))?;
    let req = EditRequest {
        doc: o.top,
        edit: Edit::Tx(Transaction {
            label: label.to_owned(),
            mode: Mode::Commit,
            ops,
        }),
        unix_ms: platform::unix_ms(),
    };
    let outcome = schemify_doc::apply_edits(&mut o.project, &[req]);
    assert_eq!(outcome.len(), 1, "one outcome per request");
    if !matches!(outcome[0], Outcome::Committed { .. }) {
        return Err(MethodError::new(DESIGN_ERROR, format!("{:?}", outcome[0])));
    }
    let save = o
        .project
        .save(o.top)
        .map_err(|c| MethodError::new(DESIGN_ERROR, format!("save: {c:?}")))?;
    let changed = save.is_some();
    if let Some(save) = save {
        assert!(!save.bytes.is_empty(), "a saved document has a header");
        platform::write_atomic(Path::new(file), save.bytes.as_bytes())
            .map_err(|e| MethodError::new(IO_ERROR, e))?;
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
            assert!(
                !matches!(&got, Err(e) if e.code == METHOD_NOT_FOUND),
                "{m}: {got:?}"
            );
        }
        assert_eq!(
            h.call("nope", &Json::Null).unwrap_err().code,
            METHOD_NOT_FOUND
        );
        assert_eq!(
            h.call("fmt", &Json::Arr(vec![Json::Null]))
                .unwrap_err()
                .code,
            INVALID_PARAMS
        );
        assert_eq!(
            h.call("edit", &Json::obj(vec![])).unwrap_err().code,
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
