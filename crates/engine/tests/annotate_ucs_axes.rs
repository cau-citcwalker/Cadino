//! Annotations keep the axes they were made along: a table made in a turned UCS runs along its X
//! axis (drawn, picked by TABLEDIT, gripped, turned by ROTATE and kept through DXF group 11), an
//! ordinate dimension made in World keeps measuring along world X when it is rotated, and QDIM's
//! Ordinate mode measures along the UCS's axes as DIMORDINATE does.

use cadcraft_engine::Session;
use cadcraft_engine::doc::{DimKind, DimStyle, Dimension, EntityKind, Handle, Table};
use cadcraft_engine::geom::{Vec2, Vec3};
use cadcraft_engine::render::{dimension_geometry, table_geometry};
use serde_json::{Value, json};

fn handle(v: &Value) -> Handle {
    Handle::parse_hex(v["handle"].as_str().or_else(|| v["handles"][0].as_str()).unwrap()).unwrap()
}

fn table(s: &Session, h: Handle) -> Table {
    match &s.doc().unwrap().entity(h).unwrap().kind {
        EntityKind::Table(t) => t.clone(),
        k => panic!("not a table: {k:?}"),
    }
}

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

/// The world point of UCS point (x, y).
fn world(s: &Session, x: f64, y: f64) -> Vec2 {
    s.cplane().apply(Vec3::new(x, y, 0.0)).xy()
}

fn near(a: Vec2, b: Vec2) -> bool {
    a.dist(b) < 1e-9
}

#[test]
fn a_table_made_in_a_turned_ucs_runs_along_its_x_axis() {
    let mut s = session(&["ucs 100,50", "", "ucs z 30"]);
    let h = handle(&s.execute("table", &json!({ "at": [0, 0], "cells": [["a", "b"], ["c", "d"]], "rowHeight": 1.0, "colWidth": 3.0 })).unwrap());
    let t = table(&s, h);
    let u = Vec2::from_angle(30f64.to_radians());
    assert!(near(t.direction.xy(), u), "{:?}", t.direction);
    assert!(near(t.insert.xy(), world(&s, 0.0, 0.0)));
    // Drawn along the UCS: its far corner is UCS (6, -2), its border lines run along the UCS axes.
    assert!(near(t.to_world(Vec2::new(6.0, -2.0)), world(&s, 6.0, -2.0)));
    let g = table_geometry(s.doc().unwrap(), &t);
    for l in &g.lines {
        let d = (l.b - l.a).normalized();
        assert!(d.cross(u).abs() < 1e-9 || d.dot(u).abs() < 1e-9, "{l:?}");
    }
    assert!(g.texts.iter().all(|x| (x.rotation - 30f64.to_radians()).abs() < 1e-9));
    // TABLEDIT picks the cell under the UCS point.
    s.execute("tabledit", &json!({ "at": [4.0, -1.5], "text": "hello" })).unwrap();
    assert_eq!(table(&s, h).cells[1][1].text, "hello");
    // Picking hits the table inside its turned outline only.
    let p = world(&s, 5.0, -1.0);
    assert!(cadcraft_engine::select::entity_distance(s.doc().unwrap(), s.doc().unwrap().entity(h).unwrap(), p, 0.01) == 0.0);
    let outside = world(&s, 5.0, 0.6);
    assert!(cadcraft_engine::select::entity_distance(s.doc().unwrap(), s.doc().unwrap().entity(h).unwrap(), outside, 0.01) > 0.5);
    // DXF keeps the direction (group 11 of AcDbTable).
    let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc().unwrap()).as_bytes()).unwrap();
    let rt = back.model.iter().find_map(|e| if let EntityKind::Table(t) = &e.kind { Some(t.clone()) } else { None }).unwrap();
    assert!(near(rt.direction.xy(), u), "{:?}", rt.direction);
    // ROTATE turns it further; MIRROR keeps it reading left to right.
    s.cmdline("ucs w").unwrap();
    s.execute("rotate", &json!({ "handles": [h.hex()], "base": [0, 0], "angle": 60 })).unwrap();
    assert!(near(table(&s, h).axis(), Vec2::Y), "{:?}", table(&s, h).direction);
    s.execute("mirror", &json!({ "handles": [h.hex()], "p1": [0, 0], "p2": [1, 1], "erase": true })).unwrap();
    assert!(near(table(&s, h).axis(), Vec2::Y), "{:?}", table(&s, h).direction);
}

#[test]
fn a_broken_turned_table_moves_its_parts_in_its_own_axes() {
    let mut s = session(&["ucs z 90"]);
    let cells: Vec<Vec<String>> = (1..=6).map(|i| vec![i.to_string()]).collect();
    let h = handle(&s.execute("table", &json!({ "at": [0, 0], "cells": cells, "rowHeight": 1.0, "colWidth": 2.0 })).unwrap());
    s.execute("table.break", &json!({ "handle": h.hex(), "height": 3.0, "spacing": 1.0 })).unwrap();
    let t = table(&s, h);
    assert!(near(t.axis(), Vec2::Y));
    // The second part is beside the first along the table's rows: world (0, 3).
    let g = t.grip_points();
    assert!(near(g[1], Vec2::new(0.0, 3.0)), "{g:?}");
    // Dragging its grip 1 along world -X moves it 1 up the table (its own +y).
    let k = cadcraft_engine::grips::stretch_grip(&EntityKind::Table(t), 1, Vec2::new(-1.0, 3.0)).unwrap();
    let EntityKind::Table(moved) = k else { panic!() };
    assert!(near(moved.breaks.as_ref().unwrap().offsets[0], Vec2::new(0.0, 1.0)), "{:?}", moved.breaks);
    assert!(near(moved.grip_points()[1], Vec2::new(-1.0, 3.0)));
    // A cell of the moved second part: its first row (row 3) is under world (-0.5, 4).
    assert_eq!(moved.cell_at_point(Vec2::new(-0.5, 4.0)), Some((3, 0)));
    assert_eq!(moved.cell_at_point(Vec2::new(-1.5, 4.0)), None);
}

fn dims(s: &Session, r: &Value) -> Vec<Dimension> {
    let d = s.doc().unwrap();
    r["handles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| match &d.entity(Handle::parse_hex(h.as_str().unwrap()).unwrap()).unwrap().kind {
            EntityKind::Dimension(dm) => dm.clone(),
            k => panic!("not a dimension: {k:?}"),
        })
        .collect()
}

#[test]
fn qdim_ordinate_in_a_turned_ucs_measures_along_its_axes() {
    let mut s = session(&["ucs 100,50", "", "ucs z 30"]);
    let mut hs = Vec::new();
    for (a, b) in [(0.0, 10.0), (10.0, 30.0)] {
        hs.push(handle(&s.execute("line", &json!({ "points": [[a, 2.0], [b, 2.0]] })).unwrap()).hex());
    }
    // QDIM takes world points (it runs in World); the line position is UCS (0, 8).
    let at = world(&s, 0.0, 8.0);
    let r = s.execute("qdim", &json!({ "handles": hs, "at": [at.x, at.y], "mode": "ordinate" })).unwrap();
    let ds = dims(&s, &r);
    assert_eq!(ds.len(), 3);
    for (d, want) in ds.iter().zip([0.0, 10.0, 30.0]) {
        assert!(matches!(d.kind, DimKind::Ordinate { x_type: true }), "{d:?}");
        assert!(d.marks.horizontal.is_some_and(|h| (h - 30f64.to_radians()).abs() < 1e-9), "{d:?}");
        assert!(near(d.defpt.xy(), world(&s, 0.0, 0.0)));
        // The leader runs along the UCS y to the line position.
        assert!(near(d.p14.xy(), world(&s, want, 8.0)), "{:?} {want}", d.p14);
        let m = dimension_geometry(d, &DimStyle::default(), 1.0).measurement;
        assert!((m - want).abs() < 1e-9, "{m} {want}");
    }
    // Beside the geometry along the UCS x: Y datums, measuring the UCS y (2).
    let at = world(&s, 40.0, 2.0);
    let r = s.execute("qdim", &json!({ "handles": hs, "at": [at.x, at.y], "mode": "ordinate" })).unwrap();
    let ds = dims(&s, &r);
    assert!(!ds.is_empty() && ds.iter().all(|d| matches!(d.kind, DimKind::Ordinate { x_type: false })));
    for d in &ds {
        assert!((dimension_geometry(d, &DimStyle::default(), 1.0).measurement - 2.0).abs() < 1e-9);
    }
}

#[test]
fn a_world_ordinate_dimension_measures_the_same_after_rotate() {
    let mut s = session(&[]);
    let h = handle(&s.execute("dimordinate", &json!({ "feature": [3, 1], "leader": [3, 5] })).unwrap());
    let measure = |s: &Session| match &s.doc().unwrap().entity(h).unwrap().kind {
        EntityKind::Dimension(d) => dimension_geometry(d, &DimStyle::default(), 1.0).measurement,
        _ => panic!(),
    };
    assert!((measure(&s) - 3.0).abs() < 1e-9);
    s.execute("rotate", &json!({ "handles": [h.hex()], "base": [0, 0], "angle": 90 })).unwrap();
    assert!((measure(&s) - 3.0).abs() < 1e-9, "{}", measure(&s));
}
