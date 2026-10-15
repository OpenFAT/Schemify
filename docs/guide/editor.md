# The editor

The editor is `schemify-gui`. You normally start it through the command line:

```sh
schemify edit path/to/cell.shcomp     # open one cell
schemify edit path/to/project         # open a project with a new, empty schematic
schemify edit                         # the current directory
```

The project is the nearest directory above the file that holds a `schemify.project`
([files.md](files.md)). The default keys are xschem's, so if you know xschem you can start
drawing right away. Press `/` for a short key summary on screen; the full table is
[below](#keys).

![The editor with a hierarchical buffer open](../screenshots/editor.png)

## Window layout

From top to bottom:

| Area | What it holds |
|---|---|
| Menu bar | File, Edit, Options, View, Properties, Layers, Tools, Symbol, Highlight, Simulation, Help, then Netlist and Simulate buttons and the Waves menu |
| Toolbar | Open, Save, Reload, Undo, Redo, Cut, Copy, Paste, Del, Push, Pop, Symbol, Wire, Line, Rect, Poly, Arc, Circle, Text, zoom Full/In/Out, Netlist, Simulate, Waves (View → Show / Hide → Show Toolbar hides it) |
| Tabs | One tab per open cell; `+` opens a new schematic |
| Work area | The canvas, with the docs page or the cell settings panel beside it |
| Bottom panel | The checks panel or the results table, when shown |
| Waves dock | The waveform viewer, under the schematic, when open |
| Status bar | `SNAP` and `GRID` fields (type a value and press Enter), the netlist dialect, the stretch setting and the last message |

Four buttons float over the top of the canvas: **Schematic**, **Symbol**, **Docs** and
**Settings**. They switch the tab between the cell's schematic and its symbol, and toggle the
docs page (`Alt+D`) and the cell settings panel. A button greys out when the file kind has no
such view (a `.shprim` has no schematic, a `.shbench` has no symbol).

The docs page opens on its own when the cell has doc blocks. Writing it is covered in
[docs-pages.md](docs-pages.md). The cell settings panel is the structured editor for the cell's
header (`doc`, `prefix`, `category`), pins, parameters and implementations: use it to reorder
pins, add a parameter or edit a SPICE body without touching the file.

Menus, toolbar, dialogs and keys all come from the bundled `default-ui` plugin. Your own
`init.lua` can rebind any key ([config.md](config.md)).

### Themes

`Shift+O` (View → Toggle colorscheme) switches the canvas between dark and light. View →
Modern theme switches the menus and panels to a dark, flatter palette without changing any key
or menu entry. The modern theme lasts for the session only; you turn it on again after a
restart.

## Mouse

| Gesture | Action |
|---|---|
| Click | Select the object under the pointer (a click on empty canvas clears the selection) |
| `Shift` + click | Add to the selection |
| `Alt` + click | Remove from the selection |
| Double click | Properties of the object under the pointer |
| Drag on empty canvas | Box select; dragging right to left also selects objects the box only touches |
| `Shift` + drag on empty canvas | Box select, added to the selection |
| `Ctrl` + drag on empty canvas | Partial select: objects inside, plus the wire ends inside the box, so the next move stretches those wires |
| Drag a selected object | Move the selection |
| `Shift` + drag a selected object | Copy the selection |
| `Ctrl` + drag a selected object | Move, stretching attached wires |
| Press on a pin or wire end and drag | Draw a wire from that point |
| Drag an end or vertex of the one selected wire, line, rectangle or polygon | Move that point only |
| Wheel | Zoom at the pointer |
| `Shift` + wheel / `Ctrl` + wheel | Pan horizontally / vertically |
| Middle drag | Pan |
| Right drag | Zoom to the dragged box |
| Right click | Context menu (insert tools, paste, or actions on the selection); while drawing, it ends the tool |
| `Shift` + right click | Select the wires, labels and ports connected to the object |
| `Ctrl` + right click | The same, stopping at junctions of three or more wires |
| `Alt` + right click | Cut the wire under the pointer at the snapped position |
| `Alt+Shift` + right click | Cut the wire at the exact pointer position |

With Options → Enable stretch (`y`) on, plain drags and `Ctrl` drags swap roles: a plain drag
stretches wires.

## Placing symbols

1. Press `Ins` or `Shift+I` (or the toolbar's Symbol button). A chooser lists every cell you
   can place, as `lib:path`, for example `core:res.shprim` or `dev:nmos.shprim`. Cells of the
   project itself have no library prefix.
2. Type to filter the list, then pick a row. The symbol follows the pointer.
3. Before you click, `Shift+R` rotates it and `Shift+F` / `Shift+V` flip it.
4. Click to place it. The instance gets the next free name from the cell's prefix (`R1`, `M3`,
   `x2`).

The bundled devices (resistor, MOSFETs, sources and so on) are in the `core` library, listed in
[files.md](files.md#the-bundled-core-library).

## Wires

Press `w`, click the start point and click the end point. A drag that starts on a pin or a wire
end draws a wire without pressing `w`. `Shift+W` starts the wire at the connection point
nearest the pointer, and `Shift` + click ends it on the nearest one.

| To | Do |
|---|---|
| Change the L-shape of the wire being drawn | `Space` cycles the routing (straight, horizontal first, vertical first) |
| Route every wire as an L by its dominant direction | `Shift+L` (Options → Enable orthogonal wiring) |
| Stay in wire mode after each segment | Options → Persistent wire/line place command |
| Lock the next move or wire to one axis | `h` (horizontal), `v` (vertical) |
| Merge collinear wires, drop duplicates, split at T junctions | `&`, or Options → Auto Join/Trim Wires to do it after every wire edit |
| Abort | `Esc` |

Wires connect at their end points and wherever a pin, label or wire end lies on a wire. Two
wires that cross in their middles do not connect.

## Labels, ports and pins

A net gets its name from a label or a port on it. Unnamed nets get a generated name.

| To place | Key | Then |
|---|---|---|
| Net label | `Alt+L` (or `Alt+Shift+L`) | Type the net name, OK, click on a wire end or pin |
| Input port | `Ctrl+P` | Type the pin name, OK, click |
| Output port | `Ctrl+Shift+P` | Type the pin name, OK, click |
| Symbol pin (symbol view only) | `Alt+P` | Type the pin name, OK, click |
| Annotation text | `t` | Type the text, OK, click |

Labels with the same name connect anywhere in the cell. The name `0`, names ending in `!` and
labels marked global are the same net in every cell. Name your ground net with a label `0`.

Ports are the interface of a `.shcomp`. After placing them, press `a` (Symbol → Make symbol from
schematic) to rebuild the symbol: one pin per port, inputs on the left, outputs and inouts on
the right, a box and the cell name. `Ctrl+L` does the reverse and adds a port for every symbol
pin that has none. `Shift+H` puts a label named after each pin on every unconnected pin of the
selected instances.

## Properties

Press `q` or double-click an object. For an instance, the dialog shows its name and one field per
parameter of its cell, with the default in the label. Leave a field empty to use the default; a
value you type is stored as an override. Computed parameters are shown but not editable.
Labels, ports and texts show their text.

Other property commands:

| Key | Action |
|---|---|
| `Shift+B` | Edit the cell header: `doc`, `prefix`, `category` |
| `Shift+T` | Toggle `ignore` on the selected instances (they leave the netlist) |
| `Alt+Q` | Edit the whole file as text; OK applies it as one undoable edit |

`Shift+Q` and `Ctrl+Shift+Q` open the same dialog as `q`.

## Selecting, moving and copying

| Key | Action |
|---|---|
| `Ctrl+A` | Select all |
| `d` | Unselect the object under the pointer |
| `Shift+D` | Unselect by area (the next drag) |
| `m` | Move the selection (click to drop) |
| `Ctrl+M` | Move, stretching wires attached to moved pins |
| `Shift+M` | Move, stretching wires and adding a wire wherever a moved pin leaves something it touched |
| `c` | Duplicate the selection on the pointer |
| `Shift+R`, `Shift+F`, `Shift+V` | Rotate, flip, vertical flip the selection as a group |
| `Alt+R`, `Alt+F`, `Alt+V` | Rotate, flip, vertical flip each object about its own anchor |
| `Del` | Delete |
| `Ctrl+C`, `Ctrl+X`, `Ctrl+V` | Copy, cut, paste (paste follows the pointer; click to drop) |
| `Alt+U` | Snap the selection (else everything) to the snap grid |

`y` toggles stretching for plain moves, so `m` and `Ctrl+M` swap. The clipboard holds plain
Schemify text, so you can paste a fragment into another editor window, a text file or a chat
and paste it back.

## Undo

`u` undoes and `Shift+U` redoes. `Ctrl+Z` zooms out, as in xschem; it does not undo. Each
document has its own history, which survives moving up and down the hierarchy and also a
restart: it is kept in the project's `.schemify/journal/`.

## Hierarchy

| Key | Action |
|---|---|
| `e` | Descend into the selected instance's schematic |
| `i` | Descend into the selected instance's symbol |
| `Ctrl+E` or `BackSpace` | Back to the parent |
| `Alt+E` | Open the selected instance's schematic in a new tab |
| `Alt+I` | Open the selected instance's symbol in a new tab |
| `Ctrl+Shift+H` | Make a subcircuit: move the selection into a new cell and put an instance of it in its place |

Select exactly one instance before descending. A primitive (`.shprim`) has no schematic; use `i`
for its symbol. Cells of the bundled `core` library open read-only.

## Search and replace

`Ctrl+F` opens the Search / replace dialog:

| Field | Meaning |
|---|---|
| field | `name` (instance names), `cell` (cell references), `net` (label and port names), `text` (annotations), any parameter name, or `*` for all of them |
| value | Text to find (case-sensitive) |
| replace with | Empty: select the matches and zoom to them. Otherwise: replace every match, as one undoable edit |
| exact match | `true` matches whole values only; `false` matches substrings |

## Highlighting nets

| Key | Action |
|---|---|
| `k` | Highlight the nets of the selection, or of the object under the pointer, through the whole hierarchy |
| `Ctrl+K` | Remove the highlight from the selected nets |
| `Shift+K` | Clear every highlight |
| `Alt+K` | Select the wires, labels and ports of the highlighted nets |
| `5` | Dim everything but the highlighted nets |
| `j` | List the highlighted nets in a text tab |
| `Alt+J`, `Ctrl+J` | Put one label, or one port, per highlighted net on the pointer |

The Highlight menu also has Auto-highlight (highlight follows the selection), Increment Hilight
Color (a new colour per highlight) and a schematic comparison with another cell.

## Live checks

Schemify checks the cell while you edit: dangling pins, shorted labels, parameters out of
range and the rest of the rules `schemify check` applies. Problems are marked on the canvas as
you draw. Simulation → Checks panel (or View → Show / Hide → Show ERC Info window) lists them
with their code (`E…` errors, `W…` warnings); click a row to select and zoom to the object.
Netlisting (`n`) refuses a cell with errors and shows the blocking lines in the panel. The codes
are explained in [netlisting.md](netlisting.md).

## Netlist and simulate

`n` writes the netlist of the tab's top cell to `.schemify/netlist/<cell>.<ext>` and shows it in
a text tab; `Shift+N` netlists the cell at the current hierarchy level. `Shift+A` turns the text
tab off or on. Pick the dialect in Simulation → Netlist dialect, or cycle it with
`Ctrl+Shift+V`; the status bar shows the current one.

`s` simulates the active `.shbench` without blocking the editor. The results table (Simulation
→ Results table), the waves dock (Waves menu) and operating-point annotation are covered in
[simulation.md](simulation.md). With the waves dock open, clicking a net adds its trace, and
`Alt+G` sends every highlighted net. `Ctrl+W` with the dock open closes the dock, not the tab.

When the optimizer or a plugin proposes a change, it appears as a staged edit drawn over the
schematic. Edit → Accept staged edit applies it (one undo); Edit → Reject staged edit drops it.

## Export images

| Key | Menu (File → Image export) | Output |
|---|---|---|
| `Ctrl+*` or `Print` | PNG Export | PNG of the visible canvas |
| `Alt+*` | SVG Export | SVG of the whole drawing |
| `*` | PDF/PS Export | PDF of the selection, else of the visible area |
| | PDF/PS Export Full | PDF of the whole drawing |
| | Hierarchical PDF/PS Export | One PDF page per schematic of the hierarchy below this cell |
| | EPS Selection Export | EPS of the selection, else of the whole drawing |

Files go to `.schemify/export/<cell>.<ext>`; the status line shows the path. Options → Color
Postscript/SVG switches between colour and black on white, and Options → Transparent SVG
background drops the SVG background.

A few export settings have no menu entry yet. Bind them in `init.lua`:

```lua
schemify.keymap.set("F6", function() schemify.cmd("file.image_export.png_export_full") end)
schemify.keymap.set("F7", function() schemify.cmd("options.png_export_scale", "2") end)
schemify.keymap.set("F8", function() schemify.cmd("options.export_page_size", "a4") end)
```

`png_export_full` exports the whole drawing, the scale sets PNG resolution (1 is screen
resolution), and the page size is what PDF and EPS exports fit (`a4`, `letter portrait`, …).

## Files and tabs

`Ctrl+N` opens a new schematic in a new tab, saved as `untitled1.shcomp` in the project root
until you use Save as (`Ctrl+Shift+S`, a project-relative path). `Ctrl+S` saves the active
document and the tab's top cell. Saves write the canonical form ([files.md](files.md)).

`Alt+S` reloads the active file from disk after you changed it in another program. Reload
refuses while you have unsaved edits: save or undo them first.

`Ctrl+W` closes the tab and `Ctrl+Q` quits. With unsaved changes, both ask you to press the
key again to discard them.

## Crash recovery

Every edit is written to the cell's journal in `.schemify/journal/` as you work. If the editor
or the machine dies with unsaved edits, the next time you open that cell a dialog offers
**Recover** (replay the edits) or **Discard**. Until you choose, the cell refuses edits and
saves. The status line reports how many edits were recovered and how many no longer applied.

## Running without the default UI

```sh
schemify edit --no-default-ui path/to/cell.shcomp
```

starts the bare core: window and canvas, mouse only, no menus, toolbar or keys. Use it to check
that a problem is not in the UI plugin, or as the base for a UI plugin of your own. Plugins
from your config and the project still load.

## Keys

Plain letters are lowercase: `r` is the r key, `Shift+R` is capital R. Every binding can be
changed with `schemify.keymap.set` in `init.lua` ([config.md](config.md)).

### Files and tabs

| Key | Action |
|---|---|
| `Ctrl+O` | Open a cell (chooser) |
| `Ctrl+Shift+O` | Open the most recent file |
| `Ctrl+S` | Save |
| `Ctrl+Shift+S` | Save as |
| `Ctrl+Alt+S` | Save as, then show the symbol |
| `Alt+S` | Reload from disk |
| `Ctrl+N` | New schematic in a new tab |
| `Ctrl+Shift+N` | New symbol (`.shprim`) in a new tab |
| `x` | New tab |
| `Ctrl+W` | Close the tab (the waves dock, when open) |
| `Ctrl+Q` | Quit |
| `Ctrl+Left`, `Ctrl+Right` | Previous, next tab |
| `b` | Merge: place another cell's schematic on the pointer |
| `Ctrl+D` | Delete cell files from disk (pick twice to confirm) |

### View

| Key | Action |
|---|---|
| `f` | Zoom to fit everything |
| `Ctrl+Shift+F` | Zoom to the selection |
| `z` | Zoom box (next drag) |
| `Shift+Z`, `Ctrl+Z` | Zoom in, zoom out |
| Arrow keys | Pan |
| `Shift+P` | Center on the pointer |
| `Space` | Pan with the pointer (when not drawing) |
| `Esc` | Abort the current command, unselect, redraw |
| `\` | Canvas only (hide menus, toolbar, tabs, status) |
| `Alt+D` | Docs page |
| `Shift+O` | Dark / light canvas |
| `%` | Grid |
| `g`, `Shift+G` | Halve, double the snap |
| `Ctrl+G` | Snap value dialog |
| `Alt+X` | Crosshair |
| `Alt+Z` | Snap cursor on the nearest connection point |
| `Ctrl+B` | Show / hide texts inside symbols |
| `Alt+B` | Cycle symbol display: details, boxes for subcircuits, boxes for all |
| `_` | Line width follows the zoom |
| `Ctrl++`, `Ctrl+-` | Thicker, thinner lines |
| `Alt+-` | Line width dialog |
| `Ctrl+=` | Fill patterns on / off |
| `Ctrl+0` … `Ctrl+9` | Current drawing layer |
| `>`, `<` | Show only the current layer, show all layers |

### Drawing

| Key | Action |
|---|---|
| `w` | Wire |
| `Shift+W` | Wire from the nearest connection point |
| `Shift+L` | Orthogonal wiring on / off |
| `Space` | Cycle the L routing while drawing or moving |
| `l` | Line |
| `r` | Rectangle |
| `p` | Polygon (click vertices; `Enter` or the first vertex closes it) |
| `Shift+C` | Arc |
| `Ctrl+Shift+C` | Circle |
| `t` | Text |
| `&` | Join / trim wires |
| `!` | Break wires at the selected instances' pins |
| `Ctrl+!` | Remove wires running through the selected instances' pins |

In the symbol view, lines, rectangles, polygons, arcs and circles become symbol graphics; in a
schematic they are annotation shapes with no connectivity.

### Placing

| Key | Action |
|---|---|
| `Ins`, `Shift+I` | Insert symbol |
| `Ctrl+I`, `Shift+Ins` | Component browser (the same chooser) |
| `Alt+L`, `Alt+Shift+L` | Net label |
| `Ctrl+P` | Input port |
| `Ctrl+Shift+P` | Output port |
| `Alt+P` | Symbol pin (symbol view) |
| `Shift+H` | Label every unconnected pin of the selected instances |

### Editing

| Key | Action |
|---|---|
| `u`, `Shift+U` | Undo, redo |
| `Ctrl+A` | Select all |
| `d`, `Shift+D` | Unselect under the pointer, unselect by area |
| `Del` | Delete |
| `m` | Move |
| `Ctrl+M` | Move, stretching attached wires |
| `Shift+M`, `Ctrl+Shift+M` | Move, stretching and adding wires to connected pins |
| `c`, `Alt+C` | Duplicate |
| `h`, `v` | Constrain to horizontal, vertical |
| `y` | Stretch on plain moves on / off |
| `Shift+R`, `Alt+R` | Rotate the selection, rotate each object in place |
| `Shift+F`, `Alt+F` | Flip the selection, flip each object in place |
| `Shift+V`, `Alt+V` | Vertical flip, vertical flip in place |
| `Ctrl+C`, `Ctrl+X`, `Ctrl+V` | Copy, cut, paste |
| `Alt+U` | Align to the snap grid |
| `Ctrl+F` | Search / replace |
| `q` | Properties |
| `Shift+B` | Cell header |
| `Shift+T` | Toggle `ignore` on selected instances |
| `Alt+Q` | Edit the file as text |

### Hierarchy and symbols

| Key | Action |
|---|---|
| `e`, `i` | Descend into the schematic, the symbol |
| `Ctrl+E`, `BackSpace` | Back to the parent |
| `Alt+E`, `Alt+I` | Schematic, symbol in a new tab |
| `a` | Make symbol from schematic |
| `Ctrl+L` | Make schematic ports from the symbol |
| `Alt+H` | Symbol pins from the selected ports |
| `Ctrl+Shift+H` | Make a subcircuit from the selection |

### Nets

| Key | Action |
|---|---|
| `k`, `Ctrl+Shift+K` | Highlight the selected nets |
| `Ctrl+K` | Unhighlight the selected nets |
| `Shift+K` | Clear all highlights |
| `Alt+K` | Select the highlighted nets |
| `5` | Dim all but the highlight |
| `j` | List highlighted nets |
| `Ctrl+Alt+J` | List highlighted nets, buses expanded |
| `Alt+J`, `Alt+Shift+J` | Labels from highlighted nets (the second with an `i` prefix) |
| `Ctrl+J` | Ports from highlighted nets |
| `Shift+X` | Highlight nets whose name differs from the pin name on selected instances |
| `#` | Select instances with duplicate names |
| `Ctrl+#` | Rename duplicate instance names |

### Netlist, simulation, export, help

| Key | Action |
|---|---|
| `n`, `Shift+N` | Netlist the top cell, the current level |
| `s` | Simulate the bench |
| `Shift+A` | Show the netlist after netlisting on / off |
| `Ctrl+Shift+V` | Next netlist dialect |
| `:` | Flat SPICE netlist on / off |
| `Alt+G`, `Ctrl+Shift+X` | Send highlighted nets to the waves dock |
| `Ctrl+*`, `Print` | PNG export |
| `Alt+*` | SVG export |
| `*` | PDF export |
| `?` | Help text |
| `/` | Key summary panel |

See also: [files.md](files.md), [simulation.md](simulation.md), [docs-pages.md](docs-pages.md),
[config.md](config.md), [cli.md](cli.md).
