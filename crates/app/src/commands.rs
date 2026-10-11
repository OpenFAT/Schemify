//! `commands`: the one command table (D27) that menus, the keymap, plugins, and later the CLI
//! and JSON-RPC all name. Core commands are a closed enum ([`Cmd`]) so dispatch is an
//! exhaustive `match`; plugin commands are rows from the composed `commands` contributions.
//!
//! Data design: ~120 core rows (static, sorted by name) + ≤ [`PLUGIN_COMMANDS_MAX`] plugin
//! rows (session, rebuilt when the plugin composition changes); looked up by name once per
//! command request (binary search). Names are `<menu>.<action>` words.

use schemify_plugin::PluginId;
use schemify_plugin::abi::Kind;

/// Plugin command rows (ARCHITECTURE: commands capacity 2^12).
pub const PLUGIN_COMMANDS_MAX: usize = 1 << 12;

macro_rules! commands {
    ($($(#[$m:meta])* $V:ident = $name:literal,)*) => {
        /// A core command. Variants are dispatched by the frame spine; names are the public,
        /// stable spelling that UI records, keymaps and plugins use.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u8)]
        pub enum Cmd { $($(#[$m])* $V,)* }

        /// `(name, command)` for every core command, sorted by name.
        pub const CORE: &[(&str, Cmd)] = &[$(($name, Cmd::$V),)*];
    };
}

commands! {
    /// Click on a row of the checks panel: select and zoom to its object.
    ChecksPick = "checks.pick",
    /// Enter in the chooser filter.
    ChooseFilter = "choose.filter",
    /// Pick a chooser row (insert symbol / open cell / waves file).
    ChoosePick = "choose.pick",
    /// Close the open dialog.
    DialogClose = "dialog.close",
    /// OK in the enter-text dialog.
    EnterTextOk = "dialog.enter_text.ok",
    /// Docs page: insert a text block after the selected block.
    DocsAddProse = "docs.add_prose",
    /// Docs page: insert a variable after the selected block.
    DocsAddVar = "docs.add_var",
    /// Docs page: delete the selected block.
    DocsDelete = "docs.delete_block",
    /// Docs page: click on a text block (select it and open its editor).
    DocsEdit = "docs.edit",
    /// Docs page: move the selected block down.
    DocsDown = "docs.move_down",
    /// Docs page: move the selected block up.
    DocsUp = "docs.move_up",
    /// Docs page: apply the text block editor (Ctrl+Enter).
    DocsProseOk = "docs.prose_ok",
    /// Docs page: select a block (click on a variable's name).
    DocsSelect = "docs.select",
    /// Docs page: a variable's expression field submitted (Enter).
    DocsVarOk = "docs.var_ok",
    /// Commit the active document's staged edit (visual diff reviewed, D18).
    AcceptStaged = "edit.accept_staged",
    /// Constrain the next move/wire horizontally.
    ConstrainH = "edit.constrained_horizontal_move",
    /// Constrain the next move/wire vertically.
    ConstrainV = "edit.constrained_vertical_move",
    /// Copy the selection to the clipboard.
    Copy = "edit.copy",
    /// Cut the selection to the clipboard.
    Cut = "edit.cut",
    /// Delete the selection.
    Delete = "edit.delete",
    /// Duplicate the selection attached to the pointer (xschem `c`).
    Duplicate = "edit.duplicate_objects",
    /// Flip each selected object about its own anchor.
    FlipInPlace = "edit.horizontal_flip_in_place_selected_object",
    /// Flip the selection as a group.
    Flip = "edit.horizontal_flip_selected_objects",
    /// Move the selection.
    Move = "edit.move_objects",
    /// Move, inserting wires at touching pins (xschem "kissing"; moves for now).
    MoveKiss = "edit.move_objects_adding_wires_to_connected_p",
    /// Move stretching attached wires (moves for now).
    MoveStretch = "edit.move_objects_stretching_attached_wires",
    /// Open the selected instance's schematic in a new tab.
    OpenSubTab = "edit.open_sub_schematic_tab",
    /// Open the selected instance's symbol in a new tab.
    OpenSymbolTab = "edit.open_symbol_tab",
    /// Paste the clipboard attached to the pointer.
    Paste = "edit.paste",
    /// Return to the parent schematic.
    Ascend = "edit.pop",
    /// Descend into the selected instance's schematic.
    Descend = "edit.push_schematic",
    /// Descend into the selected instance's symbol.
    DescendSymbol = "edit.push_symbol",
    /// Redo.
    Redo = "edit.redo",
    /// Drop the active document's staged edit.
    RejectStaged = "edit.reject_staged",
    /// Rotate each selected object about its own anchor.
    RotateInPlace = "edit.rotate_in_place_selected_objects",
    /// Rotate the selection as a group.
    Rotate = "edit.rotate_selected_objects",
    /// Select every object.
    SelectAll = "edit.select_all",
    /// Remove the axis constraint.
    Unconstrained = "edit.unconstrained_move",
    /// Undo.
    Undo = "edit.undo",
    /// Unselect the object under the pointer.
    UnselectUnderMouse = "edit.unselect_under_mouse",
    /// Vertical flip of each selected object about its anchor.
    VFlipInPlace = "edit.vertical_flip_in_place_selected_objects",
    /// Vertical flip of the selection as a group.
    VFlip = "edit.vertical_flip_selected_objects",
    /// New empty schematic in a new tab.
    NewSchematic = "file.clear_schematic",
    /// New empty symbol in a new tab.
    NewSymbol = "file.clear_symbol",
    /// Close the active tab.
    Close = "file.close_schematic",
    /// The insert-symbol chooser.
    ComponentBrowser = "file.component_browser",
    /// The open-cell chooser.
    Open = "file.open",
    /// Quit.
    Quit = "file.quit_xschem",
    /// Reload the active document from disk.
    Reload = "file.reload",
    /// Save the active document.
    Save = "file.save",
    /// Highlight the nets of the selection or under the pointer, across the hierarchy (`k`).
    Highlight = "highlight.highlight_selected_net_pins",
    /// Send the highlighted nets to the waves dock as `v(<net>)` traces (`Alt+G`), opening it
    /// on the selected job when closed.
    SendToViewer = "highlight.send_selected_net_pins_to_viewer",
    /// Clear every highlight (`Shift+K`).
    Unhighlight = "highlight.un_highlight_all_net_pins",
    /// Clear the highlight of the selected nets (`Ctrl+K`).
    UnhighlightSelected = "highlight.un_highlight_selected_net_pins",
    /// Double the snap.
    DoubleSnap = "options.double_snap_threshold",
    /// Toggle the grid.
    DrawGrid = "options.draw_grid",
    /// Toggle automatic L routing.
    Orthogonal = "options.enable_orthogonal_wiring",
    /// Halve the snap.
    HalfSnap = "options.half_snap_threshold",
    /// Properties dialog of the selection.
    Props = "properties.edit",
    /// Properties dialog (xschem: external editor; same dialog here).
    PropsEditor = "properties.edit_with_editor",
    /// Properties dialog (xschem: read-only view; same dialog here).
    PropsView = "properties.view",
    /// Field submitted in the properties dialog.
    PropsField = "props.field",
    /// OK in the properties dialog.
    PropsOk = "props.ok",
    /// Results panel: load an earlier run of the bench (`results.hist.<i>`, results history).
    ResultsHistory = "results.history",
    /// Results panel: pick a job (waves, plots and OP annotation follow it).
    ResultsPick = "results.pick",
    /// Results panel: open the selected job's raw file in the waves dock.
    ResultsWaves = "results.waves",
    /// Cancel the simulation or optimization in flight.
    SimCancel = "sim.cancel",
    /// Netlist dialect (`sim.dialect.<word>` menu items, or the word as argument).
    SetDialect = "sim.dialect",
    /// Netlist the tab's top cell (`n`): written to `.schemify/netlist/`, shown in a tab.
    Netlist = "sim.netlist",
    /// Netlist the cell shown at this hierarchy level (`Shift+N`).
    NetlistTop = "sim.netlist_top_only",
    /// New bench from this component (D11b): a `.shbench` instantiating the active cell.
    NewBench = "sim.new_bench",
    /// Run the bench's `optimize` record (D18); the best point becomes a staged edit.
    Optimize = "sim.optimize",
    /// Simulate the active bench (`s`; only `.shbench`, D11b), non-blocking.
    Simulate = "sim.simulate",
    /// Send the highlighted nets to the waves dock (xschem `Ctrl+Shift+X`; same as `Alt+G`).
    SendHighlighted = "simulation.send_highlighted_nets_to_viewer",
    /// Toggle opening the netlist in a text tab after netlisting (xschem `Shift+A`).
    ShowNetlist = "simulation.show_netlist_after_netlist_command",
    /// Place a net label (xschem `lab_pin`).
    PlaceLabel = "symbol.place_net_pin_label",
    /// Place a net label (xschem `lab_wire`).
    PlaceWireLabel = "symbol.place_net_wire_label",
    /// Place an input port.
    PlaceInPort = "symbol.place_schematic_input_port",
    /// Place an output port.
    PlaceOutPort = "symbol.place_schematic_output_port",
    /// Place a symbol pin (symbol view).
    PlaceSymPin = "symbol.place_symbol_pin",
    /// New tab (new schematic).
    TabNew = "tab.new",
    /// Next tab.
    TabNext = "tab.next",
    /// Previous tab.
    TabPrev = "tab.prev",
    /// Activate a tab.
    TabSelect = "tab.select",
    /// Space: cycle Manhattan routing while drawing/moving, else start panning.
    CycleManhattan = "tools.cycle_manhattan",
    /// Draw a symbol line.
    InsertLine = "tools.insert_line",
    /// Draw a symbol rectangle.
    InsertRect = "tools.insert_rect",
    /// Snapped wire from the nearest connection point.
    InsertSnapWire = "tools.insert_snap_wire",
    /// Insert-symbol chooser.
    InsertSymbol = "tools.insert_symbol",
    /// Place a text.
    InsertText = "tools.insert_text",
    /// Draw a wire.
    InsertWire = "tools.insert_wire",
    /// Show / hide the checks panel.
    ChecksToggle = "view.checks",
    /// Show / hide the active tab's docs page beside the canvas.
    DocsToggle = "view.docs",
    /// Center the view on the pointer.
    PanCenter = "view.pan_center",
    /// Pan down.
    PanDown = "view.pan_down",
    /// Pan left.
    PanLeft = "view.pan_left",
    /// Pan right.
    PanRight = "view.pan_right",
    /// Pan up.
    PanUp = "view.pan_up",
    /// Escape: abort the operation, unselect, redraw.
    Redraw = "view.redraw",
    /// Show / hide the results panel.
    ResultsToggle = "view.results",
    /// Grid from the status field.
    SetGrid = "view.set_grid",
    /// Snap from the status field.
    SetSnap = "view.set_snap",
    /// Dark / light canvas.
    ToggleTheme = "view.toggle_colorscheme",
    /// Zoom box (next drag or click pair).
    ZoomBox = "view.zoom_box",
    /// Fit everything.
    ZoomFull = "view.zoom_full",
    /// Zoom in.
    ZoomIn = "view.zoom_in",
    /// Zoom out.
    ZoomOut = "view.zoom_out",
    /// Fit the selection.
    ZoomSelection = "view.zoom_selection",
    /// The waveform-file chooser (D23 viewer).
    Waves = "waves.external_viewer",
    /// Toggle the OP back-annotation overlay (node voltages, device `gm`/`id`/…).
    OpAnnotate = "waves.op_annotate",
}

impl Cmd {
    /// Returns the command's public name.
    pub fn name(self) -> &'static str {
        CORE.iter()
            .find(|(_, c)| *c == self)
            .map(|(n, _)| *n)
            .expect("every variant has a row")
    }
}

/// Where a command name dispatches to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A core command.
    Core(Cmd),
    /// A plugin's command: delivered as `Event::Command` to its owner.
    Plugin(PluginId),
    /// Nobody implements it (a binding to a command not built yet).
    Unknown,
}

/// The `commands` table: core rows plus plugin rows.
#[derive(Clone, Debug, Default)]
pub struct Commands {
    name: Vec<Box<str>>,
    owner: Vec<PluginId>,
}

impl Commands {
    /// Rebuilds the plugin rows from the composed `commands` records (`command <name> …`).
    /// Rows beyond [`PLUGIN_COMMANDS_MAX`] are dropped.
    pub fn set_plugin_rows(&mut self, composed: &schemify_plugin::Composed) {
        let mut rows: Vec<(Box<str>, PluginId)> = composed
            .of(Kind::Commands)
            .iter()
            .filter_map(|r| {
                let name = r.key.strip_prefix("command ")?;
                Some((Box::from(name), r.owner))
            })
            .take(PLUGIN_COMMANDS_MAX)
            .collect();
        rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        rows.dedup_by(|a, b| a.0 == b.0);
        self.name = rows.iter().map(|r| r.0.clone()).collect();
        self.owner = rows.iter().map(|r| r.1).collect();
        assert_eq!(self.name.len(), self.owner.len());
    }

    /// Resolves a name: a plugin row wins over a core command of the same name (plugins
    /// compose over the core base, last wins, D8).
    pub fn resolve(&self, name: &str) -> Target {
        if let Ok(i) = self.name.binary_search_by(|n| (**n).cmp(name)) {
            return Target::Plugin(self.owner[i]);
        }
        match CORE.binary_search_by(|(n, _)| (*n).cmp(name)) {
            Ok(i) => Target::Core(CORE[i].1),
            Err(_) => Target::Unknown,
        }
    }

    /// Number of plugin rows.
    pub fn plugin_rows(&self) -> usize {
        self.name.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_table_is_sorted_and_names_round_trip() {
        assert!(
            CORE.windows(2).all(|w| w[0].0 < w[1].0),
            "CORE sorted by name"
        );
        let t = Commands::default();
        for &(name, cmd) in CORE {
            assert_eq!(t.resolve(name), Target::Core(cmd));
            assert_eq!(cmd.name(), name);
        }
        assert_eq!(t.resolve("no.such_command"), Target::Unknown);
        assert_eq!(t.resolve(""), Target::Unknown);
    }
}
