//! 3DMOVE, 3DROTATE, MIRROR3D and 3DSCALE on solids (their transform), lines, 3D polylines,
//! points and planar objects; refusals, locked layers, undo and the prompts. No kernel needed.

use std::sync::Arc;

use cadcraft_engine::Input;
use cadcraft_engine::Session;
use cadcraft_engine::doc::{EntityKind, Handle, Line, Point, Polyline3d, Solid3d};
use cadcraft_engine::geom::{Mat4, Mesh3, Vec2, Vec3};
use serde_json::{Value, json};

fn session() -> Session {
    let mut s = Session::new();
    s.new_drawing(true);
    s
}

fn solid(s: &mut Session) -> Handle {
    let mesh = Mesh3 { verts: vec![Vec3::ZERO, Vec3::new(10.0, 6.0, 2.0)], ..Default::default() };
    s.add_entity(EntityKind::Solid3d(Solid3d { mesh: Arc::new(mesh), ..Default::default() })).unwrap()
}

fn line(s: &mut Session, a: Vec3, b: Vec3) -> Handle {
    s.add_entity(EntityKind::Line(Line { a, b })).unwrap()
}

fn xform(s: &Session, h: Handle) -> Mat4 {
    match &s.doc().unwrap().entity(h).unwrap().kind {
        EntityKind::Solid3d(x) => x.xform,
        _ => panic!("not a solid"),
    }
}

fn line_of(s: &Session, h: Handle) -> (Vec3, Vec3) {
    match &s.doc().unwrap().entity(h).unwrap().kind {
        EntityKind::Line(l) => (l.a, l.b),
        _ => panic!("not a line"),
    }
}

fn near(a: Vec3, b: [f64; 3]) -> bool {
    (a - Vec3::new(b[0], b[1], b[2])).len() < 1e-9
}

#[test]
fn move_rotate_mirror_and_scale_lines_and_points() {
    let mut s = session();
    let l = line(&mut s, Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0));
    let pt = s.add_entity(EntityKind::Point(Point { p: Vec3::new(1.0, 2.0, 3.0), angle: 0.0 })).unwrap();
    let pl = s.add_entity(EntityKind::Polyline3d(Polyline3d { points: vec![Vec3::ZERO, Vec3::new(0.0, 5.0, 0.0)], closed: false })).unwrap();
    let all = json!([l.hex(), pt.hex(), pl.hex()]);
    s.execute("3dmove", &json!({ "handles": all, "delta": [1, 2, 3] })).unwrap();
    assert!(near(line_of(&s, l).1, [11.0, 2.0, 3.0]));
    // 90° about the X axis through the origin: y goes to z.
    s.execute("3drotate", &json!({ "handles": [l.hex()], "axis": [[0, 0, 0], [5, 0, 0]], "angle": 90 })).unwrap();
    let (a, b) = line_of(&s, l);
    assert!(near(a, [1.0, -3.0, 2.0]) && near(b, [11.0, -3.0, 2.0]), "{a:?} {b:?}");
    // Mirror across the XY plane keeps the source and adds the copy below.
    let r = s.execute("mirror3d", &json!({ "handles": [pt.hex()], "plane": "xy" })).unwrap();
    let copy = Handle::parse_hex(r["handles"][0].as_str().unwrap()).unwrap();
    let p_of = |s: &Session, h: Handle| match &s.doc().unwrap().entity(h).unwrap().kind {
        EntityKind::Point(p) => p.p,
        _ => panic!(),
    };
    assert!(near(p_of(&s, pt), [2.0, 4.0, 6.0]) && near(p_of(&s, copy), [2.0, 4.0, -6.0]));
    // Three points: the plane x = 1, erasing the source.
    s.execute("mirror3d", &json!({ "handles": [pt.hex()], "points": [[1, 0, 0], [1, 1, 0], [1, 0, 1]], "erase": true })).unwrap();
    assert!(near(p_of(&s, pt), [0.0, 4.0, 6.0]));
    s.execute("3dscale", &json!({ "handles": [pl.hex()], "base": [1, 2, 3], "factor": 2 })).unwrap();
    let EntityKind::Polyline3d(p) = &s.doc().unwrap().entity(pl).unwrap().kind else { panic!() };
    assert!(near(p.points[1], [1.0, 12.0, 3.0]), "{:?}", p.points);
    // Each command is one undo step.
    s.undo().unwrap();
    let EntityKind::Polyline3d(p) = &s.doc().unwrap().entity(pl).unwrap().kind else { panic!() };
    assert!(near(p.points[1], [1.0, 7.0, 3.0]));
}

#[test]
fn solids_compose_their_transform_and_stay_similar() {
    let mut s = session();
    let x = solid(&mut s);
    s.execute("3drotate", &json!({ "handles": [x.hex()], "axis": [[0, 0, 0], [0, 1, 0]], "angle": 30 })).unwrap();
    s.execute("3dscale", &json!({ "handles": [x.hex()], "base": [0, 0, 0], "factor": 3 })).unwrap();
    s.execute("mirror3d", &json!({ "handles": [x.hex()], "plane": "yz", "point": [5, 0, 0], "erase": true })).unwrap();
    let m = xform(&s, x).m;
    // Columns of a similarity: equal lengths (3), square to each other.
    let col = |j: usize| Vec3::new(m[0][j], m[1][j], m[2][j]);
    for (i, j) in [(0, 1), (0, 2), (1, 2)] {
        assert!(col(i).dot(col(j)).abs() < 1e-9);
    }
    for j in 0..3 {
        assert!((col(j).len() - 3.0).abs() < 1e-9);
    }
    // The model's far corner (10, 6, 2) lands where the three steps put it.
    let EntityKind::Solid3d(sx) = &s.doc().unwrap().entity(x).unwrap().kind else { panic!() };
    let p = sx.world(Vec3::new(10.0, 6.0, 2.0));
    let (c, sn) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
    let r = Vec3::new(10.0 * c + 2.0 * sn, 6.0, -10.0 * sn + 2.0 * c) * 3.0;
    assert!(near(p, [10.0 - r.x, r.y, r.z]), "{p:?} {r:?}");
}

#[test]
fn bad_parameters_and_unsupported_objects_change_nothing() {
    let mut s = session();
    let x = solid(&mut s);
    let c = s.execute("circle", &json!({ "center": [0, 0], "radius": 2 })).unwrap()["handle"].clone();
    let before = s.doc().unwrap().clone();
    let e = |r: cadcraft_engine::Result<Value>| r.err().map(|e| e.to_string()).unwrap_or_default();
    for (id, p) in [
        ("3dmove", json!({ "handles": [x.hex()] })),
        ("3dmove", json!({ "handles": [x.hex()], "delta": [1, "a", 3] })),
        ("3drotate", json!({ "handles": [x.hex()], "axis": [[0, 0, 0], [0, 0, 0]], "angle": 10 })),
        ("3drotate", json!({ "handles": [x.hex()], "axis": [[0, 0, 0]], "angle": 10 })),
        ("3drotate", json!({ "handles": [x.hex()], "axis": [[0, 0, 0], [1, 0, 0]] })),
        ("3drotate", json!({ "handles": [x.hex()], "axis": [[0, 0, 0], [1, 0, 0]], "angle": "x" })),
        ("mirror3d", json!({ "handles": [x.hex()] })),
        ("mirror3d", json!({ "handles": [x.hex()], "plane": "ab" })),
        ("mirror3d", json!({ "handles": [x.hex()], "points": [[0, 0, 0], [1, 1, 1], [2, 2, 2]] })),
        ("mirror3d", json!({ "handles": [x.hex()], "points": [[0, 0, 0], [1, 1, 1]] })),
        ("mirror3d", json!({ "handles": [x.hex()], "plane": "xy", "point": "o" })),
        ("3dscale", json!({ "handles": [x.hex()], "base": [0, 0], "factor": 0 })),
        ("3dscale", json!({ "handles": [x.hex()], "base": [0, 0], "factor": -1 })),
        ("3dscale", json!({ "handles": [x.hex()], "factor": 2 })),
        ("3dscale", json!({ "handles": [x.hex()], "base": [0, 0], "factor": 1e308 })),
        ("3dmove", json!({ "handles": "x", "delta": [1, 0, 0] })),
    ] {
        assert!(s.execute(id, &p).is_err(), "{id} {p}");
        assert_eq!(*s.doc().unwrap(), before, "{id} {p}");
    }
    // A circle stands up in the XZ plane; a dimension can't leave the horizontal: the whole
    // command is refused and the solid stays.
    let r = s.execute("3drotate", &json!({ "handles": [c.clone()], "axis": [[0, 0, 0], [1, 0, 0]], "angle": 90 })).unwrap();
    assert_eq!(r["handles"].as_array().map(Vec::len), Some(1));
    let ch = Handle::parse_hex(c.as_str().unwrap()).unwrap();
    assert!(s.doc().unwrap().entity(ch).unwrap().common.extrusion.z.abs() < 1e-9, "the circle's plane turned");
    let dim = s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [10, 0], "at": [5, -3] })).unwrap()["handle"].clone();
    let before = s.doc().unwrap().clone();
    let msg = e(s.execute("3drotate", &json!({ "handles": [x.hex(), dim], "axis": [[0, 0, 0], [1, 0, 0]], "angle": 90 })));
    assert!(msg.contains("dimension"), "{msg}");
    assert_eq!(*s.doc().unwrap(), before);
    // Locked layers: left alone.
    s.doc_mut().unwrap().layer_mut("0").unwrap().locked = true;
    assert!(e(s.execute("3dmove", &json!({ "handles": [x.hex()], "delta": [1, 0, 0] }))).contains("locked"));
    assert_eq!(xform(&s, x), Mat4::IDENTITY);
}

#[test]
fn prompts_follow_autocad() {
    let mut s = session();
    let l = line(&mut s, Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0));
    let msg = |s: &Session| s.current_prompt().map(|p| p.display()).unwrap_or_default();
    // 3DROTATE: a point on a named axis, then the angle.
    s.set_selection(vec![l]);
    s.start("3drotate").unwrap();
    assert!(msg(&s).contains("first point on axis or [Xaxis/Yaxis/Zaxis]"), "{}", msg(&s));
    s.cmdline("y").unwrap();
    assert!(msg(&s).contains("point on the Y axis <0,0,0>"), "{}", msg(&s));
    s.input(Input::Enter).unwrap();
    assert!(msg(&s).contains("rotation angle"), "{}", msg(&s));
    s.cmdline("90").unwrap();
    assert!(s.current_prompt().is_none());
    // About Y by 90°: +X goes to -Z.
    assert!(near(line_of(&s, l).1, [0.0, 0.0, -10.0]), "{:?}", line_of(&s, l));
    // 3DMOVE by a typed displacement.
    s.set_selection(vec![l]);
    s.start("3dmove").unwrap();
    assert!(msg(&s).contains("base point or [Displacement] <Displacement>"), "{}", msg(&s));
    s.input(Input::Enter).unwrap();
    s.cmdline("0,0,18").unwrap();
    assert!(near(line_of(&s, l).1, [0.0, 0.0, 8.0]), "{:?}", line_of(&s, l));
    // 3DMOVE by two picked points; the second snapped to a 3D object 5 up.
    s.set_selection(vec![l]);
    s.start("3dmove").unwrap();
    s.input(Input::Point(Vec2::ZERO)).unwrap();
    s.point_z = Some(5.0);
    s.input(Input::Point(Vec2::new(1.0, 0.0))).unwrap();
    assert!(near(line_of(&s, l).1, [1.0, 0.0, 13.0]), "{:?}", line_of(&s, l));
    // MIRROR3D on the ZX plane, keeping the source.
    s.point_z = None;
    s.set_selection(vec![l]);
    s.start("mirror3d").unwrap();
    assert!(msg(&s).contains("[XY/YZ/ZX]"), "{}", msg(&s));
    s.cmdline("zx").unwrap();
    s.cmdline("0,3").unwrap();
    assert!(msg(&s).contains("Delete source objects? [Yes/No] <No>"), "{}", msg(&s));
    s.input(Input::Enter).unwrap();
    assert_eq!(s.doc().unwrap().model.iter().count(), 2);
    // 3DSCALE: base point, factor.
    s.set_selection(vec![l]);
    s.start("3dscale").unwrap();
    s.input(Input::Point(Vec2::new(1.0, 0.0))).unwrap();
    assert!(msg(&s).contains("scale factor"), "{}", msg(&s));
    s.cmdline("2").unwrap();
    assert!(near(line_of(&s, l).0, [1.0, 0.0, 46.0]), "{:?}", line_of(&s, l));
}

/// MIRROR3D by three picked points: a third point on the line of the first two is asked for
/// again; the plane x = 1 (standing, through a point picked 5 up) mirrors the line in place.
#[test]
fn mirror3d_takes_three_points() {
    let mut s = session();
    let l = line(&mut s, Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0));
    let msg = |s: &Session| s.current_prompt().map(|p| p.display()).unwrap_or_default();
    s.set_selection(vec![l]);
    s.start("mirror3d").unwrap();
    s.input(Input::Point(Vec2::new(1.0, 0.0))).unwrap();
    assert!(msg(&s).contains("second point on mirror plane"), "{}", msg(&s));
    s.input(Input::Point(Vec2::new(1.0, 1.0))).unwrap();
    s.input(Input::Point(Vec2::new(1.0, 4.0))).unwrap();
    assert!(msg(&s).contains("third point on mirror plane"), "on one line: asked again: {}", msg(&s));
    s.point_z = Some(5.0);
    s.input(Input::Point(Vec2::new(1.0, 0.0))).unwrap();
    assert!(msg(&s).contains("Delete source objects?"), "{}", msg(&s));
    s.cmdline("y").unwrap();
    assert!(s.current_prompt().is_none());
    let (a, b) = line_of(&s, l);
    assert!(near(a, [2.0, 0.0, 0.0]) && near(b, [-8.0, 0.0, 0.0]), "{a:?} {b:?}");
    assert_eq!(s.doc().unwrap().model.iter().count(), 1, "the source went");
}

/// 3DSCALE: a bad factor is asked for again; a command refused at the end (a dimension can't
/// turn out of the horizontal) starts again from the selection rather than failing again.
#[test]
fn transforms_ask_again_after_an_error() {
    let mut s = session();
    let l = line(&mut s, Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0));
    let msg = |s: &Session| s.current_prompt().map(|p| p.display()).unwrap_or_default();
    s.set_selection(vec![l]);
    s.start("3dscale").unwrap();
    s.input(Input::Point(Vec2::ZERO)).unwrap();
    for bad in ["-2", "zero", "0"] {
        s.cmdline(bad).unwrap();
        assert!(msg(&s).contains("scale factor"), "{bad}: {}", msg(&s));
    }
    s.cmdline("3").unwrap();
    assert!(s.current_prompt().is_none());
    assert!(near(line_of(&s, l).1, [30.0, 0.0, 0.0]));
    let dim = s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [10, 0], "at": [5, -3] })).unwrap()["handle"].clone();
    let dim = Handle::parse_hex(dim.as_str().unwrap()).unwrap();
    let before = s.doc().unwrap().clone();
    s.set_selection(vec![dim]);
    s.start("3drotate").unwrap();
    s.cmdline("x").unwrap();
    s.input(Input::Enter).unwrap();
    s.cmdline("90").unwrap();
    assert!(msg(&s).contains("Select objects"), "{}", msg(&s));
    assert_eq!(*s.doc().unwrap(), before);
    s.input(Input::Pick(vec![l])).unwrap();
    s.input(Input::Enter).unwrap();
    s.cmdline("x").unwrap();
    s.input(Input::Enter).unwrap();
    s.cmdline("90").unwrap();
    assert!(s.current_prompt().is_none());
    assert!(near(line_of(&s, l).1, [30.0, 0.0, 0.0]), "on the X axis it stays");
}

#[test]
fn rhino_names_reach_the_3d_transforms() {
    assert_eq!(cadcraft_engine::cmd::resolve_alias("Rotate3D"), "3drotate");
    assert_eq!(cadcraft_engine::cmd::resolve_alias("3m"), "3dmove");
}
