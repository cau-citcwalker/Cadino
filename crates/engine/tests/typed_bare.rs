//! Commands typed bare (or chosen from their menu) start their command-line prompts instead of
//! failing with a JSON "`x` is required" error: LAYMCUR, LAYMCH, LAYISO, CLAYER, BASE, -WBLOCK,
//! GETVAR, BREAKATPOINT, REGION, STEPOUT/STEPIN/STLOUT, DATALINK and LISP. The JSON forms are
//! unchanged.

use cadcraft_engine::cmd::file::{IoHooks, set_io};
use cadcraft_engine::doc::{EntityKind, Handle};
use cadcraft_engine::{Input, Session, find_command};
use serde_json::json;

fn session() -> Session {
    let mut s = Session::new();
    s.new_drawing(true);
    s
}

fn line(s: &mut Session, a: [f64; 2], b: [f64; 2]) -> Handle {
    let v = s.execute("line", &json!({ "points": [a, b] })).unwrap();
    Handle::parse_hex(v["handles"][0].as_str().unwrap()).unwrap()
}

fn layer_of(s: &Session, h: Handle) -> String {
    s.doc().unwrap().entity(h).unwrap().common.layer.clone()
}

fn clayer(s: &Session) -> String {
    s.doc().unwrap().header.str("CLAYER", "0")
}

/// The prompt as shown after the command name.
fn prompt(s: &Session) -> String {
    s.current_prompt().map_or_else(|| "Command:".into(), |p| p.display())
}

fn logged(s: &Session, text: &str) -> bool {
    s.log.iter().any(|l| l.contains(text))
}

/// Lines on layer A and on layer 0.
fn two_layers(s: &mut Session) -> (Handle, Handle) {
    s.execute("layer.new", &json!({ "name": "A", "current": true })).unwrap();
    let a = line(s, [0.0, 0.0], [10.0, 0.0]);
    s.execute("layer.current", &json!({ "name": "0" })).unwrap();
    let z = line(s, [0.0, 5.0], [10.0, 5.0]);
    (a, z)
}

#[test]
fn laymcur_asks_for_an_object_and_the_menu_runs_it() {
    let mut s = session();
    let (a, _) = two_layers(&mut s);
    s.cmdline("laymcur").unwrap();
    assert_eq!(prompt(&s), "Select object whose layer will become current:");
    s.input(Input::Pick(vec![a])).unwrap();
    assert!(s.running.is_none());
    assert_eq!(clayer(&s), "A");
    assert!(logged(&s, "A is now the current layer."), "{:?}", s.log);
    // Format > Layer Tools > Make Current is LAYMCUR; layer.current stays programmatic.
    assert_eq!(find_command("laymcur").unwrap().menu, ["Format", "Layer Tools", "Make Current"]);
    assert!(find_command("layer.current").unwrap().menu.is_empty());
    s.execute("layer.current", &json!({ "name": "0" })).unwrap();
    assert_eq!(clayer(&s), "0");
}

#[test]
fn laymch_by_object_and_by_name() {
    let mut s = session();
    let (a, z) = two_layers(&mut s);
    s.execute("layer.new", &json!({ "name": "B" })).unwrap();
    s.cmdline("laymch").unwrap();
    assert_eq!(prompt(&s), "Select objects to be changed:");
    s.input(Input::Pick(vec![z])).unwrap();
    s.input(Input::Enter).unwrap();
    assert_eq!(prompt(&s), "Select object on destination layer or [Name]:");
    s.input(Input::Pick(vec![a])).unwrap();
    assert!(s.running.is_none());
    assert_eq!(layer_of(&s, z), "A");
    assert!(logged(&s, "1 object changed to layer \"A\"."), "{:?}", s.log);

    // Name: a layer that doesn't exist is asked for again.
    s.cmdline("laymch").unwrap();
    s.input(Input::Pick(vec![a, z])).unwrap();
    s.input(Input::Enter).unwrap();
    s.cmdline("n").unwrap();
    assert_eq!(prompt(&s), "Enter layer name:");
    s.cmdline("Nope").unwrap();
    assert!(logged(&s, "Cannot find layer \"Nope\"."));
    assert_eq!(prompt(&s), "Enter layer name:");
    s.cmdline("b").unwrap();
    assert!(s.running.is_none());
    assert_eq!((layer_of(&s, a), layer_of(&s, z)), ("B".into(), "B".into()));
    assert!(s.doc().unwrap().layer("Nope").is_none());

    // JSON form unchanged.
    s.execute("laymch", &json!({ "handles": [a.hex()], "layer": "0" })).unwrap();
    assert_eq!(layer_of(&s, a), "0");
}

#[test]
fn layiso_shows_its_setting_and_asks_for_objects() {
    let mut s = session();
    let (a, _) = two_layers(&mut s);
    s.cmdline("layiso").unwrap();
    assert!(logged(&s, "Current setting: Hide layers"));
    assert_eq!(prompt(&s), "Select objects on the layer(s) to be isolated or [Settings]:");
    s.cmdline("s").unwrap();
    assert!(s.running.is_some(), "Settings keeps prompting");
    s.input(Input::Pick(vec![a])).unwrap();
    s.input(Input::Enter).unwrap();
    assert!(s.running.is_none());
    let d = s.doc().unwrap();
    assert!(d.layer("A").unwrap().on && !d.layer("0").unwrap().on);
    assert!(logged(&s, "have been isolated"));

    // With a pickfirst selection it isolates at once.
    s.execute("layuniso", &json!({})).unwrap();
    s.set_selection(vec![a]);
    s.cmdline("layiso").unwrap();
    assert!(s.running.is_none());
    assert!(!s.doc().unwrap().layer("0").unwrap().on);
}

#[test]
fn clayer_typed_is_the_system_variable() {
    let mut s = session();
    s.execute("layer.new", &json!({ "name": "A" })).unwrap();
    s.cmdline("clayer").unwrap();
    assert_eq!(prompt(&s), "Enter new value for CLAYER <\"0\">:");
    s.cmdline("A").unwrap();
    assert!(s.running.is_none());
    assert_eq!(clayer(&s), "A");
}

#[test]
fn getvar_asks_for_the_name_and_shows_the_value() {
    let mut s = session();
    s.cmdline("getvar").unwrap();
    assert_eq!(prompt(&s), "Enter variable name or [?]:");
    s.cmdline("orthomode").unwrap();
    assert!(s.running.is_none());
    assert!(logged(&s, "ORTHOMODE = 0"), "{:?}", s.log);
    assert_eq!(s.execute("getvar", &json!({ "name": "ORTHOMODE" })).unwrap()["value"], json!(0));
}

#[test]
fn base_asks_for_the_point_with_the_current_one() {
    let mut s = session();
    s.cmdline("base").unwrap();
    assert_eq!(prompt(&s), "Enter base point <0.0000,0.0000,0.0000>:");
    s.cmdline("3,4").unwrap();
    assert!(s.running.is_none());
    let p = s.doc().unwrap().header.point("INSBASE").unwrap();
    assert_eq!((p.x, p.y), (3.0, 4.0));
    s.cmdline("base").unwrap();
    assert_eq!(prompt(&s), "Enter base point <3.0000,4.0000,0.0000>:");
    s.cmdline("").unwrap();
    assert!(s.running.is_none());
    assert_eq!(s.doc().unwrap().header.point("INSBASE").unwrap().x, 3.0);
}

#[test]
fn breakatpoint_selects_then_breaks() {
    let mut s = session();
    line(&mut s, [0.0, 0.0], [10.0, 0.0]);
    s.cmdline("breakatpoint").unwrap();
    assert_eq!(prompt(&s), "Select object:");
    s.cmdline("5,0").unwrap();
    assert_eq!(prompt(&s), "Specify break point:");
    s.cmdline("4,0").unwrap();
    assert!(s.running.is_none());
    let mut ends: Vec<f64> = s
        .doc()
        .unwrap()
        .model
        .iter()
        .map(|e| match &e.kind {
            EntityKind::Line(l) => l.a.x.max(l.b.x),
            _ => -1.0,
        })
        .collect();
    ends.sort_by(f64::total_cmp);
    assert_eq!(ends, [4.0, 10.0], "two lines meeting at the break point");
}

#[test]
fn region_asks_for_objects() {
    let mut s = session();
    let r = s.execute("rectang", &json!({ "p1": [0, 0], "p2": [4, 3] })).unwrap();
    let h = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
    s.cmdline("reg").unwrap();
    assert_eq!(prompt(&s), "Select objects:");
    s.input(Input::Pick(vec![h])).unwrap();
    s.input(Input::Enter).unwrap();
    assert!(s.running.is_none());
    assert!(matches!(s.doc().unwrap().entity(h).unwrap().kind, EntityKind::Region(_)));
    assert!(logged(&s, "1 loop(s) extracted. 1 Region(s) created."), "{:?}", s.log);
    // Nothing selected and no objects picked: nothing happens.
    s.cmdline("region").unwrap();
    s.input(Input::Enter).unwrap();
    assert!(s.running.is_none());
}

#[test]
fn file_exchange_commands_ask_for_files() {
    let mut s = session();
    for (cmd, want) in [
        ("stepout", "Enter name of STEP file to write:"),
        ("stepin", "Enter name of STEP file to import:"),
        ("stlout", "Select solids or watertight meshes:"),
        ("lisp", "Enter LISP expression:"),
    ] {
        s.cmdline(cmd).unwrap();
        assert_eq!(prompt(&s), want, "{cmd}");
        s.cancel();
        assert!(s.running.is_none());
    }
    // STLOUT asks for the file once the solids are chosen.
    let l = line(&mut s, [0.0, 0.0], [1.0, 0.0]);
    s.cmdline("stlout").unwrap();
    s.input(Input::Pick(vec![l])).unwrap();
    s.input(Input::Enter).unwrap();
    assert_eq!(prompt(&s), "Enter name of STL file to write:");
    s.cancel();
}

#[test]
fn lisp_evaluates_the_expression() {
    let mut s = session();
    s.cmdline("lisp").unwrap();
    s.cmdline("(+ 1 2)").unwrap();
    assert!(s.running.is_none());
    assert!(s.log.iter().any(|l| l.trim() == "3"), "{:?}", s.log);
    let v = s.execute("lisp", &json!({ "code": "(* 2 3)" })).unwrap();
    assert_eq!(v["value"], "6");
}

#[test]
fn datalink_selects_a_table_then_the_file() {
    let mut s = session();
    let t = s.execute("table", &json!({ "at": [0, 0], "rows": 2, "cols": 2 })).unwrap();
    let h = Handle::parse_hex(t["handle"].as_str().unwrap()).unwrap();
    let csv = std::env::temp_dir().join(format!("cadcraft-typed-datalink-{}.csv", std::process::id()));
    std::fs::write(&csv, "a,b\n1,2\n").unwrap();
    s.cmdline("datalink").unwrap();
    assert_eq!(prompt(&s), "Select a table:");
    s.input(Input::Pick(vec![h])).unwrap();
    assert_eq!(prompt(&s), "Enter name of CSV file to link:");
    s.cmdline(&csv.to_string_lossy()).unwrap();
    assert!(s.running.is_none());
    assert!(logged(&s, "Linked"), "{:?}", s.log);
    match &s.doc().unwrap().entity(h).unwrap().kind {
        EntityKind::Table(t) => assert!(t.data_link.is_some()),
        _ => panic!("not a table"),
    }
    // A selected table needs no picking.
    s.set_selection(vec![h]);
    s.cmdline("datalink").unwrap();
    assert_eq!(prompt(&s), "Enter name of CSV file to link:");
    s.cancel();
    let _ = std::fs::remove_file(&csv);
}

#[test]
fn dash_wblock_prompts_write_objects_blocks_and_the_drawing() {
    set_io(IoHooks { read: |_, _| Err("no reader in this test".into()), write: |d, _| Ok(format!("{}", d.model.len()).into_bytes()), plot: None });
    let dir = std::env::temp_dir().join(format!("cadcraft-typed-wblock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = session();
    let a = line(&mut s, [0.0, 0.0], [10.0, 0.0]);
    line(&mut s, [0.0, 5.0], [10.0, 5.0]);

    // Enter: a new drawing from a base point and objects; `.dwg` is added to a bare name.
    let part = dir.join("part");
    s.cmdline("-wblock").unwrap();
    assert_eq!(prompt(&s), "Enter name of output file:");
    s.cmdline(&part.to_string_lossy()).unwrap();
    assert_eq!(prompt(&s), "Enter name of existing block or [= (block=output file)/* (whole drawing)] <define new drawing>:");
    s.cmdline("").unwrap();
    assert_eq!(prompt(&s), "Specify insertion base point:");
    s.cmdline("0,0").unwrap();
    assert_eq!(prompt(&s), "Select objects:");
    s.input(Input::Pick(vec![a])).unwrap();
    s.input(Input::Enter).unwrap();
    assert!(s.running.is_none());
    assert_eq!(std::fs::read_to_string(dir.join("part.dwg")).unwrap(), "1");
    assert_eq!(s.doc().unwrap().model.len(), 2, "the objects stay");

    // An existing file is replaced only on Yes; `*` writes the whole drawing.
    s.cmdline("-w").unwrap();
    s.cmdline(&dir.join("part.dwg").to_string_lossy()).unwrap();
    assert_eq!(prompt(&s), "A drawing with this name already exists. Do you want to replace it? [Yes/No] <N>:");
    s.cmdline("y").unwrap();
    s.cmdline("*").unwrap();
    assert!(s.running.is_none());
    assert_eq!(std::fs::read_to_string(dir.join("part.dwg")).unwrap(), "2");

    // `=`: the block named like the file; a missing block is asked for again.
    s.execute("block", &json!({ "name": "Door", "base": [0, 0], "handles": [a.hex()], "keep": "retain" })).unwrap();
    s.cmdline("wblock").unwrap();
    s.cmdline(&dir.join("Door.dxf").to_string_lossy()).unwrap();
    s.cmdline("Nope").unwrap();
    assert!(logged(&s, "Block \"Nope\" not found."));
    s.cmdline("=").unwrap();
    assert!(s.running.is_none());
    assert_eq!(std::fs::read_to_string(dir.join("Door.dxf")).unwrap(), "1");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "occt")]
#[test]
fn typed_step_and_stl_files_are_written_and_read() {
    let dir = std::env::temp_dir().join(format!("cadcraft-typed-step-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (stp, stl) = (dir.join("box.stp"), dir.join("box.stl"));
    let mut s = session();
    let b = s.execute("box", &json!({ "corner1": [0, 0], "corner2": [10, 20], "height": 5 })).unwrap();
    let b = Handle::parse_hex(b["handle"].as_str().unwrap()).unwrap();
    s.cmdline("stepout").unwrap();
    s.cmdline(&stp.to_string_lossy()).unwrap();
    assert!(s.running.is_none() && stp.exists());
    assert!(logged(&s, "Exported 1 solid(s)"), "{:?}", s.log);
    s.cmdline("stlout").unwrap();
    s.input(Input::Pick(vec![b])).unwrap();
    s.input(Input::Enter).unwrap();
    s.cmdline(&stl.to_string_lossy()).unwrap();
    assert!(s.running.is_none() && stl.exists());

    let mut t = session();
    t.cmdline("stepin").unwrap();
    t.cmdline(&stp.to_string_lossy()).unwrap();
    assert!(t.running.is_none());
    assert!(t.doc().unwrap().model.iter().any(|e| matches!(e.kind, EntityKind::Solid3d(_))));
    assert!(logged(&t, "Imported 1 solid"), "{:?}", t.log);
    let _ = std::fs::remove_dir_all(&dir);
}
