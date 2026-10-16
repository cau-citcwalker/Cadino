//! 3D views on the Model tab: ZOOM, PAN, named views and ZOOM Previous take world points into
//! the view plane, 3DORBIT turns about the screen centre, and layouts stay in plan.

use cadcraft_engine::Session;
use cadcraft_engine::doc::Space;
use cadcraft_engine::geom::{Bounds2, Vec2, Vec3, ViewDir};
use serde_json::json;

fn session() -> Session {
    let mut s = Session::new();
    s.new_drawing(true);
    s.viewport_px = (1000.0, 1000.0);
    s
}

fn dir(s: &Session) -> ViewDir {
    s.state().unwrap().view_dir
}

fn center(s: &Session) -> Vec2 {
    s.state().unwrap().view().center
}

#[test]
fn zoom_and_pan_take_world_points_into_the_view_plane() {
    let mut s = session();
    s.execute("line", &json!({ "points": [[100, 0], [110, 10]] })).unwrap();
    s.execute("view.neiso", &json!({})).unwrap();
    let p = dir(&s).plane_xf();
    let mid = p.apply(Vec2::new(105.0, 5.0));

    s.execute("zoom", &json!({ "mode": "window", "p1": [100, 0], "p2": [110, 10] })).unwrap();
    assert!(center(&s).near(mid, 1e-9), "{:?} vs {mid:?}", center(&s));

    s.execute("zoom", &json!({ "mode": "center", "center": [0, 0], "height": 5 })).unwrap();
    assert!(center(&s).near(Vec2::ZERO, 1e-9));
    s.execute("zoom", &json!({ "mode": "object", "handles": [s.doc().unwrap().model.iter().next().unwrap().handle.hex()] })).unwrap();
    assert!(center(&s).near(mid, 1e-9), "{:?}", center(&s));

    let before = center(&s);
    s.execute("pan", &json!({ "from": [0, 0], "to": [10, 0] })).unwrap();
    let moved = before - center(&s);
    assert!(moved.near(p.apply(Vec2::new(10.0, 0.0)), 1e-9), "{moved:?}");

    // All: the line and the limits, as the view shows them.
    s.execute("zoom", &json!({ "mode": "all" })).unwrap();
    let lim = s.zoom_limits().unwrap().union(&s.view_extents(&Space::Model).unwrap());
    assert!(center(&s).near(lim.center(), 1e-6), "{:?} vs {:?}", center(&s), lim.center());
}

#[test]
fn zoom_previous_returns_to_the_direction_before() {
    let mut s = session();
    s.execute("line", &json!({ "points": [[0, 0], [10, 10]] })).unwrap();
    s.execute("zoom", &json!({ "mode": "extents" })).unwrap();
    let plan = s.state().unwrap().view();
    s.execute("vpoint", &json!({ "direction": [1, 1, 1] })).unwrap();
    s.execute("zoom", &json!({ "mode": "previous" })).unwrap();
    assert!(dir(&s).is_plan());
    assert_eq!(s.state().unwrap().view(), plan);
}

#[test]
fn an_empty_drawing_in_3d_centres_its_projected_limits() {
    let mut s = session();
    s.execute("view.swiso", &json!({})).unwrap();
    let h = &s.doc().unwrap().header;
    let (lo, hi) = (h.point("LIMMIN").map_or(Vec2::ZERO, |p| p.xy()), h.point("LIMMAX").map_or(Vec2::new(12.0, 9.0), |p| p.xy()));
    let m = dir(&s).plane_xf();
    let lim = Bounds2::from_points([lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)].map(|c| m.apply(c)));
    assert!(center(&s).near(lim.center(), 1e-6), "{:?} vs {:?}", center(&s), lim.center());
}

#[test]
fn text_counts_in_3d_extents() {
    let mut s = session();
    s.execute("text", &json!({ "at": [50, 50], "height": 5, "text": "TITLE" })).unwrap();
    s.execute("view.swiso", &json!({})).unwrap();
    assert!(!s.view_extents(&Space::Model).unwrap().is_empty());
}

#[test]
fn orbit_keeps_the_centre_and_a_zero_orbit_changes_nothing() {
    let mut s = session();
    s.execute("view.front", &json!({})).unwrap();
    s.execute("view.set", &json!({ "center": [0, 100], "height": 50 })).unwrap();
    let (d0, c0) = (dir(&s), center(&s));
    s.execute("3dorbit", &json!({})).unwrap();
    assert_eq!((dir(&s), center(&s)), (d0, c0));
    // Edge-on: the view tilts about the point at the centre instead of jumping to z = 0.
    cadcraft_engine::orbit(&mut s, 0.0, 5.0).unwrap();
    assert!((center(&s).y - 100.0).abs() < 1.0, "{:?}", center(&s));
    cadcraft_engine::orbit(&mut s, 5.0, 0.0).unwrap();
    assert!(center(&s).is_finite() && (center(&s).y - 100.0).abs() < 1.0, "{:?}", center(&s));
}

#[test]
fn orbiting_from_plan_keeps_x_to_the_right() {
    let mut s = session();
    s.execute("3dorbit", &json!({})).unwrap();
    assert!(dir(&s).is_plan(), "a bare 3DORBIT leaves plan alone");
    cadcraft_engine::orbit(&mut s, 0.0, -1.0).unwrap();
    let (x, _, _) = dir(&s).axes();
    assert!(x.dot(Vec3::new(1.0, 0.0, 0.0)) > 0.999, "{x:?}");
}

#[test]
fn layouts_and_their_viewports_stay_in_plan() {
    let mut s = session();
    s.execute("line", &json!({ "points": [[0, 0], [100, 50]] })).unwrap();
    s.execute("view.swiso", &json!({})).unwrap();
    let iso = dir(&s);
    s.execute("layout.set", &json!({ "name": "Layout1" })).unwrap();
    let r = s.execute("mview", &json!({ "p1": [20, 20], "p2": [120, 100] })).unwrap();
    let h = r["viewports"][0].as_str().unwrap().to_string();
    s.execute("mspace", &json!({ "handle": h })).unwrap();
    assert!(s.execute("vpoint", &json!({ "preset": "top" })).is_err());
    assert!(cadcraft_engine::orbit(&mut s, 10.0, 10.0).is_err());
    assert_eq!(dir(&s), iso, "the Model tab keeps its view");
    // Extents inside the viewport are plan extents.
    assert_eq!(s.view_extents(&Space::Model).unwrap(), s.doc().unwrap().extents(&Space::Model));
    s.execute("zoom", &json!({ "mode": "extents" })).unwrap();
    assert!(s.state().unwrap().zoom_frame().center.near(Vec2::new(50.0, 25.0), 1e-6));
}

#[test]
fn named_views_restore_into_the_current_direction() {
    let mut s = session();
    s.execute("line", &json!({ "points": [[0, 0], [10, 10]] })).unwrap();
    s.execute("view.seiso", &json!({})).unwrap();
    s.execute("view.set", &json!({ "center": [3, 4], "height": 20 })).unwrap();
    s.execute("view", &json!({ "option": "save", "name": "iso" })).unwrap();
    let world = s.doc().unwrap().views.iter().find(|v| v.name == "iso").unwrap().center;
    // Kept as the world point at the centre, which plan shows as is.
    assert!(dir(&s).plane_xf().apply(world).near(Vec2::new(3.0, 4.0), 1e-9));
    s.execute("plan", &json!({})).unwrap();
    s.execute("view", &json!({ "option": "restore", "name": "iso" })).unwrap();
    assert!(center(&s).near(world, 1e-9));
    s.execute("view.seiso", &json!({})).unwrap();
    s.execute("view", &json!({ "option": "restore", "name": "iso" })).unwrap();
    assert!(center(&s).near(Vec2::new(3.0, 4.0), 1e-9), "{:?}", center(&s));
}

#[test]
fn closing_the_block_editor_restores_the_view_direction() {
    let mut s = session();
    let l = s.execute("line", &json!({ "points": [[0, 0], [1, 0]] })).unwrap()["handles"][0].clone();
    s.execute("block", &json!({ "name": "B", "base": [0, 0], "handles": [l] })).unwrap();
    s.execute("bedit", &json!({ "name": "B" })).unwrap();
    s.execute("view.swiso", &json!({})).unwrap();
    s.execute("bclose", &json!({})).unwrap();
    assert!(dir(&s).is_plan());
}

/// A point typed as `x,y,z` keeps its height for the commands that build in 3D (`point_z`);
/// `@dx,dy,dz` adds to the last point's, also across command lines and in scripts; `x,y` is on
/// the XY plane.
#[test]
fn typed_points_keep_their_height() {
    let mut s = session();
    s.cmdline("line").unwrap();
    s.cmdline("1,2,5").unwrap();
    assert_eq!((s.last_point, s.point_z), (Vec2::new(1.0, 2.0), Some(5.0)));
    s.cmdline("@10,0,-2").unwrap();
    assert_eq!((s.last_point, s.point_z), (Vec2::new(11.0, 2.0), Some(3.0)));
    s.cmdline("@0,1").unwrap();
    assert_eq!(s.point_z, Some(3.0), "a relative 2D point keeps the last height");
    s.cmdline("4,4").unwrap();
    assert_eq!(s.point_z, None);
    s.cancel();
    s.script("LINE 0,0,7 @1,1,1\n").unwrap();
    assert_eq!((s.last_point, s.point_z), (Vec2::new(1.0, 1.0), Some(8.0)));
    // A hostile height is not a point.
    s.cancel();
    s.cmdline("line").unwrap();
    s.cmdline("5,5").unwrap();
    let _ = s.cmdline("1,1,1e999");
    assert_eq!((s.last_point, s.point_z), (Vec2::new(5.0, 5.0), None), "unchanged");
}
