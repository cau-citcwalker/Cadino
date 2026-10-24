//! Dynamic blocks made in the Block Editor: what a DXF gets of them (only AutoCAD's own layouts
//! as AutoCAD objects; the rest in Cadino's CADCRAFT_DYNBLOCK record, read back), array actions
//! copying objects on references, and lookup tables set with BLOOKUPTABLE (Allow reverse lookup).

use cadcraft_engine::Session;
use cadcraft_engine::doc::EntityKind;
use cadcraft_engine::doc::dynblock::file::OWN_TYPES;
use serde_json::{Value, json};

fn line(s: &mut Session, a: [f64; 2], b: [f64; 2]) -> Value {
    s.execute("line", &json!({ "points": [a, b] })).unwrap()["handles"][0].clone()
}

/// Block "Shelf": a board 36 long and a bracket at x = 2. Returns their handles in the block.
fn shelf(s: &mut Session) -> Vec<Value> {
    let board = line(s, [0.0, 0.0], [36.0, 0.0]);
    let bracket = line(s, [2.0, 0.0], [2.0, -6.0]);
    s.execute("block", &json!({ "name": "Shelf", "base": [0, 0], "handles": [board, bracket] })).unwrap();
    // BLOCK leaves a reference in their place: not wanted here.
    s.execute("erase", &json!({ "handles": [s.doc().unwrap().model.handles()[0].hex()] })).unwrap();
    s.doc().unwrap().block("Shelf").unwrap().entities.handles().iter().map(|h| json!(h.hex())).collect()
}

/// The model without handles (they change through a file) and visibility states' active lists.
fn shape(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter().filter(|(k, _)| !matches!(k.as_str(), "handle" | "selection" | "active")).map(|(k, v)| (k.clone(), shape(v))).collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(shape).collect()),
        v => v.clone(),
    }
}

/// The DXF's groups as (code, value).
fn groups(text: &str) -> Vec<(i32, String)> {
    let lines: Vec<&str> = text.lines().collect();
    lines.chunks(2).filter_map(|c| Some((c.first()?.trim().parse().ok()?, c.get(1)?.trim().to_string()))).collect()
}

#[test]
fn a_block_made_here_writes_only_autocad_layouts_as_autocad_objects() {
    let mut s = Session::new();
    let hs = shelf(&mut s);
    s.execute("bedit", &json!({ "name": "Shelf" })).unwrap();
    for p in [
        json!({ "type": "linear", "base": [0, 0], "end": [36, 0], "label": "Width" }),
        json!({ "type": "polar", "base": [0, 10], "end": [10, 10] }),
        json!({ "type": "xy", "base": [0, -10], "end": [10, -5] }),
        json!({ "type": "rotation", "base": [0, 0], "radius": 5 }),
        json!({ "type": "alignment", "base": [0, 0] }),
        json!({ "type": "flip", "base": [-2, 0], "end": [40, 0] }),
        json!({ "type": "point", "at": [36, 0] }),
        json!({ "type": "visibility", "at": [18, 12], "states": ["Plain", "Bare"] }),
        json!({ "type": "lookup", "at": [18, 16] }),
        json!({ "type": "basepoint", "at": [0, 0] }),
    ] {
        s.execute("bparameter", &p).unwrap();
    }
    let (board, bracket) = (&hs[0], &hs[1]);
    for p in [
        json!({ "type": "stretch", "parameter": "Width", "frame": [[30, -1], [40, 1]], "handles": [board] }),
        json!({ "type": "array", "parameter": "Width", "column": 12, "handles": [bracket] }),
        json!({ "type": "move", "parameter": "Position1", "handles": [bracket] }),
        json!({ "type": "scale", "parameter": "Distance1", "handles": [bracket] }),
        json!({ "type": "polarstretch", "parameter": "Distance1", "frame": [[30, -1], [40, 1]], "handles": [board] }),
        json!({ "type": "move", "parameter": "Horizontal", "handles": [bracket] }),
        json!({ "type": "rotate", "parameter": "Angle2", "handles": [bracket] }),
        json!({ "type": "flip", "parameter": "Flip state1", "handles": [board, bracket], "parameters": ["Width"] }),
        json!({ "type": "lookup", "parameter": "Lookup1", "columns": [{ "parameter": "Width" }, { "parameter": "Lookup1", "reverse": true }],
                "rows": [["24", "Short"], ["36", "Long"]] }),
    ] {
        s.execute("baction", &p).unwrap();
    }
    s.execute("bclose", &json!({})).unwrap();
    let before = shape(&s.execute("blocks.dynamic", &json!({ "name": "Shelf" })).unwrap());

    let text = cadcraft_io::write_dxf(s.doc().unwrap());
    let gs = groups(&text);
    let own = |v: &str| OWN_TYPES.iter().any(|(t, c)| v.eq_ignore_ascii_case(t) || v.eq_ignore_ascii_case(c));
    // No object of a type whose layout is Cadino's own, and no CLASS record for one.
    let objects: Vec<&str> = gs.iter().filter(|(c, _)| *c == 0).map(|(_, v)| v.as_str()).collect();
    assert!(!objects.iter().any(|t| own(t)), "{objects:?}");
    let mut in_class = false;
    for (c, v) in &gs {
        if *c == 0 {
            in_class = v == "CLASS";
        }
        assert!(!(in_class && matches!(c, 1 | 2) && own(v)), "CLASS record for {v}");
    }
    // AutoCAD's own layouts are written as AutoCAD's objects; Cadino's sections aren't.
    for t in ["BLOCKLINEARPARAMETER", "BLOCKVISIBILITYPARAMETER", "BLOCKSTRETCHACTION", "BLOCKLOOKUPACTION"] {
        assert!(objects.contains(&t), "{t} missing");
    }
    assert!(!gs.iter().any(|(c, v)| *c == 100 && v.starts_with("Cadcraft")));
    assert!(gs.iter().any(|(c, v)| *c == 3 && v == "CADCRAFT_DYNBLOCK"));

    // Read back: the same dynamic block, and it still works.
    let d = cadcraft_io::read_dxf(text.as_bytes()).unwrap();
    let mut s2 = Session::empty();
    s2.open_drawing(d, "shelf", None);
    assert_eq!(shape(&s2.execute("blocks.dynamic", &json!({ "name": "Shelf" })).unwrap()), before);
    let r = s2.execute("insert", &json!({ "name": "Shelf", "at": [0, 0] })).unwrap()["handle"].clone();
    s2.execute("blocks.reference.set", &json!({ "handle": r, "name": "Flip state1", "value": "Flipped" })).unwrap();
    assert_eq!(brackets(&s2, &r), [(2.0, 6.0)], "the flip (kept in the record) mirrors the bracket");
}

/// The bracket lines of reference `r`: (x, lower y) of each vertical line, sorted.
fn brackets(s: &Session, r: &Value) -> Vec<(f64, f64)> {
    let d = s.doc().unwrap();
    let EntityKind::Insert(i) = &d.entity(cadcraft_engine::doc::Handle::parse_hex(r.as_str().unwrap()).unwrap()).unwrap().kind else {
        panic!("a reference")
    };
    let mut out: Vec<(f64, f64)> = d
        .block(&i.block)
        .unwrap()
        .entities
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Line(l) if (l.a.x - l.b.x).abs() < 1e-9 => Some(((l.a.x * 1e6).round() / 1e6, l.b.y.round())),
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| a.partial_cmp(b).unwrap());
    out
}

/// A shelf whose bracket is arrayed every 12 along its width.
fn arrayed_shelf(s: &mut Session) -> Value {
    let hs = shelf(s);
    s.execute("bedit", &json!({ "name": "Shelf" })).unwrap();
    s.execute("bparameter", &json!({ "type": "linear", "base": [0, 0], "end": [36, 0], "label": "Width", "grips": 1 })).unwrap();
    s.execute("baction", &json!({ "type": "stretch", "parameter": "Width", "frame": [[30, -1], [40, 1]], "handles": [hs[0]] })).unwrap();
    s.execute("baction", &json!({ "type": "array", "parameter": "Width", "column": 12, "handles": [hs[1]] })).unwrap();
    s.execute("bclose", &json!({})).unwrap();
    s.execute("insert", &json!({ "name": "Shelf", "at": [100, 0] })).unwrap()["handle"].clone()
}

#[test]
fn an_array_action_copies_its_objects_as_the_distance_holds_them() {
    let mut s = Session::new();
    let r = arrayed_shelf(&mut s);
    assert_eq!(brackets(&s, &r), [(2.0, -6.0)], "a new reference shows the block as drawn");
    // 60 holds five columns 12 apart.
    s.execute("blocks.reference.set", &json!({ "handle": r, "name": "Width", "value": "60" })).unwrap();
    assert_eq!(brackets(&s, &r), [(2.0, -6.0), (14.0, -6.0), (26.0, -6.0), (38.0, -6.0), (50.0, -6.0)]);
    // Narrower: the copies are made again, not added to.
    s.execute("blocks.reference.set", &json!({ "handle": r, "name": "Width", "value": "30" })).unwrap();
    assert_eq!(brackets(&s, &r), [(2.0, -6.0), (14.0, -6.0)]);
    // Its grip drags the width, and the array follows.
    let g = s.execute("blocks.reference.grips", &json!({ "handle": r })).unwrap();
    s.execute("blocks.reference.grip", &json!({ "handle": r, "node": g[0]["node"], "part": "end", "to": [148.0, 3.0] })).unwrap();
    assert_eq!(brackets(&s, &r).len(), 4);
    s.execute("undo", &json!({})).unwrap();
    assert_eq!(brackets(&s, &r).len(), 2, "one undo step");

    // Through a DXF the copies stay marked: the next change makes them again.
    let d = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc().unwrap()).as_bytes()).unwrap();
    let mut s2 = Session::empty();
    s2.open_drawing(d, "shelf", None);
    let r2 = json!(
        s2.doc()
            .unwrap()
            .model
            .handles()
            .iter()
            .find(|h| matches!(s2.doc().unwrap().entity(**h).unwrap().kind, EntityKind::Insert(_)))
            .unwrap()
            .hex()
    );
    assert_eq!(brackets(&s2, &r2).len(), 2);
    s2.execute("blocks.reference.set", &json!({ "handle": r2, "name": "Width", "value": "40" })).unwrap();
    assert_eq!(brackets(&s2, &r2), [(2.0, -6.0), (14.0, -6.0), (26.0, -6.0)]);
}

#[test]
fn blookuptable_sets_a_table_and_reverse_lookup_decides_whether_it_can_be_chosen() {
    let mut s = Session::new();
    let hs = shelf(&mut s);
    s.execute("bedit", &json!({ "name": "Shelf" })).unwrap();
    s.execute("bparameter", &json!({ "type": "linear", "base": [0, 0], "end": [36, 0], "label": "Width" })).unwrap();
    s.execute("baction", &json!({ "type": "stretch", "parameter": "Width", "frame": [[30, -1], [40, 1]], "handles": [hs[0]] })).unwrap();
    s.execute("bparameter", &json!({ "type": "lookup", "at": [18, 10], "label": "Size" })).unwrap();
    assert!(s.execute("blookuptable", &json!({})).is_err(), "no lookup action yet");
    // BACTION Lookup at the prompts adds an empty table for the dialog to fill.
    s.script("BACTION Size 18,14\n").unwrap();
    let t = s.execute("blookuptable", &json!({})).unwrap();
    assert_eq!((t["name"].as_str(), t["rows"].as_array().map(Vec::len)), (Some("Lookup"), Some(0)), "{:?}", s.log);
    let available: Vec<&str> = t["available"].as_array().unwrap().iter().map(|a| a["label"].as_str().unwrap()).collect();
    assert_eq!(available, ["Width", "Size"]);

    let table = |reverse: bool, rows: Value| json!({ "columns": [{ "parameter": "Width" }, { "parameter": "Size", "lookup": true, "reverse": reverse }], "rows": rows });
    // Audit: the same inputs twice, refused (and `check` changes nothing either way).
    assert!(s.execute("blookuptable", &table(false, json!([["24", "Short"], ["24.0", "Again"]]))).is_err());
    let mut checked = table(false, json!([["24", "Short"]]));
    checked["check"] = json!(true);
    s.execute("blookuptable", &checked).unwrap();
    assert_eq!(s.execute("blookuptable", &json!({})).unwrap()["rows"], json!([]));

    let t = s.execute("blookuptable", &table(false, json!([["24", "Short"], ["36", "Long"]]))).unwrap();
    assert_eq!(t["rows"], json!([["24", "Short"], ["36", "Long"]]));
    assert_eq!(t["columns"][1]["reverse"], json!(false));
    s.execute("bclose", &json!({})).unwrap();

    // Read only: the reference shows the value its width gives, and it can't be chosen.
    let r = s.execute("insert", &json!({ "name": "Shelf", "at": [0, 0] })).unwrap()["handle"].clone();
    let size = |s: &mut Session| {
        let p = s.execute("blocks.reference", &json!({ "handle": r })).unwrap();
        p["properties"].as_array().unwrap().iter().find(|p| p["name"] == "Size").cloned().unwrap()
    };
    assert_eq!((size(&mut s)["value"].as_str(), size(&mut s)["editable"].as_bool()), (Some("Long"), Some(false)));
    assert!(s.execute("blocks.reference.set", &json!({ "handle": r, "name": "Size", "value": "Short" })).is_err());
    s.execute("blocks.reference.set", &json!({ "handle": r, "name": "Width", "value": "24" })).unwrap();
    assert_eq!(size(&mut s)["value"], "Short");

    // Allow reverse lookup: choosing Long sets the width; kept through a DXF.
    s.execute("bedit", &json!({ "name": "Shelf" })).unwrap();
    s.execute("blookuptable", &table(true, json!([["24", "Short"], ["36", "Long"]]))).unwrap();
    s.execute("bclose", &json!({})).unwrap();
    let d = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc().unwrap()).as_bytes()).unwrap();
    let mut s2 = Session::empty();
    s2.open_drawing(d, "shelf", None);
    let r2 = s2.execute("insert", &json!({ "name": "Shelf", "at": [0, 0] })).unwrap()["handle"].clone();
    s2.execute("blocks.reference.set", &json!({ "handle": r2, "name": "Size", "value": "Short" })).unwrap();
    let p = s2.execute("blocks.reference", &json!({ "handle": r2 })).unwrap();
    let width = p["properties"].as_array().unwrap().iter().find(|p| p["name"] == "Width").unwrap()["value"].clone();
    assert_eq!(width, "24.0000");
}
