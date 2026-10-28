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
//! - `pub struct Pump`, `Pump::start(listener: Listener, editor: &Editor) -> Pump`,
//!   `Pump::take(&mut self, editor: &mut Editor)`, `Pump::waiting(&self) -> bool`,
//!   `Pump::answer(&mut self, editor: &mut Editor)`
//! - `pub fn run(options: Options, socket: &str) -> Result<(), String>`

use std::io::{BufReader, Write};
use std::mem;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use schemify_cli::rpc::{self, Frame, Json, Listener, Request, Stream};
use schemify_editor::editor::{Editor, Options, QUEUED_MAX};
use schemify_editor::plugins::Note;
use schemify_editor::rpc::{self as commands, Dispatched};
use schemify_render::accesskit::TreeUpdate;
use schemify_render::{App, Painter, Waker};
use schemify_ui::InputEvents;

use crate::window::{self, Window};

/// The server's side of the frame loop: the request channel, the clients, the answers due
/// in arrival order and the last editor event sent. Answers due plus notifications whose
/// command has not run stay at most [`QUEUED_MAX`], so `Editor::command_answered` never
/// drops one.
pub struct Pump {
    receiver: Receiver<Message>,
    wake: Wake,
    /// Every connected client: each gets the event notifications.
    clients: Vec<Connection>,
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

// A server refusing every client, or an answer tick of 0 ms (a busy loop), is a typo.
const _: () = assert!(CLIENTS_MAX >= 1);
const _: () = assert!(ANSWER_TICK_MS >= 1);

/// The window's waker, set once the window runs; empty before (and in headless tests),
/// when the first update drains whatever arrived. Shared (`Arc`) because the window's
/// thread sets it and every reader thread wakes through it.
type Wake = Arc<OnceLock<Waker>>;

/// One client's write side: only the frame loop writes; the reader thread holds a clone
/// while the client is connected (the frame loop drops clients whose count falls to one).
/// Shared (`Arc<Mutex>`) because the reader thread and the frame loop both hold it, and
/// the reader's clone is how the frame loop sees that the client left.
type Connection = Arc<Mutex<Stream>>;

/// What the client threads hand the frame loop.
enum Message {
    /// A client connected: it gets notifications from now on.
    Joined(Connection),
    /// A request line that decoded.
    Call(Connection, Request),
    /// The error response to a line that did not decode, kept in line with the answers.
    Reply(Connection, String),
}

/// One answer due to a client.
struct Due {
    connection: Connection,
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
    window: Window,
    pump: Pump,
}

impl Pump {
    /// Starts accepting clients on `listener` (a thread accepts, one thread per client
    /// reads). Notifications start with the editor events after this call.
    ///
    /// # Panics
    ///
    /// The OS cannot start the accept thread.
    #[must_use]
    pub fn start(listener: Listener, editor: &Editor) -> Pump {
        let (sender, receiver) = mpsc::channel();
        let wake = Wake::default();
        let accept_wake = Arc::clone(&wake);
        thread::spawn(move || accept(listener, sender, accept_wake));
        Pump {
            receiver,
            wake,
            clients: Vec::new(),
            due: Vec::new(),
            unanswered: Vec::new(),
            seen: editor.host().notes_seq(),
        }
    }

    /// Takes the messages that arrived, up to the [`QUEUED_MAX`] capacity: each command is
    /// queued for the next update; `rpc.methods`, unknown methods, bad params and malformed
    /// lines get their response ready now. [`Pump::answer`] sends everything, in arrival
    /// order per client; the rest of the channel waits for the next frame.
    ///
    /// # Panics
    ///
    /// More than [`QUEUED_MAX`] answers are held: a bug in this server's bookkeeping.
    pub fn take(&mut self, editor: &mut Editor) {
        assert!(self.held() <= QUEUED_MAX, "answers held within capacity");
        // Each pass takes one message and holds at most one more answer.
        for _ in self.held()..QUEUED_MAX {
            let held = self.held();
            let Ok(message) = self.receiver.try_recv() else {
                break;
            };
            let (connection, request) = match message {
                Message::Joined(connection) => {
                    self.clients.push(connection);
                    continue;
                }
                Message::Reply(connection, line) => {
                    let answer = Answer::Line(line);
                    self.due.push(Due {
                        connection,
                        id: Json::Null,
                        answer,
                    });
                    continue;
                }
                Message::Call(connection, request) => (connection, request),
            };
            let dispatched = commands::dispatch(editor, &request.method, &request.params);
            let (id, answer) = match (dispatched, request.id) {
                (Dispatched::Queued(ticket), Some(id)) => (id, Answer::Ticket(ticket)),
                (Dispatched::Ready(Ok(value)), Some(id)) => {
                    let line = rpc::result(&id, &value);
                    (id, Answer::Line(line))
                }
                (Dispatched::Ready(Err((code, error))), Some(id)) => {
                    let line = rpc::error(&id, code, &error);
                    (id, Answer::Line(line))
                }
                // A notification: nothing to answer, but a queued command still runs.
                (Dispatched::Queued(ticket), None) => {
                    self.unanswered.push(ticket);
                    continue;
                }
                (Dispatched::Ready(_), None) => continue,
            };
            self.due.push(Due {
                connection,
                id,
                answer,
            });
            assert_eq!(self.held(), held + 1, "a call holds one answer");
        }
        assert!(self.held() <= QUEUED_MAX, "answers held within capacity");
    }

    /// Whether an answer waits for its queued command to run: the window ticks while true.
    /// False once every answer is sent, so a served editor with no client traffic sleeps.
    ///
    /// # Panics
    ///
    /// More than [`QUEUED_MAX`] answers are held: a bug in this server's bookkeeping.
    #[must_use]
    pub fn waiting(&self) -> bool {
        assert!(self.held() <= QUEUED_MAX, "answers held within capacity");
        self.held() != 0
    }

    /// Sends the answers that are ready, in arrival order per client (a queued command's
    /// once it ran, frames later for a plugin event on a worker; a client's later answers
    /// wait behind it), then the editor events since the last call as `event`
    /// notifications. Drops clients whose writes fail or whose reader ended.
    ///
    /// # Panics
    ///
    /// More than [`QUEUED_MAX`] answers are held: a bug in this server's bookkeeping.
    pub fn answer(&mut self, editor: &mut Editor) {
        assert!(self.held() <= QUEUED_MAX, "answers held within capacity");
        let before = self.due.len();
        let seen_before = self.seen;
        // Clients with an answer still waiting: their later answers wait too.
        let mut blocked: Vec<Connection> = Vec::new();
        for due in mem::take(&mut self.due) {
            let held = blocked
                .iter()
                .any(|connection| Arc::ptr_eq(connection, &due.connection));
            let line = match &due.answer {
                _ if held => None,
                Answer::Line(line) => Some(line.clone()),
                &Answer::Ticket(ticket) => editor.take_said(ticket).map(|said| {
                    let status = Json::obj(vec![("status", Json::Str(said))]);
                    rpc::result(&due.id, &status)
                }),
            };
            match line {
                Some(line) => {
                    send(&due.connection, &line);
                }
                None => {
                    if !held {
                        blocked.push(Arc::clone(&due.connection));
                    }
                    self.due.push(due);
                }
            }
        }
        self.unanswered
            .retain(|&ticket| editor.take_said(ticket).is_none());
        let host = editor.host();
        let lines: Vec<String> = host.notes_since(self.seen).map(event).collect();
        self.seen = host.notes_seq();
        assert!(self.seen >= seen_before, "the event sequence only grows");
        self.clients.retain(|client| {
            lines.iter().all(|line| send(client, line)) && Arc::strong_count(client) > 1
        });
        assert!(self.due.len() <= before, "answering never adds an answer");
        assert!(
            blocked.len() <= self.due.len(),
            "a blocked client holds an answer"
        );
    }
}

/// Opens the editor with the command stream on `socket` and runs until the window closes.
///
/// # Errors
///
/// The socket cannot be listened on, the project or file cannot be opened, or no window can
/// be created.
pub fn run(options: Options, socket: &str) -> Result<(), String> {
    let listener = rpc::listen(socket).map_err(|error| format!("--rpc {socket}: {error}"))?;
    let editor = window::open(options)?;
    let pump = Pump::start(listener, &editor);
    let served = Served {
        window: Window::new(editor),
        pump,
    };
    schemify_render::run("Schemify", served).map_err(|error| error.to_string())
}

impl App for Served {
    fn update(&mut self, events: &InputEvents, viewport: (f32, f32)) -> bool {
        self.pump.take(self.window.editor_mut());
        let changed = self.window.update(events, viewport);
        self.pump.answer(self.window.editor_mut());
        changed
    }

    fn paint<P: Painter>(&mut self, painter: &mut P, viewport: (f32, f32)) {
        self.window.paint(painter, viewport);
    }

    fn access(&mut self) -> TreeUpdate {
        self.window.access()
    }

    fn wake_ms(&self) -> Option<u32> {
        // Answers waiting on a queued command tick until it ran; nothing else polls.
        let answer = self.pump.waiting().then_some(ANSWER_TICK_MS);
        self.window.wake_ms().into_iter().chain(answer).min()
    }

    fn set_waker(&mut self, waker: Waker) {
        let set = self.pump.wake.set(waker).is_ok();
        assert!(set, "the window sets its waker once");
    }

    fn exit_requested(&self) -> bool {
        self.window.exit_requested()
    }

    fn close(&mut self) {
        self.window.close();
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
    let values: Vec<Json> = note
        .values()
        .iter()
        .map(|value| Json::Str(value.to_string()))
        .collect();
    let params = Json::obj(vec![
        ("kind", Json::Str(note.event.word().to_owned())),
        ("values", Json::Arr(values)),
    ]);
    let line = rpc::notification("event", &params);
    assert!(line.ends_with('\n'), "`send` writes whole lines");
    line
}

/// Writes one line to a client; false when the client is gone (or stopped reading).
fn send(connection: &Connection, line: &str) -> bool {
    let body = line.strip_suffix('\n');
    assert!(body.is_some(), "a frame ends its line");
    assert!(
        !body.is_some_and(|body| body.contains('\n')),
        "a frame is one line"
    );
    // A poisoned lock means a reader thread panicked mid-write: drop the client, never
    // write through the poisoned guard.
    let Ok(mut stream) = connection.lock() else {
        return false;
    };
    stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.flush())
        .is_ok()
}

/// The accept loop: one reader thread per client, at most [`CLIENTS_MAX`] at once. Runs
/// until the listener fails or the editor is gone.
///
/// # Panics
///
/// The OS cannot start a reader thread.
fn accept(listener: Listener, sender: Sender<Message>, wake: Wake) {
    // One count per reader thread, plus this one: the `Arc` is the readers' live count,
    // dropped by each thread as it ends (no counter to decrement on every exit path).
    let live = Arc::new(());
    let timeout = Some(Duration::from_millis(WRITE_TIMEOUT_MS));
    // Runs until the listener or the editor is gone: an accept loop has no other bound.
    loop {
        assert!(
            Arc::strong_count(&live) <= CLIENTS_MAX + 1,
            "readers within capacity"
        );
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
        let connection: Connection = Arc::new(Mutex::new(stream));
        if sender
            .send(Message::Joined(Arc::clone(&connection)))
            .is_err()
        {
            return;
        }
        let sender = sender.clone();
        let (token, wake) = (Arc::clone(&live), Arc::clone(&wake));
        thread::spawn(move || read_client(reader, connection, sender, wake, token));
        assert!(
            Arc::strong_count(&live) <= CLIENTS_MAX + 1,
            "readers within capacity"
        );
    }
}

/// One client's reader: decodes each line and hands the request (or a malformed line's
/// error response) to the frame loop, which answers in arrival order; ends at end of input
/// or when the editor is gone. `_token` counts it as connected.
fn read_client(
    stream: Stream,
    connection: Connection,
    sender: Sender<Message>,
    wake: Wake,
    _token: Arc<()>,
) {
    let mut input = BufReader::new(stream);
    let mut line = Vec::new();
    // Bounded by the client's input: every pass consumes a line.
    loop {
        let message = match rpc::read_frame(&mut input, &mut line) {
            Ok(Frame::End) | Err(_) => return,
            Ok(Frame::TooLong) => {
                let reply = rpc::error(&Json::Null, rpc::PARSE_ERROR, "line over 16 MiB");
                Message::Reply(Arc::clone(&connection), reply)
            }
            Ok(Frame::Line) if line.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(Frame::Line) => match rpc::decode(&line) {
                Err(reply) => Message::Reply(Arc::clone(&connection), reply),
                Ok(request) => Message::Call(Arc::clone(&connection), request),
            },
        };
        if sender.send(message).is_err() {
            return;
        }
        if let Some(waker) = wake.get() {
            waker.wake();
        }
    }
}
