//! Neovim-style `stdpath` directories (config spec §1): a pure function of the environment
//! and the OS layout, so the per-OS table is tested on any host.

use std::path::PathBuf;

/// Which directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// `init.lua`, `lua/`, the package lockfile.
    Config,
    /// Installed packages (`pack/<name>`).
    Data,
    /// Persistent session state.
    State,
    /// Disposable caches.
    Cache,
}

/// The directory layout to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    /// Linux, macOS and the BSDs: XDG, also on macOS.
    Unix,
    /// `%LOCALAPPDATA%`.
    Windows,
    /// OPFS (D31): fixed absolute paths, no app name.
    Web,
}

impl Os {
    /// The layout of the target this binary was built for.
    pub const fn current() -> Os {
        if cfg!(target_arch = "wasm32") {
            Os::Web
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Unix
        }
    }
}

/// The default app name; `SCHEMIFY_APPNAME` replaces it.
pub const APP: &str = "schemify";

/// The directory `dir` under layout `os`, reading variables through `env` (`None` = unset).
///
/// # Errors
///
/// `SCHEMIFY_APPNAME` is not one plain path segment, or the variable the default needs
/// (`HOME`; `LOCALAPPDATA`, `TEMP` on Windows) is unset or relative. The message names it.
pub fn stdpath(dir: Dir, os: Os, env: &dyn Fn(&str) -> Option<String>) -> Result<PathBuf, String> {
    if os == Os::Web {
        let p = match dir {
            Dir::Config => "/config",
            Dir::Data => "/data",
            Dir::State => "/state",
            Dir::Cache => "/cache",
        };
        return Ok(PathBuf::from(p));
    }
    let app = match env("SCHEMIFY_APPNAME").filter(|a| !a.is_empty()) {
        None => APP.to_owned(),
        Some(a) if is_segment(&a) => a,
        Some(a) => {
            return Err(format!(
                "SCHEMIFY_APPNAME `{a}` is not one plain path segment"
            ));
        }
    };
    let sep = if os == Os::Windows { '\\' } else { '/' };
    let xdg = match dir {
        Dir::Config => "XDG_CONFIG_HOME",
        Dir::Data => "XDG_DATA_HOME",
        Dir::State => "XDG_STATE_HOME",
        Dir::Cache => "XDG_CACHE_HOME",
    };
    if let Some(base) = env(xdg).filter(|b| is_absolute(b, os)) {
        return Ok(PathBuf::from(join(&base, &app, sep)));
    }
    let (var, under, suffix) = match (os, dir) {
        (Os::Windows, Dir::Config) => ("LOCALAPPDATA", "", ""),
        (Os::Windows, Dir::Data | Dir::State) => ("LOCALAPPDATA", "", "-data"),
        (Os::Windows, Dir::Cache) => ("TEMP", "", ""),
        (_, Dir::Config) => ("HOME", ".config", ""),
        (_, Dir::Data) => ("HOME", ".local/share", ""),
        (_, Dir::State) => ("HOME", ".local/state", ""),
        (_, Dir::Cache) => ("HOME", ".cache", ""),
    };
    let Some(base) = env(var).filter(|b| is_absolute(b, os)) else {
        return Err(format!("{var} is not set to an absolute path"));
    };
    let base = if under.is_empty() {
        base
    } else {
        join(&base, under, sep)
    };
    let path = join(&base, &format!("{app}{suffix}"), sep);
    assert!(path.len() > app.len());
    Ok(PathBuf::from(path))
}

/// [`stdpath`] for this process: the real environment and [`Os::current`].
///
/// # Errors
///
/// As [`stdpath`].
pub fn here(dir: Dir) -> Result<PathBuf, String> {
    stdpath(dir, Os::current(), &|k| std::env::var(k).ok())
}

/// One path segment of `A–Z a–z 0–9 . _ -`, not `.` or `..`.
fn is_segment(s: &str) -> bool {
    let ok = s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    ok && s != "." && s != ".."
}

fn is_absolute(p: &str, os: Os) -> bool {
    let b = p.as_bytes();
    match os {
        Os::Windows => {
            p.starts_with("\\\\")
                || (b.len() >= 3
                    && b[0].is_ascii_alphabetic()
                    && b[1] == b':'
                    && matches!(b[2], b'\\' | b'/'))
        }
        _ => p.starts_with('/'),
    }
}

fn join(base: &str, rest: &str, sep: char) -> String {
    let base = base.trim_end_matches(['/', '\\']);
    format!("{base}{sep}{rest}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|p| p.0 == k).map(|p| p.1.to_owned())
    }

    fn at(dir: Dir, os: Os, pairs: &[(&str, &str)]) -> Result<String, String> {
        stdpath(dir, os, &env(pairs)).map(|p| p.to_string_lossy().into_owned())
    }

    #[test]
    fn the_spec_table() {
        let home = [("HOME", "/home/u")];
        let rows = [
            (Dir::Config, "/home/u/.config/schemify"),
            (Dir::Data, "/home/u/.local/share/schemify"),
            (Dir::State, "/home/u/.local/state/schemify"),
            (Dir::Cache, "/home/u/.cache/schemify"),
        ];
        for (d, want) in rows {
            assert_eq!(at(d, Os::Unix, &home).as_deref(), Ok(want));
        }
        let win = [
            ("LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local"),
            ("TEMP", "C:\\Temp"),
        ];
        let rows = [
            (Dir::Config, "C:\\Users\\u\\AppData\\Local\\schemify"),
            (Dir::Data, "C:\\Users\\u\\AppData\\Local\\schemify-data"),
            (Dir::State, "C:\\Users\\u\\AppData\\Local\\schemify-data"),
            (Dir::Cache, "C:\\Temp\\schemify"),
        ];
        for (d, want) in rows {
            assert_eq!(at(d, Os::Windows, &win).as_deref(), Ok(want));
        }
        let rows = [
            (Dir::Config, "/config"),
            (Dir::Data, "/data"),
            (Dir::State, "/state"),
            (Dir::Cache, "/cache"),
        ];
        for (d, want) in rows {
            // The web ignores every variable, the app name included.
            assert_eq!(
                at(d, Os::Web, &[("SCHEMIFY_APPNAME", "x")]).as_deref(),
                Ok(want)
            );
        }
    }

    #[test]
    fn xdg_and_appname_override() {
        let e = [
            ("HOME", "/home/u"),
            ("XDG_CONFIG_HOME", "/cfg/"),
            ("XDG_DATA_HOME", "relative/ignored"),
            ("SCHEMIFY_APPNAME", "schemify-dev"),
        ];
        assert_eq!(
            at(Dir::Config, Os::Unix, &e).as_deref(),
            Ok("/cfg/schemify-dev")
        );
        assert_eq!(
            at(Dir::Data, Os::Unix, &e).as_deref(),
            Ok("/home/u/.local/share/schemify-dev")
        );
        let w = [
            ("LOCALAPPDATA", "C:\\L"),
            ("XDG_CONFIG_HOME", "D:\\dot"),
            ("SCHEMIFY_APPNAME", "x"),
        ];
        assert_eq!(
            at(Dir::Config, Os::Windows, &w).as_deref(),
            Ok("D:\\dot\\x")
        );
        assert_eq!(
            at(Dir::Data, Os::Windows, &w).as_deref(),
            Ok("C:\\L\\x-data")
        );
        // An empty app name means unset.
        let blank = [("HOME", "/h"), ("SCHEMIFY_APPNAME", "")];
        assert_eq!(
            at(Dir::Config, Os::Unix, &blank).as_deref(),
            Ok("/h/.config/schemify")
        );
    }

    #[test]
    fn refusals_name_the_variable() {
        for bad in ["..", ".", "a/b", "a\\b", "/abs", "sp ace"] {
            let e = [("HOME", "/h"), ("SCHEMIFY_APPNAME", bad)];
            assert!(
                at(Dir::Config, Os::Unix, &e)
                    .unwrap_err()
                    .contains("SCHEMIFY_APPNAME")
            );
        }
        assert!(at(Dir::Config, Os::Unix, &[]).unwrap_err().contains("HOME"));
        assert!(
            at(Dir::Config, Os::Unix, &[("HOME", "rel")])
                .unwrap_err()
                .contains("HOME")
        );
        assert!(
            at(Dir::Cache, Os::Windows, &[("LOCALAPPDATA", "C:\\L")])
                .unwrap_err()
                .contains("TEMP")
        );
    }
}
