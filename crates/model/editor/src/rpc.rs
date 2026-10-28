//! The D27 command stream's methods over the editor (`docs/spec/rpc.md`): a JSON-RPC
//! request becomes a queued [`Editor::command`] (the same command table menus, keys and
//! plugins name) or an immediate answer. The socket server that feeds it from clients is
//! `schemify-app`'s (`serve`); the headless `cmd` runner uses it too.
//!
//! Contents:
//! - `pub enum Dispatched`
//! - `pub fn dispatch`
//! - `pub fn methods`

use schemify_cli::rpc::{INVALID_PARAMS, Json, METHOD_NOT_FOUND};
use schemify_plugin::abi::{Kind, text};

use crate::commands::{CORE, Target};
use crate::editor::Editor;

/// What [`dispatch`] did with a request.
#[derive(Clone, Debug, PartialEq)]
pub enum Dispatched {
    /// Queued as a command: its answer is the status it leaves (ticket for
    /// [`Editor::take_said`]).
    Queued(u32),
    /// Answered now: the result, or a JSON-RPC error (code, message).
    Ready(Result<Json, (i32, String)>),
}

/// Runs one method against the editor: queued as a command (answered with the status it
/// leaves, [`Editor::command_answered`]), or answered now. Methods are the command table's
/// names (core and plugin rows, `<name>.<word>` suffix arguments included) plus
/// `rpc.methods`; params are an array of strings, passed as line-record values (the `args`
/// a menu row or key carries).
///
/// # Panics
///
/// Never: the assert checks one value per param.
pub fn dispatch(ed: &mut Editor, method: &str, params: &Json) -> Dispatched {
    if method == "rpc.methods" {
        return Dispatched::Ready(Ok(methods(ed)));
    }
    if ed.commands().resolve_arg(method).0 == Target::Unknown {
        return Dispatched::Ready(Err((METHOD_NOT_FOUND, format!("no command `{method}`"))));
    }
    let args: Option<Vec<&str>> = match params {
        Json::Arr(items) => items.iter().map(Json::as_str).collect(),
        _ => None,
    };
    let Some(args) = args else {
        return Dispatched::Ready(Err((INVALID_PARAMS, "params: an array of strings".into())));
    };
    let mut arg = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            arg.push(' ');
        }
        text::push_value(&mut arg, a);
    }
    assert!(
        arg.len() + 1 >= args.len(),
        "a value per param, space separated"
    );
    Dispatched::Queued(ed.command_answered(method, &arg))
}

/// Every method name: `rpc.methods`, the core commands, then the plugin commands, sorted
/// within each group.
///
/// # Panics
///
/// Never: the asserts check one name per method.
pub fn methods(ed: &Editor) -> Json {
    let mut names = vec![Json::Str("rpc.methods".into())];
    names.extend(CORE.iter().map(|(n, _)| Json::Str((*n).to_owned())));
    let mut plugin: Vec<&str> = ed
        .host()
        .plugins
        .composed()
        .of(Kind::Commands)
        .iter()
        .filter_map(|r| r.key.strip_prefix("command "))
        .collect();
    plugin.sort_unstable();
    plugin.dedup();
    let total = names.len() + plugin.len();
    names.extend(plugin.into_iter().map(|n| Json::Str(n.to_owned())));
    assert_eq!(names.len(), total, "one name per method");
    assert!(
        names.len() > CORE.len(),
        "`rpc.methods` and every core command"
    );
    Json::Arr(names)
}
