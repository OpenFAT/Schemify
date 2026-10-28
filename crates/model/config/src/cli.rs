//! The command lines both binaries share: argument words → [`Startup`] (where the user config
//! comes from) and the editor's [`Launch`]. Cites: config spec §2 (startup flags), §7 (the
//! editor's command line and usage text); rpc spec §3.2 (`--rpc`); D6 (`--no-default-ui`).
//!
//! Contents:
//! - `pub enum Startup`, `Startup::parse(args: &[String]) -> Result<(Startup, usize), String>`,
//!   `Startup::flags(&self) -> Vec<String>`
//! - `pub const GUI_USAGE: &str`
//! - `pub struct Launch`, `Launch::parse(args: &[String]) -> Result<Launch, String>`

use std::path::PathBuf;

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

/// `schemify-gui`'s usage text (spec §7): the whole error for a bad editor command line.
pub const GUI_USAGE: &str = "\
usage: schemify-gui [--clean | -u <init.lua>] [<project-dir>|<cell-file>] [--no-default-ui]
                    [--rpc <socket>]
       schemify-gui [--clean | -u <init.lua>] cmd <project-dir>|<cell-file> <command> [<arg>...]
  --rpc  serve the JSON-RPC command stream on a socket path or 127.0.0.1:<port>
  cmd    run one command (core or plugin) without a window and exit (schemify cmd)
";

/// The editor's command line (spec §7): `[flags] [edit] [<path>] [--no-default-ui]
/// [--rpc <socket>]`, the words after the flags in any order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    /// The global flags ([`Startup::parse`]).
    pub startup: Startup,
    /// The project directory or cell file; `None` when not given.
    pub path: Option<PathBuf>,
    /// `--no-default-ui`: the bare core without the default UI plugin (D6).
    pub no_default_ui: bool,
    /// `--rpc <socket>`: a socket path or `127.0.0.1:<port>` (rpc spec §3.2).
    pub rpc: Option<String>,
}

impl Startup {
    /// Splits the global flags (`--clean`, `-u <file>`) off the front of `args`; returns the
    /// startup and the index of the first other argument. No flags gives [`Startup::User`].
    ///
    /// # Errors
    ///
    /// `-u` without a file, or `--clean` and `-u` together or twice.
    ///
    /// # Panics
    ///
    /// Not for any `args`: the asserts check the index against the flags taken.
    pub fn parse(args: &[String]) -> Result<(Startup, usize), String> {
        let mut start = Startup::User;
        let mut i = 0;
        while let Some(word) = args.get(i) {
            match word.as_str() {
                "--clean" if start == Startup::User => start = Startup::Clean,
                "-u" if start == Startup::User => {
                    let file = args.get(i + 1).ok_or("-u needs a file")?;
                    start = Startup::File(PathBuf::from(file));
                    i += 1;
                }
                "--clean" | "-u" => return Err("give --clean or -u once".to_owned()),
                _ => break,
            }
            i += 1;
        }
        assert!(i <= args.len());
        // One flag form at most: none, `--clean` (one word) or `-u <file>` (two).
        let taken = match start {
            Startup::User => 0,
            Startup::Clean => 1,
            Startup::File(_) => 2,
        };
        assert_eq!(i, taken);
        Ok((start, i))
    }

    /// The flags that reproduce this startup (`schemify edit` forwards them to the editor);
    /// [`Startup::parse`] of them gives `self` back (for a `File` path that is UTF-8).
    #[must_use]
    pub fn flags(&self) -> Vec<String> {
        match self {
            Startup::Clean => vec!["--clean".to_owned()],
            Startup::User => Vec::new(),
            Startup::File(path) => vec!["-u".to_owned(), path.display().to_string()],
        }
    }
}

impl Launch {
    /// Parses the editor's command line (not `cmd`, which `schemify-gui` checks first). The
    /// word `edit` right after the flags is optional, so `cargo run -p schemify-app -- edit
    /// <path>` works; elsewhere `edit` is a path.
    ///
    /// # Errors
    ///
    /// A bad global flag: its message, a newline, then [`GUI_USAGE`]. `--rpc` without a
    /// socket, a second `--rpc`, an unknown `-` word or a second path: [`GUI_USAGE`] alone.
    ///
    /// # Panics
    ///
    /// Not for any `args`: the asserts check this function's own bookkeeping.
    pub fn parse(args: &[String]) -> Result<Launch, String> {
        let (startup, first) = Startup::parse(args).map_err(|e| format!("{e}\n{GUI_USAGE}"))?;
        let mut rest = &args[first..];
        if rest.first().is_some_and(|a| a == "edit") {
            rest = &rest[1..];
        }
        let mut launch = Launch {
            startup,
            ..Launch::default()
        };
        let mut words = rest.iter();
        while let Some(word) = words.next() {
            match word.as_str() {
                // The next word is the socket whatever it looks like.
                "--rpc" if launch.rpc.is_none() => {
                    launch.rpc = Some(words.next().ok_or(GUI_USAGE)?.clone());
                }
                "--no-default-ui" => launch.no_default_ui = true,
                a if a.starts_with('-') => return Err(GUI_USAGE.to_owned()),
                a if launch.path.is_none() => launch.path = Some(PathBuf::from(a)),
                _ => return Err(GUI_USAGE.to_owned()),
            }
        }
        assert!(first <= 2, "`Startup::parse` takes one flag form at most");
        // Negative space: a `-` word is a flag or the socket, never the path.
        if let Some(path) = &launch.path {
            assert!(!path.as_os_str().to_string_lossy().starts_with('-'));
        }
        Ok(launch)
    }
}
