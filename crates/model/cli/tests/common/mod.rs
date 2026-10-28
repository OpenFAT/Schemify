//! Helpers the end-to-end test crates share: the `schemify` binary with the user
//! directories inside the test's own directory (so the developer's
//! `~/.config/schemify/init.lua` never runs), and a fresh scratch directory per test.
//!
//! Contents: `pub fn core(&Path) -> Command`, `pub fn schemify(&Path, &[&str]) -> (i32,
//! String)`, `pub fn scratch(&str) -> PathBuf`.

// Each test crate uses a subset of these helpers.
#![allow(dead_code)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::process::Command;

/// The core binary, run in `dir` with its config and data directories under `dir/.xdg`.
pub fn core(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_schemify"));
    c.current_dir(dir)
        .env("XDG_CONFIG_HOME", dir.join(".xdg/config"))
        .env("XDG_DATA_HOME", dir.join(".xdg/data"))
        .env_remove("SCHEMIFY_APPNAME");
    c
}

/// Runs the core in `dir` with `args`; returns its exit code and stdout followed by stderr.
pub fn schemify(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = core(dir).args(args).output().expect("binary runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().expect("not killed"), text)
}

/// An empty directory `schemify-<test crate>-<name>-<pid>` under the temp dir; `name` is
/// unique within its test crate.
pub fn scratch(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!(
        "schemify-{}-{name}-{}",
        env!("CARGO_CRATE_NAME"),
        process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the temp dir is writable");
    dir
}
