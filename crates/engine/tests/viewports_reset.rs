//! VIEWPORTS.RESET: every viewport looks its default way again (Rhino's four, else Top), keeping
//! its centre and zoom, or zoomed to the extents (`zoom`).

use cadcraft_engine::Session;
use cadcraft_engine::geom::{Vec2, Vec3, ViewDir};
use serde_json::json;

fn dir(x: f64, y: f64, z: f64) -> ViewDir {
    ViewDir::new(Vec3::new(x, y, z))
}

fn session() -> Session {
    let mut s = Session::new();
    s.viewport_px = (1200.0, 800.0);
    s.execute("line", &json!({ "points": [[0, 0], [20, 0], [20, 10], [0, 10], [0, 0]] })).unwrap();
    s
}

/// The tile whose lower-left corner is at `(x, y)` (fractions of the drawing area).
fn at(s: &Session, x: f64, y: f64) -> (usize, cadcraft_engine::ModelTile) {
    s.model_tiles().into_iter().enumerate().find(|(_, t)| t.min.near(Vec2::new(x, y), 1e-9)).unwrap()
}

#[test]
fn reset_turns_rhinos_four_back_keeping_each_centre() {
    let mut s = session();
    s.execute("viewports.rhino", &json!({})).unwrap();
    let rev = s.state().unwrap().revision;
    // Turn Front and Top away and pan Top; make Front current.
    let (fi, _) = at(&s, 0.0, 0.0);
    s.execute("vports.activate", &json!({ "index": fi + 1 })).unwrap();
    s.execute("3dorbit", &json!({ "azimuth": 40, "elevation": 20 })).unwrap();
    let (ti, _) = at(&s, 0.0, 0.5);
    s.activate_tile(ti).unwrap();
    s.execute("view.right", &json!({})).unwrap();
    s.state_mut().unwrap().set_view_quiet(cadcraft_engine::View { center: Vec2::new(3.0, 4.0), height: 7.0 });
    let r = s.execute("viewports.reset", &json!({})).unwrap();
    assert_eq!(r["tiles"].as_array().unwrap().len(), 4);
    let (_, top) = at(&s, 0.0, 0.5);
    let (_, persp) = at(&s, 0.5, 0.5);
    let (_, front) = at(&s, 0.0, 0.0);
    let (_, right) = at(&s, 0.5, 0.0);
    assert!(top.active && top.look.dir.is_plan(), "Top is current again, in plan");
    assert_eq!((front.look.dir, right.look.dir, persp.look.dir), (dir(0.0, -1.0, 0.0), dir(1.0, 0.0, 0.0), dir(-1.0, -1.0, 1.0)));
    assert!(persp.look.camera.is_some() && front.look.camera.is_none());
    assert!(s.state().unwrap().view_dir.is_plan(), "the model view is the current tile's");
    // Top was a Right view of the point (y, z) = (3, 4) at the model's middle depth: seen from
    // above that point is at x = 10 (the rectangle's middle), y = 3, zoom kept.
    assert!(top.view.center.near(Vec2::new(10.0, 3.0), 1e-6), "{:?}", top.view.center);
    assert!((top.view.height - 7.0).abs() < 1e-9);
    // A view change: no undo step, nothing drawn changed.
    assert_eq!(s.state().unwrap().revision, rev);
}

#[test]
fn reset_with_zoom_frames_every_tile_and_other_layouts_go_top() {
    let mut s = session();
    s.execute("viewports.rhino", &json!({})).unwrap();
    let framed = s.model_tiles();
    s.execute("3dorbit", &json!({ "azimuth": 10, "elevation": 10 })).unwrap();
    s.state_mut().unwrap().set_view_quiet(cadcraft_engine::View { center: Vec2::new(100.0, 100.0), height: 1.0 });
    s.cmdline("resetviewports").unwrap();
    s.execute("viewports.reset", &json!({ "zoom": true })).unwrap();
    for (a, b) in s.model_tiles().iter().zip(&framed) {
        assert_eq!(a.look.dir, b.look.dir);
        assert!(a.view.center.near(b.view.center, 1e-6) && (a.view.height - b.view.height).abs() < 1e-6, "{:?} vs {:?}", a.view, b.view);
    }
    // Two tiles of another layout, both turned: each goes back to Top.
    s.execute("-vports", &json!({ "option": "single" })).unwrap();
    s.execute("view.swiso", &json!({})).unwrap();
    s.execute("-vports", &json!({ "option": "2" })).unwrap();
    s.execute("viewports.reset", &json!({})).unwrap();
    assert!(s.model_tiles().iter().all(|t| t.look.dir.is_plan() && t.look.camera.is_none()));
    // One view: Top too; in a layout it is refused.
    s.execute("-vports", &json!({ "option": "single" })).unwrap();
    s.execute("view.front", &json!({})).unwrap();
    s.execute("viewports.reset", &json!({ "zoom": true })).unwrap();
    assert!(s.state().unwrap().view_dir.is_plan());
    s.execute("layout.set", &json!({ "name": "Layout1" })).unwrap();
    assert!(s.execute("viewports.reset", &json!({})).is_err());
}

/// A reset is a view change ZOOM Previous takes back, in every tile: a misclicked reset button
/// loses no tuned camera.
#[test]
fn zoom_previous_brings_back_each_tile_view_from_before_a_reset() {
    let mut s = session();
    s.execute("viewports.rhino", &json!({})).unwrap();
    // Orbit the perspective tile and make it current; turn Top to Front.
    let (pi, _) = at(&s, 0.5, 0.5);
    s.activate_tile(pi).unwrap();
    s.execute("3dorbit", &json!({ "azimuth": 70, "elevation": 35 })).unwrap();
    let tuned = s.state().unwrap().screen_dir();
    let (ti, _) = at(&s, 0.0, 0.5);
    s.activate_tile(ti).unwrap();
    s.execute("view.front", &json!({})).unwrap();
    s.activate_tile(pi).unwrap();
    s.execute("viewports.reset", &json!({})).unwrap();
    assert_ne!(s.state().unwrap().screen_dir(), tuned, "reset");
    // The current tile (Perspective) gets its orbit back...
    s.execute("zoom", &json!({ "mode": "previous" })).unwrap();
    let back = s.state().unwrap().screen_dir();
    assert!((back.dir - tuned.dir).len() < 1e-9, "{:?} vs {:?}", back.dir, tuned.dir);
    // ...and Top, made current, its Front view.
    s.activate_tile(ti).unwrap();
    assert!(s.state().unwrap().view_dir.is_plan());
    s.execute("zoom", &json!({ "mode": "previous" })).unwrap();
    assert_eq!(s.state().unwrap().view_dir, dir(0.0, -1.0, 0.0));
}
