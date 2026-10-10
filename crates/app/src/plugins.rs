//! Plugin host glue for the frame spine: loads the bundled default UI (D6) and the
//! manifest's local plugins in configured order (D8), fills snapshot tables from `doc`,
//! runs `plugin::deliver`, and drains the request queues to their owners (edits to `doc`,
//! I/O to `platform`, logs to the status line).

use std::path::Path;

use schemify_doc::{DocHandle, Project};
use schemify_plugin::abi::{Event, FUEL_DEFAULT, Kind, Table, col};
use schemify_plugin::{IoOp, PluginEvents, PluginId, Plugins, RequestQueues, Snapshots, deliver};

use crate::platform;

/// The bundled default UI plugin, built by `plugins/build.sh`.
pub const DEFAULT_UI_WASM: &[u8] = include_bytes!("../assets/default_ui.wasm");

/// Delivery rounds per frame: a plugin's I/O completion is answered in the same frame, up to
/// this many request → completion hops.
const ROUNDS_MAX: u32 = 4;

/// Loaded plugins, their per-frame event table, snapshots and request queues.
pub struct Host {
    /// The plugin table (owner: `plugin`).
    pub plugins: Plugins,
    /// Events queued for the next delivery.
    pub events: PluginEvents,
    snapshots: Snapshots,
    queues: RequestQueues,
    /// Epoch of the last composition the app consumed.
    pub seen_epoch: Option<u32>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("plugins", &self.plugins.ids().len())
            .finish()
    }
}

/// What a delivery produced for the rest of the spine.
#[derive(Debug, Default)]
pub struct Delivered {
    /// Plugin edit transactions: (document, label, op text, stage?).
    pub edits: Vec<(DocHandle, String, String, bool)>,
    /// Log lines for the status line.
    pub logs: Vec<String>,
}

impl Host {
    /// Loads the bundled default UI (unless `no_default_ui`) and then every manifest plugin
    /// with a `path` source (a `.wasm` file, or a directory holding `<name>.wasm`), granting
    /// what the manifest declares. Load failures are returned as messages and recorded in
    /// the plugin health report.
    pub fn new(project: &Project, root: &Path, no_default_ui: bool) -> (Host, Vec<String>) {
        let mut plugins = Plugins::new();
        let mut notes = Vec::new();
        if !no_default_ui
            && let Err(c) = plugins.load("default-ui", DEFAULT_UI_WASM, &[], FUEL_DEFAULT)
        {
            notes.push(format!("default-ui: {c:?}"));
        }
        for rec in project.plugins() {
            let name = rec.fields.first().map_or("", String::as_str);
            let (Some("path"), Some(path)) =
                (rec.fields.get(1).map(String::as_str), rec.fields.get(2))
            else {
                notes.push(format!("plugin {name}: only path sources load in M2"));
                continue;
            };
            let mut file = root.join(path);
            if platform::is_dir(&file) {
                file = file.join(format!("{name}.wasm"));
            }
            let grants: Vec<&str> = rec
                .attrs
                .iter()
                .filter(|a| a.key == "grant")
                .filter_map(|a| a.values.first().map(String::as_str))
                .collect();
            match platform::read(&file) {
                Ok(wasm) => {
                    if let Err(c) = plugins.load(name, &wasm, &grants, FUEL_DEFAULT) {
                        notes.push(format!("plugin {name}: {c:?}"));
                    }
                }
                Err(e) => notes.push(format!("plugin {name}: {e}")),
            }
        }
        let host = Host {
            plugins,
            events: PluginEvents::new(),
            snapshots: Snapshots::new(),
            queues: RequestQueues::default(),
            seen_epoch: None,
        };
        (host, notes)
    }

    /// Queues an event (`None` = broadcast). A full table drops it with a note.
    pub fn push(&mut self, target: Option<PluginId>, ev: &Event<'_>) -> Result<(), String> {
        self.events
            .push(target, ev)
            .map_err(|c| format!("plugin event dropped: {c:?}"))
    }

    /// Returns whether a delivery has work (queued events or host-originated ones).
    pub fn has_work(&self) -> bool {
        !self.events.is_empty() || self.plugins.has_pending()
    }

    /// Refreshes the snapshot tables plugins read for the active document `h` (Docs,
    /// Instances, Wires, Labels, Selection). Cost: one copy of those columns.
    pub fn snapshot(&mut self, project: &Project, h: Option<DocHandle>, selection: &[u32]) {
        let s = &mut self.snapshots;
        let handles: Vec<DocHandle> = project.handles().collect();
        let ids: Vec<u32> = handles.iter().map(|&d| doc_id(d)).collect();
        s.fill::<u32>(Table::Docs, col::docs::HANDLE, &ids);
        let kinds: Vec<u8> = handles
            .iter()
            .map(|&d| project.doc(d).map_or(0, |x| x.kind() as u8))
            .collect();
        s.fill::<u8>(Table::Docs, col::docs::KIND, &kinds);
        let dirty: Vec<u8> = handles
            .iter()
            .map(|&d| u8::from(project.is_dirty(d)))
            .collect();
        s.fill::<u8>(Table::Docs, col::docs::DIRTY, &dirty);
        let empty: Vec<&str> = vec![""; handles.len()];
        s.fill_strings(Table::Docs, col::docs::PATH, &empty);
        s.fill_strings(Table::Docs, col::docs::CELL, &empty);
        let Some(doc) = h.and_then(|h| project.doc(h)) else {
            return;
        };
        let id = doc_id(h.expect("doc implies handle"));
        let st = doc.strings();
        let inst = doc.instances();
        let n = inst.id.len();
        s.fill::<u32>(Table::Instances, col::instances::DOC, &vec![id; n]);
        let raw: Vec<u32> = inst.id.iter().map(|i| i.0).collect();
        s.fill::<u32>(Table::Instances, col::instances::ID, &raw);
        s.fill::<i32>(Table::Instances, col::instances::X, &inst.x);
        s.fill::<i32>(Table::Instances, col::instances::Y, &inst.y);
        let o: Vec<u8> = inst.orient.iter().map(|&o| o as u8).collect();
        s.fill::<u8>(Table::Instances, col::instances::ORIENT, &o);
        let cells: Vec<&str> = inst.cell.iter().map(|&c| st.get(c)).collect();
        s.fill_strings(Table::Instances, col::instances::CELL, &cells);
        let names: Vec<&str> = inst.name.iter().map(|&c| st.get(c)).collect();
        s.fill_strings(Table::Instances, col::instances::NAME, &names);
        let w = doc.wires();
        let n = w.id.len();
        s.fill::<u32>(Table::Wires, col::wires::DOC, &vec![id; n]);
        let raw: Vec<u32> = w.id.iter().map(|i| i.0).collect();
        s.fill::<u32>(Table::Wires, col::wires::ID, &raw);
        s.fill::<i32>(Table::Wires, col::wires::X1, &w.x1);
        s.fill::<i32>(Table::Wires, col::wires::Y1, &w.y1);
        s.fill::<i32>(Table::Wires, col::wires::X2, &w.x2);
        s.fill::<i32>(Table::Wires, col::wires::Y2, &w.y2);
        // Net ids come from elab (integration point); u32::MAX = not elaborated.
        s.fill::<u32>(Table::Wires, col::wires::NET, &vec![u32::MAX; n]);
        let l = doc.labels();
        let n = l.id.len();
        s.fill::<u32>(Table::Labels, col::labels::DOC, &vec![id; n]);
        let raw: Vec<u32> = l.id.iter().map(|i| i.0).collect();
        s.fill::<u32>(Table::Labels, col::labels::ID, &raw);
        s.fill::<i32>(Table::Labels, col::labels::X, &l.x);
        s.fill::<i32>(Table::Labels, col::labels::Y, &l.y);
        s.fill::<u32>(Table::Labels, col::labels::NET, &vec![u32::MAX; n]);
        let texts: Vec<&str> = l.text.iter().map(|&t| st.get(t)).collect();
        s.fill_strings(Table::Labels, col::labels::TEXT, &texts);
        s.fill::<u32>(
            Table::Selection,
            col::selection::DOC,
            &vec![id; selection.len()],
        );
        s.fill::<u32>(Table::Selection, col::selection::ID, selection);
        debug_assert_eq!(s.check(), Ok(()));
    }

    /// The spine's `plugin::deliver` plus queue draining. I/O completions are fed back
    /// within the frame, at most `ROUNDS_MAX` (4) rounds. Timers are not supported yet (the
    /// event loop has no wake-up source) and are logged.
    pub fn deliver(&mut self, project: &Project, root: &Path) -> Delivered {
        let mut out = Delivered::default();
        for _ in 0..ROUNDS_MAX {
            if !self.has_work() {
                break;
            }
            deliver(
                &mut self.plugins,
                &self.events,
                &mut self.snapshots,
                &mut self.queues,
            );
            self.events.clear();
            for e in self.queues.edits.drain(..) {
                let Some(h) = handle_of(project, e.doc) else {
                    out.logs
                        .push(format!("{}: edit for a closed document", e.plugin));
                    continue;
                };
                let stage = e.mode == schemify_plugin::abi::EditMode::Stage;
                out.edits.push((h, e.label, e.ops, stage));
            }
            for l in self.queues.logs.drain(..) {
                let name = self.plugins.name(l.plugin).unwrap_or("plugin").to_owned();
                out.logs.push(format!("{name}: {}", l.text));
            }
            for t in self.queues.timers.drain(..) {
                out.logs
                    .push(format!("{}: timers are not supported yet", t.plugin));
            }
            self.queues.runs.clear(); // run integration point (M3)
            let evals: Vec<_> = self.queues.evals.drain(..).collect();
            for e in evals {
                // elab integration point: no evaluator yet.
                let gone = -(schemify_plugin::abi::Code::Gone as i32);
                let ev = Event::IoDone {
                    handle: e.handle,
                    status: gone,
                    bytes: b"",
                };
                let _ = self.events.push(Some(e.plugin), &ev);
            }
            let io: Vec<_> = self.queues.io.drain(..).collect();
            for r in io {
                let (status, bytes) = run_io(root, r.op, &r.args, &r.data);
                let ev = Event::IoDone {
                    handle: r.handle,
                    status,
                    bytes: &bytes,
                };
                let _ = self.events.push(Some(r.plugin), &ev);
            }
        }
        out
    }

    /// Returns `(plugin name, ui set + keymap set)` per plugin in load order, when the
    /// composition changed since the last call.
    pub fn ui_sets(&mut self) -> Option<Vec<(String, String)>> {
        let epoch = self.plugins.composed_epoch();
        if self.seen_epoch == Some(epoch) {
            return None;
        }
        self.seen_epoch = Some(epoch);
        let c = self.plugins.contributions();
        let sets = self
            .plugins
            .ids()
            .iter()
            .map(|&id| {
                let mut text = c.get(id, Kind::Ui, "").unwrap_or("").to_owned();
                text.push('\n');
                text.push_str(c.get(id, Kind::Keymap, "").unwrap_or(""));
                (self.plugins.name(id).unwrap_or("?").to_owned(), text)
            })
            .collect();
        Some(sets)
    }
}

/// Plugin-visible document id: slot index in the low 16 bits, generation in the high 16
/// (`doc::DOCS_MAX` is 2^16), so a handle kept across a close is detected as gone.
pub fn doc_id(h: DocHandle) -> u32 {
    assert!(h.index < 1 << 16);
    (h.generation << 16) | h.index
}

/// The live handle a plugin's document id names.
pub fn handle_of(project: &Project, id: u32) -> Option<DocHandle> {
    project.handles().find(|&h| doc_id(h) == id)
}

/// Executes an authorized I/O request natively (project-relative paths were checked by the
/// host). Returns the `IoDone` status and bytes.
fn run_io(root: &Path, op: IoOp, args: &[String], data: &[u8]) -> (i32, Vec<u8>) {
    let denied = -(schemify_plugin::abi::Code::Denied as i32);
    match op {
        IoOp::FsRead => match platform::read(&root.join(&args[0])) {
            Ok(b) => (0, b),
            Err(e) => (-(schemify_plugin::abi::Code::Gone as i32), e.into_bytes()),
        },
        IoOp::FsWrite => match platform::write_atomic(&root.join(&args[0]), data) {
            Ok(()) => (0, Vec::new()),
            Err(e) => (denied, e.into_bytes()),
        },
        // ponytail: spawn (run plugins), fetch and clipboard are M3; answer "denied".
        IoOp::Spawn | IoOp::Fetch | IoOp::ClipboardGet | IoOp::ClipboardSet => {
            (denied, b"not supported yet".to_vec())
        }
    }
}
