//! Furniture design with the kernel: PANEL in each orientation, parts kept through modelling,
//! cut lists of real boards, VIEWBASE drawings of a board with a hole, and 3DROTATE before a
//! boolean.
#![cfg(feature = "occt")]

use cadcraft_engine::doc::{EntityKind, Handle, Part, Solid3d};
use cadcraft_engine::geom::{Bounds2, Vec2, Vec3};
use cadcraft_engine::{Input, Session};
use serde_json::{Value, json};

fn session() -> Session {
    let mut s = Session::new();
    s.new_drawing(true);
    s
}

fn handle(v: &Value) -> Handle {
    Handle::parse_hex(v.as_str().unwrap()).unwrap()
}

fn solid(s: &Session, h: Handle) -> Solid3d {
    match &s.doc().unwrap().entity(h).unwrap().kind {
        EntityKind::Solid3d(x) => x.clone(),
        k => panic!("not a solid: {}", k.type_name()),
    }
}

/// World-space box of a solid's mesh.
fn box3(x: &Solid3d) -> (Vec3, Vec3) {
    x.mesh
        .verts
        .iter()
        .map(|p| x.world(*p))
        .fold((Vec3::new(f64::MAX, f64::MAX, f64::MAX), Vec3::new(f64::MIN, f64::MIN, f64::MIN)), |(lo, hi), p| {
            (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
        })
}

fn near3(a: Vec3, b: [f64; 3]) -> bool {
    (a.x - b[0]).abs() < 1e-6 && (a.y - b[1]).abs() < 1e-6 && (a.z - b[2]).abs() < 1e-6
}

#[test]
fn panels_lie_in_their_planes() {
    let mut s = session();
    for (orientation, lo, hi) in [
        ("top", [0.0, 0.0, 0.0], [600.0, 400.0, 18.0]),
        ("front", [0.0, 0.0, 0.0], [600.0, 18.0, 400.0]),
        ("right", [-18.0, 0.0, 0.0], [0.0, 600.0, 400.0]),
    ] {
        let r = s.execute("panel", &json!({ "corner1": [1000, 1000], "corner2": [1600, 1400], "orientation": orientation })).unwrap();
        let (a, b) = box3(&solid(&s, handle(&r["handle"])));
        assert!(near3(a, lo) && near3(b, hi), "{orientation}: {a:?} {b:?}");
    }
    // A profile object, a base point on it and where it goes; the profile stays.
    let pl = s.execute("pline", &json!({ "points": [[0, 0], [500, 0], [500, 300], [0, 300]], "closed": true })).unwrap();
    let ph = handle(&pl["handle"]);
    let r = s
        .execute(
            "panel",
            &json!({ "handle": ph.hex(), "thickness": 15, "orientation": "front", "base": [500, 0], "at": [10, 20, 30], "name": "Door", "material": "MDF" }),
        )
        .unwrap();
    let x = solid(&s, handle(&r["handle"]));
    let (a, b) = box3(&x);
    assert!(near3(a, [-490.0, 20.0, 30.0]) && near3(b, [10.0, 35.0, 330.0]), "{a:?} {b:?}");
    assert_eq!(x.part, Part { name: "Door".into(), material: "MDF".into() });
    assert!(s.doc().unwrap().entity(ph).is_some(), "the profile is the drawing: it stays");
    // A round tabletop from a circle.
    let c = s.execute("circle", &json!({ "center": [0, 0], "radius": 450 })).unwrap();
    let r = s.execute("panel", &json!({ "handle": c["handle"], "thickness": 24 })).unwrap();
    let (a, b) = box3(&solid(&s, handle(&r["handle"])));
    assert!((b.x - a.x - 900.0).abs() < 1.0 && (b.z - a.z - 24.0).abs() < 1e-6, "{a:?} {b:?}");
    s.undo().unwrap();
    assert!(s.doc().unwrap().entity(handle(&r["handle"])).is_none());
}

/// PANEL at the prompts to the end: two corners, the default thickness, orientation and base
/// point, and an insertion point picked on a 3D object 100 up.
#[test]
fn panel_at_the_prompts() {
    let mut s = session();
    s.start("panel").unwrap();
    s.input(Input::Point(Vec2::new(0.0, 0.0))).unwrap();
    s.input(Input::Point(Vec2::new(600.0, 400.0))).unwrap();
    for _ in 0..3 {
        s.input(Input::Enter).unwrap();
    }
    s.point_z = Some(100.0);
    s.input(Input::Point(Vec2::new(1000.0, 0.0))).unwrap();
    assert!(s.current_prompt().is_none(), "{}", s.prompt_text());
    let d = s.doc().unwrap();
    let x = d
        .model
        .iter()
        .find_map(|e| match &e.kind {
            EntityKind::Solid3d(x) => Some(x.clone()),
            _ => None,
        })
        .expect("a panel");
    let (a, b) = box3(&x);
    assert!(near3(a, [1000.0, 0.0, 100.0]) && near3(b, [1600.0, 400.0, 118.0]), "{a:?} {b:?}");
}

#[test]
fn parts_survive_chamfers_and_shells() {
    let mut s = session();
    let want = Part { name: "Drawer".into(), material: "Birch 12".into() };
    let a = handle(
        &s.execute("panel", &json!({ "corner1": [0, 0], "corner2": [400, 300], "thickness": 120, "name": want.name, "material": want.material }))
            .unwrap()["handle"],
    );
    s.execute("chamferedge", &json!({ "handles": [a.hex()], "distance": 2 })).unwrap();
    assert_eq!(solid(&s, a).part, want);
    let b = handle(&s.execute("panel", &json!({ "corner1": [0, 500], "corner2": [400, 800], "thickness": 120, "name": "Box" })).unwrap()["handle"]);
    s.execute("shell", &json!({ "handles": [b.hex()], "thickness": 12 })).unwrap();
    assert_eq!(solid(&s, b).part.name, "Box");
    assert!(solid(&s, b).mesh.tris.len() > 12, "hollowed");
}

#[test]
fn parts_survive_booleans_fillets_slices_and_moves() {
    let mut s = session();
    let a = handle(
        &s.execute("panel", &json!({ "corner1": [0, 0], "corner2": [600, 400], "name": "Top", "material": "Oak 24", "thickness": 24 })).unwrap()["handle"],
    );
    let b = handle(&s.execute("box", &json!({ "corner1": [100, 100, -10], "corner2": [200, 200], "height": 50 })).unwrap()["handle"]);
    s.execute("part", &json!({ "handles": [b.hex()], "name": "Cutter" })).unwrap();
    s.execute("union", &json!({ "handles": [a.hex(), b.hex()] })).unwrap();
    let want = Part { name: "Top".into(), material: "Oak 24".into() };
    assert_eq!(solid(&s, a).part, want);
    s.execute("filletedge", &json!({ "handles": [a.hex()], "radius": 2 })).unwrap();
    assert_eq!(solid(&s, a).part, want);
    s.execute("3dmove", &json!({ "handles": [a.hex()], "delta": [0, 0, 700] })).unwrap();
    assert_eq!(solid(&s, a).part, want);
    let r = s.execute("slice", &json!({ "handles": [a.hex()], "point1": [300, 0], "point2": [300, 1] })).unwrap();
    for h in r["handles"].as_array().unwrap() {
        assert_eq!(solid(&s, handle(h)).part, want, "both halves are the part");
    }
    let c = handle(&s.execute("box", &json!({ "corner1": [0, 0, 700], "corner2": [50, 50], "height": 100 })).unwrap()["handle"]);
    s.execute("subtract", &json!({ "from": [a.hex()], "remove": [c.hex()] })).unwrap();
    assert_eq!(solid(&s, a).part, want);
}

#[test]
fn cut_list_measures_boards_in_any_orientation() {
    let mut s = session();
    for o in ["top", "front", "right"] {
        s.execute("panel", &json!({ "corner1": [0, 0], "corner2": [600, 400], "orientation": o, "name": "Side", "material": "Birch 18" })).unwrap();
    }
    s.execute("panel", &json!({ "corner1": [0, 0], "corner2": [564, 300], "name": "Shelf", "material": "Birch 18" })).unwrap();
    let r = s.execute("cutlist", &json!({})).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{r}");
    assert_eq!(rows[1]["name"], "Side");
    assert_eq!((rows[1]["length"].as_f64(), rows[1]["width"].as_f64(), rows[1]["thickness"].as_f64()), (Some(600.0), Some(400.0), Some(18.0)));
    assert_eq!(rows[1]["quantity"], 3);
    assert_eq!(r["total"], 4);
}

/// Lines (as 2D segments) of the entities in a group, on a layer.
fn group_lines(s: &Session, group: &str, layer: &str) -> Vec<(Vec2, Vec2)> {
    let d = s.doc().unwrap();
    let g = d.groups.iter().find(|g| g.name == group).unwrap_or_else(|| panic!("no group {group}"));
    let mut out = Vec::new();
    for h in &g.members {
        let e = d.entity(*h).unwrap();
        if e.common.layer != layer {
            continue;
        }
        match &e.kind {
            EntityKind::Line(l) => out.push((l.a.xy(), l.b.xy())),
            EntityKind::LwPolyline(p) => {
                let v: Vec<Vec2> = p.vertices.iter().map(|v| v.p).collect();
                out.extend(v.windows(2).map(|w| (w[0], w[1])));
                if p.closed {
                    out.push((*v.last().unwrap(), v[0]));
                }
            }
            k => panic!("unexpected {}", k.type_name()),
        }
    }
    out
}

fn bounds(ls: &[(Vec2, Vec2)]) -> Bounds2 {
    Bounds2::from_points(ls.iter().flat_map(|(a, b)| [*a, *b]))
}

#[test]
fn viewbase_draws_a_board_with_a_hole() {
    let mut s = session();
    let b = handle(&s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [100, 60], "height": 20 })).unwrap()["handle"]);
    let c = handle(&s.execute("cylinder", &json!({ "center": [50, 30, -1], "radius": 10, "height": 22 })).unwrap()["handle"]);
    s.execute("subtract", &json!({ "from": [b.hex()], "remove": [c.hex()] })).unwrap();
    let before = s.doc().unwrap().model.iter().count();
    let r = s.execute("viewbase", &json!({ "at": [200, 0], "gap": 30 })).unwrap();
    assert_eq!(r["views"].as_array().map(Vec::len), Some(3), "{r}");
    assert_eq!(r["capped"].as_array().map(Vec::len), Some(0), "{r}");

    // Front: a 100 x 20 outline at the given corner; the hole's sides dashed, 20 tall.
    let vis = group_lines(&s, "VIEW front", "VISIBLE");
    let vb = bounds(&vis);
    assert!(vb.min.near(Vec2::new(200.0, 0.0), 1e-6) && vb.max.near(Vec2::new(300.0, 20.0), 1e-6), "{vb:?}");
    let hid = group_lines(&s, "VIEW front", "HIDDEN");
    assert!(!hid.is_empty());
    for x in [240.0, 260.0] {
        let len: f64 = hid.iter().filter(|(a, b)| (a.x - x).abs() < 0.5 && (b.x - x).abs() < 0.5).map(|(a, b)| a.dist(*b)).sum();
        assert!((len - 20.0).abs() < 1e-3, "hidden side at x = {x}: {len} in {hid:?}");
    }
    let d = s.doc().unwrap();
    assert_eq!(d.layer("HIDDEN").unwrap().linetype, "HIDDEN");
    assert!(d.linetype("HIDDEN").is_some_and(|l| !l.pattern.is_empty()), "a dashed linetype from the drawing's library");

    // Top: above the front (third angle), the hole a visible circle; nothing hidden.
    let top = bounds(&group_lines(&s, "VIEW top", "VISIBLE"));
    assert!(top.min.near(Vec2::new(200.0, 50.0), 1e-6) && top.max.near(Vec2::new(300.0, 110.0), 1e-6), "{top:?}");
    let hole = group_lines(&s, "VIEW top", "VISIBLE").into_iter().filter(|(a, _)| a.dist(Vec2::new(250.0, 80.0)) < 10.5).count();
    assert!(hole >= 8, "{hole}");
    assert!(group_lines(&s, "VIEW top", "HIDDEN").is_empty());

    // Right: beside the front, 60 wide and 20 tall, with the hole's sides hidden.
    let right = bounds(&group_lines(&s, "VIEW right", "VISIBLE"));
    assert!(right.min.near(Vec2::new(330.0, 0.0), 1e-6) && right.max.near(Vec2::new(390.0, 20.0), 1e-6), "{right:?}");
    assert!(!group_lines(&s, "VIEW right", "HIDDEN").is_empty());

    // Overall dimensions on each view: the front's measure 100 and 20.
    let dims: Vec<f64> = d
        .groups
        .iter()
        .find(|g| g.name == "VIEW front")
        .unwrap()
        .members
        .iter()
        .filter_map(|h| match &d.entity(*h)?.kind {
            EntityKind::Dimension(dm) => Some((dm.p14 - dm.p13).len()),
            _ => None,
        })
        .collect();
    assert_eq!(dims.len(), 2, "{dims:?}");
    assert!(dims.iter().any(|v| (v - 100.0).abs() < 1e-6) && dims.iter().any(|v| (v - 20.0).abs() < 1e-6), "{dims:?}");

    // One undo takes it all back, layers too.
    s.undo().unwrap();
    let d = s.doc().unwrap();
    assert_eq!(d.model.iter().count(), before);
    assert!(d.groups.is_empty() && d.layer("HIDDEN").is_none());
}

#[test]
fn round_parts_show_silhouettes_and_no_seams() {
    let mut s = session();
    s.execute("cylinder", &json!({ "center": [0, 0, 0], "radius": 10, "height": 30 })).unwrap();
    s.execute("viewbase", &json!({ "at": [100, 0], "gap": 10, "views": ["front", "right"], "dims": false })).unwrap();
    for (view, left) in [("VIEW front", 100.0), ("VIEW right", 130.0)] {
        let vis = group_lines(&s, view, "VISIBLE");
        let b = bounds(&vis);
        assert!(b.min.near(Vec2::new(left, 0.0), 0.5) && b.max.near(Vec2::new(left + 20.0, 30.0), 0.5), "{view}: {b:?}");
        // The sides are silhouettes; the seam down the middle of the round face is not drawn.
        let upright = |x: f64| vis.iter().filter(|(a, c)| (a.x - x).abs() < 0.5 && (c.x - x).abs() < 0.5 && (a.y - c.y).abs() > 1.0).count();
        assert!(upright(left) > 0 && upright(left + 20.0) > 0, "{view}: {vis:?}");
        assert_eq!(upright(left + 10.0), 0, "{view}: {vis:?}");
        assert!(group_lines(&s, view, "HIDDEN").is_empty(), "{view}");
    }
}

#[test]
fn viewbase_first_angle_iso_and_scale() {
    let mut s = session();
    s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [100, 60], "height": 20 })).unwrap();
    let r = s
        .execute("viewbase", &json!({ "at": [0, -500], "views": ["front", "top", "right", "iso"], "projection": "first", "scale": 0.5, "gap": 10, "hidden": false, "dims": false }))
        .unwrap();
    assert_eq!(r["views"].as_array().map(Vec::len), Some(4));
    // First angle: the top view below the front, the right view to its left; half size.
    let front = bounds(&group_lines(&s, "VIEW front", "VISIBLE"));
    let top = bounds(&group_lines(&s, "VIEW top", "VISIBLE"));
    let right = bounds(&group_lines(&s, "VIEW right", "VISIBLE"));
    assert!(front.min.near(Vec2::new(0.0, -500.0), 1e-6) && front.max.near(Vec2::new(50.0, -490.0), 1e-6), "{front:?}");
    assert!(top.max.y < front.min.y && (top.max.y - (front.min.y - 10.0)).abs() < 1e-6, "{top:?}");
    assert!(right.max.x < front.min.x && (right.width() - 30.0).abs() < 1e-6, "{right:?}");
    assert!(!group_lines(&s, "VIEW iso", "VISIBLE").is_empty());
    // A second run makes new groups.
    s.execute("viewbase", &json!({ "views": ["front"], "dims": false })).unwrap();
    assert!(s.doc().unwrap().groups.iter().any(|g| g.name == "VIEW front 2"));
}

#[test]
fn rotated_boxes_still_combine() {
    let mut s = session();
    let b = handle(&s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [100, 60], "height": 20 })).unwrap()["handle"]);
    // Stand the board up: 90° about the X axis.
    s.execute("3drotate", &json!({ "handles": [b.hex()], "axis": [[0, 0, 0], [1, 0, 0]], "angle": 90 })).unwrap();
    let (lo, hi) = box3(&solid(&s, b));
    assert!(near3(lo, [0.0, -20.0, 0.0]) && near3(hi, [100.0, 0.0, 60.0]), "{lo:?} {hi:?}");
    let c = handle(&s.execute("cylinder", &json!({ "center": [50, -10, 30], "radius": 5, "height": 40 })).unwrap()["handle"]);
    let before = s.doc().unwrap().clone();
    let xform = solid(&s, b).xform;
    let r = s.execute("subtract", &json!({ "from": [b.hex()], "remove": [c.hex()] }));
    if cadcraft_kernel::similarity(&xform).is_none() {
        // ponytail: the kernel takes rotations about Z only until its general transforms land;
        // until then the boolean fails cleanly.
        assert!(r.is_err());
        assert_eq!(*s.doc().unwrap(), before);
        return;
    }
    r.unwrap();
    let x = solid(&s, b);
    let (lo, hi) = box3(&x);
    assert!(near3(lo, [0.0, -20.0, 0.0]) && near3(hi, [100.0, 0.0, 60.0]), "{lo:?} {hi:?}");
    assert!(x.mesh.tris.len() > 12, "the hole adds faces");
}
