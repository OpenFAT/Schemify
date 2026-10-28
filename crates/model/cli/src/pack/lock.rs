//! The package lockfile (config spec §5): `schemify-pack.lock` ↔ `doc::Locked` rows (SoA:
//! type, name, kind, resolved, hash per row). Rows are written sorted by (type, name), so
//! the same packages give the same file; an empty lock is no file.
//!
//! Contents (crate-private): `fn read_lock`, `fn set_row`, `fn select`, `fn write_lock`.

use std::path::Path;

use schemify_doc::{Locked, Project};

/// Reads the lockfile at `path`; a missing file is an empty lock.
pub(super) fn read_lock(path: &Path) -> Result<Locked, String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(Locked::default());
    };
    Project::new(None, Some(&bytes), 0)
        .map(|p| p.lock().clone())
        .map_err(|e| format!("{}: {:?}", path.display(), e.diagnostics.first()))
}

/// Sets the `plugin` row of `name` (appended when missing).
pub(super) fn set_row(lock: &mut Locked, name: &str, kind: &str, resolved: &str, hash: &str) {
    let row = match lock.find("plugin", name) {
        Some(r) => r,
        None => {
            lock.ty.push("plugin".into());
            lock.name.push(name.to_owned());
            lock.kind.push(String::new());
            lock.resolved.push(String::new());
            lock.hash.push(String::new());
            lock.name.len() - 1
        }
    };
    lock.kind[row] = kind.to_owned();
    lock.resolved[row] = resolved.to_owned();
    lock.hash[row] = hash.to_owned();
    assert_eq!(lock.ty.len(), lock.hash.len());
    assert_eq!(lock.find("plugin", name), Some(row));
}

/// Keeps rows `rows` of `lock` (each at most once), in that order.
pub(super) fn select(lock: &mut Locked, rows: &[usize]) {
    assert!(rows.len() <= lock.name.len());
    for col in [
        &mut lock.ty,
        &mut lock.name,
        &mut lock.kind,
        &mut lock.resolved,
        &mut lock.hash,
    ] {
        *col = rows.iter().map(|&r| std::mem::take(&mut col[r])).collect();
    }
    assert_eq!(lock.ty.len(), lock.hash.len());
    assert_eq!(lock.name.len(), rows.len());
}

/// Writes the lockfile sorted by (type, name); an empty lock deletes the file.
pub(super) fn write_lock(path: &Path, lock: &mut Locked) -> Result<(), String> {
    let mut order: Vec<usize> = (0..lock.name.len()).collect();
    order.sort_by(|&a, &b| (&lock.ty[a], &lock.name[a]).cmp(&(&lock.ty[b], &lock.name[b])));
    select(lock, &order);
    if lock.name.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    crate::platform::write_atomic(path, lock.write().as_bytes())
}
