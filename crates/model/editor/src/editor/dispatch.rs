//! Command dispatch: a command name → a core command's arm (`run`, `run_cmd`) or a plugin event.
//!
//! Contents:
//! - `pub(super) fn kind_refusal`
//! - `Editor::dispatch_all`
//! - `Editor::dispatch`
//! - `Editor::run`
//! - `Editor::run_cmd`

use schemify_doc::{Document, EditRequest};
use schemify_format::FileKind;
use schemify_plugin::PluginId;
use schemify_plugin::abi::Event;
use schemify_ui::CommandRequests;

use crate::commands::{Cmd, Target};

use super::Editor;
use super::dialog::Dialog;

/// A `.shprim` has only a symbol; everything else opens on its schematic.
/// The status line for `cmd` refused on a file of `kind`: what the command needs.
pub(super) fn kind_refusal(cmd: Cmd, kind: FileKind) -> String {
    let ext = |k: &FileKind| format!(".{}", format!("{k:?}").to_lowercase());
    let want: Vec<String> = cmd.file_kinds().unwrap_or(&[]).iter().map(ext).collect();
    debug_assert!(!want.is_empty(), "only a restricted command is refused");
    format!(
        "{}: needs a {} file (this is a {})",
        cmd.name(),
        want.join(" or "),
        ext(&kind)
    )
}

impl Editor {
    pub(super) fn dispatch_all(
        &mut self,
        cmds: &CommandRequests,
        reqs: &mut Vec<EditRequest>,
    ) -> bool {
        let mut changed = false;
        for i in 0..cmds.len() {
            let name = self.shell.strings.get(cmds.command[i]).to_owned();
            let node = if cmds.node[i].is_none() {
                String::new()
            } else {
                self.shell.strings.get(cmds.node[i]).to_owned()
            };
            changed |= self.dispatch(&name, cmds.arg(i), &node, true, reqs).0;
        }
        changed
    }
    /// Runs command `name` with `arg` (else its `<name>.<word>` suffix): a core command, or
    /// an `Event::Command` to the plugin that owns it (returned once queued). An unknown name
    /// goes to the user config (a Lua keymap function, a lazy package) when `user`. Returns
    /// whether anything changed.
    pub(super) fn dispatch(
        &mut self,
        name: &str,
        arg: &str,
        node: &str,
        user: bool,
        reqs: &mut Vec<EditRequest>,
    ) -> (bool, Option<PluginId>) {
        let (target, suffix) = self.commands.resolve_arg(name);
        let arg = if arg.is_empty() { suffix } else { arg };
        match target {
            Target::Core(cmd) => (self.run(cmd, arg, node, reqs), None),
            Target::Plugin(id) => match self
                .host
                .push(Some(id), &Event::Command { name, args: arg })
            {
                Ok(()) => (true, Some(id)),
                Err(e) => {
                    self.session.say(e);
                    (true, None)
                }
            },
            // A Lua keymap function or a lazy package's first command (config spec §3, §5).
            Target::Unknown if user && self.host.user_command(name, arg) => (true, None),
            Target::Unknown => {
                self.session.say(format!("{name}: not available yet"));
                (true, None)
            }
        }
    }
    /// Runs one core command. Returns whether anything visible changed.
    #[allow(clippy::too_many_lines)]
    pub(super) fn run(
        &mut self,
        cmd: Cmd,
        arg: &str,
        node: &str,
        reqs: &mut Vec<EditRequest>,
    ) -> bool {
        if cmd == Cmd::FieldTyped {
            // Typing in a field: allowed in every dialog and tab, changes no document.
            return self.field_typed(node, arg);
        }
        if matches!(self.dialog, Dialog::Ctx(_)) && cmd != Cmd::DialogClose {
            // A context-menu row: the menu closes and its command runs as from the menubar.
            self.close_dialog();
        }
        let modal = !matches!(self.dialog, Dialog::None) || !self.shell.ui.dialog().is_none();
        let dialog_cmd = matches!(
            cmd,
            Cmd::DialogClose
                | Cmd::ChooseFilter
                | Cmd::ChoosePick
                | Cmd::EnterTextOk
                | Cmd::PropsOk
                | Cmd::PropsField
                | Cmd::EditFileOk
                | Cmd::Recover
                | Cmd::DiscardRecovery
        ) || (cmd == Cmd::ToggleLayer && matches!(self.dialog, Dialog::Layers));
        if modal && !dialog_cmd {
            return false; // xschem: editing keys are no-ops while a dialog is open
        }
        if cmd != Cmd::Close && cmd != Cmd::Quit {
            self.confirm = None;
        }
        if cmd == Cmd::ProjectReload {
            // Project-wide: needs no active document.
            self.reload_project();
            return true;
        }
        let Some((h, _)) = self.active() else {
            return false;
        };
        // A command that acts on other file kinds (its UI nodes are greyed out, but a key or a
        // script can still name it) changes nothing but the status line.
        if let Some(kind) = self.session.project.doc(h).map(Document::kind)
            && !cmd.accepts(kind)
        {
            self.session.say(kind_refusal(cmd, kind));
            return true;
        }
        if self.session.text_view().is_some() && !text_tab_cmd(cmd) {
            self.session
                .say("read-only text view: switch to a schematic tab");
            return true;
        }
        self.run_cmd(cmd, arg, node, reqs)
    }
    /// The arm of core command `cmd`, past [`Editor::run`]'s guards (a dialog, the file kind,
    /// a text tab): each menu's commands run in their own file. Returns whether anything
    /// visible changed.
    pub(super) fn run_cmd(
        &mut self,
        cmd: Cmd,
        arg: &str,
        node: &str,
        reqs: &mut Vec<EditRequest>,
    ) -> bool {
        match cmd {
            // Taken by the guards in `run` (they need no active document).
            Cmd::FieldTyped | Cmd::ProjectReload => unreachable!("{cmd:?}: handled in run"),
            Cmd::Undo
            | Cmd::Redo
            | Cmd::Delete
            | Cmd::SelectAll
            | Cmd::Copy
            | Cmd::Cut
            | Cmd::Paste
            | Cmd::Duplicate
            | Cmd::Move
            | Cmd::MoveKiss
            | Cmd::MoveStretch
            | Cmd::Rotate
            | Cmd::Flip
            | Cmd::VFlip
            | Cmd::RotateInPlace
            | Cmd::FlipInPlace
            | Cmd::VFlipInPlace
            | Cmd::ConstrainH
            | Cmd::ConstrainV
            | Cmd::Unconstrained
            | Cmd::PlaceLabel
            | Cmd::PlaceWireLabel
            | Cmd::PlaceInPort
            | Cmd::PlaceOutPort
            | Cmd::InsertText
            | Cmd::PlaceSymPin
            | Cmd::InsertWire
            | Cmd::InsertSnapWire
            | Cmd::InsertLine
            | Cmd::InsertRect
            | Cmd::CycleManhattan
            | Cmd::UnselectUnderMouse
            | Cmd::UnselectArea
            | Cmd::InsertPolygon
            | Cmd::InsertArc
            | Cmd::InsertCircle
            | Cmd::ClosePolygon
            | Cmd::AlignGrid
            | Cmd::BreakAtMouse
            | Cmd::BreakAtMouseAlign
            | Cmd::BreakAtPins
            | Cmd::RemoveThrough
            | Cmd::JoinTrim
            | Cmd::Search
            | Cmd::SelectConnected
            | Cmd::SelectConnectedStop
            | Cmd::ToggleIgnore => self.run_edit(cmd, arg, node, reqs),
            Cmd::Descend
            | Cmd::DescendSymbol
            | Cmd::OpenSubTab
            | Cmd::OpenSymbolTab
            | Cmd::Ascend
            | Cmd::NewSchematic
            | Cmd::TabNew
            | Cmd::NewWindowTab
            | Cmd::NewSymbol
            | Cmd::Open
            | Cmd::ComponentBrowser
            | Cmd::InsertSymbol
            | Cmd::Waves
            | Cmd::Save
            | Cmd::Reload
            | Cmd::Close
            | Cmd::Quit
            | Cmd::TabSelect
            | Cmd::TabNext
            | Cmd::TabPrev
            | Cmd::SaveAs
            | Cmd::SaveAsSymbol
            | Cmd::Merge
            | Cmd::OpenMostRecent
            | Cmd::OpenRecent
            | Cmd::DeleteFiles
            | Cmd::ExportPng
            | Cmd::ExportPngFull
            | Cmd::ExportSvg
            | Cmd::ExportPdf
            | Cmd::ExportPdfFull
            | Cmd::ExportPdfHier
            | Cmd::ExportEps
            | Cmd::ExportProject
            | Cmd::ImportProject => self.run_file(cmd, arg, node, reqs),
            Cmd::Props
            | Cmd::PropsEditor
            | Cmd::PropsView
            | Cmd::PropsField
            | Cmd::PropsOk
            | Cmd::DialogClose
            | Cmd::ChooseFilter
            | Cmd::ChoosePick
            | Cmd::EnterTextOk
            | Cmd::EditFile
            | Cmd::EditFileOk
            | Cmd::EditHeader
            | Cmd::ContextMenu
            | Cmd::Recover
            | Cmd::DiscardRecovery => self.run_dialog(cmd, arg, node, reqs),
            Cmd::ChecksToggle
            | Cmd::ResultsToggle
            | Cmd::HalfSnap
            | Cmd::DoubleSnap
            | Cmd::DrawGrid
            | Cmd::Orthogonal
            | Cmd::ToggleTheme
            | Cmd::SetSnap
            | Cmd::SetGrid
            | Cmd::PanLeft
            | Cmd::PanRight
            | Cmd::PanUp
            | Cmd::PanDown
            | Cmd::PanCenter
            | Cmd::ZoomIn
            | Cmd::ZoomOut
            | Cmd::ZoomFull
            | Cmd::ZoomSelection
            | Cmd::ZoomBox
            | Cmd::Redraw
            | Cmd::DocsToggle
            | Cmd::DocsSource
            | Cmd::ViewSchematic
            | Cmd::ViewSymbol
            | Cmd::CellToggle
            | Cmd::AutoTrim
            | Cmd::ExportColor
            | Cmd::ExportTransparent
            | Cmd::Crosshair
            | Cmd::SnapCursor
            | Cmd::EnableStretch
            | Cmd::FlatNetlist
            | Cmd::PersistentWire
            | Cmd::GridAxes
            | Cmd::ToolbarToggle
            | Cmd::Fullscreen
            | Cmd::SymbolText
            | Cmd::FillToggle
            | Cmd::VariableWidth
            | Cmd::AutoHighlight
            | Cmd::HighlightInstances
            | Cmd::IncrementColor
            | Cmd::OnlyProbes
            | Cmd::AutoHighlightPlotted
            | Cmd::CrosshairSize
            | Cmd::LineWidth
            | Cmd::DimColors
            | Cmd::SymbolWidth
            | Cmd::GridDialog
            | Cmd::SnapDialog
            | Cmd::ExportPage
            | Cmd::PngScale
            | Cmd::CycleDialect
            | Cmd::LineWidthDown
            | Cmd::LineWidthUp
            | Cmd::LayerColor
            | Cmd::ResetColors
            | Cmd::LayersDialog
            | Cmd::ToggleLayer
            | Cmd::SetLayer
            | Cmd::AllLayers
            | Cmd::CurrentLayerOnly
            | Cmd::ErcWindow
            | Cmd::SymbolsDetails
            | Cmd::SymbolsBoxesSub
            | Cmd::SymbolsBoxesAll
            | Cmd::SymbolsCycle => self.run_view(cmd, arg, node, reqs),
            Cmd::CellSet
            | Cmd::CellAddBench
            | Cmd::CellAddParam
            | Cmd::CellAddImpl
            | Cmd::CellDelete
            | Cmd::CellPinUp
            | Cmd::CellPinDown
            | Cmd::CellSyncPorts
            | Cmd::CellSymbolFromPorts
            | Cmd::CellSyncApply
            | Cmd::CellSyncCancel
            | Cmd::DocsSelect
            | Cmd::DocsEdit
            | Cmd::DocsVarOk
            | Cmd::DocsProseOk
            | Cmd::DocsAddProse
            | Cmd::DocsAddVar
            | Cmd::DocsUp
            | Cmd::DocsDown
            | Cmd::DocsDelete
            | Cmd::AddGraph => self.run_panel(cmd, arg, node, reqs),
            Cmd::Highlight
            | Cmd::Unhighlight
            | Cmd::UnhighlightSelected
            | Cmd::Netlist
            | Cmd::NetlistTop
            | Cmd::SetDialect
            | Cmd::ShowNetlist
            | Cmd::Simulate
            | Cmd::Optimize
            | Cmd::SimCancel
            | Cmd::NewBench
            | Cmd::ChecksPick
            | Cmd::SendToViewer
            | Cmd::SendHighlighted
            | Cmd::ResultsPick
            | Cmd::ResultsWaves
            | Cmd::ResultsHistory
            | Cmd::ResultsSort
            | Cmd::ResultsFilter
            | Cmd::OpAnnotate
            | Cmd::OpAnnotateGraphs
            | Cmd::AcceptStaged
            | Cmd::RejectStaged
            | Cmd::ShowStaged
            | Cmd::TextFind
            | Cmd::MakeSymbol
            | Cmd::MakeSchematic
            | Cmd::MakeSubcircuit
            | Cmd::AttachLabels
            | Cmd::PinsFromPorts
            | Cmd::ListNets
            | Cmd::ListNetsBuses
            | Cmd::LabelsFromNets
            | Cmd::LabelsFromNetsI
            | Cmd::PortsFromNets
            | Cmd::CompareWith
            | Cmd::CompareToggle
            | Cmd::CompareSwap
            | Cmd::DupNames
            | Cmd::RenameDups
            | Cmd::PinMismatch
            | Cmd::SelectHighlighted
            | Cmd::SelectOverlapped
            | Cmd::Changelog
            | Cmd::EditNetlist
            | Cmd::ListProcesses
            | Cmd::PackageStatus
            | Cmd::JobData
            | Cmd::JobErrors
            | Cmd::WavesClear
            | Cmd::WavesLoad
            | Cmd::About
            | Cmd::Help => self.run_net(cmd, arg, node, reqs),
        }
    }
}

// ---- private ----

/// Whether `cmd` makes sense on a read-only text tab: navigation, panels, the run, another
/// text or dialog, a view option (nothing that edits the level behind the tab).
fn text_tab_cmd(cmd: Cmd) -> bool {
    matches!(
        cmd,
        Cmd::TabSelect
            | Cmd::TabNext
            | Cmd::TabPrev
            | Cmd::TabNew
            | Cmd::NewSchematic
            | Cmd::NewSymbol
            | Cmd::Close
            | Cmd::Quit
            | Cmd::Open
            | Cmd::Waves
            | Cmd::ChecksToggle
            | Cmd::ResultsToggle
            | Cmd::ResultsPick
            | Cmd::ResultsWaves
            | Cmd::ResultsHistory
            | Cmd::ResultsSort
            | Cmd::ResultsFilter
            | Cmd::SimCancel
            | Cmd::SetDialect
            | Cmd::ShowNetlist
            | Cmd::OpAnnotate
            | Cmd::SendToViewer
            | Cmd::SendHighlighted
            | Cmd::Redraw
            | Cmd::ToggleTheme
            | Cmd::DialogClose
            | Cmd::ChooseFilter
            | Cmd::ChoosePick
            | Cmd::TextFind
            | Cmd::About
            | Cmd::Help
            | Cmd::Changelog
            | Cmd::ListProcesses
            | Cmd::PackageStatus
            | Cmd::JobData
            | Cmd::JobErrors
            | Cmd::ListNets
            | Cmd::ListNetsBuses
            | Cmd::EditNetlist
            | Cmd::WavesClear
            | Cmd::WavesLoad
            | Cmd::CycleDialect
            | Cmd::FlatNetlist
            | Cmd::Fullscreen
            | Cmd::ToolbarToggle
            | Cmd::OpenRecent
            | Cmd::OpenMostRecent
            | Cmd::NewWindowTab
            | Cmd::Recover
            | Cmd::DiscardRecovery
    )
}
