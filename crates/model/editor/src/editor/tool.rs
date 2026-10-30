//! The `tool_state` table and the canvas gesture state machine over it.
//!
//! Contents:
//! - `pub(super) const HIT_PX`
//! - `pub(super) const ZOOM_STEP`
//! - `pub enum Placement`
//! - `pub enum Carry`
//! - `pub enum Tool`
//! - `pub enum Form`, `Form::points`
//! - `pub(super) fn constrain`
//! - `Editor::gesture`

use schemify_doc::{EditRequest, RecordBuf};
use schemify_ui::{Button, InputEvent, Mods};

use crate::geometry::{Pt, Xform};
use crate::scene;
use crate::scene::ObjKey;
use crate::session::View;

use super::Editor;

/// Hit-test tolerance in logical px.
pub(super) const HIT_PX: f64 = 5.0;

/// Zoom factor per wheel notch and per zoom key (xschem: 1.2).
pub(super) const ZOOM_STEP: f64 = 1.2;

/// What a placement attaches to the pointer.
#[derive(Clone, Debug, PartialEq)]
pub enum Placement {
    /// An instance of a cell reference.
    Inst(String),
    /// A net label.
    Label(String),
    /// A port.
    Port {
        /// The port's net name.
        name: String,
        /// An output port (else an input).
        output: bool,
    },
    /// An annotation text.
    Text(String),
    /// A symbol pin (symbol view).
    SymPin(String),
    /// Clipboard records, relative to the pointer.
    Paste(Vec<RecordBuf>),
}

/// What a move does with the selection and the wires attached to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carry {
    /// Copies the selection; nothing attached follows.
    Copy,
    /// Moves the selection alone.
    Plain,
    /// Moves the selection and stretches the wires attached to it.
    Stretch,
    /// Stretches attached wires and adds "kissing" wires where a pin leaves a pin.
    Kiss,
}

/// The `tool_state` table: one row, the gesture in progress.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Tool {
    /// Nothing in progress.
    #[default]
    Idle,
    /// Left button down at `at` (screen) over object `hit`; becomes a drag past 8 px.
    Press {
        /// Screen position of the press.
        at: (f32, f32),
        /// Snapped world position of the press.
        world: Pt,
        /// Object under the press.
        hit: Option<u32>,
    },
    /// Box selection from `from` to `to` (world, unsnapped).
    Box {
        /// First corner.
        from: (f64, f64),
        /// Current corner.
        to: (f64, f64),
        /// Shift: add to the selection.
        additive: bool,
        /// Alt: remove from the selection.
        unselect: bool,
        /// Ctrl (a plain drag with Options → enable stretch): wires crossing the box edge get
        /// only their inside end selected (`gestures::box_partial`).
        partial: bool,
    },
    /// Moving or copying the selection from `from` to `to`, transformed by `lin` (rotations
    /// and flips about `from` made in flight).
    Move {
        /// Grab point.
        from: Pt,
        /// Current point.
        to: Pt,
        /// Rotation/flip about `from`.
        lin: Xform,
        /// Copy, plain move, or a move whose attached wires follow.
        carry: Carry,
        /// Ends on button release (a drag) instead of on click.
        drag: bool,
    },
    /// Drawing a wire from `from` to `to`.
    Wire {
        /// Start point.
        from: Pt,
        /// Current end.
        to: Pt,
        /// Ends on release.
        drag: bool,
    },
    /// Drawing a graphic (symbol graphics in the symbol view, `shape` records in a
    /// schematic): the points clicked so far and the pointer.
    Shape {
        /// What is drawn.
        form: Form,
        /// Points clicked so far (the first point is where the tool started).
        pts: Vec<Pt>,
        /// Current pointer position.
        to: Pt,
    },
    /// `D`: the next press starts a box that removes objects from the selection.
    UnselectArmed,
    /// An object attached to the pointer; a click drops it.
    Place {
        /// What.
        what: Placement,
        /// Current position.
        at: Pt,
        /// Rotation/flip made in flight.
        lin: Xform,
    },
    /// Dragging vertex `index` of the one selected wire, line, rectangle or polygon
    /// (`gestures::vertex_press`); ends on release.
    Vertex {
        /// The object.
        key: ObjKey,
        /// Its vertex (`gestures::vertices` order).
        index: u32,
        /// Where the vertex was.
        from: Pt,
        /// Current (snapped) position.
        to: Pt,
    },
    /// `z`: the next press starts a zoom box.
    ZoomArmed,
    /// Zoom box from `from` to `to` (world).
    Zoom {
        /// First corner.
        from: (f64, f64),
        /// Current corner.
        to: (f64, f64),
    },
}

/// What a shape tool draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// Two points.
    Line,
    /// Two corners.
    Rect,
    /// Vertices until closed (Enter, or a click on the first vertex).
    Poly,
    /// Three points: start, end, a point on the arc.
    Arc,
    /// Center and a point on the circle.
    Circle,
}

/// Pointer travel (logical px) before a press becomes a drag (xschem: 8 px).
const DRAG_PX: f32 = 8.0;

/// Double-click window.
const DOUBLE_CLICK_MS: u64 = 400;

impl Carry {
    /// `Stretch` when `stretch`, else `Plain`: a move key or drag under the Options →
    /// enable stretch toggle (or its Ctrl inversion).
    pub(super) fn stretch_if(stretch: bool) -> Carry {
        if stretch {
            Carry::Stretch
        } else {
            Carry::Plain
        }
    }
}

impl Form {
    /// Points that complete the shape (`None`: open-ended polygon).
    pub(super) fn points(self) -> Option<usize> {
        match self {
            Form::Line | Form::Rect | Form::Circle => Some(2),
            Form::Arc => Some(3),
            Form::Poly => None,
        }
    }
}

/// Snaps `to` onto the constraint axis through `from`.
pub(super) fn constrain(from: Pt, to: Pt, c: u8) -> Pt {
    assert!(c <= 2, "free, horizontal or vertical");
    let p = match c {
        1 => (to.0, from.1),
        2 => (from.0, to.1),
        _ => to,
    };
    debug_assert!(c != 1 || p.1 == from.1, "a horizontal constraint keeps y");
    p
}

impl Editor {
    /// One canvas pointer event through the tool state machine (xschem §2.3/§2.4 with
    /// `intuitive_interface=1`, `infix_interface=1`). Returns whether anything changed.
    pub(super) fn gesture(
        &mut self,
        ev: InputEvent,
        reqs: &mut Vec<EditRequest>,
        now: u64,
    ) -> bool {
        let Some((h, view)) = self.active() else {
            return false;
        };
        let shift = self.mods & Mods::SHIFT != 0;
        let alt = self.mods & Mods::ALT != 0;
        let ctrl = self.mods & Mods::CTRL != 0;
        match ev {
            InputEvent::PointerMove { x, y } => {
                let prev = self.pointer;
                self.pointer = (x, y);
                if let Some(p) = self.pan {
                    let mut cam = self.camera();
                    cam.pan(f64::from(x - p.0), f64::from(y - p.1));
                    self.set_camera(cam);
                    self.pan = Some((x, y));
                    return true;
                }
                let w = self.snapped_at((x, y));
                let raw = self.world_at((x, y));
                match &mut self.tool {
                    Tool::Press { at, world, hit } => {
                        let (dx, dy) = (x - at.0, y - at.1);
                        if dx * dx + dy * dy >= DRAG_PX * DRAG_PX {
                            let (world, hit, at) = (*world, *hit, *at);
                            // The press already selected what it hit (unless Alt), so a drag
                            // that starts on an object moves the selection (intuitive mode).
                            let keys = self.session.level().map_or(&[][..], |l| &l.sel);
                            let ends = self.partial_ends();
                            let on_selected = hit.is_some_and(|o| {
                                let k = self.scene.objs.key[o as usize];
                                keys.contains(&k) || ends.iter().any(|e| e.0 == k)
                            });
                            // xschem: Ctrl+drag stretches (swapped by `enable_stretch`),
                            // Ctrl+Shift+drag stretches with kissing wires, Shift+drag copies.
                            let carry = match (ctrl, shift) {
                                (true, true) => Carry::Kiss,
                                (true, false) => Carry::stretch_if(!self.opts.stretch),
                                (false, true) => Carry::Copy,
                                (false, false) => Carry::stretch_if(self.opts.stretch),
                            };
                            self.tool = if on_selected {
                                Tool::Move {
                                    from: world,
                                    to: w,
                                    lin: Xform::IDENTITY,
                                    carry,
                                    drag: true,
                                }
                            } else {
                                Tool::Box {
                                    from: self.world_at(at),
                                    to: raw,
                                    additive: shift,
                                    unselect: alt,
                                    partial: !alt && ctrl != self.opts.stretch,
                                }
                            };
                        }
                    }
                    Tool::Box { to, .. } | Tool::Zoom { to, .. } => *to = raw,
                    Tool::Move { from, to, .. } => *to = constrain(*from, w, self.opts.constrain),
                    Tool::Wire { from, to, .. } => *to = constrain(*from, w, self.opts.constrain),
                    Tool::Shape { pts, to, .. } => {
                        let last = *pts.last().expect("a shape tool has its first point");
                        *to = constrain(last, w, self.opts.constrain);
                    }
                    Tool::Place { at, .. } | Tool::Vertex { to: at, .. } => *at = w,
                    Tool::Idle | Tool::ZoomArmed | Tool::UnselectArmed => {
                        // The status line shows the pointer coordinates.
                        return prev != (x, y);
                    }
                }
                true
            }
            InputEvent::PointerDown { x, y, button } => {
                self.pointer = (x, y);
                if self.space_pan {
                    self.space_pan = false;
                    self.pan = None;
                    return true;
                }
                match button {
                    Button::Middle => {
                        self.pan = Some((x, y));
                        return false;
                    }
                    Button::Right => {
                        self.rzoom = Some((x, y));
                        return false;
                    }
                    Button::Left => {}
                }
                let w = self.snapped_at((x, y));
                match std::mem::take(&mut self.tool) {
                    Tool::Move {
                        from,
                        to,
                        lin,
                        carry,
                        drag: false,
                    } => {
                        self.finish_move(reqs, h, from, to, lin, carry);
                    }
                    Tool::Wire { from, to, .. } => {
                        let to = if shift {
                            self.nearest_conn(to).unwrap_or(to)
                        } else {
                            to
                        };
                        self.finish_wire(reqs, h, from, to);
                    }
                    Tool::Shape { form, pts, to } => {
                        self.shape_click(reqs, h, view, form, pts, to);
                    }
                    Tool::Place { what, lin, .. } => self.finish_place(reqs, h, view, what, w, lin),
                    Tool::ZoomArmed => {
                        let p = self.world_at((x, y));
                        self.tool = Tool::Zoom { from: p, to: p };
                    }
                    Tool::UnselectArmed => {
                        let p = self.world_at((x, y));
                        self.tool = Tool::Box {
                            from: p,
                            to: p,
                            additive: false,
                            unselect: true,
                            partial: false,
                        };
                    }
                    _ => {
                        // Double click: properties of the object under the pointer.
                        let double = self.last_click.is_some_and(|(t, px, py)| {
                            now.saturating_sub(t) <= DOUBLE_CLICK_MS
                                && (px - x).abs() < 4.0
                                && (py - y).abs() < 4.0
                        });
                        self.last_click = Some((now, x, y));
                        let hit = self.hit_at((x, y));
                        if self.waves.is_some() && view == View::Schematic && !alt && !ctrl {
                            self.cross_probe(h, hit, w);
                        }
                        if let Some(o) = hit.filter(|_| double) {
                            self.last_click = None;
                            let key = self.scene.objs.key[o as usize];
                            self.set_selection(vec![key]);
                            self.open_props(h);
                            return true;
                        }
                        // A vertex of the one selected wire, line, rect or polygon: drag it.
                        if !shift && !alt && !ctrl && self.vertex_press((x, y)) {
                            return true;
                        }
                        // Intuitive wire start: the snapped press equals a connection point of
                        // an unselected object (pin or wire end) exactly (xschem §2.5), within
                        // the hit tolerance on screen, no modifier.
                        if !shift && !alt && !ctrl && view == View::Schematic {
                            let tol = HIT_PX / self.camera().scale;
                            let raw = self.world_at((x, y));
                            let on_conn = self.scene.conn_points().find(|(p, o)| {
                                *p == w
                                    && (f64::from(p.0) - raw.0).abs() <= tol
                                    && (f64::from(p.1) - raw.1).abs() <= tol
                                    && !self.selected.contains(*o)
                            });
                            if let Some((p, _)) = on_conn {
                                self.tool = Tool::Wire {
                                    from: p,
                                    to: p,
                                    drag: true,
                                };
                                return true;
                            }
                        }
                        let mut sel: Vec<ObjKey> = self.selection().to_vec();
                        let hit_key = hit.map(|o| self.scene.objs.key[o as usize]);
                        // A press on a partially selected wire keeps the selection, so a drag
                        // moves it like a press on a selected object; a new selection drops it.
                        let partial =
                            hit_key.is_some_and(|k| self.partial_ends().iter().any(|e| e.0 == k));
                        let mut fresh = false;
                        match hit_key {
                            Some(k) if alt => sel.retain(|&s| s != k),
                            Some(k) if shift => sel.push(k),
                            Some(k) if !sel.contains(&k) && !partial => {
                                sel = vec![k];
                                fresh = true;
                            }
                            Some(_) => {}
                            None if !shift => {
                                sel.clear();
                                fresh = true;
                            }
                            None => {}
                        }
                        self.set_selection(sel);
                        if fresh {
                            self.set_partial(Vec::new());
                        }
                        self.tool = Tool::Press {
                            at: (x, y),
                            world: w,
                            hit,
                        };
                    }
                }
                true
            }
            InputEvent::PointerUp { x, y, button } => {
                self.pointer = (x, y);
                match button {
                    Button::Middle => {
                        self.pan = None;
                        return false;
                    }
                    Button::Right => {
                        if let Some(a) = self.rzoom.take() {
                            let (dx, dy) = (x - a.0, y - a.1);
                            if dx * dx + dy * dy >= DRAG_PX * DRAG_PX {
                                let (p, q) = (self.world_at(a), self.world_at((x, y)));
                                self.fit_world(p, q);
                                return true;
                            }
                            // A click: modified actions, end the tool, or the context menu.
                            self.right_click(reqs, h, view, (x, y));
                            return true;
                        }
                        return false;
                    }
                    Button::Left => {}
                }
                match std::mem::take(&mut self.tool) {
                    Tool::Move {
                        from,
                        to,
                        lin,
                        carry,
                        drag: true,
                    } => self.finish_move(reqs, h, from, to, lin, carry),
                    Tool::Wire {
                        from,
                        to,
                        drag: true,
                    } => self.finish_wire(reqs, h, from, to),
                    Tool::Vertex { key, index, to, .. } => {
                        self.finish_vertex(reqs, h, key, index, to);
                    }
                    Tool::Box {
                        from,
                        to,
                        additive,
                        unselect,
                        partial,
                    } => {
                        let touch = to.0 < from.0; // right-to-left drag selects touching
                        let hits = scene::box_select(
                            &self.scene.objs,
                            [from.0, from.1, to.0, to.1],
                            touch,
                        );
                        let sel = if unselect {
                            self.unselected(&hits)
                        } else {
                            let keys = hits.iter().map(|&o| self.scene.objs.key[o as usize]);
                            let mut sel = match additive {
                                true => self.selection().to_vec(),
                                false => Vec::new(),
                            };
                            sel.extend(keys);
                            sel
                        };
                        self.set_selection(sel);
                        self.box_partial(from, to, partial, additive, unselect);
                    }
                    Tool::Zoom { from, to } => {
                        if from != to {
                            self.fit_world(from, to);
                        }
                    }
                    Tool::Press { .. } => {}
                    other => self.tool = other,
                }
                true
            }
            InputEvent::Wheel { x, y, dy, .. } => {
                let v = self.canvas_rect();
                let mut cam = self.camera();
                if shift {
                    cam.pan(f64::from(dy) * 40.0, 0.0);
                } else if ctrl {
                    cam.pan(0.0, f64::from(dy) * 40.0);
                } else {
                    cam.zoom_at(v, f64::from(x), f64::from(y), ZOOM_STEP.powf(f64::from(dy)));
                }
                self.set_camera(cam);
                true
            }
            _ => false,
        }
    }
}
