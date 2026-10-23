//! INTERFERE and -INTERFERE.

use super::*;

fn err(r: Result<Value>) -> String {
    r.err().map(|e| e.to_string()).unwrap_or_default()
}

#[test]
fn hostile_parameters_are_refused() {
    let mut s = Session::new();
    let c = s.add_entity(EntityKind::Circle(cadcraft_doc::Circle { center: cadcraft_geom::Vec3::ZERO, radius: 5.0 })).unwrap();
    for p in [
        json!({}),
        json!({ "first": [c.hex()] }),
        json!({ "first": "all" }),
        json!({ "first": [true] }),
        json!({ "second": 7 }),
        json!({ "layer": "" }),
    ] {
        assert!(s.execute("interfere", &p).is_err(), "{p}");
    }
    // One solid alone can't interfere.
    let x = s.add_entity(EntityKind::Solid3d(Solid3d::default())).unwrap();
    assert!(err(s.execute("interfere", &json!({ "first": [x.hex()] }))).contains("at least two"));
    assert!(err(s.execute("interfere", &json!({ "first": [x.hex()], "second": [x.hex()] }))).contains("at least two"), "a solid against itself");
    assert_eq!(s.doc().unwrap().model.iter().count(), 2);
}

#[test]
fn prompts_follow_autocad() {
    let mut s = Session::new();
    let msg = |s: &Session| s.current_prompt().map(|p| p.message).unwrap_or_default();
    let c = s.add_entity(EntityKind::Circle(cadcraft_doc::Circle { center: cadcraft_geom::Vec3::ZERO, radius: 5.0 })).unwrap();
    s.start("inf").unwrap();
    assert_eq!(s.running.as_ref().map(|r| r.id.as_str()), Some("interfere"));
    assert!(msg(&s).contains("first set of objects"));
    s.input(Input::Pick(vec![c])).unwrap();
    s.input(Input::Enter).unwrap();
    let p = s.current_prompt().unwrap();
    assert!(p.message.contains("second set of objects"));
    assert_eq!(p.keywords, vec!["checK first set"]);
    // K checks the first set; a circle is no solid, so it is asked again.
    let _ = s.input(Input::Text("k".into()));
    assert!(s.log.iter().any(|l| l.contains("select 3D solids")), "{:?}", s.log);
    s.cancel();
}

#[cfg(feature = "occt")]
mod kernel {
    use cadcraft_doc::Handle;

    use super::*;

    fn boxed(s: &mut Session, a: [f64; 3], b: [f64; 3], name: &str) -> Handle {
        let r = s.execute("box", &json!({ "corner1": a, "corner2": b })).unwrap();
        let h = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
        s.execute("part", &json!({ "handles": [h.hex()], "name": name })).unwrap();
        h
    }

    /// A carcass side 18 × 400 × 700 and a shelf 600 × 380 × 18 that runs 10 into it; a back
    /// panel that only touches the side.
    #[test]
    fn colliding_parts_are_found_measured_and_made() {
        let mut s = Session::new();
        let side = boxed(&mut s, [0.0, 0.0, 0.0], [18.0, 400.0, 700.0], "Side");
        let shelf = boxed(&mut s, [8.0, 10.0, 300.0], [608.0, 390.0, 318.0], "Shelf");
        let back = boxed(&mut s, [18.0, 400.0, 0.0], [600.0, 408.0, 700.0], "Back");
        let r = s.execute("interfere", &json!({ "first": [side.hex(), shelf.hex(), back.hex()] })).unwrap();
        let pairs = r["pairs"].as_array().unwrap();
        assert_eq!(pairs.len(), 1, "touching isn't interfering: {r}");
        assert_eq!((pairs[0]["first"].as_str(), pairs[0]["second"].as_str()), (Some(side.hex().as_str()), Some(shelf.hex().as_str())));
        let v = 10.0 * 380.0 * 18.0;
        assert!((pairs[0]["volume"].as_f64().unwrap() - v).abs() < 1e-3 * v, "{r}");
        assert!(pairs[0]["firstName"].as_str().unwrap().starts_with("Side ["));
        let made = Handle::parse_hex(pairs[0]["handle"].as_str().unwrap()).unwrap();
        let d = s.doc().unwrap();
        let e = d.entity(made).unwrap();
        assert_eq!(e.common.layer, "INTERFERENCE");
        assert_eq!(d.layer("INTERFERENCE").unwrap().color, Color::Index(1));
        assert!(matches!(e.kind, EntityKind::Solid3d(_)));
        s.undo().unwrap();
        assert!(s.doc().unwrap().entity(made).is_none());
        // Two sets, nothing made.
        let r = s.execute("interfere", &json!({ "first": [shelf.hex()], "second": [side.hex(), back.hex()], "create": false })).unwrap();
        assert_eq!((r["pairs"].as_array().unwrap().len(), r["handles"].as_array().unwrap().len()), (1, 0), "{r}");
        assert_eq!(r["pairs"][0]["first"].as_str(), Some(shelf.hex().as_str()));
    }

    #[test]
    fn interfere_at_the_prompts_deletes_or_keeps_what_it_made() {
        let mut s = Session::new();
        let a = boxed(&mut s, [0.0, 0.0, 0.0], [10.0, 10.0, 10.0], "A");
        let b = boxed(&mut s, [5.0, 5.0, 5.0], [15.0, 15.0, 15.0], "B");
        let count = |s: &Session| s.doc().unwrap().model.iter().count();
        s.set_selection(Vec::new());
        s.start("interfere").unwrap();
        s.input(Input::Pick(vec![a, b])).unwrap();
        s.input(Input::Enter).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(s.log.iter().any(|l| l.contains("A [") && l.contains("volume 125")), "{:?}", s.log);
        assert!(s.log.iter().any(|l| l == "Interfering pairs: 1"), "{:?}", s.log);
        assert_eq!(count(&s), 3, "made and selected while asked");
        assert_eq!(s.selection().len(), 1);
        assert!(s.current_prompt().unwrap().message.contains("Delete interference objects"));
        s.input(Input::Enter).unwrap();
        assert_eq!(count(&s), 2, "deleted on Enter");
        // -INTERFERE: made only when asked.
        s.start("-interfere").unwrap();
        s.input(Input::Pick(vec![a])).unwrap();
        s.input(Input::Enter).unwrap();
        s.input(Input::Pick(vec![b])).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(s.current_prompt().unwrap().message.contains("Create interference solids"));
        assert_eq!(count(&s), 2);
        s.input(Input::Keyword("Yes".into())).unwrap();
        assert_eq!(count(&s), 3);
    }
}
