//! Cell panel and docs page commands (one undo each).
//!
//! Contents:
//! - `Editor::run_panel`

use schemify_doc::{EditRequest, Op, RecordBuf};
use schemify_format::RecordKind;

use crate::commands::Cmd;
use crate::{cell, docs};

use super::{Editor, symbol};
use crate::edit::Toward;

impl Editor {
    /// Cell panel and docs page commands (one undo each). Returns whether anything visible changed.
    pub(super) fn run_panel(
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
            Cmd::CellSet => {
                // From the panel the node is the field; from a script (RPC, init.lua) the
                // argument is `<field id> <value>`.
                let (node, arg) = if node.is_empty() {
                    arg.split_once(' ').unwrap_or((arg, ""))
                } else {
                    (node, arg)
                };
                let Some((field, choice)) = cell::parse_field(node) else {
                    return false;
                };
                let value = choice.unwrap_or(arg);
                match self.with_doc(h, |d, _| Some(cell::set_ops(d, field, value))) {
                    Some(Ok(ops)) => self.commit(reqs, h, "edit cell", ops),
                    Some(Err(why)) => self.session.say(why),
                    None => return false,
                }
            }
            Cmd::CellAddBench => {
                let word = node.strip_prefix("cell.add.").unwrap_or(arg);
                let Some(b) = cell::Bench::ALL.iter().copied().find(|b| b.word() == word) else {
                    self.session
                        .say(format!("cell.add_bench: unknown table {word}"));
                    return true;
                };
                match self.with_doc(h, |d, _| Some(cell::add_bench_ops(d, b))) {
                    Some(Ok(ops)) => self.commit(reqs, h, "add bench record", ops),
                    Some(Err(why)) => self.session.say(why),
                    None => return false,
                }
            }
            Cmd::CellAddParam => {
                let ops = self.with_doc(h, |d, _| cell::add_param_ops(d));
                self.commit(reqs, h, "add parameter", ops);
            }
            Cmd::CellAddImpl => {
                let lang = node.strip_prefix("cell.add_impl.").unwrap_or(arg);
                match self.with_doc(h, |d, _| Some(cell::add_impl_ops(d, lang))) {
                    Some(Ok(ops)) => self.commit(reqs, h, "add implementation", ops),
                    Some(Err(why)) => self.session.say(why),
                    None => return false,
                }
            }
            Cmd::CellDelete => {
                let node = if node.is_empty() { arg } else { node };
                let ops = self.with_doc(h, |d, _| {
                    if let Some(r) = cell::button_row(node, "param", "del") {
                        cell::delete_param_ops(d, r)
                    } else if let Some(r) = cell::button_row(node, "impl", "del") {
                        cell::delete_impl_ops(d, r)
                    } else if let Some((b, r)) = cell::Bench::ALL
                        .iter()
                        .find_map(|&b| cell::button_row(node, b.word(), "del").map(|r| (b, r)))
                    {
                        cell::delete_bench_ops(d, b, r)
                    } else {
                        Vec::new()
                    }
                });
                self.commit(reqs, h, "delete", ops);
            }
            Cmd::CellPinUp | Cmd::CellPinDown => {
                let node = if node.is_empty() { arg } else { node };
                let (toward, action) = if cmd == Cmd::CellPinUp {
                    (Toward::Earlier, "up")
                } else {
                    (Toward::Later, "down")
                };
                let Some(r) = cell::button_row(node, "pin", action) else {
                    return false;
                };
                let ops = self.with_doc(h, |d, _| cell::move_pin_ops(d, r, toward));
                self.commit(reqs, h, "reorder pins", ops);
            }
            Cmd::CellSyncPorts | Cmd::CellSymbolFromPorts => {
                let node = if node.is_empty() { arg } else { node };
                let sync = cmd == Cmd::CellSyncPorts;
                let action = if sync { "sync" } else { "symbol" };
                let Some(r) = cell::button_row(node, "impl", action) else {
                    return false;
                };
                self.port_sync = None;
                let ports = match self.with_doc(h, |d, _| Some(cell::impl_ports(d, r))) {
                    Some(Ok(p)) => p,
                    Some(Err(why)) => {
                        self.session.say(why);
                        return true;
                    }
                    None => return false,
                };
                if sync {
                    let epoch = self.session.epoch(h);
                    let p = cell::PortSync { impl_row: r, ports };
                    self.port_sync = Some((h, epoch, p));
                    self.session
                        .say("sync pins: review the changes, then Apply");
                } else {
                    let w = self.opts.symbol_width;
                    let ops = self.with_doc(h, |d, _| symbol::symbol_from_ports_ops(d, &ports, w));
                    if ops.is_empty() {
                        self.session
                            .say("symbol from ports: the body declares no ports");
                    }
                    self.commit(reqs, h, "symbol from ports", ops);
                }
            }
            Cmd::CellSyncApply => {
                let epoch = self.session.epoch(h);
                let Some((_, _, p)) = self
                    .port_sync
                    .take()
                    .filter(|(d, e, _)| *d == h && *e == epoch)
                else {
                    self.session.say("sync pins: nothing pending");
                    return true;
                };
                let ops = self.with_doc(h, |d, _| cell::sync_ops(d, &p.ports).ops);
                if ops.is_empty() {
                    self.session.say("pins already match the ports");
                }
                self.commit(reqs, h, "sync pins from ports", ops);
            }
            Cmd::CellSyncCancel => self.port_sync = None,
            Cmd::DocsSelect => {
                self.page.sel = docs::row_of(node).or_else(|| arg.parse::<u32>().ok())
            }
            Cmd::DocsEdit => {
                let Some(row) = docs::row_of(node).or_else(|| arg.parse::<u32>().ok()) else {
                    return false;
                };
                self.page.sel = Some(row);
                let text = self
                    .session
                    .project
                    .doc(h)
                    .filter(|d| docs::kind_of(d, row) == Some(RecordKind::Prose))
                    .map(|d| docs::prose_text(d, row));
                if let Some(text) = text {
                    self.page.editing = Some((row, text));
                    self.page.focused = false;
                    self.focus_next = Some(docs::editor_id(row));
                }
            }
            Cmd::DocsVarOk => {
                let Some(row) = docs::row_of(node) else {
                    return false;
                };
                self.page.sel = Some(row);
                let ops = self.with_doc(h, |d, _| docs::var_ops(d, row, arg));
                self.commit_docs(reqs, h, "edit variable", ops);
            }
            Cmd::DocsProseOk => {
                if let Some((row, typed)) = self.page.editing.take() {
                    let text = if arg.is_empty() {
                        typed
                    } else {
                        arg.to_owned()
                    };
                    let ops = self.with_doc(h, |d, _| docs::prose_ops(d, row, &text));
                    self.commit_docs(reqs, h, "edit text", ops);
                }
            }
            Cmd::DocsAddProse | Cmd::DocsAddVar => {
                let Some(doc) = self.session.project.doc(h) else {
                    return false;
                };
                let row = docs::insert_row(doc, self.page.sel);
                let (label, ops) = if cmd == Cmd::DocsAddProse {
                    ("add text", docs::add_prose(row))
                } else {
                    ("add variable", docs::add_var(doc, row))
                };
                self.commit_docs(reqs, h, label, ops);
                self.page.sel = Some(row);
                if let Some(t) = self.session.tab_view_mut() {
                    *t.docs = true;
                }
                if cmd == Cmd::DocsAddVar {
                    self.focus_next = Some(format!("docs.b.{row}.expr"));
                }
            }
            Cmd::DocsUp | Cmd::DocsDown => {
                let Some(row) = self.page.sel else {
                    self.session.say("select a docs block first");
                    return true;
                };
                let toward = if cmd == Cmd::DocsUp {
                    Toward::Earlier
                } else {
                    Toward::Later
                };
                let ops = self.with_doc(h, |d, _| docs::move_block(d, row, toward));
                if !ops.is_empty() {
                    assert!(
                        toward == Toward::Later || row > 0,
                        "the first block has none above"
                    );
                    self.page.sel = Some(match toward {
                        Toward::Earlier => row - 1,
                        Toward::Later => row + 1,
                    });
                }
                self.commit_docs(reqs, h, "move block", ops);
            }
            Cmd::DocsDelete => {
                let Some(row) = self.page.sel.take() else {
                    self.session.say("select a docs block first");
                    return true;
                };
                self.page.editing = None;
                let ops = self.with_doc(h, |d, _| docs::delete_block(d, row));
                self.commit_docs(reqs, h, "delete block", ops);
            }
            Cmd::AddGraph => {
                let Some(doc) = self.session.project.doc(h) else {
                    return false;
                };
                let row = crate::docs::insert_row(doc, self.page.sel);
                let rec = RecordBuf::new(RecordKind::Prose)
                    .field(schemify_doc::NEW_ID)
                    .verbatim("```plot")
                    .verbatim("v(out)")
                    .verbatim("```");
                self.commit_docs(
                    reqs,
                    h,
                    "add graph",
                    vec![Op::Insert { rec, at: Some(row) }],
                );
                if let Some(t) = self.session.tab_view_mut() {
                    *t.docs = true;
                }
            }
            _ => unreachable!("{cmd:?}: not a panel command"),
        }
        true
    }
}
