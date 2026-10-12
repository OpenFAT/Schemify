//! The D27 command stream: JSON-RPC 2.0, one JSON text per line, on stdio or a local socket
//! (normative spec: `docs/spec/rpc.md`). This module holds what both servers share: the JSON
//! codec, the line framing, the request decoder, the response encoders and the socket
//! boundary; plus the headless server `schemify rpc` runs ([`serve`] over [`Headless`]). The
//! editor's server (`schemify-gui --rpc`) lives in `schemify_app::rpc` and reuses all of it.
//!
//! Data design: one request line at a time, at most [`LINE_MAX`] bytes, parsed into a [`Json`]
//! tree at most [`DEPTH_MAX`] deep (the only allocation that scales with input). Methods are a
//! closed table ([`METHODS`]); the CLI ones run the `schemify` binary itself as a child, so a
//! failing spine can neither hang nor kill the server (D27).
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

use std::fmt::Write as _;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

/// Longest request line in bytes (16 MiB: an `edit` carries whole op texts); a longer one is
/// consumed and answered with [`PARSE_ERROR`].
pub const LINE_MAX: usize = 16 << 20;
/// Deepest nesting of arrays and objects a request may have.
pub const DEPTH_MAX: u32 = 64;

/// JSON-RPC: the line is not UTF-8 JSON.
pub const PARSE_ERROR: i32 = -32700;
/// JSON-RPC: the JSON is not a request object.
pub const INVALID_REQUEST: i32 = -32600;
/// JSON-RPC: no such method (command).
pub const METHOD_NOT_FOUND: i32 = -32601;
/// JSON-RPC: the params do not fit the method.
pub const INVALID_PARAMS: i32 = -32602;
/// JSON-RPC: the server could not run the method (no process support, a worker died).
pub const INTERNAL_ERROR: i32 = -32603;
/// The method ran and the design was refused (the CLI's exit code 1: a document that does
/// not parse, ops that do not validate, kind rules).
pub const DESIGN_ERROR: i32 = 1;
/// The method could not read or write a file (the CLI's exit code 2).
pub const IO_ERROR: i32 = 2;

/// A JSON value. Objects keep their member order (and duplicate keys; [`Json::get`] finds
/// the first), so encoding a decoded value reproduces it.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// A number; always finite (the decoder refuses `1e999`).
    Num(f64),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Json>),
    /// An object, members in text order.
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Returns the first member named `key` of an object; `None` for anything else.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(members) => members.iter().find(|m| m.0 == key).map(|m| &m.1),
            _ => None,
        }
    }

    /// Returns the text of a string value.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Builds an object from `(key, value)` pairs, in order.
    pub fn obj(members: Vec<(&str, Json)>) -> Json {
        Json::Obj(
            members
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        )
    }
}

/// Why a line is not JSON: the byte offset and what was expected there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonError {
    /// Byte offset in the line.
    pub at: usize,
    /// What went wrong.
    pub what: &'static str,
}

/// Parses one JSON text (RFC 8259; surrounding whitespace allowed).
///
/// # Errors
///
/// The first syntax error, nesting deeper than [`DEPTH_MAX`], a number outside the finite
/// `f64` range, a lone surrogate escape, or trailing characters. Never panics.
pub fn parse(text: &str) -> Result<Json, JsonError> {
    let mut p = Parser {
        b: text.as_bytes(),
        i: 0,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(p.err("trailing characters"));
    }
    assert!(p.i == text.len(), "the whole text was read");
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, what: &'static str) -> JsonError {
        JsonError { at: self.i, what }
    }

    fn ws(&mut self) {
        while self
            .b
            .get(self.i)
            .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        let hit = self.b.get(self.i) == Some(&c);
        self.i += usize::from(hit);
        hit
    }

    fn digits(&mut self) -> usize {
        let start = self.i;
        while self.b.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        self.i - start
    }

    fn literal(&mut self, word: &str, v: Json) -> Result<Json, JsonError> {
        if !self.b[self.i..].starts_with(word.as_bytes()) {
            return Err(self.err("unknown literal"));
        }
        self.i += word.len();
        Ok(v)
    }

    fn value(&mut self, depth: u32) -> Result<Json, JsonError> {
        if depth >= DEPTH_MAX {
            return Err(self.err("nested too deep"));
        }
        match self.b.get(self.i) {
            Some(b'n') => self.literal("null", Json::Null),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::Str),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.eat(b']') {
                    return Ok(Json::Arr(items));
                }
                // Each pass consumes at least one byte or returns: bounded by the line.
                loop {
                    self.ws();
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat(b']') {
                        return Ok(Json::Arr(items));
                    }
                    if !self.eat(b',') {
                        return Err(self.err("expected `,` or `]`"));
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut members = Vec::new();
                self.ws();
                if self.eat(b'}') {
                    return Ok(Json::Obj(members));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return Err(self.err("expected a member name"));
                    }
                    let key = self.string()?;
                    self.ws();
                    if !self.eat(b':') {
                        return Err(self.err("expected `:`"));
                    }
                    self.ws();
                    members.push((key, self.value(depth + 1)?));
                    self.ws();
                    if self.eat(b'}') {
                        return Ok(Json::Obj(members));
                    }
                    if !self.eat(b',') {
                        return Err(self.err("expected `,` or `}`"));
                    }
                }
            }
            _ => Err(self.err("expected a value")),
        }
    }

    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.i;
        self.eat(b'-');
        match self.b.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.err("expected a digit")),
        }
        if self.eat(b'.') && self.digits() == 0 {
            return Err(self.err("expected a digit after `.`"));
        }
        if self.eat(b'e') || self.eat(b'E') {
            if !self.eat(b'+') {
                self.eat(b'-');
            }
            if self.digits() == 0 {
                return Err(self.err("expected an exponent"));
            }
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).expect("ASCII digits and signs");
        match text.parse::<f64>() {
            Ok(x) if x.is_finite() => Ok(Json::Num(x)),
            _ => Err(JsonError {
                at: start,
                what: "number out of range",
            }),
        }
    }

    fn string(&mut self) -> Result<String, JsonError> {
        assert_eq!(self.b[self.i], b'"');
        self.i += 1;
        let mut out = String::new();
        loop {
            let run = self.i;
            while self
                .b
                .get(self.i)
                .is_some_and(|&c| c != b'"' && c != b'\\' && c >= 0x20)
            {
                self.i += 1;
            }
            // Stops only at ASCII bytes, so the run ends on a character boundary.
            out.push_str(std::str::from_utf8(&self.b[run..self.i]).expect("input is UTF-8"));
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = match self.b.get(self.i) {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        Some(b'u') => {
                            self.i += 1;
                            out.push(self.unicode()?);
                            continue;
                        }
                        _ => return Err(self.err("unknown escape")),
                    };
                    self.i += 1;
                    out.push(c);
                }
                Some(_) => return Err(self.err("control character in a string")),
                None => return Err(self.err("unterminated string")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let b = self.b;
        let Some(h) = b
            .get(self.i..self.i + 4)
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
        else {
            return Err(self.err("`\\u` needs four hex digits"));
        };
        self.i += 4;
        let text = std::str::from_utf8(h).expect("hex digits are ASCII");
        Ok(u32::from_str_radix(text, 16).expect("four hex digits"))
    }

    fn unicode(&mut self) -> Result<char, JsonError> {
        let hi = self.hex4()?;
        let code = if (0xD800..0xDC00).contains(&hi) {
            if !(self.eat(b'\\') && self.eat(b'u')) {
                return Err(self.err("lone surrogate"));
            }
            let lo = self.hex4()?;
            if !(0xDC00..0xE000).contains(&lo) {
                return Err(self.err("lone surrogate"));
            }
            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
        } else {
            hi
        };
        char::from_u32(code).ok_or(self.err("lone surrogate"))
    }
}

/// Appends `v` as compact JSON (no whitespace, so never a line break: one value is one
/// frame). Integral numbers below 2^53 are written without a fraction; other numbers in
/// Rust's shortest round-trip form; a non-finite number (never decoded) as `null`.
pub fn write(v: &Json, out: &mut String) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(x) if !x.is_finite() => out.push_str("null"),
        Json::Num(x) if x.fract() == 0.0 && x.abs() < 9_007_199_254_740_992.0 => {
            write!(out, "{}", *x as i64).expect("writing to a String");
        }
        Json::Num(x) => write!(out, "{x:?}").expect("writing to a String"),
        Json::Str(s) => write_str(s, out),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out);
            }
            out.push(']');
        }
        Json::Obj(members) => {
            out.push('{');
            for (i, (k, item)) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(k, out);
                out.push(':');
                write(item, out);
            }
            out.push('}');
        }
    }
}

fn write_str(s: &str, out: &mut String) {
    let before = out.len();
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                write!(out, "\\u{:04x}", c as u32).expect("writing to a String");
            }
            c => out.push(c),
        }
    }
    out.push('"');
    debug_assert!(!out[before..].contains(['\n', '\r']));
}

/// One decoded request. `id` is `None` for a notification: the method runs, nothing is
/// answered.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// The request id (number, string or null), echoed in the response.
    pub id: Option<Json>,
    /// The method: a command name (`docs/spec/rpc.md` §3).
    pub method: String,
    /// The params: an array or an object; an empty array when absent.
    pub params: Json,
}

/// Decodes one request line.
///
/// # Errors
///
/// The response line to send instead (with its `\n`): [`PARSE_ERROR`] for bytes that are not
/// UTF-8 JSON, [`INVALID_REQUEST`] for JSON that is not a JSON-RPC 2.0 request object (a
/// batch array included). Its id is the request's when one could be read, else `null`.
/// Never panics.
pub fn decode(line: &[u8]) -> Result<Request, String> {
    let Ok(text) = std::str::from_utf8(line) else {
        return Err(error(&Json::Null, PARSE_ERROR, "the line is not UTF-8"));
    };
    let v = parse(text).map_err(|e| {
        let msg = format!("{} at byte {}", e.what, e.at);
        error(&Json::Null, PARSE_ERROR, &msg)
    })?;
    let id = v.get("id").cloned();
    let id_ok = matches!(id, None | Some(Json::Null | Json::Num(_) | Json::Str(_)));
    let reply_id = id.clone().filter(|_| id_ok).unwrap_or(Json::Null);
    let invalid = |msg: &str| Err(error(&reply_id, INVALID_REQUEST, msg));
    if !matches!(v, Json::Obj(_)) {
        return invalid("a request is one JSON object (no batches)");
    }
    if v.get("jsonrpc").and_then(Json::as_str) != Some("2.0") || !id_ok {
        return invalid("needs \"jsonrpc\": \"2.0\" and an id that is a number, string or null");
    }
    let Some(method) = v.get("method").and_then(Json::as_str) else {
        return invalid("\"method\" must be a string");
    };
    let params = v.get("params").cloned().unwrap_or(Json::Arr(Vec::new()));
    if !matches!(params, Json::Arr(_) | Json::Obj(_)) {
        return invalid("\"params\" must be an array or an object");
    }
    Ok(Request {
        id,
        method: method.to_owned(),
        params,
    })
}

/// The response line (with its `\n`) for a successful call.
pub fn result(id: &Json, result: &Json) -> String {
    frame(Json::obj(vec![
        ("jsonrpc", Json::Str("2.0".into())),
        ("id", id.clone()),
        ("result", result.clone()),
    ]))
}

/// The response line (with its `\n`) for a failed call.
pub fn error(id: &Json, code: i32, message: &str) -> String {
    let err = Json::obj(vec![
        ("code", Json::Num(f64::from(code))),
        ("message", Json::Str(message.to_owned())),
    ]);
    frame(Json::obj(vec![
        ("jsonrpc", Json::Str("2.0".into())),
        ("id", id.clone()),
        ("error", err),
    ]))
}

/// A notification line (with its `\n`) from the server: `method` with `params`, no id.
pub fn notification(method: &str, params: &Json) -> String {
    frame(Json::obj(vec![
        ("jsonrpc", Json::Str("2.0".into())),
        ("method", Json::Str(method.to_owned())),
        ("params", params.clone()),
    ]))
}

fn frame(v: Json) -> String {
    let mut out = String::new();
    write(&v, &mut out);
    out.push('\n');
    out
}

/// What [`read_frame`] read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// A line is in the buffer (without its `\n` or a `\r` before it).
    Line,
    /// A line longer than [`LINE_MAX`] was consumed; the buffer is empty.
    TooLong,
    /// The input ended (no partial line left).
    End,
}

/// Reads the next `\n`-terminated line into `buf` (cleared first). A final line without `\n`
/// counts. Memory is bounded by [`LINE_MAX`] whatever the input.
///
/// # Errors
///
/// The reader's I/O errors.
pub fn read_frame<R: BufRead>(input: &mut R, buf: &mut Vec<u8>) -> io::Result<Frame> {
    buf.clear();
    let (mut any, mut too_long) = (false, false);
    loop {
        let chunk = input.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        any = true;
        let (take, done) = match chunk.iter().position(|&c| c == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        if !too_long && buf.len() + take > LINE_MAX + 1 {
            too_long = true;
            buf.clear();
        }
        if !too_long {
            buf.extend_from_slice(&chunk[..take]);
        }
        input.consume(take);
        if done {
            break;
        }
    }
    if too_long {
        buf.clear();
        return Ok(Frame::TooLong);
    }
    if !any {
        return Ok(Frame::End);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    assert!(buf.len() <= LINE_MAX);
    Ok(Frame::Line)
}

// ------------------------------------------------------------------ the socket boundary

/// A listening local socket: a Unix-domain socket path (Unix only) or a loopback TCP
/// `host:port` (every platform).
#[derive(Debug)]
pub enum Listener {
    /// `127.0.0.1:<port>` or `[::1]:<port>`.
    Tcp(std::net::TcpListener),
    /// A socket file.
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixListener),
}

/// One client connection of a [`Listener`].
#[derive(Debug)]
pub enum Stream {
    /// TCP.
    Tcp(std::net::TcpStream),
    /// Unix-domain socket.
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

/// Listens on `addr`: a socket address (`127.0.0.1:7777`) is TCP and must be loopback, so
/// nothing off this machine can drive the editor (D7); anything else is a Unix-domain socket
/// path. A stale socket file nobody listens on is replaced; any other existing file is left
/// alone (the bind fails).
///
/// # Errors
///
/// A non-loopback address, a path on a platform without Unix sockets, or the OS refusing
/// the bind.
pub fn listen(addr: &str) -> io::Result<Listener> {
    if let Ok(sock) = addr.parse::<std::net::SocketAddr>() {
        if !sock.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "only loopback addresses (127.0.0.1, ::1) are served",
            ));
        }
        return std::net::TcpListener::bind(sock).map(Listener::Tcp);
    }
    listen_path(Path::new(addr))
}

#[cfg(unix)]
fn listen_path(path: &Path) -> io::Result<Listener> {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    let stale = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket())
        && UnixStream::connect(path).is_err();
    if stale {
        std::fs::remove_file(path)?;
    }
    UnixListener::bind(path).map(Listener::Unix)
}

#[cfg(not(unix))]
fn listen_path(_: &Path) -> io::Result<Listener> {
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "Unix-domain sockets need Unix: listen on 127.0.0.1:<port>",
    ))
}

impl Listener {
    /// Waits for the next client.
    ///
    /// # Errors
    ///
    /// The OS's accept error.
    pub fn accept(&self) -> io::Result<Stream> {
        match self {
            Listener::Tcp(l) => l.accept().map(|(s, _)| Stream::Tcp(s)),
            #[cfg(unix)]
            Listener::Unix(l) => l.accept().map(|(s, _)| Stream::Unix(s)),
        }
    }
}

impl Stream {
    /// A second handle on the same connection (one thread reads, another writes).
    ///
    /// # Errors
    ///
    /// The OS refusing to duplicate the handle.
    pub fn try_clone(&self) -> io::Result<Stream> {
        match self {
            Stream::Tcp(s) => s.try_clone().map(Stream::Tcp),
            #[cfg(unix)]
            Stream::Unix(s) => s.try_clone().map(Stream::Unix),
        }
    }

    /// Bounds how long a write may block on a client that stops reading.
    ///
    /// # Errors
    ///
    /// The OS refusing the option.
    pub fn set_write_timeout(&self, t: Option<std::time::Duration>) -> io::Result<()> {
        match self {
            Stream::Tcp(s) => s.set_write_timeout(t),
            #[cfg(unix)]
            Stream::Unix(s) => s.set_write_timeout(t),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Stream::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Stream::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Tcp(s) => s.flush(),
            #[cfg(unix)]
            Stream::Unix(s) => s.flush(),
        }
    }
}

// ------------------------------------------------------------------ the headless server

/// The CLI commands the headless server runs as methods (params: their argv, as strings).
pub const CLI_METHODS: [&str; 6] = ["check", "diff", "fmt", "import", "netlist", "run"];
/// Every method the headless server answers, sorted.
pub const METHODS: [&str; 8] = [
    "check",
    "diff",
    "edit",
    "fmt",
    "import",
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

/// The stream loop behind [`serve`] and [`crate::mcp::serve`]: `call` answers one decoded request,
/// `Err((code, message))` becoming its error response. Same framing and errors as [`serve`].
///
/// # Errors
///
/// I/O errors of `input` or `output` only.
pub fn serve_with<R: BufRead, W: Write>(
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
            assert!(reply.ends_with('\n') && reply.matches('\n').count() == 1);
            output.write_all(reply.as_bytes())?;
            output.flush()?;
        }
    }
}

impl Headless {
    /// Runs one method. `Err` is `(code, message)` for the error response.
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
    use proptest::prelude::*;

    fn json() -> impl Strategy<Value = Json> {
        let leaf = prop_oneof![
            Just(Json::Null),
            any::<bool>().prop_map(Json::Bool),
            any::<f64>()
                .prop_filter("finite", |x| x.is_finite())
                .prop_map(Json::Num),
            (-1_000_000i64..1_000_000).prop_map(|i| Json::Num(i as f64)),
            "\\PC{0,12}|[\\x00-\\x7f]{0,12}".prop_map(Json::Str),
        ];
        leaf.prop_recursive(4, 48, 6, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..6).prop_map(Json::Arr),
                proptest::collection::vec(("[a-z\"\\\\]{0,6}", inner), 0..6).prop_map(Json::Obj),
            ]
        })
    }

    fn text(v: &Json) -> String {
        let mut s = String::new();
        write(v, &mut s);
        s
    }

    proptest! {
        /// Oracle: decoding an encoded value gives the value back, and the encoding is one
        /// line.
        #[test]
        fn json_round_trips(v in json()) {
            let s = text(&v);
            prop_assert!(!s.contains('\n'));
            prop_assert_eq!(parse(&s), Ok(v));
        }

        /// Any bytes: a request or a declared error response, never a panic.
        #[test]
        fn malformed_lines_are_declared_errors(bytes in proptest::collection::vec(any::<u8>(), 0..96)) {
            if let Err(reply) = decode(&bytes) {
                let v = parse(reply.trim_end()).unwrap();
                let code = v.get("error").and_then(|e| e.get("code")).cloned();
                prop_assert!(matches!(code, Some(Json::Num(c)) if c == f64::from(PARSE_ERROR) || c == f64::from(INVALID_REQUEST)));
            }
        }
    }

    #[test]
    fn parse_accepts_rfc_8259_and_refuses_the_rest() {
        let ok = [
            ("  null ", Json::Null),
            ("-0.5e2", Json::Num(-50.0)),
            (r#""a\u00e9\ud83d\ude00\n""#, Json::Str("aé😀\n".into())),
            (
                r#"{"a":[1,true],"a":{}}"#,
                Json::Obj(vec![
                    (
                        "a".into(),
                        Json::Arr(vec![Json::Num(1.0), Json::Bool(true)]),
                    ),
                    ("a".into(), Json::Obj(Vec::new())),
                ]),
            ),
        ];
        for (src, want) in ok {
            assert_eq!(parse(src), Ok(want), "{src}");
        }
        let deep = "[".repeat(DEPTH_MAX as usize + 1);
        for bad in [
            "",
            "01",
            "1.",
            "-",
            "1e",
            "+1",
            "1e999",
            "nul",
            "[1,]",
            "{\"a\" 1}",
            "{a:1}",
            "\"\\x\"",
            "\"\\ud800\"",
            "\"\\u12g4\"",
            "\"\\u+123\"",
            "\"a\nb\"",
            "1 2",
            deep.as_str(),
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn decode_requests_and_invalid_ones() {
        let req = decode(br#"{"jsonrpc":"2.0","id":"x","method":"fmt","params":["a"]}"#).unwrap();
        assert_eq!(req.id, Some(Json::Str("x".into())));
        assert_eq!(req.method, "fmt");
        assert_eq!(req.params, Json::Arr(vec![Json::Str("a".into())]));
        let note = decode(br#"{"jsonrpc":"2.0","method":"edit.undo"}"#).unwrap();
        assert_eq!((note.id, note.params), (None, Json::Arr(Vec::new())));
        let cases: [(&[u8], i32, &str); 7] = [
            (b"\xff", PARSE_ERROR, "null"),
            (b"{", PARSE_ERROR, "null"),
            (b"[]", INVALID_REQUEST, "null"),
            (br#"{"id":3,"method":"x"}"#, INVALID_REQUEST, "3"),
            (
                br#"{"jsonrpc":"2.0","id":[1],"method":"x"}"#,
                INVALID_REQUEST,
                "null",
            ),
            (
                br#"{"jsonrpc":"2.0","id":4,"method":7}"#,
                INVALID_REQUEST,
                "4",
            ),
            (
                br#"{"jsonrpc":"2.0","id":5,"method":"x","params":1}"#,
                INVALID_REQUEST,
                "5",
            ),
        ];
        for (bad, code, id) in cases {
            let reply = decode(bad).unwrap_err();
            let v = parse(reply.trim_end()).unwrap();
            assert_eq!(
                v.get("error").and_then(|e| e.get("code")),
                Some(&Json::Num(f64::from(code)))
            );
            assert_eq!(text(v.get("id").unwrap()), id);
        }
    }

    #[test]
    fn frames_are_bounded_lines() {
        let mut input = &b"a\r\n\nlast"[..];
        let mut buf = Vec::new();
        let mut got = Vec::new();
        loop {
            match read_frame(&mut input, &mut buf).unwrap() {
                Frame::End => break,
                f => got.push((f, String::from_utf8(buf.clone()).unwrap())),
            }
        }
        assert_eq!(
            got,
            [
                (Frame::Line, "a".to_owned()),
                (Frame::Line, String::new()),
                (Frame::Line, "last".to_owned())
            ]
        );
        let long = vec![b'x'; LINE_MAX + 1];
        let mut input = io::Cursor::new([long.as_slice(), &b"\n{}\n"[..]].concat());
        assert_eq!(read_frame(&mut input, &mut buf).unwrap(), Frame::TooLong);
        assert!(buf.is_empty());
        assert_eq!(read_frame(&mut input, &mut buf).unwrap(), Frame::Line);
        assert_eq!(buf, b"{}");
    }

    /// Every method is reachable (none answers "method not found"); unknown names do.
    #[test]
    fn every_method_is_reachable() {
        let h = Headless {
            exe: None,
            flags: Vec::new(),
            seed: 1,
        };
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
        let h = Headless {
            exe: None,
            flags: Vec::new(),
            seed: 1,
        };
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
