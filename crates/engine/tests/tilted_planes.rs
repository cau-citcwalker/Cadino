//! Objects in tilted planes (a DXF extrusion other than Z) are picked, snapped, moved and
//! grip-edited where plan view shows them, not at their own (OCS) coordinates.

use cadcraft_engine::Session;
use cadcraft_engine::doc::{EntityKind, Handle, Space};
use cadcraft_engine::geom::{Vec2, Vec3};
use cadcraft_engine::grips::GripMode;
use cadcraft_engine::snap::{mode, osnap};
use serde_json::json;

/// A drawing read from DXF entity records (and block records).
fn session(blocks: &str, entities: &str) -> Session {
    let text = format!("0\nSECTION\n2\nBLOCKS\n{blocks}0\nENDSEC\n0\nSECTION\n2\nENTITIES\n{entities}0\nENDSEC\n0\nEOF\n");
    let d = cadcraft_io::read_dxf(text.as_bytes()).unwrap();
    let mut s = Session::empty();
    s.open_drawing(d, "tilted", None);
    s
}

/// A circle standing in the XZ plane, facing -Y: OCS centre (1,2) at elevation 3 is world
/// (1,-3,2), so plan view sees a segment from (-0.5,-3) to (2.5,-3).
const STANDING_CIRCLE: &str = "0\nCIRCLE\n8\n0\n10\n1\n20\n2\n30\n3\n40\n1.5\n210\n0\n220\n-1\n230\n0\n";

fn only(s: &Session) -> cadcraft_engine::doc::Entity {
    s.doc().unwrap().model.iter().next().map(|e| (**e).clone()).unwrap()
}

fn near(a: Vec2, b: Vec2) -> bool {
    (a - b).len() < 1e-9
}

#[test]
fn a_standing_circle_is_picked_and_snapped_where_plan_view_shows_it() {
    let s = session("", STANDING_CIRCLE);
    let (d, sp) = (s.doc().unwrap(), Space::Model);
    let h = only(&s).handle;
    assert_eq!(cadcraft_engine::select::pick(d, &sp, Vec2::new(2.0, -3.02), 0.1), Some(h));
    // Not where its own coordinates would put it.
    assert_eq!(cadcraft_engine::select::pick(d, &sp, Vec2::new(2.5, 2.0), 0.1), None);
    let end = osnap(d, &sp, Vec2::new(2.45, -3.02), 0.2, mode::END, None, false).unwrap();
    assert!(near(end.point, Vec2::new(2.5, -3.0)), "{:?}", end.point);
    let cen = osnap(d, &sp, Vec2::new(1.1, -2.9), 0.3, mode::CEN, None, false).unwrap();
    assert!(near(cen.point, Vec2::new(1.0, -3.0)) && cen.mode == mode::CEN, "{:?}", cen.point);
    assert!(osnap(d, &sp, Vec2::new(1.1, 2.1), 0.3, mode::CEN, None, false).is_none());
    // Window selection sees the plan segment too.
    let w = cadcraft_engine::select::select_window(d, &sp, cadcraft_engine::geom::Bounds2::new(Vec2::new(-1.0, -4.0), Vec2::new(3.0, -2.0)), false);
    assert_eq!(w, vec![h]);
}

#[test]
fn moving_a_standing_circle_in_plan_moves_its_plane() {
    let mut s = session("", STANDING_CIRCLE);
    let h = only(&s).handle;
    s.execute("move", &json!({ "handles": [h.hex()], "delta": [0, 5] })).unwrap();
    let e = only(&s);
    // Facing -Y still; 5 along +Y is 5 down its normal: elevation 3 - 5. Its own x, y stay.
    assert_eq!(e.common.extrusion, Vec3::new(0.0, -1.0, 0.0));
    let EntityKind::Circle(c) = &e.kind else { panic!() };
    assert!((c.center - Vec3::new(1.0, 2.0, -2.0)).len() < 1e-9 && c.radius == 1.5, "{c:?}");
    let b = cadcraft_engine::doc::entity_bounds(s.doc().unwrap(), &e, 0);
    assert!((b.min.y - 2.0).abs() < 1e-9 && (b.max.y - 2.0).abs() < 1e-9, "{b:?}");
    // On a locked layer it stays.
    s.execute("layer.set", &json!({ "name": "0", "locked": true })).unwrap();
    let before = only(&s);
    let _ = s.execute("move", &json!({ "handles": [h.hex()], "delta": [3, 0] }));
    assert_eq!(only(&s), before);
}

#[test]
fn grips_of_a_standing_circle_are_in_plan_and_stretch_in_its_plane() {
    let mut s = session("", STANDING_CIRCLE);
    let h = only(&s).handle;
    let grips = s.grips_of(h).unwrap();
    assert!(grips.iter().all(|g| (g.y + 3.0).abs() < 1e-9), "{grips:?}");
    let i = grips.iter().position(|g| near(*g, Vec2::new(2.5, -3.0))).unwrap();
    // Dragged 1 further along what plan view shows: the radius grows by 1, the centre stays.
    s.grip_edit(h, i, Vec2::new(3.5, -3.0), GripMode::Stretch).unwrap();
    let e = only(&s);
    let EntityKind::Circle(c) = &e.kind else { panic!() };
    assert!((c.radius - 2.5).abs() < 1e-9 && (c.center - Vec3::new(1.0, 2.0, 3.0)).len() < 1e-9, "{c:?}");
    assert_eq!(e.common.extrusion, Vec3::new(0.0, -1.0, 0.0));
    // Grip Move in plan moves it like MOVE.
    let centre = s.grips_of(h).unwrap()[0];
    assert!(near(centre, Vec2::new(1.0, -3.0)), "{centre:?}");
    s.grip_edit(h, 0, Vec2::new(1.0, 1.0), GripMode::Move).unwrap();
    assert!(near(s.grips_of(h).unwrap()[0], Vec2::new(1.0, 1.0)));
}

#[test]
fn a_tilted_block_reference_explodes_in_its_plane() {
    // A block with a line up its y axis, referenced in the XZ plane: the line stands up along Z.
    let block = "0\nBLOCK\n8\n0\n2\nB\n70\n0\n10\n0\n20\n0\n30\n0\n0\nLINE\n8\n0\n10\n0\n20\n0\n30\n0\n11\n0\n21\n1\n31\n0\n0\nENDBLK\n8\n0\n";
    let mut s = session(block, "0\nINSERT\n8\n0\n2\nB\n10\n2\n20\n0\n30\n0\n210\n0\n220\n-1\n230\n0\n");
    let h = only(&s).handle;
    s.execute("explode", &json!({ "handles": [h.hex()] })).unwrap();
    let EntityKind::Line(l) = only(&s).kind else { panic!("{:?}", only(&s).kind) };
    assert!((l.a - Vec3::new(2.0, 0.0, 0.0)).len() < 1e-9 && (l.b - Vec3::new(2.0, 0.0, 1.0)).len() < 1e-9, "{l:?}");
}

#[test]
fn shape_edits_of_a_tilted_object_are_refused_and_leave_it_alone() {
    let mut s = session("", &format!("{STANDING_CIRCLE}0\nLINE\n8\n0\n10\n0\n20\n-5\n11\n0\n21\n5\n"));
    let h = only(&s).handle;
    let before = only(&s);
    assert!(s.execute("trim", &json!({ "handle": h.hex(), "pick": [2.0, -3.0] })).is_err());
    assert!(s.execute("breakatpoint", &json!({ "handle": h.hex(), "at": [2.0, -3.0] })).is_err());
    assert_eq!(only(&s), before);
}

/// A polyline tilted back from the XY plane (extrusion (0,-0.6,0.8)): its own (x, y) shows in
/// plan at (x, 0.8 y).
const TILTED_PLINE: &str = "0\nLWPOLYLINE\n8\n0\n90\n3\n70\n0\n10\n0\n20\n0\n10\n10\n20\n0\n10\n10\n20\n10\n210\n0\n220\n-0.6\n230\n0.8\n";

fn handle_at(s: &Session, i: usize) -> Handle {
    s.doc().unwrap().model.iter().nth(i).unwrap().handle
}

fn entity(s: &Session, h: Handle) -> cadcraft_engine::doc::Entity {
    s.doc().unwrap().entity(h).map(|e| (**e).clone()).unwrap()
}

fn all(s: &Session) -> Vec<cadcraft_engine::doc::Entity> {
    s.doc().unwrap().model.iter().map(|e| (**e).clone()).collect()
}

#[test]
fn hot_grip_options_on_a_tilted_object_act_and_preview_in_plan() {
    let mut s = session("", TILTED_PLINE);
    let h = only(&s).handle;
    s.set_selection(vec![h]);
    let i = s.grips_of(h).unwrap().iter().position(|g| near(*g, Vec2::new(10.0, 0.0))).unwrap();
    s.start_grip_edit(&[(h, i)]).unwrap();
    s.cmdline("A").unwrap();
    // The preview runs from the cursor to the next vertex where plan view shows it, (10, 8).
    let at = Vec2::new(15.0, 4.0);
    let next = Vec2::new(10.0, 8.0);
    let pv = s.preview(at);
    let seg = |e: &cadcraft_engine::doc::Entity| match &e.kind {
        EntityKind::Line(l) => (near(l.a.xy(), at) && near(l.b.xy(), next)) || (near(l.b.xy(), at) && near(l.a.xy(), next)),
        _ => false,
    };
    assert!(pv.iter().any(seg), "{pv:?}");
    s.cmdline("15,4").unwrap();
    assert!(s.running.is_none());
    let e = only(&s);
    let EntityKind::LwPolyline(p) = &e.kind else { panic!() };
    // The new vertex is the point of its plane under the pick, and shows there in plan.
    assert!(p.vertices.len() == 4 && near(p.vertices[2].p, Vec2::new(15.0, 5.0)), "{p:?}");
    assert!(s.grips_of(h).unwrap().iter().any(|g| near(*g, at)));
    assert_eq!(e.common.extrusion, Vec3::new(0.0, -0.6, 0.8));
}

#[test]
fn offset_of_a_standing_circle_works_in_its_plane() {
    let mut s = session("", STANDING_CIRCLE);
    let h = only(&s).handle;
    // Outside the segment plan view shows: bigger; inside it: smaller. Same plane and height.
    for (side, r) in [([5.0, -3.0], 2.5), ([1.2, -3.0], 0.5)] {
        let out = s.execute("offset", &json!({ "handle": h.hex(), "distance": 1, "side": side })).unwrap();
        let e = entity(&s, Handle::parse_hex(out["handle"].as_str().unwrap()).unwrap());
        let EntityKind::Circle(c) = &e.kind else { panic!() };
        assert!((c.radius - r).abs() < 1e-9 && (c.center - Vec3::new(1.0, 2.0, 3.0)).len() < 1e-9, "{c:?}");
        assert_eq!(e.common.extrusion, Vec3::new(0.0, -1.0, 0.0));
    }
}

#[test]
fn offset_through_a_point_measures_in_a_tilted_plane() {
    // Radius 2 at the origin, tilted back: an ellipse in plan, 2 across and 1.6 up.
    let circle = "0\nCIRCLE\n8\n0\n10\n0\n20\n0\n30\n0\n40\n2\n210\n0\n220\n-0.6\n230\n0.8\n";
    let mut s = session("", circle);
    // Plan (0, 4) is own (0, 5): 3 from the circle in its plane (1.6 + 2.4 in plan).
    s.cmdline("offset t").unwrap();
    s.cmdline("2,0").unwrap();
    let pv = s.preview(Vec2::new(0.0, 4.0));
    assert!(pv.iter().all(|e| matches!(e.kind, EntityKind::Ellipse(_))) && !pv.is_empty(), "{pv:?}");
    s.cmdline("0,4").unwrap();
    s.cmdline("").unwrap();
    let made = all(&s).into_iter().nth(1).unwrap();
    let EntityKind::Circle(c) = &made.kind else { panic!("{made:?}") };
    // The through distance is measured to the tessellated circle, as in plan.
    assert!((c.radius - 5.0).abs() < 1e-3 && c.center == Vec3::ZERO, "{c:?}");
    assert_eq!(made.common.extrusion, Vec3::new(0.0, -0.6, 0.8));
}

#[test]
fn lengthen_of_a_tilted_arc_works_in_its_plane() {
    // Radius 2 from 0 to 90 degrees at elevation 1, tilted back: own (x, y) shows at
    // (x, 0.8 y) moved by the elevation along the extrusion, (0, -0.6).
    let arc = "0\nARC\n8\n0\n10\n0\n20\n0\n30\n1\n40\n2\n50\n0\n51\n90\n210\n0\n220\n-0.6\n230\n0.8\n";
    let mut s = session("", arc);
    let h = only(&s).handle;
    let plan = |x: f64, y: f64| [x, 0.8 * y - 0.6];
    let r = 2f64.sqrt();
    s.execute("lengthen", &json!({ "handle": h.hex(), "pick": plan(0.0, 2.0), "to": plan(-r, r) })).unwrap();
    let e = only(&s);
    let EntityKind::Arc(a) = &e.kind else { panic!() };
    assert!((a.end.to_degrees() - 135.0).abs() < 1e-6 && a.start.abs() < 1e-9, "{a:?}");
    assert!((a.center - Vec3::new(0.0, 0.0, 1.0)).len() < 1e-9 && a.radius == 2.0, "{a:?}");
    assert_eq!(e.common.extrusion, Vec3::new(0.0, -0.6, 0.8));
}

#[test]
fn divide_of_a_standing_circle_places_markers_in_its_plane() {
    let block = "0\nBLOCK\n8\n0\n2\nM\n70\n0\n10\n0\n20\n0\n30\n0\n0\nPOINT\n8\n0\n10\n0\n20\n0\n30\n0\n0\nENDBLK\n8\n0\n";
    let mut s = session(block, STANDING_CIRCLE);
    let h = only(&s).handle;
    let pts = s.execute("divide", &json!({ "handle": h.hex(), "segments": 4 })).unwrap();
    let marks = s.execute("divide", &json!({ "handle": h.hex(), "segments": 4, "block": "M" })).unwrap();
    let get = |v: &serde_json::Value| entity(&s, Handle::parse_hex(v.as_str().unwrap()).unwrap());
    let pts = pts["handles"].as_array().unwrap();
    assert_eq!(pts.len(), 4);
    for v in pts {
        // On the circle in 3D: centre (1, -3, 2), radius 1.5, in the plane y = -3.
        let EntityKind::Point(p) = get(v).kind else { panic!() };
        assert!((p.p.y + 3.0).abs() < 1e-9 && ((p.p - Vec3::new(1.0, -3.0, 2.0)).len() - 1.5).abs() < 1e-9, "{p:?}");
    }
    let marks = marks["handles"].as_array().unwrap();
    assert_eq!(marks.len(), 4);
    for v in marks {
        // In the circle's plane, at its elevation, on it.
        let e = get(v);
        let EntityKind::Insert(i) = &e.kind else { panic!() };
        assert_eq!(e.common.extrusion, Vec3::new(0.0, -1.0, 0.0));
        assert!(i.insert.z == 3.0 && (i.insert.xy().dist(Vec2::new(1.0, 2.0)) - 1.5).abs() < 1e-9, "{i:?}");
    }
}

#[test]
fn measure_of_a_tilted_arc_starts_at_the_end_picked_in_plan() {
    // Radius 2 from 0 to 90 degrees at elevation 1, tilted back: own (x, y) is world
    // (x, 0.8 y - 0.6, 0.6 y + 0.8).
    let arc = "0\nARC\n8\n0\n10\n0\n20\n0\n30\n1\n40\n2\n50\n0\n51\n90\n210\n0\n220\n-0.6\n230\n0.8\n";
    let mut s = session("", arc);
    let h = only(&s).handle;
    // Plan (0.9, 0.44) is own (0.9, 1.3): nearer the end (0, 2), though nearer the start as raw
    // coordinates.
    let out = s.execute("measure", &json!({ "handle": h.hex(), "length": 1, "from": [0.9, 0.44] })).unwrap();
    let first = Handle::parse_hex(out["handles"][0].as_str().unwrap()).unwrap();
    let EntityKind::Point(p) = entity(&s, first).kind else { panic!() };
    let a = std::f64::consts::FRAC_PI_2 - 0.5;
    let (x, y) = (2.0 * a.cos(), 2.0 * a.sin());
    assert!((p.p - Vec3::new(x, 0.8 * y - 0.6, 0.6 * y + 0.8)).len() < 1e-9, "{p:?}");
}

#[test]
fn exploding_a_tilted_or_raised_polyline_keeps_its_plane() {
    // (0,0) to (4,0), then a half circle to (4,2): standing (facing -Y) at elevation 3.
    let standing = "0\nLWPOLYLINE\n8\n0\n90\n3\n70\n0\n38\n3\n10\n0\n20\n0\n10\n4\n20\n0\n42\n1\n10\n4\n20\n2\n210\n0\n220\n-1\n230\n0\n";
    let mut s = session("", standing);
    let h = only(&s).handle;
    s.execute("explode", &json!({ "handles": [h.hex()] })).unwrap();
    let parts = all(&s);
    assert_eq!(parts.len(), 2);
    let EntityKind::Line(l) = &parts[0].kind else { panic!("{:?}", parts[0]) };
    // Own (x, y) at elevation 3 is world (x, -3, y).
    assert!((l.a - Vec3::new(0.0, -3.0, 0.0)).len() < 1e-9 && (l.b - Vec3::new(4.0, -3.0, 0.0)).len() < 1e-9, "{l:?}");
    let EntityKind::Arc(a) = &parts[1].kind else { panic!("{:?}", parts[1]) };
    assert!((a.center - Vec3::new(4.0, 1.0, 3.0)).len() < 1e-9 && (a.radius - 1.0).abs() < 1e-9, "{a:?}");
    assert_eq!(parts[1].common.extrusion, Vec3::new(0.0, -1.0, 0.0));

    // Flat at elevation 5: every piece stays at 5.
    let raised = "0\nLWPOLYLINE\n8\n0\n90\n3\n70\n0\n38\n5\n10\n0\n20\n0\n10\n4\n20\n0\n42\n1\n10\n4\n20\n2\n";
    let mut s = session("", raised);
    let h = only(&s).handle;
    s.execute("explode", &json!({ "handles": [h.hex()] })).unwrap();
    let parts = all(&s);
    let EntityKind::Line(l) = &parts[0].kind else { panic!() };
    assert!(l.a == Vec3::new(0.0, 0.0, 5.0) && l.b == Vec3::new(4.0, 0.0, 5.0), "{l:?}");
    let EntityKind::Arc(a) = &parts[1].kind else { panic!() };
    assert_eq!(a.center.z, 5.0);
}

#[test]
fn fillet_chamfer_join_and_pedit_refuse_tilted_objects_and_leave_them_alone() {
    let line = "0\nLINE\n8\n0\n10\n-5\n20\n-5\n11\n5\n21\n5\n";
    let mut s = session("", &format!("{STANDING_CIRCLE}{TILTED_PLINE}{line}"));
    let (c, p, l) = (handle_at(&s, 0), handle_at(&s, 1), handle_at(&s, 2));
    let before = all(&s);
    let refused = [
        ("fillet", json!({ "h1": c.hex(), "p1": [2.0, -3.0], "h2": l.hex(), "p2": [4.0, 4.0], "radius": 1 })),
        ("fillet", json!({ "h1": l.hex(), "p1": [4.0, 4.0], "h2": p.hex(), "p2": [5.0, 0.0], "radius": 1 })),
        ("fillet", json!({ "handle": p.hex(), "polyline": true, "radius": 1 })),
        ("chamfer", json!({ "handle": p.hex(), "polyline": true, "d1": 1 })),
        ("join", json!({ "handles": [p.hex(), l.hex()] })),
        ("pedit", json!({ "handle": p.hex(), "option": "join", "handles2": [l.hex()] })),
        ("pedit", json!({ "handle": p.hex(), "option": "vertex", "index": 1, "action": "move", "to": [3, 3] })),
    ];
    for (cmd, params) in refused {
        let r = s.execute(cmd, &params);
        assert!(r.as_ref().is_err_and(|e| e.to_string().contains("tilted plane")), "{cmd} {params}: {r:?}");
        assert_eq!(all(&s), before, "{cmd} {params}");
    }
    // With a flat polyline first, the refusal still comes before any edit.
    let flat = s.execute("pline", &json!({ "vertices": [[20, 0], [30, 0], [30, 10]] })).unwrap()["handle"].clone();
    let before = all(&s);
    let r = s.execute("pedit", &json!({ "handles": [flat, p.hex()], "option": "vertex", "index": 1, "action": "move", "to": [25, 5] }));
    assert!(r.is_err());
    assert_eq!(all(&s), before, "nothing edited when one object is refused");
    // In-plane PEDIT options still work, and keep the plane.
    s.execute("pedit", &json!({ "handle": p.hex(), "option": "close" })).unwrap();
    let e = entity(&s, p);
    assert!(matches!(&e.kind, EntityKind::LwPolyline(q) if q.closed) && e.common.extrusion == Vec3::new(0.0, -0.6, 0.8));
}
