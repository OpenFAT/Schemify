//! The user config (D9 and D35 as amended 2026-10-11; normative spec `docs/spec/config.md`):
//! Neovim-style directories ([`dirs`]), the sandboxed Lua 5.4 runtime that runs `init.lua`,
//! and the user-config [`Table`] it fills.
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
//! # #[cfg(not(target_arch = "wasm32"))] {
//! assert_eq!(cfg.table().opts.snap, 5_000);
//! assert_eq!(cfg.table().keymaps.target[0], Target::Command("simulation.simulate".into()));
//! # }
//! ```
//!
//! Data design (six questions):
//! 1. In: the init file and the `lua/` modules it requires, the startup flags. Out: the
//!    [`Table`] (options, keymaps, sims, package specs, hooks) and, per Lua call, a list of
//!    [`Request`]s (queued commands, notifications, errors).
//! 2. One config per process. Keymaps ≤ [`KEYMAPS_MAX`], packages ≤ [`PACKS_MAX`], hooks ≤
//!    [`HOOKS_MAX`], three sims, seven options; ≤ [`REQUESTS_MAX`] requests per call.
//! 3. Steps are i32 milli-units (D33) in `[1, 10^9]`; a Lua function is a [`FnId`] (u16:
//!    keymaps + hooks < 2^13); `fuel` u64 in `[10^6, 10^11]` (plugin ABI §2).
//! 4. The binaries read the table at startup (the editor re-reads keymaps when they change);
//!    Lua callbacks append rows during a call. Rows are read whole, so they are AoS.
//! 5. Lifetime: the process; requests live for one call.
//! 6. Not parallel: one Lua state on the caller's thread.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

pub mod dirs;
#[cfg(not(target_arch = "wasm32"))]
mod lua;

/// Most keymaps in one config.
pub const KEYMAPS_MAX: usize = 4096;
/// Most user packages.
pub const PACKS_MAX: usize = 128;
/// Most event hooks.
pub const HOOKS_MAX: usize = 1024;
/// Most requests one call may queue; later ones are dropped with an error request.
pub const REQUESTS_MAX: usize = 256;
/// Lua VM instructions per call (spec §4).
pub const BUDGET: u64 = 100_000_000;
/// Bytes the Lua state may hold (spec §4).
pub const MEMORY_MAX: usize = 64 << 20;
/// Canvas layers a colour can be set for (`layer_colors`, xschem's 22 `cadlayers`).
pub const USER_LAYERS: usize = 22;

/// Where the user config comes from (`--clean`, the default, `-u <file>`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Startup {
    /// No init file and no user packages (`--clean`). The default, so tests and embedders
    /// never read the developer's own config by accident.
    #[default]
    Clean,
    /// `<config>/init.lua` when it exists.
    User,
    /// `-u <file>`.
    File(PathBuf),
}

impl Startup {
    /// Splits the global flags (`--clean`, `-u <file>`) off the front of `args`; returns the
    /// startup and the index of the first other argument. A binary with no flags gets
    /// [`Startup::User`].
    ///
    /// # Errors
    ///
    /// `-u` without a file, or both `--clean` and `-u`.
    pub fn parse(args: &[String]) -> Result<(Startup, usize), String> {
        let mut start = Startup::User;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--clean" if start == Startup::User => start = Startup::Clean,
                "-u" if start == Startup::User => {
                    let f = args.get(i + 1).ok_or("-u needs a file")?;
                    start = Startup::File(PathBuf::from(f));
                    i += 1;
                }
                "--clean" | "-u" => return Err("give --clean or -u once".into()),
                _ => break,
            }
            i += 1;
        }
        assert!(i <= args.len());
        Ok((start, i))
    }

    /// The flags that reproduce this startup (forwarded by `schemify edit` to the editor).
    pub fn flags(&self) -> Vec<String> {
        match self {
            Startup::Clean => vec!["--clean".into()],
            Startup::User => Vec::new(),
            Startup::File(p) => vec!["-u".into(), p.display().to_string()],
        }
    }
}

/// Options (`schemify.opt.<key>`, spec §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opts {
    /// Snap step, milli-units.
    pub snap: i32,
    /// Grid step, milli-units.
    pub grid: i32,
    /// Draw the grid.
    pub show_grid: bool,
    /// Light canvas colours.
    pub light: bool,
    /// Canvas colour `0xRRGGBB` of each layer `[0, USER_LAYERS)` drawn over the theme's
    /// (`layer_colors`); `None` keeps the theme's colour.
    pub layer_colors: [Option<u32>; USER_LAYERS],
    /// Dimming of the canvas toward its background, percent `[0, 100]` (`dim`, a factor in
    /// `[0, 1]` in Lua).
    pub dim: u8,
    /// Canvas line width in tenths of a pixel `[1, 100]` (`line_width`, pixels in Lua).
    pub line_width_tenths: u8,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            snap: 10_000,
            grid: 20_000,
            show_grid: true,
            light: false,
            layer_colors: [None; USER_LAYERS],
            dim: 0,
            line_width_tenths: 10,
        }
    }
}

/// A Lua function kept by the runtime (a keymap target or a hook); valid for the life of the
/// [`Config`] that returned it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FnId(pub u16);

/// What a keymap runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A command of the D27 table.
    Command(String),
    /// A Lua function: the editor runs it through [`Config::call`].
    Function(FnId),
}

/// Keymap rows in `set` order; a later `set` of a chord replaced the earlier row in place.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keymaps {
    /// Chord in the keymap record syntax (`Ctrl+Shift+N`).
    pub chord: Vec<String>,
    /// Bound command or function.
    pub target: Vec<Target>,
    /// `opts.desc`, empty when absent.
    pub desc: Vec<String>,
}

impl Keymaps {
    /// The `keymap` contribution text (plugin ABI §7): one `key <chord> <command>` record per
    /// row; a function row names the command [`fn_command`] of its id.
    pub fn records(&self) -> String {
        let mut out = String::new();
        for (chord, target) in self.chord.iter().zip(&self.target) {
            let cmd = match target {
                Target::Command(c) => c.clone(),
                Target::Function(f) => fn_command(*f),
            };
            out.push_str(&format!("key {chord} {cmd}\n"));
        }
        out
    }
}

/// Prefix of the command names that stand for Lua-function keymaps (`user.lua.<id>`).
pub const FN_COMMAND: &str = "user.lua.";

/// The command name a function keymap is bound to.
pub fn fn_command(f: FnId) -> String {
    format!("{FN_COMMAND}{}", f.0)
}

/// The function a `user.lua.<id>` command name stands for.
pub fn fn_of_command(name: &str) -> Option<FnId> {
    name.strip_prefix(FN_COMMAND)?.parse().ok().map(FnId)
}

/// The external programs of run spec §5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimKind {
    /// The simulator command line.
    Ngspice,
    /// The Verilog-A compiler.
    Openvaf,
    /// The Verilog compiler.
    Iverilog,
}

impl SimKind {
    /// Every kind, in table order.
    pub const ALL: [SimKind; 3] = [SimKind::Ngspice, SimKind::Openvaf, SimKind::Iverilog];

    /// The `schemify.sim` name.
    pub fn word(self) -> &'static str {
        match self {
            SimKind::Ngspice => "ngspice",
            SimKind::Openvaf => "openvaf",
            SimKind::Iverilog => "iverilog",
        }
    }
}

/// One `schemify.sim` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sim {
    /// Program (on `PATH` when it has no directory part).
    pub path: String,
    /// Arguments (`{netlist}`, `{raw}` placeholders for ngspice).
    pub args: Vec<String>,
}

/// Where a package comes from (spec §5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// `src`: a git repository; `rev` from `rev`/`version`, `None` = the default branch.
    Git {
        /// Anything `git clone` accepts.
        url: String,
        /// Branch, tag or commit.
        rev: Option<String>,
    },
    /// `url` + `sha256`: one `.wasm` file.
    Url {
        /// Fetched with `curl`.
        url: String,
        /// 64 lowercase hex digits.
        sha256: String,
    },
    /// `path`: a local package directory, used in place.
    Path(PathBuf),
}

impl Source {
    /// The lockfile `kind` word.
    pub fn word(&self) -> &'static str {
        match self {
            Source::Git { .. } => "git",
            Source::Url { .. } => "url",
            Source::Path(_) => "path",
        }
    }
}

/// One `schemify.pack.add` spec, validated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackSpec {
    /// One word of `A–Z a–z 0–9 . _ -`.
    pub name: String,
    /// Source.
    pub source: Source,
    /// Capabilities the user grants (plugin ABI §5).
    pub grant: Vec<String>,
    /// Per-event fuel override.
    pub fuel: Option<u64>,
    /// Commands that load the plugin on first use; empty with `lazy_event` = eager.
    pub lazy_cmd: Vec<String>,
    /// Events that load the plugin on first delivery.
    pub lazy_event: Vec<Event>,
}

impl PackSpec {
    /// Whether the plugin waits for a command or event instead of loading at startup.
    pub fn is_lazy(&self) -> bool {
        !self.lazy_cmd.is_empty() || !self.lazy_event.is_empty()
    }
}

/// Events a hook or a lazy package can wait for (plugin ABI §6 wire words).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The editor is ready.
    Load,
    /// A document opened.
    DocOpened,
    /// A document closed.
    DocClosed,
    /// A document changed.
    DocChanged,
    /// The selection changed.
    SelectionChanged,
    /// A document was saved.
    Saved,
    /// A run job finished.
    RunDone,
}

impl Event {
    /// Every event.
    pub const ALL: [Event; 7] = [
        Event::Load,
        Event::DocOpened,
        Event::DocClosed,
        Event::DocChanged,
        Event::SelectionChanged,
        Event::Saved,
        Event::RunDone,
    ];

    /// The wire word.
    pub fn word(self) -> &'static str {
        match self {
            Event::Load => "load",
            Event::DocOpened => "doc_opened",
            Event::DocClosed => "doc_closed",
            Event::DocChanged => "doc_changed",
            Event::SelectionChanged => "selection_changed",
            Event::Saved => "saved",
            Event::RunDone => "run_done",
        }
    }

    /// The event a wire word names.
    pub fn from_word(w: &str) -> Option<Event> {
        Event::ALL.into_iter().find(|e| e.word() == w)
    }
}

/// The user-config table (spec §6). Written only by this crate's Lua callbacks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    /// Options.
    pub opts: Opts,
    /// Keymaps.
    pub keymaps: Keymaps,
    /// Program per [`SimKind`] (index = position in [`SimKind::ALL`]); `None` = the default.
    pub sims: [Option<Sim>; 3],
    /// Package specs, in declaration order; names unique.
    pub packs: Vec<PackSpec>,
    /// Event hooks in registration order.
    pub hooks: Vec<(Event, FnId)>,
}

impl Table {
    /// The configured program of `kind`, if any.
    pub fn sim(&self, kind: SimKind) -> Option<&Sim> {
        self.sims[kind as usize].as_ref()
    }
}

/// How loud a notification is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// An error: config errors are reported at this level.
    Error,
    /// A warning.
    Warn,
    /// Information.
    Info,
}

/// What one Lua call asks the binary to do, in call order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// `schemify.cmd(name, args)`.
    Command {
        /// Command name.
        name: String,
        /// Argument string.
        args: String,
    },
    /// `schemify.notify`, or an error (`file:line: message`) at [`Level::Error`].
    Notify {
        /// Level.
        level: Level,
        /// Text.
        text: String,
    },
}

/// The loaded user config: the table plus the Lua state that filled it.
pub struct Config {
    table: Table,
    root: Option<PathBuf>,
    #[cfg(not(target_arch = "wasm32"))]
    lua: Option<lua::Runtime>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("root", &self.root)
            .field("table", &self.table)
            .finish()
    }
}

impl Config {
    /// A config with nothing set (`--clean`).
    pub fn empty() -> Config {
        Config {
            table: Table::default(),
            root: None,
            #[cfg(not(target_arch = "wasm32"))]
            lua: None,
        }
    }

    /// Runs the init file `start` names (spec §2) and returns the config with the requests
    /// the run queued; errors are [`Level::Error`] notifications, never fatal. Reads the
    /// init file and the `lua/` modules it requires; nothing else.
    ///
    /// Cost: one Lua state (≈ 100 µs) plus the script, bounded by [`BUDGET`] instructions.
    pub fn load(start: &Startup) -> (Config, Vec<Request>) {
        let file = match start {
            Startup::Clean => return (Config::empty(), Vec::new()),
            Startup::File(p) => p.clone(),
            Startup::User => match dirs::here(dirs::Dir::Config) {
                Ok(dir) => {
                    let f = dir.join("init.lua");
                    if !f.is_file() {
                        let mut c = Config::empty();
                        c.root = Some(dir);
                        return (c, Vec::new());
                    }
                    f
                }
                Err(e) => return (Config::empty(), vec![error(format!("user config: {e}"))]),
            },
        };
        match std::fs::read_to_string(&file) {
            Ok(src) => Config::run(&src, &file),
            Err(e) => (
                Config::empty(),
                vec![error(format!("{}: {e}", file.display()))],
            ),
        }
    }

    /// Runs `src` as the init file of the config root `root` (`require` reads `<root>/lua/`;
    /// `None` = no modules). For tests and embedders; [`Config::load`] reads the file.
    pub fn from_source(src: &str, root: Option<&Path>) -> (Config, Vec<Request>) {
        let file = root.map_or_else(|| PathBuf::from("init.lua"), |r| r.join("init.lua"));
        let (mut c, out) = Config::run(src, &file);
        c.root = root.map(Path::to_path_buf);
        (c, out)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn run(src: &str, file: &Path) -> (Config, Vec<Request>) {
        let root = file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf);
        let name = file.file_name().map_or_else(
            || "init.lua".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        let (rt, table, out) = lua::Runtime::start(src, &name, root.as_deref());
        let c = Config {
            table,
            root,
            lua: rt,
        };
        (c, out)
    }

    // ponytail: no Lua on wasm32-unknown-unknown (C Lua needs a libc); the web runs without
    // a user config. Upgrade: a pure-Rust Lua (piccolo) once it covers Lua 5.4, or C Lua on
    // a wasi-libc shim (BACKLOG).
    #[cfg(target_arch = "wasm32")]
    fn run(_: &str, file: &Path) -> (Config, Vec<Request>) {
        let note = format!("{}: no user config on the web yet", file.display());
        (Config::empty(), vec![error(note)])
    }

    /// The user-config table.
    pub fn table(&self) -> &Table {
        &self.table
    }

    /// The config root (the init file's directory): the package lockfile lives here.
    /// `None` with `--clean` or when no config directory is defined.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Calls function `f` (a keymap target) with no arguments under the per-call budget.
    /// Returns its requests; a failure is one [`Level::Error`] request. The table may change
    /// (a function can set keymaps).
    pub fn call(&mut self, f: FnId) -> Vec<Request> {
        self.invoke(f, None, &[])
    }

    /// Runs every hook registered for `event`, in order, passing a table of `fields`
    /// (`ev.doc`, `ev.epoch`, `ev.job`). Returns the requests of all of them.
    pub fn fire(&mut self, event: Event, fields: &[(&str, i64)]) -> Vec<Request> {
        let hooks: Vec<FnId> = self
            .table
            .hooks
            .iter()
            .filter(|h| h.0 == event)
            .map(|h| h.1)
            .collect();
        let mut out = Vec::new();
        for f in hooks {
            out.extend(self.invoke(f, Some(event), fields));
        }
        out
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn invoke(&mut self, f: FnId, ev: Option<Event>, fields: &[(&str, i64)]) -> Vec<Request> {
        let Some(rt) = self.lua.as_mut() else {
            return vec![error(format!("function {} called without a config", f.0))];
        };
        let table = std::mem::take(&mut self.table);
        let (table, out) = rt.call(table, f, ev, fields);
        self.table = table;
        out
    }

    #[cfg(target_arch = "wasm32")]
    fn invoke(&mut self, f: FnId, _: Option<Event>, _: &[(&str, i64)]) -> Vec<Request> {
        vec![error(format!("function {} called without a config", f.0))]
    }
}

/// An error notification.
pub(crate) fn error(text: String) -> Request {
    Request::Notify {
        level: Level::Error,
        text,
    }
}
