//! Dialog state (`Dialog`) and the fields dialogs: properties, header, search, enter text.

use schemify_doc::{DocHandle, EditRequest, Op, Target as DocTarget};
use schemify_format::RecordKind;

use crate::commands::Cmd;
use crate::geometry::{Pt, Xform};
use crate::scene::ObjKey;
use crate::session::View;
use crate::{edit, search};

use super::Editor;
use super::chooser::Chooser;
use super::tool::{Form, Placement, Tool};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TextFor {
    Label,
    Port(bool),
    Text,
    SymPin,
    /// File → Save As (`true`: as symbol).
    SaveAs(bool),
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
    /// Purpose, title, typed text (follows the typing: `field.typed`).
    Text(TextFor, String, String),
    /// The canvas context menu (right click); `true` when something was selected.
    Ctx(bool),
    /// View → Visible layers.
    Layers,
    /// Properties → Edit file: the document's text (follows the typing: `field.typed`).
    File(String),
    /// Crash recovery of a document: (document, journal records offered).
    Recover(DocHandle, u32),
}

impl Editor {
    pub(super) fn show_dialog(&mut self, id: &str) {
        let sym = self.shell.sym(id);
        self.shell.ui.set_dialog(sym);
        self.shell.invalidate();
    }
    pub(super) fn close_dialog(&mut self) {
        self.dialog = Dialog::None;
        self.shell.ui.set_dialog(schemify_ui::Sym::NONE);
        self.shell.invalidate();
    }
    pub(super) fn open_text(&mut self, purpose: TextFor, title: &str) {
        self.tool = Tool::Idle;
        self.dialog = Dialog::Text(purpose, title.to_owned(), String::new());
        self.show_dialog("dialog.enter_text");
        self.focus_next = Some("dialog.enter_text.input".into());
    }
    pub(super) fn enter_text_ok(&mut self, arg: &str, at: Pt, reqs: &mut Vec<EditRequest>) {
        let Dialog::Text(purpose, _, typed) = self.dialog.clone() else {
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
        let text = text.trim().to_owned();
        if text.is_empty() {
            return; // xschem: empty text places nothing
        }
        let what = match purpose {
            TextFor::Label => Placement::Label(text),
            TextFor::Port(out) => Placement::Port(text, out),
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
    pub(super) fn open_props(&mut self, h: DocHandle) {
        let key = match self.selection() {
            [k] => Some(*k),
            [] => self
                .hit_at(self.pointer)
                .map(|o| self.scene.objs.key[o as usize]),
            _ => None,
        };
        let Some(key) = key else {
            self.session.say("select one object to edit its properties");
            return;
        };
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let Some(loc) = edit::loc_of(doc, key) else {
            return;
        };
        let rec = doc.record(loc);
        let field = |key: &str, label: &str, value: &str, editable: bool| Field {
            key: key.to_owned(),
            label: label.to_owned(),
            value: value.to_owned(),
            original: value.to_owned(),
            editable,
        };
        let mut fields = Vec::new();
        let title;
        match rec.kind {
            RecordKind::Inst => {
                let name = rec
                    .find_attr("name")
                    .and_then(|a| a.values.first())
                    .cloned()
                    .unwrap_or_default();
                let cellref = rec.fields[1].clone();
                title = format!("Instance {name} ({cellref})");
                fields.push(field("name", "name", &name, true));
                // Schema rows (D40: an empty value means "use the default").
                let iface = self
                    .session
                    .project
                    .resolve(h, &cellref)
                    .ok()
                    .and_then(|r| self.session.project.interface(r));
                if let Some(iface) = iface {
                    let is = iface.strings();
                    let p = iface.params();
                    for r in 0..p.fields.len() {
                        let pname = is.get(p.fields[r][0]).to_owned();
                        let default = is.get(p.fields[r][1]).to_owned();
                        let computed = iface.pool().find_attr(is, p.attrs[r], "computed").is_some();
                        let over = rec
                            .find_attr(&pname)
                            .and_then(|a| a.values.first())
                            .cloned()
                            .unwrap_or_default();
                        let label = if computed {
                            format!("{pname} (computed)")
                        } else {
                            format!("{pname} (default {default})")
                        };
                        fields.push(field(&pname, &label, &over, !computed));
                    }
                }
                // Overrides the schema does not know (kept, editable).
                for a in &rec.attrs {
                    let known = fields.iter().any(|f| f.key == a.key);
                    if !known && schemify_doc::Instances::is_override(&a.key) {
                        let v = a.values.join(" ");
                        fields.push(field(
                            &a.key,
                            &format!("{} (not in schema)", a.key),
                            &v,
                            true,
                        ));
                    }
                }
            }
            RecordKind::Label | RecordKind::Port | RecordKind::Text => {
                let what = match rec.kind {
                    RecordKind::Label => "net",
                    RecordKind::Port => "pin",
                    _ => "text",
                };
                title = format!("{} {}", rec.kind.word(), rec.fields[4]);
                fields.push(field(what, what, &rec.fields[4], true));
            }
            RecordKind::Wire => {
                title = "Wire".to_owned();
                let bus = rec
                    .find_attr("bus")
                    .and_then(|a| a.values.first())
                    .cloned()
                    .unwrap_or_default();
                fields.push(field("bus", "bus (true/false)", &bus, true));
            }
            RecordKind::Pin => {
                title = format!("Pin {}", rec.fields[0]);
                fields.push(field("name", "name", &rec.fields[0], true));
                fields.push(field("dir", "dir (in/out/inout)", &rec.fields[1], true));
            }
            _ => {
                self.session
                    .say(format!("{}: no editable properties yet", rec.kind.word()));
                return;
            }
        }
        self.tool = Tool::Idle;
        let first = fields.iter().position(|f| f.editable);
        self.dialog = Dialog::Props(Props {
            of: PropsFor::Object(key),
            title,
            fields,
        });
        self.show_dialog("dialog.props");
        self.focus_next = first.map(|i| format!("dialog.props.val.{i}"));
    }
    pub(super) fn props_field(&mut self, node: &str, arg: &str) {
        let Dialog::Props(p) = &mut self.dialog else {
            return;
        };
        let i = node
            .rsplit('.')
            .next()
            .and_then(|n| n.parse::<usize>().ok());
        if let Some(f) = i.and_then(|i| p.fields.get_mut(i)).filter(|f| f.editable) {
            f.value = arg.trim().to_owned();
        }
    }
    /// Commits the properties dialog as one transaction: `set_attr` for changed values,
    /// `remove_attr` for cleared overrides (D40: only overrides are stored).
    pub(super) fn props_ok(&mut self, h: DocHandle, reqs: &mut Vec<EditRequest>) {
        let Dialog::Props(p) = std::mem::take(&mut self.dialog) else {
            self.close_dialog();
            return;
        };
        self.close_dialog();
        let key = match p.of {
            PropsFor::Object(key) => key,
            PropsFor::Header => return self.header_ok(h, &p.fields, reqs),
            PropsFor::Search => return self.search_ok(h, &p.fields, reqs),
        };
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let Some(loc) = edit::loc_of(doc, key) else {
            return;
        };
        let rec = doc.record(loc);
        let tkey = match rec.kind.key() {
            schemify_format::KeyRule::Position => loc.row.to_string(),
            _ => rec.fields[0].clone(),
        };
        let target = schemify_doc::Target::new(rec.kind, &tkey);
        let mut ops = Vec::new();
        for f in p
            .fields
            .iter()
            .filter(|f| f.editable && f.value != f.original)
        {
            let op = match (rec.kind, f.key.as_str()) {
                (RecordKind::Label | RecordKind::Port | RecordKind::Text, _) => Op::SetField {
                    target: target.clone(),
                    index: 4,
                    value: f.value.clone(),
                },
                (RecordKind::Pin, "name") => Op::SetField {
                    target: target.clone(),
                    index: 0,
                    value: f.value.clone(),
                },
                (RecordKind::Pin, _) => Op::SetField {
                    target: target.clone(),
                    index: 1,
                    value: f.value.clone(),
                },
                (_, _) if f.value.is_empty() => Op::RemoveAttr {
                    target: target.clone(),
                    key: f.key.clone(),
                    first: None,
                },
                _ => Op::SetAttr {
                    target: target.clone(),
                    key: f.key.clone(),
                    values: vec![f.value.clone()],
                },
            };
            ops.push(op);
        }
        self.commit(reqs, h, "edit properties", ops);
    }
    /// `field.typed` (a dialog field's `input` command, sent as the field's text changes): the
    /// text reaches the dialog state as typed, so OK, a click elsewhere or the chooser's
    /// filtering see it without Enter. Returns whether anything visible changed (the chooser
    /// rows). The docs page's editor is followed by `sync_page`.
    pub(super) fn field_typed(&mut self, node: &str, text: &str) -> bool {
        match &mut self.dialog {
            Dialog::Props(p) => {
                let i = node
                    .strip_prefix("dialog.props.val.")
                    .and_then(|n| n.parse::<usize>().ok());
                if let Some(f) = i.and_then(|i| p.fields.get_mut(i)).filter(|f| f.editable) {
                    f.value = text.trim().to_owned();
                }
                false
            }
            Dialog::Choose(c) if node == "dialog.choose.filter" && c.filter != text => {
                c.filter = text.to_owned();
                self.refill_chooser();
                true
            }
            Dialog::Text(_, _, typed) if node == "dialog.enter_text.input" => {
                text.clone_into(typed);
                false
            }
            Dialog::File(typed) if node == "dialog.edit_file.text" => {
                text.clone_into(typed);
                false
            }
            _ => false,
        }
    }
    /// OK of an enter-text dialog opened by a parity command.
    pub(super) fn text_ok(&mut self, purpose: TextFor, text: &str, reqs: &mut Vec<EditRequest>) {
        match purpose {
            TextFor::SaveAs(symbol) => {
                if let Some((h, _)) = self.active() {
                    self.save_as(h, text, symbol);
                }
            }
            TextFor::Setting(cmd) => self.setting(cmd, text),
            TextFor::LayerColor => {
                self.run_cmd(Cmd::LayerColor, text, "", reqs);
            }
            TextFor::Label | TextFor::Port(_) | TextFor::Text | TextFor::SymPin => {
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
    }
    /// OK of the search dialog: select the matches, or replace them all (one undo).
    pub(super) fn search_ok(
        &mut self,
        h: DocHandle,
        fields: &[Field],
        reqs: &mut Vec<EditRequest>,
    ) {
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
        self.commit(reqs, h, "edit header", ops);
    }
    /// Opens the canvas context menu at screen point `at` (xschem §1.15): the no-selection or
    /// the selection variant.
    pub(super) fn context_menu(&mut self, at: (f32, f32)) {
        let selected = !self.selection().is_empty();
        self.dialog = Dialog::Ctx(selected);
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
            self.mods & schemify_ui::Mods::SHIFT != 0,
            self.mods & schemify_ui::Mods::CTRL != 0,
            self.mods & schemify_ui::Mods::ALT != 0,
        );
        match std::mem::take(&mut self.tool) {
            Tool::Shape {
                form: Form::Poly,
                pts,
                ..
            } if pts.len() >= 2 => {
                self.finish_shape(reqs, h, view, Form::Poly, &pts, false);
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
        self.tool = Tool::Idle;
        self.dialog = Dialog::Recover(d, n);
        self.show_dialog("dialog.recover");
    }
}
