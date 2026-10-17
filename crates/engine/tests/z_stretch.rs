//! STRETCH and grip edits take a height as MOVE does (JSON `delta: [dx, dy, dz]`, typed
//! `@0,0,100`, a 3D snap's `point_z`): line ends, 3D polyline vertices and points move in Z, objects
//! that move whole move in 3D, a STRETCH of a planar object that would only partly leave its plane
//! refuses rather than losing the height, and a grip that can't leave its plane takes the point
//! into it. COPY's Array option (number of items, second point or Fit).

use std::sync::Arc;

use cadcraft_engine::doc::{Entity, EntityKind, Handle, Solid3d};
use cadcraft_engine::geom::{Mat4, Mesh3, Vec2, Vec3};
use cadcraft_engine::{Input, Session};
use serde_json::{Value, json};

fn handle(r: &Value) -> Handle {
    Handle::parse_hex(r.get("handle").unwrap_or(&r["handles"][0]).as_str().unwrap()).unwrap()
}

fn ent(s: &Session, h: Handle) -> Entity {
    (**s.doc().unwrap().entity(h).unwrap()).clone()
}

fn line_ends(s: &Session, h: Handle) -> (Vec3, Vec3) {
    match ent(s, h).kind {
        EntityKind::Line(l) => (l.a, l.b),
        k => panic!("{k:?}"),
    }
}

/// The last message the command line showed.
fn said(s: &Session) -> String {
    s.log.last().cloned().unwrap_or_default()
}

fn snapshot(s: &mut Session) -> Value {
    s.execute("drawing.inspect", &json!({})).unwrap()["entities"].clone()
}

/// A 2 x 2 square of solid edges at the origin (no kernel needed), selectable by its edges.
fn solid(s: &mut Session) -> Handle {
    let c = |x: f64, y: f64| Vec3::new(x, y, 0.0);
    let ring = vec![c(0.0, 0.0), c(2.0, 0.0), c(2.0, 2.0), c(0.0, 2.0), c(0.0, 0.0)];
    let mesh = Mesh3 { verts: ring.clone(), edges: vec![ring], ..Default::default() };
    s.add_entity(EntityKind::Solid3d(Solid3d { mesh: Arc::new(mesh), xform: Mat4::IDENTITY, ..Default::default() })).unwrap()
}

#[test]
fn json_stretch_takes_a_z_delta() {
    let mut s = Session::new();
    let l = handle(&s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap());
    let p3 = handle(&s.execute("3dpoly", &json!({ "points": [[0, 5, 0], [10, 5, 1]] })).unwrap());
    let pt = handle(&s.execute("point", &json!({ "at": [10, 8] })).unwrap());
    let c = handle(&s.execute("circle", &json!({ "center": [10, 12], "radius": 0.5 })).unwrap());
    // A window around the right-hand ends: they rise by 5; the circle inside moves whole.
    s.execute("stretch", &json!({ "window": [[12, -1], [8, 13]], "delta": [1, 0, 5] })).unwrap();
    assert_eq!(line_ends(&s, l), (Vec3::ZERO, Vec3::new(11.0, 0.0, 5.0)));
    let EntityKind::Polyline3d(p) = ent(&s, p3).kind else { panic!() };
    assert_eq!(p.points, vec![Vec3::new(0.0, 5.0, 0.0), Vec3::new(11.0, 5.0, 6.0)]);
    let EntityKind::Point(q) = ent(&s, pt).kind else { panic!() };
    assert_eq!(q.p, Vec3::new(11.0, 8.0, 5.0));
    let EntityKind::Circle(k) = ent(&s, c).kind else { panic!() };
    assert_eq!(k.center, Vec3::new(11.0, 12.0, 5.0));

    // `from`/`to` with heights, and a fence: what it crosses moves whole, in 3D.
    s.execute("stretch", &json!({ "window": [[12, -1], [10.5, 1]], "from": [0, 0, 5], "to": [0, 0, 0] })).unwrap();
    assert_eq!(line_ends(&s, l), (Vec3::ZERO, Vec3::new(11.0, 0.0, 0.0)));
    s.execute("stretch", &json!({ "fence": [[5, -1], [5, 1]], "delta": [0, 0, 2] })).unwrap();
    assert_eq!(line_ends(&s, l), (Vec3::new(0.0, 0.0, 2.0), Vec3::new(11.0, 0.0, 2.0)));
}

#[test]
fn a_planar_object_only_partly_inside_refuses_a_z_stretch() {
    let mut s = Session::new();
    let l = handle(&s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap());
    let pl = handle(&s.execute("pline", &json!({ "vertices": [[0, 5], [10, 5], [10, 8]] })).unwrap());
    let before = snapshot(&mut s);
    let e = s.execute("stretch", &json!({ "window": [[12, -1], [8, 9]], "delta": [0, 0, 5] })).unwrap_err().to_string();
    assert!(e.contains("polyline") && e.contains("in Z"), "{e}");
    assert_eq!(snapshot(&mut s), before, "nothing moves, nothing is dropped");
    // In plan it stretches as before.
    s.execute("stretch", &json!({ "window": [[12, -1], [8, 9]], "delta": [1, 0] })).unwrap();
    assert_eq!(line_ends(&s, l).1, Vec3::new(11.0, 0.0, 0.0));
    // Wholly inside, it moves up whole (its elevation).
    s.execute("stretch", &json!({ "window": [[-1, 4], [12, 9]], "delta": [0, 0, 3] })).unwrap();
    let EntityKind::LwPolyline(p) = ent(&s, pl).kind else { panic!() };
    assert_eq!((p.elevation, p.vertices[0].p), (3.0, Vec2::new(0.0, 5.0)));
    // A region can't leave the XY plane even whole, as with MOVE.
    let r = s.execute("pline", &json!({ "vertices": [[20, 0], [30, 0], [30, 5]], "closed": true })).unwrap();
    s.execute("region", &json!({ "handles": [r["handle"]] })).unwrap();
    let e = s.execute("stretch", &json!({ "window": [[19, -1], [31, 6]], "delta": [0, 0, 1] })).unwrap_err().to_string();
    assert!(e.contains("region") && e.contains("in Z"), "{e}");
}

#[test]
fn a_solid_wholly_in_the_window_moves_whole() {
    let mut s = Session::new();
    let h = solid(&mut s);
    let l = handle(&s.execute("line", &json!({ "points": [[1.8, 1], [5, 1]] })).unwrap());
    // A crossing window over one corner stretches the line beside it; the solid stays (STRETCH
    // doesn't stretch 3D solids).
    s.execute("stretch", &json!({ "window": [[3, 3], [1.5, 0.5]], "delta": [1, 0] })).unwrap();
    assert_eq!(line_ends(&s, l).0, Vec3::new(2.8, 1.0, 0.0));
    let EntityKind::Solid3d(k) = ent(&s, h).kind else { panic!() };
    assert_eq!(k.xform, Mat4::IDENTITY);
    // Wholly inside, it moves whole, in Z too.
    s.execute("stretch", &json!({ "window": [[3, 3], [-1, -1]], "delta": [1, 0, 4] })).unwrap();
    let EntityKind::Solid3d(k) = ent(&s, h).kind else { panic!() };
    assert_eq!(k.xform, Mat4::translate(Vec3::new(1.0, 0.0, 4.0)));
}

#[test]
fn typed_stretch_points_with_a_height() {
    let mut s = Session::new();
    let l = handle(&s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap());
    s.script("STRETCH C 12,1 8,-1  0,0 @0,0,7\n").unwrap();
    assert!(s.running.is_none(), "{}", s.prompt_text());
    assert_eq!(line_ends(&s, l), (Vec3::ZERO, Vec3::new(10.0, 0.0, 7.0)));
    // The Displacement option, and a base point with a height brought back to the plane.
    s.script("STRETCH C 12,1 8,-1  D 1,0,-7\n").unwrap();
    assert_eq!(line_ends(&s, l).1, Vec3::new(11.0, 0.0, 0.0));
    s.script("STRETCH C 12,1 8,-1  0,0,3 0,0\n").unwrap();
    assert_eq!(line_ends(&s, l).1, Vec3::new(11.0, 0.0, -3.0));
}

fn grip(s: &Session, h: Handle, at: Vec2) -> (Handle, usize) {
    (h, s.grips_of(h).unwrap().iter().position(|g| g.near(at, 1e-9)).unwrap())
}

#[test]
fn grip_stretch_takes_a_height_and_relative_points_start_at_the_grip() {
    let mut s = Session::new();
    let l = handle(&s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap());
    s.start_grip_edit(&[grip(&s, l, Vec2::new(10.0, 0.0))]).unwrap();
    s.cmdline("@0,0,100").unwrap();
    assert!(s.running.is_none());
    assert_eq!(line_ends(&s, l), (Vec3::ZERO, Vec3::new(10.0, 0.0, 100.0)));
    // From a raised grip: `@dx,dy` and a plain pick keep its height, `x,y,z` sets it.
    s.start_grip_edit(&[grip(&s, l, Vec2::new(10.0, 0.0))]).unwrap();
    s.cmdline("@5,0").unwrap();
    assert_eq!(line_ends(&s, l).1, Vec3::new(15.0, 0.0, 100.0));
    s.start_grip_edit(&[grip(&s, l, Vec2::new(15.0, 0.0))]).unwrap();
    s.point_z = None;
    s.input(Input::Point(Vec2::new(16.0, 0.0))).unwrap();
    assert_eq!(line_ends(&s, l).1, Vec3::new(16.0, 0.0, 100.0));
    s.start_grip_edit(&[grip(&s, l, Vec2::new(16.0, 0.0))]).unwrap();
    s.cmdline("16,0,30").unwrap();
    assert_eq!(line_ends(&s, l).1, Vec3::new(16.0, 0.0, 30.0));
    // A 3D snap's height (as the UI gives it).
    s.start_grip_edit(&[grip(&s, l, Vec2::new(0.0, 0.0))]).unwrap();
    s.point_z = Some(-4.0);
    s.input(Input::Point(Vec2::new(0.0, 0.0))).unwrap();
    s.point_z = None;
    assert_eq!(line_ends(&s, l), (Vec3::new(0.0, 0.0, -4.0), Vec3::new(16.0, 0.0, 30.0)));
    // The midpoint moves the whole line.
    s.start_grip_edit(&[grip(&s, l, Vec2::new(8.0, 0.0))]).unwrap();
    s.cmdline("@0,0,4").unwrap();
    assert_eq!(line_ends(&s, l), (Vec3::ZERO, Vec3::new(16.0, 0.0, 34.0)));
    // A circle's centre moves it up; the undo step takes it back.
    let c = handle(&s.execute("circle", &json!({ "center": [50, 0], "radius": 2 })).unwrap());
    s.start_grip_edit(&[grip(&s, c, Vec2::new(50.0, 0.0))]).unwrap();
    s.cmdline("@0,0,6").unwrap();
    let EntityKind::Circle(k) = ent(&s, c).kind else { panic!() };
    assert_eq!(k.center, Vec3::new(50.0, 0.0, 6.0));
    s.execute("undo", &json!({})).unwrap();
    let EntityKind::Circle(k) = ent(&s, c).kind else { panic!() };
    assert_eq!(k.center, Vec3::new(50.0, 0.0, 0.0));
    // JSON: several hot grips with a 3D delta.
    s.execute("grip.stretch", &json!({ "grips": [[l.hex(), 0]], "delta": [0, 0, 1] })).unwrap();
    assert_eq!(line_ends(&s, l).0, Vec3::new(0.0, 0.0, 1.0));
}

fn vertex(s: &Session, h: Handle, i: usize) -> Vec2 {
    let EntityKind::LwPolyline(p) = ent(s, h).kind else { panic!() };
    p.vertices[i].p
}

/// A polyline's vertex can't leave its plane: a point with a height (a 3D snap) is taken into it
/// as in plan, so a snap to a point of a tilted polyline's own plane, or to one at another height,
/// still stretches it.
#[test]
fn a_polyline_vertex_grip_takes_a_point_with_a_height_into_its_plane() {
    let mut s = Session::new();
    let pl = handle(&s.execute("pline", &json!({ "vertices": [[0, 0], [10, 0], [10, 8]] })).unwrap());
    s.execute("stretch", &json!({ "window": [[-1, -1], [12, 9]], "delta": [0, 0, 5] })).unwrap();
    s.start_grip_edit(&[grip(&s, pl, Vec2::new(10.0, 0.0))]).unwrap();
    // A floor point's height, as a 3D snap gives it.
    s.point_z = Some(0.0);
    s.input(Input::Point(Vec2::new(20.0, 0.0))).unwrap();
    s.point_z = None;
    assert!(s.running.is_none(), "{}", said(&s));
    assert_eq!(vertex(&s, pl, 1), Vec2::new(20.0, 0.0));
    s.start_grip_edit(&[grip(&s, pl, Vec2::new(20.0, 0.0))]).unwrap();
    s.cmdline("@1,0,5").unwrap();
    assert_eq!(vertex(&s, pl, 1), Vec2::new(21.0, 0.0));

    // A square in a UCS turned 45 degrees about X: its far corner dragged to (5, 5) of its plane.
    let mut s = Session::new();
    for t in ["ucs x 45", "pline 0,0 10,0 10,10 0,10 c", "ucs w"] {
        s.cmdline(t).unwrap();
    }
    let sq = s.doc().unwrap().model.last().unwrap().handle;
    let h = 5.0 * std::f64::consts::FRAC_1_SQRT_2;
    s.execute("grip.stretch", &json!({ "grips": [[sq.hex(), 4]], "delta": [-5, -h, -h] })).unwrap();
    assert!(vertex(&s, sq, 2).dist(Vec2::new(5.0, 5.0)) < 1e-9, "{:?}", vertex(&s, sq, 2));
}

#[test]
fn grip_move_takes_a_height_and_rotate_stays_in_the_plane() {
    let mut s = Session::new();
    let l = handle(&s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap());
    let c = handle(&s.execute("circle", &json!({ "center": [20, 0], "radius": 2 })).unwrap());
    s.set_selection(vec![l, c]);
    s.start_grip_edit(&[grip(&s, l, Vec2::new(10.0, 0.0))]).unwrap();
    s.cmdline("").unwrap();
    assert!(s.prompt_text().contains("MOVE"), "{}", s.prompt_text());
    s.cmdline("@0,0,10").unwrap();
    assert_eq!(line_ends(&s, l), (Vec3::new(0.0, 0.0, 10.0), Vec3::new(10.0, 0.0, 10.0)));
    let EntityKind::Circle(k) = ent(&s, c).kind else { panic!() };
    assert_eq!(k.center, Vec3::new(20.0, 0.0, 10.0));
    // ROTATE about a raised grip: in the plane, heights kept.
    s.set_selection(vec![l]);
    s.start_grip_edit(&[grip(&s, l, Vec2::new(0.0, 0.0))]).unwrap();
    s.cmdline("").unwrap();
    s.cmdline("").unwrap();
    assert!(s.prompt_text().contains("ROTATE"), "{}", s.prompt_text());
    s.cmdline("90").unwrap();
    let (a, b) = line_ends(&s, l);
    assert_eq!(a, Vec3::new(0.0, 0.0, 10.0));
    assert!((b - Vec3::new(0.0, 10.0, 10.0)).len() < 1e-9, "{b:?}");
}

fn circle_centres(s: &Session) -> Vec<Vec3> {
    let mut v: Vec<Vec3> =
        s.doc().unwrap().model.iter().filter_map(|e| if let EntityKind::Circle(c) = &e.kind { Some(c.center) } else { None }).collect();
    v.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.z.total_cmp(&b.z)));
    v
}

#[test]
fn copy_array_places_items_at_a_step_or_fitted() {
    let x = |x: f64| Vec3::new(x, 0.0, 0.0);
    let mut s = Session::new();
    s.execute("circle", &json!({ "center": [0, 0], "radius": 1 })).unwrap();
    s.cmdline("copy all  0,0 a").unwrap();
    assert!(s.prompt_text().contains("number of items"), "{}", s.prompt_text());
    // Four items, the original included: three copies 10 apart.
    s.cmdline("4 10,0").unwrap();
    assert!(s.prompt_text().contains("Array"), "back at the second point: {}", s.prompt_text());
    assert_eq!(circle_centres(&s), vec![x(0.0), x(10.0), x(20.0), x(30.0)]);
    // Undo takes the whole array back; Enter ends.
    s.cmdline("u ").unwrap();
    assert!(s.running.is_none());
    assert_eq!(circle_centres(&s), vec![x(0.0)]);

    // Fit: the last item at the second point; one more copy after it, then Undo takes only it.
    s.cmdline("copy all  0,0 a 3 f 10,0 3,0 u ").unwrap();
    assert!(s.running.is_none());
    assert_eq!(circle_centres(&s), vec![x(0.0), x(5.0), x(10.0)]);
    s.execute("undo", &json!({})).unwrap();
    assert_eq!(circle_centres(&s), vec![x(0.0)]);

    // With a height, and a count that isn't an integer of 2 or more asked again.
    s.cmdline("copy all  0,0 a").unwrap();
    for t in ["1", "2.5"] {
        s.cmdline(t).unwrap();
        assert!(said(&s).contains("integer"), "{}", said(&s));
        assert!(s.prompt_text().contains("number of items"), "{}", s.prompt_text());
    }
    s.cmdline("3 @0,0,10 ").unwrap();
    assert_eq!(circle_centres(&s), vec![x(0.0), Vec3::new(0.0, 0.0, 10.0), Vec3::new(0.0, 0.0, 20.0)]);
}
