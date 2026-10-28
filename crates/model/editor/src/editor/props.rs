//! The properties dialog of one object: its fields from the record (editable or not), each
//! typed or picked value, and OK as one transaction of the changed attributes.

use super::Editor;
use super::dialog::{Dialog, Field, Props, PropsFor};
use super::tool::Tool;
use crate::edit;
use schemify_doc::{DocHandle, EditRequest, Op};
use schemify_format::RecordKind;

impl Editor {
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
}
