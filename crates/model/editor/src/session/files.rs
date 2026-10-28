//! The session's files: opening by library path, cell row or file path, recovery from a
//! journal, creating, reindexing, journaling and saving.
//!
//! Contents:
//! - `Session::open_file`
//! - `Session::open_cell`
//! - `Session::open_path`
//! - `Session::pending_recovery`
//! - `Session::decide_recovery`
//! - `Session::close_discarding`
//! - `Session::drop_journal`
//! - `Session::reindex`
//! - `Session::create`
//! - `Session::flush_journal`
//! - `Session::save`

use std::path::{Path, PathBuf};

use schemify_doc::{DocHandle, OpenError, Recovery};
use schemify_format::FileKind;

use crate::libs::cell_relpath;
use crate::libs::{self, BUILTIN_DIR, absolute, rel_path};
use crate::platform;

use super::Session;

impl Session {
    /// Opens (or returns the already open) cell file `relpath` of library row `lib`, with its
    /// journal sidecar. A crash-recovery offer is queued for the user's decision
    /// ([`Session::pending_recovery`], [`Session::decide_recovery`]); until then the document
    /// refuses edits and saves (`doc` E044), so nothing is lost or overwritten.
    ///
    /// # Errors
    ///
    /// The file cannot be read or does not parse.
    ///
    /// # Panics
    ///
    /// Never: the assert checks that the returned handle is open.
    pub fn open_file(&mut self, lib: u8, relpath: &str) -> Result<DocHandle, String> {
        let dir = self
            .lib_dir
            .get(usize::from(lib))
            .and_then(Option::as_deref);
        let kept = self
            .kept
            .take_if(|(p, _)| dir.is_some_and(|d| absolute(&d.join(relpath)) == *p));
        let indexed = kept.is_some();
        let (path, bytes, builtin) = match kept {
            Some((path, bytes)) => (path, bytes, false),
            None => libs::cell_bytes(&self.lib_dir, &self.project, lib, relpath)?,
        };
        if let Some(h) = self.handle_of(&path) {
            return Ok(h);
        }
        // Builtin cells are read-only: no journal, and `save` refuses their paths.
        let journal_path = if builtin {
            PathBuf::new()
        } else {
            self.journal_path(&path)
        };
        let journal = if builtin {
            None
        } else {
            platform::read(&journal_path).ok()
        };
        let name = self.project.libs().name[usize::from(lib)].clone();
        let opened = if indexed {
            self.project
                .open_indexed(&name, relpath, bytes, journal.as_deref())
        } else {
            self.project
                .open(&name, relpath, &bytes, journal.as_deref())
        };
        let opened = match opened {
            Ok(o) => o,
            Err(OpenError::AlreadyOpen(h)) => return Ok(h),
            Err(OpenError::Parse(e)) => {
                let d = e.diagnostics.first();
                return Err(format!(
                    "{relpath}:{}: {:?}",
                    d.map_or(0, |d| d.line),
                    d.map(|d| d.code)
                ));
            }
            Err(OpenError::Code(c)) => return Err(format!("{relpath}: {c} {}", c.meaning())),
        };
        let h = opened.handle;
        match opened.recovery {
            Recovery::None => {}
            Recovery::Offer { txs } | Recovery::Diverged { txs } => {
                self.recovery.push((h, txs));
                self.status = format!("{relpath}: {txs} unsaved edit(s) can be recovered");
            }
        }
        self.docs.handle.push(h);
        self.docs.path.push(path);
        self.docs.journal.push(journal_path);
        self.docs.epoch.push(0);
        self.flush_journal(h);
        assert!(self.project.doc(h).is_some());
        Ok(h)
    }

    /// Opens indexed cell `row` (see [`Session::open_file`]).
    ///
    /// # Errors
    ///
    /// As [`Session::open_file`].
    ///
    /// # Panics
    ///
    /// If `row` is not a row of the current cell index (a row kept across a re-index).
    pub fn open_cell(&mut self, row: usize) -> Result<DocHandle, String> {
        let cells = self.project.cells();
        assert!(row < cells.lib.len(), "cell row from a previous index");
        let lib = cells.lib[row];
        let rel = cell_relpath(&self.project, row);
        self.open_file(lib, &rel)
    }

    /// Opens a file given by path (absolute or relative to the working directory): the
    /// library whose directory contains it, or the project.
    ///
    /// # Errors
    ///
    /// The file is outside every local library, unreadable, or does not parse.
    pub fn open_path(&mut self, path: &Path) -> Result<DocHandle, String> {
        let (lib, rel) = self
            .lib_of(path)
            .ok_or_else(|| format!("{}: outside the project", path.display()))?;
        self.open_file(lib as u8, &rel)
    }

    /// The local library holding `path` and the path relative to it: the longest matching
    /// directory wins (libraries may live under the root).
    fn lib_of(&self, path: &Path) -> Option<(usize, String)> {
        let abs = absolute(path);
        let mut best: Option<(usize, String, usize)> = None;
        for (row, dir) in self.lib_dir.iter().enumerate() {
            let Some(dir) = dir else { continue };
            let dir = absolute(dir);
            if let Some(rel) = rel_path(&dir, &abs) {
                let depth = dir.components().count();
                if best.as_ref().is_none_or(|b| depth > b.2) {
                    best = Some((row, rel, depth));
                }
            }
        }
        best.map(|(row, rel, _)| (row, rel))
    }

    /// The next document waiting for a crash-recovery decision: (document, journal records
    /// offered). Documents closed meanwhile are skipped.
    pub fn pending_recovery(&mut self) -> Option<(DocHandle, u32)> {
        let project = &self.project;
        self.recovery.retain(|(h, _)| project.doc(*h).is_some());
        self.recovery.first().copied()
    }

    /// Recovers (`true`: replays the journal's unsaved edits) or discards the offer of `h`,
    /// writes the journal, and reports the outcome in the status line.
    ///
    /// # Panics
    ///
    /// Never: the assert checks that `h` left the recovery list.
    pub fn decide_recovery(&mut self, h: DocHandle, recover: bool) {
        let Some(i) = self.recovery.iter().position(|r| r.0 == h) else {
            return;
        };
        self.recovery.remove(i);
        let name = self.name(h);
        self.status = if recover {
            match self.project.recover(h) {
                Ok((applied, dropped)) => format!(
                    "{name}: recovered {applied} unsaved edit(s) from the journal ({dropped} dropped)"
                ),
                Err(c) => format!("{name}: recovery failed ({c})"),
            }
        } else {
            match self.project.discard_recovery(h) {
                Ok(()) => format!("{name}: unsaved edits from the journal discarded"),
                Err(c) => format!("{name}: discard failed ({c})"),
            }
        };
        self.bump(h);
        self.flush_journal(h);
        assert!(!self.recovery.iter().any(|r| r.0 == h));
    }

    /// Closes `h` dropping its unsaved edits and its journal (Save As moved them to another
    /// file). Tabs must no longer show it.
    ///
    /// # Panics
    ///
    /// Never: the assert checks that `h` was closed.
    pub fn close_discarding(&mut self, h: DocHandle) {
        if let Some(path) = self.path(h).map(Path::to_path_buf) {
            self.drop_journal(&path);
        }
        let _ = self.project.close(h);
        assert!(self.project.doc(h).is_none());
    }

    /// Deletes the journal sidecar of cell file `path` (it was deleted, or its edits moved).
    pub fn drop_journal(&mut self, path: &Path) {
        let j = self.journal_path(path);
        if platform::exists(&j) {
            let _ = platform::remove(&j);
        }
    }

    /// Re-indexes the local library whose directory holds `path` (a file was written or
    /// deleted there), so choosers and `lib:cell` references see it. Cell rows shift: callers
    /// drop anything keyed by row (the symbol cache).
    ///
    /// # Panics
    ///
    /// Never: `lib_of` matches local libraries only.
    pub fn reindex(&mut self, path: &Path) {
        let Some((row, _)) = self.lib_of(path) else {
            return;
        };
        let dir = self.lib_dir[row]
            .as_deref()
            .expect("matched a local library");
        let others: Vec<&Path> = self
            .lib_dir
            .iter()
            .enumerate()
            .filter_map(|(r, d)| d.as_deref().filter(|d| r != row && d.starts_with(dir)))
            .collect();
        self.kept = None; // the new index may have read other bytes
        if let Err(e) = libs::index(&mut self.project, row, dir, &others, None) {
            self.status = e;
        }
    }

    /// Creates a new, unsaved cell document in the project directory.
    ///
    /// # Errors
    ///
    /// `doc` refuses (too many documents).
    ///
    /// # Panics
    ///
    /// If `kind` is not `Shcomp` or `Shprim`.
    pub fn create(&mut self, kind: FileKind) -> Result<DocHandle, String> {
        assert!(matches!(kind, FileKind::Shcomp | FileKind::Shprim));
        let ext = kind.word();
        let rel = loop {
            self.untitled += 1;
            let rel = format!("untitled{}.{ext}", self.untitled);
            if !platform::exists(&self.root.join(&rel)) {
                break rel;
            }
        };
        let h = self
            .project
            .create("", &rel, kind)
            .map_err(|c| format!("{rel}: {c}"))?;
        let path = self.root.join(&rel);
        self.docs.handle.push(h);
        self.docs.journal.push(self.journal_path(&path));
        self.docs.path.push(path);
        self.docs.epoch.push(0);
        Ok(h)
    }
}

impl Session {
    /// `.schemify/journal/<project-relative cell path>.journal` (edits spec §3).
    fn journal_path(&self, file: &Path) -> PathBuf {
        let rel = rel_path(&absolute(&self.root), &absolute(file)).unwrap_or_else(|| {
            file.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        });
        self.root
            .join(".schemify")
            .join("journal")
            .join(format!("{rel}.journal"))
    }

    /// Writes the journal bytes `doc` produced for `h` since the last call.
    pub fn flush_journal(&mut self, h: DocHandle) {
        let Some(w) = self.project.take_journal(h) else {
            return;
        };
        let Some(i) = self.row(h) else { return };
        let path = &self.docs.journal[i];
        if path.as_os_str().is_empty() {
            return; // builtin library cell: read-only, never journaled
        }
        let r = if w.rewrite {
            platform::write_atomic(path, w.text.as_bytes())
        } else {
            platform::append(path, w.text.as_bytes())
        };
        if let Err(e) = r {
            self.status = format!("journal: {e}");
        }
    }

    /// Save spine: `doc::save` → atomic write → `doc::saved` → journal. Returns whether
    /// anything was written (a clean document is not rewritten).
    ///
    /// # Errors
    ///
    /// `doc` refuses (read-only, conflict, kind rules) or the write fails.
    ///
    /// # Panics
    ///
    /// Never: the assert checks that a saved document is clean.
    pub fn save(&mut self, h: DocHandle) -> Result<bool, String> {
        let req = self
            .project
            .save(h)
            .map_err(|c| format!("save refused: {c} {}", c.meaning()))?;
        let Some(req) = req else { return Ok(false) };
        let path = self.path(h).ok_or("not a file document")?.to_path_buf();
        if path.starts_with(BUILTIN_DIR) {
            return Err("the bundled core library is read-only".into());
        }
        platform::write_atomic(&path, req.bytes.as_bytes())?;
        self.project
            .saved(h, req.bytes)
            .map_err(|c| format!("save: {c}"))?;
        self.flush_journal(h);
        assert!(!self.project.is_dirty(h));
        Ok(true)
    }
}
