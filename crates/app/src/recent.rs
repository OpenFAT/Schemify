//! `recent`: the recent-files list (File → Open recent, Open Most Recent) as user state, in
//! `<state dir>/recent` (`platform::state_dir`). The file has the config syntax (D9: one
//! syntax for config): `schemify 1 config` and one `set recent.<i> <path>` per entry, most
//! recent first.
//!
//! Data design: ≤ [`RECENT_MAX`] absolute paths, most recent first; read once at start,
//! rewritten whole after every open (a few hundred bytes).

use std::path::{Path, PathBuf};

use schemify_format::{Document as FormatDoc, FileKind, RecordKind, parse, write};

use crate::platform;

/// Entries kept (xschem keeps 10).
pub const RECENT_MAX: usize = 10;

/// The list file inside the state directory.
pub fn file(dir: &Path) -> PathBuf {
    dir.join("recent")
}

/// Reads the list; a missing or unreadable file is an empty list (state is optional).
pub fn load(dir: &Path) -> Vec<PathBuf> {
    let Ok(bytes) = platform::read(&file(dir)) else {
        return Vec::new();
    };
    let Ok(d) = parse(&bytes) else {
        return Vec::new();
    };
    let mut out: Vec<(usize, PathBuf)> = (1..d.record_count())
        .filter(|&r| d.record_kind(r) == RecordKind::Set)
        .filter_map(|r| {
            let i = d
                .field(r, 0)?
                .strip_prefix("recent.")?
                .parse::<usize>()
                .ok()?;
            Some((i, PathBuf::from(d.field(r, 1)?)))
        })
        .collect();
    out.sort_by_key(|e| e.0);
    out.truncate(RECENT_MAX);
    out.into_iter().map(|e| e.1).collect()
}

/// Moves `path` to the front of `list` (dropping an older copy and the oldest beyond
/// [`RECENT_MAX`]).
pub fn push(list: &mut Vec<PathBuf>, path: PathBuf) {
    list.retain(|p| *p != path);
    list.insert(0, path);
    list.truncate(RECENT_MAX);
    assert!(list.len() <= RECENT_MAX);
}

/// The list as file text.
pub fn text(list: &[PathBuf]) -> String {
    let mut d = FormatDoc::new(FileKind::Config);
    for (i, p) in list.iter().take(RECENT_MAX).enumerate() {
        d.begin_record(RecordKind::Set);
        d.push_field(&format!("recent.{i}"));
        d.push_field(&p.to_string_lossy());
    }
    write(&d)
}

/// Writes the list.
///
/// # Errors
///
/// The write fails.
pub fn save(dir: &Path, list: &[PathBuf]) -> Result<(), String> {
    platform::write_atomic(&file(dir), text(list).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_recent_first_without_duplicates_capped() {
        let mut l = Vec::new();
        for i in 0..12 {
            push(&mut l, PathBuf::from(format!("/p/{i}.shcomp")));
        }
        push(&mut l, PathBuf::from("/p/5.shcomp"));
        assert_eq!(l.len(), RECENT_MAX);
        assert_eq!(l[0], PathBuf::from("/p/5.shcomp"));
        assert_eq!(l[1], PathBuf::from("/p/11.shcomp"));
        assert_eq!(l.iter().filter(|p| p.ends_with("5.shcomp")).count(), 1);
        let t = text(&l);
        assert!(t.starts_with("schemify 1 config\n"), "{t}");
        let d = parse(t.as_bytes()).unwrap();
        assert_eq!(d.field(1, 1), Some("/p/5.shcomp"));
    }
}
