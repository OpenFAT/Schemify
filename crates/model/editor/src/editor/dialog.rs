//! Dialog state (`Dialog`) and the fields dialogs: properties, header, search, enter text.
//!
//! Contents:
//! - `pub(super) enum TextFor`
//! - `pub(super) struct Field`
//! - `pub(super) enum PropsFor`
//! - `pub(super) struct Props`
//! - `pub(super) enum Dialog`
//! - `Editor::show_dialog`
//! - `Editor::close_dialog`
//! - `Editor::open_text`
//! - `Editor::enter_text_ok`
//! - `Editor::open_search`
//! - `Editor::search_ok`
//! - `Editor::open_header`
//! - `Editor::header_ok`
//! - `Editor::context_menu`
//! - `Editor::right_click`
//! - `Editor::prompt_recovery`

use schemify_doc::{DocHandle, EditRequest, Op, Target as DocTarget};
use schemify_ui::{Mods, Sym};

use crate::commands::Cmd;
use crate::geometry::{Pt, Xform};
use crate::scene::ObjKey;
use crate::search;
use crate::session::View;

use super::Editor;
use super::chooser::Chooser;
use super::finish::Outline;
use super::tool::{Form, Placement, Tool};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TextFor {
    Label,
    Port {
        /// An output port (else an input).
        output: bool,
    },
    Text,
    SymPin,
    /// File → Save As.
    SaveAs {
        /// Save as a symbol (else as a schematic).
        symbol: bool,
    },
    /// A one-number setting: snap, grid, line width, crosshair, symbol width, dim.
    Setting(Cmd),
    /// Layer color `rrggbb` of the current layer.
    LayerColor,
}

#[derive(Clone, Debug)]
pub(super) struct Field {
    pub(super) key: String,
    pub(super) label: String,
    pub(super) value: String,
    pub(super) original: String,
    pub(super) editable: bool,
}

/// What a fields dialog (the `dialog.props` node) edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PropsFor {
    /// One object's properties.
    Object(ObjKey),
    /// The header attributes (Properties → Edit Header).
    Header,
    /// Tools → Search: field, value, replacement, exact.
    Search,
}

#[derive(Clone, Debug)]
pub(super) struct Props {
    pub(super) of: PropsFor,
    pub(super) title: String,
    pub(super) fields: Vec<Field>,
}

#[derive(Clone, Debug, Default)]
pub(super) enum Dialog {
    #[default]
    None,
    Choose(Chooser),
    Props(Props),
    /// An enter-text dialog.
    Text {
        /// What the text is for.
        purpose: TextFor,
        /// The dialog's title.
        title: String,
        /// The typed text (follows the typing: `field.typed`).
        typed: String,
    },
    /// The canvas context menu (right click).
    Ctx {
        /// Something was selected when it opened.
        selected: bool,
    },
    /// View → Visible layers.
    Layers,
    /// Properties → Edit file: the document's text (follows the typing: `field.typed`).
    File(String),
    /// Crash recovery of a document.
    Recover {
        /// The document.
        doc: DocHandle,
        /// Journal records offered.
        records: u32,
    },
}

impl Editor {
    pub(super) fn show_dialog(&mut self, id: &str) {
        assert!(id.starts_with("dialog."), "a dialog node id: {id}");
        let sym = self.shell.sym(id);
        self.shell.ui.set_dialog(sym);
        self.shell.invalidate();
    }
    pub(super) fn close_dialog(&mut self) {
        self.dialog = Dialog::None;
        self.shell.ui.set_dialog(Sym::NONE);
        self.shell.invalidate();
        assert!(self.shell.ui.dialog().is_none(), "no dialog stays open");
    }
    pub(super) fn open_text(&mut self, purpose: TextFor, title: &str) {
        assert!(!title.is_empty(), "an enter-text dialog has a title");
        self.tool = Tool::Idle;
        self.dialog = Dialog::Text {
            purpose,
            title: title.to_owned(),
            typed: String::new(),
        };
        self.show_dialog("dialog.enter_text");
        self.focus_next = Some("dialog.enter_text.input".into());
        assert!(matches!(self.tool, Tool::Idle), "a dialog ends the gesture");
    }
    pub(super) fn enter_text_ok(&mut self, arg: &str, at: Pt, reqs: &mut Vec<EditRequest>) {
        let Dialog::Text { purpose, typed, .. } = self.dialog.clone() else {
            self.close_dialog();
            return;
        };
        // Enter in the field submits its text as `arg`; the OK button submits none, and the
        // typing reached the dialog as `field.typed` before the click, so that copy is used.
        let text = if arg.is_empty() {
            typed
        } else {
            arg.to_owned()
        };
        self.close_dialog();
        assert!(
            matches!(self.dialog, Dialog::None),
            "the text dialog closed"
        );
        let text = text.trim().to_owned();
        if text.is_empty() {
            return; // xschem: empty text places nothing
        }
        let what = match purpose {
            TextFor::Label => Placement::Label(text),
            TextFor::Port { output } => Placement::Port { name: text, output },
            TextFor::Text => Placement::Text(text),
            TextFor::SymPin => Placement::SymPin(text),
            other => {
                self.text_ok(other, &text, reqs);
                return;
            }
        };
        self.tool = Tool::Place {
            what,
            at,
            lin: Xform::IDENTITY,
        };
    }
}

impl Editor {
    /// OK of an enter-text dialog opened by a parity command.
    fn text_ok(&mut self, purpose: TextFor, text: &str, reqs: &mut Vec<EditRequest>) {
        match purpose {
            TextFor::SaveAs { symbol } => {
                if let Some((h, _)) = self.active() {
                    self.save_as(h, text, symbol);
                }
            }
            TextFor::Setting(cmd) => self.setting(cmd, text),
            TextFor::LayerColor => {
                self.run_cmd(Cmd::LayerColor, text, "", reqs);
            }
            TextFor::Label | TextFor::Port { .. } | TextFor::Text | TextFor::SymPin => {
                unreachable!("placements are handled by enter_text_ok")
            }
        }
    }
    pub(super) fn open_search(&mut self) {
        self.tool = Tool::Idle;
        let f = |key: &str, label: &str, value: &str| Field {
            key: key.to_owned(),
            label: label.to_owned(),
            value: value.to_owned(),
            original: value.to_owned(),
            editable: true,
        };
        self.dialog = Dialog::Props(Props {
            of: PropsFor::Search,
            title: "Search / replace".into(),
            fields: vec![
                f(
                    "field",
                    "field (name, cell, net, text, a param, * = any)",
                    "*",
                ),
                f("value", "value", ""),
                f("replace", "replace with (empty: select matches)", ""),
                f("exact", "exact match (true/false)", "false"),
            ],
        });
        self.show_dialog("dialog.props");
        self.focus_next = Some("dialog.props.val.1".into());
        debug_assert!(
            matches!(&self.dialog, Dialog::Props(p) if p.fields[1].key == "value"),
            "the focused field is the search value"
        );
    }
    /// OK of the search dialog: select the matches, or replace them all (one undo).
    pub(super) fn search_ok(
        &mut self,
        h: DocHandle,
        fields: &[Field],
        reqs: &mut Vec<EditRequest>,
    ) {
        assert!(
            fields.iter().any(|f| f.key == "value"),
            "the search dialog's fields"
        );
        let get = |k: &str| {
            fields
                .iter()
                .find(|f| f.key == k)
                .map_or("", |f| f.value.as_str())
        };
        let q = search::Query {
            field: match get("field") {
                "" => "*".to_owned(),
                f => f.to_owned(),
            },
            value: get("value").to_owned(),
            exact: get("exact") == "true",
        };
        if q.value.is_empty() {
            self.session.say("search: no value");
            return;
        }
        let with = get("replace").to_owned();
        if with.is_empty() {
            let keys = self.with_doc(h, |d, _| search::find(d, &q));
            self.session.say(if keys.is_empty() {
                "not found".into()
            } else {
                format!("{} object(s) found", keys.len())
            });
            self.set_selection(keys);
            if let Some(b) = self.selection_bbox() {
                self.fit(b);
            }
        } else {
            let ops = self.with_doc(h, |d, _| search::replace(d, &q, &with));
            self.session.say(format!("{} replacement(s)", ops.len()));
            self.commit(reqs, h, "replace", ops);
        }
    }
    pub(super) fn open_header(&mut self, h: DocHandle) {
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let f = |key: &str| {
            let v = doc.header_value(key).unwrap_or("").to_owned();
            Field {
                key: key.to_owned(),
                label: key.to_owned(),
                value: v.clone(),
                original: v,
                editable: true,
            }
        };
        let fields = vec![f("doc"), f("prefix"), f("category")];
        assert!(
            fields.iter().all(|f| f.value == f.original),
            "a fresh form has nothing changed"
        );
        self.tool = Tool::Idle;
        self.dialog = Dialog::Props(Props {
            of: PropsFor::Header,
            title: "Header".into(),
            fields,
        });
        self.show_dialog("dialog.props");
        self.focus_next = Some("dialog.props.val.0".into());
    }
    /// OK of the header dialog: one `set_attr`/`remove_attr` per changed field.
    pub(super) fn header_ok(
        &mut self,
        h: DocHandle,
        fields: &[Field],
        reqs: &mut Vec<EditRequest>,
    ) {
        assert!(
            fields.iter().all(|f| f.editable),
            "every header field is editable"
        );
        let ops: Vec<Op> = fields
            .iter()
            .filter(|f| f.value != f.original)
            .map(|f| {
                if f.value.is_empty() {
                    Op::RemoveAttr {
                        target: DocTarget::header(),
                        key: f.key.clone(),
                        first: None,
                    }
                } else {
                    Op::SetAttr {
                        target: DocTarget::header(),
                        key: f.key.clone(),
                        values: vec![f.value.clone()],
                    }
                }
            })
            .collect();
        assert!(ops.len() <= fields.len(), "one op per changed field");
        self.commit(reqs, h, "edit header", ops);
    }
    /// Opens the canvas context menu at screen point `at` (xschem §1.15): the no-selection or
    /// the selection variant.
    pub(super) fn context_menu(&mut self, at: (f32, f32)) {
        assert!(at.0.is_finite() && at.1.is_finite(), "a screen point");
        let selected = !self.selection().is_empty();
        self.dialog = Dialog::Ctx { selected };
        let sym = self.shell.sym("dialog.ctx");
        self.shell.ui.set_dialog_at(sym, Some(at));
        self.shell.invalidate();
    }
    /// A right click without a drag: modified clicks select connected wires or cut the wire
    /// (xschem B3 with Shift / Ctrl / Alt / Alt+Shift); a tool in progress ends (a polygon
    /// keeps its points, open); otherwise the context menu.
    pub(super) fn right_click(
        &mut self,
        reqs: &mut Vec<EditRequest>,
        h: DocHandle,
        view: View,
        at: (f32, f32),
    ) {
        let (shift, ctrl, alt) = (
            self.mods & Mods::SHIFT != 0,
            self.mods & Mods::CTRL != 0,
            self.mods & Mods::ALT != 0,
        );
        match std::mem::take(&mut self.tool) {
            Tool::Shape {
                form: Form::Poly,
                pts,
                ..
            } if pts.len() >= 2 => {
                self.finish_shape(reqs, h, view, Form::Poly, &pts, Outline::Open);
                return;
            }
            Tool::Idle | Tool::Press { .. } => {}
            _ => return, // abort the tool in progress
        }
        let cmd = match (shift, ctrl, alt) {
            (true, false, false) => Some(Cmd::SelectConnected),
            (false, true, false) => Some(Cmd::SelectConnectedStop),
            (false, false, true) => Some(Cmd::BreakAtMouseAlign),
            (true, false, true) => Some(Cmd::BreakAtMouse),
            _ => None,
        };
        match cmd {
            Some(c) => {
                self.run_cmd(c, "", "", reqs);
            }
            None => self.context_menu(at),
        }
    }
    /// Shows the crash-recovery prompt for the next document the journal offers edits for,
    /// when no other dialog is open.
    pub(super) fn prompt_recovery(&mut self) {
        if !matches!(self.dialog, Dialog::None) || !self.shell.ui.dialog().is_none() {
            return;
        }
        let Some((d, n)) = self.session.pending_recovery() else {
            return;
        };
        assert!(n > 0, "a recovery offers journal records");
        self.tool = Tool::Idle;
        self.dialog = Dialog::Recover { doc: d, records: n };
        self.show_dialog("dialog.recover");
    }
}
