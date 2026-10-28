//! Edit and Tools commands: undo, the clipboard, transforms, drawing tools, wire tools.
//!
//! Contents:
//! - `Editor::run_edit`

use schemify_doc::{Edit, EditRequest, Op, TableId};
use schemify_format::RecordKind;

use crate::commands::Cmd;
use crate::edit::Ids;
use crate::geometry::{Pt, Xform};
use crate::scene::ObjKey;
use crate::session::View;
use crate::{edit, platform, wiring};

use super::dialog::TextFor;
use super::finish::Outline;
use super::tool::{Form, Placement, Tool};
use super::{Editor, apply};

impl Editor {
    /// Edit and Tools commands: undo, the clipboard, transforms, drawing tools, wire tools. Returns whether anything visible changed.
    pub(super) fn run_edit(
        &mut self,
        cmd: Cmd,
        _arg: &str,
        _node: &str,
        reqs: &mut Vec<EditRequest>,
    ) -> bool {
        let Some((h, view)) = self.active() else {
            return false;
        };
        let ptr = self.pointer_world_snapped();
        match cmd {
            Cmd::Undo | Cmd::Redo => {
                self.tool = Tool::Idle;
                let edit = if cmd == Cmd::Undo {
                    Edit::Undo
                } else {
                    Edit::Redo
                };
                apply::push_req(reqs, h, edit);
            }
            Cmd::Delete => {
                let ops = self.with_doc(h, edit::delete);
                self.commit(reqs, h, "delete", ops);
                self.set_selection(Vec::new());
                assert!(self.selection().is_empty());
            }
            Cmd::SelectAll => {
                let all: Vec<ObjKey> = self.scene.objs.key.clone();
                self.set_selection(all);
                assert_eq!(
                    self.selection().len(),
                    self.scene.objs.len(),
                    "one key per object"
                );
            }
            Cmd::Copy | Cmd::Cut => {
                let base = self.selection_pivot();
                let recs = self.with_doc(h, |doc, sel| {
                    edit::selected_records(doc, sel)
                        .into_iter()
                        .map(|(_, _, r)| r)
                        .collect::<Vec<_>>()
                });
                let back = Xform::translate(-base.0, -base.1);
                let copied = recs.len();
                self.clipboard = recs
                    .into_iter()
                    .filter_map(|mut r| crate::geometry::transform_record(&mut r, back).map(|()| r))
                    .collect();
                assert!(
                    self.clipboard.len() <= copied,
                    "a copy is dropped, never added"
                );
                self.session
                    .say(format!("{} object(s) copied", self.clipboard.len()));
                if self.system_clipboard
                    && !self.clipboard.is_empty()
                    && let Err(e) = platform::clipboard_set(&crate::clip::encode(&self.clipboard))
                {
                    self.session
                        .say(format!("system clipboard: {e} (copied here only)"));
                }
                if cmd == Cmd::Cut {
                    let ops = self.with_doc(h, edit::delete);
                    self.commit(reqs, h, "cut", ops);
                    self.set_selection(Vec::new());
                }
            }
            Cmd::Paste => {
                // The system clipboard wins when it holds a Schemify fragment (another
                // window, another process, a text editor); else the editor's own.
                if self.system_clipboard {
                    match platform::clipboard_get() {
                        Ok(text) => {
                            if let Ok(recs) = crate::clip::decode(&text) {
                                self.clipboard = recs;
                            }
                        }
                        // Web: the read is in flight; this paste runs again when it lands.
                        Err(e) if e == platform::CLIPBOARD_PENDING => return false,
                        Err(_) => {}
                    }
                }
                if !self.clipboard.is_empty() {
                    self.tool = Tool::Place {
                        what: Placement::Paste(self.clipboard.clone()),
                        at: ptr,
                        lin: Xform::IDENTITY,
                    };
                }
            }
            Cmd::Duplicate | Cmd::Move | Cmd::MoveKiss | Cmd::MoveStretch => {
                // xschem: `m` moves, Ctrl+M stretches (swapped by `enable_stretch`), Shift+M
                // stretches and adds "kissing" wires.
                let stretch = match cmd {
                    Cmd::Move => u8::from(self.opts.stretch),
                    Cmd::MoveStretch => u8::from(!self.opts.stretch),
                    Cmd::MoveKiss => 2,
                    _ => 0,
                };
                assert!(stretch <= 2, "plain, stretch or kissing move");
                let partial = cmd != Cmd::Duplicate && !self.partial_ends().is_empty();
                if !self.selection().is_empty() || partial {
                    self.tool = Tool::Move {
                        from: ptr,
                        to: ptr,
                        lin: Xform::IDENTITY,
                        copy: cmd == Cmd::Duplicate,
                        drag: false,
                        stretch,
                    };
                }
            }
            Cmd::Rotate | Cmd::Flip | Cmd::VFlip => {
                let about: fn(Pt) -> Xform = match cmd {
                    Cmd::Rotate => Xform::rotate,
                    Cmd::Flip => Xform::flip_x,
                    _ => Xform::flip_y,
                };
                match &mut self.tool {
                    Tool::Move { from, lin, .. } => *lin = lin.then(about(*from)),
                    Tool::Place { at, lin, .. } => *lin = lin.then(about(*at)),
                    _ => {
                        let pivot = self.selection_pivot();
                        let ops =
                            self.with_doc(h, |doc, sel| edit::transform(doc, sel, about(pivot)));
                        self.commit(reqs, h, cmd.name(), ops);
                    }
                }
            }
            Cmd::RotateInPlace | Cmd::FlipInPlace | Cmd::VFlipInPlace => {
                let about: fn(Pt) -> Xform = match cmd {
                    Cmd::RotateInPlace => Xform::rotate,
                    Cmd::FlipInPlace => Xform::flip_x,
                    _ => Xform::flip_y,
                };
                let ops = self.with_doc(h, |doc, sel| edit::transform_each(doc, sel, about));
                self.commit(reqs, h, cmd.name(), ops);
            }
            Cmd::ConstrainH => self.opts.constrain = if self.opts.constrain == 1 { 0 } else { 1 },
            Cmd::ConstrainV => self.opts.constrain = if self.opts.constrain == 2 { 0 } else { 2 },
            Cmd::Unconstrained => self.opts.constrain = 0,
            Cmd::PlaceLabel | Cmd::PlaceWireLabel => self.open_text(TextFor::Label, "Net label"),
            Cmd::PlaceInPort => self.open_text(TextFor::Port(false), "Input port"),
            Cmd::PlaceOutPort => self.open_text(TextFor::Port(true), "Output port"),
            Cmd::InsertText => self.open_text(TextFor::Text, "Text"),
            Cmd::PlaceSymPin => {
                if view == View::Symbol {
                    self.open_text(TextFor::SymPin, "Symbol pin name");
                } else {
                    self.session.say("symbol pins go in the symbol view (i)");
                }
            }
            Cmd::InsertWire | Cmd::InsertSnapWire => {
                if view != View::Schematic {
                    self.session.say("wires go in a schematic");
                } else {
                    let from = if cmd == Cmd::InsertSnapWire {
                        self.nearest_conn(ptr).unwrap_or(ptr)
                    } else {
                        ptr
                    };
                    self.tool = Tool::Wire {
                        from,
                        to: from,
                        drag: false,
                    };
                }
            }
            Cmd::InsertLine | Cmd::InsertRect => {
                // Symbol graphics in the symbol view, `shape` records in a schematic.
                self.tool = Tool::Shape {
                    form: if cmd == Cmd::InsertRect {
                        Form::Rect
                    } else {
                        Form::Line
                    },
                    pts: vec![ptr],
                    to: ptr,
                };
            }
            Cmd::CycleManhattan => match self.tool {
                Tool::Wire { .. } | Tool::Move { .. } | Tool::Shape { .. } => {
                    self.opts.manhattan = (self.opts.manhattan + 1) % 3;
                    assert!(
                        self.opts.manhattan < 3,
                        "straight, horizontal or vertical first"
                    );
                }
                _ => {
                    self.space_pan = true;
                    self.pan = Some(self.pointer);
                }
            },
            Cmd::UnselectUnderMouse => {
                if let Some(o) = self.hit_at(self.pointer) {
                    let key = self.scene.objs.key[o as usize];
                    let sel: Vec<ObjKey> = self
                        .selection()
                        .iter()
                        .copied()
                        .filter(|&k| k != key)
                        .collect();
                    self.set_selection(sel);
                }
            }
            Cmd::UnselectArea => self.tool = Tool::UnselectArmed,
            Cmd::InsertPolygon | Cmd::InsertArc | Cmd::InsertCircle => {
                let form = match cmd {
                    Cmd::InsertPolygon => Form::Poly,
                    Cmd::InsertArc => Form::Arc,
                    _ => Form::Circle,
                };
                self.tool = Tool::Shape {
                    form,
                    pts: vec![ptr],
                    to: ptr,
                };
            }
            Cmd::ClosePolygon => {
                if let Tool::Shape {
                    form: Form::Poly,
                    pts,
                    ..
                } = std::mem::take(&mut self.tool)
                {
                    self.finish_shape(reqs, h, view, Form::Poly, &pts, Outline::Closed);
                }
            }
            Cmd::AlignGrid => {
                let snap_m = self.opts.snap;
                let ops = self.with_doc(h, |d, sel| wiring::align(d, sel, snap_m));
                self.commit(reqs, h, "align to grid", ops);
            }
            Cmd::BreakAtMouse | Cmd::BreakAtMouseAlign => self.cut_wire(reqs, h, cmd),
            Cmd::BreakAtPins | Cmd::RemoveThrough => {
                let sel = self.selection().to_vec();
                let ops = {
                    let p = &self.session.project;
                    let mut ids = Ids::new(&mut self.rng);
                    if cmd == Cmd::RemoveThrough {
                        wiring::remove_through(p, h, &sel, &mut ids)
                    } else {
                        let pts: Vec<Pt> = p.doc(h).map_or_else(Vec::new, |d| {
                            sel.iter()
                                .filter(|k| k.table == TableId::Instances)
                                .filter_map(|&k| edit::loc_of(d, k))
                                .flat_map(|l| wiring::inst_pins(p, h, d, l.row as usize))
                                .collect()
                        });
                        p.doc(h)
                            .map_or_else(Vec::new, |d| wiring::break_at(d, &pts, &mut ids))
                    }
                };
                self.commit(reqs, h, cmd.name(), ops);
            }
            Cmd::JoinTrim => {
                let ops = {
                    let mut ids = Ids::new(&mut self.rng);
                    wiring::join_trim(&self.session.project, h, &mut ids)
                };
                if ops.is_empty() {
                    self.session.say("wires already joined and trimmed");
                }
                self.commit(reqs, h, "join/trim wires", ops);
            }
            Cmd::Search => self.open_search(),
            Cmd::SelectConnected | Cmd::SelectConnectedStop => {
                let mut seeds = self.selection().to_vec();
                if seeds.is_empty()
                    && let Some(o) = self.hit_at(self.pointer)
                {
                    seeds.push(self.scene.objs.key[o as usize]);
                }
                let stop = cmd == Cmd::SelectConnectedStop;
                let keys = wiring::connected(&self.session.project, h, &seeds, stop);
                self.session
                    .say(format!("{} connected object(s)", keys.len()));
                self.set_selection(keys);
            }
            Cmd::ToggleIgnore => {
                let ops = self.with_doc(h, |d, sel| {
                    edit::selected_records(d, sel)
                        .into_iter()
                        .filter(|(_, _, r)| r.kind == RecordKind::Inst)
                        .map(|(_, target, r)| match r.find_attr("ignore") {
                            Some(_) => Op::RemoveAttr {
                                target,
                                key: "ignore".into(),
                                first: None,
                            },
                            None => Op::SetAttr {
                                target,
                                key: "ignore".into(),
                                values: vec!["true".into()],
                            },
                        })
                        .collect::<Vec<_>>()
                });
                if ops.is_empty() {
                    self.session.say("select instances to toggle `ignore`");
                }
                self.commit(reqs, h, "toggle ignore", ops);
            }
            _ => unreachable!("{cmd:?}: not a edit command"),
        }
        true
    }
}
