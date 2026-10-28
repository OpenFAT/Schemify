//! The user config's side of the host: its table, keymap and hook requests, user commands,
//! lazy packages woken by their command or event, and plugin functions called from
//! expressions.
//!
//! Contents:
//! - `Host::user`
//! - `Host::config`
//! - `Host::user_command`
//! - `Host::absorb`
//! - `Host::notes_since`
//! - `Host::notes_seq`
//! - `Host::user_event`
//! - `Host::is_lazy`
//! - `Host::call_fn`
//! - `Host::fn_hash`

use schemify_config::{Config, Request};
use schemify_elab::Val;
use schemify_expr::{Code as XCode, Error as XError, Span, Value};
use schemify_plugin::PluginId;
use schemify_plugin::abi::Event;

use super::deliver::fn_result;
use super::load::{content_hash, load_installed};
use super::{Host, NOTES_MAX, Note};

impl Host {
    /// The user-config table (options, keymaps, sims, packages, hooks).
    #[must_use]
    pub fn user(&self) -> &schemify_config::Table {
        self.user.table()
    }

    /// The user config itself: its root locates the package lockfile (`pack::Paths::of`).
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.user
    }

    /// Runs a command nobody in the command table owns, if the user config or a lazy package
    /// does: a Lua keymap function (`user.lua.<id>`) runs now; a lazy package listing `name`
    /// loads and gets the command as `Event::Command`. Returns whether it was handled.
    ///
    /// # Panics
    ///
    /// Never: the lazy list holds at most `schemify_config::PACKS_MAX` packages and loses one here.
    pub fn user_command(&mut self, name: &str, args: &str) -> bool {
        if let Some(f) = schemify_config::fn_of_command(name) {
            let keys = self.user.table().keymaps.clone();
            let out = self.user.call(f);
            self.absorb(out, &keys);
            return true;
        }
        let Some(i) = self
            .lazy
            .iter()
            .position(|p| p.lazy_cmd.iter().any(|c| c == name))
        else {
            return false;
        };
        let p = self.lazy.remove(i);
        let mut notes = Vec::new();
        match load_installed(&mut self.plugins, &mut self.hashes, &mut notes, &p) {
            Some(id) => {
                let ev = Event::Command { name, args };
                if let Err(e) = self.push(Some(id), &ev) {
                    notes.push(e);
                }
            }
            None => notes.push(format!("{name}: its package did not load")),
        }
        self.user_logs.extend(notes);
        assert!(self.lazy.len() < schemify_config::PACKS_MAX);
        true
    }

    /// Queues the requests of a user-config call; recomposes the UI when the call changed the
    /// keymaps (`keys` is the table before it; a function may call `schemify.keymap.set`).
    pub(super) fn absorb(&mut self, out: Vec<Request>, keys: &schemify_config::Keymaps) {
        for r in out {
            match r {
                Request::Command { name, args } => self.user_cmds.push((name, args)),
                Request::Notify { text, .. } => self.user_logs.push(text),
            }
        }
        if *keys != self.user.table().keymaps {
            self.seen_epoch = None;
        }
    }

    /// The editor events recorded after sequence number `seq` (a value [`Host::notes_seq`]
    /// returned), oldest first, as plugin-abi §6 wire text without the line end; only the
    /// last [`NOTES_MAX`] are kept.
    pub fn notes_since(&self, seq: u64) -> impl Iterator<Item = &Note> {
        let new = self
            .notes_seq
            .saturating_sub(seq)
            .min(self.notes.len() as u64) as usize;
        self.notes.iter().skip(self.notes.len() - new)
    }

    /// The sequence number of the newest recorded editor event (0 before the first).
    #[must_use]
    pub fn notes_seq(&self) -> u64 {
        self.notes_seq
    }

    /// The user side of an editor event: records it for the command stream, loads the lazy
    /// packages that wait for it, then runs the user's hooks.
    pub(super) fn user_event(&mut self, ev: &Event<'_>) {
        let (event, fields): (schemify_config::Event, Vec<(&str, i64)>) = match *ev {
            Event::DocOpened { doc } => {
                (schemify_config::Event::DocOpened, vec![("doc", doc.into())])
            }
            Event::DocClosed { doc } => {
                (schemify_config::Event::DocClosed, vec![("doc", doc.into())])
            }
            Event::DocChanged { doc, epoch } => (
                schemify_config::Event::DocChanged,
                vec![("doc", doc.into()), ("epoch", epoch.into())],
            ),
            Event::SelectionChanged => (schemify_config::Event::SelectionChanged, Vec::new()),
            Event::Saved { doc } => (schemify_config::Event::Saved, vec![("doc", doc.into())]),
            Event::RunDone { job } => (schemify_config::Event::RunDone, vec![("job", job.into())]),
            _ => return,
        };
        assert!(fields.len() <= 2);
        let mut note = Note {
            event,
            values: [0; 2],
            len: fields.len() as u8,
        };
        for (slot, &(_, v)) in note.values.iter_mut().zip(&fields) {
            *slot = u32::try_from(v).expect("event fields are u32");
        }
        if self.notes.len() == NOTES_MAX {
            self.notes.pop_front();
        }
        self.notes.push_back(note);
        self.notes_seq += 1;
        debug_assert!(self.notes.len() <= NOTES_MAX);
        self.wake_lazy(event);
        if self.user.table().hooks.iter().any(|h| h.0 == event) {
            let keys = self.user.table().keymaps.clone();
            let out = self.user.fire(event, &fields);
            self.absorb(out, &keys);
        }
    }

    /// Loads every lazy package waiting for `event` (its `Load` is delivered before the
    /// event that woke it, plugin-abi §6).
    fn wake_lazy(&mut self, event: schemify_config::Event) {
        let mut i = 0;
        while i < self.lazy.len() {
            if !self.lazy[i].lazy_event.contains(&event) {
                i += 1;
                continue;
            }
            let p = self.lazy.remove(i);
            let mut notes = Vec::new();
            load_installed(&mut self.plugins, &mut self.hashes, &mut notes, &p);
            self.user_logs.extend(notes);
        }
    }

    /// Whether a user package is still waiting to load (tests, status).
    #[must_use]
    pub fn is_lazy(&self, name: &str) -> bool {
        self.lazy.iter().any(|p| p.name == name)
    }

    /// The D20 function registry (`elab::Funcs`): calls `ns.f` through the composed
    /// `functions` contributions and `schemify_call`. `None` when no loaded plugin exports
    /// `name`, or its plugin's event is running on a worker (elab then falls back to the
    /// record's `cached` value, unverified, until the next recompute).
    ///
    /// Args travel as line-record values (strings quoted, numbers canonical); a number,
    /// `true`/`false` or a quoted string comes back ([`fn_result`]). Failures map to
    /// expression errors: wrong arity `X008`, a complex argument or a result that is none of
    /// those `X003`, a plugin error code `X010`.
    pub fn call_fn(&mut self, name: &str, args: &[Value<'_>]) -> Option<Result<Val, XError>> {
        let f = &self.plugins.composed().functions;
        let row = f.find(name)?;
        let (plugin, fn_id, arity) = (f.plugin[row], f.fn_id[row], f.arity[row]);
        if self.plugins.busy(plugin) {
            // Its event runs on a worker: the record's `cached` value stands in (D20).
            return None;
        }
        let err = |code, msg: String| Some(Err(XError::new(code, Span::default(), msg)));
        if args.len() != usize::from(arity) {
            return err(
                XCode::X008,
                format!("{name} takes {arity} arguments, got {}", args.len()),
            );
        }
        self.args.clear();
        for (k, a) in args.iter().enumerate() {
            if k > 0 {
                self.args.push(' ');
            }
            match *a {
                Value::Num(x) => schemify_expr::write_num(&mut self.args, x),
                Value::Int(i) => schemify_expr::write_int(&mut self.args, i),
                Value::Bool(b) => self.args.push_str(if b { "true" } else { "false" }),
                Value::Str(s) => self.args.push_str(&crate::docs::quote(s)),
                Value::Cnum { .. } => {
                    return err(
                        XCode::X003,
                        format!("{name}: complex arguments unsupported"),
                    );
                }
            }
        }
        match self
            .plugins
            .call(plugin, fn_id, &self.args, &mut self.result)
        {
            Ok(()) => Some(fn_result(self.result.trim()).ok_or_else(|| {
                let t = self.result.trim();
                XError::new(
                    XCode::X003,
                    Span::default(),
                    format!("{name} returned `{t}`: not a number, bool or quoted string"),
                )
            })),
            Err(code) => err(XCode::X010, format!("{name}: plugin reports {code:?}")),
        }
    }

    /// D20 cache hash of an expression calling `fns` (sorted, distinct `ns.f` names): the
    /// content hash of the one plugin serving them, or of their hashes joined when several
    /// do. `None` when any function is not loaded.
    #[must_use]
    pub fn fn_hash(&self, fns: &[String]) -> Option<String> {
        let f = &self.plugins.composed().functions;
        let mut owners: Vec<PluginId> = Vec::with_capacity(fns.len());
        for name in fns {
            owners.push(f.plugin[f.find(name)?]);
        }
        owners.sort_unstable();
        owners.dedup();
        let hashes: Vec<&str> = owners
            .iter()
            .filter_map(|id| self.hashes.iter().find(|(h, _)| h == id))
            .map(|(_, h)| h.as_str())
            .collect();
        match hashes.as_slice() {
            [] => None,
            [one] => Some((*one).to_owned()),
            many => Some(content_hash(many.join("").as_bytes())),
        }
    }
}
