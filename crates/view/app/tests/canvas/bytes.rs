//! Idle, pan and zoom frames write no instance bytes, and an idle window asks for no frame.

use crate::common::{Session, Ui, VIEW};

/// A pan or zoom frame writes no instance row, only the camera (D39): after the first frame
/// packs the canvas, the rows a middle drag's and a wheel notch's frames leave to upload are
/// zero bytes while the frame's camera moves (the uniform `Pipelines::draw` writes). Negative
/// space: a delete writes rows.
#[test]
fn a_pan_or_zoom_frame_writes_no_instance_bytes() {
    use schemify_render::gpu::InstanceRows;
    use schemify_ui::{Button, InputEvent, InputEvents};
    let mut s = Session::open("inv", "top.shcomp", "zero-bytes", Ui::Default);
    let mut rows = InstanceRows::default();
    // One frame painted as the window paints it; returns the row bytes it left to upload and
    // its camera, then takes the rows as the GPU does.
    let mut paint = |s: &mut Session| {
        s.now += 1000;
        let input = InputEvents::default().raw(VIEW);
        let mut f = schemify_app::paint::frame(&mut s.ed, &mut s.caches, input, s.now, &mut rows);
        f.output.textures_delta.clear();
        let bytes = rows.dirty_bytes();
        let cam = rows.frame.take().expect("a canvas frame").cam;
        rows.clear_dirty();
        (bytes, (cam.cx, cam.cy, cam.scale))
    };
    let (first, mut cam) = paint(&mut s);
    assert!(first > 0, "the first frame packs the canvas");
    let (x, y) = (VIEW.0 / 2.0, VIEW.1 / 2.0);
    let mid = Button::Middle;
    s.send([
        InputEvent::PointerMove { x, y },
        InputEvent::PointerDown { x, y, button: mid },
    ]);
    paint(&mut s);
    for k in 1..=4 {
        let x = x + 15.0 * k as f32;
        assert!(
            s.send([InputEvent::PointerMove { x, y }]),
            "pan {k} moves the view"
        );
        let (bytes, now) = paint(&mut s);
        assert_eq!(bytes, 0, "pan frame {k} writes no row");
        assert_ne!(now, cam, "pan frame {k} moves the camera");
        cam = now;
    }
    s.send([InputEvent::PointerUp { x, y, button: mid }]);
    paint(&mut s);
    for (k, dy) in [1.0, 1.0, -1.0].into_iter().enumerate() {
        assert!(
            s.send([InputEvent::Wheel { x, y, dx: 0.0, dy }]),
            "zoom {k} moves the view"
        );
        let (bytes, now) = paint(&mut s);
        assert_eq!(bytes, 0, "zoom frame {k} writes no row");
        assert_ne!(now.2, cam.2, "zoom frame {k} scales the camera");
        cam = now;
    }
    let n = s.ed.scene().objs.len();
    let g = s.ed.scene().objs.geom[0];
    s.click(((g[0] + g[2]) / 2, (g[1] + g[3]) / 2));
    paint(&mut s);
    s.ed.command("edit.delete", "");
    s.frame(Default::default());
    assert_eq!(s.ed.scene().objs.len(), n - 1, "deleted");
    assert!(paint(&mut s).0 > 0, "an edit writes its rows");
}

/// An idle window asks egui for no frame (D39 idle 0%): once settled, a frame with no input
/// reports a repaint delay of "never", and so does one a bounded number of frames after a zoom.
#[test]
fn idle_frames_ask_for_no_repaint() {
    use schemify_render::egui::ViewportId;
    use schemify_ui::InputEvents;
    let mut s = Session::open("inv", "top.shcomp", "idle-repaint", Ui::Default);
    let delay = |s: &mut Session| {
        s.now += 1000;
        let mut rows = schemify_render::gpu::InstanceRows::default();
        let mut f = schemify_app::paint::frame(
            &mut s.ed,
            &mut s.caches,
            InputEvents::default().raw(VIEW),
            s.now,
            &mut rows,
        );
        f.output.textures_delta.clear();
        f.output.viewport_output[&ViewportId::ROOT].repaint_delay
    };
    for _ in 0..4 {
        delay(&mut s);
    }
    assert_eq!(
        delay(&mut s),
        std::time::Duration::MAX,
        "a settled idle frame wants no other"
    );
    s.send([schemify_ui::InputEvent::Wheel {
        x: 400.0,
        y: 400.0,
        dx: 0.0,
        dy: 1.0,
    }]);
    // egui smooths a wheel notch over its next frames (about 0.2 s at 60 fps, `WheelState`)
    // and holds tooltips back meanwhile: bounded, then it sleeps.
    let settled = (0..30).any(|_| delay(&mut s) == std::time::Duration::MAX);
    assert!(settled, "an idle window settles after a zoom");
}
