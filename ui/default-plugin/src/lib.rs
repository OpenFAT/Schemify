//! The bundled default UI (D6): the xschem-parity shell built on the same ABI as any
//! third-party plugin. The app embeds this module (`crates/app/assets/default_ui.wasm`, rebuilt
//! by `ui/build.sh`) and loads it first, so every later plugin overrides it (D8). Without
//! it the core still runs: a blank window with the canvas.
//!
//! On `Load` it contributes three immediate-mode sets:
//! - `ui` — `ui.records`: menus, toolbar, tabs, canvas, status bar, dialogs, theme and the
//!   canvas palette (`canvas.<dark|light>.<layer>`, `.grid`, `.sel` style selectors, `fg`).
//! - `keymap` — `keymap.records`: xschem's canvas bindings (D25).
//! - `commands` — the commands this plugin implements itself: `help.keys` (key `/`), which
//!   toggles a key-summary panel (`keys.panel`), and
//!   `view.theme_modern`, which toggles the `modern.records` style preset; both re-contribute
//!   the `ui` set.
//!
//! Every other command named by a menu item or key (`file.save`, `edit.undo`, …) is a core
//! command the app dispatches; this plugin only binds names to widgets and keys.
//!
//! Contract the app relies on (node ids it `set`s or adds children to):
//! - `tab.0` (first tab; the app retitles it and adds `tab.<i>` siblings), `tabs`, `canvas`;
//! - `docs.pane` (removed by the app while the active tab hides its docs) holding the `docs.bar`
//!   toolbar (`docs.add_prose`, `docs.add_var`, `docs.move_up`, `docs.move_down`,
//!   `docs.delete_block`, `view.docs`, and the `docs.source` button, whose `label`, `checked`
//!   and `style` (`docs-toggle` / `docs-toggle-on`) the app sets) and the `docs.page` scroll
//!   node, under which the app adds one `docs.b.<row>` panel per doc block, styled with
//!   `docs-block`, `docs-source`, `docs-preview-cap`, `docs-preview` … (`docs/spec/docs-page.md`);
//! - `status.snap`, `status.grid` (text fields, values), `status.mode`, `status.msg` (labels);
//! - `view.switch`, a child of `canvas` (so it floats over the schematic), holding the buttons
//!   `switch.schematic`, `switch.symbol`, `switch.docs`: the app sets each one's `style` to
//!   `viewsw-on`, `viewsw` or `viewsw-off` and `disabled` on the side the file kind lacks, and
//!   removes `view.switch` in a text tab;
//! - `cell.pane` with the `cell.bar` toolbar and the `cell.page` scroll node (the cell settings
//!   panel, `view.cell`): the app removes `cell.pane` while hidden and adds its rows under
//!   `cell.page` (`crates/app/src/cell.rs`);
//! - `dialog.enter_text` with `dialog.enter_text.title` / `.input` (command
//!   `dialog.enter_text.ok`);
//! - `dialog.choose` with `dialog.choose.title`, `.filter` (command `choose.filter`) and
//!   `.list`, under which the app adds `dialog.choose.row.<i>` items (command `choose.pick`);
//! - `dialog.props` with `dialog.props.title`, `.fields` (rows `dialog.props.row.<i>` holding
//!   `dialog.props.key.<i>` and `dialog.props.val.<i>`, command `props.field`), OK
//!   (`props.ok`) and Cancel (`dialog.close`);
//! - `dialog.ctx` (the canvas context menu: the app adds `dialog.ctx.row.<i>` items running
//!   ordinary commands and opens it at the pointer), `dialog.layers` with `.list` (the app adds
//!   checkable `dialog.layers.row.<n>` items, command `layers.toggle.<n>`), `dialog.edit_file`
//!   with the `.text` text area (command `dialog.edit_file.ok`), `dialog.recover` with `.title`
//!   (commands `dialog.recover.ok` / `.discard`), and `menu.file.open_recent`, under which the
//!   app adds one `file.open_recent.<i>` item per recent file;
//! - the chooser's `dialog.choose.list` is a `scroll` node inside a fixed-height box;
//! - `bottom` (under the work area), where the app puts the checks panel (`checks.pane`) or
//!   the results table (`results.pane`); `work`, whose `canvas` the app replaces with a
//!   `textview` in a netlist tab; the checkable items `sim.dialect.<word>`,
//!   `simulation.show_netlist_after_netlist_command` and `waves.op_annotate`, whose `checked`
//!   the app sets.

#![deny(unsafe_code)] // not `forbid`: the SDK's `export!` allows `unsafe(no_mangle)`

use std::cell::Cell;

use schemify_abi::guest::{self, Host};
use schemify_abi::{Event, Kind, Out};

guest::export!(on_event);

/// The `ui` contribution set.
const UI: &str = include_str!("../ui.records");
/// The "Modern" preset: style overrides appended after [`UI`] (later edits win); nodes and
/// keys unchanged.
const MODERN: &str = include_str!("../modern.records");
/// The `keymap` contribution set.
const KEYMAP: &str = include_str!("../keymap.records");
/// Commands owned by this plugin (routed to it by the app as `Event::Command`).
const COMMANDS: &str =
    "command help.keys \"Key bindings\"\ncommand view.theme_modern \"Modern theme\"\n";
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
    /// Whether the [`MODERN`] preset is applied. Not persisted across restarts.
    // ponytail: session-only; persist through an init.lua option once config reaches plugins.
    static MODERN_ON: Cell<bool> = const { Cell::new(false) };
}

/// The `ui` set for the current toggles: the base, the keys panel, then the preset last so its
/// styles win.
fn ui_set() -> String {
    let mut s = String::with_capacity(UI.len() + MODERN.len() + KEYS_PANEL.len());
    s.push_str(UI);
    if KEYS_SHOWN.get() {
        s.push_str(KEYS_PANEL);
    }
    if MODERN_ON.get() {
        s.push_str(MODERN);
    }
    assert!(s.len() >= UI.len());
    s
}

fn on_event(ev: Event<'_>, _host: &Host, out: &mut Out) {
    match ev {
        Event::Load => {
            out.contribute(Kind::Ui, UI);
            out.contribute(Kind::Keymap, KEYMAP);
            out.contribute_commands(COMMANDS);
        }
        Event::Command {
            name: "help.keys",
            ..
        } => {
            KEYS_SHOWN.set(!KEYS_SHOWN.get());
            // Immediate mode: re-declare the whole set; the host replaces the previous one.
            out.contribute(Kind::Ui, &ui_set());
            assert!(out.as_str().starts_with("contribute ui"));
        }
        Event::Command {
            name: "view.theme_modern",
            ..
        } => {
            MODERN_ON.set(!MODERN_ON.get());
            out.contribute(Kind::Ui, &ui_set());
            assert!(out.as_str().starts_with("contribute ui"));
        }
        _ => {}
    }
}
