//! `schemify-gui --rpc <socket>`: the D27 command stream into the running editor
//! (`docs/spec/rpc.md`). Clients connect to a local socket ([`schemify_cli::rpc::listen`]);
//! a thread per client reads its request lines; the frame loop takes them between frames,
//! queues each as [`Editor::command`] (the same command table menus, keys and plugins
//! name), answers after the frame that ran it, and sends every editor event as an `event`
//! notification to every client.
//!
//! Spine position: [`Pump::take`] before `Editor::update` (beside input), [`Pump::answer`]
//! after it. The window polls every [`POLL_MS`] while the server runs, so a request reaches
//! an idle editor within that time (idle CPU stays near 0%, D39: the server exists only when
//! asked for).

use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};

use schemify_cli::rpc::{
    self, Frame, INVALID_PARAMS, Json, Listener, METHOD_NOT_FOUND, Request, Stream,
};
use schemify_plugin::abi::text;
use schemify_render::{App, Painter};
use schemify_ui::InputEvents;

use crate::commands::{CORE, Target};
use crate::editor::{Editor, Options, QUEUED_MAX, Window};

/// Tick period while the server runs.
pub const POLL_MS: u32 = 50;
/// Clients connected at once; more are refused (their connection is closed).
pub const CLIENTS_MAX: usize = 16;
/// How long a write may block on a client that stopped reading; it is dropped then.
const WRITE_TIMEOUT_MS: u64 = 200;

/// One client's write side: only the frame loop writes; the reader thread holds a clone
/// while the client is connected (the frame loop drops clients whose count falls to one).
type Conn = Arc<Mutex<Stream>>;

/// What the client threads hand the frame loop.
enum Msg {
    /// A client connected: it gets notifications from now on.
    Joined(Conn),
    /// A request line that decoded.
    Call(Conn, Request),
    /// The error response to a line that did not decode, kept in line with the answers.
    Reply(Conn, String),
}

/// One answer due: a ready response line, or the ticket of a queued command whose status
/// result is sent once the editor has it ([`Editor::take_said`]).
type Due = (Conn, Json, Result<String, u32>);

/// What [`dispatch`] did with a request.
#[derive(Clone, Debug, PartialEq)]
pub enum Dispatched {
    /// Queued as a command: its answer is the status it leaves (ticket for
    /// [`Editor::take_said`]).
    Queued(u32),
    /// Answered now: the result, or a JSON-RPC error (code, message).
    Ready(Result<Json, (i32, String)>),
}

/// The server's side of the frame loop: the request channel, the clients, this frame's
/// answers in arrival order and the last editor event sent.
pub struct Pump {
    rx: Receiver<Msg>,
    clients: Vec<Conn>,
    due: Vec<Due>,
    /// Tickets of commands sent as notifications (no `id`): taken and dropped once they ran.
    unanswered: Vec<u32>,
    seen: u64,
}

impl Pump {
    /// Starts accepting clients on `listener` (a thread accepts, one thread per client
    /// reads). Notifications start with the editor events after this call.
    pub fn start(listener: Listener, ed: &Editor) -> Pump {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || accept(listener, tx));
        Pump {
            rx,
            clients: Vec::new(),
            due: Vec::new(),
            unanswered: Vec::new(),
            seen: ed.host().notes_seq(),
        }
    }

    /// Takes the messages that arrived (at most [`QUEUED_MAX`] answers wait, so
    /// [`Editor::command_answered`] never drops one): each command is queued for the next
    /// update; `rpc.methods`, unknown methods, bad params and malformed lines get their
    /// response ready now. Everything is sent by [`Pump::answer`], in arrival order per
    /// client.
    pub fn take(&mut self, ed: &mut Editor) {
        assert!(self.due.len() + self.unanswered.len() <= QUEUED_MAX);
        for _ in self.due.len() + self.unanswered.len()..QUEUED_MAX {
            let (conn, req) = match self.rx.try_recv() {
                Ok(Msg::Joined(conn)) => {
                    self.clients.push(conn);
                    continue;
                }
                Ok(Msg::Reply(conn, line)) => {
                    self.due.push((conn, Json::Null, Ok(line)));
                    continue;
                }
                Ok(Msg::Call(conn, req)) => (conn, req),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            };
            let out = match dispatch(ed, &req.method, &req.params) {
                Dispatched::Queued(ticket) => Err(ticket),
                Dispatched::Ready(out) => Ok(req.id.as_ref().map(|id| reply(id, out))),
            };
            match (req.id, out) {
                (Some(id), Err(ticket)) => self.due.push((conn, id, Err(ticket))),
                (Some(id), Ok(Some(line))) => self.due.push((conn, id, Ok(line))),
                // A notification: nothing to answer (its command still runs).
                (None, Err(ticket)) => self.unanswered.push(ticket),
                (None, Ok(_)) | (Some(_), Ok(None)) => {}
            }
        }
        assert!(self.due.len() + self.unanswered.len() <= QUEUED_MAX);
    }

    /// Sends the answers that are ready, in arrival order per client (a queued command's is
    /// `{"status": <what it said>}` once it ran, frames later for a plugin event on a
    /// worker; a client's later answers wait behind it), then the editor events since the
    /// last call as `event` notifications. Clients whose writes fail, or whose reader ended,
    /// are dropped.
    pub fn answer(&mut self, ed: &mut Editor) {
        let mut waiting: Vec<Conn> = Vec::new();
        let mut kept: Vec<Due> = Vec::new();
        for (conn, id, due) in std::mem::take(&mut self.due) {
            let blocked = waiting.iter().any(|c| Arc::ptr_eq(c, &conn));
            let line = match &due {
                Ok(line) if !blocked => Some(line.clone()),
                &Err(ticket) if !blocked => ed.take_said(ticket).map(|said| {
                    let status = Json::obj(vec![("status", Json::Str(said))]);
                    rpc::result(&id, &status)
                }),
                _ => None,
            };
            match line {
                Some(line) => {
                    send(&conn, &line);
                }
                None => {
                    if !blocked {
                        waiting.push(conn.clone());
                    }
                    kept.push((conn, id, due));
                }
            }
        }
        self.due = kept;
        self.unanswered.retain(|&t| ed.take_said(t).is_none());
        let host = ed.host();
        let lines: Vec<String> = host.notes_since(self.seen).map(event).collect();
        self.seen = host.notes_seq();
        self.clients
            .retain(|c| lines.iter().all(|l| send(c, l)) && Arc::strong_count(c) > 1);
        assert!(self.due.len() + self.unanswered.len() <= QUEUED_MAX);
    }
}

/// Runs one method against the editor: queued as a command (answered with the status it
/// leaves, [`Editor::command_answered`]), or answered now. Methods are the command table's
/// names (core and plugin rows, `<name>.<word>` suffix arguments included) plus
/// `rpc.methods`; params are an array of strings, passed as line-record values (the `args`
/// a menu row or key carries).
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
    Dispatched::Queued(ed.command_answered(method, &arg))
}

/// Every method name: `rpc.methods`, the core commands, then the plugin commands, sorted
/// within each group.
pub fn methods(ed: &Editor) -> Json {
    let mut names = vec![Json::Str("rpc.methods".into())];
    names.extend(CORE.iter().map(|(n, _)| Json::Str((*n).to_owned())));
    let mut plugin: Vec<&str> = ed
        .host()
        .plugins
        .composed()
        .of(schemify_plugin::abi::Kind::Commands)
        .iter()
        .filter_map(|r| r.key.strip_prefix("command "))
        .collect();
    plugin.sort_unstable();
    plugin.dedup();
    names.extend(plugin.into_iter().map(|n| Json::Str(n.to_owned())));
    Json::Arr(names)
}

/// Opens the editor with the command stream on `socket` and runs until the window closes.
///
/// # Errors
///
/// The socket cannot be listened on, the project or file cannot be opened, or no window can
/// be created.
pub fn run(mut opts: Options, socket: &str) -> Result<(), String> {
    let listener = rpc::listen(socket).map_err(|e| format!("--rpc {socket}: {e}"))?;
    if opts.state_dir.is_none() {
        opts.state_dir = crate::platform::state_dir();
    }
    opts.system_clipboard = true;
    let ed = Editor::new(opts)?;
    let pump = Pump::start(listener, &ed);
    let served = Served {
        win: Window::new(ed),
        pump,
    };
    schemify_render::run("Schemify", served).map_err(|e| e.to_string())
}

/// The editor window with the server's two spine steps around its update.
struct Served {
    win: Window,
    pump: Pump,
}

impl App for Served {
    fn update(&mut self, events: &InputEvents, viewport: (f32, f32)) -> bool {
        self.pump.take(self.win.editor());
        let changed = self.win.update(events, viewport);
        self.pump.answer(self.win.editor());
        changed
    }

    fn paint<P: Painter>(&mut self, p: &mut P, viewport: (f32, f32)) {
        self.win.paint(p, viewport);
    }

    fn access(&mut self) -> schemify_render::accesskit::TreeUpdate {
        self.win.access()
    }

    fn wake_ms(&self) -> Option<u32> {
        Some(self.win.wake_ms().map_or(POLL_MS, |w| w.min(POLL_MS)))
    }

    fn exit_requested(&self) -> bool {
        self.win.exit_requested()
    }
}

/// The response line for an immediate result.
fn reply(id: &Json, out: Result<Json, (i32, String)>) -> String {
    match out {
        Ok(v) => rpc::result(id, &v),
        Err((code, msg)) => rpc::error(id, code, &msg),
    }
}

/// An `event` notification from one recorded editor event (`event doc_changed 3 7` →
/// `{"kind": "doc_changed", "values": ["3", "7"]}`).
fn event(line: &str) -> String {
    let rest = line.strip_prefix("event ").unwrap_or(line);
    let (kind, values) = text::split_word(rest).unwrap_or((rest, ""));
    let values: Vec<Json> = text::values(values)
        .filter_map(Result::ok)
        .map(|v| Json::Str(v.into_owned()))
        .collect();
    let params = Json::obj(vec![
        ("kind", Json::Str(kind.to_owned())),
        ("values", Json::Arr(values)),
    ]);
    rpc::notification("event", &params)
}

/// Writes one line to a client; false when the client is gone (or stopped reading).
fn send(conn: &Conn, line: &str) -> bool {
    use std::io::Write;
    let Ok(mut stream) = conn.lock() else {
        return false;
    };
    stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.flush())
        .is_ok()
}

/// The accept loop: one reader thread per client, at most [`CLIENTS_MAX`] at once.
fn accept(listener: Listener, tx: Sender<Msg>) {
    let live = Arc::new(());
    // A server thread: runs until the listener fails or the editor is gone.
    loop {
        let Ok(stream) = listener.accept() else {
            return;
        };
        if Arc::strong_count(&live) > CLIENTS_MAX {
            continue; // dropping the stream closes it
        }
        let timeout = Some(std::time::Duration::from_millis(WRITE_TIMEOUT_MS));
        let Ok(reader) = stream.try_clone() else {
            continue;
        };
        if stream.set_write_timeout(timeout).is_err() {
            continue;
        }
        let conn: Conn = Arc::new(Mutex::new(stream));
        if tx.send(Msg::Joined(conn.clone())).is_err() {
            return;
        }
        let (tx, token) = (tx.clone(), live.clone());
        std::thread::spawn(move || read_client(reader, conn, tx, token));
    }
}

/// One client's reader: decodes each line and hands the request (or a malformed line's
/// error response) to the frame loop, which answers in arrival order; ends at end of input
/// or when the editor is gone. `_token` counts it as connected.
fn read_client(stream: Stream, conn: Conn, tx: Sender<Msg>, _token: Arc<()>) {
    let mut input = std::io::BufReader::new(stream);
    let mut buf = Vec::new();
    // Bounded by the client's input: every pass consumes a line.
    loop {
        let msg = match rpc::read_frame(&mut input, &mut buf) {
            Ok(Frame::End) | Err(_) => return,
            Ok(Frame::TooLong) => {
                let line = rpc::error(&Json::Null, rpc::PARSE_ERROR, "line over 16 MiB");
                Msg::Reply(conn.clone(), line)
            }
            Ok(Frame::Line) if buf.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(Frame::Line) => match rpc::decode(&buf) {
                Err(line) => Msg::Reply(conn.clone(), line),
                Ok(req) => Msg::Call(conn.clone(), req),
            },
        };
        if tx.send(msg).is_err() {
            return;
        }
    }
}
