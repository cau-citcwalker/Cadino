//! Commands given points of a moved or turned UCS take them as such: LIMITS stores the world box
//! through its corners, TABLEDIT finds the cell under the point, REVCLOUD Object makes the cloud
//! where the object is, the JSON `select {window}` takes the UCS rectangle, and DIMORDINATE
//! measures along the axes of a UCS turned by any angle (kept through DXF). Redoing
//! VIEWPORTS.SINGLE remembers the tiles it replaced.

use cadcraft_engine::doc::{DimStyle, Dimension, EntityKind, Handle};
use cadcraft_engine::geom::{Vec2, Vec3};
use cadcraft_engine::render::dimension_geometry;
use cadcraft_engine::{ModelTile, Session};
use serde_json::{Value, json};

fn handle(v: &Value) -> Handle {
    Handle::parse_hex(v["handle"].as_str().or_else(|| v["handles"][0].as_str()).unwrap()).unwrap()
}

fn kind(s: &Session, h: Handle) -> EntityKind {
    s.doc().unwrap().entity(h).unwrap().kind.clone()
}

fn last(s: &Session) -> Handle {
    s.doc().unwrap().model.last().unwrap().handle
}

/// The UCS coordinates of world point (x, y).
fn ucs(s: &Session, x: f64, y: f64) -> Vec2 {
    s.cplane().inverse().unwrap().apply(Vec3::new(x, y, 0.0)).xy()
}

/// A session in the UCS that `setup` (lines typed at the command line) sets up.
fn session(setup: &[&str]) -> Session {
    let mut s = Session::new();
    s.viewport_px = (1000.0, 1000.0);
    s.execute("view.set", &json!({ "center": [100, 60], "height": 200 })).unwrap();
    for t in setup {
        s.cmdline(t).unwrap();
    }
    assert!(s.running.is_none(), "{}", s.prompt_text());
    s
}

const TURNED: &[&str] = &["ucs 100,50", "", "ucs z 45"];

#[test]
fn limits_stores_the_world_box_through_the_ucs_corners() {
    let corner = |s: &Session, n: &str| s.doc().unwrap().header.point(n).unwrap();
    let mut s = session(&[]);
    s.execute("limits", &json!({ "min": [0, 0], "max": [12, 9] })).unwrap();
    assert_eq!((corner(&s, "LIMMIN"), corner(&s, "LIMMAX")), (Vec3::ZERO, Vec3::new(12.0, 9.0, 0.0)));
    // UCS (0, 0) is world (100, 50) and UCS (10, 20) world (80, 60).
    let mut s = session(&["ucs 100,50", "", "ucs z 90"]);
    s.execute("limits", &json!({ "min": [0, 0], "max": [10, 20] })).unwrap();
    let (lo, hi) = (corner(&s, "LIMMIN"), corner(&s, "LIMMAX"));
    assert!((lo - Vec3::new(80.0, 50.0, 0.0)).len() < 1e-9 && (hi - Vec3::new(100.0, 60.0, 0.0)).len() < 1e-9, "{lo:?} {hi:?}");
    // A tilted plane (the Front tile's) would flatten the box: the corners are taken as given.
    let mut s = session(&[]);
    s.execute("cplane", &json!({ "preset": "front" })).unwrap();
    s.execute("limits", &json!({ "min": [0, 0], "max": [420, 297] })).unwrap();
    assert_eq!((corner(&s, "LIMMIN"), corner(&s, "LIMMAX")), (Vec3::ZERO, Vec3::new(420.0, 297.0, 0.0)));
}

#[test]
fn tabledit_picks_the_cell_under_a_point_of_a_turned_ucs() {
    let mut s = session(&[]);
    // A1 spans world x 0..3, B1 x 3..6, both y -1..0; tables stay square to world X.
    let t = handle(&s.execute("table", &json!({ "at": [0, 0], "cells": [["a", "b"]], "rowHeight": 1.0, "colWidth": 3.0 })).unwrap());
    let cell = |s: &Session, c: usize| match kind(s, t) {
        EntityKind::Table(tb) => tb.cells[0][c].text.clone(),
        k => panic!("{k:?}"),
    };
    for setup in [TURNED, &["ucs z 45"]] {
        for line in setup {
            s.cmdline(line).unwrap();
        }
        let p = ucs(&s, 4.0, -0.5);
        s.execute("tabledit", &json!({ "at": [p.x, p.y], "text": "json" })).unwrap();
        assert_eq!(cell(&s, 1), "json", "{setup:?}");
        let p = ucs(&s, 1.0, -0.5);
        s.cmdline("tabledit").unwrap();
        s.cmdline(&format!("{},{}", p.x, p.y)).unwrap();
        s.cmdline("typed").unwrap();
        assert_eq!(cell(&s, 0), "typed", "{setup:?}: {}", s.prompt_text());
        s.cmdline("ucs w").unwrap();
    }
}

#[test]
fn revcloud_object_makes_the_cloud_where_the_object_is() {
    for id in ["revcloud", "revcloud.polygonal", "revcloud.freehand"] {
        // A raised, turned UCS: circles at UCS (10, 10) and (30, 10), radius 5, height 10.
        let mut s = session(&["ucs 100,50,10", "", "ucs z 45"]);
        let check = |s: &Session, circle: Handle, at: Vec2| {
            assert!(s.doc().unwrap().entity(circle).is_none(), "{id}: the circle is replaced");
            let w = s.cplane().apply(at.to3(0.0));
            let EntityKind::LwPolyline(p) = kind(s, last(s)) else { panic!("{id}: {}", s.prompt_text()) };
            assert!((p.elevation - 10.0).abs() < 1e-9, "{id}: at the circle's height, not {}", p.elevation);
            assert!(p.vertices.len() > 3 && p.vertices.iter().all(|v| (v.p.dist(w.xy()) - 5.0).abs() < 0.01), "{id}: on the circle");
        };
        let c = handle(&s.execute("circle", &json!({ "center": [10, 10], "radius": 5 })).unwrap());
        s.start(id).unwrap();
        s.cmdline("o").unwrap();
        s.cmdline("15,10").unwrap();
        assert!(s.running.is_none(), "{id}: {}", s.prompt_text());
        check(&s, c, Vec2::new(10.0, 10.0));
        let c = handle(&s.execute("circle", &json!({ "center": [30, 10], "radius": 5 })).unwrap());
        s.execute(id, &json!({ "handle": c.hex() })).unwrap();
        check(&s, c, Vec2::new(30.0, 10.0));
    }
}

#[test]
fn a_json_window_in_a_turned_ucs_is_the_ucs_rectangle() {
    let mut s = session(TURNED);
    let line = handle(&s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap());
    // UCS (5, 3) is outside the window's UCS rectangle but inside the world box through its corners.
    s.execute("point", &json!({ "at": [5, 3] })).unwrap();
    s.execute("select", &json!({ "window": [[-1, -1], [11, 1]] })).unwrap();
    assert_eq!(s.selection(), vec![line]);
    s.execute("select", &json!({ "window": [[4, -1], [6, 4]], "crossing": true })).unwrap();
    assert_eq!(s.selection().len(), 2);
    s.execute("select", &json!({ "window": [[4, -1], [6, 1]], "crossing": true })).unwrap();
    assert_eq!(s.selection(), vec![line]);
}

fn dims(s: &Session) -> Vec<Dimension> {
    s.doc().unwrap().model.iter().filter_map(|e| if let EntityKind::Dimension(d) = &e.kind { Some(d.clone()) } else { None }).collect()
}

#[test]
fn dimordinate_in_a_turned_ucs_measures_along_its_axes_and_round_trips() {
    let mut s = session(&["ucs 100,50", "", "ucs z 30"]);
    // The feature at UCS (20, 30): the leader along the UCS y gives the X datum 20, along its x
    // the Y datum 30.
    s.cmdline("dimordinate 20,30 20,45").unwrap();
    s.cmdline("dimordinate 20,30 35,31").unwrap();
    let check = |ds: &[Dimension], why: &str| {
        let [x, y] = ds else { panic!("{why}: {ds:?}") };
        for (d, want, text) in [(x, 20.0, -60.0), (y, 30.0, 30.0)] {
            assert!(d.marks.horizontal.is_some_and(|h| (h - 30f64.to_radians()).abs() < 1e-9), "{why}: {d:?}");
            let g = dimension_geometry(d, &DimStyle::default(), 1.0);
            assert!((g.measurement - want).abs() < 1e-9, "{why}: {}", g.measurement);
            // The text runs along the leader, square to the UCS.
            assert!((g.text_angle - f64::to_radians(text)).abs() < 1e-9, "{why}: {}", g.text_angle.to_degrees());
        }
    };
    check(&dims(&s), "drawn");
    // DXF: group 51 is the negative of the UCS's angle; read back, the dimensions measure the same.
    let text = cadcraft_io::write_dxf(s.doc().unwrap());
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let g51 = lines.chunks(2).filter(|t| t[0] == "51").filter_map(|t| t.get(1)?.parse::<f64>().ok()).filter(|h| *h != 0.0).collect::<Vec<_>>();
    assert_eq!(g51.iter().filter(|h| (*h + 30.0).abs() < 1e-9).count(), 2, "{g51:?}");
    let back = cadcraft_io::read_dxf(text.as_bytes()).unwrap();
    let ds: Vec<Dimension> = back.model.iter().filter_map(|e| if let EntityKind::Dimension(d) = &e.kind { Some(d.clone()) } else { None }).collect();
    check(&ds, "read back");
    // ROTATE turns the axes with the dimension: the values stay.
    s.cmdline("ucs w").unwrap();
    let all = s.doc().unwrap().model.iter().map(|e| e.handle.hex()).collect::<Vec<_>>();
    s.execute("rotate", &json!({ "handles": all, "base": [0, 0], "angle": 40 })).unwrap();
    for (d, want) in dims(&s).iter().zip([20.0, 30.0]) {
        assert!((dimension_geometry(d, &DimStyle::default(), 1.0).measurement - want).abs() < 1e-9);
    }
}

fn tiles(s: &Session) -> Vec<ModelTile> {
    s.model_tiles()
}

#[test]
fn redoing_viewports_single_remembers_the_tiles_it_replaced() {
    let mut s = session(&[]);
    s.execute("line", &json!({ "points": [[0, 0], [20, 10]] })).unwrap();
    s.execute("-vports", &json!({ "option": "3" })).unwrap();
    let three = tiles(&s);
    assert_eq!(three.len(), 3);
    s.execute("viewports.single", &json!({})).unwrap();
    let after_single = s.state().unwrap().tiles.maximized.clone();
    assert!(after_single.is_some());
    s.undo().unwrap();
    assert_eq!(tiles(&s).len(), 3);
    s.redo().unwrap();
    assert!(tiles(&s).is_empty());
    assert_eq!(s.state().unwrap().tiles.maximized, after_single, "as VIEWPORTS.SINGLE left it");
    // MAXIMIZE brings back the user's three tiles, not Rhino's four.
    s.execute("viewports.maximize", &json!({})).unwrap();
    let back = tiles(&s);
    assert_eq!(back.len(), 3);
    for t in &three {
        assert!(back.iter().any(|b| b.min == t.min && b.max == t.max), "{t:?}");
    }
}
