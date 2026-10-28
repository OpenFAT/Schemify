//! `schemify mcp`: D27's MCP bridge, a thin Model Context Protocol server on stdio over the
//! headless command stream ([`crate::rpc`]). It speaks the same framing (one JSON-RPC 2.0
//! message per line) and only maps MCP's lifecycle and tool calls onto [`Headless::call`]:
//! every headless method but `rpc.methods` is one tool. Spec: `docs/spec/rpc.md` §7.
//!
//! Why here and not a wasm plugin: a plugin owns no stdio (plugin-abi §5), and the bridge
//! adds no behaviour of its own, so it is a second front door to the same table.
//!
//! Contents:
//! - `pub const PROTOCOL: &str`, `pub const PROTOCOLS: [&str; 3]`
//! - `pub fn serve<R: BufRead, W: Write>(&mut R, &mut W, &Headless) -> io::Result<()>`

use std::io::{self, BufRead, Write};

use crate::rpc::{self, Headless, INVALID_PARAMS, Json, METHOD_NOT_FOUND, METHODS, MethodError};

/// The protocol revision answered when the client asks for one this bridge does not list.
pub const PROTOCOL: &str = "2025-06-18";
/// Protocol revisions this bridge speaks (the message shapes it uses are the same in all).
pub const PROTOCOLS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

/// Serves MCP on `input`/`output` until the input ends: `initialize`, `ping`, `tools/list`,
/// `tools/call`; notifications (`notifications/initialized`, …) are accepted silently.
///
/// # Errors
///
/// I/O errors of `input` or `output` only; every request-level failure is a response.
pub fn serve<R: BufRead, W: Write>(input: &mut R, output: &mut W, h: &Headless) -> io::Result<()> {
    rpc::serve_with(input, output, |method, params| call(h, method, params))
}

// ---- private ----

/// One MCP method.
fn call(h: &Headless, method: &str, params: &Json) -> Result<Json, MethodError> {
    match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Json::as_str);
            let version = asked.filter(|v| PROTOCOLS.contains(v)).unwrap_or(PROTOCOL);
            Ok(Json::obj(vec![
                ("protocolVersion", str_value(version)),
                (
                    "capabilities",
                    Json::obj(vec![("tools", Json::obj(vec![]))]),
                ),
                (
                    "serverInfo",
                    Json::obj(vec![
                        ("name", str_value("schemify")),
                        ("version", str_value(env!("CARGO_PKG_VERSION"))),
                    ]),
                ),
            ]))
        }
        "ping" => Ok(Json::obj(vec![])),
        "tools/list" => Ok(Json::obj(vec![("tools", Json::Arr(tools()))])),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Json::as_str) else {
                return Err(MethodError::new(
                    INVALID_PARAMS,
                    "tools/call needs a tool `name`".to_owned(),
                ));
            };
            if name == "rpc.methods" || !METHODS.contains(&name) {
                return Err(MethodError::new(
                    INVALID_PARAMS,
                    format!("unknown tool `{name}`"),
                ));
            }
            let empty = Json::obj(vec![]);
            let arguments = params.get("arguments").unwrap_or(&empty);
            // CLI tools take `{"args": [...]}`; `edit` takes its params object as is.
            let forwarded = match name {
                "edit" => arguments.clone(),
                _ => arguments
                    .get("args")
                    .cloned()
                    .unwrap_or(Json::Arr(Vec::new())),
            };
            Ok(tool_result(h.call(name, &forwarded)))
        }
        _ => Err(MethodError::new(
            METHOD_NOT_FOUND,
            format!("unknown method `{method}`"),
        )),
    }
}

/// A JSON string value.
fn str_value(text: &str) -> Json {
    Json::Str(text.to_owned())
}

/// The JSON Schema of a string.
fn string_schema() -> Json {
    Json::obj(vec![("type", str_value("string"))])
}

/// A JSON Schema object with `props`, `required` listing the mandatory ones.
fn schema(props: Vec<(&str, Json)>, required: &[&str]) -> Json {
    for r in required {
        assert!(
            props.iter().any(|(name, _)| name == r),
            "required `{r}` is not a property"
        );
    }
    Json::obj(vec![
        ("type", str_value("object")),
        ("properties", Json::obj(props)),
        (
            "required",
            Json::Arr(required.iter().map(|r| str_value(r)).collect()),
        ),
    ])
}

/// The tool list: one tool per headless method, with its input schema.
fn tools() -> Vec<Json> {
    let mut out = Vec::new();
    let tool_methods = METHODS.iter().filter(|m| **m != "rpc.methods");
    for &m in tool_methods {
        let (about, input) = match m {
            "edit" => (
                "Apply edit ops (Schemify edits spec op text, one record per line) to a \
                 .shprim/.shcomp/.shbench file as one transaction and save it.",
                schema(
                    vec![
                        ("file", string_schema()),
                        ("ops", string_schema()),
                        ("label", string_schema()),
                    ],
                    &["file", "ops"],
                ),
            ),
            _ => (
                "Run `schemify <tool> <args…>` and return its exit code and output \
                 (see `schemify --help` for the arguments).",
                schema(
                    vec![(
                        "args",
                        Json::obj(vec![
                            ("type", str_value("array")),
                            ("items", string_schema()),
                        ]),
                    )],
                    &["args"],
                ),
            ),
        };
        out.push(Json::obj(vec![
            ("name", str_value(m)),
            ("description", str_value(about)),
            ("inputSchema", input),
        ]));
    }
    assert_eq!(out.len(), METHODS.len() - 1);
    out
}

/// A tool call's MCP result: the method's result (or error) as one text block; `isError`
/// when the method failed or the command exited non-zero.
fn tool_result(out: Result<Json, MethodError>) -> Json {
    let (text, failed) = match out {
        Ok(v) => {
            let failed = matches!(v.get("code"), Some(Json::Num(c)) if *c != 0.0);
            let mut body = String::new();
            match (v.get("stdout"), v.get("stderr")) {
                (Some(Json::Str(stdout)), Some(Json::Str(stderr))) => {
                    let code = v.get("code").cloned().unwrap_or(Json::Null);
                    rpc::write(&code, &mut body);
                    body.insert_str(0, "exit code ");
                    body.push('\n');
                    body.push_str(stdout);
                    body.push_str(stderr);
                }
                _ => rpc::write(&v, &mut body),
            }
            (body, failed)
        }
        Err(e) => (format!("error {}: {}", e.code, e.message), true),
    };
    assert!(!text.is_empty(), "a tool result says something");
    let block = Json::obj(vec![
        ("type", Json::Str("text".into())),
        ("text", Json::Str(text)),
    ]);
    Json::obj(vec![
        ("content", Json::Arr(vec![block])),
        ("isError", Json::Bool(failed)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(lines: &[&str]) -> Vec<Json> {
        let h = Headless {
            exe: None,
            flags: Vec::new(),
            seed: 1,
        };
        let input = lines.join("\n");
        let mut out = Vec::new();
        serve(&mut input.as_bytes(), &mut out, &h).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| rpc::parse(l).unwrap())
            .collect()
    }

    /// The MCP lifecycle: initialize (version negotiated), the initialized notification
    /// (no answer), tools/list (every headless method but `rpc.methods`), tools/call
    /// (failures are tool results with `isError`, unknown tools and methods are errors).
    #[test]
    fn lifecycle_tools_and_calls() {
        let r = session(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fmt","arguments":{"args":["x.shcomp"]}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope"}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"resources/list"}"#,
            r#"{"jsonrpc":"2.0","id":6,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
            r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#,
        ]);
        assert_eq!(r.len(), 7, "the notification is not answered");
        let result = |i: usize| r[i].get("result").cloned().unwrap_or(Json::Null);
        assert_eq!(
            result(0).get("protocolVersion"),
            Some(&Json::Str("2024-11-05".into()))
        );
        let Some(Json::Arr(tools)) = result(1).get("tools").cloned() else {
            panic!("tools/list: {:?}", r[1])
        };
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Json::as_str))
            .collect();
        let want: Vec<&str> = METHODS
            .iter()
            .copied()
            .filter(|m| *m != "rpc.methods")
            .collect();
        assert_eq!(names, want);
        assert!(tools.iter().all(|t| t.get("inputSchema").is_some()));
        assert_eq!(
            result(2).get("isError"),
            Some(&Json::Bool(true)),
            "no binary"
        );
        let code = |i: usize| r[i].get("error").and_then(|e| e.get("code")).cloned();
        assert_eq!(code(3), Some(Json::Num(f64::from(INVALID_PARAMS))));
        assert_eq!(code(4), Some(Json::Num(f64::from(METHOD_NOT_FOUND))));
        assert_eq!(
            result(5).get("protocolVersion"),
            Some(&Json::Str(PROTOCOL.into()))
        );
        assert_eq!(result(6), Json::obj(vec![]));
    }
}
