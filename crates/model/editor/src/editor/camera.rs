//! Screen ↔ world: the active level's camera, pointer positions, fits.
//!
//! Contents:
//! - `Editor::canvas_rect`
//! - `Editor::camera`
//! - `Editor::set_camera`
//! - `Editor::to_screen`
//! - `Editor::pointer_world`
//! - `Editor::pointer_world_snapped`
//! - `Editor::world_at`
//! - `Editor::snapped_at`
//! - `Editor::nearest_conn`
//! - `Editor::fit`
//! - `Editor::fit_world`

use schemify_canvas::Camera;
use schemify_canvas::kurbo::Rect as KRect;

use crate::geometry::{Pt, snap};
use crate::scene::UNIT;

use super::Editor;

impl Editor {
    /// The canvas rectangle in logical px.
    pub fn canvas_rect(&self) -> KRect {
        let r = self.shell.canvas_rect(self.viewport);
        KRect::new(
            f64::from(r.x),
            f64::from(r.y),
            f64::from(r.x + r.w),
            f64::from(r.y + r.h),
        )
    }
    /// The active level's camera (fitted on first build).
    pub fn camera(&self) -> Camera {
        self.session.level().and_then(|l| l.cam).unwrap_or(Camera {
            cx: 0.0,
            cy: 0.0,
            scale: 0.01,
        })
    }
    pub(super) fn set_camera(&mut self, cam: Camera) {
        self.session.set_camera(cam);
    }
    /// Screen position (logical px) of a world point, for scripted input.
    pub fn to_screen(&self, p: Pt) -> (f32, f32) {
        let (x, y) = self.camera().to_screen(self.canvas_rect(), p.0, p.1);
        (x as f32, y as f32)
    }
    pub(super) fn pointer_world(&self) -> (f64, f64) {
        let v = self.canvas_rect();
        self.camera()
            .to_world(v, f64::from(self.pointer.0), f64::from(self.pointer.1))
    }
    pub(super) fn pointer_world_snapped(&self) -> Pt {
        let (x, y) = self.pointer_world();
        (snap(x, self.opts.snap), snap(y, self.opts.snap))
    }
    pub(super) fn world_at(&self, s: (f32, f32)) -> (f64, f64) {
        self.camera()
            .to_world(self.canvas_rect(), f64::from(s.0), f64::from(s.1))
    }
    pub(super) fn snapped_at(&self, s: (f32, f32)) -> Pt {
        let (x, y) = self.world_at(s);
        (snap(x, self.opts.snap), snap(y, self.opts.snap))
    }
    pub(super) fn nearest_conn(&self, p: Pt) -> Option<Pt> {
        let d = |q: Pt| {
            (i64::from(q.0) - i64::from(p.0)).pow(2) + (i64::from(q.1) - i64::from(p.1)).pow(2)
        };
        self.scene.conn_points().map(|c| c.0).min_by_key(|&q| d(q))
    }
    pub(super) fn fit(&mut self, b: [i32; 4]) {
        let v = self.canvas_rect();
        if v.width() <= 0.0 || v.height() <= 0.0 {
            return;
        }
        let mut cam = self.camera();
        let pad = 20 * UNIT;
        cam.fit(v, [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad]);
        self.set_camera(cam);
    }
    pub(super) fn fit_world(&mut self, p: (f64, f64), q: (f64, f64)) {
        let b = [
            p.0.min(q.0) as i32,
            p.1.min(q.1) as i32,
            p.0.max(q.0) as i32,
            p.1.max(q.1) as i32,
        ];
        let v = self.canvas_rect();
        let mut cam = self.camera();
        cam.fit(v, b);
        self.set_camera(cam);
    }
}
