//! The command stream between frames: queued commands and edits, their answers, the kept plugin log.
//!
//! Contents:
//! - `pub const QUEUED_MAX`
//! - `pub(super) struct Said`, `pub(super) struct Queued`
//! - `pub(super) const KEPT_LOG_MAX`
//! - `Editor::command`
//! - `Editor::request_edits`
//! - `Editor::command_answered`
//! - `Editor::keep_plugin_log`
//! - `Editor::take_plugin_log`
//! - `Editor::take_said`
//! - `Editor::dispatch_queued`
//! - `Editor::resolve_said`

use std::mem;

use schemify_doc::EditRequest;
use schemify_plugin::PluginId;

use super::Editor;

/// Commands [`Editor::command`] queues between frames.
pub const QUEUED_MAX: usize = 256;

/// The status an answered command ([`Editor::command_answered`]) left: `status` is `None`
/// while a plugin command's event has not completed (it may run on a worker, plugin-abi §2).
#[derive(Debug)]
pub(super) struct Said {
    ticket: u32,
    plugin: Option<PluginId>,
    status: Option<String>,
    /// The plugin's event count right after the delivery that took its command
    /// ([`schemify_plugin::Plugins::event_counts`]): finished once that many have.
    target: Option<u64>,
    /// The plugin's last log line (or fault) since that delivery.
    last: Option<String>,
}

/// A command [`Editor::command`] queued for the next frame.
#[derive(Debug)]
pub(super) struct Queued {
    name: String,
    arg: String,
    /// The answer's ticket ([`Editor::command_answered`]), if one was asked for.
    ticket: Option<u32>,
}

/// Plugin log lines [`Editor::keep_plugin_log`] holds between two takes.
pub(super) const KEPT_LOG_MAX: usize = 4096;

impl Editor {
    /// Queues command `name` with argument `arg` for the next [`Editor::update`], dispatched
    /// after that frame's input exactly as a menu row or key would run it (`<name>.<word>`
    /// suffix arguments included). The entry point of the CLI / JSON-RPC command stream (D27)
    /// and of scripted tests. At most [`QUEUED_MAX`] commands wait; more are dropped with a
    /// status message.
    ///
    /// # Panics
    ///
    /// Never: commands past [`QUEUED_MAX`] are refused, not queued.
    pub fn command(&mut self, name: &str, arg: &str) {
        if self.queued.len() >= QUEUED_MAX {
            self.session.say(format!("{name}: command queue full"));
            return;
        }
        self.queued.push(Queued {
            name: name.to_owned(),
            arg: arg.to_owned(),
            ticket: None,
        });
        self.ui_stale = true;
        assert!(self.queued.len() <= QUEUED_MAX);
    }
    /// Queues edit requests for the next frame, where `doc::apply_edits` runs them with the
    /// frame's own (the path plugins and the optimizer take). Requests past [`QUEUED_MAX`]
    /// are refused on the status line, as a full command queue is.
    ///
    /// # Panics
    ///
    /// Never: requests past [`QUEUED_MAX`] are refused, not queued.
    pub fn request_edits(&mut self, reqs: impl IntoIterator<Item = EditRequest>) {
        assert!(self.queued_edits.len() <= QUEUED_MAX);
        for r in reqs {
            if self.queued_edits.len() >= QUEUED_MAX {
                self.session.say("edit queue full");
                return;
            }
            self.queued_edits.push(r);
        }
        self.ui_stale = true;
        assert!(self.queued_edits.len() <= QUEUED_MAX);
    }
    /// [`Editor::command`] for a caller that wants the command's own status message (the
    /// JSON-RPC stream, rpc spec §3.2): returns a ticket for [`Editor::take_said`]. A core
    /// command's message is the status line it set while running (empty when it set none:
    /// the status line then keeps the previous message); a plugin command's is the last log
    /// line or fault of that plugin in the delivery that completed its event, frames later
    /// for a plugin on a worker. At most [`QUEUED_MAX`] answers wait; a caller that never
    /// takes its tickets stops getting new ones answered (they say `command queue full`).
    ///
    /// # Panics
    ///
    /// Never: tickets past [`QUEUED_MAX`] are refused, not queued.
    pub fn command_answered(&mut self, name: &str, arg: &str) -> u32 {
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1);
        if self.queued.len() >= QUEUED_MAX || self.said.len() >= QUEUED_MAX {
            let full = format!("{name}: command queue full");
            if self.said.len() < QUEUED_MAX {
                self.said.push(Said {
                    ticket,
                    plugin: None,
                    status: Some(full.clone()),
                    target: None,
                    last: None,
                });
            }
            self.session.say(full);
            assert!(self.said.len() <= QUEUED_MAX);
            return ticket;
        }
        self.queued.push(Queued {
            name: name.to_owned(),
            arg: arg.to_owned(),
            ticket: Some(ticket),
        });
        self.ui_stale = true;
        assert!(self.queued.len() <= QUEUED_MAX);
        ticket
    }
    /// Starts keeping every plugin log line (the window keeps only the status line's last
    /// one), for [`Editor::take_plugin_log`]: a headless caller reports them all.
    pub fn keep_plugin_log(&mut self) {
        self.kept_log.get_or_insert_with(|| (Vec::new(), false));
    }
    /// The plugin log lines kept since the last call, and whether any delivery since
    /// [`Editor::keep_plugin_log`] failed (an error-level log or an aborted call; sticky).
    pub fn take_plugin_log(&mut self) -> (Vec<String>, bool) {
        match &mut self.kept_log {
            Some((lines, failed)) => (mem::take(lines), *failed),
            None => (Vec::new(), false),
        }
    }
    /// Takes the message answered command `ticket` left (see [`Editor::command_answered`]):
    /// `None` while it has not run or its plugin event has not completed. A ticket taken
    /// already, or never issued, answers an empty message.
    pub fn take_said(&mut self, ticket: u32) -> Option<String> {
        let Some(i) = self.said.iter().position(|s| s.ticket == ticket) else {
            let waiting = self.queued.iter().any(|q| q.ticket == Some(ticket));
            return if waiting { None } else { Some(String::new()) };
        };
        let status = self.said[i].status.take()?;
        self.said.remove(i);
        debug_assert!(
            !self.said.iter().any(|s| s.ticket == ticket),
            "tickets are unique"
        );
        Some(status)
    }
    /// Runs the commands [`Editor::command`] queued, in order, recording what each
    /// answered one said (a plugin command's answer waits for its event, see
    /// [`Editor::resolve_said`]).
    pub(super) fn dispatch_queued(&mut self, reqs: &mut Vec<EditRequest>) -> bool {
        let mut changed = false;
        for Queued { name, arg, ticket } in mem::take(&mut self.queued) {
            // Each command's own message: the status line starts empty for it and gets the
            // previous message back when the command says nothing.
            let before = self.session.take_status();
            let (c, plugin) = self.dispatch(&name, &arg, "", false, reqs);
            changed |= c;
            if let Some(ticket) = ticket {
                let status = plugin.is_none().then(|| self.session.status().to_owned());
                self.said.push(Said {
                    ticket,
                    plugin,
                    status,
                    target: None,
                    last: None,
                });
            }
            if self.session.status().is_empty() {
                self.session.say(before);
            }
        }
        assert!(self.queued.is_empty());
        assert!(
            self.said.len() <= 2 * QUEUED_MAX,
            "the RPC pump takes its answers"
        );
        changed
    }
    /// After a delivery: answers the waiting plugin commands whose event has finished,
    /// with the plugin's last line in `logs` (`<plugin>: <text>`, faults included) since the
    /// delivery that took the command, or an empty message. Exact for a plugin on a worker
    /// too: it counts the plugin's events, not whether it looks idle (one that gets an
    /// event every frame never does).
    pub(super) fn resolve_said(&mut self, logs: &[String]) {
        let plugins = &self.host.plugins;
        for s in self.said.iter_mut().filter(|s| s.status.is_none()) {
            let id = s.plugin.expect("only plugin commands wait");
            let Some((given, finished)) = plugins.event_counts(id) else {
                // Unloaded meanwhile (quarantined): the delivery's last word is about it.
                s.status = Some(
                    s.last
                        .take()
                        .or_else(|| logs.last().cloned())
                        .unwrap_or_default(),
                );
                continue;
            };
            let name = plugins.name(id).expect("loaded");
            let prefix = format!("{name}: ");
            if let Some(l) = logs.iter().rev().find(|l| l.starts_with(&prefix)) {
                s.last = Some(l.clone());
            }
            let target = *s.target.get_or_insert(given);
            if finished >= target {
                s.status = Some(s.last.take().unwrap_or_default());
            }
        }
    }
}
