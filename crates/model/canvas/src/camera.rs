//! The pan/zoom camera: world milli-units (D33) ↔ logical screen pixels for one view rectangle.
//! Owned by the editor's session; the renderer reads it. No spec clause: the mapping is ours.
//!
//! Contents:
//! - `pub struct Camera { cx, cy, scale }`, `Camera::SCALE_MIN`, `Camera::SCALE_MAX`
//! - `Camera::to_screen`, `Camera::origin`, `Camera::to_world`, `Camera::pan`,
//!   `Camera::zoom_at`, `Camera::fit`

use kurbo::Rect;

/// Pan/zoom: the world point at the view center and the zoom in pixels per milli-unit.
/// `scale` is positive; [`Camera::zoom_at`] and [`Camera::fit`] keep it in
/// `[SCALE_MIN, SCALE_MAX]`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Camera {
    /// World x (milli-units) at the view center.
    pub cx: f64,
    /// World y (milli-units, y down) at the view center.
    pub cy: f64,
    /// Logical pixels per milli-unit.
    pub scale: f64,
}

impl Camera {
    /// Smallest zoom: 1 px per 10^6 milli-units.
    pub const SCALE_MIN: f64 = 1e-6;
    /// Largest zoom: 10 px per milli-unit.
    pub const SCALE_MAX: f64 = 10.0;

    /// World → screen for a view rectangle `view`.
    #[inline]
    #[must_use]
    pub fn to_screen(&self, view: Rect, x: i32, y: i32) -> (f64, f64) {
        debug_assert!(self.scale > 0.0, "zoom is positive");
        let (ox, oy) = self.origin(view);
        (
            f64::from(x) * self.scale + ox,
            f64::from(y) * self.scale + oy,
        )
    }

    /// Screen position of world (0, 0): `screen = world · scale + origin`.
    #[inline]
    #[must_use]
    pub fn origin(&self, view: Rect) -> (f64, f64) {
        // A NaN center or zoom would put every point at NaN, drawing nothing without a word.
        debug_assert!(self.scale.is_finite(), "zoom is finite");
        debug_assert!(self.cx.is_finite(), "center x is finite");
        debug_assert!(self.cy.is_finite(), "center y is finite");
        (
            view.center().x - self.cx * self.scale,
            view.center().y - self.cy * self.scale,
        )
    }

    /// Screen → world (unrounded milli-units).
    ///
    /// # Panics
    ///
    /// If `scale` is not positive.
    #[must_use]
    pub fn to_world(&self, view: Rect, sx: f64, sy: f64) -> (f64, f64) {
        assert!(self.scale > 0.0, "zoom is positive");
        assert!(self.scale.is_finite(), "zoom is finite");
        let (ox, oy) = self.origin(view);
        ((sx - ox) / self.scale, (sy - oy) / self.scale)
    }

    /// Moves the picture by `(dx, dy)` screen pixels.
    ///
    /// # Panics
    ///
    /// If `scale` is not positive, or the move leaves the center non-finite (a NaN or
    /// infinite `dx`, `dy`).
    pub fn pan(&mut self, dx: f64, dy: f64) {
        assert!(self.scale > 0.0, "zoom is positive");
        self.cx -= dx / self.scale;
        self.cy -= dy / self.scale;
        assert!(
            self.cx.is_finite(),
            "pan by {dx} left the center x non-finite"
        );
        assert!(
            self.cy.is_finite(),
            "pan by {dy} left the center y non-finite"
        );
    }

    /// Multiplies the zoom by `factor`, clamped to `[SCALE_MIN, SCALE_MAX]`, keeping the world
    /// point under `(sx, sy)` fixed.
    ///
    /// # Panics
    ///
    /// If `factor` is not positive and finite, `scale` is not positive, or the cursor
    /// `(sx, sy)` is not finite.
    pub fn zoom_at(&mut self, view: Rect, sx: f64, sy: f64, factor: f64) {
        assert!(factor > 0.0, "zoom factor is positive");
        assert!(factor.is_finite(), "zoom factor is finite");
        let (wx, wy) = self.to_world(view, sx, sy);
        self.scale = (self.scale * factor).clamp(Self::SCALE_MIN, Self::SCALE_MAX);
        let (nx, ny) = self.to_world(view, sx, sy);
        self.cx += wx - nx;
        self.cy += wy - ny;
        assert!(
            self.cx.is_finite(),
            "zoom at x {sx} left the center x non-finite"
        );
        assert!(
            self.cy.is_finite(),
            "zoom at y {sy} left the center y non-finite"
        );
    }

    /// Fits world bounds `[x0, y0, x1, y1]` into `view` with a 5% margin; an empty extent
    /// counts as 1 milli-unit wide.
    ///
    /// # Panics
    ///
    /// If the bounds are inverted (`x1 < x0` or `y1 < y0`).
    pub fn fit(&mut self, view: Rect, bounds: [i32; 4]) {
        let [x0, y0, x1, y1] = bounds.map(f64::from);
        assert!(x0 <= x1, "inverted bounds in x");
        assert!(y0 <= y1, "inverted bounds in y");
        let (width, height) = (x1 - x0, y1 - y0);
        self.cx = (x0 + x1) / 2.0;
        self.cy = (y0 + y1) / 2.0;
        let scale = (view.width() / width.max(1.0)).min(view.height() / height.max(1.0)) * 0.95;
        self.scale = scale.clamp(Self::SCALE_MIN, Self::SCALE_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: Rect = Rect::new(0.0, 0.0, 800.0, 600.0);

    /// Oracle: screen → world inverts world → screen.
    #[test]
    fn to_world_inverts_to_screen() {
        let cam = Camera {
            cx: 1_234.0,
            cy: -500.0,
            scale: 0.25,
        };
        let (sx, sy) = cam.to_screen(VIEW, 7_000, -3_000);
        assert_eq!(cam.to_world(VIEW, sx, sy), (7_000.0, -3_000.0));
        assert_eq!(cam.to_screen(VIEW, 1_234, -500), (400.0, 300.0), "center");
    }

    /// The world point under the cursor stays put; the zoom clamps at its bounds.
    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let mut cam = Camera {
            cx: 0.0,
            cy: 0.0,
            scale: 1.0,
        };
        let before = cam.to_world(VIEW, 100.0, 50.0);
        cam.zoom_at(VIEW, 100.0, 50.0, 4.0);
        let after = cam.to_world(VIEW, 100.0, 50.0);
        assert!((before.0 - after.0).abs() < 1e-9);
        assert!((before.1 - after.1).abs() < 1e-9);
        cam.zoom_at(VIEW, 0.0, 0.0, 1e9);
        assert_eq!(cam.scale, Camera::SCALE_MAX);
        let cx = cam.cx;
        cam.pan(10.0, 0.0);
        assert_eq!(cam.cx, cx - 1.0, "10 px at 10 px per milli-unit");
    }

    /// The fitted bounds lie inside the view, centered.
    #[test]
    fn fit_shows_the_bounds() {
        let mut cam = Camera {
            cx: 0.0,
            cy: 0.0,
            scale: 1.0,
        };
        cam.fit(VIEW, [-1_000, 0, 3_000, 1_000]);
        let (x0, y0) = cam.to_screen(VIEW, -1_000, 0);
        let (x1, y1) = cam.to_screen(VIEW, 3_000, 1_000);
        assert!(x0 >= 0.0 && x1 <= 800.0, "{x0} {x1}");
        assert!(y0 >= 0.0 && y1 <= 600.0, "{y0} {y1}");
        assert_eq!(((x0 + x1) / 2.0, (y0 + y1) / 2.0), (400.0, 300.0));
    }
}
