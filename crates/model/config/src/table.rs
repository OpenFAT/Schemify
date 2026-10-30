//! The user-config table (config spec §6) and the request rows one Lua call returns: plain
//! data, written only by this crate's Lua callbacks (`api`, `pack`), read by both binaries.
//! Cites: config spec §3 (options, programs), §5 (package specs), §6 (capacities); plugin ABI
//! §6 (event words), §7 (keymap records); run spec §5 (programs); D33 (milli-units).
//!
//! Contents:
//! - `pub const KEYMAPS_MAX`, `pub const PACKS_MAX`, `pub const HOOKS_MAX`,
//!   `pub const REQUESTS_MAX`, `pub const SIM_ENV_MAX`, `pub const USER_LAYERS`
//! - the option domains: `pub(crate) const STEP_MIN`, `STEP_MAX`, `DIM_MAX`,
//!   `LINE_WIDTH_TENTHS_MIN`, `LINE_WIDTH_TENTHS_MAX`
//! - `pub struct Table`, `Table::sim(&self, kind: SimKind) -> Option<&Sim>`,
//!   `Table::assert_invariants(&self)`
//! - `pub struct Opts` (`impl Default`), `Opts::assert_in_domain(&self)`
//! - `pub struct FnId`, `pub enum Target`
//! - `pub struct Keymaps`, `Keymaps::records(&self) -> String`
//! - `pub enum SimKind`, `SimKind::ALL`, `SimKind::SIMULATORS`, `SimKind::word(self)`
//! - `pub struct Sim`
//! - `pub enum Source`, `Source::word(&self)`
//! - `pub struct PackSpec`
//! - `pub enum Event`, `Event::ALL`, `Event::word(self)`, `Event::from_word(word: &str)`
//! - `pub enum Level`, `pub enum Request`
//! - `pub fn fn_of_command(name: &str) -> Option<FnId>`

use std::path::PathBuf;

/// Most keymaps in one config.
pub const KEYMAPS_MAX: usize = 4096;
/// Most user packages.
pub const PACKS_MAX: usize = 128;
/// Most event hooks.
pub const HOOKS_MAX: usize = 1024;
/// Most requests one call may queue; the next one fails the call with an error request.
pub const REQUESTS_MAX: usize = 256;
/// Most `env` entries of one `schemify.sim` row.
pub const SIM_ENV_MAX: usize = 64;
/// Canvas layers a colour can be set for (`layer_colors`, xschem's 22 `cadlayers`).
pub const USER_LAYERS: usize = 22;
/// Smallest snap or grid step, milli-units (spec §3).
pub(crate) const STEP_MIN: i32 = 1;
/// Largest snap or grid step, milli-units (spec §3).
pub(crate) const STEP_MAX: i32 = 1_000_000_000;
/// Largest dimming, percent.
pub(crate) const DIM_MAX: u8 = 100;
/// Thinnest canvas line, tenths of a pixel.
pub(crate) const LINE_WIDTH_TENTHS_MIN: u8 = 1;
/// Widest canvas line, tenths of a pixel.
pub(crate) const LINE_WIDTH_TENTHS_MAX: u8 = 100;

/// The user-config table (spec §6). Rows are read whole, so each table is a `Vec` of rows
/// except the keymaps, whose columns the editor compares and renders separately.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    /// Options.
    pub opts: Opts,
    /// Keymaps, at most [`KEYMAPS_MAX`].
    pub keymaps: Keymaps,
    /// Program per [`SimKind`] (index = position in [`SimKind::ALL`]); `None` = the default.
    pub sims: [Option<Sim>; SimKind::ALL.len()],
    /// Package specs in declaration order, names unique, at most [`PACKS_MAX`].
    pub packs: Vec<PackSpec>,
    /// Event hooks in registration order, at most [`HOOKS_MAX`].
    pub hooks: Vec<(Event, FnId)>,
}

/// Options (`schemify.opt.<key>`, spec §3), in the table's units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opts {
    /// Snap step, milli-units in `[1, 10^9]`.
    pub snap: i32,
    /// Grid step, milli-units in `[1, 10^9]`.
    pub grid: i32,
    /// Draw the grid.
    pub show_grid: bool,
    /// Light canvas colours.
    pub light: bool,
    /// Canvas colour `0xRRGGBB` of each layer drawn over the theme's (`layer_colors`);
    /// `None` keeps the theme's colour.
    pub layer_colors: [Option<u32>; USER_LAYERS],
    /// Dimming toward the canvas background, percent `[0, 100]` (a factor in `[0, 1]` in Lua).
    pub dim: u8,
    /// Canvas line width in tenths of a pixel `[1, 100]` (pixels in Lua).
    pub line_width_tenths: u8,
    /// The simulator Simulate and `schemify run` start when neither the request nor the PDK
    /// names one (run spec §5): one of [`SimKind::SIMULATORS`], never a compiler. `None` lets
    /// detection choose.
    pub simulator: Option<SimKind>,
}

/// A Lua function kept by the runtime (a keymap target or a hook); valid for the life of the
/// [`Config`](crate::Config) that returned it. `u16`: keymaps + hooks < 2^13.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FnId(pub u16);

/// What a keymap runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A command of the D27 table.
    Command(String),
    /// A Lua function: the editor runs it through [`Config::call`](crate::Config::call).
    Function(FnId),
}

/// Keymap rows in `set` order, one row per chord: a later `set` of a chord replaces its row
/// in place. The three columns always have the same length.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keymaps {
    /// Chord in the keymap record syntax (`Ctrl+Shift+N`).
    pub chord: Vec<String>,
    /// Bound command or function.
    pub target: Vec<Target>,
    /// `opts.desc`, empty when absent.
    pub desc: Vec<String>,
}

/// The external programs of run spec §5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimKind {
    /// The ngspice command line.
    Ngspice,
    /// The Verilog-A compiler (ngspice's OSDI route).
    Openvaf,
    /// The Verilog compiler.
    Iverilog,
    /// The ESPice command line (it compiles Verilog-A itself, run spec §5).
    Espice,
    /// The Xyce command line.
    Xyce,
    /// The LTspice command line (natively or a Wine wrapper).
    Ltspice,
    /// The Spectre command line.
    Spectre,
}

/// One `schemify.sim` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sim {
    /// Program (on `PATH` when it has no directory part); never empty.
    pub path: String,
    /// Arguments (`{netlist}`, `{raw}` placeholders for the simulators).
    pub args: Vec<String>,
    /// Variables set for the program on top of the inherited environment (ESPice's `ZIG`),
    /// sorted by name, names unique, at most [`SIM_ENV_MAX`].
    pub env: Vec<(String, String)>,
}

/// Where a package comes from (spec §5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// `src`: a git repository; `rev` from `rev`/`version`, `None` = the default branch.
    Git {
        /// Anything `git clone` accepts; a local path is already expanded.
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
    /// `path`: a local package directory used in place, expanded and cleaned of `.` segments.
    Path(PathBuf),
}

/// One `schemify.pack.add` spec, validated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackSpec {
    /// One word of `A–Z a–z 0–9 . _ -`, at most 64 bytes, not `.` or `..`.
    pub name: String,
    /// Source.
    pub source: Source,
    /// Capabilities the user grants (plugin ABI §5).
    pub grant: Vec<String>,
    /// Per-event fuel override in `[10^6, 10^11]`.
    pub fuel: Option<u64>,
    /// Commands that load the plugin on first use; empty with `lazy_event` = eager.
    pub lazy_cmd: Vec<String>,
    /// Events that load the plugin on first delivery.
    pub lazy_event: Vec<Event>,
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

/// What one Lua call asks the binary to do, in call order; at most [`REQUESTS_MAX`] plus the
/// one error that ends a failed call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// `schemify.cmd(name, args)`.
    Command {
        /// Command name: one word of at most 128 bytes.
        name: String,
        /// Argument string.
        args: String,
    },
    /// `schemify.notify`, `print`, or an error (`file:line: message`) at [`Level::Error`].
    Notify {
        /// Level.
        level: Level,
        /// Text.
        text: String,
    },
}

/// Prefix of the command names that stand for Lua-function keymaps (`user.lua.<id>`).
const FN_COMMAND: &str = "user.lua.";

// Each `ALL` lists every variant once, in declaration order, so `kind as usize` indexes it.
const _: () = assert!(SimKind::ALL.len() == SimKind::Spectre as usize + 1);
const _: () = assert!(Event::ALL.len() == Event::RunDone as usize + 1);
const _: () = assert!(SimKind::SIMULATORS.len() + 2 == SimKind::ALL.len());

/// The default options (spec §3); [`Opts::default`] returns them.
const OPTS_DEFAULT: Opts = Opts {
    snap: 10_000,
    grid: 20_000,
    show_grid: true,
    light: false,
    layer_colors: [None; USER_LAYERS],
    dim: 0,
    line_width_tenths: 10,
    simulator: None,
};

// The defaults sit inside the domains `schemify.opt` enforces (spec §3).
const _: () = assert!(OPTS_DEFAULT.snap >= STEP_MIN);
const _: () = assert!(OPTS_DEFAULT.snap <= STEP_MAX);
const _: () = assert!(OPTS_DEFAULT.grid >= STEP_MIN);
const _: () = assert!(OPTS_DEFAULT.grid <= STEP_MAX);
const _: () = assert!(OPTS_DEFAULT.line_width_tenths >= LINE_WIDTH_TENTHS_MIN);
const _: () = assert!(OPTS_DEFAULT.line_width_tenths <= LINE_WIDTH_TENTHS_MAX);

/// The function a `user.lua.<id>` command name ([`Keymaps::records`]) stands for.
#[must_use]
pub fn fn_of_command(name: &str) -> Option<FnId> {
    let id: u16 = name.strip_prefix(FN_COMMAND)?.parse().ok()?;
    Some(FnId(id))
}

impl Table {
    /// The configured program of `kind`, if any.
    #[must_use]
    pub fn sim(&self, kind: SimKind) -> Option<&Sim> {
        self.sims[kind as usize].as_ref()
    }

    /// Asserts what every table this crate hands out holds: the capacities, the keymap
    /// columns in step, the option domains, sorted `env` rows, unique package names. The
    /// partner of the checks each `schemify.*` writer makes before it appends a row.
    pub(crate) fn assert_invariants(&self) {
        let keymaps = &self.keymaps;
        assert_eq!(keymaps.chord.len(), keymaps.target.len());
        assert_eq!(keymaps.chord.len(), keymaps.desc.len());
        assert!(keymaps.chord.len() <= KEYMAPS_MAX);
        assert!(self.packs.len() <= PACKS_MAX);
        assert!(self.hooks.len() <= HOOKS_MAX);
        self.opts.assert_in_domain();
        for sim in self.sims.iter().flatten() {
            assert!(!sim.path.is_empty());
            assert!(sim.env.len() <= SIM_ENV_MAX);
            assert!(
                sim.env.windows(2).all(|pair| pair[0].0 < pair[1].0),
                "`env` rows are sorted by name and the names are unique"
            );
        }
        for (i, pack) in self.packs.iter().enumerate() {
            let earlier = &self.packs[..i];
            assert!(
                earlier.iter().all(|other| other.name != pack.name),
                "a second spec of package `{}` replaces its row",
                pack.name
            );
        }
        // O(n log n) over up to `KEYMAPS_MAX` chords: checked in debug builds only.
        if cfg!(debug_assertions) {
            let mut chords: Vec<&str> = keymaps.chord.iter().map(String::as_str).collect();
            chords.sort_unstable();
            debug_assert!(
                chords.windows(2).all(|pair| pair[0] != pair[1]),
                "a second `set` of a chord replaces its row"
            );
        }
    }
}

impl Opts {
    /// Asserts each option lies in the domain `schemify.opt` lets through (spec §3).
    pub(crate) fn assert_in_domain(&self) {
        assert!(
            (STEP_MIN..=STEP_MAX).contains(&self.snap),
            "snap {}",
            self.snap
        );
        assert!(
            (STEP_MIN..=STEP_MAX).contains(&self.grid),
            "grid {}",
            self.grid
        );
        assert!(self.dim <= DIM_MAX);
        assert!(self.line_width_tenths >= LINE_WIDTH_TENTHS_MIN);
        assert!(self.line_width_tenths <= LINE_WIDTH_TENTHS_MAX);
        if let Some(kind) = self.simulator {
            assert!(
                SimKind::SIMULATORS.contains(&kind),
                "`{}` is a compiler, never the default simulator",
                kind.word()
            );
        }
    }
}

impl Default for Opts {
    fn default() -> Self {
        OPTS_DEFAULT
    }
}

impl Keymaps {
    /// The `keymap` contribution text (plugin ABI §7): one `key <chord> <command>` line per
    /// row; a function row names `user.lua.<id>`, which [`fn_of_command`] maps back.
    ///
    /// # Panics
    ///
    /// The columns differ in length (a bug in the writer).
    #[must_use]
    pub fn records(&self) -> String {
        use std::fmt::Write as _;
        assert_eq!(self.chord.len(), self.target.len());
        assert_eq!(self.chord.len(), self.desc.len());
        let mut out = String::new();
        let rows = self.chord.iter().zip(&self.target);
        for (chord, target) in rows {
            match target {
                Target::Command(c) => writeln!(out, "key {chord} {c}"),
                Target::Function(f) => writeln!(out, "key {chord} {FN_COMMAND}{}", f.0),
            }
            .expect("writing to a String");
        }
        assert_eq!(out.matches('\n').count(), self.chord.len());
        out
    }
}

impl SimKind {
    /// Every kind, in table order (the index of [`Table::sims`]).
    pub const ALL: [SimKind; 7] = [
        SimKind::Ngspice,
        SimKind::Openvaf,
        SimKind::Iverilog,
        SimKind::Espice,
        SimKind::Xyce,
        SimKind::Ltspice,
        SimKind::Spectre,
    ];

    /// The simulators: every kind but the compilers.
    pub const SIMULATORS: [SimKind; 5] = [
        SimKind::Ngspice,
        SimKind::Espice,
        SimKind::Xyce,
        SimKind::Ltspice,
        SimKind::Spectre,
    ];

    /// The `schemify.sim` name.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            SimKind::Ngspice => "ngspice",
            SimKind::Openvaf => "openvaf",
            SimKind::Iverilog => "iverilog",
            SimKind::Espice => "espice",
            SimKind::Xyce => "xyce",
            SimKind::Ltspice => "ltspice",
            SimKind::Spectre => "spectre",
        }
    }
}

impl Source {
    /// The lockfile `kind` word.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Source::Git { .. } => "git",
            Source::Url { .. } => "url",
            Source::Path(_) => "path",
        }
    }
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
    #[must_use]
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

    /// The event a wire word names; `None` for any other word.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Event> {
        Event::ALL.into_iter().find(|e| e.word() == word)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round trips: every word maps back to its value, and a foreign word to nothing.
    #[test]
    fn words_round_trip() {
        let events = Event::ALL.into_iter().enumerate();
        for (i, e) in events {
            assert_eq!(e as usize, i);
            assert_eq!(Event::from_word(e.word()), Some(e));
        }
        assert_eq!(Event::from_word("Load"), None);
        let kinds = SimKind::ALL.into_iter().enumerate();
        for (i, k) in kinds {
            assert_eq!(k as usize, i);
        }
        let keys = Keymaps {
            chord: vec!["F5".into(), "w".into()],
            target: vec![Target::Function(FnId(42)), Target::Command("x.y".into())],
            desc: vec![String::new(), "d".into()],
        };
        let text = keys.records();
        assert_eq!(text, "key F5 user.lua.42\nkey w x.y\n");
        let cmd = text.lines().next().unwrap().rsplit(' ').next().unwrap();
        assert_eq!(fn_of_command(cmd), Some(FnId(42)));
        for bad in [
            "user.lua.",
            "user.lua.x",
            "user.lua.65536",
            "x.y",
            "user.lua",
        ] {
            assert_eq!(fn_of_command(bad), None, "{bad}");
        }
    }
}
