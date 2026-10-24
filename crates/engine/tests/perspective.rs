//! True perspective views: Rhino's Perspective tile, PERSPECTIVE and LENSLENGTH, zooming and
//! panning (the eye dollies and moves with the target), orbiting about the target, zoom to
//! extents, and the camera saved with the tiles.

use std::sync::Mutex;

use cadcraft_engine::cmd::file::{IoHooks, set_io};
use cadcraft_engine::geom::{Camera, Vec2, Vec3, ViewDir};
use cadcraft_engine::{ModelTile, Session, View};
use serde_json::json;

/// A 600 × 400 × 700 cabinet's outline in lines (solids need the kernel).
fn session() -> Session {
    let mut s = Session::new();
    s.viewport_px = (1200.0, 800.0);
    s.execute("line", &json!({ "points": [[0, 0], [600, 0], [600, 400], [0, 400], [0, 0]] })).unwrap();
    s.execute("3dpoly", &json!({ "points": [[0, 0, 0], [0, 0, 700], [600, 0, 700], [600, 400, 700], [0, 400, 700], [0, 0, 700]] })).unwrap();
    s
}

fn at(s: &Session, x: f64, y: f64) -> (usize, ModelTile) {
    s.model_tiles().into_iter().enumerate().find(|(_, t)| t.min.near(Vec2::new(x, y), 1e-9)).unwrap()
}

fn corners() -> Vec<Vec3> {
    (0..8)
        .map(|i| Vec3::new(if i & 1 == 0 { 0.0 } else { 600.0 }, if i & 2 == 0 { 0.0 } else { 400.0 }, if i & 4 == 0 { 0.0 } else { 700.0 }))
        .collect()
}

/// Every corner of the cabinet is in view `v` seen through `dir` in an area `aspect` wide.
fn all_in_view(dir: ViewDir, v: View, aspect: f64) -> bool {
    corners().into_iter().all(|p| {
        let q = dir.project(p) - v.center;
        dir.sees(p) && q.y.abs() <= v.height / 2.0 && q.x.abs() <= v.height * aspect / 2.0
    })
}

#[test]
fn rhinos_perspective_tile_is_a_real_perspective() {
    let mut s = session();
    s.execute("viewports.rhino", &json!({})).unwrap();
    let (pi, persp) = at(&s, 0.5, 0.5);
    let cam = persp.look.camera.expect("Perspective has a camera");
    assert_eq!(cam.lens, Camera::LENS);
    let proj = persp.look.projection(persp.view);
    assert!(proj.persp.is_some() && !proj.is_plan());
    assert!(all_in_view(proj, persp.view, 1.5), "zoomed to the model's extents");
    // The others are parallel.
    assert!(s.model_tiles().iter().filter(|t| t.min != persp.min).all(|t| t.look.camera.is_none()));
    // Made current, it is the model view: PERSPECTIVE 1, and 0 makes it parallel.
    s.activate_tile(pi).unwrap();
    assert_eq!(s.execute("getvar", &json!({ "name": "PERSPECTIVE" })).unwrap()["value"], json!(1));
    assert!(s.state().unwrap().screen_dir().persp.is_some());
    s.execute("setvar", &json!({ "name": "PERSPECTIVE", "value": 0 })).unwrap();
    assert!(s.state().unwrap().screen_dir().persp.is_none());
    s.cmdline("PERSPECTIVE").unwrap();
    s.cmdline("1").unwrap();
    assert!(s.state().unwrap().camera.is_some(), "typed PERSPECTIVE 1");
    // PLAN turns it off (plan is the 2D drawing area); another preset keeps it.
    s.execute("view.seiso", &json!({})).unwrap();
    assert!(s.state().unwrap().camera.is_some());
    s.execute("plan", &json!({})).unwrap();
    assert!(s.state().unwrap().camera.is_none() && s.state().unwrap().screen_dir().is_plan());
    // LENSLENGTH is read-only, as in AutoCAD: DVIEW Zoom sets it.
    assert!(s.execute("setvar", &json!({ "name": "LENSLENGTH", "value": 35 })).is_err());
    assert!(s.execute("setvar", &json!({ "name": "PERSPECTIVE", "value": "x" })).is_err());
}

#[test]
fn zooming_dollies_the_eye_and_panning_moves_it_with_the_target() {
    let mut s = session();
    s.execute("view.swiso", &json!({})).unwrap();
    s.execute("setvar", &json!({ "name": "PERSPECTIVE", "value": 1 })).unwrap();
    let st = s.state().unwrap();
    let (dir, view, cam) = (st.screen_dir(), st.view(), st.camera.unwrap());
    let target = dir.world_of(view.center.to3(cam.target));
    let eye = dir.eye().unwrap();
    // The wheel zooms in about the target: the eye halves its distance along the line of sight.
    s.zoom_about(2.0, view.center).unwrap();
    let st = s.state().unwrap();
    let eye2 = st.screen_dir().eye().unwrap();
    assert!(((eye2 - target).len() - (eye - target).len() / 2.0).abs() < 1e-6);
    assert!(((eye2 - target).normalized() - (eye - target).normalized()).len() < 1e-9);
    // A pan moves the eye across, keeping the direction and distance.
    let v = st.view();
    s.state_mut().unwrap().set_view_quiet(View { center: v.center + Vec2::new(100.0, 0.0), height: v.height });
    let eye3 = s.state().unwrap().screen_dir().eye().unwrap();
    assert!(((eye3 - eye2).len() - 100.0).abs() < 1e-6);
    // Zoom to extents backs off until the whole model is in view.
    s.zoom_extents().unwrap();
    let st = s.state().unwrap();
    assert!(all_in_view(st.screen_dir(), st.view(), 1.5));
}

#[test]
fn orbiting_in_perspective_turns_about_the_target() {
    let mut s = session();
    s.execute("view.swiso", &json!({})).unwrap();
    s.execute("setvar", &json!({ "name": "PERSPECTIVE", "value": 1 })).unwrap();
    let st = s.state().unwrap();
    let (dir, view, cam) = (st.screen_dir(), st.view(), st.camera.unwrap());
    let target = dir.world_of(view.center.to3(cam.target));
    let dist = (dir.eye().unwrap() - target).len();
    s.execute("3dorbit", &json!({ "azimuth": 30, "elevation": 10 })).unwrap();
    let st = s.state().unwrap();
    let d = st.screen_dir();
    // The target stays at the centre of the screen, and the eye as far from it.
    assert!(d.project(target).near(st.view().center, 1e-6));
    assert!(((d.eye().unwrap() - target).len() - dist).abs() < 1e-6);
    assert!((d.dir - dir.dir).len() > 0.1, "turned");
}

static SAVED: Mutex<Vec<Option<Camera>>> = Mutex::new(Vec::new());

/// The cameras go into the drawing's tiles, so a save keeps them (the DXF side is
/// `cadcraft-io`'s `perspective_vport` test).
#[test]
fn a_saved_drawing_keeps_its_perspective_tile() {
    set_io(IoHooks {
        read: |_, _| Err("no reader in this test".into()),
        write: |d, _| {
            let cams = d.vports.iter().find(|c| c.is_active()).map(|c| c.tiles.iter().map(|t| t.camera).collect()).unwrap_or_default();
            if let Ok(mut s) = SAVED.lock() {
                *s = cams;
            }
            Ok(b"0\nEOF\n".to_vec())
        },
        plot: None,
    });
    let mut s = session();
    s.execute("viewports.rhino", &json!({})).unwrap();
    let (pi, _) = at(&s, 0.5, 0.5);
    s.activate_tile(pi).unwrap();
    s.execute("dview", &json!({ "lens": 35 })).unwrap();
    let path = std::env::temp_dir().join(format!("cadino-persp-{}.dxf", std::process::id()));
    s.execute("saveas", &json!({ "path": path.to_string_lossy() })).unwrap();
    let _ = std::fs::remove_file(&path);
    let saved = SAVED.lock().unwrap().clone();
    assert_eq!(saved.len(), 4);
    assert_eq!(saved[0].map(|c| c.lens), Some(35.0), "the current Perspective tile first, with its lens");
    assert!(saved[1..].iter().all(Option::is_none));
}

/// Zoom to extents in perspective frames what block references hold too: a 2000 high cabinet
/// inside a block stays in view (its box isn't the flat footprint of the reference).
#[test]
fn zoom_extents_in_perspective_sees_the_height_of_block_contents() {
    let mut s = Session::new();
    s.viewport_px = (1200.0, 800.0);
    s.execute("line", &json!({ "points": [[0, 0], [600, 0], [600, 400], [0, 400], [0, 0]] })).unwrap();
    s.execute("3dpoly", &json!({ "points": [[0, 0, 0], [0, 0, 2000], [600, 0, 2000], [600, 400, 2000], [0, 400, 2000], [0, 400, 0]] })).unwrap();
    let handles: Vec<_> = s.doc().unwrap().model.iter().map(|e| e.handle.0).collect();
    s.execute("block", &json!({ "name": "TALL", "base": [0, 0], "handles": handles })).unwrap();
    assert_eq!(s.doc().unwrap().model.len(), 1, "one reference");
    s.execute("view.swiso", &json!({})).unwrap();
    s.execute("setvar", &json!({ "name": "PERSPECTIVE", "value": 1 })).unwrap();
    s.execute("zoom", &json!({ "mode": "extents" })).unwrap();
    let st = s.state().unwrap();
    let (dir, v) = (st.screen_dir(), st.view());
    for i in 0..8 {
        let p = Vec3::new(if i & 1 == 0 { 0.0 } else { 600.0 }, if i & 2 == 0 { 0.0 } else { 400.0 }, if i & 4 == 0 { 0.0 } else { 2000.0 });
        let q = dir.project(p) - v.center;
        assert!(dir.sees(p) && q.y.abs() <= v.height / 2.0 && q.x.abs() <= v.height * 1.5 / 2.0, "{p:?} out of view at {q:?}");
    }
}

/// Closing the Block Editor brings back the drawing's camera, whatever the editor's view did.
#[test]
fn closing_the_block_editor_keeps_the_drawings_camera() {
    let mut s = session();
    s.execute("view.swiso", &json!({})).unwrap();
    s.execute("setvar", &json!({ "name": "PERSPECTIVE", "value": 1 })).unwrap();
    let before = s.state().unwrap().camera;
    assert!(before.is_some());
    s.execute("bedit", &json!({ "name": "DOOR", "create": true })).unwrap();
    s.execute("view.top", &json!({})).unwrap();
    assert!(s.state().unwrap().camera.is_none());
    s.execute("bclose", &json!({ "save": false })).unwrap();
    assert_eq!(s.state().unwrap().camera, before);
}

/// ZOOM Previous after a preset in perspective brings the eye back where it was.
#[test]
fn zoom_previous_brings_back_the_camera() {
    let mut s = session();
    s.execute("view.swiso", &json!({})).unwrap();
    s.execute("setvar", &json!({ "name": "PERSPECTIVE", "value": 1 })).unwrap();
    s.execute("zoom", &json!({ "mode": "extents" })).unwrap();
    let eye = s.state().unwrap().screen_dir().eye().unwrap();
    s.execute("view.neiso", &json!({})).unwrap();
    s.execute("zoom", &json!({ "mode": "previous" })).unwrap();
    let st = s.state().unwrap();
    assert!(st.camera.is_some());
    let back = st.screen_dir().eye().unwrap();
    assert!((back - eye).len() < 1e-6 * (1.0 + eye.len()), "{back:?} vs {eye:?}");
}

/// In perspective, ZOOM Window's band runs through the picked corners as the screen shows them,
/// and the window ends up in view.
#[test]
fn zoom_window_in_perspective_follows_the_cursor_and_frames_the_window() {
    let mut s = session();
    s.execute("view.swiso", &json!({})).unwrap();
    s.execute("setvar", &json!({ "name": "PERSPECTIVE", "value": 1 })).unwrap();
    s.execute("zoom", &json!({ "mode": "extents" })).unwrap();
    s.cmdline("ZOOM").unwrap();
    s.cmdline("W").unwrap();
    s.cmdline("100,50").unwrap();
    let dir = s.state().unwrap().screen_dir();
    let cursor = Vec2::new(500.0, 350.0);
    let band = s.preview(cursor);
    let corner = dir.project(cursor.to3(0.0));
    let on_band = band.iter().any(|e| match &e.kind {
        cadcraft_engine::doc::EntityKind::LwPolyline(p) => {
            p.vertices.iter().any(|v| dir.project(v.p.to3(0.0)).dist(corner) < 1e-6 * (1.0 + corner.len()))
        }
        _ => false,
    });
    assert!(on_band, "the band's corner is under the cursor");
    s.cmdline("500,350").unwrap();
    let st = s.state().unwrap();
    let (dir, v) = (st.screen_dir(), st.view());
    for p in [Vec2::new(100.0, 50.0), Vec2::new(500.0, 350.0)] {
        let q = dir.project(p.to3(0.0)) - v.center;
        assert!(q.y.abs() <= v.height / 2.0 && q.x.abs() <= v.height * 1.5 / 2.0, "{p:?} out of view at {q:?}");
    }
}
