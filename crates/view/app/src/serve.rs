//! `serve`: request lines on a local socket → commands queued into the running editor, and
//! their answers and the editor's events → response and notification lines
//! (`schemify-gui --rpc <socket>`; D27, `docs/spec/rpc.md`). Clients connect through
//! `schemify_cli::rpc::listen`; a thread per client reads its lines; the frame loop takes
//! them between frames, queues each through `schemify_editor::rpc::dispatch` (the command
//! table menus, keys and plugins name), answers after the frame that ran it, and sends
//! every editor event as an `event` notification to every client.
//!
//! Spine position: [`Pump::take`] before `Editor::update` (beside input), [`Pump::answer`]
//! after it. A reader thread that hands over a message wakes the window ([`Waker`]), so an
//! idle editor answers at once and sleeps otherwise (idle CPU 0%, D39).
//!
//! Contents:
//! - `pub struct Pump`, `Pump::start(listener: Listener, ed: &Editor) -> Pump`,
//!   `Pump::take(&mut self, ed: &mut Editor)`, `Pump::waiting(&self) -> bool`,
//!   `Pump::answer(&mut self, ed: &mut Editor)`
//! - `pub fn run(opts: Options, socket: &str) -> Result<(), String>`

use std::io::Write;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use schemify_cli::rpc::{self, Frame, Json, Listener, Request, Stream};
use schemify_editor::editor::{Editor, Options, QUEUED_MAX};
use schemify_editor::plugins::Note;
use schemify_editor::rpc::{Dispatched, dispatch};
use schemify_render::{App, Painter, Waker};
use schemify_ui::InputEvents;

use crate::window::Window;

/// The server's side of the frame loop: the request channel, the clients, the answers due
/// in arrival order and the last editor event sent. Answers due plus notifications whose
/// command has not run stay at most [`QUEUED_MAX`], so `Editor::command_answered` never
/// drops one.
pub struct Pump {
    rx: Receiver<Msg>,
    wake: Wake,
    /// Every connected client: each gets the event notifications.
    clients: Vec<Conn>,
    /// Answers in arrival order; per client, a later answer waits behind an earlier one.
    due: Vec<Due>,
    /// Tickets of commands sent as notifications (no `id`): taken and dropped once they ran.
    unanswered: Vec<u32>,
    /// `Host::notes_seq` of the last event sent.
    seen: u64,
}

/// Tick period while an answer waits for its queued command to run.
const ANSWER_TICK_MS: u32 = 20;
/// Clients connected at once; more are refused (their connection is closed).
const CLIENTS_MAX: usize = 16;
/// How long a write may block on a client that stopped reading; it is dropped then.
const WRITE_TIMEOUT_MS: u64 = 200;

/// The window's waker, set once the window runs; empty before (and in headless tests),
/// when the first update drains whatever arrived.
type Wake = Arc<OnceLock<Waker>>;

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

/// One answer due to a client.
struct Due {
    conn: Conn,
    /// The request's id (`Json::Null` for a malformed line's reply).
    id: Json,
    answer: Answer,
}

/// What a due answer is made of.
enum Answer {
    /// A ready response line.
    Line(String),
    /// The ticket of a queued command, answered `{"status": <what it said>}` once it ran
    /// (`Editor::take_said`).
    Ticket(u32),
}

/// The editor window with the server's two spine steps around its update.
struct Served {
    win: Window,
    pump: Pump,
}

impl Pump {
    /// Starts accepting clients on `listener` (a thread accepts, one thread per client
    /// reads). Notifications start with the editor events after this call.
    pub fn start(listener: Listener, ed: &Editor) -> Pump {
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = Wake::default();
        let w = wake.clone();
        std::thread::spawn(move || accept(listener, tx, w));
        Pump {
            rx,
            wake,
            clients: Vec::new(),
            due: Vec::new(),
            unanswered: Vec::new(),
            seen: ed.host().notes_seq(),
        }
    }

    /// Takes the messages that arrived, up to the [`QUEUED_MAX`] capacity: each command is
    /// queued for the next update; `rpc.methods`, unknown methods, bad params and malformed
    /// lines get their response ready now. [`Pump::answer`] sends everything, in arrival
    /// order per client; the rest of the channel waits for the next frame.
    pub fn take(&mut self, ed: &mut Editor) {
        assert!(self.held() <= QUEUED_MAX);
        // Each pass takes one message and holds at most one more answer.
        for _ in self.held()..QUEUED_MAX {
            let Ok(msg) = self.rx.try_recv() else { break };
            let (conn, req) = match msg {
                Msg::Joined(conn) => {
                    self.clients.push(conn);
                    continue;
                }
                Msg::Reply(conn, line) => {
                    let answer = Answer::Line(line);
                    self.due.push(Due {
                        conn,
                        id: Json::Null,
                        answer,
                    });
                    continue;
                }
                Msg::Call(conn, req) => (conn, req),
            };
            let answer = match dispatch(ed, &req.method, &req.params) {
                Dispatched::Queued(ticket) => Answer::Ticket(ticket),
                Dispatched::Ready(Ok(v)) => {
                    let Some(id) = &req.id else { continue };
                    Answer::Line(rpc::result(id, &v))
                }
                Dispatched::Ready(Err((code, msg))) => {
                    let Some(id) = &req.id else { continue };
                    Answer::Line(rpc::error(id, code, &msg))
                }
            };
            match (req.id, answer) {
                (Some(id), answer) => self.due.push(Due { conn, id, answer }),
                // A notification: nothing to answer, but its command still runs.
                (None, Answer::Ticket(ticket)) => self.unanswered.push(ticket),
                (None, Answer::Line(_)) => unreachable!("a notification's result is dropped"),
            }
        }
        assert!(self.held() <= QUEUED_MAX);
    }

    /// Whether an answer waits for its queued command to run: the window ticks while true.
    /// False once every answer is sent, so a served editor with no client traffic sleeps.
    pub fn waiting(&self) -> bool {
        assert!(self.held() <= QUEUED_MAX);
        self.held() != 0
    }

    /// Sends the answers that are ready, in arrival order per client (a queued command's
    /// once it ran, frames later for a plugin event on a worker; a client's later answers
    /// wait behind it), then the editor events since the last call as `event`
    /// notifications. Drops clients whose writes fail or whose reader ended.
    pub fn answer(&mut self, ed: &mut Editor) {
        assert!(self.held() <= QUEUED_MAX);
        let before = self.due.len();
        // Clients with an answer still waiting: their later answers wait too.
        let mut blocked: Vec<Conn> = Vec::new();
        for due in std::mem::take(&mut self.due) {
            let held = blocked.iter().any(|c| Arc::ptr_eq(c, &due.conn));
            let line = match &due.answer {
                _ if held => None,
                Answer::Line(line) => Some(line.clone()),
                &Answer::Ticket(ticket) => ed.take_said(ticket).map(|said| {
                    let status = Json::obj(vec![("status", Json::Str(said))]);
                    rpc::result(&due.id, &status)
                }),
            };
            match line {
                Some(line) => {
                    send(&due.conn, &line);
                }
                None => {
                    if !held {
                        blocked.push(due.conn.clone());
                    }
                    self.due.push(due);
                }
            }
        }
        self.unanswered.retain(|&t| ed.take_said(t).is_none());
        let host = ed.host();
        let lines: Vec<String> = host.notes_since(self.seen).map(event).collect();
        self.seen = host.notes_seq();
        self.clients
            .retain(|c| lines.iter().all(|l| send(c, l)) && Arc::strong_count(c) > 1);
        assert!(self.due.len() <= before);
        assert!(blocked.len() <= self.due.len());
    }
}

/// Opens the editor with the command stream on `socket` and runs until the window closes.
///
/// # Errors
///
/// The socket cannot be listened on, the project or file cannot be opened, or no window can
/// be created.
pub fn run(opts: Options, socket: &str) -> Result<(), String> {
    let listener = rpc::listen(socket).map_err(|e| format!("--rpc {socket}: {e}"))?;
    let ed = crate::window::open(opts)?;
    let pump = Pump::start(listener, &ed);
    let served = Served {
        win: Window::new(ed),
        pump,
    };
    schemify_render::run("Schemify", served).map_err(|e| e.to_string())
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
        // Answers waiting on a queued command tick until it ran; nothing else polls.
        let answer = self.pump.waiting().then_some(ANSWER_TICK_MS);
        self.win.wake_ms().into_iter().chain(answer).min()
    }

    fn set_waker(&mut self, waker: Waker) {
        let set = self.pump.wake.set(waker).is_ok();
        assert!(set, "the window sets its waker once");
    }

    fn exit_requested(&self) -> bool {
        self.win.exit_requested()
    }

    fn close(&mut self) {
        self.win.close();
    }
}

// ---- private ----

impl Pump {
    /// Answers held: due ones plus notifications whose command has not run.
    fn held(&self) -> usize {
        self.due.len() + self.unanswered.len()
    }
}

/// An `event` notification from one recorded editor event (`doc_changed` 3 7 →
/// `{"kind": "doc_changed", "values": ["3", "7"]}`).
fn event(note: &Note) -> String {
    let values = note.values().iter().map(|v| Json::Str(v.to_string()));
    let params = Json::obj(vec![
        ("kind", Json::Str(note.event.word().to_owned())),
        ("values", Json::Arr(values.collect())),
    ]);
    rpc::notification("event", &params)
}

/// Writes one line to a client; false when the client is gone (or stopped reading).
fn send(conn: &Conn, line: &str) -> bool {
    assert!(line.ends_with('\n'), "a frame is one line");
    let Ok(mut stream) = conn.lock() else {
        return false;
    };
    stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.flush())
        .is_ok()
}

/// The accept loop: one reader thread per client, at most [`CLIENTS_MAX`] at once. Runs
/// until the listener fails or the editor is gone.
fn accept(listener: Listener, tx: Sender<Msg>, wake: Wake) {
    // One count per reader thread, plus this one.
    let live = Arc::new(());
    let timeout = Some(std::time::Duration::from_millis(WRITE_TIMEOUT_MS));
    loop {
        let Ok(stream) = listener.accept() else {
            return;
        };
        if Arc::strong_count(&live) > CLIENTS_MAX {
            continue; // dropping the stream closes it
        }
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
        let (tx, token, wake) = (tx.clone(), live.clone(), wake.clone());
        std::thread::spawn(move || read_client(reader, conn, tx, wake, token));
        assert!(Arc::strong_count(&live) <= CLIENTS_MAX + 1);
    }
}

/// One client's reader: decodes each line and hands the request (or a malformed line's
/// error response) to the frame loop, which answers in arrival order; ends at end of input
/// or when the editor is gone. `_token` counts it as connected.
fn read_client(stream: Stream, conn: Conn, tx: Sender<Msg>, wake: Wake, _token: Arc<()>) {
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
        if let Some(w) = wake.get() {
            w.wake();
        }
    }
}
