//! The chooser dialog: cells or waveform files filtered as typed, picked by row.

use std::path::{Path, PathBuf};

use schemify_doc::DocHandle;
use schemify_format::FileKind;

use crate::geometry::Xform;
use crate::platform;

use super::Editor;
use super::dialog::Dialog;
use super::tool::{Placement, Tool};

/// Chooser rows listed (the list scrolls; typing filters). Every row is a composed node, so
/// the cap bounds the per-frame compose cost while a chooser is open.
// ponytail: a fixed cap; a virtualized list (rows composed for the visible window only) when
// libraries list more cells than this.
pub(super) const CHOOSER_ROWS: usize = 512;

#[derive(Clone, Debug)]
pub(super) enum Pick {
    Cell(usize),
    File(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChooseFor {
    Insert,
    Open,
    Waves,
    /// Insert another cell's schematic at the pointer (File → Merge).
    Merge,
    /// Delete a cell file (picked twice to confirm).
    Delete,
    /// The cell to compare the active document with.
    Compare,
}

#[derive(Clone, Debug)]
pub(super) struct Chooser {
    pub(super) purpose: ChooseFor,
    pub(super) filter: String,
    pub(super) rows: Vec<(String, Pick)>,
    /// Analysis a waves chooser was opened for (`waves.load.<a>`), shown in its title.
    pub(super) note: String,
}

impl Editor {
    pub(super) fn open_chooser(&mut self, purpose: ChooseFor) {
        self.tool = Tool::Idle;
        self.dialog = Dialog::Choose(Chooser {
            purpose,
            filter: String::new(),
            rows: Vec::new(),
            note: String::new(),
        });
        self.refill_chooser();
        self.show_dialog("dialog.choose");
        self.focus_next = Some("dialog.choose.filter".into());
    }
    /// Recomputes the chooser rows from the filter (substring, case-insensitive).
    pub(super) fn refill_chooser(&mut self) {
        let Dialog::Choose(c) = &self.dialog else {
            return;
        };
        let (purpose, filter) = (c.purpose, c.filter.to_lowercase());
        let mut rows = Vec::new();
        match purpose {
            ChooseFor::Insert
            | ChooseFor::Open
            | ChooseFor::Merge
            | ChooseFor::Delete
            | ChooseFor::Compare => {
                let p = &self.session.project;
                let cells = p.cells();
                for row in 0..cells.lib.len() {
                    let kind = cells.kind[row];
                    let skip = match purpose {
                        ChooseFor::Insert => kind == FileKind::Shbench,
                        // Only files with a schematic merge or compare.
                        ChooseFor::Merge | ChooseFor::Compare => kind == FileKind::Shprim,
                        // Only files on disk delete (not the bundled core library).
                        ChooseFor::Delete => self.session.lib_dir(cells.lib[row]).is_none(),
                        _ => false,
                    };
                    if skip {
                        continue;
                    }
                    let lib = &p.libs().name[usize::from(cells.lib[row])];
                    let rel = crate::session::cell_relpath(p, row);
                    let label = if lib.is_empty() {
                        rel
                    } else {
                        format!("{lib}:{rel}")
                    };
                    if label.to_lowercase().contains(&filter) {
                        rows.push((label, Pick::Cell(row)));
                    }
                }
            }
            ChooseFor::Waves => {
                for f in platform::list_files(&self.session.root, &[]) {
                    if f.extension().is_some_and(|e| e == "raw") {
                        let label = f
                            .strip_prefix(&self.session.root)
                            .unwrap_or(&f)
                            .display()
                            .to_string();
                        if label.to_lowercase().contains(&filter) {
                            rows.push((label, Pick::File(f)));
                        }
                    }
                }
            }
        }
        rows.truncate(CHOOSER_ROWS);
        if let Dialog::Choose(c) = &mut self.dialog {
            c.rows = rows;
        }
    }
    pub(super) fn choose_filter(&mut self, arg: &str) {
        let Dialog::Choose(c) = &mut self.dialog else {
            return;
        };
        c.filter = arg.to_owned();
        let purpose = c.purpose;
        self.refill_chooser();
        if purpose == ChooseFor::Waves && !arg.is_empty() && platform::exists(Path::new(arg)) {
            self.open_waves(PathBuf::from(arg));
        } else if let Dialog::Choose(c) = &self.dialog
            && c.rows.len() == 1
        {
            let node = "dialog.choose.row.0".to_owned();
            let h = self.active().map(|a| a.0);
            if let Some(h) = h {
                self.choose_pick(&node, h);
            }
        }
    }
    pub(super) fn choose_pick(&mut self, node: &str, h: DocHandle) {
        let Dialog::Choose(c) = &self.dialog else {
            return;
        };
        let i = node
            .rsplit('.')
            .next()
            .and_then(|n| n.parse::<usize>().ok());
        let Some((_, pick)) = i.and_then(|i| c.rows.get(i)).cloned() else {
            return;
        };
        let purpose = c.purpose;
        self.close_dialog();
        match (purpose, pick) {
            (ChooseFor::Insert, Pick::Cell(row)) => {
                let Some(cellref) = self.cellref_for(h, row) else {
                    self.session
                        .say("that cell is not referenceable from here (lib:cell or ./cell)");
                    return;
                };
                let at = self.pointer_world_snapped();
                self.tool = Tool::Place {
                    what: Placement::Inst(cellref),
                    at,
                    lin: Xform::IDENTITY,
                };
            }
            (ChooseFor::Open, Pick::Cell(row)) => match self.session.open_cell(row) {
                Ok(d) => self.show_opened(d),
                Err(e) => self.session.say(e),
            },
            (ChooseFor::Waves, Pick::File(f)) => self.open_waves(f),
            (ChooseFor::Merge, Pick::Cell(row)) => self.merge_cell(row),
            (ChooseFor::Delete, Pick::Cell(row)) => self.delete_cell(row),
            (ChooseFor::Compare, Pick::Cell(row)) => self.compare_with(row),
            _ => {}
        }
    }
    /// How document `h` references indexed cell `row`: `lib:cell` for a library root, or
    /// `./cell` for a project cell in `h`'s directory (netlist spec §1).
    pub(super) fn cellref_for(&self, h: DocHandle, row: usize) -> Option<String> {
        let p = &self.session.project;
        let cells = p.cells();
        let lib = &p.libs().name[usize::from(cells.lib[row])];
        let s = p.strings();
        let stem = s.get(cells.stem[row]);
        let candidates = [format!("./{stem}"), format!("{lib}:{stem}")];
        candidates
            .into_iter()
            .find(|c| p.resolve(h, c).ok() == Some(row))
    }
}
