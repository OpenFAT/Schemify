//! The JSON-RPC 2.0 wire of the command stream: bytes → bounded `\n` frames → [`Request`]s,
//! and results/errors/notifications → response lines (`docs/spec/rpc.md` §2–§4). Both
//! servers (`schemify rpc`, `schemify-gui --rpc`) frame through here.
//!
//! Contents:
//! - `pub const LINE_MAX: usize`
//! - `pub const PARSE_ERROR, INVALID_REQUEST, METHOD_NOT_FOUND, INVALID_PARAMS,
//!   INTERNAL_ERROR, DESIGN_ERROR, IO_ERROR: i32`
//! - `pub struct Request { id, method, params }`
//! - `pub enum Frame { Line, TooLong, End }`
//! - `pub fn decode(&[u8]) -> Result<Request, String>`
//! - `pub fn result(&Json, &Json) -> String`, `pub fn error(&Json, i32, &str) -> String`,
//!   `pub fn notification(&str, &Json) -> String`
//! - `pub fn read_frame<R: BufRead>(&mut R, &mut Vec<u8>) -> io::Result<Frame>`

use std::io::{self, BufRead};

use super::json::{self, Json};

/// Longest request line in bytes (16 MiB: an `edit` carries whole op texts); a longer one is
/// consumed and answered with [`PARSE_ERROR`].
pub const LINE_MAX: usize = 16 << 20;

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
    let v = json::parse(text).map_err(|e| {
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
    frame(&[("id", id), ("result", result)])
}

/// The response line (with its `\n`) for a failed call.
pub fn error(id: &Json, code: i32, message: &str) -> String {
    let err = Json::obj(vec![
        ("code", Json::Num(f64::from(code))),
        ("message", Json::Str(message.to_owned())),
    ]);
    frame(&[("id", id), ("error", &err)])
}

/// A notification line (with its `\n`) from the server: `method` with `params`, no id.
pub fn notification(method: &str, params: &Json) -> String {
    frame(&[
        ("method", &Json::Str(method.to_owned())),
        ("params", params),
    ])
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
    // Each pass consumes a non-empty chunk or ends: bounded by the input.
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
        assert!(buf.is_empty());
        return Ok(Frame::End);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    }
    // The chunk test above allows one byte for the `\n`; an unterminated last line can use
    // that byte for content.
    if buf.len() > LINE_MAX {
        buf.clear();
        return Ok(Frame::TooLong);
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    assert!(buf.len() <= LINE_MAX);
    Ok(Frame::Line)
}

// ---- private ----

/// `{"jsonrpc":"2.0",<members>}\n`, written in place: no copy of the member values.
fn frame(members: &[(&str, &Json)]) -> String {
    let mut out = String::from("{\"jsonrpc\":\"2.0\"");
    for (k, v) in members {
        out.push(',');
        json::write_str(k, &mut out);
        out.push(':');
        json::write(v, &mut out);
    }
    out.push_str("}\n");
    assert_eq!(out.matches('\n').count(), 1, "one frame is one line");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::json::tests::{json, text};
    use proptest::prelude::*;

    proptest! {
        /// Oracle: a response frame is the encoding of the whole response object.
        #[test]
        fn frames_encode_the_response_object(id in json(), v in json()) {
            let tree = |k: &str, v: &Json| {
                let o = Json::obj(vec![("jsonrpc", Json::Str("2.0".into())), ("id", id.clone()), (k, v.clone())]);
                format!("{}\n", text(&o))
            };
            prop_assert_eq!(result(&id, &v), tree("result", &v));
            let err = Json::obj(vec![("code", Json::Num(-1.0)), ("message", Json::Str("m".into()))]);
            prop_assert_eq!(error(&id, -1, "m"), tree("error", &err));
        }

        /// Any bytes: a request or a declared error response, never a panic.
        #[test]
        fn malformed_lines_are_declared_errors(bytes in proptest::collection::vec(any::<u8>(), 0..96)) {
            if let Err(reply) = decode(&bytes) {
                let v = json::parse(reply.trim_end()).unwrap();
                let code = v.get("error").and_then(|e| e.get("code")).cloned();
                prop_assert!(matches!(code, Some(Json::Num(c)) if c == f64::from(PARSE_ERROR) || c == f64::from(INVALID_REQUEST)));
            }
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
            let v = json::parse(reply.trim_end()).unwrap();
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

    /// The boundary: a line of exactly `LINE_MAX` bytes is a line, one more is too long,
    /// with or without its `\n` (an unterminated last line one byte over used to reach the
    /// exit assert and panic the server).
    #[test]
    fn line_max_is_the_last_accepted_length() {
        let mut buf = Vec::new();
        let at = vec![b'x'; LINE_MAX];
        let mut input = io::Cursor::new([at.as_slice(), b"\n"].concat());
        assert_eq!(read_frame(&mut input, &mut buf).unwrap(), Frame::Line);
        assert_eq!(buf.len(), LINE_MAX);
        let mut input = io::Cursor::new(vec![b'x'; LINE_MAX + 1]);
        assert_eq!(read_frame(&mut input, &mut buf).unwrap(), Frame::TooLong);
    }
}
