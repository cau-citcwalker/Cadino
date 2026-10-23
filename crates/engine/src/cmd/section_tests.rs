//! SECTION, SECTIONPLANE, LIVESECTION and SECTIONPLANETOBLOCK: parameters, prompts and the
//! section object's round trip without the kernel; real cuts with it.

use cadcraft_doc::LwPolyline;

use super::*;

fn err(r: Result<Value>) -> String {
    r.err().map(|e| e.to_string()).unwrap_or_default()
}

fn msg(s: &Session) -> String {
    s.current_prompt().map(|p| p.message).unwrap_or_default()
}

fn poly(s: &Session, h: &Value) -> (Entity, LwPolyline) {
    let h = Handle::parse_hex(h.as_str().unwrap()).unwrap();
    let e = (**s.doc().unwrap().entity(h).unwrap()).clone();
    let EntityKind::LwPolyline(p) = e.kind.clone() else { panic!("not a polyline: {:?}", e.kind) };
    (e, p)
}

#[test]
fn hostile_parameters_are_refused() {
    let mut s = Session::new();
    let c = s.add_entity(EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::ZERO, radius: 5.0 })).unwrap();
    let pt = s.add_entity(EntityKind::Point(cadcraft_doc::Point { p: Vec3::ZERO, angle: 0.0 })).unwrap();
    for (id, p) in [
        ("section", json!({ "handles": [c.hex()] })),
        ("section", json!({ "handles": [c.hex()], "normal": [0, 0, 0] })),
        ("section", json!({ "handles": [c.hex()], "normal": [0, 0, "z"] })),
        ("section", json!({ "handles": [c.hex()], "normal": [1, 0, f64::MAX], "origin": "q" })),
        ("section", json!({ "handles": [c.hex()], "points": [[0, 0], [1, 1], [2, 2]] })),
        ("section", json!({ "handles": [c.hex()], "points": [[0, 0], [1, 1]] })),
        ("section", json!({ "handles": [c.hex()], "plane": "uv" })),
        ("section", json!({ "handles": [c.hex()], "object": pt.hex() })),
        ("section", json!({ "handles": [c.hex()], "object": "ZZ" })),
        // A circle is no solid.
        ("section", json!({ "handles": [c.hex()], "plane": "xy" })),
        ("sectionplane", json!({})),
        ("sectionplane", json!({ "plane": "xy", "name": 3 })),
        ("sectionplane", json!({ "plane": "xy", "name": "x".repeat(300) })),
        ("sectionplane", json!({ "orthographic": "sideways" })),
        // No solids to centre an orthographic plane on.
        ("sectionplane", json!({ "orthographic": "front" })),
        ("livesection", json!({})),
        ("livesection", json!({ "handle": c.hex() })),
        ("sectionplanetoblock", json!({})),
        ("sectionplanetoblock", json!({ "section": c.hex() })),
    ] {
        assert!(s.execute(id, &p).is_err(), "{id} {p}");
    }
    assert_eq!(s.doc().unwrap().model.iter().count(), 2, "the drawing is as it was");
    assert!(err(s.execute("section", &json!({ "handles": [c.hex()], "object": c.hex() }))).contains("select 3D solids"));
}

#[test]
fn planes_from_every_form() {
    let mut s = Session::new();
    let p = |s: &Session, v: Value| plane_param(s, "section", &v).unwrap();
    assert_eq!(p(&s, json!({ "plane": "yz", "point": [1, 2, 3] })), (Vec3::new(1.0, 2.0, 3.0), Vec3::X));
    assert_eq!(p(&s, json!({ "origin": [0, 0, 5], "normal": [0, 0, 10] })), (Vec3::new(0.0, 0.0, 5.0), Vec3::Z));
    let (o, n) = p(&s, json!({ "points": [[0, 0, 0], [1, 0, 0], [0, 0, 1]] }));
    assert_eq!((o, n), (Vec3::ZERO, Vec3::new(0.0, -1.0, 0.0)));
    // A circle standing in the XZ plane at y = 4 (extrusion −Y, so its elevation is −4).
    let c = s.add_entity(EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::new(0.0, 0.0, -4.0), radius: 1.0 })).unwrap();
    s.doc_mut().unwrap().modify_entity(c, |e| e.common.extrusion = Vec3::new(0.0, -1.0, 0.0)).unwrap();
    let (o, n) = p(&s, json!({ "object": c.hex() }));
    assert!((n - Vec3::new(0.0, -1.0, 0.0)).len() < 1e-12 && (o.y - 4.0).abs() < 1e-12, "{o:?} {n:?}");
    // The view's plane: square to the current view.
    s.execute("vpoint", &json!({ "preset": "front" })).unwrap();
    assert!((p(&s, json!({ "plane": "view" })).1 - Vec3::new(0.0, -1.0, 0.0)).len() < 1e-12);
}

/// Lines and arcs come as bulges, each curve turned to follow the one before, whatever way the
/// kernel gave it.
#[test]
fn chains_follow_on_and_keep_their_arcs() {
    let f = Frame::ocs(Vec3::Z);
    let p = |x: f64, y: f64| Vec3::new(x, y, 0.0);
    // A 10 × 10 square whose top is a half circle bulging up, two curves given backwards.
    let w = Wire3 {
        curves: vec![
            EdgeCurve::Line { a: p(0.0, 0.0), b: p(10.0, 0.0) },
            EdgeCurve::Line { a: p(10.0, 10.0), b: p(10.0, 0.0) },
            EdgeCurve::Arc { a: p(0.0, 10.0), mid: p(5.0, 15.0), b: p(10.0, 10.0) },
            EdgeCurve::Line { a: p(0.0, 10.0), b: p(0.0, 0.0) },
        ],
        closed: true,
    };
    let (vs, closed) = chain(&w, &f).unwrap();
    assert!(closed);
    let pts: Vec<(f64, f64)> = vs.iter().map(|v| (v.p.x, v.p.y)).collect();
    assert_eq!(pts, vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
    // From (10,10) to (0,10) over the top: counterclockwise, a half circle.
    assert!((vs[2].bulge - 1.0).abs() < 1e-9, "{:?}", vs[2]);
    assert!(vs.iter().enumerate().all(|(i, v)| i == 2 || v.bulge == 0.0));
    let area = cadcraft_geom::Polyline { vertices: vs, closed: true }.area().abs();
    assert!((area - (100.0 + std::f64::consts::PI * 12.5)).abs() < 1e-6, "{area}");
    // A circle alone: two half circles.
    let (vs, closed) =
        chain(&Wire3 { curves: vec![EdgeCurve::Circle { center: p(1.0, 1.0), radius: 2.0, normal: Vec3::Z }], closed: true }, &f).unwrap();
    assert!(closed && vs.len() == 2 && vs.iter().all(|v| v.bulge == 1.0));
    // An open chain keeps its last point.
    let (vs, closed) = chain(&Wire3 { curves: vec![EdgeCurve::Line { a: p(0.0, 0.0), b: p(1.0, 0.0) }], closed: false }, &f).unwrap();
    assert!(!closed && vs.len() == 2);
}

/// The section's hatch spaces its lines about an 80th of the section apart, whatever the units.
#[test]
fn hatch_scales_follow_the_section_size() {
    let mut d = cadcraft_doc::Drawing::new_imperial();
    d.header.set("MEASUREMENT", cadcraft_doc::HVal::Int(0));
    // ANSI31 is 1/8 apart in an inch drawing: 700 / 80 / 0.125 = 70, rounded down to 50.
    assert_eq!(hatch_scale(&d, "ANSI31", 700.0), 50.0);
    d.header.set("MEASUREMENT", cadcraft_doc::HVal::Int(1));
    // 3.175 apart in a metric one: 2.76, so 2.
    assert_eq!(hatch_scale(&d, "ANSI31", 700.0), 2.0);
    assert_eq!(hatch_scale(&d, "ANSI31", f64::NAN), 1.0);
    assert_eq!(hatch_scale(&d, "NO SUCH PATTERN", 80.0), 1.0);
}

#[test]
fn flat_ellipses_seen_from_below_run_the_other_way() {
    let e = EdgeCurve::Ellipse { center: Vec3::ZERO, major: Vec3::X * 2.0, ratio: 0.5, normal: -Vec3::Z, start: 0.0, end: 1.0 };
    let Some(EntityKind::Ellipse(el)) = flat_kind(&e) else { panic!("not an ellipse") };
    assert_eq!((el.start, el.end), (-1.0, 0.0));
    let bad = EdgeCurve::Spline { degree: 3, poles: vec![Vec3::ZERO; 2], weights: vec![], knots: vec![0.0; 3], points: vec![Vec3::ZERO, Vec3::X] };
    assert!(matches!(flat_kind(&bad), Some(EntityKind::LwPolyline(_))), "a spline that doesn't add up is drawn through its points");
    assert!(flat_kind(&EdgeCurve::Line { a: Vec3::ZERO, b: Vec3::ZERO }).is_none());
}

/// A section object is a rectangle in its plane with xdata: it saves, opens (DXF and DWG) and
/// moves with its plane; LIVESECTION switches its flag; names don't repeat.
#[test]
fn section_planes_save_open_move_and_switch() {
    let mut s = Session::new();
    let r = s.execute("sectionplane", &json!({ "origin": [0, 5, 0], "normal": [0, -1, 0] })).unwrap();
    assert_eq!((r["name"].as_str(), r["live"].as_bool()), (Some("Section Plane(1)"), Some(false)));
    let (e, pl) = poly(&s, &r["handle"]);
    assert!(pl.closed && pl.vertices.len() == 4);
    let x = section_obj(&e).unwrap();
    assert!((x.normal - Vec3::new(0.0, -1.0, 0.0)).len() < 1e-12 && (x.origin.y - 5.0).abs() < 1e-9, "{x:?}");
    assert!(err(s.execute("sectionplane", &json!({ "plane": "xy", "name": "section plane(1)" }))).contains("already"));
    let r2 = s.execute("sectionplane", &json!({ "plane": "xy", "point": [0, 0, 7], "name": "Shelf cut", "live": true })).unwrap();
    // Several: the one meant must be named.
    assert!(err(s.execute("livesection", &json!({}))).contains("several"));
    // Another application's xdata on it (a hyperlink) stays when LIVESECTION switches the flag.
    let h1 = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
    let link = [RawTag { code: 1001, value: "PE_URL".into() }, RawTag { code: 1000, value: "https://example.com".into() }];
    s.doc_mut()
        .unwrap()
        .modify_entity(h1, |e| {
            let ours = std::mem::take(&mut e.common.xdata);
            e.common.xdata = link.iter().cloned().chain(ours).collect();
        })
        .unwrap();
    let on = s.execute("livesection", &json!({ "handle": r["handle"] })).unwrap();
    assert_eq!(on["live"], true);
    assert!(on["message"].as_str().unwrap().contains("Section Plane(1)"));
    let (e, _) = poly(&s, &r["handle"]);
    assert_eq!(e.common.xdata.get(..2), Some(&link[..]), "{:?}", e.common.xdata);
    assert!(section_obj(&e).unwrap().live);
    s.execute("livesection", &json!({ "handle": r2["handle"], "on": false })).unwrap();
    for name in ["x.dxf", "x.dwg"] {
        let bytes = cadcraft_io::write(s.doc().unwrap(), name).unwrap();
        let back = cadcraft_io::read(&bytes, name).unwrap();
        let mut objs: Vec<SectionObj> = back.model.iter().filter_map(|e| section_obj(e)).collect();
        objs.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(
            objs.iter().map(|o| (o.name.as_str(), o.live)).collect::<Vec<_>>(),
            vec![("Section Plane(1)", true), ("Shelf cut", false)],
            "{name}"
        );
        assert!((objs[1].origin.z - 7.0).abs() < 1e-9 && (objs[1].normal - Vec3::Z).len() < 1e-9, "{name}: {:?}", objs[1]);
    }
    // MOVE takes the plane along.
    let h = Handle::parse_hex(r2["handle"].as_str().unwrap()).unwrap();
    s.execute("3dmove", &json!({ "handles": [h.hex()], "delta": [0, 0, 3] })).unwrap();
    let moved = section_obj(s.doc().unwrap().entity(h).unwrap()).unwrap();
    assert!((moved.origin.z - 10.0).abs() < 1e-9, "{moved:?}");
    // Undo takes a section plane away.
    s.undo().unwrap();
    s.undo().unwrap();
    s.undo().unwrap();
    s.undo().unwrap();
    assert_eq!(section_objs(&s).unwrap().len(), 1);
}

#[test]
fn prompts_follow_autocad() {
    let mut s = Session::new();
    let c = s.add_entity(EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::ZERO, radius: 5.0 })).unwrap();
    s.start("section").unwrap();
    assert!(msg(&s).contains("Select objects"));
    s.input(Input::Pick(vec![c])).unwrap();
    s.input(Input::Enter).unwrap();
    let p = s.current_prompt().unwrap();
    assert_eq!(p.display(), "Specify first point on Section plane or [Object/Zaxis/View/XY/YZ/ZX/3points] <3points>:");
    assert_eq!(p.keywords, vec!["Object", "Zaxis", "View", "XY", "YZ", "ZX", "3points"]);
    s.input(Input::Keyword("XY".into())).unwrap();
    assert!(msg(&s).contains("XY-plane"));
    // Enter: the XY plane through 0,0,0, which needs a solid to cut.
    s.input(Input::Enter).unwrap();
    assert!(s.log.iter().any(|l| l.contains("select 3D solids")), "{:?}", s.log);
    s.cancel();
    s.set_selection(Vec::new());
    s.start("section").unwrap();
    s.input(Input::Pick(vec![c])).unwrap();
    s.input(Input::Enter).unwrap();
    s.input(Input::Point(Vec2::ZERO)).unwrap();
    assert!(msg(&s).contains("second point on plane"));
    s.input(Input::Point(Vec2::new(1.0, 0.0))).unwrap();
    assert!(msg(&s).contains("third point on plane"));
    let _ = s.input(Input::Point(Vec2::new(2.0, 0.0)));
    assert!(msg(&s).contains("third point on plane"), "a third point on the line is asked again");
    s.cancel();
    // SECTIONPLANE: a point and a through point, or Orthographic.
    s.set_selection(Vec::new());
    s.start("sectionplane").unwrap();
    assert_eq!(s.current_prompt().unwrap().keywords, vec!["Orthographic"]);
    s.input(Input::Point(Vec2::ZERO)).unwrap();
    assert!(msg(&s).contains("through point"));
    s.input(Input::Point(Vec2::new(10.0, 0.0))).unwrap();
    let x = section_objs(&s).unwrap();
    assert_eq!(x.len(), 1);
    // Drawn left to right, the viewer is in front (on the right of the line seen from above).
    assert!((x[0].normal - Vec3::new(0.0, -1.0, 0.0)).len() < 1e-12, "{:?}", x[0]);
    s.start("sectionplane").unwrap();
    s.input(Input::Text("o".into())).unwrap();
    assert_eq!(s.current_prompt().unwrap().keywords, vec!["Front", "bAck", "Top", "Bottom", "Left", "Right"]);
    s.cancel();
    // LIVESECTION: Enter takes the only one.
    s.start("livesection").unwrap();
    assert!(msg(&s).contains("Select section object"));
    s.input(Input::Enter).unwrap();
    assert!(section_objs(&s).unwrap()[0].live);
    assert!(s.log.iter().any(|l| l.contains("Live section on")), "{:?}", s.log);
}

#[cfg(feature = "occt")]
mod kernel {
    use super::*;

    fn handle(v: &Value) -> Handle {
        Handle::parse_hex(v.as_str().unwrap()).unwrap()
    }

    fn polys(s: &Session, r: &Value) -> Vec<(Entity, LwPolyline)> {
        r["handles"].as_array().unwrap().iter().map(|h| poly(s, h)).collect()
    }

    fn area(p: &LwPolyline) -> f64 {
        cadcraft_geom::Polyline { vertices: p.vertices.clone(), closed: p.closed }.area().abs()
    }

    /// A 100 × 50 × 30 box with a hole of radius 10 straight down through it at 50,25.
    fn holed_box(s: &mut Session) -> Handle {
        let b = handle(&s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [100, 50, 30] })).unwrap()["handle"]);
        let c = handle(&s.execute("cylinder", &json!({ "center": [50, 25, -1], "radius": 10, "height": 32 })).unwrap()["handle"]);
        s.execute("subtract", &json!({ "from": [b.hex()], "remove": [c.hex()] })).unwrap();
        b
    }

    #[test]
    fn section_cuts_outlines_in_the_plane() {
        let mut s = Session::new();
        let b = holed_box(&mut s);
        // Level at z = 10: the outline and the hole.
        let r = s.execute("section", &json!({ "handles": [b.hex()], "plane": "xy", "point": [0, 0, 10] })).unwrap();
        let ps = polys(&s, &r);
        assert_eq!(ps.len(), 2, "{r}");
        let mut areas: Vec<f64> = ps.iter().map(|(_, p)| area(p)).collect();
        areas.sort_by(f64::total_cmp);
        assert!((areas[0] - std::f64::consts::PI * 100.0).abs() < 1e-6 && (areas[1] - 5000.0).abs() < 1e-6, "{areas:?}");
        assert!(ps.iter().all(|(e, p)| p.closed && (p.elevation - 10.0).abs() < 1e-9 && e.common.extrusion == Vec3::Z));
        s.undo().unwrap();
        // Standing at y = 25 through the hole: two rectangles 40 × 30, in the plane y = 25.
        let r = s.execute("section", &json!({ "handles": [b.hex()], "points": [[0, 25, 0], [1, 25, 0], [0, 25, 1]] })).unwrap();
        let ps = polys(&s, &r);
        assert_eq!(ps.len(), 2, "{r}");
        for (e, p) in &ps {
            assert!((area(p) - 1200.0).abs() < 1e-6, "{}", area(p));
            let m = e.plane().unwrap();
            assert!(p.vertices.iter().all(|v| (m.apply(v.p.to3(0.0)).y - 25.0).abs() < 1e-9), "in the plane");
        }
        // A plane that misses.
        assert!(err(s.execute("section", &json!({ "handles": [b.hex()], "plane": "xy", "point": [0, 0, 40] }))).contains("does not cut"));
    }

    #[test]
    fn section_planes_draw_a_hatched_2d_section_with_what_lies_beyond() {
        let mut s = Session::new();
        holed_box(&mut s);
        // A shelf behind the cut, seen beyond it (its front edge at y = 60).
        s.execute("box", &json!({ "corner1": [0, 60, 0], "corner2": [100, 80, 5] })).unwrap();
        let r = s.execute("sectionplane", &json!({ "orthographic": "front", "name": "Front" })).unwrap();
        let x = section_objs(&s).unwrap().remove(0);
        assert!((x.origin.y - 40.0).abs() < 1e-6 && (x.normal - Vec3::new(0.0, -1.0, 0.0)).len() < 1e-12, "{x:?}");
        // The box is cut at y = 40, behind its hole: one region, 100 × 30, hatched.
        let g = s.execute("sectionplanetoblock", &json!({ "section": r["handle"], "at": [500, 0] })).unwrap();
        assert_eq!(g["regions"], 1, "{g}");
        assert!(g["background"].as_u64().unwrap() >= 4, "{g}");
        let d = s.doc().unwrap();
        let blk = d.block(g["block"].as_str().unwrap()).unwrap();
        let hatches: Vec<&Hatch> = blk.entities.iter().filter_map(|e| if let EntityKind::Hatch(h) = &e.kind { Some(h) } else { None }).collect();
        assert_eq!(hatches.len(), 1);
        assert_eq!(hatches[0].pattern, "ANSI31");
        let outline = blk.entities.iter().find_map(|e| if let EntityKind::LwPolyline(p) = &e.kind { Some(p.clone()) } else { None }).unwrap();
        assert!((area(&outline) - 3000.0).abs() < 1e-6);
        // Seen from the front: x along X, y up Z; the base point is the drawing's lower-left.
        let b = Bounds2::from_points(outline.vertices.iter().map(|v| v.p));
        assert!(b.min.near(Vec2::ZERO, 1e-6) && b.max.near(Vec2::new(100.0, 30.0), 1e-6), "{b:?}");
        assert!(blk.base.xy().near(Vec2::ZERO, 1e-6), "{:?}", blk.base);
        let ins = d.entity(handle(&g["handle"])).unwrap();
        let EntityKind::Insert(i) = &ins.kind else { panic!("not a block reference") };
        assert!(i.insert.xy().near(Vec2::new(500.0, 0.0), 1e-9));
        // Again into the same block: redefined, no second reference.
        let again = s.execute("sectionplanetoblock", &json!({ "block": g["block"], "hatch": "none", "background": false })).unwrap();
        assert_eq!((again["redefined"].as_bool(), again["handle"].is_null()), (Some(true), true), "{again}");
        let blk = s.doc().unwrap().block(g["block"].as_str().unwrap()).unwrap().clone();
        assert!(blk.entities.iter().all(|e| matches!(e.kind, EntityKind::LwPolyline(_))));
    }
}
