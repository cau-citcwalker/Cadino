//! Objects in tilted planes (a DXF extrusion other than Z) are picked, snapped, moved and
//! grip-edited where plan view shows them, not at their own (OCS) coordinates.

use cadcraft_engine::Session;
use cadcraft_engine::doc::{EntityKind, Space};
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
