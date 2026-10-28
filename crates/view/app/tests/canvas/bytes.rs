//! Idle, pan and zoom frames write no instance bytes and no UI bytes.

use crate::common::{Session, VIEW};

/// A pan or zoom frame writes no instance row, only the camera (D39): after the first frame
/// packs the canvas, the rows a middle drag's and a wheel notch's frames leave to upload are
/// zero bytes while the frame's camera moves (the uniform `Pipelines::draw` writes). Negative
/// space: a delete writes rows.
#[test]
fn a_pan_or_zoom_frame_writes_no_instance_bytes() {
    use schemify_render::gpu::InstanceRows;
    use schemify_render::kurbo::Affine;
    use schemify_render::{Batch, GpuPainter};
    use schemify_ui::{Button, InputEvent};
    let mut s = Session::open("inv", "top.shcomp", "zero-bytes", true);
    let (mut batch, mut rows) = (Batch::default(), InstanceRows::default());
    // One frame painted as the window paints it; returns the row bytes it left to upload and
    // its camera, then takes the rows as the GPU does.
    let mut paint = |s: &mut Session| {
        batch.begin_frame();
        let mut p = GpuPainter {
            batch: &mut batch,
            base: Affine::IDENTITY,
            rows: Some(&mut rows),
        };
        schemify_app::paint::frame(&mut s.ed, &mut s.caches, &mut p);
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

/// The UI's retained rows through the real frame spine (`render::ui_rows`): once settled, a
/// frame with no input and every pan frame write no UI byte; opening a menu takes overlay
/// slots and writes them, closing it gives them back.
#[test]
fn idle_and_pan_frames_write_no_ui_bytes() {
    use schemify_render::gpu::InstanceRows;
    use schemify_render::kurbo::Affine;
    use schemify_render::{Batch, GpuPainter};
    use schemify_ui::{Button, InputEvent};
    let mut s = Session::open("inv", "top.shcomp", "ui-bytes", true);
    let (mut batch, mut rows) = (Batch::default(), InstanceRows::default());
    // One frame painted as the window paints it (until its glyphs settle); returns the UI
    // bytes it left to upload, taken as the GPU takes them.
    let mut paint = |s: &mut Session, batch: &mut Batch| {
        let mut n = 0;
        for _ in 0..16 {
            batch.begin_frame();
            let mut p = GpuPainter {
                batch: &mut *batch,
                base: Affine::IDENTITY,
                rows: Some(&mut rows),
            };
            schemify_app::paint::frame(&mut s.ed, &mut s.caches, &mut p);
            rows.frame = None;
            rows.clear_dirty();
            n += batch.ui.dirty_bytes();
            batch.ui.clear_dirty();
            if batch.settled() {
                return n;
            }
        }
        panic!("the frame settles");
    };
    assert!(
        paint(&mut s, &mut batch) > 0,
        "the first frame writes the UI"
    );
    s.frame(Default::default());
    assert_eq!(
        paint(&mut s, &mut batch),
        0,
        "an idle frame writes no UI byte"
    );
    let (x, y) = (VIEW.0 / 2.0, VIEW.1 / 2.0);
    let mid = Button::Middle;
    s.send([
        InputEvent::PointerMove { x, y },
        InputEvent::PointerDown { x, y, button: mid },
    ]);
    paint(&mut s, &mut batch);
    for k in 1..=4 {
        let x = x + 15.0 * k as f32;
        s.send([InputEvent::PointerMove { x, y }]);
        assert_eq!(
            paint(&mut s, &mut batch),
            0,
            "pan frame {k} writes no UI byte"
        );
    }
    s.send([InputEvent::PointerUp { x, y, button: mid }]);
    paint(&mut s, &mut batch);
    let floats = batch.ui.bytes(1).len();
    s.click_node("menu.file");
    assert!(paint(&mut s, &mut batch) > 0, "the open menu is written");
    assert!(batch.ui.bytes(1).len() > floats, "in overlay slots");
    s.key("Escape");
    paint(&mut s, &mut batch);
    assert_eq!(
        batch.ui.bytes(1).len(),
        floats,
        "closed, its slots are given back"
    );
}
