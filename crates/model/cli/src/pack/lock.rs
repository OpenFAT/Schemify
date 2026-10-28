//! The package lockfile (config spec §5): `schemify-pack.lock` ↔ `doc::Locked` rows (SoA:
//! type, name, kind, resolved, hash per row). Rows are written sorted by (type, name), so
//! the same packages give the same file; an empty lock is no file.
//!
//! Contents (crate-private): `fn read_lock`, `fn set_row`, `fn select`, `fn write_lock`.

use std::fs;
use std::io::ErrorKind;
use std::mem;
use std::path::Path;

use schemify_doc::{Locked, Project};

use crate::platform;

/// Reads the lockfile at `path`; a missing file is an empty lock.
pub(super) fn read_lock(path: &Path) -> Result<Locked, String> {
    let Ok(bytes) = fs::read(path) else {
        return Ok(Locked::default());
    };
    let lock = Project::new(None, Some(&bytes), 0)
        .map(|p| p.lock().clone())
        .map_err(|e| format!("{}: {:?}", path.display(), e.diagnostics.first()))?;
    // The columns travel in step, as `write_lock` leaves them.
    assert_eq!(lock.ty.len(), lock.name.len());
    assert_eq!(lock.hash.len(), lock.name.len());
    Ok(lock)
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
    for &r in rows {
        assert!(r < lock.name.len(), "row {r} of {}", lock.name.len());
    }
    for col in [
        &mut lock.ty,
        &mut lock.name,
        &mut lock.kind,
        &mut lock.resolved,
        &mut lock.hash,
    ] {
        *col = rows.iter().map(|&r| mem::take(&mut col[r])).collect();
    }
    assert_eq!(lock.ty.len(), lock.hash.len());
    assert_eq!(lock.name.len(), rows.len());
}

/// Writes the lockfile sorted by (type, name); an empty lock deletes the file.
pub(super) fn write_lock(path: &Path, lock: &mut Locked) -> Result<(), String> {
    let mut order: Vec<usize> = (0..lock.name.len()).collect();
    order.sort_by(|&a, &b| (&lock.ty[a], &lock.name[a]).cmp(&(&lock.ty[b], &lock.name[b])));
    select(lock, &order);
    let sorted = (0..lock.name.len().saturating_sub(1))
        .all(|r| (&lock.ty[r], &lock.name[r]) <= (&lock.ty[r + 1], &lock.name[r + 1]));
    assert!(sorted, "rows are written sorted by (type, name)");
    if lock.name.is_empty() {
        return match fs::remove_file(path) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    platform::write_atomic(path, lock.write().as_bytes())
}
