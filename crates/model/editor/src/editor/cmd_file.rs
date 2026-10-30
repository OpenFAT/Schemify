//! File, hierarchy and tab commands: open, save, export, descend, close, quit.
//!
//! Contents:
//! - `Editor::run_file`

use schemify_doc::EditRequest;
use schemify_format::FileKind;
use schemify_plugin::abi::Event;

use crate::commands::Cmd;
use crate::session::View;
use crate::{platform, plugins};

use super::Editor;
use super::chooser::ChooseFor;
use super::dialog::{Dialog, TextFor};
use super::tool::Tool;

impl Editor {
    /// File, hierarchy and tab commands: open, save, export, descend, close, quit. Returns whether anything visible changed.
    pub(super) fn run_file(
        &mut self,
        cmd: Cmd,
        arg: &str,
        node: &str,
        reqs: &mut Vec<EditRequest>,
    ) -> bool {
        let Some((h, _)) = self.active() else {
            return false;
        };
        match cmd {
            Cmd::Descend | Cmd::DescendSymbol | Cmd::OpenSubTab | Cmd::OpenSymbolTab => {
                self.descend(h, cmd);
            }
            Cmd::Ascend => {
                self.tool = Tool::Idle;
                if !self.session.pop_level() {
                    self.session.say("already at the top level");
                }
            }
            Cmd::NewSchematic | Cmd::TabNew | Cmd::NewWindowTab | Cmd::NewSymbol => {
                let (kind, v) = if cmd == Cmd::NewSymbol {
                    (FileKind::Shprim, View::Symbol)
                } else {
                    (FileKind::Shcomp, View::Schematic)
                };
                match self.session.create(kind) {
                    Ok(d) => self.session.open_tab(d, v),
                    Err(e) => self.session.say(e),
                }
            }
            // With an argument (a script, a JSON-RPC caller): that cell, no dialog.
            Cmd::Open if !arg.trim().is_empty() => self.open_named(arg),
            Cmd::Open => self.open_chooser(ChooseFor::Open),
            Cmd::ComponentBrowser | Cmd::InsertSymbol => self.open_chooser(ChooseFor::Insert),
            Cmd::Waves => self.open_chooser(ChooseFor::Waves),
            Cmd::Save => {
                let top = self.session.tabs()[self.session.active()].stack[0].doc;
                for d in [h, top] {
                    match self.session.save(d) {
                        Ok(true) => {
                            self.session.say(format!("saved {}", self.session.name(d)));
                            let _ = self.host.push(
                                None,
                                &Event::Saved {
                                    doc: plugins::doc_id(d),
                                },
                            );
                        }
                        Ok(false) => {}
                        Err(e) => self.session.say(e),
                    }
                }
            }
            Cmd::Reload => self.reload(h),
            Cmd::Close => self.close_tab(),
            Cmd::Quit => {
                let dirty = self
                    .session
                    .project
                    .handles()
                    .any(|d| self.session.project.is_dirty(d));
                if dirty && self.confirm != Some(Cmd::Quit) {
                    self.confirm = Some(Cmd::Quit);
                    self.session
                        .say("unsaved changes: Ctrl+Q again to quit without saving");
                } else {
                    self.settle();
                    self.quit = true;
                }
                assert!(
                    self.quit || self.confirm == Some(Cmd::Quit),
                    "quit or ask first"
                );
            }
            Cmd::TabSelect => {
                let i = node
                    .rsplit('.')
                    .next()
                    .and_then(|n| n.parse::<usize>().ok());
                if let Some(i) = i.filter(|&i| i < self.session.tabs().len()) {
                    self.switch_tab(i);
                }
            }
            Cmd::TabNext | Cmd::TabPrev => {
                let n = self.session.tabs().len();
                assert!(n > 0, "an active document has a tab");
                let i = if cmd == Cmd::TabNext {
                    (self.session.active() + 1) % n
                } else {
                    (self.session.active() + n - 1) % n
                };
                self.switch_tab(i);
            }
            Cmd::SaveAs | Cmd::SaveAsSymbol => {
                let symbol = cmd == Cmd::SaveAsSymbol;
                if arg.trim().is_empty() {
                    let current = self
                        .session
                        .path(h)
                        .and_then(|p| crate::libs::rel_path(&self.session.root, p))
                        .unwrap_or_default();
                    let title = if symbol { "Save symbol as" } else { "Save as" };
                    self.open_text(TextFor::SaveAs { symbol }, title);
                    if let Dialog::Text { typed, .. } = &mut self.dialog {
                        *typed = current;
                    }
                } else {
                    self.save_as(h, arg.trim(), symbol);
                }
            }
            Cmd::Merge => self.open_chooser(ChooseFor::Merge),
            Cmd::OpenMostRecent => self.open_recent(0, reqs),
            Cmd::OpenRecent => match arg.trim().parse::<usize>() {
                Ok(i) => self.open_recent(i, reqs),
                Err(_) => self
                    .session
                    .say(format!("{arg:?}: not a recent-file index")),
            },
            Cmd::DeleteFiles => {
                self.pending_delete = None;
                self.open_chooser(ChooseFor::Delete);
            }
            Cmd::ExportPng
            | Cmd::ExportPngFull
            | Cmd::ExportSvg
            | Cmd::ExportPdf
            | Cmd::ExportPdfFull
            | Cmd::ExportPdfHier
            | Cmd::ExportEps => self.export(h, cmd, arg),
            Cmd::ExportProject => {
                self.session
                    .say(match platform::export_project(&self.session.root) {
                        Ok(p) => format!("exported project {}", p.display()),
                        Err(e) => format!("export project: {e}"),
                    });
            }
            Cmd::ImportProject => {
                self.session
                    .say(match platform::import_project(arg.trim() == "folder") {
                        Ok(()) => "import project: pick a zip or folder".into(),
                        Err(e) => format!("import project: {e}"),
                    });
            }
            _ => unreachable!("{cmd:?}: not a file command"),
        }
        true
    }
}
