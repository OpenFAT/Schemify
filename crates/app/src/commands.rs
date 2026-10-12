//! `commands`: the one command table (D27) that menus, the keymap, plugins, and later the CLI
//! and JSON-RPC all name. Core commands are a closed enum ([`Cmd`]) so dispatch is an
//! exhaustive `match`; plugin commands are rows from the composed `commands` contributions.
//!
//! Data design: ~210 core rows (static, sorted by name, `< 256` so `Cmd` stays `u8`) + ≤
//! [`PLUGIN_COMMANDS_MAX`] plugin rows (session, rebuilt when the plugin composition
//! changes); looked up by name once per command request (binary search). Names are
//! `<menu>.<action>` words; a command that takes an argument may also be named
//! `<name>.<word>` ([`Commands::resolve_arg`]).

use schemify_format::FileKind;
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
    /// Cell panel: add a bench output, spec or optimizer (`cell.add.output|spec|opt`; from a
    /// script, the word as argument).
    CellAddBench = "cell.add_bench",
    /// Cell panel: add an impl of the language its node id names (`cell.add_impl.<lang>`;
    /// from a script, the language as argument).
    CellAddImpl = "cell.add_impl",
    /// Cell panel: add a parameter.
    CellAddParam = "cell.add_param",
    /// Cell panel: delete the parameter or impl its node id (or argument) names
    /// (`cell.param.<r>.del`, `cell.impl.<r>.del`).
    CellDelete = "cell.delete",
    /// Cell panel: move a pin one place later in port order.
    CellPinDown = "cell.pin_down",
    /// Cell panel: move a pin one place earlier in port order.
    CellPinUp = "cell.pin_up",
    /// Cell panel: a field submitted (Enter; Ctrl+Enter in a body; a dropdown choice). From a
    /// script the argument is `<field id> <value>` (ids in `cell.rs`).
    CellSet = "cell.set",
    /// Cell panel: regenerate the symbol (pins in port order, box, labels) from the ports of
    /// the impl body its node id (or argument) names (`cell.impl.<r>.symbol`). One undo.
    CellSymbolFromPorts = "cell.symbol_from_ports",
    /// Cell panel: apply the pending pin sync (one undo); refused when the document changed
    /// since `cell.sync_ports`.
    CellSyncApply = "cell.sync_apply",
    /// Cell panel: drop the pending pin sync; the document is untouched.
    CellSyncCancel = "cell.sync_cancel",
    /// Cell panel: read the ports of the impl body its node id (or argument) names
    /// (`cell.impl.<r>.sync`) and show what syncing the pins to them changes, without
    /// editing; `cell.sync_apply` applies it.
    CellSyncPorts = "cell.sync_ports",
    /// Click on a row of the checks panel: select and zoom to its object.
    ChecksPick = "checks.pick",
    /// Enter in the chooser filter.
    ChooseFilter = "choose.filter",
    /// Pick a chooser row (insert symbol / open cell / waves file).
    ChoosePick = "choose.pick",
    /// The canvas context menu at the pointer (right click): its rows run ordinary commands.
    ContextMenu = "ctx.menu",
    /// Close the open dialog.
    DialogClose = "dialog.close",
    /// OK in the edit-file dialog: apply the edited text as one undoable edit.
    EditFileOk = "dialog.edit_file.ok",
    /// OK in the enter-text dialog.
    EnterTextOk = "dialog.enter_text.ok",
    /// Crash recovery dialog: drop the journal's unsaved edits.
    DiscardRecovery = "dialog.recover.discard",
    /// Crash recovery dialog: replay the journal's unsaved edits.
    Recover = "dialog.recover.ok",
    /// Docs page: insert a text block after the selected block.
    DocsAddProse = "docs.add_prose",
    /// Docs page: insert a variable after the selected block.
    DocsAddVar = "docs.add_var",
    /// Docs page: delete the selected block.
    DocsDelete = "docs.delete_block",
    /// Docs page: click on a text block (select it and open its editor, with a live preview);
    /// from a script, the block row as argument.
    DocsEdit = "docs.edit",
    /// Docs page: move the selected block down.
    DocsDown = "docs.move_down",
    /// Docs page: move the selected block up.
    DocsUp = "docs.move_up",
    /// Docs page: apply the text block editor (Ctrl+Enter).
    DocsProseOk = "docs.prose_ok",
    /// Docs page: select a block (click on a variable's name).
    DocsSelect = "docs.select",
    /// Docs page: show every prose block as its Markdown / LaTeX source, or rendered again
    /// (a toggle per tab; var blocks are unchanged).
    DocsSource = "docs.source",
    /// Docs page: a variable's expression field submitted (Enter).
    DocsVarOk = "docs.var_ok",
    /// Commit the staged edit under review (visual diff reviewed, D18), or stage `<k>` of
    /// the active document (`edit.accept_staged.<k>`).
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
    /// Move stretching attached wires and adding a wire wherever a moved connection point leaves
    /// another object it touched (xschem "kissing").
    MoveKiss = "edit.move_objects_adding_wires_to_connected_p",
    /// Move stretching attached wires: their ends on moved connection points follow.
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
    /// Drop the staged edit under review, or stage `<k>` (`edit.reject_staged.<k>`).
    RejectStaged = "edit.reject_staged",
    /// Rotate each selected object about its own anchor.
    RotateInPlace = "edit.rotate_in_place_selected_objects",
    /// Rotate the selection as a group.
    Rotate = "edit.rotate_selected_objects",
    /// Select every object.
    SelectAll = "edit.select_all",
    /// Review another staged edit of the active document (`edit.show_staged.next|prev|<k>`):
    /// the visual diff and the docs banner follow it.
    ShowStaged = "edit.show_staged",
    /// Remove the axis constraint.
    Unconstrained = "edit.unconstrained_move",
    /// Undo.
    Undo = "edit.undo",
    /// The next drag removes the objects in its box from the selection (xschem `D`).
    UnselectArea = "edit.unselect_area",
    /// Unselect the object under the pointer.
    UnselectUnderMouse = "edit.unselect_under_mouse",
    /// Vertical flip of each selected object about its anchor.
    VFlipInPlace = "edit.vertical_flip_in_place_selected_objects",
    /// Vertical flip of the selection as a group.
    VFlip = "edit.vertical_flip_selected_objects",
    /// A text field's `input` command: its text as typed (the node is the field), so dialog
    /// and docs-page state follow the typing without polling the field.
    FieldTyped = "field.typed",
    /// New empty schematic in a new tab.
    NewSchematic = "file.clear_schematic",
    /// New empty symbol in a new tab.
    NewSymbol = "file.clear_symbol",
    /// Close the active tab.
    Close = "file.close_schematic",
    /// The insert-symbol chooser.
    ComponentBrowser = "file.component_browser",
    /// New empty schematic in a new tab (xschem "Create new window/tab").
    NewWindowTab = "file.create_new_window_tab",
    /// Delete a cell file of the project from disk: chooser, then the same pick again to confirm.
    DeleteFiles = "file.delete_files",
    /// Pack the project into a zip (`.schemify/export/<name>.zip`; a download on the web).
    ExportProject = "file.export_project",
    /// Export the selection (else the whole drawing) as Encapsulated PostScript.
    ExportEps = "file.image_export.eps_selection_export",
    /// Export the active cell and every schematic below it as a multi-page PDF.
    ExportPdfHier = "file.image_export.hierarchical_pdf_ps_export",
    /// Export the selection, else the visible area, as PDF.
    ExportPdf = "file.image_export.pdf_ps_export",
    /// Export the whole drawing as PDF.
    ExportPdfFull = "file.image_export.pdf_ps_export_full",
    /// Export the visible canvas as PNG (the CPU raster of `render::Painter`) at the PNG
    /// scale (`options.png_export_scale`).
    ExportPng = "file.image_export.png_export",
    /// Export the whole drawing as PNG: one pixel per schematic unit times the PNG scale.
    ExportPngFull = "file.image_export.png_export_full",
    /// Export the whole drawing as SVG.
    ExportSvg = "file.image_export.svg_export",
    /// Web: import a project from a zip (`file.import_project.folder`: from a folder) and
    /// reload on it (web spec §3).
    ImportProject = "file.import_project",
    /// Insert another cell's schematic attached to the pointer (xschem merge).
    Merge = "file.merge",
    /// The open-cell chooser.
    Open = "file.open",
    /// Reopen the most recently opened file.
    OpenMostRecent = "file.open_most_recent",
    /// Reopen entry `<i>` of the recent-files list (`file.open_recent.<i>`).
    OpenRecent = "file.open_recent",
    /// Quit.
    Quit = "file.quit",
    /// Reload the active document from disk.
    Reload = "file.reload",
    /// Save the active document.
    Save = "file.save",
    /// Save the active document under a new project-relative path and edit that file.
    SaveAs = "file.save_as",
    /// Save As, then show the new file's symbol (a cell file carries its symbol, D11).
    SaveAsSymbol = "file.save_as_symbol",
    /// About Schemify, in a text tab.
    About = "help.about",
    /// The help text, in a text tab.
    Help = "help.help",
    /// Toggle highlighting the selection's nets on every selection change.
    AutoHighlight = "highlight.auto_highlight_net_pins",
    /// Show / hide the comparison with the chosen cell.
    CompareToggle = "highlight.compare_schematics",
    /// Toggle outlining instances with a pin on a highlighted net.
    HighlightInstances = "highlight.enable_highlight_connected_instances",
    /// Select every instance whose name is not unique.
    DupNames = "highlight.highlight_duplicate_instance_names",
    /// Highlight the nets of selected instances' pins whose name differs from the pin's.
    PinMismatch = "highlight.highlight_net_pin_mismatches_on_sel_inst",
    /// Highlight the nets of the selection or under the pointer, across the hierarchy (`k`).
    Highlight = "highlight.highlight_selected_net_pins",
    /// Toggle giving each new highlight the next color.
    IncrementColor = "highlight.increment_hilight_color",
    /// Rename instances with duplicate names (one undo).
    RenameDups = "highlight.rename_duplicate_instance_names",
    /// Select the wires, labels and ports of the highlighted nets.
    SelectHighlighted = "highlight.select_hilight_nets_pins",
    /// Select instances placed exactly on another instance of the same cell.
    SelectOverlapped = "highlight.select_overlapped_instances",
    /// Send the highlighted nets to the waves dock as `v(<net>)` traces (`Alt+G`), opening it
    /// on the selected job when closed.
    SendToViewer = "highlight.send_selected_net_pins_to_viewer",
    /// Choose a cell to compare the active document with (`format::diff`).
    CompareWith = "highlight.set_schematic_to_compare_and_compare_wit",
    /// Open the compared cell and compare it with this one.
    CompareSwap = "highlight.swap_compare_schematics",
    /// Clear every highlight (`Shift+K`).
    Unhighlight = "highlight.un_highlight_all_net_pins",
    /// Clear the highlight of the selected nets (`Ctrl+K`).
    UnhighlightSelected = "highlight.un_highlight_selected_net_pins",
    /// Toggle dimming everything but the highlight.
    OnlyProbes = "highlight.view_only_probes",
    /// Show every layer.
    AllLayers = "layers.draw_all_layers",
    /// Show only the current drawing layer.
    CurrentLayerOnly = "layers.draw_current_layer_only",
    /// Current drawing layer for lines, rects, polygons, arcs (`layers.set_current.<n>`).
    SetLayer = "layers.set_current",
    /// Show / hide layer `<n>` (`layers.toggle.<n>`).
    ToggleLayer = "layers.toggle",
    /// Toggle joining/trimming wires after every wire edit.
    AutoTrim = "options.auto_join_trim_wires",
    /// Toggle color (else black on white) PDF/EPS/SVG export.
    ExportColor = "options.color_postscript_svg",
    /// Crosshair half-size in pixels (0 = full canvas): dialog, or the size as argument.
    CrosshairSize = "options.crosshair.crosshair_size",
    /// Toggle the crosshair at the snapped pointer.
    Crosshair = "options.crosshair.draw_crosshair",
    /// Toggle the diamond on the connection point nearest the pointer.
    SnapCursor = "options.crosshair.draw_snap_cursor",
    /// Next netlist dialect (ngspice, Xyce, CDL, Spectre, Verilog).
    CycleDialect = "options.cycle_netlist_format",
    /// Double the snap.
    DoubleSnap = "options.double_snap_threshold",
    /// Toggle the grid.
    DrawGrid = "options.draw_grid",
    /// Toggle automatic L routing.
    Orthogonal = "options.enable_orthogonal_wiring",
    /// Toggle stretching attached wires on plain moves (xschem `y`).
    EnableStretch = "options.enable_stretch",
    /// Paper PDF and EPS exports fit (`a4`, `letter portrait`, …): dialog, or the size as
    /// argument.
    ExportPage = "options.export_page_size",
    /// Halve the snap.
    HalfSnap = "options.half_snap_threshold",
    /// Toggle flat (hierarchy expanded) SPICE netlists.
    FlatNetlist = "options.netlist_format_symbol_mode.flat_netlist",
    /// Toggle staying in wire/line mode after each segment.
    PersistentWire = "options.persistent_wire_line_place_command",
    /// PNG export scale (1 = screen resolution, 96 dpi; 2 = 192 dpi): dialog, or the factor
    /// as argument.
    PngScale = "options.png_export_scale",
    /// Toggle the SVG background rectangle.
    ExportTransparent = "options.transparent_svg_background",
    /// Re-read `schemify.project` and `schemify.lock`, re-index every library and refresh
    /// the open documents' symbols and elaboration, without a restart (pdk spec §9.2). Open
    /// documents and their undo history stay; refused while an open document's library is
    /// no longer declared. Also run after a plugin writes the manifest or lockfile.
    ProjectReload = "project.reload",
    /// Properties dialog of the selection.
    Props = "properties.edit",
    /// Edit the active document as text; OK applies it as one undoable edit.
    EditFile = "properties.edit_file_danger",
    /// Edit the header attributes (`doc`, `prefix`, `category`).
    EditHeader = "properties.edit_header_license_text",
    /// Properties dialog (xschem: external editor; same dialog here).
    PropsEditor = "properties.edit_with_editor",
    /// Toggle `ignore true` on the selected instances.
    ToggleIgnore = "properties.toggle_ignore_attribute_on_selected_inst",
    /// Properties dialog (xschem: read-only view; same dialog here).
    PropsView = "properties.view",
    /// Field submitted in the properties dialog.
    PropsField = "props.field",
    /// OK in the properties dialog.
    PropsOk = "props.ok",
    /// Results panel: filter the job rows (the filter field's text: every word must appear).
    ResultsFilter = "results.filter",
    /// Results panel: load an earlier run of the bench (`results.hist.<i>`, results history).
    ResultsHistory = "results.history",
    /// Results panel: pick a job (waves, plots and OP annotation follow it).
    ResultsPick = "results.pick",
    /// Results panel: sort the job rows by a header column (`results.head.<col>`; again:
    /// reversed).
    ResultsSort = "results.sort",
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
    /// List the files of the hierarchy below this cell with their modification times.
    Changelog = "simulation.changelog_from_current_hierarchy",
    /// Netlist and show the netlist in a text tab.
    EditNetlist = "simulation.edit_netlist",
    /// Add a plot block to the docs page.
    AddGraph = "simulation.graphs.add_waveform_graph",
    /// Toggle the OP back-annotation (same as `waves.op_annotate`).
    OpAnnotateGraphs = "simulation.graphs.annotate_operating_point_into_schematic",
    /// Toggle highlighting the nets plotted in the waves dock.
    AutoHighlightPlotted = "simulation.graphs.auto_highlight_plotted_nets",
    /// List the run in flight and its jobs in a text tab.
    ListProcesses = "simulation.list_running_sub_processes",
    /// Send the highlighted nets to the waves dock (xschem `Ctrl+Shift+X`; same as `Alt+G`).
    SendHighlighted = "simulation.send_highlighted_nets_to_viewer",
    /// Toggle opening the netlist in a text tab after netlisting (xschem `Shift+A`).
    ShowNetlist = "simulation.show_netlist_after_netlist_command",
    /// Show the selected job's simulator output in a text tab.
    JobData = "simulation.view_last_job_data",
    /// Show the selected job's error lines in a text tab.
    JobErrors = "simulation.view_last_job_errors",
    /// Put a label named after each pin on every unconnected pin of the selected instances.
    AttachLabels = "symbol.attach_net_labels_to_component_instance",
    /// Add symbol pins for the selected schematic ports.
    PinsFromPorts = "symbol.create_symbol_pins_from_selected_schemat",
    /// Attach one label per highlighted net to the pointer.
    LabelsFromNets = "symbol.list_of_nets.create_labels_from_highlight_nets",
    /// As `LabelsFromNets` with an `i` prefix.
    LabelsFromNetsI = "symbol.list_of_nets.create_labels_from_highlight_nets_with_i",
    /// Attach one port per highlighted net to the pointer.
    PortsFromNets = "symbol.list_of_nets.create_pins_from_highlight_nets",
    /// List the highlighted nets in a text tab.
    ListNets = "symbol.list_of_nets.print_list_of_highlight_nets",
    /// List the highlighted nets, buses expanded per bit.
    ListNetsBuses = "symbol.list_of_nets.print_list_of_highlight_nets_with_buses",
    /// Move the selection into a new cell and put an instance of it in its place.
    MakeSubcircuit = "symbol.make_schematic_and_symbol_from_selected",
    /// Add a schematic port for every symbol pin that has none.
    MakeSchematic = "symbol.make_schematic_from_symbol",
    /// Rebuild the cell's symbol (pins, box, name) from its schematic ports.
    MakeSymbol = "symbol.make_symbol_from_schematic",
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
    /// Symbol box width that make-symbol uses: dialog, or the width as argument.
    SymbolWidth = "symbol.set_symbol_width",
    /// Cycle the three symbol display modes (xschem Alt+B).
    SymbolsCycle = "symbol.show_symbols.cycle",
    /// Draw every instance as a box.
    SymbolsBoxesAll = "symbol.show_symbols.show_instance_bounding_boxes_for_all_sym",
    /// Draw schematic-cell instances as boxes.
    SymbolsBoxesSub = "symbol.show_symbols.show_instance_bounding_boxes_for_subcirc",
    /// Draw instances with their symbol graphics.
    SymbolsDetails = "symbol.show_symbols.show_symbol_details",
    /// New tab (new schematic).
    TabNew = "tab.new",
    /// Next tab.
    TabNext = "tab.next",
    /// Previous tab.
    TabPrev = "tab.prev",
    /// Activate a tab.
    TabSelect = "tab.select",
    /// Find in the text tab (the field's text): the file's lines containing it, numbered.
    TextFind = "text.find",
    /// Snap every coordinate of the selection (else everything) to the snap.
    AlignGrid = "tools.align_to_grid",
    /// Cut the wire under the pointer at the exact pointer position.
    BreakAtMouse = "tools.break_wires_at_mouse_position",
    /// Cut the wire under the pointer at the snapped pointer.
    BreakAtMouseAlign = "tools.break_wires_at_mouse_position_align_cut",
    /// Split wires at the pins of the selected instances.
    BreakAtPins = "tools.break_wires_at_selected_instance_pins",
    /// Close and finish the polygon being drawn (Enter).
    ClosePolygon = "tools.close_polygon",
    /// Space: cycle Manhattan routing while drawing/moving, else start panning.
    CycleManhattan = "tools.cycle_manhattan",
    /// Draw an arc: center, radius point, end angle.
    InsertArc = "tools.insert_arc",
    /// Draw a circle: center, radius point.
    InsertCircle = "tools.insert_circle",
    /// Draw a symbol line.
    InsertLine = "tools.insert_line",
    /// Draw a polygon: click vertices, Enter or the first vertex closes it.
    InsertPolygon = "tools.insert_polygon",
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
    /// Merge collinear touching wires, drop duplicates, split at T junctions.
    JoinTrim = "tools.join_trim_wires",
    /// The user packages' state (`schemify pack status`) in a text tab.
    PackageStatus = "tools.package_status",
    /// Split wires at the selected instances' pins and delete the pieces inside them.
    RemoveThrough = "tools.remove_wires_running_through_selected_in",
    /// Search / replace dialog: select objects by attribute, or replace a value everywhere.
    Search = "tools.search",
    /// Select the wires, labels and ports connected to the selection.
    SelectConnected = "tools.select_all_connected_wires_labels_pins",
    /// As `SelectConnected`, stopping at junctions of three or more wires.
    SelectConnectedStop = "tools.select_conn_wires_stop_at_junctions",
    /// Show / hide the active tab's cell settings panel.
    CellToggle = "view.cell",
    /// Color of the current drawing layer: dialog, or `rrggbb` as argument.
    LayerColor = "view.change_current_layer_color",
    /// Show / hide the checks panel.
    ChecksToggle = "view.checks",
    /// Dim every layer toward the background: dialog, or the factor 0..1 as argument.
    DimColors = "view.dim_colors",
    /// Show / hide the active tab's docs page beside the canvas.
    DocsToggle = "view.docs",
    /// Toggle the canvas-only view (menus, toolbar, tabs and status hidden).
    Fullscreen = "view.fullscreen",
    /// Thinner lines (-0.5 px).
    LineWidthDown = "view.line_width_decrease",
    /// Thicker lines (+0.5 px).
    LineWidthUp = "view.line_width_increase",
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
    /// Drop layer color changes, dimming and hidden layers.
    ResetColors = "view.reset_all_colors_to_default",
    /// Show / hide the results panel.
    ResultsToggle = "view.results",
    /// Show the active cell's schematic (refused for a `.shprim`, which has none).
    ViewSchematic = "view.schematic",
    /// Grid from the status field.
    SetGrid = "view.set_grid",
    /// Grid spacing dialog.
    GridDialog = "view.set_grid_spacing",
    /// Line width in pixels: dialog, or the width as argument.
    LineWidth = "view.set_line_width",
    /// Snap from the status field.
    SetSnap = "view.set_snap",
    /// Snap value dialog.
    SnapDialog = "view.set_snap_value",
    /// Toggle the x = 0 / y = 0 axes.
    GridAxes = "view.show_hide.draw_grid_axes",
    /// Show / hide the checks panel (xschem's ERC info window).
    ErcWindow = "view.show_hide.show_erc_info_window",
    /// Show / hide the toolbar.
    ToolbarToggle = "view.show_hide.show_toolbar",
    /// Show / hide the texts inside symbol instances.
    SymbolText = "view.show_hide.symbol_text",
    /// Per-layer visibility dialog.
    LayersDialog = "view.show_hide.visible_layers",
    /// Show the active cell's symbol (refused for a `.shbench`, which has none).
    ViewSymbol = "view.symbol",
    /// Dark / light canvas.
    ToggleTheme = "view.toggle_colorscheme",
    /// Toggle filling of filled layers.
    FillToggle = "view.toggle_fill_pattern",
    /// Toggle line widths that follow the zoom.
    VariableWidth = "view.toggle_variable_line_width",
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
    /// Close the waves dock (unload its files).
    WavesClear = "waves.clear",
    /// The waveform-file chooser (D23 viewer).
    Waves = "waves.external_viewer",
    /// Waveform file chooser for analysis `<a>` (`waves.load.tran`, ...).
    WavesLoad = "waves.load",
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

    /// The file kinds this command acts on, `None` when it acts on any. The one table both
    /// the dispatcher (a refusal) and the UI (every node bound to the command is disabled,
    /// so it greys out) read; a handler may still refine the check.
    pub fn file_kinds(self) -> Option<&'static [FileKind]> {
        use FileKind::{Shbench, Shcomp, Shprim};
        match self {
            Cmd::Simulate | Cmd::Optimize | Cmd::CellAddBench => Some(&[Shbench]),
            Cmd::NewBench => Some(&[Shcomp, Shprim]),
            Cmd::MakeSymbol | Cmd::MakeSchematic | Cmd::PinsFromPorts => Some(&[Shcomp]),
            Cmd::MakeSubcircuit => Some(&[Shcomp, Shbench]),
            _ => None,
        }
    }

    /// Whether this command acts on a file of `kind` ([`Cmd::file_kinds`]).
    pub fn accepts(self, kind: FileKind) -> bool {
        self.file_kinds().is_none_or(|ks| ks.contains(&kind))
    }

    /// Whether `<name>.<word>` names this command with `<word>` as its argument (D27:
    /// arguments are data, so a key binding or menu row can carry one without a field).
    pub fn takes_suffix(self) -> bool {
        matches!(
            self,
            Cmd::SetDialect
                | Cmd::SetLayer
                | Cmd::ToggleLayer
                | Cmd::OpenRecent
                | Cmd::WavesLoad
                | Cmd::AcceptStaged
                | Cmd::RejectStaged
                | Cmd::ShowStaged
                | Cmd::ImportProject
        )
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

    /// Resolves a name that may carry an argument suffix: an exact row first, else
    /// `<core name>.<word>` for a command that [`Cmd::takes_suffix`], returning `<word>` as
    /// the argument (`layers.set_current.3` → (`SetLayer`, `"3"`)). The argument is empty
    /// for an exact match.
    pub fn resolve_arg<'n>(&self, name: &'n str) -> (Target, &'n str) {
        let t = self.resolve(name);
        if t != Target::Unknown {
            return (t, "");
        }
        if let Some((base, word)) = name.rsplit_once('.')
            && !word.is_empty()
            && let Target::Core(c) = self.resolve(base)
            && c.takes_suffix()
        {
            return (Target::Core(c), word);
        }
        (Target::Unknown, "")
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

    #[test]
    fn suffix_arguments_only_for_commands_that_take_one() {
        let t = Commands::default();
        assert_eq!(
            t.resolve_arg("layers.set_current.3"),
            (Target::Core(Cmd::SetLayer), "3")
        );
        assert_eq!(
            t.resolve_arg("sim.dialect.xyce"),
            (Target::Core(Cmd::SetDialect), "xyce")
        );
        assert_eq!(t.resolve_arg("edit.undo"), (Target::Core(Cmd::Undo), ""));
        // A command without an argument does not swallow a suffix.
        assert_eq!(t.resolve_arg("edit.undo.x").0, Target::Unknown);
        assert_eq!(t.resolve_arg("layers.set_current.").0, Target::Unknown);
        assert!(CORE.len() < 256, "Cmd is repr(u8)");
    }
}
