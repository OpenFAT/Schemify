//! The bundled default UI (D6): the xschem-parity shell built on the same ABI as any
//! third-party plugin. The app embeds this module (`crates/app/assets/default_ui.wasm`, rebuilt
//! by `plugins/build.sh`) and loads it first, so every later plugin overrides it (D8). Without
//! it the core still runs: a blank window with the canvas.
//!
//! On `Load` it contributes three immediate-mode sets:
//! - `ui` — `ui.records`: menus, toolbar, tabs, canvas, status bar, dialogs, theme and the
//!   canvas palette (`canvas.<dark|light>.<layer>`, `.grid`, `.sel` style selectors, `fg`).
//! - `keymap` — `keymap.records`: xschem's canvas bindings (D25).
//! - `commands` — the commands this plugin implements itself: `help.keys` and
//!   `help.show_keybindings`, which toggle a key-summary panel (`keys.panel`) by re-contributing
//!   the `ui` set.
//!
//! Every other command named by a menu item or key (`file.save`, `edit.undo`, …) is a core
//! command the app dispatches; this plugin only binds names to widgets and keys.
//!
//! Contract the app relies on (node ids it `set`s or adds children to):
//! - `tab.0` (first tab; the app retitles it and adds `tab.<i>` siblings), `tabs`, `canvas`;
//! - `docs.pane` (removed by the app while the active tab hides its docs) holding the `docs.bar`
//!   toolbar (`docs.add_prose`, `docs.add_var`, `docs.move_up`, `docs.move_down`,
//!   `docs.delete_block`, `view.docs`) and the `docs.page` scroll node, under which the app adds
//!   one `docs.b.<row>` panel per doc block (`docs/spec/docs-page.md`);
//! - `status.snap`, `status.grid` (text fields, values), `status.mode`, `status.msg` (labels);
//! - `dialog.enter_text` with `dialog.enter_text.title` / `.input` (command
//!   `dialog.enter_text.ok`);
//! - `dialog.choose` with `dialog.choose.title`, `.filter` (command `choose.filter`) and
//!   `.list`, under which the app adds `dialog.choose.row.<i>` items (command `choose.pick`);
//! - `dialog.props` with `dialog.props.title`, `.fields` (rows `dialog.props.row.<i>` holding
//!   `dialog.props.key.<i>` and `dialog.props.val.<i>`, command `props.field`), OK
//!   (`props.ok`) and Cancel (`dialog.close`).

use std::cell::Cell;

use schemify_abi::guest::{self, Host};
use schemify_abi::{Event, Kind, Out};

guest::export!(on_event);

/// The `ui` contribution set.
const UI: &str = include_str!("../ui.records");
/// The `keymap` contribution set.
const KEYMAP: &str = include_str!("../keymap.records");
/// Commands owned by this plugin (routed to it by the app as `Event::Command`).
const COMMANDS: &str =
    "command help.keys \"Key bindings\"\ncommand help.show_keybindings \"Show keybindings\"\n";
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

fn on_event(ev: Event<'_>, _host: &Host, out: &mut Out) {
    match ev {
        Event::Load => {
            out.contribute(Kind::Ui, UI);
            out.contribute(Kind::Keymap, KEYMAP);
            out.contribute_commands(COMMANDS);
        }
        Event::Command {
            name: "help.keys" | "help.show_keybindings",
            ..
        } => {
            let shown = !KEYS_SHOWN.get();
            KEYS_SHOWN.set(shown);
            // Immediate mode: re-declare the whole set; the host replaces the previous one.
            if shown {
                out.contribute(Kind::Ui, &format!("{UI}{KEYS_PANEL}"));
            } else {
                out.contribute(Kind::Ui, UI);
            }
            assert!(out.as_str().starts_with("contribute ui"));
        }
        _ => {}
    }
}
