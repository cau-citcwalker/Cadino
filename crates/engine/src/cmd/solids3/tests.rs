//! Tests of 3DARRAY, 3DALIGN, POLYSOLID, CONVTOSOLID, CONVTOSURFACE and 3DPRINT, and of the
//! LOFT, PLANESURF and SLICE options.

use cadcraft_doc::Line;
use cadcraft_geom::Vec2;

use super::*;

fn err(r: Result<Value>) -> String {
    r.err().map(|e| e.to_string()).unwrap_or_default()
}

fn v3(p: [f64; 3]) -> Vec3 {
    Vec3::new(p[0], p[1], p[2])
}

fn near3(a: Vec3, b: [f64; 3], tol: f64) -> bool {
    (a - v3(b)).len() < tol
}

fn line3(s: &mut Session, a: [f64; 3], b: [f64; 3]) -> Handle {
    s.add_entity(EntityKind::Line(Line { a: v3(a), b: v3(b) })).unwrap()
}

fn lines(s: &Session) -> Vec<(Vec3, Vec3)> {
    s.doc()
        .unwrap()
        .model
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Line(l) => Some((l.a, l.b)),
            _ => None,
        })
        .collect()
}

fn has_line(s: &Session, a: [f64; 3], b: [f64; 3]) -> bool {
    lines(s).iter().any(|(p, q)| near3(*p, a, 1e-9) && near3(*q, b, 1e-9))
}

fn msg(s: &Session) -> String {
    s.current_prompt().map(|p| p.message).unwrap_or_default()
}

fn count(s: &Session) -> usize {
    s.doc().unwrap().model.iter().count()
}

#[test]
fn a_rectangular_3d_array_copies_along_rows_columns_and_levels() {
    let mut s = Session::new();
    let l = line3(&mut s, [0.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
    let p = json!({ "handles": [l.hex()], "rows": 2, "columns": 3, "levels": 2, "rowSpacing": 10, "columnSpacing": 5, "levelSpacing": -7 });
    let r = s.execute("3darray", &p).unwrap();
    assert_eq!((r["items"].as_u64(), r["handles"].as_array().map(Vec::len)), (Some(12), Some(11)), "{r}");
    assert_eq!(lines(&s).len(), 12);
    // Columns along X, rows along Y, levels along Z (here downward).
    assert!(has_line(&s, [10.0, 10.0, -7.0], [11.0, 10.0, -7.0]));
    s.undo().unwrap();
    assert_eq!(lines(&s).len(), 1, "one undo step");
}

#[test]
fn a_polar_3d_array_turns_or_carries_copies_round_an_axis() {
    let mut s = Session::new();
    let l = line3(&mut s, [10.0, 0.0, 0.0], [12.0, 0.0, 0.0]);
    let polar = |extra: Value| {
        let mut p = json!({ "handles": [l.hex()], "type": "polar", "count": 4, "axis": [[0, 0, 0], [0, 0, 1]] });
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        p
    };
    s.execute("3darray", &polar(json!({}))).unwrap();
    assert_eq!(lines(&s).len(), 4);
    assert!(has_line(&s, [0.0, 10.0, 0.0], [0.0, 12.0, 0.0]) && has_line(&s, [0.0, -10.0, 0.0], [0.0, -12.0, 0.0]));
    s.undo().unwrap();
    // Not turned: the copies keep their direction, their middles go round.
    s.execute("3darray", &polar(json!({ "rotate": false }))).unwrap();
    assert!(has_line(&s, [-1.0, 11.0, 0.0], [1.0, 11.0, 0.0]), "{:?}", lines(&s));
    s.undo().unwrap();
    // Less than a turn: the last copy at the end of the angle; clockwise when negative; about a
    // horizontal axis the copies stand up.
    s.execute("3darray", &polar(json!({ "count": 3, "fill": -180, "axis": [[0, 0, 0], [1, 0, 0]] }))).unwrap();
    let ls = lines(&s);
    assert_eq!(ls.len(), 3);
    s.undo().unwrap();
    let up = line3(&mut s, [0.0, 10.0, 0.0], [0.0, 12.0, 0.0]);
    s.execute("3darray", &json!({ "handles": [up.hex()], "type": "p", "count": 3, "fill": 180, "axis": [[0, 0, 0], [1, 0, 0]] })).unwrap();
    assert!(has_line(&s, [0.0, 0.0, 10.0], [0.0, 0.0, 12.0]) && has_line(&s, [0.0, -10.0, 0.0], [0.0, -12.0, 0.0]), "{:?}", lines(&s));
}

#[test]
fn three_d_align_moves_turns_and_copies() {
    let mut s = Session::new();
    let l = line3(&mut s, [0.0, 0.0, 0.0], [2.0, 0.0, 0.0]);
    // X onto Z, the XY plane onto the XZ plane, at (5, 5, 0).
    let p = json!({ "handles": [l.hex()], "source": [[0, 0, 0], [1, 0, 0], [0, 1, 0]], "destination": [[5, 5, 0], [5, 5, 1], [6, 5, 0]] });
    s.execute("3dalign", &p).unwrap();
    assert!(has_line(&s, [5.0, 5.0, 0.0], [5.0, 5.0, 2.0]), "{:?}", lines(&s));
    s.undo().unwrap();
    // Two pairs: the base point and a direction (here turned right round); Copy keeps the source.
    s.execute("3dalign", &json!({ "handles": [l.hex()], "source": [[0, 0, 0], [1, 0, 0]], "destination": [[0, 0, 3], [-1, 0, 3]], "copy": true }))
        .unwrap();
    assert!(has_line(&s, [0.0, 0.0, 0.0], [2.0, 0.0, 0.0]) && has_line(&s, [0.0, 0.0, 3.0], [-2.0, 0.0, 3.0]), "{:?}", lines(&s));
    // One pair: a move.
    s.execute("3dalign", &json!({ "handles": [l.hex()], "source": [[0, 0, 0], [1, 0, 0]], "destination": [[1, 1, 1]] })).unwrap();
    assert!(has_line(&s, [1.0, 1.0, 1.0], [3.0, 1.0, 1.0]));
    // The motion never scales.
    let m = align_matrix(&[Vec3::ZERO, Vec3::X * 2.0, Vec3::Y], &[Vec3::Z, Vec3::Z + Vec3::Y * 9.0, Vec3::Z - Vec3::X]).unwrap();
    assert!(near3(m.apply(Vec3::X * 2.0), [0.0, 2.0, 1.0], 1e-12) && near3(m.apply(Vec3::Y), [-1.0, 0.0, 1.0], 1e-12));
}

#[test]
fn hostile_parameters_are_refused() {
    let mut s = Session::new();
    let l = line3(&mut s, [0.0, 0.0, 0.0], [5.0, 0.0, 0.0]).hex();
    let x = s.add_entity(EntityKind::Solid3d(Solid3d::default())).unwrap().hex();
    for (id, p, says) in [
        ("3darray", json!({ "handles": [l], "rows": 0 }), "rows"),
        ("3darray", json!({ "handles": [l], "rows": 1.5 }), "rows"),
        ("3darray", json!({ "handles": [l], "rows": 1e300 }), "rows"),
        ("3darray", json!({ "handles": [l], "columns": "x" }), "columns"),
        ("3darray", json!({ "handles": [l], "rows": 2 }), "rowSpacing"),
        ("3darray", json!({ "handles": [l], "rows": 2, "rowSpacing": 0 }), "rowSpacing"),
        ("3darray", json!({ "handles": [l] }), "more than one"),
        ("3darray", json!({ "handles": [l], "rows": 1000, "columns": 1000, "rowSpacing": 1, "columnSpacing": 1 }), "MAXARRAY"),
        ("3darray", json!({ "handles": [l], "type": 3 }), "type"),
        ("3darray", json!({ "handles": [l], "type": "hex" }), "type"),
        ("3darray", json!({ "handles": [l], "type": "polar", "axis": [[0, 0], [0, 1]] }), "count"),
        ("3darray", json!({ "handles": [l], "type": "polar", "count": 1, "axis": [[0, 0], [0, 1]] }), "count"),
        ("3darray", json!({ "handles": [l], "type": "polar", "count": 3, "fill": 0, "axis": [[0, 0], [0, 1]] }), "fill"),
        ("3darray", json!({ "handles": [l], "type": "polar", "count": 3, "fill": 361, "axis": [[0, 0], [0, 1]] }), "fill"),
        ("3darray", json!({ "handles": [l], "type": "polar", "count": 3, "rotate": "y", "axis": [[0, 0], [0, 1]] }), "rotate"),
        ("3darray", json!({ "handles": [l], "type": "polar", "count": 3, "axis": [[0, 0], [0, 0]] }), "axis"),
        ("3dalign", json!({ "handles": [l], "source": [], "destination": [[0, 0]] }), "source"),
        ("3dalign", json!({ "handles": [l], "source": [[0, 0]], "destination": [[0, 0], [1, 0], [2, 1], [3, 3]] }), "destination"),
        ("3dalign", json!({ "handles": [l], "source": [[0, 0], [0, 0]], "destination": [[1, 1], [2, 2]] }), "different"),
        ("3dalign", json!({ "handles": [l], "source": [[0, 0], [1, 0], [2, 0]], "destination": [[0, 0], [0, 1], [1, 1]] }), "one line"),
        ("polysolid", json!({ "points": [[0, 0]] }), "two different points"),
        ("polysolid", json!({ "points": [[0, 0], [0, 0]] }), "two different points"),
        ("polysolid", json!({ "points": "x" }), "points"),
        ("polysolid", json!({ "points": [[0, 0], [1, 0]], "width": -1 }), "width"),
        ("polysolid", json!({ "points": [[0, 0], [1, 0]], "height": "tall" }), "height"),
        ("polysolid", json!({ "points": [[0, 0], [1, 0]], "justify": "up" }), "justify"),
        ("polysolid", json!({ "points": [[0, 0], [1, 0]], "bulges": ["a"] }), "bulges"),
        ("polysolid", json!({ "points": [[0, 0], [1, 0]], "bulges": [0, 0, 0] }), "bulges"),
        ("polysolid", json!({ "handle": "zz" }), "handle"),
        ("polysolid", json!({}), "points"),
        ("polysolid", json!({ "handle": x }), "can't be converted"),
        ("convtosolid", json!({ "handles": [l] }), "select closed polylines"),
        ("convtosurface", json!({ "handles": [l] }), "select 3D solids"),
        ("3dprint", json!({ "handles": [l] }), "path"),
        ("3dprint", json!({ "path": "out.stl", "handles": [l] }), "Select 3D solids"),
        ("loft", json!({ "handles": [l], "mode": 3 }), "mode"),
        ("loft", json!({ "handles": [l], "guides": "x" }), "guides"),
        ("loft", json!({ "handles": [l], "guides": [7] }), "guides"),
        ("loft", json!({ "handles": [l], "path": [1] }), "path"),
        ("loft", json!({ "handles": [l], "guides": [l], "path": l }), "not both"),
        ("slice", json!({ "handles": [x], "points": [[0, 0]] }), "points"),
        ("slice", json!({ "handles": [x], "points": [[0, 0], [1, 0], [2, 0]] }), "one line"),
        ("slice", json!({ "handles": [x], "plane": "ab" }), "plane"),
        ("slice", json!({ "handles": [x], "origin": [0, 0] }), "normal"),
        ("slice", json!({ "handles": [x], "origin": [0, 0], "normal": [0, 0, 0] }), "normal"),
        ("slice", json!({ "handles": [x], "zaxis": [[0, 0], [0, 0]] }), "normal"),
        ("slice", json!({ "handles": [x], "view": 1 }), "view"),
        ("slice", json!({ "handles": [x], "object": l }), "has no plane"),
        ("slice", json!({ "handles": [x], "surface": "zz" }), "surface"),
        ("slice", json!({ "handles": [x], "surface": l }), "surface"),
    ] {
        let e = err(s.execute(id, &p));
        assert!(e.contains(says), "{id} {p}: {e}");
    }
    assert_eq!(count(&s), 2, "the drawing is as it was");
}

#[test]
fn locked_objects_are_left_alone() {
    let mut s = Session::new();
    let l = line3(&mut s, [0.0, 0.0, 0.0], [5.0, 0.0, 0.0]).hex();
    s.doc_mut().unwrap().layer_mut("0").unwrap().locked = true;
    assert!(err(s.execute("3darray", &json!({ "handles": [l], "rows": 2, "rowSpacing": 3 }))).contains("locked"));
    assert!(err(s.execute("3dalign", &json!({ "handles": [l], "source": [[0, 0]], "destination": [[1, 1]] }))).contains("locked"));
    assert!(err(s.execute("polysolid", &json!({ "handle": l }))).contains("locked"));
    assert!(err(s.execute("convtosurface", &json!({ "handles": [l] }))).contains("locked"));
    assert_eq!(lines(&s), vec![(Vec3::ZERO, Vec3::X * 5.0)]);
}

#[test]
fn polysolid_paths_bulges_and_defaults() {
    let p = json!({ "points": [[0, 0, 2], [10, 0], [10, 0], [10, 10]], "bulges": [0, 0, 1] });
    let segs = drawn_path("polysolid", &p).unwrap();
    // The repeated point is dropped; all at the first point's height; a bulge of 1 is a half
    // circle to the right of the chord (counterclockwise).
    assert_eq!(
        segs,
        vec![
            Seg::Line { a: Vec3::new(0.0, 0.0, 2.0), b: Vec3::new(10.0, 0.0, 2.0) },
            Seg::Arc { a: Vec3::new(10.0, 0.0, 2.0), mid: Vec3::new(15.0, 5.0, 2.0), b: Vec3::new(10.0, 10.0, 2.0) }
        ]
    );
    let closed = drawn_path("polysolid", &json!({ "points": [[0, 0], [10, 0], [10, 10], [0, 0]], "closed": true })).unwrap();
    assert_eq!(closed.len(), 3, "the last point on the first closes the loop");
    assert_eq!(justify_param("polysolid", &json!({ "justify": "L" })).unwrap(), Justify::Left);
    assert_eq!(justify_param("polysolid", &json!({})).unwrap(), Justify::Center);
    // AutoCAD's initial PSOLHEIGHT and PSOLWIDTH, then the drawing's.
    let mut s = Session::new();
    let metric = s.doc().unwrap().header.i64("MEASUREMENT", 0) == 1;
    assert_eq!((psol(&s, "PSOLHEIGHT"), psol(&s, "PSOLWIDTH")), if metric { (80.0, 5.0) } else { (4.0, 0.25) });
    s.doc_mut().unwrap().header.set_f64("PSOLWIDTH", 18.0);
    s.doc_mut().unwrap().header.set_f64("PSOLHEIGHT", f64::NAN);
    assert_eq!((psol(&s, "PSOLHEIGHT"), psol(&s, "PSOLWIDTH")), (if metric { 80.0 } else { 4.0 }, 18.0));
}

#[test]
fn three_point_arcs_have_the_bulge_of_their_sweep() {
    let p = |x: f64, y: f64| Vec2::new(x, y);
    assert!((three_point_bulge(p(1.0, 0.0), p(0.0, 1.0), p(-1.0, 0.0)) - 1.0).abs() < 1e-12);
    assert!((three_point_bulge(p(1.0, 0.0), p(0.0, -1.0), p(-1.0, 0.0)) + 1.0).abs() < 1e-12);
    let h = std::f64::consts::FRAC_1_SQRT_2;
    assert!((three_point_bulge(p(1.0, 0.0), p(h, h), p(0.0, 1.0)) - (PI / 8.0).tan()).abs() < 1e-12);
    // Three quarters of a turn, clockwise.
    assert!((three_point_bulge(p(1.0, 0.0), p(-1.0, 0.0), p(0.0, 1.0)) + (3.0 * PI / 8.0).tan()).abs() < 1e-12);
    assert_eq!(three_point_bulge(p(0.0, 0.0), p(1.0, 1.0), p(2.0, 2.0)), 0.0);
    assert_eq!(three_point_bulge(p(0.0, 0.0), p(f64::NAN, 1.0), p(2.0, 2.0)), 0.0);
}

fn square(c: [f64; 2], half: f64, z: f64) -> Loop {
    let pts: Vec<Vec3> =
        [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].iter().map(|(x, y)| Vec3::new(c[0] + x * half, c[1] + y * half, z)).collect();
    let segs = (0..4).map(|i| Seg::Line { a: pts[i], b: pts[(i + 1) % 4] }).collect();
    Loop { owner: Handle(1), pts, curve: Curve::Segs(segs) }
}

#[test]
fn loops_nest_into_outlines_holes_and_islands() {
    let ring = Curve::Segs(vec![Seg::Circle { center: Vec3::new(5.0, 5.0, 0.0), radius: 3.0, normal: Vec3::Z }]);
    let loops = vec![
        square([5.0, 5.0], 5.0, 0.0),
        Loop { owner: Handle(2), pts: sample(&ring), curve: ring },
        square([5.0, 5.0], 1.0, 0.0),
        // In another plane: not a hole.
        square([5.0, 5.0], 1.0, 4.0),
        square([30.0, 5.0], 1.0, 0.0),
    ];
    let mut n = nest(&loops);
    n.sort();
    assert_eq!(n, vec![(0, vec![1]), (2, vec![]), (3, vec![]), (4, vec![])]);
    // A circle's points are on it.
    assert!(loops[1].pts.iter().all(|p| ((*p - Vec3::new(5.0, 5.0, 0.0)).len() - 3.0).abs() < 1e-12));
    let arc = Curve::Segs(vec![Seg::Arc { a: Vec3::new(1.0, 0.0, 0.0), mid: Vec3::new(0.0, -1.0, 0.0), b: Vec3::new(-1.0, 0.0, 0.0) }]);
    assert!(sample(&arc).iter().all(|p| (p.len() - 1.0).abs() < 1e-12 && p.y <= 1e-12), "the arc's own side");
}

#[test]
fn prompts_follow_autocad() {
    let mut s = Session::new();
    let l = line3(&mut s, [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]);
    let keywords = |s: &Session| s.current_prompt().map(|p| p.keywords).unwrap_or_default();
    // 3DARRAY Rectangular: two rows 5 apart.
    s.set_selection(vec![l]);
    s.start("3darray").unwrap();
    assert_eq!((msg(&s), keywords(&s)), ("Enter the type of array".into(), vec!["Rectangular".to_string(), "Polar".into()]));
    s.input(Input::Enter).unwrap();
    assert_eq!(msg(&s), "Enter the number of rows (---)");
    s.input(Input::Text("2".into())).unwrap();
    assert_eq!(msg(&s), "Enter the number of columns (|||)");
    let _ = s.input(Input::Text("0".into()));
    assert_eq!(msg(&s), "Enter the number of columns (|||)", "asked again");
    s.input(Input::Enter).unwrap();
    s.input(Input::Enter).unwrap();
    assert_eq!(msg(&s), "Specify the distance between rows (---)");
    s.input(Input::Text("5".into())).unwrap();
    assert!(s.current_prompt().is_none());
    assert!(has_line(&s, [1.0, 5.0, 0.0], [2.0, 5.0, 0.0]));
    // 3DARRAY Polar: 4 items round the Z axis through the origin, picked at heights 0 and 1.
    s.set_selection(vec![l]);
    s.start("3darray").unwrap();
    s.input(Input::Text("p".into())).unwrap();
    assert_eq!(msg(&s), "Enter the number of items in the array");
    s.input(Input::Text("4".into())).unwrap();
    assert_eq!(msg(&s), "Specify the angle to fill (+=ccw, -=cw)");
    s.input(Input::Enter).unwrap();
    assert_eq!(keywords(&s), vec!["Yes", "No"]);
    s.input(Input::Enter).unwrap();
    assert_eq!(msg(&s), "Specify center point of array");
    s.input(Input::Point(Vec2::ZERO)).unwrap();
    assert_eq!(msg(&s), "Specify second point on axis of rotation");
    s.point_z = Some(1.0);
    s.input(Input::Point(Vec2::ZERO)).unwrap();
    s.point_z = None;
    assert!(has_line(&s, [0.0, 1.0, 0.0], [0.0, 2.0, 0.0]) && has_line(&s, [0.0, -1.0, 0.0], [0.0, -2.0, 0.0]));
    let before = lines(&s).len();
    // 3DALIGN: base point (Copy first), Continue after the second point; one destination pair
    // more.
    s.set_selection(vec![l]);
    s.start("3dalign").unwrap();
    assert_eq!((msg(&s), keywords(&s)), ("Specify base point".into(), vec!["Copy".to_string()]));
    s.input(Input::Text("c".into())).unwrap();
    s.input(Input::Point(Vec2::new(1.0, 0.0))).unwrap();
    assert_eq!((msg(&s), keywords(&s)), ("Specify second point".into(), vec!["Continue".to_string()]));
    s.input(Input::Point(Vec2::new(2.0, 0.0))).unwrap();
    assert_eq!(msg(&s), "Specify third point");
    s.input(Input::Enter).unwrap();
    assert_eq!(msg(&s), "Specify first destination point");
    s.input(Input::Point(Vec2::new(0.0, 7.0))).unwrap();
    assert_eq!((msg(&s), keywords(&s)), ("Specify second destination point".into(), vec!["eXit".to_string()]));
    s.input(Input::Point(Vec2::new(0.0, 9.0))).unwrap();
    assert!(s.current_prompt().is_none());
    assert_eq!(lines(&s).len(), before + 1, "a copy");
    assert!(has_line(&s, [0.0, 7.0, 0.0], [0.0, 8.0, 0.0]));
    // POLYSOLID: the current settings, then a start point or an option.
    s.set_selection(Vec::new());
    s.start("polysolid").unwrap();
    assert!(s.log.iter().any(|l| l.contains("Justification = Center")), "{:?}", s.log);
    assert_eq!(keywords(&s), vec!["Object", "Height", "Width", "Justify"]);
    s.input(Input::Text("h".into())).unwrap();
    assert_eq!(msg(&s), "Specify height");
    s.input(Input::Text("30".into())).unwrap();
    s.input(Input::Text("j".into())).unwrap();
    s.input(Input::Text("l".into())).unwrap();
    s.input(Input::Point(Vec2::ZERO)).unwrap();
    assert_eq!(keywords(&s), vec!["Arc", "Undo"]);
    s.input(Input::Point(Vec2::new(10.0, 0.0))).unwrap();
    s.input(Input::Text("a".into())).unwrap();
    assert_eq!(
        (msg(&s), keywords(&s)),
        ("Specify endpoint of arc".into(), vec!["Close".to_string(), "Line".into(), "Second point".into(), "Undo".into()])
    );
    s.input(Input::Text("s".into())).unwrap();
    assert_eq!(msg(&s), "Specify second point on arc");
    s.cancel();
    assert_eq!(s.doc().unwrap().header.f64("PSOLHEIGHT", 0.0), 30.0);
    // 3DPRINT: solids, then the file.
    s.set_selection(Vec::new());
    s.start("3dprint").unwrap();
    assert_eq!(msg(&s), "Select solids or watertight meshes");
    s.input(Input::Pick(vec![l])).unwrap();
    s.input(Input::Enter).unwrap();
    assert_eq!(msg(&s), "Enter name of STL file to write");
    s.cancel();
}

#[test]
fn loft_and_slice_prompts_follow_autocad() {
    let mut s = Session::new();
    let keywords = |s: &Session| s.current_prompt().map(|p| p.keywords).unwrap_or_default();
    let (a, b) = (line3(&mut s, [0.0, 0.0, 0.0], [1.0, 0.0, 0.0]), line3(&mut s, [0.0, 0.0, 5.0], [1.0, 0.0, 5.0]));
    s.set_selection(Vec::new());
    s.start("loft").unwrap();
    assert_eq!((msg(&s), keywords(&s)), ("Select cross sections in lofting order".into(), vec!["MOde".to_string()]));
    s.input(Input::Text("mo".into())).unwrap();
    assert_eq!(keywords(&s), vec!["SOlid", "SUrface"]);
    s.input(Input::Text("su".into())).unwrap();
    s.input(Input::Pick(vec![a, b])).unwrap();
    s.input(Input::Enter).unwrap();
    assert_eq!(keywords(&s), vec!["Guides", "Path", "Cross sections only", "Settings"]);
    s.input(Input::Text("s".into())).unwrap();
    assert_eq!(keywords(&s), vec!["Ruled", "Smooth"]);
    s.cancel();
    s.set_selection(Vec::new());
    s.start("loft").unwrap();
    s.input(Input::Pick(vec![a, b])).unwrap();
    s.input(Input::Enter).unwrap();
    s.input(Input::Text("g".into())).unwrap();
    assert_eq!(msg(&s), "Select guide curves");
    s.cancel();
    // SLICE: the objects, then the plane's options; XY through a point, then the side.
    s.set_selection(Vec::new());
    s.start("slice").unwrap();
    assert_eq!(msg(&s), "Select objects to slice");
    s.input(Input::Pick(vec![a])).unwrap();
    s.input(Input::Enter).unwrap();
    assert_eq!(msg(&s), "Specify start point of slicing plane");
    assert_eq!(keywords(&s), vec!["planar Object", "Surface", "Zaxis", "View", "XY", "YZ", "ZX", "3points"]);
    s.input(Input::Text("xy".into())).unwrap();
    assert_eq!(msg(&s), "Specify a point on the XY-plane");
    s.input(Input::Enter).unwrap();
    assert_eq!((msg(&s), keywords(&s)), ("Specify a point on desired side".into(), vec!["keep Both sides".to_string()]));
    s.cancel();
    s.set_selection(vec![a]);
    s.start("slice").unwrap();
    s.input(Input::Enter).unwrap();
    assert_eq!(msg(&s), "Specify first point on plane");
    s.input(Input::Point(Vec2::ZERO)).unwrap();
    s.input(Input::Point(Vec2::new(1.0, 0.0))).unwrap();
    assert_eq!(msg(&s), "Specify third point on plane");
    let _ = s.input(Input::Point(Vec2::new(2.0, 0.0)));
    assert_eq!(msg(&s), "Specify third point on plane");
    s.cancel();
    s.set_selection(vec![a]);
    s.start("slice").unwrap();
    s.input(Input::Text("o".into())).unwrap();
    assert!(msg(&s).starts_with("Select a circle"));
    let _ = s.input(Input::Pick(vec![b]));
    assert!(msg(&s).starts_with("Select a circle"), "a line has no plane: asked again");
    s.cancel();
}

#[cfg(feature = "occt")]
mod occt {
    use cadcraft_doc::{Circle, Face3d, HatchLoop, PolyMesh, Region};
    use cadcraft_geom::PolyVertex;

    use super::*;

    fn shapes(s: &Session) -> Vec<(Handle, Solid3d)> {
        s.doc()
            .unwrap()
            .model
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Solid3d(x) => Some((e.handle, x.clone())),
                _ => None,
            })
            .collect()
    }

    fn box3(x: &Solid3d) -> (Vec3, Vec3) {
        let pts: Vec<Vec3> = x.mesh.verts.iter().map(|p| x.world(*p)).collect();
        let f = pts[0];
        pts.iter().fold((f, f), |(lo, hi), p| {
            (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
        })
    }

    fn spans(x: &Solid3d, lo: [f64; 3], hi: [f64; 3], tol: f64) -> bool {
        let (a, b) = box3(x);
        near3(a, lo, tol) && near3(b, hi, tol)
    }

    fn volume(x: &Solid3d) -> f64 {
        let m = &x.mesh;
        m.tris.iter().filter_map(|t| m.tri(t)).map(|[a, b, c]| a.dot(b.cross(c)) / 6.0).sum::<f64>().abs()
    }

    fn area(x: &Solid3d) -> f64 {
        let m = &x.mesh;
        m.tris.iter().filter_map(|t| m.tri(t)).map(|[a, b, c]| (b - a).cross(c - a).len() / 2.0).sum()
    }

    fn handle(v: &Value) -> Handle {
        Handle::parse_hex(v.as_str().unwrap()).unwrap()
    }

    fn cube(s: &mut Session, at: [f64; 3], size: f64) -> Handle {
        let r = s.execute("box", &json!({ "corner1": at, "corner2": [at[0] + size, at[1] + size], "height": size })).unwrap();
        handle(&r["handle"])
    }

    fn poly(s: &mut Session, pts: &[(f64, f64)], closed: bool, width: f64, thickness: f64) -> Handle {
        let vs = pts.iter().map(|(x, y)| PolyVertex::new(Vec2::new(*x, *y))).collect();
        let EntityKind::LwPolyline(mut p) = super::super::super::helpers::lwpoly(vs, closed) else { panic!("not a polyline") };
        p.const_width = width;
        let h = s.add_entity(EntityKind::LwPolyline(p)).unwrap();
        s.doc_mut().unwrap().modify_entity(h, |e| e.common.thickness = thickness).unwrap();
        h
    }

    fn circle(s: &mut Session, c: [f64; 3], r: f64) -> Handle {
        s.add_entity(EntityKind::Circle(Circle { center: v3(c), radius: r })).unwrap()
    }

    #[test]
    fn arrays_and_alignments_of_solids() {
        let mut s = Session::new();
        let b = cube(&mut s, [0.0, 0.0, 0.0], 10.0);
        let p = json!({ "handles": [b.hex()], "rows": 2, "columns": 2, "levels": 2, "rowSpacing": 20, "columnSpacing": 20, "levelSpacing": 20 });
        s.execute("3darray", &p).unwrap();
        let xs = shapes(&s);
        assert_eq!(xs.len(), 8);
        assert!(xs.iter().any(|(_, x)| spans(x, [20.0, 20.0, 20.0], [30.0, 30.0, 30.0], 1e-6)));
        s.undo().unwrap();
        // Six round a vertical axis at x = 50: the far one is at x 90 to 100.
        s.execute("3darray", &json!({ "handles": [b.hex()], "type": "polar", "count": 6, "axis": [[50, 5, 0], [50, 5, 1]] })).unwrap();
        let xs = shapes(&s);
        assert_eq!(xs.len(), 6);
        assert!(xs.iter().any(|(_, x)| spans(x, [90.0, 0.0, 0.0], [100.0, 10.0, 10.0], 1e-6)));
        s.undo().unwrap();
        // X onto Y and Y onto -X at (100, 0, 0): the cube turns a quarter about its corner.
        let p =
            json!({ "handles": [b.hex()], "source": [[0, 0, 0], [10, 0, 0], [0, 10, 0]], "destination": [[100, 0, 0], [100, 10, 0], [90, 0, 0]] });
        s.execute("3dalign", &p).unwrap();
        let xs = shapes(&s);
        assert!(spans(&xs[0].1, [90.0, 0.0, 0.0], [100.0, 10.0, 10.0], 1e-6), "{:?}", box3(&xs[0].1));
    }

    #[test]
    fn polysolid_walls_along_points_and_curves() {
        let mut s = Session::new();
        // An L of wall 2 wide and 30 high, left of the path: 296 square units of plan.
        let r = s.execute("polysolid", &json!({ "points": [[0, 0], [100, 0], [100, 50]], "width": 2, "height": 30, "justify": "left" })).unwrap();
        let (_, x) = shapes(&s).into_iter().find(|(h, _)| *h == handle(&r["handle"])).unwrap();
        assert!(spans(&x, [0.0, 0.0, 0.0], [100.0, 50.0, 30.0], 1e-6), "{:?}", box3(&x));
        assert!((volume(&x) - 296.0 * 30.0).abs() < 1e-3, "{}", volume(&x));
        // A half circle of radius 5, 1 by 1: about 5π.
        s.execute("polysolid", &json!({ "points": [[0, 100], [10, 100]], "bulges": [1], "width": 1, "height": 1 })).unwrap();
        let v = volume(&shapes(&s)[1].1);
        assert!((v / (5.0 * PI) - 1.0).abs() < 0.01, "{v}");
        // A circle of radius 10 becomes a ring 2 wide and 5 high, and goes.
        let c = circle(&mut s, [0.0, 300.0, 0.0], 10.0);
        s.execute("polysolid", &json!({ "handle": c.hex(), "width": 2, "height": 5 })).unwrap();
        assert!(s.doc().unwrap().entity(c).is_none());
        let v = volume(&shapes(&s)[2].1);
        assert!((v / (2.0 * PI * 10.0 * 2.0 * 5.0) - 1.0).abs() < 0.01, "{v}");
        // Drawn at the command line: a line, then an arc on tangent (a quarter round (10, 10)).
        s.start("polysolid").unwrap();
        s.input(Input::Text("w".into())).unwrap();
        s.input(Input::Text("1".into())).unwrap();
        s.input(Input::Text("h".into())).unwrap();
        s.input(Input::Text("4".into())).unwrap();
        s.input(Input::Point(Vec2::new(0.0, 500.0))).unwrap();
        s.input(Input::Point(Vec2::new(10.0, 500.0))).unwrap();
        s.input(Input::Text("a".into())).unwrap();
        s.input(Input::Point(Vec2::new(20.0, 510.0))).unwrap();
        s.input(Input::Enter).unwrap();
        let xs = shapes(&s);
        assert_eq!(xs.len(), 4);
        assert!(spans(&xs[3].1, [0.0, 499.5, 0.0], [20.5, 510.0, 4.0], 1e-3), "{:?}", box3(&xs[3].1));
        s.undo().unwrap();
        assert_eq!(shapes(&s).len(), 3, "one undo step");
        // Closed loops with arcs hold their whole length: a planter drawn with a Second point
        // arc and Close (300, a half circle of radius 100, 300 and 200 round), and a loop with a
        // quarter-round corner.
        for t in ["polysolid", "w", "20", "h", "300", "1200,300", "1500,300", "a", "s", "1600,400", "1500,500", "l", "1200,500", "c"] {
            s.cmdline(t).unwrap();
        }
        let v = volume(&shapes(&s)[3].1);
        assert!((v / (6000.0 * (800.0 + 100.0 * PI)) - 1.0).abs() < 0.005, "{v}");
        let q = (PI / 8.0).tan();
        s.execute(
            "polysolid",
            &json!({ "points": [[0, 0], [200, 0], [300, 100], [300, 200], [0, 200]], "bulges": [0, q], "closed": true, "width": 20, "height": 300 }),
        )
        .unwrap();
        let v = volume(&shapes(&s)[4].1);
        assert!((v / (6000.0 * (800.0 + 50.0 * PI)) - 1.0).abs() < 0.005, "{v}");
        // Two rounded corners, each after a sharp one (a gap of one stretch, once).
        s.execute(
            "polysolid",
            &json!({ "points": [[0, 0], [200, 0], [300, 100], [300, 200], [100, 200], [0, 100]], "bulges": [0, q, 0, 0, q, 0], "closed": true,
                     "width": 20, "height": 300 }),
        )
        .unwrap();
        let v = volume(&shapes(&s)[5].1);
        assert!((v / (6000.0 * (600.0 + 100.0 * PI)) - 1.0).abs() < 0.005, "{v}");
    }

    #[test]
    fn conversions_to_solids_and_surfaces() {
        let mut s = Session::new();
        let closed = poly(&mut s, &[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)], true, 0.0, 5.0);
        let wide = poly(&mut s, &[(0.0, 50.0), (10.0, 50.0)], false, 2.0, 3.0);
        let round = circle(&mut s, [50.0, 0.0, 0.0], 2.0);
        s.doc_mut().unwrap().modify_entity(round, |e| e.common.thickness = -4.0).unwrap();
        let plain = poly(&mut s, &[(0.0, 80.0), (10.0, 80.0)], false, 0.0, 0.0);
        let r = s.execute("convtosolid", &json!({ "handles": [closed.hex(), wide.hex(), round.hex(), plain.hex()] })).unwrap();
        assert_eq!((r["handles"].as_array().map(Vec::len), r["skipped"].as_u64()), (Some(3), Some(1)), "{r}");
        assert!(r["message"].as_str().unwrap().contains("1 object(s) could not be converted"));
        let xs = shapes(&s);
        assert!((volume(&xs[0].1) - 500.0).abs() < 1e-6 && (volume(&xs[1].1) - 60.0).abs() < 1e-6, "{} {}", volume(&xs[0].1), volume(&xs[1].1));
        assert!(spans(&xs[2].1, [48.0, -2.0, -4.0], [52.0, 2.0, 0.0], 0.05), "{:?}", box3(&xs[2].1));
        assert!(s.doc().unwrap().entity(closed).is_none() && s.doc().unwrap().entity(plain).is_some());
        // A solid to a surface and back, keeping its part name and material (the cut list's).
        let (b, _) = xs[0].clone();
        let top = cadcraft_doc::Part { name: "Top".into(), material: "Oak 18".into() };
        s.doc_mut()
            .unwrap()
            .modify_entity(b, |e| {
                if let EntityKind::Solid3d(x) = &mut e.kind {
                    x.part = top.clone();
                }
            })
            .unwrap();
        let r = s.execute("convtosurface", &json!({ "handles": [b.hex()] })).unwrap();
        let surf = handle(&r["handles"][0]);
        let x = shapes(&s).into_iter().find(|(h, _)| *h == surf).unwrap().1;
        assert!(x.mesh.open && x.part == top, "{:?}", x.part);
        // Still a surface when its B-rep is tessellated again, as opening a saved drawing does.
        assert!(cadcraft_kernel::tessellate(&x.brep).unwrap().open);
        let r = s.execute("convtosolid", &json!({ "handles": [surf.hex()] })).unwrap();
        let back = shapes(&s).into_iter().find(|(h, _)| *h == handle(&r["handles"][0])).unwrap().1;
        assert!(!back.mesh.open && (volume(&back) - 500.0).abs() < 1e-6);
        assert_eq!(back.part, top);
        // A line with thickness, a region with a hole, a 3D face and a closed mesh.
        let l = line3(&mut s, [0.0, 200.0, 0.0], [10.0, 200.0, 0.0]);
        s.doc_mut().unwrap().modify_entity(l, |e| e.common.thickness = 6.0).unwrap();
        let lp = |pts: &[(f64, f64)], outer: bool| HatchLoop {
            vertices: pts.iter().map(|(x, y)| PolyVertex::new(Vec2::new(*x, *y))).collect(),
            outer,
            sources: Vec::new(),
        };
        let region = Region {
            loops: vec![
                lp(&[(0.0, 300.0), (10.0, 300.0), (10.0, 310.0), (0.0, 310.0)], true),
                lp(&[(4.0, 304.0), (6.0, 304.0), (6.0, 306.0), (4.0, 306.0)], false),
            ],
        };
        let rg = s.add_entity(EntityKind::Region(region)).unwrap();
        let face = s
            .add_entity(EntityKind::Face3d(Face3d {
                corners: [Vec3::new(0.0, 400.0, 0.0), Vec3::new(10.0, 400.0, 0.0), Vec3::new(10.0, 410.0, 5.0), Vec3::new(0.0, 410.0, 5.0)],
                hidden_edges: 0,
            }))
            .unwrap();
        let r = s.execute("convtosurface", &json!({ "handles": [l.hex(), rg.hex(), face.hex()] })).unwrap();
        let made: Vec<Handle> = r["handles"].as_array().unwrap().iter().map(handle).collect();
        assert_eq!(made.len(), 3, "{r}");
        let get = |s: &Session, h: Handle| shapes(s).into_iter().find(|(k, _)| *k == h).unwrap().1;
        assert!((area(&get(&s, made[0])) - 60.0).abs() < 1e-6, "{}", area(&get(&s, made[0])));
        assert!((area(&get(&s, made[1])) - 96.0).abs() < 1e-6, "{}", area(&get(&s, made[1])));
        assert!((area(&get(&s, made[2])) - 10.0 * 125f64.sqrt()).abs() < 1e-6, "{}", area(&get(&s, made[2])));
        // A polyface cube (eight corners, six faces) becomes a solid.
        let c = |x: f64, y: f64, z: f64| Vec3::new(x, y + 600.0, z);
        let vertices = vec![
            c(0.0, 0.0, 0.0),
            c(1.0, 0.0, 0.0),
            c(1.0, 1.0, 0.0),
            c(0.0, 1.0, 0.0),
            c(0.0, 0.0, 1.0),
            c(1.0, 0.0, 1.0),
            c(1.0, 1.0, 1.0),
            c(0.0, 1.0, 1.0),
        ];
        let faces = vec![[1, 4, 3, 2], [5, 6, 7, 8], [1, 2, 6, 5], [2, 3, 7, 6], [3, 4, 8, 7], [4, 1, 5, 8]];
        let mesh = s.add_entity(EntityKind::PolyMesh(PolyMesh { vertices, m: 0, n: 0, closed_m: false, closed_n: false, faces })).unwrap();
        let r = s.execute("convtosolid", &json!({ "handles": [mesh.hex()] })).unwrap();
        let x = get(&s, handle(&r["handles"][0]));
        assert!(!x.mesh.open && (volume(&x) - 1.0).abs() < 1e-9, "{}", volume(&x));
    }

    #[test]
    fn three_d_print_writes_an_stl_and_says_how_big() {
        let mut s = Session::new();
        cube(&mut s, [0.0, 0.0, 0.0], 10.0);
        s.execute("planesurf", &json!({ "corner1": [50, 0, 0], "corner2": [60, 10] })).unwrap();
        let dir = std::env::temp_dir().join(format!("cadrhino-3dprint-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("print.stl");
        let r = s.execute("3dprint", &json!({ "path": path.to_string_lossy() })).unwrap();
        let m = r["message"].as_str().unwrap();
        assert!(m.contains("10.0000 x 10.0000 x 10.0000") && m.contains("1 surface(s) left out"), "{m}");
        assert!(std::fs::metadata(&path).unwrap().len() > 84);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loft_guides_path_and_surface_mode() {
        let mut s = Session::new();
        let secs = |s: &mut Session| [circle(s, [0.0, 0.0, 0.0], 5.0), circle(s, [0.0, 0.0, 10.0], 5.0)];
        // Surface mode: the tube without its ends, and the sections stay.
        let [a, b] = secs(&mut s);
        let r = s.execute("loft", &json!({ "handles": [a.hex(), b.hex()], "mode": "surface" })).unwrap();
        assert_eq!(r["surface"], true);
        let x = shapes(&s).pop().unwrap().1;
        assert!(x.mesh.open && (area(&x) / (2.0 * PI * 5.0 * 10.0) - 1.0).abs() < 0.01, "{}", area(&x));
        assert_eq!(count(&s), 3);
        s.undo().unwrap();
        // Along a path: a solid; sections and path go.
        let path = line3(&mut s, [0.0, 0.0, 0.0], [0.0, 0.0, 10.0]);
        let r = s.execute("loft", &json!({ "handles": [a.hex(), b.hex()], "path": path.hex() })).unwrap();
        assert_eq!(r["surface"], false);
        let x = shapes(&s).pop().unwrap().1;
        assert!((volume(&x) / (PI * 250.0) - 1.0).abs() < 0.01, "{}", volume(&x));
        assert_eq!(count(&s), 1);
        s.undo().unwrap();
        // Following a guide on the sections' rims.
        let guide = line3(&mut s, [5.0, 0.0, 0.0], [5.0, 0.0, 10.0]);
        s.execute("loft", &json!({ "handles": [a.hex(), b.hex()], "guides": [guide.hex()] })).unwrap();
        let x = shapes(&s).pop().unwrap().1;
        assert!(spans(&x, [-5.0, -5.0, 0.0], [5.0, 5.0, 10.0], 0.1), "{:?}", box3(&x));
    }

    #[test]
    fn planar_surfaces_with_holes_and_islands() {
        let mut s = Session::new();
        let outer = poly(&mut s, &[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)], true, 0.0, 0.0);
        let hole = circle(&mut s, [5.0, 5.0, 0.0], 2.0);
        let island = poly(&mut s, &[(4.5, 4.5), (5.5, 4.5), (5.5, 5.5), (4.5, 5.5)], true, 0.0, 0.0);
        let above = circle(&mut s, [5.0, 5.0, 3.0], 1.0);
        let r = s.execute("planesurf", &json!({ "handles": [outer.hex(), hole.hex(), island.hex(), above.hex()] })).unwrap();
        let made: Vec<Handle> = r["handles"].as_array().unwrap().iter().map(handle).collect();
        assert_eq!(made.len(), 3, "{r}");
        let get = |h: Handle| shapes(&s).into_iter().find(|(k, _)| *k == h).unwrap().1;
        let areas: Vec<f64> = made.iter().map(|h| area(&get(*h))).collect();
        assert!((areas[0] - (100.0 - PI * 4.0)).abs() < 0.05 && (areas[1] - 1.0).abs() < 1e-6 && (areas[2] - PI).abs() < 0.02, "{areas:?}");
        assert_eq!(count(&s), 7, "the curves stay");
    }

    #[test]
    fn slice_by_planes_objects_and_surfaces() {
        let mut s = Session::new();
        let heights = |s: &Session| {
            let mut v: Vec<(f64, f64)> = shapes(s).iter().map(|(_, x)| box3(x)).map(|(a, b)| (a.z, b.z)).collect();
            v.sort_by(|a, b| a.0.total_cmp(&b.0));
            v
        };
        let near = |v: Vec<(f64, f64)>, want: &[(f64, f64)]| {
            v.len() == want.len() && v.iter().zip(want).all(|(a, b)| (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6)
        };
        let b = cube(&mut s, [0.0, 0.0, 0.0], 10.0);
        let slice = |s: &mut Session, p: Value, want: &[(f64, f64)]| {
            let mut q = json!({ "handles": [b.hex()] });
            if let (Some(o), Some(e)) = (q.as_object_mut(), p.as_object()) {
                o.extend(e.clone());
            }
            s.execute("slice", &q).unwrap();
            assert!(near(heights(s), want), "{p}: {:?}", heights(s));
            s.undo().unwrap();
        };
        slice(&mut s, json!({ "points": [[0, 0, 4], [1, 0, 4], [0, 1, 4]] }), &[(0.0, 4.0), (4.0, 10.0)]);
        slice(&mut s, json!({ "plane": "xy", "point": [0, 0, 6], "keep": "side", "side": [5, 5, 9] }), &[(6.0, 10.0)]);
        slice(&mut s, json!({ "zaxis": [[0, 0, 3], [0, 0, 10]], "keep": "side", "side": [5, 5, 0] }), &[(0.0, 3.0)]);
        slice(&mut s, json!({ "origin": [0, 0, 2], "normal": [0, 0, -1] }), &[(0.0, 2.0), (2.0, 10.0)]);
        slice(&mut s, json!({ "view": true, "point": [0, 0, 8] }), &[(0.0, 8.0), (8.0, 10.0)]);
        let c = circle(&mut s, [5.0, 5.0, 7.0], 20.0);
        slice(&mut s, json!({ "object": c.hex() }), &[(0.0, 7.0), (7.0, 10.0)]);
        // A surface across the box at z = 5: both pieces, or the one above.
        let r = s.execute("planesurf", &json!({ "corner1": [-5, -5, 5], "corner2": [15, 15] })).unwrap();
        let surf = handle(&r["handle"]);
        s.execute("slice", &json!({ "handles": [b.hex(), surf.hex()], "surface": surf.hex() })).unwrap();
        let solid_heights = |s: &Session| {
            let mut v: Vec<(f64, f64)> = shapes(s).iter().filter(|(_, x)| !x.mesh.open).map(|(_, x)| box3(x)).map(|(a, b)| (a.z, b.z)).collect();
            v.sort_by(|a, b| a.0.total_cmp(&b.0));
            v
        };
        assert!(near(solid_heights(&s), &[(0.0, 5.0), (5.0, 10.0)]), "{:?}", solid_heights(&s));
        assert!(s.doc().unwrap().entity(surf).is_some(), "the surface stays");
        s.undo().unwrap();
        s.execute("slice", &json!({ "handles": [b.hex()], "surface": surf.hex(), "keep": "side", "side": [5, 5, 20] })).unwrap();
        assert!(near(solid_heights(&s), &[(5.0, 10.0)]), "{:?}", solid_heights(&s));
        s.undo().unwrap();
        // At the command line: the ZX plane through a point 3 out, both sides.
        s.set_selection(vec![b]);
        s.start("slice").unwrap();
        s.input(Input::Text("zx".into())).unwrap();
        s.input(Input::Point(Vec2::new(0.0, 3.0))).unwrap();
        s.input(Input::Enter).unwrap();
        let mut ys: Vec<(f64, f64)> = shapes(&s).iter().filter(|(_, x)| !x.mesh.open).map(|(_, x)| box3(x)).map(|(a, b)| (a.y, b.y)).collect();
        ys.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert!(near(ys.clone(), &[(0.0, 3.0), (3.0, 10.0)]), "{ys:?}");
    }
}
