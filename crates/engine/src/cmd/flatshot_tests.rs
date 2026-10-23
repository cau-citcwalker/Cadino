//! FLATSHOT, SOLVIEW and SOLDRAW.

use super::*;

fn err(r: Result<Value>) -> String {
    r.err().map(|e| e.to_string()).unwrap_or_default()
}

#[test]
fn hostile_parameters_are_refused() {
    let mut s = Session::new();
    for p in [
        json!({ "destination": "printer" }),
        json!({ "destination": 3 }),
        json!({ "destination": "replace" }),
        json!({ "destination": "replace", "block": "nothing here" }),
        json!({ "destination": "export" }),
        json!({ "view": "sideways" }),
        json!({ "view": [0, 0, 0] }),
        json!({ "at": "q" }),
        json!({ "hiddenLayer": "a|b" }),
        // No solids.
        json!({}),
    ] {
        assert!(s.execute("flatshot", &p).is_err(), "{p}");
    }
    assert!(err(s.execute("flatshot", &json!({}))).contains("no 3D solids"));
    assert!(s.doc().unwrap().blocks.keys().all(|k| !k.starts_with("FLATSHOT")));
    // With file access denied (the AI assistant's setting), the export and 3DPRINT write nothing.
    let _denied = crate::limits::deny_files("file access is off");
    let path = std::env::temp_dir().join("cadcraft-denied-flatshot.dxf");
    assert!(err(s.execute("flatshot", &json!({ "destination": "export", "path": path.to_string_lossy() }))).contains("file access is off"));
    assert!(err(s.execute("3dprint", &json!({ "path": path.with_extension("stl").to_string_lossy() }))).contains("file access is off"));
    assert!(!path.exists() && !path.with_extension("stl").exists());
}

#[test]
fn solview_and_soldraw_run_viewbase() {
    let mut s = Session::new();
    // VIEWBASE's own refusal: there is nothing to draw.
    assert!(err(s.execute("solview", &json!({}))).contains("there are no 3D solids"));
    s.start("soldraw").unwrap();
    assert!(s.log.iter().any(|l| l.contains("VIEWBASE")), "{:?}", s.log);
    assert_eq!(s.running.as_ref().map(|r| r.id.as_str()), Some("soldraw"));
    s.cancel();
}

#[test]
fn prompts() {
    let mut s = Session::new();
    let msg = |s: &Session| s.current_prompt().map(|p| p.message).unwrap_or_default();
    s.start("flatshot").unwrap();
    assert_eq!(s.current_prompt().unwrap().keywords, vec!["Insert", "Replace", "Export"]);
    s.input(Input::Keyword("Replace".into())).unwrap();
    assert!(msg(&s).contains("block to replace"));
    let _ = s.input(Input::Text("NOPE".into()));
    assert!(msg(&s).contains("block to replace"), "a missing block is asked again");
    s.cancel();
    s.start("flatshot").unwrap();
    s.input(Input::Enter).unwrap();
    assert!(msg(&s).contains("obscured lines"));
    s.input(Input::Enter).unwrap();
    assert!(msg(&s).contains("insertion point"));
    s.cancel();
}

#[cfg(feature = "occt")]
mod kernel {
    use cadcraft_doc::{EntityKind, Handle};
    use cadcraft_geom::Bounds2;

    use super::*;

    fn handle(v: &Value) -> Handle {
        Handle::parse_hex(v.as_str().unwrap()).unwrap()
    }

    /// A 100 × 50 × 30 box with a hole of radius 10 down through it.
    fn holed_box(s: &mut Session) {
        let b = handle(&s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [100, 50, 30] })).unwrap()["handle"]);
        let c = handle(&s.execute("cylinder", &json!({ "center": [50, 25, -1], "radius": 10, "height": 32 })).unwrap()["handle"]);
        s.execute("subtract", &json!({ "from": [b.hex()], "remove": [c.hex()] })).unwrap();
    }

    fn block_bounds(s: &Session, name: &str) -> Bounds2 {
        let d = s.doc().unwrap();
        d.block(name).unwrap().entities.iter().fold(Bounds2::EMPTY, |b, e| b.union(&cadcraft_doc::entity_bounds(d, e, 0)))
    }

    #[test]
    fn flat_views_insert_replace_and_export() {
        let mut s = Session::new();
        holed_box(&mut s);
        // From the front with obscured lines: the hole's sides are hidden, dashed.
        let r = s.execute("flatshot", &json!({ "view": "front", "hidden": true, "at": [300, 0] })).unwrap();
        let name = r["block"].as_str().unwrap().to_string();
        assert!(r["visible"].as_u64().unwrap() >= 4 && r["hidden"].as_u64().unwrap() >= 2, "{r}");
        let b = block_bounds(&s, &name);
        assert!(b.min.near(Vec2::new(0.0, 0.0), 1e-6) && b.max.near(Vec2::new(100.0, 30.0), 1e-6), "{b:?}");
        let d = s.doc().unwrap();
        let blk = d.block(&name).unwrap();
        assert!(blk.entities.iter().any(|e| e.common.linetype == "HIDDEN"), "obscured lines are dashed");
        assert!(d.linetype("HIDDEN").is_some(), "the linetype is loaded");
        let EntityKind::Insert(i) = &d.entity(handle(&r["handle"])).unwrap().kind else { panic!("not a block reference") };
        assert!(i.insert.xy().near(Vec2::new(300.0, 0.0), 1e-9));
        // From the top into the same block: its references show the new view.
        let top = s.execute("flatshot", &json!({ "view": "top", "destination": "replace", "block": name })).unwrap();
        assert!(top["handle"].is_null() && top["hidden"] == 0, "{top}");
        let b = block_bounds(&s, &name);
        assert!(b.max.near(Vec2::new(100.0, 50.0), 1e-6), "{b:?}");
        let blk = s.doc().unwrap().block(&name).unwrap().clone();
        assert!(blk.entities.iter().any(|e| matches!(e.kind, EntityKind::Circle(_))), "the hole seen from above");
        // Export: a file (when the test binary's io hooks write one: they are process-wide and
        // other tests install failing ones), and the drawing as it was either way.
        let dir = std::env::temp_dir().join(format!("cadino-flatshot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("front.dxf");
        let before = s.doc().unwrap().blocks.len();
        match s.execute("flatshot", &json!({ "view": "front", "destination": "export", "path": path.to_string_lossy() })) {
            Ok(x) => {
                assert!(x["message"].as_str().unwrap().contains("front.dxf"));
                assert!(path.exists());
            }
            Err(e) => assert!(e.to_string().contains("wblock") || e.to_string().contains("write"), "{e}"),
        }
        assert_eq!(s.doc().unwrap().blocks.len(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
