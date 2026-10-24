//! Dynamic block authoring in the Block Editor (BPARAMETER, BACTION, BVSTATE, BVSHOW, BVHIDE,
//! BGRIPSET): a door made dynamic with a stretchable width, a flip and Open/Closed visibility
//! states, then used through a reference (its grips and custom properties), saved as DXF and
//! read back.

use cadcraft_engine::Session;
use cadcraft_engine::doc::{EntityKind, Handle, Line};
use cadcraft_engine::geom::Vec3;
use serde_json::{Value, json};

fn h(v: &Value) -> Handle {
    Handle::parse_hex(v.as_str().unwrap()).unwrap()
}

fn line(s: &mut Session, a: [f64; 2], b: [f64; 2]) -> Value {
    s.execute("line", &json!({ "points": [a, b] })).unwrap()["handles"][0].clone()
}

/// Block "Door": the open leaf, the swing arc, the closed leaf and the sill. Returns their
/// handles (as the block keeps them) in that order.
fn door(s: &mut Session) -> Vec<Value> {
    let leaf = line(s, [0.0, 0.0], [0.0, 36.0]);
    let swing = s.execute("arc", &json!({ "center": [0, 0], "radius": 36, "start": 0, "end": 90 })).unwrap()["handle"].clone();
    let closed = line(s, [0.0, 0.0], [36.0, 0.0]);
    let sill = line(s, [0.0, -2.0], [36.0, -2.0]);
    let hs = vec![leaf, swing, closed, sill];
    s.execute("block", &json!({ "name": "Door", "base": [0, 0], "handles": hs })).unwrap();
    s.execute("erase", &json!({ "handles": [s.doc().unwrap().model.handles()[0].hex()] })).unwrap();
    s.doc().unwrap().block("Door").unwrap().entities.handles().iter().map(|h| json!(h.hex())).collect()
}

/// Make "Door" dynamic in the Block Editor.
fn author(s: &mut Session, hs: &[Value]) {
    let (leaf, swing, closed, sill) = (&hs[0], &hs[1], &hs[2], &hs[3]);
    s.execute("bedit", &json!({ "name": "Door" })).unwrap();
    let w = s
        .execute("bparameter", &json!({ "type": "linear", "base": [0, -4], "end": [36, -4], "valueSet": { "list": [30, 32] }, "grips": 1 }))
        .unwrap();
    assert_eq!(w["label"], "Distance1");
    s.execute("bparameter.set", &json!({ "parameter": "Distance1", "label": "Width", "description": "Door leaf width" })).unwrap();
    let r =
        s.execute("baction", &json!({ "type": "stretch", "parameter": "Width", "frame": [[34, -6], [40, 2]], "handles": [closed, sill] })).unwrap();
    assert_eq!(r["name"], "Stretch");
    assert!(
        s.execute("baction", &json!({ "type": "rotate", "parameter": "Width", "handles": [sill] })).is_err(),
        "a linear parameter doesn't rotate"
    );

    s.execute("bparameter", &json!({ "type": "flip", "base": [-2, 0], "end": [40, 0], "labelAt": [18, 0] })).unwrap();
    s.execute("baction", &json!({ "parameter": "Flip state1", "handles": [leaf, swing, closed, sill], "parameters": ["Width"] })).unwrap();

    let v = s.execute("bparameter", &json!({ "type": "visibility", "at": [18, 40], "states": ["Open"] })).unwrap();
    assert_eq!(v["label"], "Visibility1");
    assert!(s.execute("bparameter", &json!({ "type": "visibility", "at": [0, 0] })).is_err(), "one visibility parameter a block");
    let r = s.execute("bvstate", &json!({ "operation": "new", "name": "Closed" })).unwrap();
    assert_eq!(r, json!({ "states": ["Open", "Closed"], "current": "Closed" }));
    s.execute("bvhide", &json!({ "handles": [leaf, swing] })).unwrap();
    // The editor shows the current state: the open leaf and the swing are hidden in Closed.
    let shown = |s: &Session, v: &Value| s.doc().unwrap().model.get(h(v)).unwrap().common.visible;
    assert!(!shown(s, leaf) && !shown(s, swing) && shown(s, closed));
    s.execute("bvstate", &json!({ "operation": "set", "name": "Open" })).unwrap();
    assert!(shown(s, leaf) && shown(s, closed));
    s.execute("bvhide", &json!({ "handles": [closed] })).unwrap();
    assert!(!shown(s, closed));
    s.execute("bclose", &json!({})).unwrap();
}

fn props(s: &mut Session, r: &Value) -> Vec<(String, String)> {
    let p = s.execute("blocks.reference", &json!({ "handle": r })).unwrap();
    p["properties"].as_array().unwrap().iter().map(|p| (p["name"].as_str().unwrap().to_string(), p["value"].as_str().unwrap().to_string())).collect()
}

fn prop(s: &mut Session, r: &Value, name: &str) -> String {
    props(s, r).into_iter().find(|(n, _)| n == name).map(|(_, v)| v).unwrap_or_default()
}

/// The lines of the block reference `r` shows, by their place in it, and which are visible.
fn shown_lines(s: &Session, r: &Value) -> Vec<(Line, bool)> {
    let d = s.doc().unwrap();
    let EntityKind::Insert(i) = &d.entity(h(r)).unwrap().kind else { panic!("a reference") };
    let b = d.block(&i.block).unwrap();
    b.entities.iter().filter_map(|e| if let EntityKind::Line(l) = &e.kind { Some((l.clone(), e.common.visible)) } else { None }).collect()
}

fn near(a: Vec3, x: f64, y: f64) -> bool {
    (a.x - x).abs() < 1e-9 && (a.y - y).abs() < 1e-9
}

#[test]
fn a_door_made_dynamic_flips_stretches_and_changes_state_and_survives_dxf() {
    let mut s = Session::new();
    let hs = door(&mut s);
    author(&mut s, &hs);

    // The definition shows its first state: the closed leaf is hidden.
    let def = s.doc().unwrap().block("Door").unwrap().clone();
    let visible: Vec<bool> = def.entities.iter().map(|e| e.common.visible).collect();
    assert_eq!(visible, [true, true, false, true]);
    let m = s.execute("blocks.dynamic", &json!({ "name": "Door" })).unwrap();
    let labels: Vec<&str> = m["parameters"].as_array().unwrap().iter().map(|p| p["kind"]["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["Width", "Flip state1", "Visibility1"]);
    let actions: Vec<&str> = m["actions"].as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap()).collect();
    assert_eq!(actions, ["Stretch", "Flip"]);

    let r = s.execute("insert", &json!({ "name": "Door", "at": [100, 0] })).unwrap()["handle"].clone();
    assert_eq!(
        props(&mut s, &r),
        [("Width".to_string(), "36.0000".to_string()), ("Flip state1".into(), "Not Flipped".into()), ("Visibility1".into(), "Open".into())]
    );
    let grips = s.execute("blocks.reference.grips", &json!({ "handle": r })).unwrap();
    let shapes: Vec<&str> = grips.as_array().unwrap().iter().map(|g| g["shape"].as_str().unwrap()).collect();
    assert_eq!(shapes, ["linear", "flip", "list"]);

    // Width: the closed leaf and the sill stretch to the list's nearest value.
    s.execute("blocks.reference.set", &json!({ "handle": r, "name": "Width", "value": "31" })).unwrap();
    assert_eq!(prop(&mut s, &r, "Width"), "30.0000");
    // (The lines: the open leaf, the closed leaf and the sill.)
    let ls = shown_lines(&s, &r);
    assert!(near(ls[1].0.b, 30.0, 0.0) && near(ls[2].0.b, 30.0, -2.0), "{ls:?}");

    // Flip: everything mirrors across the wall line, the width's grip too.
    s.execute("blocks.reference.set", &json!({ "handle": r, "name": "Flip state1", "value": "Flipped" })).unwrap();
    let ls = shown_lines(&s, &r);
    assert!(near(ls[0].0.b, 0.0, -36.0) && near(ls[2].0.a, 0.0, 2.0), "{ls:?}");
    let g = s.execute("blocks.reference.grips", &json!({ "handle": r })).unwrap();
    assert_eq!(g[0]["at"], json!([130.0, 4.0]), "the width grip flipped with the door");
    // Stretched after the flip, the flipped sill follows.
    s.execute("blocks.reference.grip", &json!({ "handle": r, "node": g[0]["node"], "part": "end", "to": [132.2, 4.0] })).unwrap();
    assert_eq!(prop(&mut s, &r, "Width"), "32.0000");
    assert!(near(shown_lines(&s, &r)[2].0.b, 32.0, 2.0));

    // Visibility: Closed shows the closed leaf only.
    s.execute("blocks.reference.set", &json!({ "handle": r, "name": "Visibility1", "value": "Closed" })).unwrap();
    let vis: Vec<bool> = shown_lines(&s, &r).iter().map(|(_, v)| *v).collect();
    assert_eq!(vis, [false, true, true]);

    // Saved and read back: the definition and the reference's values are kept, and it still
    // changes.
    let text = cadcraft_io::write_dxf(s.doc().unwrap());
    let d = cadcraft_io::read_dxf(text.as_bytes()).unwrap();
    let mut s2 = Session::empty();
    s2.open_drawing(d, "door", None);
    let m2 = s2.execute("blocks.dynamic", &json!({ "name": "Door" })).unwrap();
    assert_eq!(m2["parameters"].as_array().unwrap().len(), 3);
    assert_eq!(m2["actions"].as_array().unwrap().len(), 2);
    let r2 = json!(
        s2.doc()
            .unwrap()
            .model
            .handles()
            .iter()
            .find(|x| matches!(s2.doc().unwrap().entity(**x).unwrap().kind, EntityKind::Insert(_)))
            .unwrap()
            .hex()
    );
    assert_eq!(
        props(&mut s2, &r2),
        [("Width".to_string(), "32.0000".to_string()), ("Flip state1".into(), "Flipped".into()), ("Visibility1".into(), "Closed".into())]
    );
    s2.execute("blocks.reference.set", &json!({ "handle": r2, "name": "Flip state1", "value": "Not Flipped" })).unwrap();
    s2.execute("blocks.reference.set", &json!({ "handle": r2, "name": "Visibility1", "value": "Open" })).unwrap();
    let ls = shown_lines(&s2, &r2);
    assert!(near(ls[0].0.b, 0.0, 36.0) && ls[0].1 && !ls[1].1 && near(ls[2].0.b, 32.0, -2.0), "{ls:?}");

    // The Block Editor reopens on it and keeps authoring.
    s2.execute("bedit", &json!({ "name": "Door" })).unwrap();
    let r = s2.execute("bvstate", &json!({ "operation": "rename", "name": "Closed", "newName": "Shut" })).unwrap();
    assert_eq!(r["states"], json!(["Open", "Shut"]));
    s2.execute("bgripset", &json!({ "parameter": "Width", "grips": 2 })).unwrap();
    let removed = s2.execute("bparameter.delete", &json!({ "parameter": "Flip state1" })).unwrap();
    assert_eq!(removed["removed"], json!(["Flip state1", "Flip"]));
    s2.execute("bclose", &json!({})).unwrap();
    let m3 = s2.execute("blocks.dynamic", &json!({ "name": "Door" })).unwrap();
    assert_eq!(m3["parameters"][0]["grips"].as_array().unwrap().len(), 2);
    assert_eq!(m3["actions"].as_array().unwrap().len(), 1);
}

#[test]
fn authoring_is_refused_outside_the_block_editor_and_with_bad_parameters() {
    let mut s = Session::new();
    let hs = door(&mut s);
    assert!(s.execute("bparameter", &json!({ "type": "point", "at": [0, 0] })).is_err());
    assert!(s.execute("bvstate", &json!({ "name": "Open" })).is_err());
    s.execute("bedit", &json!({ "name": "Door" })).unwrap();
    for p in [
        json!({}),
        json!({ "type": "nope" }),
        json!({ "type": "linear", "base": [0, 0] }),
        json!({ "type": "linear", "base": [0, 0], "end": [0, 0] }),
        json!({ "type": "rotation", "base": [0, 0] }),
        json!({ "type": "linear", "base": [0, 0], "end": [1, 0], "valueSet": { "list": ["a"] } }),
        json!({ "type": "linear", "base": [0, 0], "end": [1, 0], "valueSet": { "increment": -1 } }),
        json!({ "type": "linear", "base": [0, 0], "end": [1e308, 1e308], "grips": "x" }),
        json!(null),
    ] {
        assert!(s.execute("bparameter", &p).is_err(), "{p}");
    }
    assert!(s.execute("baction", &json!({ "type": "move", "parameter": "Distance1", "handles": [hs[0]] })).is_err(), "no parameters yet");
    s.execute("bparameter", &json!({ "type": "point", "at": [0, 36] })).unwrap();
    for p in [
        json!({ "type": "move", "parameter": "Nope", "handles": [hs[0]] }),
        json!({ "type": "flip", "parameter": "Position1", "handles": [hs[0]] }),
        json!({ "type": "move", "parameter": "Position1", "handles": [] }),
        json!({ "type": "stretch", "parameter": "Position1", "handles": [hs[0]] }),
        json!({ "type": "move", "parameter": "Position1", "handles": ["zz"] }),
    ] {
        assert!(s.execute("baction", &p).is_err(), "{p}");
    }
    s.execute("baction", &json!({ "type": "move", "parameter": "Position1", "handles": [hs[0]] })).unwrap();
    assert!(s.execute("bvshow", &json!({ "handles": [hs[0]] })).is_err(), "no visibility parameter");
    // A position moves its objects on a reference.
    s.execute("bclose", &json!({})).unwrap();
    let r = s.execute("insert", &json!({ "name": "Door", "at": [0, 0] })).unwrap()["handle"].clone();
    s.execute("blocks.reference.set", &json!({ "handle": r, "name": "Position1 Y", "value": "40" })).unwrap();
    assert!(near(shown_lines(&s, &r)[0].0.b, 0.0, 40.0));
}

#[test]
fn the_prompts_author_as_autocad_asks() {
    let mut s = Session::new();
    let hs = door(&mut s);
    s.execute("bedit", &json!({ "name": "Door" })).unwrap();
    // BPARAMETER Linear with a label and a value set given through its options. (A text answer
    // takes the rest of a script line, spaces and all.)
    s.script("BPARAMETER L LABEL\nWidth\nV L\n30,32\n0,-4 36,-4 18,-8 1\n").unwrap();
    let m = s.execute("blocks.dynamic", &json!({ "name": "Door" })).unwrap();
    assert_eq!(m["parameters"][0]["kind"]["label"], "Width", "{:?} {:?}", s.log, s.current_prompt());
    assert_eq!(m["parameters"][0]["kind"]["values"]["list"], json!([30.0, 32.0, 36.0]));
    // BACTION (AC): the parameter by its label, Stretch, the second point, a frame, the objects
    // (a crossing window over the closed leaf's end and the sill's) and the location.
    s.script("AC Width T S 34,-6 40,2 40,1 34,-3  40,-10\n").unwrap();
    let m = s.execute("blocks.dynamic", &json!({ "name": "Door" })).unwrap();
    assert_eq!(m["actions"][0]["kind"]["type"], "stretch", "{:?} {:?}", s.log, s.current_prompt());
    let sel: Vec<&str> = m["actions"][0]["selection"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(sel, [hs[2].as_str().unwrap(), hs[3].as_str().unwrap()]);
    // Visibility parameter, then -BVSTATE New with Hide all.
    s.script("BPARAMETER V 18,40 1\n").unwrap();
    s.script("-BVSTATE N\nEmpty\nH\n").unwrap();
    let r = s.execute("bvstate", &json!({ "operation": "set", "name": "Empty" })).unwrap();
    assert_eq!(r["states"], json!(["VisibilityState0", "Empty"]));
    assert!(s.doc().unwrap().model.iter().all(|e| !e.common.visible), "Hide all hides every object");
    // BVSHOW the sill in every state.
    s.execute("bvshow", &json!({ "handles": [hs[3]], "all": true })).unwrap();
    assert!(s.doc().unwrap().model.get(h(&hs[3])).unwrap().common.visible);
    // Without the visibility parameter every object shows again.
    s.execute("bparameter.delete", &json!({ "parameter": "Visibility1" })).unwrap();
    assert!(s.doc().unwrap().model.iter().all(|e| e.common.visible));
}

#[test]
fn the_test_block_window_tries_the_block_as_edited_and_keeps_nothing() {
    let mut s = Session::new();
    let hs = door(&mut s);
    s.execute("bedit", &json!({ "name": "Door" })).unwrap();
    s.execute("bparameter", &json!({ "type": "flip", "base": [-2, 0], "end": [40, 0] })).unwrap();
    s.execute("baction", &json!({ "parameter": "Flip state1", "handles": hs })).unwrap();
    let editor = s.doc().unwrap().clone();
    // Not saved into the block yet: the test window shows the block as edited.
    let t = s.execute("btestblock", &json!({})).unwrap();
    assert_eq!(s.doc().unwrap().model.len(), 1);
    let r = t["handle"].clone();
    s.execute("blocks.reference.set", &json!({ "handle": r, "name": "Flip state1", "value": "Flipped" })).unwrap();
    assert!(near(shown_lines(&s, &r)[0].0.b, 0.0, -36.0));
    for (cmd, p) in [("bsave", json!({})), ("bparameter", json!({ "type": "point", "at": [0, 0] })), ("btestblock", json!({}))] {
        assert!(s.execute(cmd, &p).is_err(), "{cmd} in the test window");
    }
    // BCLOSE closes the window: the editor as it was, still unsaved.
    s.execute("bclose", &json!({})).unwrap();
    assert_eq!(*s.doc().unwrap(), editor);
    assert!(s.state().unwrap().block_editor().is_some() && s.state().unwrap().block_unsaved());
    s.execute("bclose", &json!({ "save": false })).unwrap();
    assert!(s.state().unwrap().block_editor().is_none());
    assert_eq!(s.execute("blocks.dynamic", &json!({ "name": "Door" })).unwrap(), json!(null), "nothing saved");
}
