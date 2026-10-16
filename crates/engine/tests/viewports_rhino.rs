//! Rhino-style model viewports: each tile looks along its own direction in its own visual style,
//! the current tile's being the model view's (`DocState::view_dir`), VIEWPORTS.RHINO (4VIEW),
//! VIEWPORTS.SINGLE and VIEWPORTS.MAXIMIZE.

use cadcraft_engine::geom::{Vec2, Vec3, ViewDir};
use cadcraft_engine::{ModelTile, Session};
use serde_json::{Value, json};

fn dir(x: f64, y: f64, z: f64) -> ViewDir {
    ViewDir::new(Vec3::new(x, y, z))
}

/// A 20 × 10 rectangle of lines from the origin (solids need the kernel).
fn session() -> Session {
    let mut s = Session::new();
    s.viewport_px = (1200.0, 800.0);
    s.execute("line", &json!({ "points": [[0, 0], [20, 0], [20, 10], [0, 10], [0, 0]] })).unwrap();
    s
}

fn tiles(s: &Session) -> Vec<ModelTile> {
    s.model_tiles()
}

/// The tile whose lower-left corner is at `(x, y)` (fractions of the drawing area).
fn at(s: &Session, x: f64, y: f64) -> (usize, ModelTile) {
    tiles(s).into_iter().enumerate().find(|(_, t)| t.min.near(Vec2::new(x, y), 1e-9)).unwrap()
}

fn view_dir(s: &Session) -> ViewDir {
    s.state().unwrap().view_dir
}

#[test]
fn four_rhino_views_look_their_own_way_zoomed_to_extents() {
    let mut s = session();
    let r = s.execute("viewports.rhino", &json!({})).unwrap();
    assert_eq!(r["tiles"].as_array().unwrap().len(), 4);
    let (_, top) = at(&s, 0.0, 0.5);
    let (_, persp) = at(&s, 0.5, 0.5);
    let (_, front) = at(&s, 0.0, 0.0);
    let (_, right) = at(&s, 0.5, 0.0);
    assert!(top.active && top.look.dir.is_plan(), "Top is current, in plan");
    assert_eq!(persp.look.dir, dir(-1.0, -1.0, 1.0));
    assert_eq!(front.look.dir, dir(0.0, -1.0, 0.0));
    assert_eq!(right.look.dir, dir(1.0, 0.0, 0.0));
    assert!(view_dir(&s).is_plan(), "the model view is the current tile's");
    // Each is zoomed to the extents as it sees them: centres in its own view plane.
    assert!(top.view.center.near(Vec2::new(10.0, 5.0), 1e-6), "{:?}", top.view.center);
    assert!(front.view.center.near(dir(0.0, -1.0, 0.0).project(Vec3::new(10.0, 5.0, 0.0)), 1e-6), "{:?}", front.view.center);
    assert!(right.view.center.near(dir(1.0, 0.0, 0.0).project(Vec3::new(10.0, 5.0, 0.0)), 1e-6), "{:?}", right.view.center);
    // Front sees the 20-long box edge-on: it fits its width (a 600 × 400 tile).
    assert!((front.view.height - 20.0 / 1.5 * 1.05).abs() < 1e-6, "{}", front.view.height);
    // The JSON summary carries every tile's direction and style; 4VIEW is Rhino's name for it.
    assert!(r["tiles"].as_array().unwrap().iter().all(|t| t["direction"].as_array().is_some_and(|d| d.len() == 3) && t["style"] == "2dWireframe"));
    s.execute("-vports", &json!({ "option": "single" })).unwrap();
    s.cmdline("4view").unwrap();
    assert_eq!(tiles(&s).len(), 4);
    // An undo step, like any arrangement.
    s.undo().unwrap();
    assert!(tiles(&s).is_empty());
}

#[test]
fn the_current_tile_trades_its_direction_and_style() {
    let mut s = session();
    s.execute("viewports.rhino", &json!({})).unwrap();
    let (fi, front) = at(&s, 0.0, 0.0);
    s.execute("vports.activate", &json!({ "index": fi + 1 })).unwrap();
    assert_eq!(view_dir(&s), front.look.dir);
    assert_eq!(s.state().unwrap().view(), front.view);
    // View commands act on the current tile only.
    s.execute("vscurrent", &json!({ "style": "shaded" })).unwrap();
    s.execute("3dorbit", &json!({ "azimuth": 30 })).unwrap();
    let turned = view_dir(&s);
    assert_ne!(turned, front.look.dir);
    let (ti, top) = at(&s, 0.0, 0.5);
    assert!(top.look.dir.is_plan() && !top.look.shaded, "Top keeps its look");
    s.activate_tile(ti).unwrap();
    assert!(view_dir(&s).is_plan() && !s.state().unwrap().shaded);
    let (_, front) = at(&s, 0.0, 0.0);
    assert_eq!((front.look.dir, front.look.shaded, front.look.edges), (turned, true, false), "Front keeps its orbit and shading");
    // PLAN and the presets too.
    s.execute("view.right", &json!({})).unwrap();
    assert_eq!(at(&s, 0.0, 0.5).1.look.dir, dir(1.0, 0.0, 0.0));
    assert_eq!(at(&s, 0.5, 0.0).1.look.dir, dir(1.0, 0.0, 0.0), "the Right tile is unchanged");
    assert_eq!(at(&s, 0.5, 0.5).1.look.dir, dir(-1.0, -1.0, 1.0));
}

#[test]
fn split_tiles_copy_the_current_look_and_undo_keeps_looks() {
    let mut s = session();
    s.execute("view.swiso", &json!({})).unwrap();
    s.execute("vscurrent", &json!({ "style": "shadedwithedges" })).unwrap();
    s.execute("-vports", &json!({ "option": "2" })).unwrap();
    assert!(tiles(&s).iter().all(|t| t.look.dir == dir(-1.0, -1.0, 1.0) && t.look.shaded && t.look.edges));
    s.execute("viewports.rhino", &json!({})).unwrap();
    let looks = |s: &Session| {
        let mut l: Vec<[f64; 3]> = tiles(s).iter().map(|t| [t.look.dir.dir.x, t.look.dir.dir.y, t.look.dir.dir.z]).collect();
        l.sort_by(|a, b| a.partial_cmp(b).unwrap());
        l
    };
    let rhino = looks(&s);
    // Undo and redo bring the arrangement back from the drawing; the looks come back too.
    s.execute("-vports", &json!({ "option": "single" })).unwrap();
    s.undo().unwrap();
    assert_eq!(looks(&s), rhino);
    // A named configuration keeps them as well.
    s.execute("-vports", &json!({ "option": "save", "name": "Rhino" })).unwrap();
    s.execute("-vports", &json!({ "option": "2" })).unwrap();
    s.execute("-vports", &json!({ "option": "restore", "name": "rhino" })).unwrap();
    assert_eq!(looks(&s), rhino);
}

/// Undoing a single view made while another tile than the first was current brings every tile
/// back with its own look, the one that was current current again: tile 0 doesn't take its look.
#[test]
fn undo_after_another_tile_became_current_keeps_each_tiles_look() {
    for single in [json!({ "option": "single" }), json!({})] {
        let mut s = session();
        s.execute("viewports.rhino", &json!({})).unwrap();
        let before = tiles(&s);
        let (fi, front) = at(&s, 0.0, 0.0);
        let rev = s.state().unwrap().revision;
        s.activate_tile(fi).unwrap();
        assert_eq!(s.state().unwrap().revision, rev, "making a tile current changes nothing drawn (the canvas keeps its lists)");
        let id = if single.get("option").is_some() { "-vports" } else { "viewports.single" };
        s.execute(id, &single).unwrap();
        assert!(tiles(&s).is_empty());
        s.undo().unwrap();
        let after = tiles(&s);
        assert_eq!(after.len(), 4);
        for t in &before {
            let a = after.iter().find(|a| a.min == t.min).unwrap();
            assert_eq!(a.look, t.look, "the tile at {:?} keeps its look", t.min);
        }
        let (_, f) = at(&s, 0.0, 0.0);
        assert!(f.active && f.look == front.look && view_dir(&s) == front.look.dir, "Front is current again");
        assert!(at(&s, 0.0, 0.5).1.look.dir.is_plan(), "Top is in plan");
    }
}

#[test]
fn maximize_shows_the_current_tile_alone_and_brings_the_others_back() {
    let mut s = session();
    // One view and nothing to bring back: the four Rhino views.
    s.execute("viewports.maximize", &json!({})).unwrap();
    assert_eq!(tiles(&s).len(), 4);
    let (pi, persp) = at(&s, 0.5, 0.5);
    s.activate_tile(pi).unwrap();
    s.execute("view.set", &json!({ "center": [1, 2], "height": 30 })).unwrap();
    let before = tiles(&s);
    s.execute("viewports.maximize", &json!({})).unwrap();
    assert!(tiles(&s).is_empty());
    assert_eq!(view_dir(&s), persp.look.dir, "the maximized view keeps its direction");
    assert_eq!(s.state().unwrap().view().center, Vec2::new(1.0, 2.0));
    // Zoomed while maximized, then restored: the other tiles as they were, this one as it is.
    s.execute("view.set", &json!({ "center": [3, 4], "height": 10 })).unwrap();
    s.execute("viewports.maximize", &json!({})).unwrap();
    let after = tiles(&s);
    assert_eq!(after.len(), 4);
    let (_, p) = at(&s, 0.5, 0.5);
    assert!(p.active && p.view.center == Vec2::new(3.0, 4.0) && p.look == persp.look);
    for t in before.iter().filter(|t| !t.active) {
        assert!(after.iter().any(|a| a.min == t.min && a.view == t.view && a.look == t.look), "{t:?}");
    }
    // VIEWPORTS.SINGLE with one view changes nothing.
    s.execute("viewports.single", &json!({})).unwrap();
    s.execute("viewports.single", &json!({})).unwrap();
    assert!(tiles(&s).is_empty());
    s.execute("viewports.maximize", &json!({})).unwrap();
    assert_eq!(tiles(&s).len(), 4, "still remembered");
}

#[test]
fn layouts_refuse_and_hostile_params_never_panic() {
    let mut s = session();
    s.execute("layout.set", &json!({ "name": "Layout1" })).unwrap();
    for id in ["viewports.rhino", "viewports.single", "viewports.maximize"] {
        assert!(s.execute(id, &json!({})).is_err(), "{id} in a layout");
    }
    s.execute("layout.set", &json!({ "name": "Model" })).unwrap();
    let hostile = [json!(null), json!([1, 2]), json!("x"), json!({ "index": 1e308 }), json!({ "anything": { "deep": [null] } })];
    for id in ["viewports.rhino", "viewports.single", "viewports.maximize"] {
        for p in &hostile {
            let _: Option<Value> = s.execute(id, p).ok();
        }
    }
    // An empty drawing fits its limits as each view sees them.
    let mut e = Session::new();
    e.viewport_px = (800.0, 800.0);
    e.execute("viewports.rhino", &json!({})).unwrap();
    assert!(tiles(&e).iter().all(|t| t.view.center.is_finite() && t.view.height > 0.0));
}
