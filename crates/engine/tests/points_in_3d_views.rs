//! Points in 3D views: the engine's object snaps leave 3D solids to the UI there (their edges in
//! plan are not where they show), and typed points are on the XY plane whatever height the last
//! 3D snap gave (`Session::point_z`).

use std::sync::Arc;

use cadcraft_engine::Session;
use cadcraft_engine::doc::{EntityKind, Solid3d};
use cadcraft_engine::geom::{Mesh3, Vec2, Vec3};
use cadcraft_engine::snap::tracking::CursorQuery;
use serde_json::json;

fn snapped(s: &mut Session, at: Vec2) -> Option<Vec2> {
    let q = CursorQuery { base: None, deferred: false, corner: false, aperture: 0.5, tol: 0.1, now: 0.0, shift: false };
    s.snap_cursor(at, &q).snap.map(|h| h.point)
}

#[test]
fn engine_snaps_skip_solids_in_3d_views_only() {
    let mut s = Session::new();
    // A solid's top edge, 10 up: in plan its end is at (10, 0).
    let mesh = Mesh3 { edges: vec![vec![Vec3::new(0.0, 0.0, 10.0), Vec3::new(10.0, 0.0, 10.0)]], ..Default::default() };
    s.add_entity(EntityKind::Solid3d(Solid3d { mesh: Arc::new(mesh), ..Default::default() })).unwrap();
    s.execute("line", &json!({ "points": [[20, 0], [30, 0]] })).unwrap();
    s.execute("osnap", &json!({ "modes": ["end"] })).unwrap();
    assert_eq!(snapped(&mut s, Vec2::new(10.1, 0.1)), Some(Vec2::new(10.0, 0.0)), "plan view snaps to the solid");
    s.execute("view.swiso", &json!({})).unwrap();
    assert_eq!(snapped(&mut s, Vec2::new(10.1, 0.1)), None, "no shadow snap in a 3D view");
    assert_eq!(snapped(&mut s, Vec2::new(30.1, 0.1)), Some(Vec2::new(30.0, 0.0)), "2D objects still snap");
    s.execute("plan", &json!({})).unwrap();
    assert_eq!(snapped(&mut s, Vec2::new(10.1, 0.1)), Some(Vec2::new(10.0, 0.0)));
}

#[test]
fn snap_overrides_and_tab_candidates_skip_solids_with_the_flag() {
    use cadcraft_engine::doc::Space;
    use cadcraft_engine::snap::mode;
    let mut s = Session::new();
    let mesh = Mesh3 { edges: vec![vec![Vec3::new(0.0, 0.0, 10.0), Vec3::new(10.0, 0.0, 10.0)]], ..Default::default() };
    s.add_entity(EntityKind::Solid3d(Solid3d { mesh: Arc::new(mesh), ..Default::default() })).unwrap();
    let d = s.doc().unwrap();
    let at = Vec2::new(10.1, 0.1);
    let with = cadcraft_engine::pointmod::snap_candidates(d, &Space::Model, at, 0.5, mode::END, None, false);
    let without = cadcraft_engine::pointmod::snap_candidates(d, &Space::Model, at, 0.5, mode::END | mode::NO_SOLIDS, None, false);
    assert_eq!((with.len(), without.len()), (1, 0));
}

#[test]
fn typed_points_are_on_the_xy_plane() {
    let mut s = Session::new();
    s.point_z = Some(20.0);
    s.cmdline("box").unwrap();
    s.cmdline("0,0").unwrap();
    assert_eq!(s.point_z, None);
    s.cancel();
    s.point_z = Some(20.0);
    s.script("box\n0,0\n").unwrap();
    assert_eq!(s.point_z, None);
    s.cancel();
}

#[cfg(feature = "occt")]
#[test]
fn a_typed_box_corner_after_a_3d_snap_is_at_z_0() {
    let mut s = Session::new();
    // The last click was on a 3D object 20 up; the corner is typed.
    s.point_z = Some(20.0);
    for t in ["box", "0,0", "4,4", "2"] {
        s.cmdline(t).unwrap();
    }
    let z: Vec<f64> = s
        .doc()
        .unwrap()
        .model
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Solid3d(x) => Some(x.mesh.verts.iter().map(|p| x.world(*p).z).collect::<Vec<_>>()),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(!z.is_empty());
    assert!(z.iter().all(|z| (-1e-9..=2.0 + 1e-9).contains(z)), "{z:?}");
}
