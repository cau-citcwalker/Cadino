//! Command-line forms and names AutoCAD users type: -OSNAP, -LAYERSTATE, QLEADER (LE), FILL,
//! QTEXT, UCSICON, the TH/TI system variable aliases, CLAYER's checks, one-line scripts with
//! fonts and file names, the shown command names, and the file prompts' file dialog hint.

use cadcraft_engine::cmd::file::{IoHooks, set_io};
use cadcraft_engine::doc::EntityKind;
use cadcraft_engine::{Input, Picker, Session, sysvars};
use serde_json::json;

fn session() -> Session {
    let mut s = Session::new();
    s.new_drawing(true);
    s
}

fn logged(s: &Session, text: &str) -> bool {
    s.log.iter().any(|l| l.contains(text))
}

fn var(s: &Session, name: &str) -> i64 {
    sysvars::get(s, name).and_then(|v| v.as_i64()).unwrap_or(-1)
}

#[test]
fn dash_osnap_sets_the_running_object_snaps() {
    use cadcraft_engine::snap::mode;
    let mut s = session();
    s.cmdline("-OSNAP").unwrap();
    assert!(logged(&s, "Current osnap modes: End,Cen,Int,Ext"), "{:?}", s.log);
    assert_eq!(s.prompt_text(), "-OSNAP Enter list of object snap modes:");
    s.cmdline("end,MIDpoint,per").unwrap();
    assert!(s.running.is_none());
    assert_eq!(s.settings.osmode, mode::END | mode::MID | mode::PER);
    // A name it doesn't know asks again; NONe turns them all off; -OS is -OSNAP.
    s.cmdline("-os").unwrap();
    s.cmdline("end,bogus").unwrap();
    assert!(logged(&s, "Unknown object snap mode \"bogus\"."));
    assert!(s.running.is_some());
    s.cmdline("none").unwrap();
    assert_eq!(s.settings.osmode, 0);
    // Enter keeps them; the JSON form is OSNAP's.
    s.settings.osmode = mode::CEN;
    s.cmdline("-osnap").unwrap();
    s.input(Input::Enter).unwrap();
    assert_eq!(s.settings.osmode, mode::CEN);
    s.execute("-osnap", &json!({ "modes": ["end"] })).unwrap();
    assert_eq!(s.settings.osmode, mode::END);
}

#[test]
fn dash_layerstate_saves_lists_restores_renames_and_deletes() {
    let mut s = session();
    s.execute("layer.new", &json!({ "name": "Walls", "color": 1 })).unwrap();
    s.cmdline("-layerstate").unwrap();
    assert_eq!(s.prompt_text(), "-LAYERSTATE Enter an option [?/Save/Restore/Name/Delete]:");
    s.cmdline("s").unwrap();
    s.cmdline("Plan").unwrap();
    assert!(logged(&s, "Layer state \"Plan\" saved."));
    // Back at the options: freeze Walls, then restore by any case.
    s.execute("layer.set", &json!({ "name": "Walls", "frozen": true })).unwrap();
    s.cmdline("r").unwrap();
    s.cmdline("nosuch").unwrap();
    assert!(logged(&s, "Cannot find layer state \"nosuch\"."));
    assert!(s.prompt_text().contains("restore"), "a refused name asks again: {}", s.prompt_text());
    s.cmdline("PLAN").unwrap();
    assert!(!s.doc().unwrap().layer("Walls").unwrap().frozen);
    s.cmdline("n").unwrap();
    s.cmdline("plan").unwrap();
    s.cmdline("Section").unwrap();
    s.cmdline("?").unwrap();
    s.input(Input::Enter).unwrap();
    assert!(logged(&s, "  Section"));
    s.cmdline("d").unwrap();
    s.cmdline("section").unwrap();
    assert!(s.doc().unwrap().layer_states.is_empty());
    s.input(Input::Enter).unwrap();
    assert!(s.running.is_none());
    // The JSON form lists them.
    assert_eq!(s.execute("-layerstate", &json!({})).unwrap()["states"], json!([]));
}

#[test]
fn qleader_and_le_take_three_points_a_width_and_the_annotation() {
    for name in ["QLEADER", "le"] {
        let mut s = session();
        s.cmdline(name).unwrap();
        assert_eq!(s.prompt_text(), "QLEADER Specify first leader point, or [Settings] <Settings>:");
        s.cmdline("0,0").unwrap();
        assert_eq!(s.prompt_text(), "QLEADER Specify next point:");
        s.cmdline("10,10").unwrap();
        s.cmdline("20,10").unwrap();
        // The third point ends the points (QLEADER's Maximum Points).
        assert_eq!(s.prompt_text(), "QLEADER Specify text width <0.0000>:");
        s.cmdline("40").unwrap();
        assert_eq!(s.prompt_text(), "QLEADER Enter first line of annotation text <Mtext>:");
        s.cmdline("DOOR D1").unwrap();
        assert_eq!(s.prompt_text(), "QLEADER Enter next line of annotation text:");
        s.input(Input::Enter).unwrap();
        assert!(s.running.is_none());
        let d = s.doc().unwrap();
        let kinds: Vec<&EntityKind> = d.model.iter().map(|e| &e.kind).collect();
        assert!(matches!(kinds[0], EntityKind::Leader(l) if l.vertices.len() == 3 && l.arrow), "{name}");
        assert!(matches!(kinds[1], EntityKind::MText(m) if m.contents == "DOOR D1" && m.width == 40.0), "{name}");
    }
    // Enter after two points ends them; Enter at the first line leaves the leader alone.
    let mut s = session();
    s.cmdline("qleader").unwrap();
    s.cmdline("").unwrap();
    assert!(logged(&s, "Leader Settings dialog is not available yet"));
    s.cmdline("0,0").unwrap();
    s.cmdline("5,5").unwrap();
    s.cmdline("").unwrap();
    s.cmdline("").unwrap();
    s.cmdline("").unwrap();
    assert!(s.running.is_none());
    assert_eq!(s.doc().unwrap().model.len(), 1);
}

#[test]
fn fill_qtext_and_ucsicon_set_their_modes() {
    let mut s = session();
    for (name, var_name, shown) in
        [("fill", "FILLMODE", "FILL Enter mode [ON/OFF] <ON>:"), ("ucsicon", "UCSICON", "UCSICON Enter an option [ON/OFF] <ON>:")]
    {
        s.cmdline(name).unwrap();
        assert_eq!(s.prompt_text(), shown);
        s.cmdline("off").unwrap();
        assert_eq!(var(&s, var_name), 0, "{name}");
        s.cmdline(name).unwrap();
        s.cmdline("ON").unwrap();
        assert_eq!(var(&s, var_name), 1, "{name}");
    }
    s.cmdline("qtext").unwrap();
    assert_eq!(s.prompt_text(), "QTEXT Enter mode [ON/OFF] <OFF>:");
    s.cmdline("on").unwrap();
    assert_eq!(var(&s, "QTEXTMODE"), 1);
    assert_eq!(s.execute("ucsicon", &json!({ "on": false })).unwrap()["on"], json!(false));
    assert!(!s.settings.ucsicon);
}

#[test]
fn system_variables_scripts_set_are_accepted() {
    let mut s = session();
    for (name, value) in [
        ("FILEDIA", 0),
        ("CMDDIA", 0),
        ("CMDECHO", 0),
        ("LWUNITS", 0),
        ("EXPERT", 5),
        ("MAXACTVP", 16),
        ("REGENAUTO", 0),
        ("BLIPMODE", 1),
        ("DRAGMODE", 1),
        ("UCSFOLLOW", 1),
        ("QTEXTMODE", 1),
    ] {
        assert!(sysvars::get(&s, name).is_some(), "{name}");
        s.cmdline(&format!("setvar {name} {value}")).unwrap();
        assert_eq!(var(&s, name), value, "{name}: {:?}", s.log);
    }
    assert!(!s.settings.filedia && !s.settings.lwunits_mm);
    // Out of range is refused, as for other variables.
    assert!(sysvars::set(&mut s, "EXPERT", &json!(9)).is_err());
}

#[test]
fn th_and_ti_set_their_system_variables_and_tilemode_switches_tabs() {
    let mut s = session();
    s.cmdline("TH").unwrap();
    assert_eq!(s.prompt_text(), "SETVAR Enter new value for THICKNESS <0.0>:");
    s.cmdline("2").unwrap();
    assert_eq!(sysvars::get(&s, "THICKNESS").and_then(|v| v.as_f64()), Some(2.0));
    assert_eq!(var(&s, "TILEMODE"), 1);
    s.cmdline("ti").unwrap();
    s.cmdline("0").unwrap();
    assert_eq!(var(&s, "TILEMODE"), 0);
    assert_ne!(s.state().unwrap().space, cadcraft_engine::doc::Space::Model);
    s.cmdline("tilemode 1").unwrap();
    assert_eq!(s.state().unwrap().space, cadcraft_engine::doc::Space::Model);
}

#[test]
fn clayer_must_name_a_layer_that_exists_and_is_thawed() {
    let mut s = session();
    s.cmdline("clayer NoSuch").unwrap();
    assert!(logged(&s, "Cannot find layer \"NoSuch\"."), "{:?}", s.log);
    s.cancel();
    assert_eq!(s.doc().unwrap().header.str("CLAYER", "0"), "0");
    s.execute("layer.new", &json!({ "name": "Walls" })).unwrap();
    s.execute("layer.new", &json!({ "name": "Ice" })).unwrap();
    s.execute("layer.set", &json!({ "name": "Ice", "frozen": true })).unwrap();
    assert!(sysvars::set(&mut s, "CLAYER", &json!("Ice")).is_err());
    // A typed name in any case sets the layer's own name.
    s.cmdline("clayer walls").unwrap();
    assert!(s.running.is_none());
    assert_eq!(s.doc().unwrap().header.str("CLAYER", "0"), "Walls");
    s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
    assert_eq!(s.doc().unwrap().model.iter().last().unwrap().common.layer, "Walls");
}

/// AutoCAD script lines: a space ends a font or file name; a name with spaces is quoted.
#[test]
fn one_line_scripts_with_fonts_and_file_names() {
    let mut s = session();
    s.script("-style Notes arial.ttf 2.5 1 0 n n\nline 0,0 10,0 \n").unwrap();
    assert!(s.running.is_none(), "{}", s.prompt_text());
    let st = s.doc().unwrap().text_style("Notes").cloned().unwrap();
    assert_eq!((st.font.as_str(), st.height), ("arial.ttf", 2.5));
    assert_eq!(s.doc().unwrap().model.len(), 1, "the next line ran");
    s.script("-style Labels \"Noto Sans.ttf\" 0 1 0 n n\n").unwrap();
    assert_eq!(s.doc().unwrap().text_style("Labels").unwrap().font, "Noto Sans.ttf");

    set_io(IoHooks { read: |_, _| Err("no reader in this test".into()), write: |d, _| Ok(format!("{}", d.model.len()).into_bytes()), plot: None });
    let dir = std::env::temp_dir().join(format!("cadcraft-cl-forms-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("out1.dxf");
    s.script(&format!("-wblock {} *\nline 0,5 10,5 \n", out.to_string_lossy())).unwrap();
    assert!(s.running.is_none(), "{}", s.prompt_text());
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "1");
    assert_eq!(s.doc().unwrap().model.len(), 2, "the next line ran");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prompts_show_autocads_command_names() {
    let mut s = session();
    s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [10, 0], "at": [5, 5] })).unwrap();
    s.start("dimtedit.left").unwrap();
    assert_eq!(s.prompt_text(), "DIMTEDIT Select dimension:");
    s.cancel();
    s.start("radius.measure").unwrap();
    assert!(s.prompt_text().starts_with("MEASUREGEOM "), "{}", s.prompt_text());
    s.cancel();
    // A dash name running a command's prompts echoes and shows the dash, and Enter repeats it.
    s.cmdline("-dimstyle").unwrap();
    assert!(logged(&s, "Command: -DIMSTYLE"));
    assert_eq!(s.prompt_text(), "-DIMSTYLE Enter a dimension style option [ANnotative/Save/Restore/STatus/Variables/Apply/?] <Restore>:");
    s.cancel();
    s.input(Input::Enter).unwrap();
    assert_eq!(s.running.as_ref().map(|r| r.shown()), Some("-DIMSTYLE".to_string()));
    s.cancel();
    // Option and mode prompts offer their options without "or"; others keep it.
    s.cmdline("measuregeom").unwrap();
    assert!(s.prompt_text().starts_with("MEASUREGEOM Enter an option [Distance/"), "{}", s.prompt_text());
    s.cancel();
    s.cmdline("line").unwrap();
    s.cmdline("0,0").unwrap();
    assert_eq!(s.prompt_text(), "LINE Specify next point or [Undo]:");
}

#[test]
fn file_name_prompts_offer_the_file_dialog() {
    let mut s = session();
    let file = |s: &Session| s.current_prompt().and_then(|p| p.picker);
    s.cmdline("stepin").unwrap();
    assert_eq!(file(&s), Some(Picker::File { save: false, label: "STEP (STP, STEP)", exts: &["stp", "step"] }));
    s.cancel();
    s.cmdline("igesout").unwrap();
    assert!(matches!(file(&s), Some(Picker::File { save: true, .. })));
    s.cancel();
    s.cmdline("stlout").unwrap();
    assert_eq!(file(&s), None, "objects first");
    s.input(Input::Enter).unwrap();
    s.execute("table", &json!({ "at": [0, 0], "rows": 2, "cols": 2 })).unwrap();
    let t = s.doc().unwrap().model.handles()[0];
    s.cmdline("datalink").unwrap();
    s.input(Input::Pick(vec![t])).unwrap();
    assert!(matches!(file(&s), Some(Picker::File { save: false, label: "CSV (CSV, TXT, TSV)", .. })));
}

#[test]
fn legacy_names_reach_their_commands() {
    let mut s = session();
    for (typed, running) in [("DDVPOINT", "vpoint"), ("vp", "vpoint"), ("VSM", "vscurrent"), ("visualstyles", "vscurrent")] {
        s.cmdline(typed).unwrap();
        assert_eq!(s.running.as_ref().map(|r| r.id.as_str()), Some(running), "{typed}");
        s.cancel();
    }
    assert_eq!(cadcraft_engine::cmd::resolve_alias("DDOSNAP"), "dsettings");
}
