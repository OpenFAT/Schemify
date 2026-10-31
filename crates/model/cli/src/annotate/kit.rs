//! A bench's corners → the device models they define → the devices an extracted netlist
//! calls that none of them defines (`schemify annotate`'s kit check). Each corner's `lib`
//! and `include` lines are walked as the simulator reads them: `.lib <section>` to `.endl`,
//! nested `.include` and `.lib <file> <section>`, relative paths from the naming file.
//!
//! Cites: run spec §5 (corner paths: the bench's directory, the project root for a project
//! `set` value, `~/`).
//!
//! Contents (`pub(super)`): `type Kit`, `fn check_kit(&Opened, DocHandle, &str, &[(&str,
//! &str)], &[String], (&Path, &Path), &mut BTreeMap<…, Kit>) -> Vec<(&str, String)>`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use schemify_doc::{Assignment, DocHandle};
use schemify_elab::{CornerLine, Elab, elaborate, no_funcs};

use crate::platform;
use crate::spines::Opened;

/// The names one corner's model files define (lowercase) and the files that could not be
/// read.
pub(super) type Kit = (BTreeSet<String>, Vec<String>);

/// Most model files one corner may read (sky130's `tt` section reads about 30).
const KIT_FILES_MAX: usize = 1 << 12;

/// The findings of the device check on bench `h`, as `(level, message)`: per corner of the
/// bench (elaborated under `cfg`, as the post-layout runs select), the devices in `needs`
/// that its model files do not define, or the files it could not read; one note for a bench
/// without a corner. `dirs` are the bench's directory and the project root (run spec §5);
/// `kits` memoizes the walks by file list.
pub(super) fn check_kit(
    o: &Opened,
    h: DocHandle,
    cfg: &str,
    env: &[Assignment<&str>],
    needs: &[String],
    dirs: (&Path, &Path),
    kits: &mut BTreeMap<Vec<(PathBuf, Option<String>)>, Kit>,
) -> Vec<(&'static str, String)> {
    assert!(!needs.is_empty());
    let doc = o.project.doc(h).expect("an open bench");
    let corners: Vec<String> = (0..doc.corners().kind.len())
        .map(|r| doc.strings().get(doc.corners().fields[r][0]).to_owned())
        .collect();
    if corners.is_empty() {
        return vec![(
            "note",
            "no corner loads model files: devices not checked".to_owned(),
        )];
    }
    let mut out = Vec::new();
    for corner in &corners {
        let req = super::request(Some(cfg), Some(corner), env);
        let e = elaborate(&o.project, h, &req, &mut no_funcs);
        let files = corner_files(&e, dirs.0, dirs.1);
        let (names, unread) = kits.entry(files.clone()).or_insert_with(|| kit(&files));
        if !unread.is_empty() {
            let why = format!(
                "corner {corner}: devices not checked: {}",
                unread.join("; ")
            );
            out.push(("warning", why));
            continue;
        }
        let missing: Vec<&str> = needs
            .iter()
            .filter(|n| !names.contains(*n))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            let why = format!(
                "corner {corner}: {} not defined by the extracted netlist or the corner's model \
                 files",
                missing.join(", ")
            );
            out.push(("warning", why));
        }
    }
    assert!(out.len() <= corners.len());
    out
}

// ---- private ----

/// The model files the corner of elaboration `e` loads: `(path, section)` per `lib` line,
/// `(path, None)` per `include`; a relative path is joined onto the bench's directory, or
/// onto the project root when a project `set` value began it (run spec §5).
fn corner_files(e: &Elab, bench_dir: &Path, root: &Path) -> Vec<(PathBuf, Option<String>)> {
    let b = &e.bench;
    assert_eq!(b.corner_start.len(), b.corner_kind.len() + 1);
    let mut out = Vec::new();
    for k in 0..b.corner_kind.len() {
        let args = &b.corner_args[b.corner_start[k] as usize..b.corner_start[k + 1] as usize];
        let arg = |i: usize| args.get(i).map(|n| e.names.get(*n));
        let section = match b.corner_kind[k] {
            CornerLine::Lib => arg(1).map(str::to_owned),
            CornerLine::Include => None,
            CornerLine::Temp => continue,
        };
        let Some(path) = arg(0) else { continue };
        let dir = if b.corner_project.get(k).copied().unwrap_or(false) {
            root
        } else {
            bench_dir
        };
        out.push((corner_path(dir, path), section));
    }
    assert!(out.len() <= b.corner_kind.len());
    out
}

/// A corner path as the simulator opens it: `~/…` under the home directory, an absolute
/// path as written, anything else joined onto `dir` (run spec §5).
fn corner_path(dir: &Path, path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = platform::env_var("HOME")
    {
        return Path::new(&home).join(rest);
    }
    let joined = dir.join(path);
    assert!(joined.is_absolute() || !dir.is_absolute());
    joined
}

/// Walks one corner's model files: each `(path, section)` read whole (`None`) or only
/// between `.lib <section>` and its `.endl`, following `.include` and `.lib <file>
/// <section>` lines, a relative path taken from the directory of the file naming it (as
/// ngspice does). Collects the `.subckt` and `.model` names; a binned model `nch.3` also
/// answers a call to `nch`.
fn kit(files: &[(PathBuf, Option<String>)]) -> Kit {
    let (mut names, mut unread) = (BTreeSet::new(), Vec::new());
    let mut todo: Vec<(PathBuf, Option<String>)> = files.iter().rev().cloned().collect();
    let mut seen: BTreeSet<(PathBuf, Option<String>)> = BTreeSet::new();
    let unquote = |w: &str| w.trim_matches(['"', '\'']).to_owned();
    while let Some((path, section)) = todo.pop() {
        // `a/../a/x` is `a/x`: without this an include cycle grows its path forever.
        let path = normal(&path);
        if !seen.insert((path.clone(), section.clone())) {
            continue;
        }
        if seen.len() > KIT_FILES_MAX {
            unread.push(format!("more than {KIT_FILES_MAX} model files"));
            break;
        }
        let text = match platform::read(&path) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                unread.push(e);
                continue;
            }
        };
        let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
        // `None` outside every `.lib` section, else whether it is the wanted one.
        let mut inside: Option<bool> = None;
        for line in text.lines() {
            let mut w = line.split_whitespace();
            let Some(word) = w.next() else { continue };
            let active = matches!((&section, inside), (Some(_), Some(true)) | (None, None));
            match word.to_ascii_lowercase().as_str() {
                ".lib" => match (w.next(), w.next()) {
                    (Some(file), Some(sec)) if active => {
                        todo.push((dir.join(unquote(file)), Some(sec.to_owned())));
                    }
                    (Some(name), None) => {
                        inside = Some(
                            section
                                .as_deref()
                                .is_some_and(|s| s.eq_ignore_ascii_case(name)),
                        );
                    }
                    _ => {}
                },
                ".endl" => inside = None,
                ".include" | ".inc" if active => {
                    if let Some(file) = w.next() {
                        todo.push((dir.join(unquote(file)), None));
                    }
                }
                ".subckt" | ".model" if active => {
                    if let Some(n) = w.next() {
                        let n = n.to_ascii_lowercase();
                        if let Some((base, bin)) = n.rsplit_once('.')
                            && !bin.is_empty()
                            && bin.bytes().all(|b| b.is_ascii_digit())
                        {
                            names.insert(base.to_owned());
                        }
                        names.insert(n);
                    }
                }
                _ => {}
            }
        }
    }
    assert!(seen.len() <= KIT_FILES_MAX + 1);
    (names, unread)
}

/// `p` with `.` dropped and each `..` taking off the component before it (lexically:
/// symlinks are not followed, so a symlink cycle ends at [`KIT_FILES_MAX`]).
fn normal(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
            {
                out.pop();
            }
            Component::Prefix(_)
            | Component::RootDir
            | Component::ParentDir
            | Component::Normal(_) => {
                out.push(c);
            }
        }
    }
    assert!(out.components().count() <= p.components().count());
    out
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{kit, normal};

    #[test]
    fn kit_follows_sections_and_includes() {
        let dir =
            std::env::temp_dir().join(format!("schemify-annotate-kit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("corners")).unwrap();
        let w = |f: &str, t: &str| std::fs::write(dir.join(f), t).unwrap();
        w(
            "lib.spice",
            "* kit\n.lib tt\n.include \"corners/tt.spice\"\n.endl tt\n.lib ff\n.include \"ff.spice\"\n\
             .endl ff\n.lib mc\n.lib 'lib.spice' tt\n.model mcx nmos\n.endl\n.subckt outside a\n.ends\n",
        );
        w(
            "corners/tt.spice",
            ".SUBCKT NFET d g s b\n.ends\n.model nch.3 nmos\n.include ../loop.spice\n",
        );
        w(
            "loop.spice",
            ".include \"corners/tt.spice\"\n.subckt res a b\n.ends\n",
        );
        w("ff.spice", ".subckt fast a\n.ends\n");
        let lib = dir.join("lib.spice");
        let names = |files: &[(PathBuf, Option<String>)]| {
            let (n, unread) = kit(files);
            assert!(unread.is_empty(), "{unread:?}");
            n.into_iter().collect::<Vec<_>>()
        };
        assert_eq!(
            names(&[(lib.clone(), Some("TT".into()))]),
            ["nch", "nch.3", "nfet", "res"]
        );
        assert_eq!(
            names(&[(lib.clone(), Some("mc".into()))]),
            ["mcx", "nch", "nch.3", "nfet", "res"]
        );
        // Read whole, a file contributes only what lies outside its sections.
        assert_eq!(names(&[(lib.clone(), None)]), ["outside"]);
        let (_, unread) = kit(&[(dir.join("missing.spice"), None)]);
        assert_eq!(unread.len(), 1);
        assert_eq!(normal(Path::new("/a/./b/../c/../../d")), Path::new("/d"));
        assert_eq!(normal(Path::new("../x/../y")), Path::new("../y"));
    }
}
