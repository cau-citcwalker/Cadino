//! Grips of dynamic block references: listed, dragged by `blocks.reference.grip` (one undo
//! step), and dragged interactively the way a click on a grip starts it, with a preview and a
//! typed distance. The drawing is the synthetic DOOR block of the io tests, with grips added.

use cadcraft_engine::doc::dynblock::GripPart;
use cadcraft_engine::doc::{EntityKind, Handle, Space};
use cadcraft_engine::geom::{Vec2, Vec3};
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

/// A session with DOOR (Width's end grip 9 shown in the Open state, the visibility grip 10) and
/// a reference to it at (100, 0).
fn session() -> (Session, Handle) {
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
    let r = s.execute("insert", &json!({ "name": "Door", "at": [100, 0] })).unwrap();
    let h = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
    assert!(s.doc().unwrap().space_of(h) == Some(Space::Model));
    (s, h)
}

fn width(s: &mut Session, h: Handle) -> String {
    let r = s.execute("blocks.reference", &json!({ "handle": h.hex() })).unwrap();
    let p = r["properties"].as_array().unwrap().iter().find(|p| p["name"] == "Width").cloned().unwrap_or(Value::Null);
    p["value"].as_str().unwrap_or_default().to_string()
}

#[test]
fn grips_are_listed_and_dragged_in_one_undo_step() {
    let (mut s, h) = session();
    let g = s.execute("blocks.reference.grips", &json!({ "handle": h.hex() })).unwrap();
    let parts: Vec<(i64, &str, &str)> =
        g.as_array().unwrap().iter().map(|g| (g["node"].as_i64().unwrap(), g["part"].as_str().unwrap(), g["shape"].as_str().unwrap())).collect();
    assert_eq!(parts, [(1, "at", "list"), (2, "end", "linear")]);
    assert_eq!(g[1]["at"], json!([136.0, -4.0]));
    s.execute("blocks.reference.grip", &json!({ "handle": h.hex(), "node": 2, "part": "end", "to": [130.4, 7] })).unwrap();
    assert_eq!(width(&mut s, h), "30.0000");
    s.undo().unwrap();
    assert_eq!(width(&mut s, h), "36.0000");
    for bad in [
        json!({ "handle": h.hex(), "node": 2, "part": "middle", "to": [130, 0] }),
        json!({ "handle": h.hex(), "node": 2, "part": "end" }),
        json!({ "handle": h.hex(), "part": "end", "to": [130, 0] }),
        json!({ "handle": h.hex(), "node": 1, "part": "at", "to": [130, 0] }),
        json!({ "handle": h.hex(), "node": 2, "part": "end", "to": [10, 0] }),
        json!({ "handle": "FFFFF", "node": 2, "part": "end", "to": [130, 0] }),
    ] {
        assert!(s.execute("blocks.reference.grip", &bad).is_err(), "{bad}");
    }
    assert_eq!(width(&mut s, h), "36.0000");
}

#[test]
fn a_grip_drag_previews_the_reference_and_takes_a_point_or_a_distance() {
    let (mut s, h) = session();
    s.start_dyn_grip(h, 2, GripPart::End).unwrap();
    // The preview is the reference re-evaluated at the cursor, and the rubber band.
    assert!(s.preview(Vec2::new(132.0, 0.0)).len() > 1);
    s.input(Input::Point(Vec2::new(132.2, 3.0))).unwrap();
    assert!(s.running.is_none());
    assert_eq!(width(&mut s, h), "32.0000");
    // A typed distance (snapped to the list).
    s.start_dyn_grip(h, 2, GripPart::End).unwrap();
    s.input(Input::Text("29".into())).unwrap();
    assert_eq!(width(&mut s, h), "30.0000");
    // A list grip offers its choices instead; a grip that isn't there is refused.
    assert!(s.start_dyn_grip(h, 1, GripPart::At).is_err());
    assert!(s.start_dyn_grip(h, 2, GripPart::Base).is_err());
}

#[test]
fn a_tilted_references_grips_show_preview_and_drag_where_plan_view_shows_it() {
    // Tilted back (extrusion (0,-0.6,0.8)): its own (x, y) shows in plan at (x, 0.8 y).
    let (mut flat, fh) = session();
    let (mut s, h) = session();
    s.doc_mut().unwrap().modify_entity(h, |e| e.common.extrusion = Vec3::new(0.0, -0.6, 0.8)).unwrap();
    let g = s.execute("blocks.reference.grips", &json!({ "handle": h.hex() })).unwrap();
    let at = Vec2::new(g[1]["at"][0].as_f64().unwrap(), g[1]["at"][1].as_f64().unwrap());
    assert!(at.near(Vec2::new(136.0, -3.2), 1e-9), "{at:?}");
    // The drag previews the reference through its plane: the flat one's preview, y × 0.8.
    flat.start_dyn_grip(fh, 2, GripPart::End).unwrap();
    s.start_dyn_grip(h, 2, GripPart::End).unwrap();
    let ends = |s: &Session, c: Vec2| -> Vec<Vec2> {
        s.preview(c)
            .iter()
            .flat_map(|e| match &e.kind {
                EntityKind::Line(l) => vec![l.a.xy(), l.b.xy()],
                _ => Vec::new(),
            })
            .collect()
    };
    let (a, b) = (ends(&flat, Vec2::new(130.4, 7.0)), ends(&s, Vec2::new(130.4, 5.6)));
    assert!(a.len() > 2 && a.len() == b.len(), "{a:?} {b:?}");
    for (p, q) in a.iter().zip(&b) {
        assert!(q.near(Vec2::new(p.x, 0.8 * p.y), 1e-9), "{p:?} {q:?}");
    }
    // Dropped at the plan point over (130.4, 7) of its plane: Width 30, as the flat one there.
    s.input(Input::Point(Vec2::new(130.4, 5.6))).unwrap();
    assert_eq!(width(&mut s, h), "30.0000");
}
