//! The bundled default UI (D6): the xschem-parity shell built on the same ABI as any
//! third-party plugin. The app embeds this module (`crates/model/editor/assets/default_ui.wasm`, rebuilt
//! by `ui/build.sh`) and loads it first, so every later plugin overrides it (D8). Without
//! it the core still runs: a blank window with the canvas.
//!
//! On `Load` it contributes three immediate-mode sets:
//! - `ui` — `ui.records`: menus, toolbar, tabs, canvas, status bar, dialogs, theme and the
//!   canvas palette (`canvas.<dark|light>.<layer>`, `.grid`, `.sel` style selectors, `fg`).
//! - `keymap` — `keymap.records`: xschem's canvas bindings (D25).
//! - `commands` — the command this plugin implements itself: `help.keys` (key `/`), which
//!   toggles a key-summary panel (`keys.panel`) and re-contributes the `ui` set. Themes are
//!   records the core composes (`view.theme.<id>`, `ui/themes/`).
//!
//! Every other command named by a menu item or key (`file.save`, `edit.undo`, …) is a core
//! command the app dispatches; this plugin only binds names to widgets and keys. The mouse
//! chords `MouseMiddle`, `MouseRight`, `Wheel` (with modifiers) are keymap rows too.
//!
//! It also writes a `state` record (`state.rs`) on `Load` and on every `StateChanged`: the
//! session-state tables (`Tabs`, `Tool`, `Status`, `Options`, `Dialog`, `DialogRows`)
//! rendered into its own nodes: the tab bar (`tab.0`, then `tab.<i>` under `tabs`), the status
//! fields `status.snap`, `status.grid`, `status.msg`, the option check marks
//! (`options.draw_grid`, `options.enable_orthogonal_wiring`, the `edit.*constrained*_move`
//! items), and the titles, fields and rows of `dialog.enter_text`, `dialog.choose` and
//! `dialog.props`. Any other UI plugin can render the same tables its own way.
//!
//! Contract the app relies on (node ids it `set`s or adds children to):
//! - `tabs`, `canvas`;
//! - `docs.pane` (removed by the app while the active tab hides its docs) holding the `docs.bar`
//!   toolbar (`docs.add_prose`, `docs.add_var`, `docs.move_up`, `docs.move_down`,
//!   `docs.delete_block`, `view.docs`, and the `docs.source` button, whose `label`, `checked`
//!   and `style` (`docs-toggle` / `docs-toggle-on`) the app sets) and the `docs.page` scroll
//!   node, under which the app adds one `docs.b.<row>` panel per doc block, styled with
//!   `docs-block`, `docs-source`, `docs-preview-cap`, `docs-preview` … (`docs/spec/docs-page.md`);
//! - `status.mode` (a label);
//! - `view.switch`, a child of `canvas` (so it floats over the schematic), holding the buttons
//!   `switch.schematic`, `switch.symbol`, `switch.docs`: the app sets each one's `style` to
//!   `viewsw-on`, `viewsw` or `viewsw-off` and `disabled` on the side the file kind lacks, and
//!   removes `view.switch` in a text tab;
//! - `cell.pane` with the `cell.bar` toolbar and the `cell.page` scroll node (the cell settings
//!   panel, `view.cell`): the app removes `cell.pane` while hidden and adds its rows under
//!   `cell.page` (`crates/model/editor/src/cell.rs`);
//! - the dialogs the app opens by id: `dialog.enter_text` (OK runs `dialog.enter_text.ok`),
//!   `dialog.choose` (rows run `choose.pick`, the filter `choose.filter`), `dialog.props`
//!   (fields run `props.field`; OK `props.ok`, Cancel `dialog.close`);
//! - `dialog.ctx` (the canvas context menu: the app adds `dialog.ctx.row.<i>` items running
//!   ordinary commands and opens it at the pointer), `dialog.layers` with `.list` (the app adds
//!   checkable `dialog.layers.row.<n>` items, command `layers.toggle.<n>`), `dialog.edit_file`
//!   with the `.text` text area (command `dialog.edit_file.ok`), `dialog.recover` with `.title`
//!   (commands `dialog.recover.ok` / `.discard`), and `menu.file.open_recent`, under which the
//!   app adds one `file.open_recent.<i>` item per recent file;
//! - the chooser's `dialog.choose.list` is a `scroll` node inside a fixed-height box;
//! - the docks `dock.left`, `dock.right`, `dock.bottom`, each the kept side of its
//!   `dock.<side>.split`, where the app shows the open pane's content (`<id>.pane`: the PDKs
//!   panel, the checks panel, the results table, the core's placeholders) under a header it
//!   adds, and which it removes while hidden; the icon bar's `activity.<pane id>` buttons and
//!   the `view.toggle_<side>` items, whose `checked` the app sets; `work`, whose `canvas` the
//!   app replaces with a `textview` in a netlist tab; the checkable items `sim.dialect.<word>`,
//!   `simulation.show_netlist_after_netlist_command` and `waves.op_annotate`, whose `checked`
//!   the app sets.

#![deny(unsafe_code)] // not `forbid`: the SDK's `export!` allows `unsafe(no_mangle)`

mod state;

use std::cell::Cell;

use schemify_abi::guest::{self, Host};
use schemify_abi::{Event, Kind, Out};

guest::export!(on_event);

/// The `ui` contribution set.
const UI: &str = include_str!("../ui.records");
/// The `keymap` contribution set.
const KEYMAP: &str = include_str!("../keymap.records");
/// Commands owned by this plugin (routed to it by the app as `Event::Command`).
const COMMANDS: &str = "command help.keys \"Key bindings\"\n";
/// The key-summary panel `help.keys` toggles: appended to the `ui` set while shown.
const KEYS_PANEL: &str = "
style keys-panel
  bg ffffcc
  dir column
  pad 6
  pad-y 4
  gap 2
node keys.panel panel
  parent window
  style keys-panel
node keys.edit label
  parent keys.panel
  label \"w wire   m move   c copy   Del delete   Shift+R rotate   Shift+F flip   u / Shift+U undo / redo\"
node keys.place label
  parent keys.panel
  label \"Ins insert symbol   Alt+L net label   Ctrl+P port   t text   q properties\"
node keys.view label
  parent keys.panel
  label \"f full zoom   z zoom box   Shift+Z / Ctrl+Z zoom in / out   e / Ctrl+E descend / return\"
";

thread_local! {
    /// Whether `keys.panel` is shown. The guest is single-threaded; the cell outlives events.
    static KEYS_SHOWN: Cell<bool> = const { Cell::new(false) };
}

/// The `ui` set for the current toggles: the base, then the keys panel. Themes (Modern
/// among them) are records the core composes after every plugin (`view.theme.<id>`).
fn ui_set() -> String {
    let mut s = String::with_capacity(UI.len() + KEYS_PANEL.len());
    s.push_str(UI);
    if KEYS_SHOWN.get() {
        s.push_str(KEYS_PANEL);
    }
    assert!(s.len() >= UI.len());
    s
}

fn on_event(ev: Event<'_>, host: &Host, out: &mut Out) {
    match ev {
        Event::Load => {
            out.contribute(Kind::Ui, UI);
            out.contribute(Kind::Keymap, KEYMAP);
            out.contribute_commands(COMMANDS);
            // The first `state` record subscribes this plugin to `StateChanged`.
            out.state(&state::render(host));
        }
        Event::StateChanged => out.state(&state::render(host)),
        Event::Command {
            name: "help.keys", ..
        } => {
            KEYS_SHOWN.set(!KEYS_SHOWN.get());
            // Immediate mode: re-declare the whole set; the host replaces the previous one.
            out.contribute(Kind::Ui, &ui_set());
            assert!(out.as_str().starts_with("contribute ui"));
        }
        _ => {}
    }
}
