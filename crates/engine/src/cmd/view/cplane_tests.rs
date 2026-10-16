#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use cadcraft_doc::{Entity, EntityKind, Solid3d, Space};
use cadcraft_geom::{Mat4, Mesh3, Vec2, Vec3};
use serde_json::{Value, json};

use super::preset;
use crate::Session;

fn near(a: Vec3, b: Vec3) -> bool {
    (a - b).len() < 1e-9
}

fn near_m(a: &Mat4, b: &Mat4) -> bool {
    a.m.iter().flatten().zip(b.m.iter().flatten()).all(|(x, y)| (x - y).abs() < 1e-9)
}

fn front() -> Mat4 {
    preset("front").unwrap()
}

fn right() -> Mat4 {
    preset("right").unwrap()
}

fn set(s: &mut Session, p: Value) {
    s.execute("cplane", &p).unwrap();
}

fn last(s: &Session) -> Entity {
    (**s.doc().unwrap().model.last().unwrap()).clone()
}

/// Where an object's own 2D point `p` is in the world.
fn world(e: &Entity, p: Vec2) -> Vec3 {
    e.plane().unwrap().apply(p.to3(0.0))
}

#[test]
fn line_and_circle_drawn_in_front_stand_in_the_xz_plane() {
    let mut s = Session::new();
    set(&mut s, json!({ "preset": "front" }));
    assert_eq!(s.cplane(), front());
    s.execute("line", &json!({ "points": [[0, 0], [10, 5]] })).unwrap();
    let EntityKind::Line(l) = last(&s).kind else { panic!() };
    assert!(near(l.a, Vec3::ZERO) && near(l.b, Vec3::new(10.0, 0.0, 5.0)), "{l:?}");
    s.execute("circle", &json!({ "center": [2, 3], "radius": 1.5 })).unwrap();
    let c = last(&s);
    assert!(near(c.common.extrusion, Vec3::new(0.0, -1.0, 0.0)));
    let EntityKind::Circle(k) = &c.kind else { panic!() };
    assert!(near(world(&c, k.center.xy()), Vec3::new(2.0, 0.0, 3.0)) && (k.radius - 1.5).abs() < 1e-12);
    // Typed at the command line too.
    s.cmdline("line 1,1 4,1").unwrap();
    s.cancel();
    let EntityKind::Line(l) = last(&s).kind else { panic!() };
    assert!(near(l.a, Vec3::new(1.0, 0.0, 1.0)) && near(l.b, Vec3::new(4.0, 0.0, 1.0)), "{l:?}");
    // A polyline built point by point in a plane away from the origin stays in it.
    set(&mut s, json!({ "origin": [0, -2, 0], "xaxis": [0, 0, 1], "ypoint": [1, -2, 0] }));
    s.cmdline("pline 0,0 3,0 3,4").unwrap();
    s.cancel();
    let p = last(&s);
    let EntityKind::LwPolyline(k) = &p.kind else { panic!() };
    let w: Vec<Vec3> = k.vertices.iter().map(|v| world(&p, v.p)).collect();
    assert_eq!(w.len(), 3);
    assert!(near(w[0], Vec3::new(0.0, -2.0, 0.0)) && near(w[1], Vec3::new(0.0, -2.0, 3.0)) && near(w[2], Vec3::new(4.0, -2.0, 3.0)), "{w:?}");
}

#[test]
fn world_keeps_plan_exactly() {
    let mut s = Session::new();
    assert_eq!(s.cplane(), Mat4::IDENTITY);
    assert!(Arc::ptr_eq(&s.pick_doc().unwrap(), &s.state().unwrap().doc));
    // Setting World again changes nothing: no undo step.
    let before = s.state().unwrap().doc.clone();
    set(&mut s, json!({ "preset": "world" }));
    set(&mut s, json!({}));
    assert!(Arc::ptr_eq(&before, &s.state().unwrap().doc));
    assert!(s.state().unwrap().undo.is_empty());
    s.execute("circle", &json!({ "center": [2, 3], "radius": 1.5 })).unwrap();
    let c = last(&s);
    assert_eq!(c.common.extrusion, Vec3::Z);
    assert_eq!(c.kind, EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::new(2.0, 3.0, 0.0), radius: 1.5 }));
    assert_eq!(s.cplane_matrix(&cadcraft_geom::Mat3::translate(Vec2::new(1.0, 0.0))), None);
}

#[test]
fn move_in_right_moves_along_world_y_and_z() {
    let mut s = Session::new();
    s.execute("line", &json!({ "points": [[0, 0], [1, 0]] })).unwrap();
    let h = last(&s).handle;
    set(&mut s, json!({ "preset": "right" }));
    s.execute("move", &json!({ "handles": [h.hex()], "delta": [2, 3] })).unwrap();
    let EntityKind::Line(l) = &s.doc().unwrap().entity(h).unwrap().kind else { panic!() };
    assert!(near(l.a, Vec3::new(0.0, 2.0, 3.0)) && near(l.b, Vec3::new(1.0, 2.0, 3.0)), "{l:?}");
    // ROTATE about the plane's normal (world X) turns world Y into world Z.
    s.execute("rotate", &json!({ "handles": [h.hex()], "base": [2, 3], "angle": 90 })).unwrap();
    let EntityKind::Line(l) = &s.doc().unwrap().entity(h).unwrap().kind else { panic!() };
    assert!(near(l.a, Vec3::new(0.0, 2.0, 3.0)) && near(l.b, Vec3::new(1.0, 2.0, 3.0)), "the line lies along the normal: {l:?}");
    // A locked layer's objects stay.
    s.doc_mut().unwrap().layers.iter_mut().for_each(|l| l.locked = true);
    let before = s.state().unwrap().doc.clone();
    s.execute("move", &json!({ "handles": [h.hex()], "delta": [5, 5] })).unwrap();
    assert_eq!(s.doc().unwrap().entity(h), before.entity(h));
}

#[test]
fn a_solid_made_in_front_stands_in_xz() {
    let mut s = Session::new();
    set(&mut s, json!({ "preset": "front" }));
    // A 2 × 3 × 4 box from the plane's origin, as BOX builds it in the plane's coordinates.
    let c = |x: f64, y: f64, z: f64| Vec3::new(x, y, z);
    let pts = [c(0.0, 0.0, 0.0), c(2.0, 0.0, 0.0), c(2.0, 3.0, 0.0), c(0.0, 3.0, 0.0)];
    let mut edges: Vec<Vec<Vec3>> = Vec::new();
    for i in 0..4 {
        let (a, b) = (pts[i], pts[(i + 1) % 4]);
        edges.push(vec![a, b]);
        edges.push(vec![a + c(0.0, 0.0, 4.0), b + c(0.0, 0.0, 4.0)]);
        edges.push(vec![a, a + c(0.0, 0.0, 4.0)]);
    }
    let mesh = Mesh3 { edges, ..Default::default() };
    let sol = Solid3d { mesh: Arc::new(mesh), ..Default::default() };
    s.add_entity(EntityKind::Solid3d(sol)).unwrap();
    let EntityKind::Solid3d(sol) = last(&s).kind else { panic!() };
    let corners = sol.corners();
    assert!(corners.iter().all(|p| (0.0..=2.0 + 1e-9).contains(&p.x) && (-4.0 - 1e-9..=1e-9).contains(&p.y) && (-1e-9..=3.0 + 1e-9).contains(&p.z)));
    assert!(corners.iter().any(|p| near(*p, c(2.0, 0.0, 3.0))) && corners.iter().any(|p| near(*p, c(0.0, -4.0, 0.0))));
}

#[test]
fn snaps_and_picks_see_objects_in_the_plane() {
    use crate::snap::{self, mode};
    let mut s = Session::new();
    set(&mut s, json!({ "preset": "front" }));
    s.execute("line", &json!({ "points": [[0, 0], [10, 5]] })).unwrap();
    s.execute("circle", &json!({ "center": [2, 3], "radius": 1.5 })).unwrap();
    let circle = last(&s).handle;
    let seen = s.pick_doc().unwrap();
    let end = snap::osnap(&seen, &Space::Model, Vec2::new(9.9, 5.1), 0.5, mode::END, None, false).unwrap();
    assert!((end.point - Vec2::new(10.0, 5.0)).len() < 1e-9 && end.mode == mode::END);
    let cen = snap::osnap(&seen, &Space::Model, Vec2::new(2.1, 3.05), 0.3, mode::CEN, None, false).unwrap();
    assert!((cen.point - Vec2::new(2.0, 3.0)).len() < 1e-9 && cen.mode == mode::CEN);
    // The running snap the UI asks for at a point prompt.
    s.settings.osmode = mode::END;
    s.start("line").unwrap();
    let q = snap::tracking::CursorQuery { base: None, deferred: false, corner: false, aperture: 0.5, tol: 0.1, now: 0.0, shift: false };
    let r = s.snap_cursor(Vec2::new(10.1, 4.9), &q);
    assert!(r.snap.is_some() && (r.point - Vec2::new(10.0, 5.0)).len() < 1e-9);
    s.cancel();
    // Seen from World the circle is edge-on: nothing to snap at its centre's plan point.
    set(&mut s, json!({ "preset": "world" }));
    assert!(snap::osnap(s.doc().unwrap(), &Space::Model, Vec2::new(2.1, 3.05), 0.3, mode::CEN, None, false).is_none());
    // Picking: the circle is under its local point in Front only.
    s.execute("select", &json!({ "at": [3.5, 3.0], "aperture": 0.1 })).unwrap();
    assert!(s.selection().is_empty());
    set(&mut s, json!({ "preset": "front" }));
    s.execute("select", &json!({ "at": [3.5, 3.0], "aperture": 0.1 })).unwrap();
    assert_eq!(s.selection(), vec![circle]);
    s.execute("select", &json!({ "clear": true })).unwrap();
    s.execute("select", &json!({ "window": [[0.4, 1.4], [3.6, 4.6]] })).unwrap();
    assert_eq!(s.selection(), vec![circle]);
}

#[test]
fn grips_edit_in_the_plane() {
    let mut s = Session::new();
    set(&mut s, json!({ "preset": "front" }));
    s.execute("circle", &json!({ "center": [2, 3], "radius": 1.5 })).unwrap();
    let h = last(&s).handle;
    let g = s.grips_of(h).unwrap();
    assert!((g[0] - Vec2::new(2.0, 3.0)).len() < 1e-9, "{g:?}");
    let q = g.iter().position(|p| (*p - Vec2::new(3.5, 3.0)).len() < 1e-9).unwrap();
    s.execute("grip.move", &json!({ "handle": h.hex(), "index": q, "to": [4.0, 3.0] })).unwrap();
    let c = last(&s);
    let EntityKind::Circle(k) = &c.kind else { panic!() };
    assert!((k.radius - 2.0).abs() < 1e-9 && near(c.common.extrusion, -Vec3::Y));
    assert!(near(world(&c, k.center.xy()), Vec3::new(2.0, 0.0, 3.0)));
    // Moving by the centre grip moves it in the plane.
    s.execute("grip.move", &json!({ "handles": [h.hex()], "baseHandle": h.hex(), "index": 0, "to": [5.0, 3.0], "mode": "move" })).unwrap();
    let c = last(&s);
    let EntityKind::Circle(k) = &c.kind else { panic!() };
    assert!(near(world(&c, k.center.xy()), Vec3::new(5.0, 0.0, 3.0)));
}

#[test]
fn stretch_in_front_moves_the_vertices_in_the_window() {
    let mut s = Session::new();
    set(&mut s, json!({ "preset": "front" }));
    s.execute("rectang", &json!({ "p1": [0, 0], "p2": [4, 2] })).unwrap();
    let h = last(&s).handle;
    s.execute("stretch", &json!({ "window": [[3, -1], [5, 3]], "delta": [2, 0] })).unwrap();
    let e = s.doc().unwrap().entity(h).unwrap().as_ref().clone();
    let EntityKind::LwPolyline(p) = &e.kind else { panic!() };
    let w: Vec<Vec3> = p.vertices.iter().map(|v| world(&e, v.p)).collect();
    assert!(w.iter().any(|q| near(*q, Vec3::new(6.0, 0.0, 2.0))) && w.iter().any(|q| near(*q, Vec3::new(0.0, 0.0, 2.0))), "{w:?}");
}

#[test]
fn what_cannot_be_placed_or_computed_in_the_plane_is_refused() {
    let mut s = Session::new();
    s.execute("circle", &json!({ "center": [0, 0], "radius": 1 })).unwrap();
    let h = last(&s).handle;
    set(&mut s, json!({ "preset": "front" }));
    let before = s.state().unwrap().doc.clone();
    // A dimension stays level: it can't stand in Front.
    assert!(s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [4, 0], "at": [2, 2] })).is_err());
    // Commands that compute in plan are refused, at the prompt too.
    assert!(s.execute("offset", &json!({ "handle": h.hex(), "distance": 1, "side": [5, 5] })).is_err());
    assert!(s.start("offset").is_err() && s.current_prompt().is_none());
    assert!(s.start("trim").is_err());
    assert!(Arc::ptr_eq(&before, &s.state().unwrap().doc));
    // A plane that moves along its normal keeps level objects level.
    set(&mut s, json!({ "preset": "world" }));
    set(&mut s, json!({ "origin": [0, 0, 5] }));
    assert!(s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [4, 0], "at": [2, 2] })).is_ok());
    let EntityKind::Dimension(d) = last(&s).kind else { panic!() };
    assert!((d.defpt.z - 5.0).abs() < 1e-9);
}

#[test]
fn hostile_cplane_params_never_change_the_drawing() {
    let mut s = Session::new();
    set(&mut s, json!({ "preset": "right" }));
    for id in ["cplane", "ucs"] {
        let before = s.state().unwrap().doc.clone();
        for p in [
            json!({ "preset": 5 }),
            json!({ "preset": "sideways" }),
            json!({ "preset": "" }),
            json!({ "origin": "a" }),
            json!({ "origin": [1] }),
            json!({ "origin": [1, 2, 3, 4] }),
            json!({ "origin": ["NaN", 0, 0] }),
            json!({ "origin": [f64::INFINITY, 0, 0] }),
            json!({ "xaxis": [0, 0, 0] }),
            json!({ "xaxis": [1e-300, 0, 0] }),
            json!({ "origin": [0, 0, 0], "xaxis": [1, 0, 0], "ypoint": [5, 0, 0] }),
            json!({ "ypoint": [0, 0, 0] }),
            json!([1, 2]),
            json!(7),
            json!("front"),
        ] {
            // Errors name the command given.
            assert!(matches!(s.execute(id, &p), Err(crate::EngineError::BadParams { cmd, .. }) if cmd == id), "{id} {p}");
            assert!(Arc::ptr_eq(&before, &s.state().unwrap().doc), "{id} {p}");
            assert_eq!(s.cplane(), right());
        }
        // At the prompt: bad answers are asked again; nothing changes until the plane is given.
        s.cmdline(&format!("{id} sideways")).unwrap();
        s.cmdline("3point 0,0 0,0").unwrap();
        assert!(s.current_prompt().is_some());
        s.cancel();
        assert!(Arc::ptr_eq(&before, &s.state().unwrap().doc));
    }
    // Huge but finite: accepted, and drawing there never panics.
    set(&mut s, json!({ "origin": [1e300, 1e300, 1e300] }));
    let _ = s.execute("line", &json!({ "points": [[1e300, 0], [-1e300, 0]] }));
    let _ = s.execute("move", &json!({ "delta": [1e308, 1e308] }));
    // Outside the Model tab there is no construction plane to set.
    let mut s = Session::new();
    s.execute("layout.set", &json!({ "name": "Layout1" })).unwrap();
    assert!(s.execute("cplane", &json!({ "preset": "front" })).is_err());
    assert_eq!(s.cplane(), Mat4::IDENTITY);
}

#[test]
fn ucs_at_the_prompt() {
    let mut s = Session::new();
    s.cmdline("ucs").unwrap();
    assert_eq!(s.prompt_text(), "UCS Specify origin of UCS or [World/Top/Front/Right/BAck/Left/BOttom/3point/X/Y/Z] <World>:");
    s.cmdline("front").unwrap();
    assert_eq!(s.cplane(), front());
    s.cmdline("ucs w").unwrap();
    assert_eq!(s.cplane(), Mat4::IDENTITY);
    s.cmdline("ucs ba").unwrap();
    assert_eq!(s.cplane(), preset("back").unwrap());
    s.cmdline("cplane bo").unwrap();
    assert_eq!(s.cplane(), preset("bottom").unwrap());
    s.cmdline("ucs ").unwrap();
    assert_eq!(s.cplane(), Mat4::IDENTITY);
    // Three points: origin, X axis, a point on the XY plane (World's coordinates here).
    s.cmdline("ucs 3point 1,2,3 1,4,3 1,2,9").unwrap();
    assert!(near_m(&s.cplane(), &Mat4::from_cols(Vec3::Y, Vec3::Z, Vec3::X, Vec3::new(1.0, 2.0, 3.0))));
    // A point at the first prompt is the origin, in the current plane's coordinates; Enter keeps
    // the axes.
    s.cmdline("ucs 0,0,1  ").unwrap();
    assert!(near_m(&s.cplane(), &Mat4::from_cols(Vec3::Y, Vec3::Z, Vec3::X, Vec3::new(2.0, 2.0, 3.0))), "{:?}", s.cplane());
    // An X axis point only: the plane turns about its normal.
    s.cmdline("ucs 0,0 0,1 ").unwrap();
    let c = s.cplane();
    assert!(near(c.col(0), Vec3::Z) && near(c.col(1), -Vec3::Y) && near(c.col(2), Vec3::X) && near(c.col(3), Vec3::new(2.0, 2.0, 3.0)), "{c:?}");
    // The same in JSON, and an X axis along the normal tilts the plane the least.
    set(&mut s, json!({ "xaxis": [0, 1, 0] }));
    assert!(near_m(&s.cplane(), &Mat4::from_cols(Vec3::Y, Vec3::Z, Vec3::X, Vec3::new(2.0, 2.0, 3.0))));
    set(&mut s, json!({ "xaxis": [1, 0, 0] }));
    let c = s.cplane();
    assert!(near(c.col(0), Vec3::X) && near(c.col(1), Vec3::Z) && near(c.col(2), -Vec3::Y), "{c:?}");
}

#[test]
fn undo_brings_the_plane_back_and_leaves_the_views() {
    let mut s = Session::new();
    set(&mut s, json!({ "preset": "front" }));
    set(&mut s, json!({ "origin": [0, 0, 7] }));
    assert!(near(s.cplane().col(3), Vec3::new(0.0, 0.0, 7.0)));
    s.undo().unwrap();
    assert_eq!(s.cplane(), front());
    s.undo().unwrap();
    assert_eq!(s.cplane(), Mat4::IDENTITY);
    s.redo().unwrap();
    assert_eq!(s.cplane(), front());
    // Tiles: each keeps its own plane; undo restores the current tile's, views stay as shown.
    let mut s = Session::new();
    s.execute("viewports.rhino", &json!({})).unwrap();
    let planes: Vec<Mat4> = s.model_tiles().iter().map(|t| t.cplane).collect();
    assert_eq!(planes, vec![Mat4::IDENTITY, Mat4::IDENTITY, front(), right()]);
    s.activate_tile(2).unwrap();
    assert_eq!(s.cplane(), front());
    let views = s.model_tiles().iter().map(|t| t.view).collect::<Vec<_>>();
    set(&mut s, json!({ "origin": [0, -3, 0] }));
    assert!(near(s.cplane().col(3), Vec3::new(0.0, -3.0, 0.0)));
    assert_eq!(s.model_tiles().iter().map(|t| t.view).collect::<Vec<_>>(), views);
    s.undo().unwrap();
    assert_eq!(s.cplane(), front());
    assert!(s.model_tiles()[2].active);
    assert_eq!(s.model_tiles().iter().map(|t| t.view).collect::<Vec<_>>(), views);
    s.activate_tile(3).unwrap();
    assert_eq!(s.cplane(), right());
    // Maximized, the tile keeps its plane as the drawing's one view; and gets it back tiled.
    s.execute("viewports.single", &json!({})).unwrap();
    assert_eq!(s.cplane(), right());
    set(&mut s, json!({ "preset": "left" }));
    s.execute("viewports.maximize", &json!({})).unwrap();
    // (The current tile comes first.)
    assert_eq!(s.model_tiles().iter().map(|t| t.cplane).collect::<Vec<_>>(), vec![preset("left").unwrap(), Mat4::IDENTITY, Mat4::IDENTITY, front()]);
    assert_eq!(s.cplane(), preset("left").unwrap());
}

#[test]
fn tile_planes_round_trip_through_dxf() {
    let mut s = Session::new();
    s.execute("viewports.rhino", &json!({})).unwrap();
    s.activate_tile(1).unwrap();
    let tilted = Mat4::frame(Vec3::new(1.0, 2.0, 3.0), Vec3::new(1.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0)).unwrap();
    set(&mut s, json!({ "origin": [1, 2, 3], "xaxis": [1, 1, 0], "ypoint": [1, 2, 4] }));
    assert!(near_m(&s.cplane(), &tilted));
    let d = super::super::vports::with_live_tiles(&s).unwrap();
    let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(&d).as_bytes()).unwrap();
    let cfg = back.vports.iter().find(|c| c.is_active()).unwrap();
    let mut want = s.model_tiles().iter().map(|t| t.cplane).collect::<Vec<_>>();
    // The file keeps the current tile first.
    want.swap(0, 1);
    assert_eq!(cfg.tiles.len(), 4);
    for (t, w) in cfg.tiles.iter().zip(&want) {
        assert!(near_m(&t.cplane, w), "{:?} {w:?}", t.cplane);
    }
    // One view: the drawing's UCS.
    let mut s = Session::new();
    set(&mut s, json!({ "preset": "front" }));
    set(&mut s, json!({ "origin": [5, 0, 0] }));
    let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc().unwrap()).as_bytes()).unwrap();
    assert!(near_m(&back.header.cplane(), &s.cplane()));
    let mut t = Session::new();
    t.open_drawing(back, "back", None);
    assert!(near_m(&t.cplane(), &s.cplane()));
    // World writes no UCS: files are as they were.
    let plain = Session::new();
    assert!(!cadcraft_io::write_dxf(plain.doc().unwrap()).contains("\n110\n"));
}

/// `text` (a DXF file) with its line ends as `\n`.
fn lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[test]
fn a_ucs_from_another_program_opens_in_world_and_is_kept() {
    // A file as another program writes it: a UCS in the header and on the *ACTIVE VPORT, no
    // Cadino mark.
    let mut s = Session::new();
    set(&mut s, json!({ "origin": [5, 1, 0], "xaxis": [0, 1, 0] }));
    s.execute("line", &json!({ "points": [[0, 0], [4, 0]] })).unwrap();
    let ours = lf(&cadcraft_io::write_dxf(s.doc().unwrap()));
    assert_eq!(ours.matches("1001\nCADCRAFT\n1000\nCPLANE\n").count(), 1, "Cadino marks its plane");
    let header = "9\n$UCSORG\n10\n5\n20\n1\n30\n0\n9\n$UCSXDIR\n10\n0\n20\n1\n30\n0\n9\n$UCSYDIR\n10\n-1\n20\n0\n30\n0\n";
    let foreign = ours.replacen("1001\nCADCRAFT\n1000\nCPLANE\n", "", 1).replacen("0\nENDSEC\n", &format!("{header}0\nENDSEC\n"), 1);
    let d = cadcraft_io::read_dxf(foreign.as_bytes()).unwrap();
    assert_eq!(d.header.cplane(), Mat4::IDENTITY);
    assert!(d.header.point("UCSORG").is_some(), "the header keeps the file's UCS");
    let mut t = Session::new();
    t.open_drawing(d, "foreign", None);
    assert_eq!(t.cplane(), Mat4::IDENTITY, "2D work starts in World, as in AutoCAD");
    // Plan commands run as they always did, typed points are world points.
    t.cmdline("line 0,0 3,0").unwrap();
    t.cancel();
    let EntityKind::Line(l) = last(&t).kind else { panic!() };
    assert_eq!((l.a, l.b), (Vec3::ZERO, Vec3::new(3.0, 0.0, 0.0)));
    assert!(t.execute("offset", &json!({ "handle": last(&t).handle.hex(), "distance": 1, "side": [1, 1] })).is_ok());
    // Saved again, the VPORT keeps the other program's UCS, still unmarked.
    let again = lf(&cadcraft_io::write_dxf(t.doc().unwrap()));
    let back = cadcraft_io::read_dxf(again.as_bytes()).unwrap();
    assert_eq!(back.header.cplane(), Mat4::IDENTITY);
    assert!(!again.contains("1000\nCPLANE\n"));
    let vport = &again[again.find("AcDbViewportTableRecord").unwrap()..];
    assert!(vport.contains("\n65\n") && vport.contains("\n110\n") && vport.contains("\n111\n"), "{}", &vport[..600.min(vport.len())]);
    // A hostile file can't set Cadino's own variables from its header.
    let hostile = foreign.replacen(
        "9\n$UCSORG\n",
        "9\n$CADCRAFT_CPLANEORG\n10\n1\n20\n2\n30\n3\n9\n$CADCRAFT_CPLANEXDIR\n10\n0\n20\n0\n30\n1\n9\n$CADCRAFT_CPLANEYDIR\n10\n1\n20\n0\n30\n0\n9\n$UCSORG\n",
        1,
    );
    assert_eq!(cadcraft_io::read_dxf(hostile.as_bytes()).unwrap().header.cplane(), Mat4::IDENTITY);
}

/// AutoCAD's everyday 2D UCS: the origin at (10, 5), X along world Y (so the plane's Y runs
/// along world -X).
fn turned(s: &mut Session) {
    set(s, json!({ "origin": [10, 5, 0], "xaxis": [0, 1, 0] }));
}

#[test]
fn a_level_plane_runs_plan_commands_in_its_coordinates() {
    let mut s = Session::new();
    turned(&mut s);
    s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
    let h = last(&s).handle;
    let EntityKind::Line(l) = last(&s).kind else { panic!() };
    assert!(near(l.a, Vec3::new(10.0, 5.0, 0.0)) && near(l.b, Vec3::new(10.0, 15.0, 0.0)), "{l:?}");
    // OFFSET at the prompt, points in the plane: 2 toward its +Y is 2 toward world -X.
    s.cmdline("offset 2").unwrap();
    s.cmdline("5,0").unwrap();
    // The rubber band is in the plane's coordinates too.
    let pv = s.preview(Vec2::new(5.0, 1.0));
    assert!(
        pv.iter().any(|e| matches!(&e.kind, EntityKind::Line(k) if near(k.a, Vec3::new(0.0, 2.0, 0.0)) && near(k.b, Vec3::new(10.0, 2.0, 0.0)))),
        "{} {pv:?}",
        s.prompt_text()
    );
    s.cmdline("5,1").unwrap();
    s.cmdline("").unwrap();
    let off = last(&s);
    assert_ne!(off.handle, h);
    let EntityKind::Line(l) = off.kind else { panic!() };
    assert!(near(l.a, Vec3::new(8.0, 5.0, 0.0)) && near(l.b, Vec3::new(8.0, 15.0, 0.0)), "{l:?}");
    assert_eq!(off.common.extrusion, Vec3::Z);
    // HATCH of a rectangle drawn in the plane fills it where it is.
    s.execute("rectang", &json!({ "p1": [0, 0], "p2": [4, 2] })).unwrap();
    let rect = last(&s).handle;
    s.cmdline("hatch s").unwrap();
    s.input(crate::Input::Pick(vec![rect])).unwrap();
    s.cmdline("").unwrap();
    let hatch = s.doc().unwrap().model.iter().find(|e| matches!(e.kind, EntityKind::Hatch(_))).map(|e| (**e).clone()).unwrap();
    let b = cadcraft_doc::entity_bounds(s.doc().unwrap(), &hatch, 0);
    assert!(b.min.near(Vec2::new(8.0, 5.0), 1e-9) && b.max.near(Vec2::new(10.0, 9.0), 1e-9), "{b:?}");
    // The JSON forms of plan commands take world coordinates there.
    let n = s.doc().unwrap().model.len();
    s.execute("offset", &json!({ "handle": h.hex(), "distance": 1, "side": [11, 10] })).unwrap();
    let EntityKind::Line(l) = last(&s).kind else { panic!() };
    assert!(s.doc().unwrap().model.len() == n + 1 && near(l.a, Vec3::new(11.0, 5.0, 0.0)), "{l:?}");
    // Still the plane after them.
    assert!(near(s.cplane().col(3), Vec3::new(10.0, 5.0, 0.0)));
    // A tilted plane still refuses them, the drawing unchanged.
    set(&mut s, json!({ "preset": "front" }));
    let before = s.state().unwrap().doc.clone();
    for id in ["offset", "hatch", "ncopy", "geomconstraint"] {
        assert!(s.start(id).is_err(), "{id}");
        assert!(s.execute(id, &json!({ "handle": h.hex(), "distance": 1, "side": [0, 0] })).is_err(), "{id}");
    }
    assert!(Arc::ptr_eq(&before, &s.state().unwrap().doc));
}

#[test]
fn xline_offset_and_tangent_circles_work_in_the_plane() {
    let mut s = Session::new();
    set(&mut s, json!({ "preset": "front" }));
    s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
    let a = last(&s).handle;
    s.execute("line", &json!({ "points": [[0, 0], [0, 10]] })).unwrap();
    let b = last(&s).handle;
    // XLINE Offset: parallel to the line, 2 toward the side picked, in the XZ plane.
    s.execute("xline", &json!({ "handle": a.hex(), "distance": 2, "side": [5, 5] })).unwrap();
    let EntityKind::XLine(r) = last(&s).kind else { panic!() };
    assert!(near(r.dir, Vec3::X) || near(r.dir, -Vec3::X), "{r:?}");
    assert!((r.base.z - 2.0).abs() < 1e-9 && r.base.y.abs() < 1e-9, "{r:?}");
    // A sloping line: its direction in the plane, not its plan shadow's.
    s.execute("line", &json!({ "points": [[0, 0], [10, 5]] })).unwrap();
    let slope = last(&s).handle;
    s.execute("xline", &json!({ "handle": slope.hex(), "distance": 2, "side": [0, 5] })).unwrap();
    let EntityKind::XLine(r) = last(&s).kind else { panic!() };
    let want = Vec3::new(2.0, 0.0, 1.0).normalized();
    assert!(near(r.dir, want) || near(r.dir, -want), "{r:?}");
    let off = r.base - r.dir * r.base.dot(r.dir);
    assert!((off.len() - 2.0).abs() < 1e-9 && r.base.y.abs() < 1e-9, "{r:?}");
    // CIRCLE TTR: tangent to both, standing in the plane.
    s.execute("circle.ttr", &json!({ "h1": a.hex(), "p1": [3, 0], "h2": b.hex(), "p2": [0, 3], "radius": 1 })).unwrap();
    let c = last(&s);
    let EntityKind::Circle(k) = &c.kind else { panic!("{c:?}") };
    assert!(near(world(&c, k.center.xy()), Vec3::new(1.0, 0.0, 1.0)) && (k.radius - 1.0).abs() < 1e-9, "{c:?}");
    // And CIRCLE's own Ttr option at the prompt.
    s.cmdline("circle t 3,0 0,3 1").unwrap();
    let c = last(&s);
    let EntityKind::Circle(k) = &c.kind else { panic!("{c:?}") };
    assert!(near(world(&c, k.center.xy()), Vec3::new(1.0, 0.0, 1.0)), "{c:?}");
}

#[test]
fn dimension_picks_and_grip_dimensions_see_the_plane() {
    let mut s = Session::new();
    turned(&mut s);
    s.execute("line", &json!({ "points": [[0, 0], [4, 0]] })).unwrap();
    let h = last(&s).handle;
    // DIMLINEAR "select object": the line's ends as the plane sees them, 4 along its X.
    s.execute("dimlinear", &json!({ "object": h.hex(), "at": [2, 2] })).unwrap();
    let EntityKind::Dimension(d) = last(&s).kind else { panic!() };
    assert!(near(d.p13, Vec3::new(10.0, 5.0, 0.0)) && near(d.p14, Vec3::new(10.0, 9.0, 0.0)), "{d:?}");
    // The same at the prompt: the object picked where the plane shows it.
    s.cmdline("dimlinear").unwrap();
    for t in ["", "2,0", "2,3"] {
        s.cmdline(t).unwrap();
    }
    let EntityKind::Dimension(d) = last(&s).kind else { panic!("{}", s.prompt_text()) };
    assert!(near(d.p13, Vec3::new(10.0, 5.0, 0.0)) && near(d.p14, Vec3::new(10.0, 9.0, 0.0)), "{d:?}");
    // Grip dimensions measure in the plane: from the other end, at the plane's origin.
    s.settings.dynmode = true;
    let end = s.grips_of(h).unwrap().iter().position(|g| g.near(Vec2::new(4.0, 0.0), 1e-9)).unwrap();
    s.start_grip_edit(&[(h, end)]).unwrap();
    let dims = crate::grip_dims::grip_dims(&s);
    assert!(dims.first().is_some_and(|g| g.anchor.near(Vec2::ZERO, 1e-9)), "{:?}", dims.first().map(|g| g.anchor));
    s.cancel();
}

#[test]
fn the_block_editor_shows_every_tile_in_world() {
    let mut s = Session::new();
    s.execute("line", &json!({ "points": [[0, 0], [4, 0]] })).unwrap();
    let l = last(&s).handle;
    s.execute("block", &json!({ "name": "B", "base": [0, 0], "handles": [l.hex()] })).unwrap();
    s.execute("viewports.rhino", &json!({})).unwrap();
    assert!(s.model_tiles().iter().any(|t| t.cplane != Mat4::IDENTITY));
    s.execute("bedit", &json!({ "name": "B" })).unwrap();
    assert_eq!(s.cplane(), Mat4::IDENTITY);
    assert!(s.model_tiles().iter().all(|t| t.cplane == Mat4::IDENTITY));
    assert!(s.execute("cplane", &json!({ "preset": "front" })).unwrap_err().to_string().contains("Block Editor"));
}

#[test]
fn a_single_view_saves_its_look_and_a_maximized_one_its_tiles() {
    use super::super::vports::with_live_tiles;
    let reopen = |d: &cadcraft_doc::Drawing| {
        let mut t = Session::new();
        t.open_drawing(cadcraft_io::read_dxf(cadcraft_io::write_dxf(d).as_bytes()).unwrap(), "back", None);
        t
    };
    // One view along a direction and in a style of its own.
    let mut s = Session::new();
    s.execute("line", &json!({ "points": [[0, 0], [20, 10]] })).unwrap();
    s.execute("view.swiso", &json!({})).unwrap();
    s.execute("vscurrent", &json!({ "style": "shaded" })).unwrap();
    let mut t = reopen(&with_live_tiles(&s).unwrap());
    let (a, b) = (s.state().unwrap(), t.state().unwrap());
    assert!(t.model_tiles().is_empty());
    assert!((a.view_dir.dir - b.view_dir.dir).len() < 1e-12 && b.shaded, "{:?}", b.view_dir);
    // Plan wireframe again: the file's own view is dropped, nothing else is written.
    t.execute("plan", &json!({})).unwrap();
    t.execute("vscurrent", &json!({ "style": "2dwireframe" })).unwrap();
    let d = with_live_tiles(&t).unwrap();
    assert!(!d.vports.iter().any(|c| c.is_active()));
    assert!(with_live_tiles(&Session::new()).is_none());
    // Maximized: the tiles it replaced are saved, itself in its place and current.
    let mut s = Session::new();
    s.execute("viewports.rhino", &json!({})).unwrap();
    s.activate_tile(1).unwrap();
    s.execute("vscurrent", &json!({ "style": "shaded" })).unwrap();
    s.execute("viewports.single", &json!({})).unwrap();
    assert!(s.model_tiles().is_empty());
    let t = reopen(&with_live_tiles(&s).unwrap());
    let tiles = t.model_tiles();
    assert_eq!(tiles.len(), 4);
    let cur = tiles.iter().find(|x| x.active).unwrap();
    assert!(cur.look.shaded && !cur.look.dir.is_plan() && cur.min == Vec2::new(0.5, 0.5), "{cur:?}");
}

#[test]
fn stretch_and_mirrored_text_rubber_bands_are_in_the_plane() {
    let mut s = Session::new();
    turned(&mut s);
    s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
    // STRETCH: a crossing window round the (10, 0) end, all in the plane's coordinates.
    s.cmdline("stretch").unwrap();
    for t in ["12,1", "8,-1", "", "0,0"] {
        s.cmdline(t).unwrap();
    }
    let pv = s.preview(Vec2::new(0.0, 2.0));
    assert!(
        pv.iter().any(|e| matches!(&e.kind, EntityKind::Line(k) if near(k.a, Vec3::ZERO) && near(k.b, Vec3::new(10.0, 2.0, 0.0)))),
        "{} {pv:?}",
        s.prompt_text()
    );
    s.cancel();
    // MIRROR keeps a text reading (MIRRTEXT 0): drawn one by one, in the plane's coordinates.
    s.execute("text", &json!({ "at": [2, 1], "text": "AB", "height": 1 })).unwrap();
    let t = last(&s).handle;
    s.execute("select", &json!({ "handles": [t.hex()] })).unwrap();
    s.cmdline("mirror").unwrap();
    s.cmdline("0,-5").unwrap();
    let (_, extra) = s.drag(Vec2::new(0.0, 5.0)).unwrap();
    let text = extra.iter().find_map(|e| if let EntityKind::Text(x) = &e.kind { Some(x.insert) } else { None }).unwrap();
    assert!((-6.0..0.0).contains(&text.x) && (0.0..=2.0).contains(&text.y), "{text:?}");
    s.cancel();
}

#[test]
fn constraints_pick_objects_where_the_plane_shows_them() {
    let mut s = Session::new();
    s.viewport_px = (1000.0, 1000.0);
    turned(&mut s);
    s.execute("line", &json!({ "points": [[0, 0], [8, 0.4]] })).unwrap();
    s.execute("line", &json!({ "points": [[0, 3], [8, 3.9]] })).unwrap();
    s.cmdline("geomconstraint pa").unwrap();
    s.input(crate::Input::Point(Vec2::new(4.0, 0.2))).unwrap();
    s.input(crate::Input::Point(Vec2::new(4.0, 3.45))).unwrap();
    assert!(s.running.is_none(), "{}", s.prompt_text());
    assert_eq!(s.doc().unwrap().constraints.len(), 1);
}

#[test]
fn every_constraint_command_runs_in_a_level_plane_and_is_refused_in_a_tilted_one() {
    use crate::cplane::{PLAN_ONLY, in_world};
    // Every GEOMCONSTRAINT / DIMCONSTRAINT command the Parametric menu runs picks objects in plan.
    let picks = |id: &str| (id.starts_with("gc") || id.starts_with("dc")) && !id.starts_with("dcdisplay") || id.ends_with("constraint");
    let ids: Vec<&str> = crate::command_specs().iter().map(|c| c.id).filter(|id| picks(id) && *id != "delconstraint").collect();
    assert!(ids.len() >= 20, "{ids:?}");
    let mut s = Session::new();
    turned(&mut s);
    for id in &ids {
        assert!(PLAN_ONLY.contains(id) && in_world(&s, id), "{id}");
    }
    // GCPARALLEL picks the lines where the plane shows them.
    s.viewport_px = (1000.0, 1000.0);
    s.execute("line", &json!({ "points": [[0, 0], [8, 0.4]] })).unwrap();
    s.execute("line", &json!({ "points": [[0, 3], [8, 3.9]] })).unwrap();
    s.cmdline("gcparallel").unwrap();
    s.input(crate::Input::Point(Vec2::new(4.0, 0.2))).unwrap();
    s.input(crate::Input::Point(Vec2::new(4.0, 3.45))).unwrap();
    assert!(s.running.is_none(), "{}", s.prompt_text());
    assert_eq!(s.doc().unwrap().constraints.len(), 1);
    // In Front: refused at the prompt and as JSON, the drawing unchanged.
    set(&mut s, json!({ "preset": "front" }));
    let before = s.state().unwrap().doc.clone();
    for id in &ids {
        assert!(s.start(id).is_err(), "{id}");
        assert!(s.execute(id, &json!({ "h1": "1", "h2": "2" })).is_err(), "{id}");
    }
    assert!(Arc::ptr_eq(&before, &s.state().unwrap().doc));
}

#[test]
fn edits_storing_picked_points_run_in_world_in_a_level_plane() {
    let mut s = Session::new();
    turned(&mut s);
    for id in ["copybase", "dimtedit", "dimjogline", "mledit", "mleaderedit", "mleaderedit.add", "mleaderedit.remove", "mleaderalign", "zoom", "pan"]
    {
        assert!(crate::cplane::in_world(&s, id), "{id}");
    }
    // View commands keep working in a tilted plane.
    set(&mut s, json!({ "preset": "front" }));
    assert!(s.start("zoom").is_ok());
    s.cancel();
    assert!(s.start("dimtedit").is_err());
}

#[test]
fn undo_through_a_maximized_view_saves_what_is_shown() {
    use super::super::vports::with_live_tiles;
    let mut s = Session::new();
    s.execute("line", &json!({ "points": [[0, 0], [20, 10]] })).unwrap();
    s.execute("viewports.rhino", &json!({})).unwrap();
    s.execute("viewports.single", &json!({})).unwrap();
    assert!(s.state().unwrap().tiles.maximized.is_some());
    // U, U: back to the one plan view the drawing had; saving writes no tiles.
    s.undo().unwrap();
    s.undo().unwrap();
    assert!(s.model_tiles().is_empty());
    assert!(s.state().unwrap().tiles.maximized.is_none());
    let tiled = with_live_tiles(&s).is_some_and(|d| d.vports.iter().any(|c| c.is_active() && c.tiles.len() > 1));
    assert!(!tiled);
    // Redo, redo: the Rhino tiles then one view again, still saved as shown.
    s.redo().unwrap();
    assert_eq!(s.model_tiles().len(), 4);
    s.redo().unwrap();
    assert!(s.model_tiles().is_empty());
    let tiled = with_live_tiles(&s).is_some_and(|d| d.vports.iter().any(|c| c.is_active() && c.tiles.len() > 1));
    assert!(!tiled);
}

#[test]
fn a_dimension_in_a_moved_plane_attaches_to_the_objects_it_was_snapped_to() {
    let mut s = Session::new();
    set(&mut s, json!({ "origin": [100, 0, 0] }));
    // Something at the world origin that a world lookup would wrongly find.
    s.cmdline("ucs w").unwrap();
    s.execute("line", &json!({ "points": [[0, 0], [50, 0]] })).unwrap();
    let decoy = last(&s).handle;
    set(&mut s, json!({ "origin": [100, 0, 0] }));
    s.execute("line", &json!({ "points": [[0, 0], [50, 0]] })).unwrap();
    let h = last(&s).handle;
    s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [50, 0], "at": [25, 10] })).unwrap();
    let EntityKind::Dimension(d) = last(&s).kind else { panic!() };
    assert!(d.assoc.len() == 2 && d.assoc.iter().all(|a| a.handle == h), "{:?} (decoy {decoy:?})", d.assoc);
    // Editing the line drags the dimension with it.
    s.execute("move", &json!({ "handles": [h.hex()], "from": [0, 0], "to": [0, 5] })).unwrap();
    let EntityKind::Dimension(d) = last(&s).kind else { panic!() };
    assert!(near(d.p13, Vec3::new(100.0, 5.0, 0.0)) && near(d.p14, Vec3::new(150.0, 5.0, 0.0)), "{d:?}");
}
