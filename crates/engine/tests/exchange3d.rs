//! IGESOUT, IGESIN and RHINOOUT with the OpenCASCADE kernel (run in this process).
#![cfg(feature = "occt")]

use std::path::PathBuf;

use cadcraft_engine::Session;
use cadcraft_engine::doc::{EntityKind, Line, Solid3d};
use cadcraft_engine::geom::Vec3;
use serde_json::json;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cadrhino-exchange3d-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn session() -> Session {
    let mut s = Session::new();
    s.new_drawing(true);
    s
}

fn kinds(s: &Session) -> Vec<EntityKind> {
    s.doc().unwrap().model.iter().map(|e| e.kind.clone()).collect()
}

fn solids(s: &Session) -> Vec<Solid3d> {
    kinds(s)
        .into_iter()
        .filter_map(|k| match k {
            EntityKind::Solid3d(x) => Some(x),
            _ => None,
        })
        .collect()
}

fn polylines(s: &Session) -> Vec<(Vec<Vec3>, bool)> {
    kinds(s)
        .into_iter()
        .filter_map(|k| match k {
            EntityKind::Polyline3d(p) => Some((p.points, p.closed)),
            _ => None,
        })
        .collect()
}

/// The solid's corners in the world.
fn bounds(x: &Solid3d) -> (Vec3, Vec3) {
    let v = Vec3::new;
    x.mesh.verts.iter().map(|p| x.world(*p)).fold((v(f64::MAX, f64::MAX, f64::MAX), v(f64::MIN, f64::MIN, f64::MIN)), |(lo, hi), p| {
        (v(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), v(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
    })
}

fn near(a: Vec3, b: Vec3) -> bool {
    (a - b).len() < 1e-6
}

/// A cabinet side, a line and an arc.
fn drawing() -> Session {
    let mut s = session();
    s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [600, 18], "height": 720 })).unwrap();
    s.execute("line", &json!({ "points": [[0, -50], [600, -50]] })).unwrap();
    s.execute("arc", &json!({ "p1": [0, -100], "p2": [300, -200], "p3": [600, -100] })).unwrap();
    s
}

#[test]
fn iges_round_trips_a_box_a_line_and_an_arc() {
    let mut s = drawing();
    // A zero-length line the kernel can't make is left out, not an error.
    s.add_entity(EntityKind::Line(Line { a: Vec3::new(5.0, 5.0, 0.0), b: Vec3::new(5.0, 5.0, 0.0) })).unwrap();
    let path = temp("round.igs");
    let r = s.execute("igesout", &json!({ "path": path.to_str().unwrap() })).unwrap();
    assert_eq!((r["solids"].as_u64(), r["curves"].as_u64(), r["skipped"].as_u64()), (Some(1), Some(2), Some(1)), "{r}");
    assert!(r["message"].as_str().unwrap().contains("1 solid and 2 curves"), "{r}");

    let mut t = session();
    t.execute("layer.new", &json!({ "name": "Imported", "current": true })).unwrap();
    let r = t.execute("igesin", &json!({ "path": path.to_str().unwrap() })).unwrap();
    assert_eq!((r["solids"].as_u64(), r["curves"].as_u64()), (Some(1), Some(2)), "{r}");
    let xs = solids(&t);
    assert_eq!(xs.len(), 1);
    let b = bounds(&xs[0]);
    assert!(near(b.0, Vec3::ZERO) && near(b.1, Vec3::new(600.0, 18.0, 720.0)), "{b:?}");
    assert!(!xs[0].mesh.open && xs[0].brep.contains("CASCADE Topology"));
    let ps = polylines(&t);
    assert_eq!(ps.len(), 2, "{ps:?}");
    let line = ps.iter().find(|(p, _)| p.len() == 2).expect("the line");
    assert!(near(line.0[0], Vec3::new(0.0, -50.0, 0.0)) || near(line.0[1], Vec3::new(0.0, -50.0, 0.0)), "{line:?}");
    let arc = ps.iter().find(|(p, _)| p.len() > 2).expect("the arc");
    assert!(!arc.1 && arc.0.iter().all(|p| p.y <= -100.0 + 1e-6 && p.y >= -200.0 - 1e-6), "{arc:?}");
    // On the current layer, and one undo step.
    assert!(t.doc().unwrap().model.iter().all(|e| e.common.layer == "Imported"));
    t.undo().unwrap();
    assert!(kinds(&t).is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn turned_solids_and_closed_outlines_come_back_as_they_were() {
    let mut s = session();
    let h = s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [10, 20], "height": 30 })).unwrap();
    // A quarter turn about X (as 3DROTATE makes), then moved: the 20-long side stands up Z.
    let h = cadcraft_engine::doc::Handle::parse_hex(h["handle"].as_str().unwrap()).unwrap();
    s.doc_mut()
        .unwrap()
        .modify_entity(h, |e| {
            if let EntityKind::Solid3d(x) = &mut e.kind {
                x.xform.m = [[1.0, 0.0, 0.0, 100.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
            }
        })
        .unwrap();
    s.execute("rectang", &json!({ "p1": [0, -40], "p2": [50, -60] })).unwrap();
    let path = temp("turned.igs");
    s.execute("igesout", &json!({ "path": path.to_str().unwrap() })).unwrap();
    let mut t = session();
    t.execute("igesin", &json!({ "path": path.to_str().unwrap() })).unwrap();
    let b = bounds(&solids(&t)[0]);
    assert!(near(b.0, Vec3::new(100.0, -30.0, 0.0)) && near(b.1, Vec3::new(110.0, 0.0, 20.0)), "{b:?}");
    let ps = polylines(&t);
    assert!(matches!(ps.as_slice(), [(p, true)] if p.len() == 4), "the rectangle, closed: {ps:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn rhinoout_writes_an_iges_and_a_step_file() {
    let mut s = drawing();
    let base = temp("cabinet");
    let r = s.execute("rhinoout", &json!({ "path": format!("{}.3dm", base.display()) })).unwrap();
    let (igs, stp) = (base.with_extension("igs"), base.with_extension("stp"));
    assert!(igs.is_file() && stp.is_file(), "{r}");
    let m = r["message"].as_str().unwrap();
    assert!(m.contains(&igs.display().to_string()) && m.contains(&stp.display().to_string()), "{m}");
    // The STEP file keeps the solid; Rhino opens either.
    let mut t = session();
    t.execute("stepin", &json!({ "path": stp.to_str().unwrap() })).unwrap();
    assert_eq!(solids(&t).len(), 1);
    // Curves only: the IGES file and no STEP file.
    let mut c = session();
    c.execute("line", &json!({ "points": [[0, 0], [1, 1]] })).unwrap();
    let only = temp("lines");
    let r = c.execute("rhinoout", &json!({ "path": only.to_str().unwrap() })).unwrap();
    assert!(only.with_extension("igs").is_file() && !only.with_extension("stp").exists() && r["step"].is_null(), "{r}");
    for p in [igs, stp, only.with_extension("igs")] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn the_interactive_forms_ask_for_the_file() {
    let mut s = drawing();
    let path = temp("asked.igs");
    s.cmdline("igesout").unwrap();
    s.input(cadcraft_engine::Input::Text(format!("\"{}\"", path.display()))).unwrap();
    assert!(s.running.is_none() && path.is_file());
    let mut t = session();
    t.cmdline("igesin").unwrap();
    t.input(cadcraft_engine::Input::Text(path.display().to_string())).unwrap();
    assert_eq!(solids(&t).len(), 1);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn bad_files_leave_the_drawing_as_it_was() {
    let mut s = drawing();
    let before = kinds(&s).len();
    let junk = temp("junk.igs");
    std::fs::write(&junk, "garbage, not IGES").unwrap();
    assert!(s.execute("igesin", &json!({ "path": junk.to_str().unwrap() })).is_err());
    assert!(s.execute("igesin", &json!({ "path": temp("missing.igs").to_str().unwrap() })).is_err());
    assert_eq!(kinds(&s).len(), before);
    let _ = std::fs::remove_file(&junk);
}

/// Two cabinets side by side come back as two closed solids; a circle standing in the XZ plane
/// comes back standing there.
#[test]
fn touching_cabinets_and_a_standing_circle_round_trip() {
    let mut s = session();
    for x in [0, 600] {
        s.execute("box", &json!({ "corner1": [x, 0, 0], "corner2": [x + 600, 560], "height": 720 })).unwrap();
    }
    let c = s.execute("circle", &json!({ "center": [300, 360], "radius": 100 })).unwrap()["handle"].clone();
    let c = cadcraft_engine::doc::Handle::parse_hex(c.as_str().unwrap()).unwrap();
    s.doc_mut().unwrap().modify_entity(c, |e| e.common.extrusion = Vec3::new(0.0, -1.0, 0.0)).unwrap();
    let path = temp("cabinets.igs");
    s.execute("igesout", &json!({ "path": path.to_str().unwrap() })).unwrap();
    let mut t = session();
    let r = t.execute("igesin", &json!({ "path": path.to_str().unwrap() })).unwrap();
    std::fs::remove_file(&path).ok();
    assert_eq!((r["solids"].as_u64(), r["surfaces"].as_u64(), r["curves"].as_u64()), (Some(2), Some(0), Some(1)), "{r}");
    let xs = solids(&t);
    for (x, lo) in xs.iter().zip([0.0, 600.0]) {
        let (a, b) = bounds(x);
        assert!(near(a, Vec3::new(lo, 0.0, 0.0)) && near(b, Vec3::new(lo + 600.0, 560.0, 720.0)), "{a:?} {b:?}");
    }
    let (pts, closed) = polylines(&t).remove(0);
    assert!(closed && pts.iter().all(|p| p.y.abs() < 1e-6), "in the XZ plane: {pts:?}");
    assert!(pts.iter().all(|p| ((*p - Vec3::new(300.0, 0.0, 360.0)).len() - 100.0).abs() < 1e-6), "{pts:?}");
}

/// In a Front construction plane, STEPIN and IGESIN put what they read where the file has it in
/// the world (not turned into the plane again).
#[test]
fn imports_land_where_the_file_has_them_in_any_construction_plane() {
    let mut s = session();
    s.execute("box", &json!({ "corner1": [10, 20, 30], "corner2": [110, 80], "height": 40 })).unwrap();
    s.execute("line", &json!({ "points": [[0, -50], [600, -50]] })).unwrap();
    let (igs, stp) = (temp("front.igs"), temp("front.stp"));
    s.execute("igesout", &json!({ "path": igs.to_str().unwrap() })).unwrap();
    s.execute("stepout", &json!({ "path": stp.to_str().unwrap() })).unwrap();
    let mut t = session();
    t.execute("cplane", &json!({ "preset": "front" })).unwrap();
    t.execute("igesin", &json!({ "path": igs.to_str().unwrap() })).unwrap();
    t.execute("stepin", &json!({ "path": stp.to_str().unwrap() })).unwrap();
    let _ = (std::fs::remove_file(&igs), std::fs::remove_file(&stp));
    let xs = solids(&t);
    assert_eq!(xs.len(), 2);
    for x in &xs {
        let (a, b) = bounds(x);
        assert!(near(a, Vec3::new(10.0, 20.0, 30.0)) && near(b, Vec3::new(110.0, 80.0, 70.0)), "{a:?} {b:?}");
    }
    let ps = polylines(&t);
    assert!(ps.iter().all(|(p, _)| p.iter().all(|q| (q.y + 50.0).abs() < 1e-6 && q.z.abs() < 1e-6)), "{ps:?}");
}

/// Ellipses and regions with holes go out (as curves: an ellipse whole, a region loop by loop),
/// and IGESIN's solids are the closed ones, its surfaces the open ones.
#[test]
fn ellipses_regions_with_holes_and_surfaces_round_trip() {
    let mut s = session();
    s.execute("ellipse", &json!({ "center": [0, 0], "major": [400, 0], "ratio": 0.5 })).unwrap();
    let pv = |x: f64, y: f64| cadcraft_engine::geom::PolyVertex::new(cadcraft_engine::geom::Vec2::new(x, y));
    let mut region = cadcraft_engine::doc::Region::from_loop(vec![pv(1000.0, 0.0), pv(1200.0, 0.0), pv(1200.0, 200.0), pv(1000.0, 200.0)]);
    let hole = cadcraft_engine::doc::Region::from_loop(vec![pv(1050.0, 50.0), pv(1150.0, 50.0), pv(1150.0, 150.0), pv(1050.0, 150.0)]);
    region.loops.extend(hole.loops.into_iter().map(|mut l| {
        l.outer = false;
        l
    }));
    s.add_entity(EntityKind::Region(region)).unwrap();
    // An open surface beside a box.
    s.execute("box", &json!({ "corner1": [0, 500, 0], "corner2": [100, 600], "height": 50 })).unwrap();
    s.execute("planesurf", &json!({ "corner1": [300, 500, 0], "corner2": [400, 600] })).unwrap();
    let path = temp("ellipse.igs");
    let r = s.execute("igesout", &json!({ "path": path.to_str().unwrap() })).unwrap();
    assert_eq!((r["curves"].as_u64(), r["skipped"].as_u64()), (Some(3), Some(0)), "{r}");
    let mut t = session();
    let r = t.execute("igesin", &json!({ "path": path.to_str().unwrap() })).unwrap();
    std::fs::remove_file(&path).ok();
    assert_eq!((r["solids"].as_u64(), r["surfaces"].as_u64(), r["curves"].as_u64()), (Some(1), Some(1), Some(3)), "{r}");
    assert!(r["message"].as_str().unwrap().contains("1 solid, 1 surface and 3 curves"), "{r}");
    // The ellipse: closed, on its outline.
    let ps = polylines(&t);
    let oval = ps.iter().find(|(p, _)| p.iter().all(|q| q.x.abs() <= 400.0 + 1e-6)).expect("the ellipse");
    assert!(oval.1 && oval.0.iter().all(|q| ((q.x / 400.0).powi(2) + (q.y / 200.0).powi(2) - 1.0).abs() < 1e-4), "{oval:?}");
}

/// `handles: []` (as MCP clients send optional arrays) is none given: the selection is exported,
/// as STEPOUT does.
#[test]
fn an_empty_handle_list_exports_the_selection() {
    let mut s = session();
    let a = s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [10, 10], "height": 10 })).unwrap()["handle"].clone();
    s.execute("box", &json!({ "corner1": [50, 0, 0], "corner2": [60, 10], "height": 10 })).unwrap();
    s.execute("line", &json!({ "points": [[0, -50], [600, -50]] })).unwrap();
    s.set_selection(vec![cadcraft_engine::doc::Handle::parse_hex(a.as_str().unwrap()).unwrap()]);
    let (igs, stp) = (temp("sel.igs"), temp("sel.stp"));
    let r = s.execute("igesout", &json!({ "path": igs.to_str().unwrap(), "handles": [] })).unwrap();
    assert_eq!((r["solids"].as_u64(), r["curves"].as_u64()), (Some(1), Some(0)), "{r}");
    let r = s.execute("stepout", &json!({ "path": stp.to_str().unwrap(), "handles": [] })).unwrap();
    assert_eq!(r["exported"], 1, "{r}");
    let _ = (std::fs::remove_file(&igs), std::fs::remove_file(&stp));
}
