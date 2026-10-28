//! Capabilities (D7): manifest grants in → [`Grants`]; a grant set and an I/O request in →
//! allow or deny. Pure, so the import path and the fuzz tests share one decision. Cites
//! plugin-abi §5 (capabilities, project-relative paths, `read:<NAME>`, `fs_remove` scope).
//!
//! Contents:
//! - `pub struct Grants`: `parse(grants: &[&str]) -> Result<Grants, Code>`,
//!   `has(&self, cap: Cap) -> bool`, `may_spawn(&self, binary: &str) -> bool`,
//!   `may_read_env(&self, name: &str) -> bool`
//! - `pub fn authorize(grants: &Grants, op: IoOp, args: &[String]) -> Result<(), Code>`
//! - `pub fn env_path(path: &str) -> Option<(&str, &str)>`

use schemify_abi::{Cap, Code};

use crate::IoOp;

/// Capabilities granted to one plugin (§5): a bit per [`Cap`], plus the exact binaries of
/// `spawn:<binary>` and the variables of `read:<NAME>` grants. Invariant: the `Spawn` bit is
/// set exactly when `spawn` is non-empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grants {
    caps: u8,
    spawn: Vec<Box<str>>,
    read_env: Vec<Box<str>>,
}

/// Longest project-relative path, in bytes.
const PATH_BYTES_MAX: usize = 4096;

// Every capability has a bit in `caps`.
const _: () = assert!(Cap::ALL.len() <= 8);

impl Grants {
    /// Parses manifest grants (`read_project`, `spawn:ngspice`, …); duplicates collapse.
    ///
    /// # Errors
    ///
    /// `E_PARSE` for any grant [`Cap::parse_grant`] rejects.
    pub fn parse(grants: &[&str]) -> Result<Grants, Code> {
        let mut g = Grants::default();
        for grant in grants {
            let (cap, arg) = Cap::parse_grant(grant).ok_or(Code::Parse)?;
            g.caps |= 1 << cap as u8;
            let list = match cap {
                Cap::Spawn => &mut g.spawn,
                Cap::ReadEnv => &mut g.read_env,
                Cap::ReadProject | Cap::WriteProject | Cap::Network | Cap::Clipboard => continue,
            };
            if !list.iter().any(|b| &**b == arg) {
                list.push(arg.into());
            }
        }
        debug_assert_eq!(g.has(Cap::Spawn), !g.spawn.is_empty());
        debug_assert_eq!(g.has(Cap::ReadEnv), !g.read_env.is_empty());
        Ok(g)
    }

    /// Returns whether `cap` is granted (for `spawn`: for at least one binary).
    pub fn has(&self, cap: Cap) -> bool {
        self.caps & (1 << cap as u8) != 0
    }

    /// Returns whether spawning exactly `binary` is granted.
    pub fn may_spawn(&self, binary: &str) -> bool {
        self.spawn.iter().any(|b| &**b == binary)
    }

    /// Returns whether reading under environment variable `name` is granted (`read:<name>`).
    pub fn may_read_env(&self, name: &str) -> bool {
        self.read_env.iter().any(|v| &**v == name)
    }
}

/// Decides whether `grants` allow an I/O request (§5).
///
/// # Errors
///
/// `E_DENIED` without the capability, for a path that is not project-relative (absolute,
/// `..`, `.` or empty segments, backslashes, drive or scheme colons), or for an
/// `argv[0]` that is not exactly a granted binary; `E_PARSE` for the wrong number of
/// arguments.
pub fn authorize(grants: &Grants, op: IoOp, args: &[String]) -> Result<(), Code> {
    let arity_ok = match op {
        IoOp::FsRead | IoOp::FsList | IoOp::FsWrite | IoOp::FsRemove | IoOp::Fetch => {
            args.len() == 1
        }
        IoOp::Spawn => !args.is_empty(),
        IoOp::ClipboardGet | IoOp::ClipboardSet => args.is_empty(),
    };
    if !arity_ok {
        return Err(Code::Parse);
    }
    let allowed = match op {
        // A `${NAME}/…` path reads under a granted variable's directory; anything else is
        // project-relative under `read_project`.
        IoOp::FsRead | IoOp::FsList => match env_path(&args[0]) {
            Some((name, _)) => grants.may_read_env(name),
            None => grants.has(Cap::ReadProject) && project_relative(&args[0]),
        },
        IoOp::FsWrite => grants.has(Cap::WriteProject) && project_relative(&args[0]),
        IoOp::FsRemove => grants.has(Cap::WriteProject) && removable(&args[0]),
        IoOp::Spawn => grants.may_spawn(&args[0]),
        IoOp::Fetch => grants.has(Cap::Network) && !args[0].is_empty(),
        IoOp::ClipboardGet | IoOp::ClipboardSet => grants.has(Cap::Clipboard),
    };
    // A spawn grant implies the capability bit (the `Grants` invariant).
    debug_assert!(!(op == IoOp::Spawn && allowed) || grants.has(op.cap()));
    if allowed { Ok(()) } else { Err(Code::Denied) }
}

/// Splits a `${NAME}` or `${NAME}/<rel>` read path (§5, capability `read:<NAME>`) into the
/// variable and the path below its directory (`""` for the directory itself). `None` when
/// `path` does not start with `${`, names no valid variable ([`Cap::env_name`]) or continues
/// with anything but a project-relative path.
pub fn env_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("${")?;
    let (name, rest) = rest.split_once('}')?;
    if !Cap::env_name(name) {
        return None;
    }
    let rel = match rest {
        "" => "",
        r => r.strip_prefix('/').filter(|r| project_relative(r))?,
    };
    Some((name, rel))
}

// ---- private ----

/// `fs_remove` targets: project-relative, strictly inside `.schemify/` (the tool's own
/// git-ignored directory, never the directory itself), and never a manifest or lockfile
/// name, so a plugin can delete what it installed and nothing a user wrote.
fn removable(path: &str) -> bool {
    let inside = path
        .strip_prefix(".schemify/")
        .is_some_and(|rest| !rest.is_empty());
    let last = path.rsplit('/').next().unwrap_or(path);
    let ok =
        project_relative(path) && inside && !matches!(last, "schemify.project" | "schemify.lock");
    debug_assert!(!ok || path.starts_with(".schemify/"));
    ok
}

/// A project-relative path. A segment starting with `${` is refused, so it can never be
/// mistaken for (or shadow) a `read:<NAME>` path ([`env_path`]).
fn project_relative(path: &str) -> bool {
    path.len() <= PATH_BYTES_MAX
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != ".." && !seg.starts_with("${"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// `fs_remove` (§5): granted by `write_project`, only strictly inside `.schemify/`, never
    /// a manifest or lockfile, never through `..`.
    #[test]
    fn fs_remove_is_scoped_to_the_tool_directory() {
        let w = Grants::parse(&["write_project"]).unwrap();
        let r = Grants::parse(&["read_project"]).unwrap();
        for ok in [
            ".schemify/pdks/sky130/abc",
            ".schemify/pdks/x/y/STATE",
            ".schemify/a",
        ] {
            assert_eq!(authorize(&w, IoOp::FsRemove, &args(&[ok])), Ok(()), "{ok}");
            let denied = authorize(&r, IoOp::FsRemove, &args(&[ok]));
            assert_eq!(denied, Err(Code::Denied), "{ok}");
        }
        for bad in [
            "schemify.project",
            "schemify.lock",
            ".schemify",
            ".schemify/",
            ".schemify/schemify.project",
            ".schemify/pdks/schemify.lock",
            ".schemify/../schemify.project",
            ".schemify/pdks/../../lib",
            "lib/a.shcomp",
            "/tmp/x",
            ".schemifyx/a",
            ".schemify//a",
        ] {
            let got = authorize(&w, IoOp::FsRemove, &args(&[bad]));
            assert_eq!(got, Err(Code::Denied), "{bad}");
        }
        assert_eq!(authorize(&w, IoOp::FsRemove, &[]), Err(Code::Parse));
    }

    /// `read:<NAME>` grants reads (and listings) under `${NAME}/` only, for exactly the
    /// granted names; writes never take an env path; `read_project` does not cover one.
    #[test]
    fn env_reads_follow_their_grants() {
        let g = Grants::parse(&["read:PDK_ROOT", "write_project"]).unwrap();
        for ok in ["${PDK_ROOT}", "${PDK_ROOT}/sky130A/libs.tech/xschem/x.sym"] {
            assert_eq!(authorize(&g, IoOp::FsRead, &args(&[ok])), Ok(()), "{ok}");
            assert_eq!(authorize(&g, IoOp::FsList, &args(&[ok])), Ok(()), "{ok}");
        }
        for bad in [
            "${HOME}/.ssh/id_rsa",
            "${PDK_ROOT}/../etc",
            "${PDK_ROOT}//x",
            "${PDK_ROOT}x",
            "${pdk_root}/x",
            "${PDK_ROOT",
            "lib/a.shcomp",
        ] {
            let got = authorize(&g, IoOp::FsRead, &args(&[bad]));
            assert_eq!(got, Err(Code::Denied), "{bad}");
        }
        let write = authorize(&g, IoOp::FsWrite, &args(&["${PDK_ROOT}/x"]));
        assert_eq!(write, Err(Code::Denied));
        let project = Grants::parse(&["read_project"]).unwrap();
        let list = authorize(&project, IoOp::FsList, &args(&["${PDK_ROOT}/x"]));
        assert_eq!(list, Err(Code::Denied));
        assert_eq!(authorize(&project, IoOp::FsList, &args(&["lib"])), Ok(()));
        assert_eq!(env_path("${A}/b/c"), Some(("A", "b/c")));
        assert_eq!(env_path("${A}"), Some(("A", "")));
    }

    #[test]
    fn authorize_follows_grants_and_paths() {
        let g = Grants::parse(&["read_project", "spawn:ngspice"]).unwrap();
        assert_eq!(
            authorize(&g, IoOp::FsRead, &args(&["lib/a.shcomp"])),
            Ok(())
        );
        for bad in [
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "./a",
            "c:\\x",
            "a\\b",
            "",
        ] {
            let got = authorize(&g, IoOp::FsRead, &args(&[bad]));
            assert_eq!(got, Err(Code::Denied), "{bad}");
        }
        assert_eq!(
            authorize(&g, IoOp::FsWrite, &args(&["a"])),
            Err(Code::Denied)
        );
        assert_eq!(
            authorize(&g, IoOp::Spawn, &args(&["ngspice", "-b"])),
            Ok(())
        );
        let path = authorize(&g, IoOp::Spawn, &args(&["/usr/bin/ngspice"]));
        assert_eq!(path, Err(Code::Denied));
        assert_eq!(
            authorize(&g, IoOp::Spawn, &args(&["sh"])),
            Err(Code::Denied)
        );
        assert_eq!(authorize(&g, IoOp::Spawn, &[]), Err(Code::Parse));
        let fetch = authorize(&g, IoOp::Fetch, &args(&["https://x"]));
        assert_eq!(fetch, Err(Code::Denied));
        assert_eq!(authorize(&g, IoOp::ClipboardGet, &[]), Err(Code::Denied));
    }

    #[test]
    fn grants_parse_and_dedupe() {
        let g = Grants::parse(&["spawn:a", "spawn:a", "spawn:b", "read:X"]).unwrap();
        assert!(g.has(Cap::Spawn) && g.has(Cap::ReadEnv) && !g.has(Cap::Network));
        assert_eq!(g.spawn.len(), 2);
        assert!(g.may_spawn("b") && !g.may_spawn("c") && g.may_read_env("X"));
        assert_eq!(Grants::parse(&["root"]), Err(Code::Parse));
        assert_eq!(Grants::parse(&["spawn"]), Err(Code::Parse));
    }
}
