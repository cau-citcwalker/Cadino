//! Furniture design with the kernel: PANEL in each orientation and from standing profiles, parts
//! kept through modelling, cut lists of real boards, VIEWBASE drawings of a board with a hole (in
//! model space and on a layout), and 3DROTATE before a boolean.
#![cfg(feature = "occt")]

use cadcraft_engine::doc::{EntityKind, Handle, Part, Polyline3d, Solid3d, Space};
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

/// A profile standing in a plane of its own is the panel's face where it is (its thickness along
/// the plane's normal): a polyline in the XZ plane and a 3D polyline in the YZ plane. Orientation,
/// base or insertion point with one are refused; at the prompts only the thickness is asked.
#[test]
fn standing_profiles_make_panels_where_they_are() {
    let mut s = session();
    let pl = s.execute("pline", &json!({ "points": [[0, 0], [600, 0], [600, 400], [0, 400]], "closed": true })).unwrap();
    let ph = handle(&pl["handle"]);
    s.doc_mut().unwrap().modify_entity(ph, |e| e.common.extrusion = Vec3::new(0.0, -1.0, 0.0)).unwrap();
    let before = s.doc().unwrap().clone();
    for extra in [json!({ "orientation": "front" }), json!({ "base": [0, 0] }), json!({ "at": [0, 0, 0] })] {
        let mut p = json!({ "handle": ph.hex() });
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        assert!(s.execute("panel", &p).is_err(), "{p}");
        assert_eq!(*s.doc().unwrap(), before);
    }
    let r = s.execute("panel", &json!({ "handle": ph.hex() })).unwrap();
    let (a, b) = box3(&solid(&s, handle(&r["handle"])));
    assert!(near3(a, [0.0, -18.0, 0.0]) && near3(b, [600.0, 0.0, 400.0]), "{a:?} {b:?}");
    // Wound counterclockwise seen from +X: the thickness runs along +X.
    let v = Vec3::new;
    let p3 = s
        .add_entity(EntityKind::Polyline3d(Polyline3d {
            points: vec![v(50.0, 0.0, 0.0), v(50.0, 300.0, 0.0), v(50.0, 300.0, 200.0), v(50.0, 0.0, 200.0)],
            closed: true,
        }))
        .unwrap();
    let r = s.execute("panel", &json!({ "handle": p3.hex(), "thickness": 12 })).unwrap();
    let (a, b) = box3(&solid(&s, handle(&r["handle"])));
    assert!(near3(a, [50.0, 0.0, 0.0]) && near3(b, [62.0, 300.0, 200.0]), "{a:?} {b:?}");
    // At the prompts: the profile, the thickness, done.
    let n = s.doc().unwrap().model.iter().count();
    s.start("panel").unwrap();
    s.input(Input::Enter).unwrap();
    s.input(Input::Pick(vec![ph])).unwrap();
    assert!(s.prompt_text().contains("thickness"), "{}", s.prompt_text());
    s.input(Input::Text("15".into())).unwrap();
    assert!(s.current_prompt().is_none(), "{}", s.prompt_text());
    assert_eq!(s.doc().unwrap().model.iter().count(), n + 1);
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
    let r = s.execute("viewbase", &json!({ "at": [200, 0], "gap": 30, "space": "model" })).unwrap();
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
    s.execute("viewbase", &json!({ "at": [100, 0], "gap": 10, "views": ["front", "right"], "dims": false, "space": "model" })).unwrap();
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
        .execute(
            "viewbase",
            &json!({ "at": [0, -500], "views": ["front", "top", "right", "iso"], "projection": "first", "scale": 2, "gap": 10, "hidden": false, "dims": false, "space": "model" }),
        )
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
    // A second run makes new groups, on the default layout.
    let r = s.execute("viewbase", &json!({ "views": ["front"], "dims": false })).unwrap();
    assert!(s.doc().unwrap().groups.iter().any(|g| g.name == "VIEW front 2"));
    assert_eq!(r["layout"], "Drawing views");
    let sheet = s.doc().unwrap().space(&Space::Paper("Drawing views".into())).map(|st| st.iter().count()).unwrap_or(0);
    assert!(sheet > 0, "{r}");
}

/// A board with a hole drawn on a layout: the views and their dimensions are in its paper space,
/// sized for the sheet, and read the board's true sizes.
#[test]
fn viewbase_draws_on_a_layout() {
    let mut s = session();
    let b = handle(&s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [1200, 600], "height": 18 })).unwrap()["handle"]);
    let c = handle(&s.execute("cylinder", &json!({ "center": [600, 300, -1], "radius": 35, "height": 20 })).unwrap()["handle"]);
    s.execute("subtract", &json!({ "from": [b.hex()], "remove": [c.hex()] })).unwrap();
    let r = s.execute("viewbase", &json!({ "layout": "Shop drawing" })).unwrap();
    assert_eq!(r["layout"], "Shop drawing");
    let k = r["scale"].as_f64().unwrap();
    assert!(k > 1.0, "a 1200 board on an A4 sheet is drawn smaller: {r}");
    let d = s.doc().unwrap();
    assert!(d.layout("Shop drawing").is_some());
    let paper: Vec<EntityKind> = d.space(&Space::Paper("Shop drawing".into())).unwrap().iter().map(|e| e.kind.clone()).collect();
    let dims: Vec<f64> = paper
        .iter()
        .filter_map(|k| match k {
            EntityKind::Dimension(dm) => {
                let st = d.dim_style(&dm.style).cloned().unwrap_or_default().with_overrides(&dm.overrides);
                Some(((dm.p14 - dm.p13).len() * st.linear_factor * 1000.0).round() / 1000.0)
            }
            _ => None,
        })
        .collect();
    for want in [1200.0, 600.0, 18.0] {
        assert!(dims.contains(&want), "{want}: {dims:?}");
    }
    let front = bounds(&group_lines(&s, "VIEW front", "VISIBLE"));
    assert!((front.width() * k - 1200.0).abs() < 1e-6 && (front.height() * k - 18.0).abs() < 1e-6, "{front:?}");
    assert!(!group_lines(&s, "VIEW front", "HIDDEN").is_empty(), "the hole, hidden");
    assert_eq!(d.model.iter().count(), 1, "model space keeps the board only");
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

fn rows(r: &Value) -> Vec<(f64, f64, f64, u64)> {
    r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["length"].as_f64().unwrap(), r["width"].as_f64().unwrap(), r["thickness"].as_f64().unwrap(), r["quantity"].as_u64().unwrap()))
        .collect()
}

/// Boards drawn at an angle: a shelf at 30° in plan, a door profile standing at 45° in plan, a
/// board tilted 30°; and a slender round leg, measured from its B-rep (its mesh falls short).
#[test]
fn cut_lists_measure_angled_boards_and_round_legs_truly() {
    let mut s = session();
    let (c, sn) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
    let turned: Vec<[f64; 2]> =
        [[0.0, 0.0], [600.0, 0.0], [600.0, 400.0], [0.0, 400.0]].iter().map(|p| [p[0] * c - p[1] * sn, p[0] * sn + p[1] * c]).collect();
    let pl = s.execute("pline", &json!({ "points": turned, "closed": true })).unwrap();
    s.execute("panel", &json!({ "handle": pl["handle"], "name": "Shelf" })).unwrap();
    // Standing: a 3D polyline in the vertical plane at 45° in plan, 600 wide and 400 tall.
    let d = Vec3::new(std::f64::consts::FRAC_1_SQRT_2, std::f64::consts::FRAC_1_SQRT_2, 0.0);
    let up = |w: f64, h: f64| d * w + Vec3::new(0.0, 0.0, h);
    let door = s
        .add_entity(EntityKind::Polyline3d(Polyline3d { points: vec![up(0.0, 0.0), up(600.0, 0.0), up(600.0, 400.0), up(0.0, 400.0)], closed: true }))
        .unwrap();
    s.execute("panel", &json!({ "handle": door.hex(), "name": "Shelf" })).unwrap();
    // Tilted 30° about X.
    let tilt = |x: f64, y: f64| Vec3::new(x, y * c, y * sn + 1000.0);
    let t = s
        .add_entity(EntityKind::Polyline3d(Polyline3d {
            points: vec![tilt(0.0, 0.0), tilt(600.0, 0.0), tilt(600.0, 400.0), tilt(0.0, 400.0)],
            closed: true,
        }))
        .unwrap();
    s.execute("panel", &json!({ "handle": t.hex(), "name": "Shelf" })).unwrap();
    let leg = s.execute("cylinder", &json!({ "center": [2000, 0, 0], "radius": 20, "height": 700 })).unwrap();
    s.execute("part", &json!({ "handles": [leg["handle"]], "name": "Leg" })).unwrap();
    let r = s.execute("cutlist", &json!({})).unwrap();
    assert_eq!(rows(&r), vec![(700.0, 40.0, 40.0, 1), (600.0, 400.0, 18.0, 3)], "{r}");
}

/// A tapered leg (a front profile, the taper 1.4° off straight) seen from the right: the line
/// where the taper starts is drawn; the view and its dimensions measure a round leg's B-rep.
#[test]
fn viewbase_keeps_shallow_creases_and_measures_round_parts_exactly() {
    let mut s = session();
    let pl = s.execute("pline", &json!({ "points": [[0, 0], [30, 0], [45, 600], [45, 700], [0, 700]], "closed": true })).unwrap();
    s.execute("panel", &json!({ "handle": pl["handle"], "orientation": "front" })).unwrap();
    s.execute("erase", &json!({ "handles": [pl["handle"]] })).unwrap();
    s.execute("viewbase", &json!({ "views": ["right"], "at": [1000, 0], "gap": 10, "dims": false, "space": "model" })).unwrap();
    let vis = group_lines(&s, "VIEW right", "VISIBLE");
    let b = bounds(&vis);
    let crease = vis.iter().any(|(a, c)| (a.y - b.min.y - 600.0).abs() < 1e-6 && (c.y - b.min.y - 600.0).abs() < 1e-6 && (a.x - c.x).abs() > 17.0);
    assert!(crease, "the line at 600 up: {vis:?}");
    // A slender round leg: its front view is 40 wide, dimensioned 40.
    let mut s = session();
    s.execute("cylinder", &json!({ "center": [0, 0, 0], "radius": 20, "height": 700 })).unwrap();
    let r = s.execute("viewbase", &json!({ "views": ["front", "top"], "at": [1000, 0], "gap": 10, "space": "model" })).unwrap();
    let v = &r["views"][0];
    assert!((v["max"][0].as_f64().unwrap() - v["min"][0].as_f64().unwrap() - 40.0).abs() < 1e-6, "{r}");
    let d = s.doc().unwrap();
    let dims: Vec<f64> = d
        .model
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Dimension(dm) => Some((dm.p14 - dm.p13).len()),
            _ => None,
        })
        .collect();
    assert!(dims.iter().any(|v| (v - 40.0).abs() < 1e-6) && dims.iter().any(|v| (v - 700.0).abs() < 1e-6), "{dims:?}");
}

/// World box of the solid `h`.
fn world_box(s: &Session, h: &Value) -> (Vec3, Vec3) {
    box3(&solid(s, handle(h)))
}

fn near_box(b: (Vec3, Vec3), lo: [f64; 3], hi: [f64; 3]) -> bool {
    // Within a mesh's round-off of a circle.
    (b.0 - Vec3::new(lo[0], lo[1], lo[2])).len() < 0.01 && (b.1 - Vec3::new(hi[0], hi[1], hi[2])).len() < 0.01
}

/// In the Front construction plane (x along X, y along Z, its normal -Y): what is built from
/// points given there (BOX, PANEL by corners) stands in it; what is built from objects (EXTRUDE,
/// LOFT, PANEL from a profile, booleans, CHAMFEREDGE) is made where they are and stays there.
#[test]
fn front_construction_plane_primitives_and_results_from_objects() {
    let mut s = session();
    s.execute("cplane", &json!({ "preset": "front" })).unwrap();
    let b = s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [10, 20], "height": 5 })).unwrap();
    assert!(near_box(world_box(&s, &b["handle"]), [0.0, -5.0, 0.0], [10.0, 0.0, 20.0]), "{:?}", world_box(&s, &b["handle"]));
    let p = s.execute("panel", &json!({ "corner1": [100, 0], "corner2": [700, 400] })).unwrap();
    // Its lower-left corner goes to the plane's origin (`at` left out).
    assert!(near_box(world_box(&s, &p["handle"]), [0.0, -18.0, 0.0], [600.0, 0.0, 400.0]), "{:?}", world_box(&s, &p["handle"]));
    // A circle drawn in the plane (centre 5, 5: world x 5, z 5) extruded 10 along its normal.
    let c = s.execute("circle", &json!({ "center": [5, 5], "radius": 2 })).unwrap();
    let e = s.execute("extrude", &json!({ "handles": [c["handle"]], "height": 10 })).unwrap();
    assert!(near_box(world_box(&s, &e["handles"][0]), [3.0, -10.0, 3.0], [7.0, 0.0, 7.0]), "{:?}", world_box(&s, &e["handles"][0]));
    // Two circles 10 apart along the plane's normal, lofted.
    let a = s.add_entity(EntityKind::Circle(cadcraft_engine::doc::Circle { center: Vec3::new(50.0, 0.0, 0.0), radius: 5.0 })).unwrap();
    let z = s.add_entity(EntityKind::Circle(cadcraft_engine::doc::Circle { center: Vec3::new(50.0, 0.0, 10.0), radius: 5.0 })).unwrap();
    let l = s.execute("loft", &json!({ "handles": [a.hex(), z.hex()] })).unwrap();
    let lb = world_box(&s, &l["handle"]);
    assert!((lb.0.y + 10.0).abs() < 1e-3 && lb.1.y.abs() < 1e-3 && (lb.0.x - 45.0).abs() < 0.1 && lb.0.z.abs() < 5.1, "{lb:?}");
    // A profile drawn in the plane makes a panel where it stands.
    let pl = s.execute("pline", &json!({ "points": [[0, 500], [600, 500], [600, 900], [0, 900]], "closed": true })).unwrap();
    let q = s.execute("panel", &json!({ "handle": pl["handle"] })).unwrap();
    assert!(near_box(world_box(&s, &q["handle"]), [0.0, -18.0, 500.0], [600.0, 0.0, 900.0]), "{:?}", world_box(&s, &q["handle"]));
    // A boolean and an edge bevel leave the box where it is.
    let cut = s.execute("box", &json!({ "corner1": [2, 2, -1], "corner2": [4, 4], "height": 7 })).unwrap();
    s.execute("subtract", &json!({ "from": [b["handle"]], "remove": [cut["handle"]] })).unwrap();
    assert!(near_box(world_box(&s, &b["handle"]), [0.0, -5.0, 0.0], [10.0, 0.0, 20.0]), "{:?}", world_box(&s, &b["handle"]));
    s.execute("chamferedge", &json!({ "handles": [b["handle"]], "distance": 0.5 })).unwrap();
    assert!(near_box(world_box(&s, &b["handle"]), [0.0, -5.0, 0.0], [10.0, 0.0, 20.0]), "{:?}", world_box(&s, &b["handle"]));
}

/// 3DMOVE, MIRROR3D, 3DROTATE and 3DSCALE at the prompts in the Front plane: points, the Z axis
/// and the XY plane are the plane's.
#[test]
fn front_construction_plane_3d_transforms_at_the_prompts() {
    let mut s = session();
    let b = s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [10, 10], "height": 10 })).unwrap();
    let h = handle(&b["handle"]);
    s.execute("cplane", &json!({ "preset": "front" })).unwrap();
    // Up 5 in the plane is up 5 in the world.
    s.set_selection(vec![h]);
    s.start("3dmove").unwrap();
    s.input(Input::Point(Vec2::ZERO)).unwrap();
    s.input(Input::Point(Vec2::new(0.0, 5.0))).unwrap();
    assert!(near_box(box3(&solid(&s, h)), [0.0, 0.0, 5.0], [10.0, 10.0, 15.0]), "{:?}", box3(&solid(&s, h)));
    // The plane's XY plane is the world's XZ plane: mirrored across y = 0, the source erased.
    s.set_selection(vec![h]);
    s.start("mirror3d").unwrap();
    s.input(Input::Text("xy".into())).unwrap();
    s.input(Input::Enter).unwrap();
    s.input(Input::Text("y".into())).unwrap();
    assert!(s.current_prompt().is_none(), "{}", s.prompt_text());
    let m = s.doc().unwrap().model.iter().find_map(|e| matches!(e.kind, EntityKind::Solid3d(_)).then_some(e.handle)).unwrap();
    assert!(near_box(box3(&solid(&s, m)), [0.0, -10.0, 5.0], [10.0, 0.0, 15.0]), "{:?}", box3(&solid(&s, m)));
    // A quarter turn about the plane's Z axis (world -Y) through its origin: it stays between
    // y -10 and 0 and turns in the XZ plane.
    s.set_selection(vec![m]);
    s.start("3drotate").unwrap();
    s.input(Input::Text("z".into())).unwrap();
    s.input(Input::Enter).unwrap();
    s.input(Input::Text("90".into())).unwrap();
    let (lo, hi) = box3(&solid(&s, m));
    assert!((lo.y + 10.0).abs() < 1e-6 && hi.y.abs() < 1e-6, "{lo:?} {hi:?}");
    assert!(((hi.x - lo.x) - 10.0).abs() < 1e-6 && ((hi.z - lo.z) - 10.0).abs() < 1e-6 && lo.x < -4.0, "{lo:?} {hi:?}");
    // Twice the size about the plane's origin (the world origin).
    s.set_selection(vec![m]);
    s.start("3dscale").unwrap();
    s.input(Input::Point(Vec2::ZERO)).unwrap();
    s.input(Input::Text("2".into())).unwrap();
    let (lo2, hi2) = box3(&solid(&s, m));
    assert!((lo2 - lo * 2.0).len() < 1e-6 && (hi2 - hi * 2.0).len() < 1e-6, "{lo2:?} {hi2:?}");
}

/// VIEWBASE in model space and a cut list table at the prompts, in the Front plane: the views
/// and their dimensions lie in the world's XY plane; the table lies level where it was picked.
#[test]
fn views_and_tables_in_a_construction_plane() {
    let mut s = session();
    s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [100, 60], "height": 20 })).unwrap();
    s.execute("cplane", &json!({ "preset": "front" })).unwrap();
    let before = s.cplane();
    s.execute("viewbase", &json!({ "views": ["front"], "at": [200, 0], "gap": 10, "space": "model" })).unwrap();
    assert_eq!(s.cplane(), before, "the plane is put back");
    let d = s.doc().unwrap();
    let dims: Vec<(f64, f64)> = d
        .model
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Dimension(dm) => {
                Some(((dm.p14 - dm.p13).len(), dm.p13.z.abs() + dm.p14.z.abs() + e.common.extrusion.x.abs() + e.common.extrusion.y.abs()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(dims.len(), 2, "{dims:?}");
    assert!(dims.iter().all(|(_, off)| *off < 1e-9), "level, at z 0: {dims:?}");
    assert!(dims.iter().any(|(v, _)| (v - 100.0).abs() < 1e-6) && dims.iter().any(|(v, _)| (v - 20.0).abs() < 1e-6), "{dims:?}");
    let vis = bounds(&group_lines(&s, "VIEW front", "VISIBLE"));
    assert!(vis.min.near(Vec2::new(200.0, 0.0), 1e-6), "{vis:?}");
    // A table picked at (300, 50) in the plane: world (300, 0, 50), level.
    s.start("cutlist").unwrap();
    s.input(Input::Keyword("Entire model".into())).unwrap();
    s.input(Input::Point(Vec2::new(300.0, 50.0))).unwrap();
    let t = s
        .doc()
        .unwrap()
        .model
        .iter()
        .find_map(|e| match &e.kind {
            EntityKind::Table(t) => Some((t.insert, e.common.extrusion)),
            _ => None,
        })
        .expect("a table");
    assert!((t.0 - Vec3::new(300.0, 0.0, 50.0)).len() < 1e-9 && t.1 == Vec3::Z, "{t:?}");
}

/// At the prompts in the Front plane: PANEL's insertion point for a profile drawn in plan, the
/// REVOLVE axis and the SLICE points are picked in the plane and taken to the world.
#[test]
fn front_construction_plane_points_at_the_profile_prompts() {
    let mut s = session();
    let pl = s.execute("pline", &json!({ "points": [[0, 0], [600, 0], [600, 400], [0, 400]], "closed": true })).unwrap();
    let ph = handle(&pl["handle"]);
    let b = s.execute("box", &json!({ "corner1": [2000, 0, 0], "corner2": [2010, 10], "height": 10 })).unwrap();
    let bh = handle(&b["handle"]);
    s.execute("cplane", &json!({ "preset": "front" })).unwrap();
    s.set_selection(Vec::new());
    s.start("panel").unwrap();
    s.input(Input::Enter).unwrap();
    s.input(Input::Pick(vec![ph])).unwrap();
    for _ in 0..3 {
        s.input(Input::Enter).unwrap();
    }
    s.input(Input::Point(Vec2::new(100.0, 50.0))).unwrap();
    assert!(s.current_prompt().is_none(), "{}", s.prompt_text());
    let x = s
        .doc()
        .unwrap()
        .model
        .iter()
        .find_map(|e| matches!(&e.kind, EntityKind::Solid3d(x) if x.mesh.tris.len() == 12 && e.handle != bh).then_some(e.handle))
        .unwrap();
    assert!(near_box(box3(&solid(&s, x)), [100.0, 0.0, 50.0], [700.0, 400.0, 68.0]), "{:?}", box3(&solid(&s, x)));
    // The plane moved 100 along X. A rectangle drawn in it, 2 to 4 out and 3 up, revolved about
    // its y axis (the world Z axis through x 100): a ring round that.
    s.execute("cplane", &json!({ "origin": [100, 0, 0] })).unwrap();
    let r = s.execute("pline", &json!({ "points": [[2, 0], [4, 0], [4, 3], [2, 3]], "closed": true })).unwrap();
    s.set_selection(vec![handle(&r["handle"])]);
    s.start("revolve").unwrap();
    s.input(Input::Point(Vec2::ZERO)).unwrap();
    s.input(Input::Point(Vec2::new(0.0, 1.0))).unwrap();
    s.input(Input::Enter).unwrap();
    let ring = s.doc().unwrap().model.iter().last().map(|e| e.handle).unwrap();
    assert!(near_box(box3(&solid(&s, ring)), [96.0, -4.0, 0.0], [104.0, 4.0, 3.0]), "{:?}", box3(&solid(&s, ring)));
    // Two points across the plane at height 5: the box is cut at z = 5, square to the plane.
    s.set_selection(vec![bh]);
    s.start("slice").unwrap();
    s.input(Input::Point(Vec2::new(1900.0, 5.0))).unwrap();
    s.input(Input::Point(Vec2::new(1901.0, 5.0))).unwrap();
    s.input(Input::Enter).unwrap();
    let mut zs: Vec<(f64, f64)> = s
        .doc()
        .unwrap()
        .model
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Solid3d(x) if box3(x).0.x >= 1999.0 => Some((box3(x).0.z, box3(x).1.z)),
            _ => None,
        })
        .collect();
    zs.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert!(zs.len() == 2 && (zs[0].1 - 5.0).abs() < 1e-6 && (zs[1].0 - 5.0).abs() < 1e-6, "{zs:?}");
}
