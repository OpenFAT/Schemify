<!-- xschem 3.4.7 canvas keys, from the feature inventory captured by running xschem (clean room). Oracle of keymap_covers_the_xschem_inventory. -->

### 2.1 Keyboard bindings (schematic and symbol canvas, `handle_key_press` cb:2479-3999)

Column notes:
- **Ctx**:
  - `any` = schematic or symbol editing.
  - `G>` = when the pointer is inside a graph, the key is first routed to the graph handler (see 2.2).
  - `cad` = the binding depends on `cadence_compat`.
- **help?**: `y` if the binding is listed in `src/keys.help`, `NO` if it exists only in callback.c.
- Upper-case letters imply Shift.

| Key | Mod | Ctx | Action | cb line | help? |
|---|---|---|---|---|---|
| 0 | – | any | Set selected net/label logic value 0 (`logic_set`) | 2487 | y |
| 1 | – | any | Set logic 1 | 2487 | y |
| 2 | – | any | Set logic X | 2487 | y |
| 3 | – | any | Set logic Z | 2487 | y |
| 4 | – | any | Toggle logic value 1->0, 0->1, X->X | 2487 | y |
| 0-9 | Ctrl | any | Set current drawing layer (`rectcolor`) to N and update the layer toolbar button | 2497, 2525 | y |
| 5 | – | any | Toggle "view only probes" (`only_probes`) | 2514 | y |
| a | – | any, G> | Make symbol from the current schematic's pins (confirm dialog; saves first) | 2542 | y |
| a | Ctrl | any, G> | Select all | 2557 | y |
| A | – | any, G> | Toggle `netlist_show` (show netlist in a window after netlisting) | 2566 | y |
| A | Ctrl | G only | No schematic action. In a graph, toggles horizontal cursor 1 when `graph_use_ctrl_key` is set | 2584 | NO |
| b | – | any, G> | Merge (import) a file into the current schematic | 2592 | y |
| b | Ctrl | any, G> | Toggle display of text inside symbols (`sym_txt`) | 2601 | y |
| b | Alt | any | Cycle `hide_symbols` 0 -> 1 -> 2 (full / bbox for subckt symbols / bbox for all) | 2618 | y |
| B | – | any, G> | Edit file header/license metadata (`update_schematic_header`, tcl:1404) | 2627 | y |
| B | Ctrl | G only | Graph: toggle hcursor2 (with `graph_use_ctrl_key`) | 2635 | NO |
| c | – | any | Duplicate (copy) selection, attached to the mouse | 2643 | y |
| c | Ctrl | any | Copy selection to the clipboard file (`save_selection(2)`) | 2657 | y |
| c | Alt | any | Duplicate with `connect_by_kissing` (insert wires where moved pins detach from touching pins/wires) | 2665 | y |
| C | – | any | Start arc (3-point) | 2679 | y |
| C | Ctrl | any | Start circle (3-point, sweep 360) | 2692 | y |
| d | – | any | Unselect the object under the mouse | 2706 | y |
| d | Ctrl | any | Delete files dialog (`delete_files`) | 2715 | y |
| D | – | any | Unselect by area (next drag) | 2721 | y |
| e | – | any | Descend into the selected instance's schematic | 2738 | y |
| e | Ctrl | any | Go back to the parent (pop hierarchy) | 2743 | y |
| e | Alt | any | Open the selected instance's schematic in a new window/tab (`open_sub_schematic`, tcl:3959) | 2747 | y |
| E | Alt | any | Open the selected schematic in a new xschem process | 2756 | y |
| f | – | any, G> | Zoom full (`zoom_full_center` option centers the drawing) | 2765 | y |
| f | Ctrl | any, G> | Find/select by substring or regex (`property_search`, tcl:5896) | 2775 | y |
| f | Alt | any | Horizontal flip of each selected object around its own anchor (works during move/copy) | 2783 | y |
| F | – | any | Horizontal flip of the selection as a group (works during move/copy) | 2797 | y |
| F | Ctrl | any | Zoom to fit the selection | 2810 | y |
| g | – | any | Halve snap (`cadsnap / 2`) | 2817 | y |
| g | Ctrl | any | Snap value input dialog | 2823 | y |
| g | Alt | any | Highlight selected nets and send them to the waveform viewer. Targets the xschem graph if the graph dialog is open or a graph is selected, otherwise Gaw or Bespice per `sim(spicewave,default)` | 2829 | y (says "gaw") |
| G | – | any | Double snap | 2864 | y |
| h | – | any | Toggle horizontal constraint (`constr_mv=1`) for move/copy/wire/line | 2879 | y |
| h | Ctrl | any | Launcher: follow `url`, run `program`, or eval `tclcommand` of the selected instance (actions.c:1805) | 2873 | y |
| h | Alt | any | Create symbol pins from schematic pins (`schpins_to_sympins`, tcl:5279) | 2898 | y |
| H | – | any | Attach net labels to the selected instance pins (`attach_labels_to_inst`) | 2903 | y |
| H | Ctrl | any | Make a schematic and a symbol from the selected components | 2907 | y |
| i | – | any | Descend into the selected instance's symbol | 2912 | y |
| i | Ctrl | any | Insert-symbol dialog (persistent file dialog, or the new browser if `new_symbol_browser`) | 2917 | y |
| i | Alt | any | Edit the selected symbol in a new window | 2924 | y |
| I | – | any | Insert symbol (file dialog, then place) | 2932 | y |
| I | Alt | any | Edit the selected symbol in a new xschem process | 2941 | NO |
| j | – | any | Print the list of highlighted nets/pins | 2949 | y |
| j | Ctrl | any | Create ipins/ports from highlighted nets (`print_hilight_net(0)`) | 2954 | y |
| j | Alt | any | Create labels without the `i` prefix from highlighted nets | 2958 | y |
| j | Ctrl+Alt | any | Print highlighted nets with bus/label expansion | 2962 | y |
| J | Alt | any | Create labels with the `i` prefix from highlighted nets | 2968 | y |
| k | – | any | Highlight selected nets | 2974 | y |
| k | Ctrl | any | Unhighlight selected nets | 2985 | y |
| k | Alt | any | Select the whole net attached to the selected wire/label/pin | 2982 | y |
| K | – | any | Clear all highlights | 2991 | y |
| K | Ctrl | any | Highlight with "drill": propagates through instances whose pins have `propag=` | 2999 | y |
| l | – | any | Start line (current layer) | 3008 | y |
| l | Ctrl | any | Create a schematic view from the selected symbol (`create_sch_from_sym`) | 3024 | y |
| l | Alt | any | Place `lab_pin.sym` at the mouse (actions.c:1451) | 3028 | y |
| L | – | any | Toggle `orthogonal_wiring` (auto H-V/V-H routing) | 3033 | y |
| L | Alt | any | Place `lab_wire.sym` | 3043 | y |
| m | – | any, G> | Move selection. With `enable_stretch` set, also stretches attached wires | 3048 | y |
| m | Ctrl | any, G> | Move selection stretching attached wires (`select_attached_nets`). Plain move if `enable_stretch` is set | 3066 | y |
| m | Alt | any | Move with `connect_by_kissing` | 3082 | y |
| M | – | any | Move with `connect_by_kissing` (same as Alt+m) | 3096 | y |
| M | Ctrl | any | Move with stretch plus `connect_by_kissing` | 3111 | y |
| n | – | any | Hierarchical netlist with ERC (`xschem netlist -erc`) | 3125 | y |
| n | Ctrl | any | Clear schematic (new empty) | 3130 | y |
| N | – | any | Netlist the current level only (no hierarchy). Turns the Netlist menu button green or red | 3136 | y |
| N | Ctrl | any | Clear symbol (new empty symbol) | 3175 | y |
| o | Ctrl | any | Open/load file (file dialog or the new file browser) | 3181 | y |
| O | – | any | Toggle light/dark color scheme | 3201 | y |
| O | Ctrl | any | Load the most recent file | 3198 | y |
| p | – | any | Start polygon | 3227 | y |
| p | Ctrl | any | Place `ipin.sym` | 3224 | y |
| p | Alt | any | Add a symbol pin box (PINLAYER, `name=XXX dir=inout`) attached to the mouse | 3214 | y |
| P | – | any | Re-center the view on the mouse position | 3243 | y (says "Pan") |
| P | Ctrl | any | Place `opin.sym` | 3249 | y |
| q | – | any | Edit properties dialog (`edit_property(0)`) | 3262 | y |
| q | Ctrl | any | Quit xschem (`quit_xschem`) | 3255 | y |
| q | Alt | any | Open the .sch file, or the selected symbol's file, in a text editor | 3266 | y |
| Q | – | any | Edit properties in an external editor (`edit_property(1)`) | 3282 | y |
| Q | Ctrl | any | View properties, read-only (`edit_property(2)`) | 3287 | y |
| r | – | any | Start rectangle | 3292 | y |
| r | Ctrl | cad=1, G> | Run simulation (confirm) | 3306 | NO |
| r | Alt | any | Rotate each selected object around its own anchor (works during move/copy) | 3318 | y |
| R | – | any | Rotate the selection 90 degrees as a group (works during move/copy) | 3332 | y |
| s | – | cad=0, G> | Run simulation (confirm, invokes the `Simulate` menu) | 3349 | y |
| s | – | cad=1 | Start/end a wire snapped to the nearest pin or net endpoint | 3362 | NO |
| s | Ctrl | any, G> | Save. Untitled files go to Save As | 3366 | y |
| s | Alt | any | Reload from disk (confirm) | 3380 | y |
| s | Ctrl+Alt | any | Save as symbol | 3394 | y |
| S | – | any | Change element order (renumber the selected object's index) | 3400 | y |
| S | Ctrl | any | Save as (schematic) | 3405 | y |
| t | – | any, G> | Place text (dialog, then text follows the mouse) | 3411 | y |
| t | Ctrl | G only | No schematic action. Graph: dataset tracking | 3428 | NO |
| T | – | any | Toggle `spice_ignore` / `verilog_ignore` / `vhdl_ignore` / `tedax_ignore` (per current netlist type) on selected instances | 3436 | y |
| u | – | any | Undo | 3442 | y |
| u | Ctrl | any | Unselect attached "floater" objects (shapes/texts with a `name=` attribute) | 3461 | y |
| u | Alt | any | Align selection/schematic to the snap grid (`round_schematic_to_grid`, optional autotrim) | 3448 | y |
| U | – | any | Redo | 3467 | y |
| v | – | any | Toggle vertical constraint (`constr_mv=2`) | 3475 | y |
| v | Ctrl | any | Paste from the clipboard (`merge_file(2)`); pasted objects follow the mouse | 3495 | y |
| v | Alt | any | Vertical flip around each object's anchor | 3499 | y |
| V | – | any | Vertical flip of the selection (rotate 180 + flip) | 3523 | y |
| V | Ctrl | any | Cycle netlist type spice -> vhdl -> verilog -> tedax -> symbol | 3546 | y |
| w | – | any | Start wire | 3554 | y |
| w | Ctrl | any | Close the current schematic/tab (`xschem exit`) | 3572 | y |
| W | – | cad=0 | Start/end a wire snapped to the closest pin or net endpoint | 3581 | y |
| x | – | any | Start a new xschem process (new CAD session) | 3588 | y |
| x | Ctrl | any | Cut selection to the clipboard | 3600 | y |
| x | Alt | any | Toggle crosshair (`draw_crosshair`) | 3592 | y |
| X | – | any | Highlight mismatches between instance pin names and attached net names | 3610 | y |
| X | Ctrl | any | Create an ngspice `xplot` command file for highlighted nets | 3614 | y |
| y | – | any | Toggle `enable_stretch` | 3619 | y |
| z | – | any | Zoom box (no-op while drawing a rect/line/wire/poly/arc) | 3626 | y |
| z | Ctrl | any | Zoom out (one step, factor 1.2) | 3632 | NO (help says "Zoom out", but Ctrl+z is not undo) |
| z | Alt | cad=1 | Toggle `snap_cursor` | 3635 | NO |
| Z | – | any | Zoom in | 3649 | y |
| Space | – | any | During wire/line/move: cycle `manhattan_lines` 0 -> 1 -> 2. Otherwise: start pan (view follows the mouse until the next click) | 3655 | y |
| _ | – | any | Toggle `change_lw` (line width follows zoom) | 3680 | y |
| % | – | any | Toggle grid drawing | 3691 | y |
| $ | – | any | Toggle `draw_pixmap` (backing pixmap) | 3706 | y |
| $ | Ctrl | any | Toggle `draw_window` (direct window drawing vs XCopyArea) | 3712 | y |
| = | Ctrl | any | Toggle fill pattern for filled layers (effectively 0 <-> 1) | 3724 | y |
| + | Ctrl | any | Line width +0.5 | 3752 | y |
| - | Ctrl | any | Line width -0.5 (min 0) | 3760 | y |
| - | Alt | any | Line width input dialog | 3767 | y |
| Return | – | drawing polygon | Close the polygon (last point = first) and finish | 3772 | NO |
| Escape | any | any | Abort the current operation, unselect, redraw. Also sets `tclstop=1` (stops a running simulation) and clears persistent-wire state | 3778 | y |
| Delete | – | any | Delete selection | 3793 | y |
| Right | – | any, G> | Pan the view right (content moves left by 200 x zoom) | 3800 | y |
| Right | Ctrl | any, G> | Next tab | 3801 | y |
| Left | – | any, G> | Pan left | 3823 | y |
| Left | Ctrl | any, G> | Previous tab | 3824 | y |
| Down | – | any, G> | Pan down | 3846 | y |
| Up | – | any, G> | Pan up | 3856 | y |
| BackSpace | – | any | Go back to the parent schematic | 3866 | y |
| Print | – | any (unix+cairo) | Screen grab: B1-drag a screen rectangle, which is inserted as an embedded image (`flags=image,unscaled`, base64 `image_data`) on GRIDLAYER (draw.c:178-290) | 3872 | y |
| Insert | – | any | Insert symbol (file dialog then place, or the new browser) | 3879 | y |
| Insert | Shift | any | Persistent insert-symbol dialog | 3880 | y |
| * | (Shift) | any | PostScript/PDF export (`ps_draw(7)`) | 3897 | y |
| * | Ctrl | any | PNG/XPM image export | 3902 | y |
| * | Alt | any | SVG export | 3906 | y |
| & | – | any | Join/break/trim wires (`trim_wires`, check.c:151) | 3912 | y |
| \\ | – | any | Toggle fullscreen | 3919 | y |
| > | (Shift) | any | Draw only the current layer | 3926 | y |
| < | (Shift) | any | Draw all layers | 3933 | y |
| ? | – | any | Open help text (`xschem.help`) | 3939 | y |
| / | – | any | Show the key-binding help window (`show_bindkeys`, tcl:8535) | 3943 | y |
| : | – | any | Toggle flat netlist | 3948 | y |
| # | – | any | Highlight instances with duplicate names (refdes) | 3959 | y |
| # | Ctrl | any | Rename duplicate instance names | 3960 | y |
| ! | – | any | Break wires at the selected instance pins | 3983 | y |
| ! | Ctrl | any | Remove wires running through the selected instance pins (`break_wires_at_pins(1)`, check.c:534) | 3984 | NO |
| ; ~ \| | Ctrl | – | Placeholders, disabled (`if(0 && …)`) | 3968-3982 | – |
| Ctrl+Enter | – | Tk dialogs | Confirm and close dialog boxes (Tk-level, not callback.c) | – | y |

**Mismatches between keys.help and callback.c**
- **In callback.c, missing from keys.help:** Alt+Shift+I, Ctrl+Shift+A, Ctrl+Shift+B, Ctrl+t, Ctrl+r (cadence), s (cadence), Alt+z (cadence), Return (polygon close), Ctrl+!, and all graph keys (2.2).
- **In keys.help, not handled by callback.c:** only Ctrl+Enter (dialog-level).
- **Wording differences:**
  - keys.help "Shift P = Pan view area": the code re-centers on the mouse.
  - keys.help "Left = Move right / Right = Move left": this describes content movement.
  - The menu shows Alt-Z for "Draw snap cursor", but it only works when `cadence_compat=1` (cb:3634).

### 2.2
