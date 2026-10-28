//! Highlight, netlist, simulation, symbol and Help commands.
//!
//! Contents:
//! - `Editor::run_net`

use schemify_doc::{Edit, EditRequest, RecordBuf};
use schemify_format::{Orient, RecordKind};

use crate::commands::Cmd;
use crate::geometry::Xform;
use crate::{edit, spines};

use super::Editor;
use super::apply::push_req;
use super::chooser::ChooseFor;
use super::dialog::Dialog;
use super::highlight::{duplicate_names, overlapped};
use super::symbol::{PITCH, pins_from_ports};
use super::text::{expand_bus, help_text};
use super::tool::{Placement, Tool};

impl Editor {
    /// Highlight, netlist, simulation, symbol and Help commands. Returns whether anything visible changed.
    pub(super) fn run_net(
        &mut self,
        cmd: Cmd,
        arg: &str,
        node: &str,
        reqs: &mut Vec<EditRequest>,
    ) -> bool {
        let Some((h, _)) = self.active() else {
            return false;
        };
        let ptr = self.pointer_world_snapped();
        match cmd {
            Cmd::Highlight | Cmd::Unhighlight | Cmd::UnhighlightSelected => {
                self.highlight_cmd(h, cmd);
            }
            Cmd::Netlist | Cmd::NetlistTop => self.netlist(cmd == Cmd::Netlist),
            Cmd::SetDialect => {
                let word = if arg.is_empty() {
                    node.rsplit('.').next().unwrap_or("")
                } else {
                    arg.trim()
                };
                match spines::dialect_of(word) {
                    Some(d) => {
                        self.dialect = d;
                        self.session.say(format!("netlist dialect: {word}"));
                    }
                    None => self.session.say(format!("{word:?}: not a netlist dialect")),
                }
            }
            Cmd::ShowNetlist => self.show_netlist = !self.show_netlist,
            Cmd::Simulate | Cmd::Optimize => self.simulate(h, cmd == Cmd::Optimize, arg),
            Cmd::SimCancel => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.session.say(if self.sim.cancel() {
                        "simulation cancelled"
                    } else {
                        "nothing is running"
                    });
                }
            }
            Cmd::NewBench => self.new_bench(h),
            Cmd::ChecksPick => self.checks_pick(h, node),
            Cmd::SendToViewer | Cmd::SendHighlighted => self.send_to_viewer(),
            Cmd::ResultsPick
            | Cmd::ResultsWaves
            | Cmd::ResultsHistory
            | Cmd::ResultsSort
            | Cmd::ResultsFilter => {
                self.results_cmd(cmd, node, arg);
            }
            Cmd::OpAnnotate | Cmd::OpAnnotateGraphs => {
                self.op_on = !self.op_on;
                self.op_job = None;
                self.session.say(format!(
                    "OP annotation {}",
                    if self.op_on { "on" } else { "off" }
                ));
            }
            Cmd::AcceptStaged | Cmd::RejectStaged => {
                // The stage under review, or the one the suffix names (0-based).
                let n = self.session.project.stages(h).len();
                let pick = arg
                    .parse::<usize>()
                    .unwrap_or(self.stage_pick.min(n.saturating_sub(1)));
                let Some(id) = self.session.project.stages(h).get(pick).map(|st| st.id) else {
                    self.session.say("no staged edit on this document");
                    return true;
                };
                let edit = if cmd == Cmd::AcceptStaged {
                    Edit::Accept(id)
                } else {
                    Edit::Reject(id)
                };
                push_req(reqs, h, edit);
            }
            Cmd::ShowStaged => {
                let n = self.session.project.stages(h).len().max(1);
                self.stage_pick = match arg {
                    "next" => (self.stage_pick + 1) % n,
                    "prev" => (self.stage_pick + n - 1) % n,
                    k => k.parse::<usize>().map_or(self.stage_pick, |k| k.min(n - 1)),
                };
            }
            Cmd::TextFind => self.find_in_text(arg),
            Cmd::MakeSymbol => {
                let ops = self.make_symbol_ops(h);
                self.commit(reqs, h, "make symbol", ops);
            }
            Cmd::MakeSchematic => {
                let ops = self.make_schematic_ops(h, ptr);
                if ops.is_empty() {
                    self.session.say("every pin already has a port");
                }
                self.commit(reqs, h, "make schematic", ops);
            }
            Cmd::MakeSubcircuit => self.make_subcircuit(reqs, h),
            Cmd::AttachLabels => {
                let ops = self.attach_labels_ops(h);
                self.commit(reqs, h, "attach labels", ops);
            }
            Cmd::PinsFromPorts => {
                let ops = self.with_doc(h, pins_from_ports);
                if ops.is_empty() {
                    self.session.say("select schematic ports without a pin");
                }
                self.commit(reqs, h, "pins from ports", ops);
            }
            Cmd::ListNets | Cmd::ListNetsBuses => {
                let mut body = String::from("Highlighted nets\n\n");
                for k in &self.highlight {
                    if cmd == Cmd::ListNetsBuses {
                        for bit in expand_bus(k) {
                            body.push_str(&bit);
                            body.push('\n');
                        }
                    } else {
                        body.push_str(k);
                        body.push('\n');
                    }
                }
                self.text_tab("highlighted nets", "nets", body);
            }
            Cmd::LabelsFromNets | Cmd::LabelsFromNetsI | Cmd::PortsFromNets => {
                let names = self.highlighted_local_names();
                if names.is_empty() {
                    self.session.say("no highlighted nets at this level (k)");
                    return true;
                }
                let kind = if cmd == Cmd::PortsFromNets {
                    RecordKind::Port
                } else {
                    RecordKind::Label
                };
                let prefix = if cmd == Cmd::LabelsFromNetsI { "i" } else { "" };
                let recs: Vec<RecordBuf> = names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| {
                        edit::placed(
                            kind,
                            (0, i as i32 * PITCH),
                            Orient::R0,
                            &format!("{prefix}{n}"),
                            edit::AUTO_ID,
                        )
                    })
                    .collect();
                self.tool = Tool::Place {
                    what: Placement::Paste(recs),
                    at: ptr,
                    lin: Xform::IDENTITY,
                };
            }
            Cmd::CompareWith => self.open_chooser(ChooseFor::Compare),
            Cmd::CompareToggle => match self.compare.as_mut() {
                Some(c) => {
                    c.on = !c.on;
                    c.key = None;
                }
                None => self.open_chooser(ChooseFor::Compare),
            },
            Cmd::CompareSwap => self.compare_swap(h),
            Cmd::DupNames => {
                let keys = self.with_doc(h, |d, _| duplicate_names(d).keys);
                self.session
                    .say(format!("{} instance(s) with a duplicate name", keys.len()));
                self.set_selection(keys);
            }
            Cmd::RenameDups => {
                let ops = self.with_doc(h, |d, _| duplicate_names(d).ops);
                if ops.is_empty() {
                    self.session.say("no duplicate instance names");
                }
                self.commit(reqs, h, "rename duplicates", ops);
            }
            Cmd::PinMismatch => self.pin_mismatch(h),
            Cmd::SelectHighlighted => {
                let keys = self.hl.keys.clone();
                self.set_selection(keys);
            }
            Cmd::SelectOverlapped => {
                let keys = self.with_doc(h, |d, _| overlapped(d));
                self.session
                    .say(format!("{} overlapped instance(s)", keys.len()));
                self.set_selection(keys);
            }
            Cmd::Changelog => self.changelog(h),
            Cmd::EditNetlist => {
                let show = self.show_netlist;
                self.show_netlist = true;
                self.netlist(true);
                self.show_netlist = show;
            }
            Cmd::ListProcesses => self.list_processes(),
            Cmd::PackageStatus => self.package_status(),
            Cmd::JobData | Cmd::JobErrors => self.job_log(cmd == Cmd::JobErrors),
            Cmd::WavesClear => {
                self.waves = None;
                self.shell.invalidate();
            }
            Cmd::WavesLoad => {
                self.open_chooser(ChooseFor::Waves);
                if let Dialog::Choose(c) = &mut self.dialog {
                    c.note = arg.trim().to_owned();
                }
            }
            Cmd::About => self.text_tab(
                "About Schemify",
                "about",
                format!(
                    "Schemify {}\n\nA schematic editor: xschem's keys and menus (D25), \
                     line-record design files that diff and merge (D9, D34), one expression \
                     language (D12), plugins in WASM (D3).\n\nLicensed MIT OR Apache-2.0.\n",
                    env!("CARGO_PKG_VERSION")
                ),
            ),
            Cmd::Help => self.text_tab("Help", "help", help_text()),
            _ => unreachable!("{cmd:?}: not a net command"),
        }
        true
    }
}
