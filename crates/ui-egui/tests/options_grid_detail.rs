//! Options ▸ Grid (Rhino's grid options) and Options ▸ Display Detail (how the drawing area
//! simplifies as the view zooms out): their OPTIONS JSON keys, the profile and the tabs.

use cadcraft_engine::Session;
use cadcraft_ui_egui::{CadApp, Services};
use serde_json::json;

fn app() -> CadApp {
    CadApp::new(Session::new(), Services::default())
}

/// Two frames, returning the painted text of the last.
fn render(app: &mut CadApp, ctx: &egui::Context) -> String {
    fn collect(shape: &egui::Shape, text: &mut String) {
        match shape {
            egui::Shape::Text(s) => {
                text.push_str(s.galley.text());
                text.push('\n');
            }
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, text)),
            _ => {}
        }
    }
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))), ..Default::default() };
    let mut text = String::new();
    for _ in 0..2 {
        app.logic(ctx);
        let mut output = ctx.run_ui(input.clone(), |ui| app.ui(ui));
        text.clear();
        output.shapes.iter().for_each(|s| collect(&s.shape, &mut text));
        output.textures_delta.clear();
    }
    text
}

#[test]
fn grid_options_are_profile_variables_set_through_options() {
    let mut a = app();
    let r = a
        .run("options", json!({"GRIDLINECOUNT": 200, "GRIDUNIT": [1, 1], "GRIDMAJOR": 10, "GRIDAXES": 0, "UCSICON": 0, "SNAPUNIT": [1, 1]}))
        .unwrap();
    assert_eq!((r["GRIDLINECOUNT"].clone(), r["GRIDAXES"].clone(), r["UCSICON"].clone()), (json!(200), json!(0), json!(0)));
    let st = &a.session.settings;
    assert_eq!((st.gridlinecount, st.gridaxes), (200, false), "profile settings, as GRIDMAJOR");
    assert!(!a.session.settings.ucsicon);
    // SETVAR reaches the same variables; a new drawing keeps them.
    a.cmdline("setvar gridlinecount 50");
    a.run("new", json!({})).unwrap();
    assert_eq!(a.session.settings.gridlinecount, 50);
    let mut b = app();
    b.load_profile(&a.profile_json());
    assert_eq!((b.session.settings.gridlinecount, b.session.settings.gridaxes), (50, false), "kept for next time");
    for bad in [json!({"GRIDLINECOUNT": 1001}), json!({"GRIDLINECOUNT": -1}), json!({"GRIDAXES": 2}), json!({"UCSICON": "maybe"})] {
        assert!(a.run("options", bad.clone()).is_err(), "{bad}");
    }
    // A reset turns the axes icon back on, and the grid's axes and line count.
    a.run("options", json!({"reset": true})).unwrap();
    assert!(a.session.settings.ucsicon && a.session.settings.gridaxes);
    assert_eq!(a.session.settings.gridlinecount, 0);
}

#[test]
fn display_detail_is_an_interface_preference_all_or_nothing() {
    let mut a = app();
    let r = a.run("options", json!({})).unwrap();
    assert_eq!(r["displayDetail"]["grid"], json!({"simplify": true, "pixels": 8.0}), "{r}");
    let r = a.run("options", json!({"displayDetail": {"text": {"simplify": false}, "curveTolerance": 0.05}})).unwrap();
    assert_eq!(r["displayDetail"]["text"]["simplify"], json!(false));
    assert_eq!(r["displayDetail"]["text"]["pixels"], json!(2.0), "the rest of the kind stays");
    assert_eq!(r["displayDetail"]["curveTolerance"], json!(0.05));
    let before = a.run("options", json!({})).unwrap();
    for bad in [
        json!({"displayDetail": {"grid": {"pixels": 0.1}}, "PICKBOX": 9}),
        json!({"displayDetail": {"meshes": {"simplify": false}}}),
        json!({"displayDetail": {"text": {"simplify": 1}}, "theme": "light"}),
        json!({"displayDetail": "off"}),
        json!({"displayDetail": {"curves": {"simplify": false}}, "GRIDLINECOUNT": 5000}),
    ] {
        assert!(a.run("options", bad.clone()).is_err(), "{bad}");
        assert_eq!(a.run("options", json!({})).unwrap(), before, "{bad} changed nothing");
    }
    // Kept for next time, and reset to what the drawing area always did.
    let mut b = app();
    b.load_profile(&a.profile_json());
    assert_eq!(b.options.detail, a.options.detail);
    let r = a.run("options", json!({"reset": true})).unwrap();
    assert_eq!(r["displayDetail"]["text"]["simplify"], json!(true));
}

#[test]
fn a_detail_change_rebuilds_the_display_lists_keeping_the_old_on_screen() {
    let mut a = app();
    a.run("circle", json!({"center": [0, 0], "radius": 5})).unwrap();
    let ctx = egui::Context::default();
    render(&mut a, &ctx);
    assert!(a.canvas.list.is_some());
    a.run("options", json!({"displayDetail": {"curves": {"pixels": 2}}})).unwrap();
    assert!(a.canvas.list.is_some(), "kept on screen while the new detail builds");
    for _ in 0..500 {
        render(&mut a, &ctx);
        if !a.canvas.building() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(a.canvas.list.is_some() && !a.canvas.building(), "built again");
}

#[test]
fn grid_and_display_detail_tabs_render() {
    let mut a = app();
    let ctx = egui::Context::default();
    for (tab, labels) in [
        ("grid", ["Apply grid changes to", "Grid line count", "Minor grid lines every", "Show world axes icon"]),
        ("detail", ["Simplify when zoomed out", "Curves", "Linetypes", "Hatches"]),
    ] {
        a.ui.dialog = Some(format!("options:{tab}"));
        let text = render(&mut a, &ctx);
        for label in labels {
            assert!(text.contains(label), "{tab}: {label} in {text}");
        }
    }
    // Off, a kind shows its fixed value instead of its threshold.
    a.run("options", json!({"displayDetail": {"curves": {"simplify": false}}})).unwrap();
    a.ui.dialog = Some("options:detail".into());
    assert!(render(&mut a, &ctx).contains("Chords never stray by more than"));
}
