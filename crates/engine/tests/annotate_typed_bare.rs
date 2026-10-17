//! LEADER and Dimension ▸ Align Text ▸ Home/Left/Center/Right started bare (typed, or from their
//! menu) prompt instead of failing with "`points` is required" / "select dimensions". The JSON
//! forms are unchanged.

use cadcraft_engine::doc::{Dimension, EntityKind, Leader};
use cadcraft_engine::geom::Vec2;
use cadcraft_engine::{Input, Session};
use serde_json::json;

fn the_dim(s: &Session) -> Dimension {
    s.doc().unwrap().model.iter().find_map(|e| if let EntityKind::Dimension(d) = &e.kind { Some(d.clone()) } else { None }).unwrap()
}

fn leaders(s: &Session) -> Vec<Leader> {
    s.doc().unwrap().model.iter().filter_map(|e| if let EntityKind::Leader(l) = &e.kind { Some(l.clone()) } else { None }).collect()
}

#[test]
fn leader_prompts_for_points_format_and_annotation() {
    let mut s = Session::new();
    s.cmdline("leader").unwrap();
    assert_eq!(s.prompt_text(), "LEADER Specify leader start point:");
    s.cmdline("0,0").unwrap();
    assert_eq!(s.prompt_text(), "LEADER Specify next point:");
    s.cmdline("5,5").unwrap();
    assert_eq!(s.prompt_text(), "LEADER Specify next point or [Annotation/Format/Undo] <Annotation>:");
    assert!(!s.preview(Vec2::new(9.0, 5.0)).is_empty(), "the leader follows the cursor");
    // Format: a spline leader without an arrowhead.
    s.cmdline("f").unwrap();
    assert_eq!(s.prompt_text(), "LEADER Enter leader format option [Spline/STraight/Arrow/None] <Exit>:");
    s.cmdline("s").unwrap();
    s.cmdline("f").unwrap();
    s.cmdline("n").unwrap();
    // Undo takes back a point.
    s.cmdline("8,8").unwrap();
    s.cmdline("u").unwrap();
    s.cmdline("10,5").unwrap();
    s.cmdline("").unwrap();
    assert_eq!(s.prompt_text(), "LEADER Enter first line of annotation text or <options>:");
    s.cmdline("Weld all round").unwrap();
    assert_eq!(s.prompt_text(), "LEADER Enter next line of annotation text:");
    s.cmdline("Typ.").unwrap();
    s.cmdline("").unwrap();
    assert!(s.running.is_none());
    let l = &leaders(&s)[0];
    assert_eq!(l.vertices.iter().map(|v| v.xy()).collect::<Vec<_>>(), [Vec2::new(0.0, 0.0), Vec2::new(5.0, 5.0), Vec2::new(10.0, 5.0)]);
    assert!(l.spline && !l.arrow);
    let text = s.doc().unwrap().model.iter().find_map(|e| if let EntityKind::MText(t) = &e.kind { Some(t.contents.clone()) } else { None });
    assert_eq!(text.as_deref(), Some("Weld all round\\PTyp."));

    // Enter at the first annotation line: the leader alone. LEAD is the same command.
    s.cmdline("lead 20,0 25,5").unwrap();
    s.cmdline("").unwrap();
    s.cmdline("").unwrap();
    assert!(s.running.is_none());
    assert_eq!(leaders(&s).len(), 2);
    assert_eq!(s.doc().unwrap().model.iter().filter(|e| matches!(e.kind, EntityKind::MText(_))).count(), 1);

    // The JSON form is unchanged.
    assert!(s.execute("leader", &json!({})).is_err());
    let r = s.execute("leader", &json!({ "points": [[0, 0], [1, 1]] })).unwrap();
    assert_eq!(r["handles"].as_array().map(Vec::len), Some(1));
    assert!(leaders(&s).last().is_some_and(|l| l.arrow && !l.spline));
}

#[test]
fn align_text_menu_entries_select_the_dimension() {
    let mut s = Session::new();
    s.script("dimlinear 0,0 10,0 5,3\n").unwrap();
    let h = s.doc().unwrap().model.iter().find(|e| matches!(e.kind, EntityKind::Dimension(_))).unwrap().handle;

    // No selection: "Select dimension", then the alignment is applied.
    s.start("dimtedit.left").unwrap();
    assert_eq!(s.prompt_text(), "DIMTEDIT Select dimension:");
    s.input(Input::Pick(vec![h])).unwrap();
    assert!(s.running.is_none());
    assert_eq!(the_dim(&s).overrides.get("textJust").and_then(|v| v.as_i64()), Some(1));
    s.start("dimtedit.right").unwrap();
    s.input(Input::Pick(vec![h])).unwrap();
    assert_eq!(the_dim(&s).overrides.get("textJust").and_then(|v| v.as_i64()), Some(2));

    // A preselected dimension is taken at once.
    s.set_selection(vec![h]);
    s.start("dimtedit.center").unwrap();
    assert!(s.running.is_none());
    assert_eq!(the_dim(&s).overrides.get("textJust").and_then(|v| v.as_i64()), Some(0));
    s.execute("dimtedit.angle", &json!({ "handles": [h.hex()], "angle": 30 })).unwrap();
    s.set_selection(vec![]);
    s.start("dimtedit.home").unwrap();
    s.input(Input::Pick(vec![h])).unwrap();
    assert!(s.running.is_none());
    let d = the_dim(&s);
    assert!(d.text_rotation == 0.0 && !d.overrides.contains_key("textJust"), "{d:?}");

    // The JSON form with nothing selected still refuses.
    assert!(s.execute("dimtedit.home", &json!({})).is_err());
}
