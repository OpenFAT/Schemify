//! The canvas tables: symbols, scene, selection marks, overlay, the scene as shown, the palette.
//!
//! Contents:
//! - `pub(super) type ShownKey`
//! - `pub(super) const USER_LAYERS`
//! - `pub(super) const LAYER_NAMES`
//! - `Editor::load_symbols`
//! - `Editor::refresh_symbols_of`
//! - `Editor::build_scene`
//! - `Editor::build_overlay`
//! - `Editor::refresh_palette`
//! - `Editor::build_shown`
//! - `Editor::view_palette`

use schemify_canvas::{CanvasPrims, Palette};
use schemify_doc::{DocHandle, TableId};
use schemify_format::{FileKind, Orient};
use schemify_plugin::abi::Event;

use crate::geometry::Xform;
use crate::scene::{LAYER_SEL, ObjKey, UNIT};
use crate::session::View;
use crate::{checks, edit, export, scene};

use super::Editor;
use super::finish;
use super::tool::{Form, Placement, Tool};

/// What a displayed scene (`shown`) was made for: (scene build key, symbol mode, symbol texts).
pub(super) type ShownKey = ((DocHandle, View, u32, u32), u8, bool);

/// Canvas layers a user can draw on and hide (xschem's 22).
pub(super) const USER_LAYERS: u8 = 22;

/// Names of the special layers (xschem Layers menu).
pub(super) const LAYER_NAMES: [&str; 6] = ["BG", "WIRE", "GRID", "TEXT", "", "PIN"];

impl Editor {
    /// Refreshes the symbol cache for the active schematic. Skipped while nothing changed
    /// since the last call.
    pub(super) fn load_symbols(&mut self) {
        let Some((h, View::Schematic)) = self.active() else {
            return;
        };
        let key = (h, self.generation);
        if self.symbols_key != Some(key) {
            self.symbols_key = Some(key);
            self.refresh_symbols_of(h);
        }
        assert_eq!(self.symbols_key, Some(key));
    }
    /// Makes the symbol cache current for every cell `d` instantiates, from the library
    /// index (`Project::interface`): an open cell draws its edited document, any other its
    /// indexed symbol, so nothing is opened. Cost: one resolve and one scan of the open
    /// documents per distinct cell; a symbol is rebuilt only when its source changed.
    pub(super) fn refresh_symbols_of(&mut self, d: DocHandle) {
        let project = &self.session.project;
        let Some(doc) = project.doc(d) else {
            return;
        };
        let mut cells = doc.instances().cell.clone();
        cells.sort_unstable_by_key(|c| c.0);
        cells.dedup();
        for c in cells {
            let Ok(row) = project.resolve(d, doc.strings().get(c)) else {
                continue;
            };
            let open = project.open_handle(row);
            let epoch = open.map_or(0, |o| self.session.epoch(o));
            if let Some(iface) = project.interface(row) {
                self.symbols.refresh(row, open, epoch, iface);
            }
        }
    }
    /// `scene::build` when the active document, view, edit epoch or symbols changed; then
    /// the selection bitmap and marks when the selection changed; fits a level that has no
    /// camera yet once the canvas has a size. True when the scene or the camera changed.
    pub(super) fn build_scene(&mut self) -> bool {
        let Some((h, view)) = self.active() else {
            return false;
        };
        let epoch = self.session.epoch(h);
        let full = self.scene_values != self.values_gen || view != View::Schematic;
        let stale = full
            || !self.revalued.is_empty()
            || self.scene.built != Some((h, view, epoch, self.symbols.epoch));
        if stale {
            let fe = self.host.plugins.composed_epoch();
            let values = self
                .cell
                .as_ref()
                .filter(|_| self.cell_key == Some((h, epoch, fe)));
            let project = &self.session.project;
            if full {
                scene::build(
                    &mut self.scene,
                    project,
                    h,
                    view,
                    &self.symbols,
                    epoch,
                    values,
                );
            } else {
                // An edit: only the objects whose rows changed are re-stamped.
                let revalued = &self.revalued;
                scene::update(
                    &mut self.scene,
                    project,
                    h,
                    &self.symbols,
                    epoch,
                    values,
                    revalued,
                );
            }
            assert!(
                self.session.project.doc(h).is_none()
                    || self.scene.built == Some((h, view, epoch, self.symbols.epoch)),
                "the scene is built for the active level"
            );
            self.canvas_gen[0] += 1;
            self.scene_values = self.values_gen;
            self.revalued.clear();
            let sel: Vec<ObjKey> = self
                .selection()
                .iter()
                .copied()
                .filter(|&k| self.scene.find(k).is_some())
                .collect();
            self.set_selection(sel);
            self.sel_changed = true;
        }
        // Outside `stale`: the first viewport can be empty (the web canvas has no size until
        // its first `Resized`), and the scene is not rebuilt when the real size arrives.
        let unfitted = self.session.level().is_some_and(|l| l.cam.is_none());
        let fitted = unfitted && self.canvas_rect().width() > 0.0;
        if fitted {
            let e = self.scene.extent;
            let empty = e == [0; 4];
            self.fit(if empty {
                [-400 * UNIT, -300 * UNIT, 400 * UNIT, 300 * UNIT]
            } else {
                e
            });
        }
        if self.sel_changed {
            let sel = self.session.level().map_or(&[][..], |l| &l.sel);
            let objs = sel.iter().filter_map(|&k| self.scene.find(k));
            self.selected.set(self.scene.objs.len(), objs);
            assert!(self.selected.len() <= sel.len());
            scene::selected_prims(&self.scene, &self.selected, &mut self.sel_prims);
            scene::marks(
                &self.scene,
                &self.sel_prims,
                self.sel_color,
                &mut self.marks,
            );
            assert!(self.sel_prims.len() <= self.scene.prims.len());
            self.canvas_gen[3] += 1;
            self.sel_changed = false;
        }
        if std::mem::take(&mut self.sel_moved) {
            let _ = self.host.push(None, &Event::SelectionChanged);
        }
        stale || fitted
    }
    /// Rubber bands and previews of the gesture in progress (overlay primitives).
    pub(super) fn build_overlay(&mut self) {
        let mut o = CanvasPrims::default();
        match &self.tool {
            Tool::Box { from, to, .. } | Tool::Zoom { from, to } => {
                o.rect(
                    from.0 as i32,
                    from.1 as i32,
                    to.0 as i32,
                    to.1 as i32,
                    LAYER_SEL,
                );
            }
            Tool::Move { from, to, lin, .. } => {
                let x = lin.then(Xform::translate(to.0 - from.0, to.1 - from.1));
                scene::push_moved(&self.scene, &self.sel_prims, x, &mut o);
            }
            Tool::Wire { from, to, .. } => {
                let pts = self.manhattan_points(*from, *to);
                for w in pts.windows(2) {
                    o.line(w[0].0, w[0].1, w[1].0, w[1].1, 1);
                }
            }
            Tool::Shape { .. } => {} // `parity_overlay` previews shapes
            Tool::Place { what, at, lin } => {
                let p = &self.session.project;
                let row = match (what, self.active()) {
                    (Placement::Inst(c), Some((h, _))) => p.resolve(h, c).ok(),
                    _ => None,
                };
                match what {
                    Placement::Inst(c) => {
                        let o2 = lin.orient(Orient::R0);
                        let drawn =
                            row.is_some_and(|r| scene::stamp(&self.symbols, r, o2, *at, &mut o));
                        if !drawn {
                            let u = 10 * UNIT;
                            o.rect(at.0 - u, at.1 - u, at.0 + u, at.1 + u, LAYER_SEL);
                            o.text(at.0 - u, at.1 + u + UNIT, 8 * UNIT, LAYER_SEL, c);
                        }
                    }
                    Placement::Label(t)
                    | Placement::Port(t, _)
                    | Placement::Text(t)
                    | Placement::SymPin(t) => {
                        o.rect(
                            at.0 - 2500,
                            at.1 - 2500,
                            at.0 + 2500,
                            at.1 + 2500,
                            LAYER_SEL,
                        );
                        o.text(at.0 + 4 * UNIT, at.1 - 17 * UNIT, 16 * UNIT, LAYER_SEL, t);
                    }
                    Placement::Paste(recs) => {
                        let x = lin.then(Xform::translate(at.0, at.1));
                        scene::preview_records(recs, x, &mut o);
                    }
                }
            }
            Tool::Idle
            | Tool::Press { .. }
            | Tool::ZoomArmed
            | Tool::UnselectArmed
            | Tool::Vertex { .. } => {} // `gesture_overlay` previews vertex drags
        }
        self.parity_overlay(&mut o);
        // Selected texts have no stroke to recolor: outline them (in object order; a frame
        // looks at the selection, not at every object).
        let sel = self.session.level().map_or(&[][..], |l| &l.sel);
        let mut texts: Vec<u32> = sel
            .iter()
            .filter(|k| k.table == TableId::Texts)
            .filter_map(|&k| self.scene.find(k))
            .collect();
        texts.sort_unstable();
        texts.dedup();
        for i in texts {
            let b = self.scene.objs.bbox(i as usize);
            o.rect(b[0], b[1], b[2], b[3], LAYER_SEL);
        }
        self.overlay = o;
    }
    pub(super) fn refresh_palette(&mut self) {
        let (p, sel) = self.shell.palette(self.opts.light);
        self.palette = p;
        self.sel_color = sel;
        self.sel_changed = true;
    }
    /// Rebuilds `shown` (the scene as displayed) when the symbol display mode or the symbol
    /// texts switch differ from the defaults: instances drawn as boxes, their texts dropped.
    pub(super) fn build_shown(&mut self) {
        let mode = self.opts.show_symbols;
        let texts = self.opts.symbol_text;
        if mode == 0 && texts {
            if self.shown.take().is_some() {
                self.canvas_gen[0] += 1; // the canvas draws the scene as built again
            }
            self.shown_key = None;
            return;
        }
        let Some(built) = self.scene.built else {
            return;
        };
        let key = (built, mode, texts);
        if self.shown_key == Some(key) {
            return;
        }
        let sc = &self.scene;
        let p = &self.session.project;
        let h = built.0;
        // Objects drawn as boxes: instances (mode 2) or instances of schematic cells (mode 1).
        let boxed: Vec<bool> = match p.doc(h) {
            Some(doc) => sc
                .objs
                .key
                .iter()
                .map(|k| {
                    if k.table != TableId::Instances || mode == 0 {
                        return false;
                    }
                    if mode == 2 {
                        return true;
                    }
                    edit::loc_of(doc, *k).is_some_and(|l| {
                        let cell = doc.strings().get(doc.instances().cell[l.row as usize]);
                        p.resolve(h, cell)
                            .is_ok_and(|r| p.cells().kind[r] == FileKind::Shcomp)
                    })
                })
                .collect(),
            None => vec![false; sc.objs.len()],
        };
        let is_inst: Vec<bool> = sc
            .objs
            .key
            .iter()
            .map(|k| k.table == TableId::Instances)
            .collect();
        assert_eq!(boxed.len(), sc.objs.len(), "one flag per object");
        let keep: Vec<bool> = boxed.iter().map(|b| !b).collect();
        let mut out = export::subset(sc, &keep);
        if !texts {
            // Rebuild the texts without the instances' own.
            let mut t = CanvasPrims::default();
            let pt = &out.texts;
            let mut start = 0usize;
            let owners: Vec<u32> = sc
                .owner
                .texts
                .iter()
                .copied()
                // As `subset` keeps them: a dead row (owner `u32::MAX`) is not in `out`.
                .filter(|&o| keep.get(o as usize).copied().unwrap_or(false))
                .collect();
            for i in 0..pt.x.len() {
                let end = pt.end[i] as usize;
                let owner = owners.get(i).copied().unwrap_or(u32::MAX);
                if !is_inst.get(owner as usize).copied().unwrap_or(false) {
                    let rot = pt.rot.get(i).copied().unwrap_or(0);
                    t.text_rot(
                        pt.x[i],
                        pt.y[i],
                        pt.size[i],
                        pt.layer[i],
                        &pt.text[start..end],
                        rot,
                    );
                }
                start = end;
            }
            out.texts = t.texts;
        }
        for (i, b) in boxed.iter().enumerate() {
            if *b {
                let bb = sc.objs.bbox(i);
                out.rect(bb[0], bb[1], bb[2], bb[3], 4);
            }
        }
        self.shown = Some(out);
        self.shown_key = Some(key);
        self.canvas_gen[0] += 1;
    }
    /// Overlay additions of the parity options: crosshair, snap cursor, grid axes and the
    /// preview of a shape tool in progress.
    fn parity_overlay(&self, o: &mut CanvasPrims) {
        self.gesture_overlay(o);
        let v = self.canvas_rect();
        let cam = self.camera();
        let w = self.view_world_rect();
        if let Tool::Shape { form, pts, to } = &self.tool {
            assert!(!pts.is_empty(), "a shape tool holds its first point");
            let layer = self.opts.layer;
            match form {
                Form::Rect => {
                    o.rect(pts[0].0, pts[0].1, to.0, to.1, layer);
                }
                Form::Circle => {
                    o.arc(
                        pts[0].0,
                        pts[0].1,
                        finish::dist(pts[0], *to),
                        0,
                        360_000,
                        layer,
                    );
                }
                Form::Arc if pts.len() == 2 => {
                    if let Some(a) = finish::arc_3(pts[0], pts[1], *to) {
                        o.arc(a.center.0, a.center.1, a.r, a.start, a.sweep, layer);
                    } else {
                        o.line(pts[0].0, pts[0].1, pts[1].0, pts[1].1, layer);
                    }
                }
                _ => {
                    let mut all = pts.clone();
                    all.push(*to);
                    all.dedup();
                    if all.len() >= 2 {
                        o.poly(&all, false, layer);
                    }
                }
            }
        }
        let (px, py) = self.pointer_world_snapped();
        let inside = |x: f64, y: f64| x >= v.x0 && x <= v.x1 && y >= v.y0 && y <= v.y1;
        let (sx, sy) = (f64::from(self.pointer.0), f64::from(self.pointer.1));
        if self.opts.crosshair && inside(sx, sy) {
            let r = if self.opts.crosshair_px == 0 {
                None
            } else {
                Some((f64::from(self.opts.crosshair_px) / cam.scale) as i32)
            };
            let (x0, x1) = r.map_or((w[0], w[2]), |r| (px - r, px + r));
            let (y0, y1) = r.map_or((w[1], w[3]), |r| (py - r, py + r));
            o.line(x0, py, x1, py, 8);
            o.line(px, y0, px, y1, 8);
        }
        if self.opts.snap_cursor
            && inside(sx, sy)
            && let Some(q) = self.nearest_conn((px, py))
        {
            let d = (6.0 / cam.scale) as i32;
            let near =
                (i64::from(q.0) - i64::from(px)).abs() + (i64::from(q.1) - i64::from(py)).abs();
            if near <= i64::from(d) * 8 {
                o.poly(
                    &[
                        (q.0 - d, q.1),
                        (q.0, q.1 - d),
                        (q.0 + d, q.1),
                        (q.0, q.1 + d),
                    ],
                    true,
                    LAYER_SEL,
                );
            }
        }
        if self.opts.grid_axes {
            if w[1] <= 0 && w[3] >= 0 {
                o.line(w[0], 0, w[2], 0, 2);
            }
            if w[0] <= 0 && w[2] >= 0 {
                o.line(0, w[1], 0, w[3], 2);
            }
        }
    }
    /// The palette the canvas draws with: the theme's, with layer color changes, hidden
    /// layers (alpha 0), dimming toward the background, the fill switch and the line width
    /// (scaled with the zoom when variable).
    pub fn view_palette(&self) -> Palette {
        let mut p = self.palette.clone();
        for &(l, rgb) in &self.layer_colors {
            p.color[usize::from(l)] = (rgb << 8) | 0xff;
        }
        let dim = if self.opts.only_probes && !self.highlight.is_empty() {
            70
        } else {
            self.opts.dim
        };
        if dim > 0 {
            let bg = p.background;
            let mix = |c: u32, shift: u32| {
                let (a, b) = (
                    f64::from((c >> shift) & 0xff),
                    f64::from((bg >> shift) & 0xff),
                );
                ((a + (b - a) * f64::from(dim) / 100.0).round() as u32) << shift
            };
            for (l, c) in p.color.iter_mut().enumerate() {
                // The highlight and annotation layers stay bright ("view only probes").
                if l == usize::from(checks::LAYER_HILITE) || l == 0 {
                    continue;
                }
                *c = mix(*c, 24) | mix(*c, 16) | mix(*c, 8) | (*c & 0xff);
            }
        }
        for l in 0..USER_LAYERS {
            if self.opts.hidden & (1 << l) != 0 {
                p.color[usize::from(l)] &= !0xff;
            }
        }
        if !self.opts.fill {
            p.fill = 0;
        }
        let mut w = f32::from(self.opts.width_tenths) / 10.0;
        if self.opts.variable_width {
            // ponytail: width ∝ zoom around the default fit (scale 0.01 px per milli-unit),
            // clamped; xschem's `change_lw` rule if users ask for its exact curve.
            w *= ((self.camera().scale / 0.01) as f32).clamp(0.5, 4.0);
        }
        for x in &mut p.width {
            *x *= w;
        }
        p
    }
}
