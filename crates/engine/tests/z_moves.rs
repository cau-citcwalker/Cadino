//! 3D solids report their world bounding box (`bbox3`), MOVE and COPY take a Z displacement
//! (JSON `delta: [dx, dy, dz]` and typed `@0,0,100`), and BOX takes its height from two corners
//! at different heights.

use std::sync::Arc;

use cadcraft_engine::Session;
use cadcraft_engine::doc::{EntityKind, Solid3d};
use cadcraft_engine::geom::{Mat4, Mesh3, Vec3};
use serde_json::{Value, json};

/// A 2 x 3 x 4 block of mesh points placed by `at`.
fn solid(s: &mut Session, at: Vec3) -> String {
    let c = |x: f64, y: f64, z: f64| Vec3::new(x, y, z);
    let verts = vec![c(0.0, 0.0, 0.0), c(2.0, 0.0, 0.0), c(2.0, 3.0, 0.0), c(0.0, 3.0, 4.0), c(2.0, 3.0, 4.0)];
    let sol = Solid3d { mesh: Arc::new(Mesh3 { verts, ..Default::default() }), xform: Mat4::translate(at), ..Default::default() };
    s.add_entity(EntityKind::Solid3d(sol)).unwrap().hex()
}

fn bbox3(s: &mut Session, h: &str) -> Value {
    let r = s.execute("entities", &json!({ "type": "3dsolid" })).unwrap();
    r["entities"].as_array().unwrap().iter().find(|e| e["handle"] == h).map(|e| e["bbox3"].clone()).unwrap_or(Value::Null)
}

fn boxed(min: [f64; 3], max: [f64; 3]) -> Value {
    json!([min, max])
}

#[test]
fn solid_summaries_carry_their_world_bounding_box() {
    let mut s = Session::new();
    let h = solid(&mut s, Vec3::new(10.0, 0.0, 5.0));
    assert_eq!(bbox3(&mut s, &h), boxed([10.0, 0.0, 5.0], [12.0, 3.0, 9.0]));
    let i = s.execute("drawing.inspect", &json!({})).unwrap();
    assert_eq!(i["entities"][0]["bbox3"], boxed([10.0, 0.0, 5.0], [12.0, 3.0, 9.0]), "{i}");
    let p = s.execute("properties", &json!({ "handles": [h] })).unwrap();
    assert!(p.to_string().contains("\"bbox3\":[[10.0,0.0,5.0],[12.0,3.0,9.0]]"), "{p}");
    // Other objects have none.
    s.execute("circle", &json!({ "center": [0, 0], "radius": 1 })).unwrap();
    let r = s.execute("entities", &json!({ "type": "circle" })).unwrap();
    assert!(r["entities"][0].get("bbox3").is_none(), "{r}");
}

#[test]
fn json_move_and_copy_take_a_z_delta() {
    let mut s = Session::new();
    let h = solid(&mut s, Vec3::ZERO);
    s.execute("move", &json!({ "handles": [h], "delta": [1, 0, 100] })).unwrap();
    assert_eq!(bbox3(&mut s, &h), boxed([1.0, 0.0, 100.0], [3.0, 3.0, 104.0]));
    s.execute("move", &json!({ "handles": [h], "from": [0, 0, 100], "to": "0,0,0" })).unwrap();
    s.execute("move", &json!({ "handles": [h], "delta": "-1,0,0" })).unwrap();
    assert_eq!(bbox3(&mut s, &h), boxed([0.0, 0.0, 0.0], [2.0, 3.0, 4.0]));
    let r = s.execute("copy", &json!({ "handles": [h], "delta": [0, 0, 10], "count": 2 })).unwrap();
    let copies: Vec<String> = r["handles"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(copies.len(), 2);
    assert_eq!(bbox3(&mut s, &copies[0]), boxed([0.0, 0.0, 10.0], [2.0, 3.0, 14.0]));
    assert_eq!(bbox3(&mut s, &copies[1]), boxed([0.0, 0.0, 20.0], [2.0, 3.0, 24.0]));
    assert_eq!(bbox3(&mut s, &h), boxed([0.0, 0.0, 0.0], [2.0, 3.0, 4.0]), "the source stays");
}

#[test]
fn a_z_move_lifts_planar_objects_and_refuses_regions() {
    let mut s = Session::new();
    s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
    let l = s.doc().unwrap().model.last().unwrap().handle.hex();
    let c = s.execute("circle", &json!({ "center": [0, 0], "radius": 1 })).unwrap()["handle"].as_str().unwrap().to_string();
    s.execute("move", &json!({ "handles": [l, c], "delta": [0, 0, 7] })).unwrap();
    let r = s.execute("properties", &json!({ "handles": [l, c] })).unwrap().to_string();
    assert!(r.contains("\"a\":{\"x\":0.0,\"y\":0.0,\"z\":7.0}") && r.contains("\"center\":{\"x\":0.0,\"y\":0.0,\"z\":7.0}"), "{r}");
    // A region stays on the XY plane: the move is refused, nothing moves, nothing is dropped.
    let p = s.execute("pline", &json!({ "vertices": [[20, 0], [30, 0], [30, 5]], "closed": true })).unwrap()["handle"].as_str().unwrap().to_string();
    s.execute("region", &json!({ "handles": [p] })).unwrap();
    let reg = s.execute("entities", &json!({ "type": "region" })).unwrap()["entities"][0]["handle"].as_str().unwrap().to_string();
    let before = s.execute("drawing.inspect", &json!({})).unwrap()["entities"].clone();
    let e = s.execute("move", &json!({ "handles": [reg, l], "delta": [0, 0, 1] })).unwrap_err().to_string();
    assert!(e.contains("region") && e.contains("in Z"), "{e}");
    assert_eq!(s.execute("drawing.inspect", &json!({})).unwrap()["entities"], before);
    // Without a Z it moves as before.
    s.execute("move", &json!({ "handles": [reg], "delta": [1, 0, 0] })).unwrap();
}

#[test]
fn typed_points_with_a_height_move_and_copy_in_z() {
    let mut s = Session::new();
    let h = solid(&mut s, Vec3::ZERO);
    s.cmdline("move all  0,0,0 @0,0,100").unwrap();
    assert!(s.running.is_none());
    assert_eq!(bbox3(&mut s, &h), boxed([0.0, 0.0, 100.0], [2.0, 3.0, 104.0]));
    // A plain second point after a base with a height comes back down.
    s.cmdline("move all  5,5,100 5,5").unwrap();
    assert_eq!(bbox3(&mut s, &h), boxed([0.0, 0.0, 0.0], [2.0, 3.0, 4.0]));
    // The Displacement option and Enter at the second point use the point itself.
    s.script("MOVE ALL\n\nD\n0,0,30\n").unwrap();
    assert_eq!(bbox3(&mut s, &h), boxed([0.0, 0.0, 30.0], [2.0, 3.0, 34.0]));
    s.script("MOVE ALL\n\n0,0,-30\n\n").unwrap();
    assert_eq!(bbox3(&mut s, &h), boxed([0.0, 0.0, 0.0], [2.0, 3.0, 4.0]));
    s.cmdline("copy all  0,0 @0,0,50 ").unwrap();
    assert!(s.running.is_none());
    let r = s.execute("entities", &json!({ "type": "3dsolid" })).unwrap();
    let mut zs: Vec<f64> = r["entities"].as_array().unwrap().iter().map(|e| e["bbox3"][0][2].as_f64().unwrap()).collect();
    zs.sort_by(f64::total_cmp);
    assert_eq!(zs, vec![0.0, 50.0]);
}

#[test]
fn box_with_corners_at_different_heights_asks_no_height() {
    let mut s = Session::new();
    let n = s.doc().unwrap().entity_count();
    let r = s.cmdline("box 0,0,0 600,400,750");
    if cfg!(feature = "occt") {
        r.unwrap();
        assert!(s.running.is_none(), "{:?}", s.current_prompt().map(|p| p.message));
        let r = s.execute("entities", &json!({ "type": "3dsolid" })).unwrap();
        assert_eq!(r["entities"][0]["bbox3"], boxed([0.0, 0.0, 0.0], [600.0, 400.0, 750.0]), "{r}");
        // Corners on one level still ask for the height.
        s.cmdline("box 0,0,0 10,10,0").unwrap();
        assert!(s.current_prompt().is_some_and(|p| p.message.contains("height")));
        s.cmdline("5").unwrap();
        assert_eq!(s.doc().unwrap().entity_count(), n + 2);
    } else {
        // No kernel: the box is tried at once (and fails) rather than asking for a height.
        assert!(!s.current_prompt().is_some_and(|p| p.message.contains("height")), "{:?}", s.log);
    }
}
