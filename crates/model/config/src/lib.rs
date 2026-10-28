//! The user config (D9 and D35 as amended 2026-10-11; normative spec `docs/spec/config.md`):
//! the init file and its `lua/` modules → the user-config [`Table`] plus, per Lua call, a
//! list of [`Request`]s. Neovim-style directories live in [`dirs`], the command lines in
//! `cli`, the table in `table`, the sandboxed Lua 5.4 state in `lua`, the `schemify.*` API
//! in `api` and `pack`.
//!
//! This crate owns the table and is the only code that touches the Lua state. Both binaries
//! (`schemify`, `schemify-gui`) read [`Config::table`] and drain [`Request`]s; the editor
//! calls back into Lua only through [`Config::call`] and [`Config::fire`].
//!
//! ```
//! use schemify_config::{Config, Target};
//!
//! let (cfg, out) = Config::from_source(
//!     "schemify.opt.snap = 5\nschemify.keymap.set('F5', 'simulation.simulate')",
//!     None,
//! );
//! assert!(out.is_empty());
//! assert_eq!(cfg.table().opts.snap, 5_000);
//! assert_eq!(cfg.table().keymaps.target[0], Target::Command("simulation.simulate".into()));
//! ```
//!
//! Data design (six questions):
//! 1. In: the init file, the `lua/` modules it requires, the startup flags. Out: the
//!    [`Table`] (options, keymaps, sims, package specs, hooks) and, per Lua call, the
//!    [`Request`]s it queued (commands, notifications, errors).
//! 2. One config per process. Keymaps ≤ [`KEYMAPS_MAX`], packages ≤ [`PACKS_MAX`], hooks ≤
//!    [`HOOKS_MAX`], seven programs, eight options; ≤ [`REQUESTS_MAX`] requests per call.
//! 3. Steps are i32 milli-units (D33) in `[1, 10^9]`; a Lua function is a [`FnId`] (u16:
//!    keymaps + hooks < 2^13); `fuel` u64 in `[10^6, 10^11]` (plugin ABI §2).
//! 4. The binaries read the table at startup (the editor re-reads keymaps when they change);
//!    Lua callbacks write rows during a call. Rows are read whole, so they are AoS.
//! 5. Lifetime: the process; requests live for one call.
//! 6. Not parallel: one Lua state on the caller's thread.
//!
//! Contents:
//! - `pub mod dirs`; re-exports of `cli` ([`Startup`], [`Launch`], [`GUI_USAGE`]) and
//!   `table` ([`Table`] and its rows, the capacities, [`fn_of_command`])
//! - `pub const BUDGET: u64`, `pub const MEMORY_MAX: usize`
//! - `pub type ReadFile = fn(&Path) -> Option<String>`
//! - `pub struct Config`: `Config::load(start: &Startup)`, `Config::load_with(start, read)`,
//!   `Config::from_source(src: &str, root: Option<&Path>)`, `Config::table(&self)`,
//!   `Config::root(&self)`, `Config::call(&mut self, f: FnId)`,
//!   `Config::fire(&mut self, event: Event, fields: &[(&str, i64)])`

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

mod api;
mod cli;
pub mod dirs;
mod lua;
mod pack;
mod table;

pub use cli::{GUI_USAGE, Launch, Startup};
pub use table::{
    Event, FnId, HOOKS_MAX, KEYMAPS_MAX, Keymaps, Level, Opts, PACKS_MAX, PackSpec, REQUESTS_MAX,
    Request, SIM_ENV_MAX, Sim, SimKind, Source, Table, Target, USER_LAYERS, fn_of_command,
};

/// Lua VM instructions per call (spec §4).
pub const BUDGET: u64 = 100_000_000;
/// Bytes the Lua state may hold (spec §4).
pub const MEMORY_MAX: usize = 64 << 20;

/// Reads a config file (`init.lua`, a `lua/` module) as text; `None` when it is missing or
/// not UTF-8. The web build passes one over its storage (web spec §2); [`Config::load`]
/// reads through `std::fs`.
pub type ReadFile = fn(&Path) -> Option<String>;

/// The loaded user config: the table plus the Lua state that filled it.
pub struct Config {
    table: Table,
    root: Option<PathBuf>,
    /// `None` with no init file, or when the state could not be built.
    lua: Option<lua::Runtime>,
}

impl Config {
    /// Runs the init file `start` names (spec §2) and returns the config with the requests
    /// the run queued; errors are [`Level::Error`] notifications, never fatal. Reads the init
    /// file and the `lua/` modules it requires through `std::fs`, nothing else.
    ///
    /// Cost: one Lua state (a few ms) plus the script, bounded by [`BUDGET`] instructions.
    pub fn load(start: &Startup) -> (Config, Vec<Request>) {
        Config::load_with(start, read_std)
    }

    /// [`Config::load`] reading the init file and its modules through `read` (the web build's
    /// storage holds `/config/init.lua`). A missing `<config>/init.lua` is no error: the
    /// config is empty but keeps `<config>` as its [`root`](Config::root).
    pub fn load_with(start: &Startup, read: ReadFile) -> (Config, Vec<Request>) {
        // `dir`: the user config directory, where a missing init file is no error.
        let (file, dir) = match start {
            Startup::Clean => return (Config::empty(), Vec::new()),
            Startup::File(p) => (p.clone(), None),
            Startup::User => match dirs::here(dirs::Dir::Config) {
                Ok(dir) => (dir.join("init.lua"), Some(dir)),
                Err(e) => return (Config::empty(), vec![error(format!("user config: {e}"))]),
            },
        };
        match (read(&file), dir) {
            (Some(src), _) => Config::run(&src, &file, read),
            (None, Some(dir)) => {
                let mut c = Config::empty();
                c.root = Some(dir);
                (c, Vec::new())
            }
            (None, None) => (
                Config::empty(),
                vec![error(format!("{}: cannot read the file", file.display()))],
            ),
        }
    }

    /// Runs `src` as the init file of the config root `root` (`require` reads `<root>/lua/`
    /// through `std::fs`; `None` = no modules). For tests and embedders.
    pub fn from_source(src: &str, root: Option<&Path>) -> (Config, Vec<Request>) {
        let file = root.map_or_else(|| PathBuf::from("init.lua"), |r| r.join("init.lua"));
        let (mut c, out) = Config::run(src, &file, read_std);
        c.root = root.map(Path::to_path_buf);
        (c, out)
    }

    /// The user-config table.
    pub fn table(&self) -> &Table {
        &self.table
    }

    /// The config root (the init file's directory): the package lockfile lives here. `None`
    /// with `--clean` or when no config directory is defined.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Calls function `f` (a keymap target) with no arguments under a fresh [`BUDGET`].
    /// Returns its requests; a failure, or an `f` this config never returned, is one
    /// [`Level::Error`] request. The table may change (a function can set keymaps).
    pub fn call(&mut self, f: FnId) -> Vec<Request> {
        self.invoke(f, None, &[])
    }

    /// Runs every hook registered for `event`, in registration order, each under a fresh
    /// [`BUDGET`] with a table of `fields` (`ev.doc`, `ev.epoch`, `ev.job`) plus `ev.event`.
    /// Returns the requests of all of them; one hook's failure does not stop the next.
    pub fn fire(&mut self, event: Event, fields: &[(&str, i64)]) -> Vec<Request> {
        let hooks: Vec<FnId> = (self.table.hooks.iter())
            .filter(|h| h.0 == event)
            .map(|h| h.1)
            .collect();
        let mut out = Vec::new();
        for f in hooks {
            out.extend(self.invoke(f, Some(event), fields));
        }
        out
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("root", &self.root)
            .field("table", &self.table)
            .finish()
    }
}

// ---- private ----

/// [`ReadFile`] over `std::fs`.
fn read_std(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// An error notification.
fn error(text: String) -> Request {
    Request::Notify {
        level: Level::Error,
        text,
    }
}

impl Config {
    /// A config with nothing set (`--clean`).
    fn empty() -> Config {
        Config {
            table: Table::default(),
            root: None,
            lua: None,
        }
    }

    /// Runs init file `file` with text `src`; its directory is the config root.
    fn run(src: &str, file: &Path, read: ReadFile) -> (Config, Vec<Request>) {
        let root = (file.parent())
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf);
        let name = file.file_name().map_or_else(
            || "init.lua".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        let (lua, table, out) = lua::Runtime::start(src, &name, root.as_deref(), read);
        assert!(out.len() <= REQUESTS_MAX + 1);
        // No state, nothing ran: the table is empty and the one request says why.
        if lua.is_none() {
            assert_eq!(table, Table::default());
        }
        (Config { table, root, lua }, out)
    }

    /// Runs kept function `f`, with an event table when `ev` is given.
    fn invoke(&mut self, f: FnId, ev: Option<Event>, fields: &[(&str, i64)]) -> Vec<Request> {
        let Some(lua) = self.lua.as_mut() else {
            return vec![error(format!("function {} called without a config", f.0))];
        };
        let table = std::mem::take(&mut self.table);
        let (table, out) = lua.call(table, f, ev, fields);
        self.table = table;
        assert!(out.len() <= REQUESTS_MAX + 1);
        out
    }
}
