//! The UI step: plugin sets and images in, `ui::compose` + layout as a projection of the tables.
//!
//! Contents:
//! - `pub(super) struct UiInputs`
//! - `Editor::ui_step`
//! - `Editor::ui_inputs`
//! - `Editor::compose_ui`
//! - `Editor::state_contribution`

use schemify_doc::Document;
use schemify_ui::Prop;

use crate::commands::Target;
use crate::docs::quote as q;
use crate::session::View;
use crate::{cell, docs};

use super::chooser::ChooseFor;
use super::dialog::Dialog;
use super::tool::{Form, Tool};
use super::{Editor, dispatch};

/// What [`Editor::ui_inputs`] made current.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct UiInputs {
    /// An image loaded: the page needs a new layout.
    pub(super) loaded: bool,
    /// The plugin composition changed: the palette needs a refresh.
    pub(super) restyle: bool,
}

impl Editor {
    /// The UI step of the frame spine: [`Editor::ui_inputs`], the pure [`Editor::compose_ui`],
    /// then the palette when the plugin composition changed (it reads the composed styles).
    /// Without `always`, composes only when an input loaded, the plugin composition changed,
    /// a relayout is asked for or the state changed outside a frame (`ui_stale`): the top of
    /// the frame, where the last frame's composition is otherwise current. Returns whether the
    /// layout changed.
    pub(super) fn ui_step(&mut self, relayout: bool, always: bool) -> bool {
        let UiInputs { loaded, restyle } = self.ui_inputs();
        if !(always || relayout || loaded || restyle || self.ui_stale) {
            return false;
        }
        let changed = self.compose_ui(relayout || loaded);
        if restyle {
            self.refresh_palette();
        }
        changed
    }
    /// Makes current what `ui::compose` reads: the plugin ui and keymap sets (when their
    /// composition changed; the command rows follow), the docs page following the active
    /// document, and the page's prose images (file I/O, once per path; a failure goes to the
    /// status line here, where it is decided).
    pub(super) fn ui_inputs(&mut self) -> UiInputs {
        let restyle = match self.host.ui_sets() {
            Some(sets) => {
                self.shell.set_plugins(&sets);
                self.commands.set_plugin_rows(self.host.plugins.composed());
                true
            }
            None => false,
        };
        let mut loaded = false;
        if let Some((h, _)) = self.active() {
            self.page.follow(h);
            assert_eq!(
                self.page.doc,
                Some(h),
                "the page follows the active document"
            );
            // ponytail: an image file changed on disk is not reloaded; watch it with the documents.
            if self.docs_shown()
                && let Some(doc) = self.session.project.doc(h)
            {
                let failed = &mut self.page.images_failed;
                let err;
                (loaded, err) =
                    docs::load_images(doc, &self.session.root, &mut self.shell.text, failed);
                if let Some(e) = err {
                    self.session.say(e);
                }
            }
        }
        assert!(self.page.images_failed.len() <= docs::IMAGES_FAILED_MAX);
        UiInputs { loaded, restyle }
    }
    /// `ui::compose` + `ui::layout` as a projection: the state contribution of the tables
    /// (tabs, status line, dialogs, docs page) replaces the shell's, then the shell recomposes
    /// and lays out what changed. It writes only the shell's composed tables and layout and the
    /// plot slots it found (`graphs`, which `paint` reads); no I/O and no model table, so it may
    /// run more than once a frame. Returns whether the layout changed.
    pub(super) fn compose_ui(&mut self, relayout: bool) -> bool {
        let (status, failed) = (self.session.status().len(), self.page.images_failed.len());
        let (text, graphs) = self.state_contribution();
        self.graphs = graphs;
        self.shell.set_state(&text);
        let changed = self.shell.refresh(self.viewport, relayout);
        self.ui_stale = false;
        assert_eq!(
            self.session.status().len(),
            status,
            "compose writes no status"
        );
        assert_eq!(
            self.page.images_failed.len(),
            failed,
            "compose reads no file"
        );
        changed
    }
    /// The state contribution: session state projected into plugin-declared nodes, and the
    /// docs page's filled plot slots `(graph node id, expression)` for the painter.
    pub(super) fn state_contribution(&self) -> (String, Vec<(String, String)>) {
        use std::fmt::Write;
        let mut t = String::with_capacity(1024);
        let mut graphs = Vec::new();
        let s = &self.session;
        for (i, tab) in s.tabs().iter().enumerate() {
            let l = tab.stack.last().expect("tabs are never empty");
            let mut name = match &tab.text {
                Some(v) => v.title.clone(),
                None => s.name(l.doc),
            };
            if tab.text.is_some() {
                // A text tab is read-only: no symbol or dirty marks.
            } else if l.view == View::Symbol && tab.stack.len() > 1 {
                name.push_str(" (symbol)");
            }
            if tab.text.is_none() && s.project.is_dirty(l.doc) {
                name.push('*');
            }
            let checked = u8::from(i == s.active());
            if i == 0 {
                let _ = writeln!(t, "set tab.0 label {}", q(&name));
                let _ = writeln!(t, "set tab.0 checked {checked}");
                let _ = writeln!(t, "set tab.0 order -1");
            } else {
                let _ = writeln!(t, "node tab.{i} tab");
                let _ = writeln!(t, "  parent tabs");
                let _ = writeln!(t, "  label {}", q(&name));
                let _ = writeln!(t, "  command tab.select");
                let _ = writeln!(t, "  checked {checked}");
                let _ = writeln!(t, "  order -1");
            }
        }
        let units = crate::geometry::coord;
        let _ = writeln!(t, "set status.snap value {}", q(&units(self.opts.snap)));
        let _ = writeln!(t, "set status.grid value {}", q(&units(self.opts.grid)));
        let banner = match &self.tool {
            Tool::Wire { .. } => "DRAW WIRE! ",
            Tool::Shape {
                form: Form::Rect, ..
            } => "DRAW RECTANGLE! ",
            Tool::Shape {
                form: Form::Line, ..
            } => "DRAW LINE! ",
            Tool::Shape {
                form: Form::Poly, ..
            } => "DRAW POLYGON! ",
            Tool::Shape {
                form: Form::Arc, ..
            } => "DRAW ARC! ",
            Tool::Shape {
                form: Form::Circle, ..
            } => "DRAW CIRCLE! ",
            Tool::UnselectArmed => "UNSELECT AREA! ",
            Tool::Move { copy: true, .. } => "COPY! ",
            Tool::Move { .. } => "MOVE! ",
            Tool::Place { .. } => "PLACE! ",
            Tool::ZoomArmed | Tool::Zoom { .. } => "ZOOM BOX! ",
            _ => "",
        };
        let (mx, my) = self.pointer_world_snapped();
        let msg = format!(
            "{banner}mouse = {} {}  selected: {}  path: {}  {}",
            units(mx),
            units(my),
            self.selection().len(),
            s.hier_path(),
            s.status()
        );
        let _ = writeln!(t, "set status.msg label {}", q(&msg));
        let flags = [
            ("options.draw_grid", self.opts.show_grid),
            ("options.enable_orthogonal_wiring", self.opts.orthogonal),
            ("edit.unconstrained_move", self.opts.constrain == 0),
            ("edit.constrained_horizontal_move", self.opts.constrain == 1),
            ("edit.constrained_vertical_move", self.opts.constrain == 2),
        ];
        for (id, on) in flags {
            let _ = writeln!(t, "set {id} checked {}", u8::from(on));
        }
        // The docs page (D19): blocks under `docs.page`, or the pane removed while hidden.
        match self.active().and_then(|(h, _)| self.session.project.doc(h)) {
            Some(doc) if self.docs_shown() => {
                let h = self.active().expect("doc implies a level").0;
                let cell = self
                    .cell
                    .as_ref()
                    .filter(|_| self.cell_key.is_some_and(|k| k.0 == h));
                #[cfg(not(target_arch = "wasm32"))]
                let plots: Vec<(String, Option<String>)> = self
                    .sim
                    .plots
                    .iter()
                    .map(|p| (p.expr.clone(), p.waves.as_ref().err().cloned()))
                    .collect();
                #[cfg(target_arch = "wasm32")]
                let plots: Vec<(String, Option<String>)> = Vec::new();
                let source = self.docs_source();
                let extras = docs::Extras {
                    plots: &plots,
                    staged: self.staged.as_ref().map(|(l, d)| (l.as_str(), d)),
                    source,
                };
                docs::contribute(&mut t, doc, cell, &self.page, extras, &mut graphs);
                // The Source / Rendered toggle names the mode shown; pressed while source.
                let (label, style) = if source {
                    ("Source", "docs-toggle-on")
                } else {
                    ("Rendered", "docs-toggle")
                };
                let _ = writeln!(t, "set docs.source label {label}");
                let _ = writeln!(t, "set docs.source style {style}");
                let _ = writeln!(t, "set docs.source checked {}", u8::from(source));
            }
            _ => t.push_str("remove docs.pane\n"),
        }
        // The view switcher over the canvas: the shown side is `viewsw-on`, a side the file
        // kind lacks is disabled and `viewsw-off`; a text tab has no canvas and no switcher.
        match self.active().filter(|_| s.text_view().is_none()) {
            Some((h, view)) => {
                let kind = s.project.doc(h).map(Document::kind);
                for (id, v) in [
                    ("switch.schematic", View::Schematic),
                    ("switch.symbol", View::Symbol),
                ] {
                    let has = kind.is_some_and(|k| super::has_view(k, v));
                    let style = match (has, view == v) {
                        (false, _) => "viewsw-off",
                        (true, true) => "viewsw-on",
                        (true, false) => "viewsw",
                    };
                    let _ = writeln!(t, "set {id} style {style}");
                    let _ = writeln!(t, "set {id} disabled {}", u8::from(!has));
                }
                let tab = s.tabs().get(s.active());
                for (id, on) in [
                    ("switch.docs", tab.is_some_and(|tab| tab.docs)),
                    ("switch.cell", tab.is_some_and(|tab| tab.cell)),
                ] {
                    let style = if on { "viewsw-on" } else { "viewsw" };
                    let _ = writeln!(t, "set {id} style {style}");
                }
            }
            None => t.push_str("remove view.switch\n"),
        }
        // Commands that act on other file kinds (`Cmd::file_kinds`): every node bound to one is
        // disabled, so menus, toolbar buttons and panels grey it out.
        // The block reads only the file kind, the composed nodes' ids and commands, and how
        // the registry resolves them: kept by a key of those (no allocation while it stands).
        if let Some(kind) = self
            .active()
            .and_then(|(h, _)| s.project.doc(h))
            .map(Document::kind)
        {
            let nodes = &self.shell.tables.nodes;
            let commands = nodes.prop(Prop::Command);
            let key = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (kind, self.host.plugins.composed_epoch()).hash(&mut h);
                commands.hash(&mut h);
                nodes.id().hash(&mut h);
                h.finish()
            };
            let mut block = self.kind_block.borrow_mut();
            if block.0 != Some(key) {
                block.1.clear();
                for (i, &c) in commands.iter().enumerate() {
                    if c.is_none() {
                        continue;
                    }
                    let Target::Core(cmd) = self.commands.resolve(self.shell.strings.get(c)) else {
                        continue;
                    };
                    if !cmd.accepts(kind) {
                        let id = self.shell.strings.get(nodes.id()[i]);
                        let b = &mut block.1;
                        let _ = writeln!(b, "set {id} disabled 1");
                        let _ = writeln!(
                            b,
                            "set {id} tooltip {}",
                            q(&dispatch::kind_refusal(cmd, kind))
                        );
                    }
                }
                block.0 = Some(key);
            }
            t.push_str(&block.1);
        }
        // The cell settings panel: the active cell's header, pins, params and impls.
        let shown = s.text_view().is_none() && s.tabs().get(s.active()).is_some_and(|tab| tab.cell);
        match self
            .active()
            .filter(|_| shown)
            .and_then(|(h, _)| s.project.doc(h))
        {
            Some(doc) => {
                let sync = self.port_sync.as_ref().filter(|(d, e, _)| {
                    self.active().is_some_and(|(h, _)| h == *d) && s.epoch(*d) == *e
                });
                cell::contribute(&mut t, doc, sync.map(|p| &p.2));
            }
            None => t.push_str("remove cell.pane\n"),
        }
        match &self.dialog {
            Dialog::None => {}
            Dialog::Text(_, title, typed) => {
                let _ = writeln!(t, "set dialog.enter_text.title label {}", q(title));
                let _ = writeln!(t, "set dialog.enter_text.input value {}", q(typed));
                let _ = writeln!(t, "set dialog.enter_text.input input field.typed");
            }
            Dialog::Ctx(_) | Dialog::Layers | Dialog::File(_) | Dialog::Recover(..) => {}
            Dialog::Choose(c) => {
                let title = match c.purpose {
                    ChooseFor::Insert => "Insert symbol (type to filter, click to place)".into(),
                    ChooseFor::Open => "Open cell".into(),
                    ChooseFor::Waves if !c.note.is_empty() => format!(
                        "Open waveform file for {} (.raw; or type a path + Enter)",
                        c.note
                    ),
                    ChooseFor::Waves => {
                        "Open waveform file (.raw; or type a path + Enter)".to_owned()
                    }
                    ChooseFor::Merge => "Merge: insert a cell's schematic at the pointer".into(),
                    ChooseFor::Delete => "Delete a cell file (pick twice to confirm)".into(),
                    ChooseFor::Compare => "Compare the active schematic with".into(),
                };
                let _ = writeln!(t, "set dialog.choose.title label {}", q(&title));
                let _ = writeln!(t, "set dialog.choose.filter value {}", q(&c.filter));
                let _ = writeln!(t, "set dialog.choose.filter input field.typed");
                for (i, (label, _)) in c.rows.iter().enumerate() {
                    let _ = writeln!(t, "node dialog.choose.row.{i} item");
                    let _ = writeln!(t, "  parent dialog.choose.list");
                    let _ = writeln!(t, "  label {}", q(label));
                    let _ = writeln!(t, "  command choose.pick");
                    let _ = writeln!(t, "  style list-row");
                }
            }
            Dialog::Props(p) => {
                let _ = writeln!(t, "set dialog.props.title label {}", q(&p.title));
                for (i, f) in p.fields.iter().enumerate() {
                    let _ = writeln!(t, "node dialog.props.row.{i} panel");
                    let _ = writeln!(t, "  parent dialog.props.fields");
                    let _ = writeln!(t, "  style dialog-row");
                    let _ = writeln!(t, "node dialog.props.key.{i} label");
                    let _ = writeln!(t, "  parent dialog.props.row.{i}");
                    let _ = writeln!(t, "  label {}", q(&f.label));
                    let _ = writeln!(t, "  style dialog-key");
                    let kind = if f.editable { "text_input" } else { "label" };
                    let _ = writeln!(t, "node dialog.props.val.{i} {kind}");
                    let _ = writeln!(t, "  parent dialog.props.row.{i}");
                    if f.editable {
                        let _ = writeln!(t, "  value {}", q(&f.value));
                        let _ = writeln!(t, "  command props.field");
                        let _ = writeln!(t, "  input field.typed");
                        let _ = writeln!(t, "  style dialog-input");
                    } else {
                        let _ = writeln!(t, "  label {}", q("(computed)"));
                    }
                }
            }
        }
        self.contribute_integration(&mut t);
        self.contribute_parity(&mut t);
        (t, graphs)
    }
}
