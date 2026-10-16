//! An AutoCAD 2D session under a level UCS (moved to 100,50; turned by `UCS Z 90`; both) gives
//! the world results the same session gives in World with its points converted: LINE, CIRCLE,
//! OFFSET, TRIM, FILLET, associative DIMALIGNED, DIMRADIUS, DIMTEDIT, COPYBASE + PASTECLIP and a
//! dynamic block's stretch grip. DIMORDINATE measures from the UCS origin along its axes, and
//! picks and windows behave as they do in AutoCAD there.

use cadcraft_engine::doc::dynblock::GripPart;
use cadcraft_engine::doc::{DimStyle, Entity, EntityKind, Handle};
use cadcraft_engine::geom::{Mat4, Vec2, Vec3};
use cadcraft_engine::{Input, Session};
use serde_json::{Value, json};

/// DXF text from lines of `code value`.
fn dxf(src: &str) -> String {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (code, value) = l.split_once(' ').unwrap_or((l, ""));
            format!("{code}\n{value}\n")
        })
        .collect()
}

/// A session on the DOOR dynamic block drawing (as in `dynamic_grips.rs`: Width's end grip 9),
/// a reference to it inserted at world (100, 0) in World, and a 200-unit plan view.
fn door_session() -> (Session, Handle) {
    let grip = "0 BLOCKLINEARGRIP\n5 68\n330 61\n100 AcDbEvalExpr\n90 9\n98 33\n99 232\n100 AcDbBlockElement\n300 End Grip\n98 33\n99 232\n1071 0\n";
    let src = include_str!("../../io/tests/data/dynblock_door.txt")
        .replace("\r\n", "\n")
        .replace("170 4\n91 0\n91 0\n", "170 4\n91 8\n91 9\n")
        .replace("1030 0\n93 0\n170 0\n171 0\n100 AcDbBlockVisibilityParameter", "1030 0\n93 10\n170 0\n171 0\n100 AcDbBlockVisibilityParameter")
        .replace("91 5\n93 32\n95 6\n360 67", "91 5\n93 32\n95 6\n360 67\n92 -1\n92 -1\n92 -1\n92 -1\n91 6\n93 32\n95 9\n360 68")
        .replace("95 1\n333 63\n", "95 2\n333 63\n333 68\n")
        .replace("0 BLOCKFLIPGRIP", &format!("{grip}0 BLOCKFLIPGRIP"));
    let d = cadcraft_io::read_dxf(dxf(&src).as_bytes()).unwrap();
    let mut s = Session::empty();
    s.open_drawing(d, "door", None);
    s.viewport_px = (1000.0, 1000.0);
    s.execute("view.set", &json!({ "center": [60, 20], "height": 200 })).unwrap();
    let r = s.execute("insert", &json!({ "name": "Door", "at": [100, 0] })).unwrap();
    (s, Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap())
}

/// The UCS coordinates of world point (x, y): what is typed, and where the cursor is.
fn ucs(s: &Session, x: f64, y: f64) -> Vec2 {
    s.cplane().inverse().unwrap().apply(Vec3::new(x, y, 0.0)).xy()
}

/// World point (x, y) typed in the current UCS.
fn at(s: &Session, x: f64, y: f64) -> String {
    let p = ucs(s, x, y);
    format!("{},{}", p.x, p.y)
}

fn near(a: Vec3, b: Vec3) -> bool {
    (a - b).len() < 1e-9
}

fn last(s: &Session) -> Handle {
    s.doc().unwrap().model.last().unwrap().handle
}

fn kind(s: &Session, h: Handle) -> EntityKind {
    s.doc().unwrap().entity(h).unwrap().kind.clone()
}

/// Type the lines `lines` makes (from the session as it is) at the command line, each a whole
/// line.
fn typed(s: &mut Session, lines: impl FnOnce(&Session) -> Vec<String>) {
    for l in lines(s) {
        s.cmdline(&l).unwrap_or_else(|e| panic!("{l}: {e} at {}", s.prompt_text()));
    }
}

/// The 2D session, with every point typed in the UCS `setup` sets up (as typed at the command
/// line). Returns the drawing's model space.
fn session(setup: &[&str]) -> Vec<Entity> {
    let (mut s, door) = door_session();
    for t in setup {
        s.cmdline(t).unwrap();
    }
    assert!(s.running.is_none(), "{}", s.prompt_text());
    // Lines: L1 along x, L2 standing on its end, L3 across L1 at x = 20; a circle.
    let mut lines = Vec::new();
    for (a, b) in [((0.0, 0.0), (80.0, 0.0)), ((80.0, 0.0), (80.0, 50.0)), ((20.0, -10.0), (20.0, 30.0))] {
        typed(&mut s, |s| vec![format!("line {} {}", at(s, a.0, a.1), at(s, b.0, b.1)), String::new()]);
        lines.push(last(&s));
    }
    let l2 = lines[1];
    typed(&mut s, |s| vec![format!("circle {} 8", at(s, 50.0, 25.0))]);
    // OFFSET L1 by 5 toward +y; TRIM it left of L3 (Quick); FILLET it with L2 at radius 5.
    typed(&mut s, |s| vec!["offset 5".into(), at(s, 40.0, 0.0), at(s, 40.0, 10.0), String::new()]);
    typed(&mut s, |s| vec!["trim".into(), at(s, 10.0, 0.0), String::new()]);
    typed(&mut s, |s| vec!["fillet r 5".into(), at(s, 70.0, 0.0), at(s, 80.0, 10.0)]);
    assert!(s.running.is_none(), "{}", s.prompt_text());
    // An associative aligned dimension on L1's ends, a radius one on the circle.
    typed(&mut s, |s| vec!["dimaligned".into(), at(s, 20.0, 0.0), at(s, 75.0, 0.0), at(s, 50.0, -10.0)]);
    let aligned = last(&s);
    let EntityKind::Dimension(d) = kind(&s, aligned) else { panic!("{}", s.prompt_text()) };
    assert_eq!(d.assoc.len(), 2, "attached to L1's ends: {d:?}");
    typed(&mut s, |s| vec!["dimradius".into(), at(s, 58.0, 25.0), at(s, 65.0, 32.0)]);
    assert!(matches!(kind(&s, last(&s)), EntityKind::Dimension(_)), "{}", s.prompt_text());
    // DIMTEDIT moves the aligned dimension's text.
    s.set_selection(vec![aligned]);
    typed(&mut s, |s| vec!["dimtedit".into(), at(s, 40.0, -15.0)]);
    // COPYBASE L2 from its top end, PASTECLIP that end at (120, 50).
    s.set_selection(vec![l2]);
    typed(&mut s, |s| vec!["copybase".into(), at(s, 80.0, 50.0), "pasteclip".into(), at(s, 120.0, 50.0)]);
    // The door's Width grip: the rubber band runs from the grip to the cursor in the UCS, and
    // dropping it at (132.2, 3) makes the door 32 wide.
    s.start_dyn_grip(door, 2, GripPart::End).unwrap();
    assert!(s.current_prompt().unwrap().base.unwrap().near(ucs(&s, 136.0, -4.0), 1e-9));
    let pv = s.preview(ucs(&s, 132.0, 0.0));
    let band = pv
        .iter()
        .any(|e| matches!(&e.kind, EntityKind::Line(l) if l.a.xy().near(ucs(&s, 136.0, -4.0), 1e-9) && l.b.xy().near(ucs(&s, 132.0, 0.0), 1e-9)));
    assert!(pv.len() > 1 && band, "{pv:?}");
    typed(&mut s, |s| vec![at(s, 132.2, 3.0)]);
    assert!(s.running.is_none(), "{}", s.prompt_text());
    let r = s.execute("blocks.reference", &json!({ "handle": door.hex() })).unwrap();
    let width = r["properties"].as_array().unwrap().iter().find(|p| p["name"] == "Width").unwrap()["value"].clone();
    assert_eq!(width, json!("32.0000"));
    s.doc().unwrap().model.iter().map(|e| (**e).clone()).collect()
}

/// Whether two JSON values agree, numbers to 1e-6.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => (x.as_f64().unwrap() - y.as_f64().unwrap()).abs() < 1e-6,
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

/// An object as JSON without the definition points an aligned dimension doesn't use (they are
/// placed with it, from the plane's origin).
fn unused_out(mut v: Value) -> Value {
    if let Some(k) = v.get_mut("kind").and_then(Value::as_object_mut).filter(|k| k.get("kind") == Some(&json!("aligned"))) {
        k.remove("p15");
        k.remove("p16");
    }
    v
}

#[test]
fn a_2d_session_in_a_level_ucs_gives_the_world_results() {
    let world = session(&[]);
    // What World gives, in world coordinates.
    let line = |a: (f64, f64), b: (f64, f64)| {
        let (a, b) = (Vec3::new(a.0, a.1, 0.0), Vec3::new(b.0, b.1, 0.0));
        world.iter().any(|e| matches!(&e.kind, EntityKind::Line(l) if near(l.a, a) && near(l.b, b) || near(l.a, b) && near(l.b, a)))
    };
    assert!(line((20.0, 0.0), (75.0, 0.0)), "L1 trimmed and filleted");
    assert!(line((80.0, 5.0), (80.0, 50.0)), "L2 filleted");
    assert!(line((0.0, 5.0), (80.0, 5.0)), "the offset");
    assert!(line((120.0, 5.0), (120.0, 50.0)), "the pasted copy");
    let text = world.iter().find_map(|e| match &e.kind {
        EntityKind::Dimension(d) if d.user_text_pos => Some(d.text_mid),
        _ => None,
    });
    assert!(text.is_some_and(|t| t.xy().near(Vec2::new(40.0, -15.0), 1e-9)), "{text:?}");
    for setup in [&["ucs 100,50", ""][..], &["ucs z 90"], &["ucs 100,50", "", "ucs z 90"]] {
        let got = session(setup);
        assert_eq!(got.len(), world.len(), "{setup:?}");
        for (g, w) in got.iter().zip(&world) {
            let (g, w) = (unused_out(serde_json::to_value(g).unwrap()), unused_out(serde_json::to_value(w).unwrap()));
            assert!(same(&g, &w), "{setup:?}:\n{g}\n{w}");
        }
    }
}

fn measured(s: &Session) -> f64 {
    let EntityKind::Dimension(d) = kind(s, last(s)) else { panic!("{}", s.prompt_text()) };
    cadcraft_engine::render::dimension_geometry(&d, &DimStyle::default(), 1.0).measurement
}

#[test]
fn dimordinate_measures_from_the_ucs_origin_along_its_axes() {
    for setup in [&["ucs 100,50", ""][..], &["ucs z 90"], &["ucs 100,50", "", "ucs z 90"]] {
        let mut s = Session::new();
        for t in setup {
            s.cmdline(t).unwrap();
        }
        // The feature at UCS (20, 30): its leader along the UCS's y gives the X datum, 20.
        s.cmdline("dimordinate 20,30 20,45").unwrap();
        assert!((measured(&s) - 20.0).abs() < 1e-9, "{setup:?}: {}", measured(&s));
        let EntityKind::Dimension(d) = kind(&s, last(&s)) else { panic!() };
        assert!(near(d.defpt, s.cplane().col(3)), "{setup:?}: the datum is the UCS origin");
        // Along the UCS's x: the Y datum, 30; the Xdatum keyword forces the X datum.
        s.cmdline("dimordinate 20,30 35,31").unwrap();
        assert!((measured(&s) - 30.0).abs() < 1e-9, "{setup:?}: {}", measured(&s));
        s.cmdline("dimordinate 20,30 x 35,31").unwrap();
        assert!((measured(&s) - 20.0).abs() < 1e-9, "{setup:?}: {}", measured(&s));
        // The JSON form takes world points (DIMORDINATE runs in World), measured the same way.
        let w = |p: Vec2| s.cplane().apply(p.to3(0.0));
        let (f, l) = (w(Vec2::new(20.0, 30.0)), w(Vec2::new(20.0, 45.0)));
        s.execute("dimordinate", &json!({ "feature": [f.x, f.y], "leader": [l.x, l.y] })).unwrap();
        assert!((measured(&s) - 20.0).abs() < 1e-9, "{setup:?}: {}", measured(&s));
    }
    // In World, as before: from the world origin.
    let mut s = Session::new();
    s.cmdline("dimordinate 20,30 20,45").unwrap();
    assert!((measured(&s) - 20.0).abs() < 1e-9);
    // A UCS turned by 30° can't be measured along: refused, the drawing unchanged.
    s.cmdline("ucs z 30").unwrap();
    let n = s.doc().unwrap().model.len();
    assert!(s.cmdline("dimordinate 20,30 20,45").is_err() || s.doc().unwrap().model.len() == n);
    assert!(s.execute("dimordinate", &json!({ "feature": [20, 30], "leader": [20, 45] })).is_err());
    assert_eq!(s.doc().unwrap().model.len(), n);
}

#[test]
fn ucs_turns_about_its_own_axes() {
    let mut s = Session::new();
    s.cmdline("ucs 100,50").unwrap();
    s.cmdline("").unwrap();
    s.cmdline("ucs z 90").unwrap();
    let c = s.cplane();
    assert!(near(c.col(3), Vec3::new(100.0, 50.0, 0.0)) && near(c.col(0), Vec3::Y) && near(c.col(1), -Vec3::X), "{c:?}");
    // Enter takes 90°; X turns about the plane's own x, standing it up.
    s.cmdline("ucs z").unwrap();
    s.cmdline("").unwrap();
    assert!(near(s.cplane().col(0), -Vec3::X));
    s.cmdline("ucs x 90").unwrap();
    assert!(near(s.cplane().col(2), Vec3::Y), "{:?}", s.cplane());
    // A bad angle changes nothing.
    let before = s.cplane();
    assert!(s.cmdline("ucs z nonsense").is_err() || s.running.is_some());
    s.cancel();
    assert_eq!(s.cplane(), before);
    s.cmdline("ucs w").unwrap();
    assert_eq!(s.cplane(), Mat4::IDENTITY);
}

#[test]
fn selection_windows_are_the_screens_in_a_turned_ucs() {
    let mut s = Session::new();
    s.viewport_px = (1000.0, 1000.0);
    s.execute("view.set", &json!({ "center": [0, 0], "height": 40 })).unwrap();
    let line = s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
    let line = Handle::parse_hex(line["handles"][0].as_str().unwrap()).unwrap();
    let dot = s.execute("point", &json!({ "at": [5, 3] })).unwrap();
    let dot = Handle::parse_hex(dot["handle"].as_str().unwrap_or_else(|| dot["handles"][0].as_str().unwrap())).unwrap();
    // UCS Z 90: drawn right to left on screen (world x falling) is a crossing window.
    s.cmdline("ucs z 90").unwrap();
    s.idle_click(ucs(&s, 12.0, -2.0), false).unwrap();
    s.idle_click(ucs(&s, 5.0, 2.0), false).unwrap();
    assert_eq!(s.selection(), vec![line]);
    // And left to right a window: the line isn't inside.
    s.set_selection(Vec::new());
    s.idle_click(ucs(&s, 5.0, -2.0), false).unwrap();
    s.idle_click(ucs(&s, 12.0, 2.0), false).unwrap();
    assert!(s.selection().is_empty());
    // UCS Z 45: a typed Window is the screen's box, not one along the UCS's axes.
    s.cmdline("ucs w").unwrap();
    s.cmdline("ucs z 45").unwrap();
    typed(&mut s, |s| vec!["erase".into(), "w".into(), at(s, -1.0, -1.0), at(s, 11.0, 1.0), String::new()]);
    assert!(s.doc().unwrap().entity(line).is_none() && s.doc().unwrap().entity(dot).is_some());
}

#[test]
fn edits_storing_picked_points_put_them_where_they_were_picked() {
    let mut s = Session::new();
    s.cmdline("ucs 100,50").unwrap();
    s.cmdline("").unwrap();
    let w = |x: f64, y: f64| Vec2::new(x + 100.0, y + 50.0);
    // DIMJOGLINE: the jog where it was picked.
    s.execute("dimaligned", &json!({ "p1": [0, 0], "p2": [40, 0], "at": [20, 10] })).unwrap();
    let dim = last(&s);
    s.start("dimjogline").unwrap();
    s.input(Input::Pick(vec![dim])).unwrap();
    s.cmdline("10,10").unwrap();
    let EntityKind::Dimension(d) = kind(&s, dim) else { panic!() };
    assert!(d.marks.jog_line.is_some_and(|j| j.xy().near(w(10.0, 10.0), 1e-9)), "{:?}", d.marks.jog_line);
    // MLEADEREDIT Add: the new arrowhead where it was picked.
    let r = s.execute("mleader", &json!({ "points": [[0, 20], [5, 25]], "text": "N" })).unwrap();
    let ml = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
    s.start("mleaderedit.add").unwrap();
    s.input(Input::Pick(vec![ml])).unwrap();
    s.cmdline("-3,22").unwrap();
    s.input(Input::Enter).unwrap();
    let EntityKind::MLeader(m) = kind(&s, ml) else { panic!() };
    assert!(m.leaders.iter().any(|l| l.first().is_some_and(|p| p.xy().near(w(-3.0, 22.0), 1e-9))), "{:?}", m.leaders);
    // BASE: $INSBASE is the world point.
    s.execute("base", &json!({ "at": [1, 2] })).unwrap();
    assert!(s.doc().unwrap().header.point("INSBASE").is_some_and(|p| near(p, Vec3::new(101.0, 52.0, 0.0))));
    // ZOOM Window: the view goes where the window was drawn.
    s.cmdline("zoom w 0,0 10,10").unwrap();
    assert!(s.state().unwrap().view().center.near(w(5.0, 5.0), 1e-9), "{:?}", s.state().unwrap().view());
    // TABLEDIT picks the cell under the point as the UCS shows the table.
    let t = s.execute("table", &json!({ "at": [0, -40], "cells": [["a", "b"]], "rowHeight": 1.0, "colWidth": 3.0 })).unwrap();
    let t = Handle::parse_hex(t["handle"].as_str().unwrap()).unwrap();
    s.execute("tabledit", &json!({ "at": [4, -40.5], "text": "hello" })).unwrap();
    let EntityKind::Table(tb) = kind(&s, t) else { panic!() };
    assert_eq!(tb.cells[0][1].text, "hello");
    // COPYBASE then PASTEBLOCK: the block's base is the base point, inserted where picked.
    let l = s.execute("line", &json!({ "points": [[0, 60], [10, 60]] })).unwrap();
    s.set_selection(vec![Handle::parse_hex(l["handles"][0].as_str().unwrap()).unwrap()]);
    s.cmdline("copybase 0,60").unwrap();
    s.cmdline("pasteblock 20,60").unwrap();
    let EntityKind::Insert(ins) = kind(&s, last(&s)) else { panic!("{}", s.prompt_text()) };
    let base = s.doc().unwrap().block(&ins.block).unwrap().base;
    assert!(ins.insert.xy().near(w(20.0, 60.0), 1e-9) && near(base, Vec3::new(100.0, 110.0, 0.0)), "{ins:?} {base:?}");
}
