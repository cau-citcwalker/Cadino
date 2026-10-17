//! DIMSTYLE, COLOR, LWEIGHT, FIND, WBLOCK, the Layer States Manager and MLSTYLE typed bare or
//! chosen from a menu open their dialogs (never a "`name` is required" error); dash forms and JSON
//! calls never do.

use cadcraft_engine::Session;
use cadcraft_ui_egui::{CadApp, Services, dialogs, menus};
use serde_json::json;

fn draw(app: &mut CadApp, ctx: &egui::Context) {
    let mut out = ctx.run_ui(Default::default(), |ui| dialogs::show(app, ui.ctx()));
    out.textures_delta.clear();
}

#[test]
fn typed_names_and_menus_open_the_dialogs() {
    let mut app = CadApp::new(Session::new(), Services::default());
    let ctx = egui::Context::default();
    for (typed, dialog) in [
        ("DIMSTYLE", "dimstyle"),
        ("d", "dimstyle"),
        ("DST", "dimstyle"),
        ("ddim", "dimstyle"),
        ("COLOR", "color"),
        ("col", "color"),
        ("colour", "color"),
        ("LWEIGHT", "lweight"),
        ("lw", "lweight"),
        ("lineweight", "lweight"),
        ("FIND", "find"),
        ("WBLOCK", "wblock"),
        ("w", "wblock"),
        ("LAYERSTATE", "layerstates"),
        ("LAS", "layerstates"),
        ("lman", "layerstates"),
        ("MLSTYLE", "mlstyle"),
    ] {
        app.ui.dialog = None;
        let n = app.session.log.len();
        assert!(app.cmdline(typed).is_none(), "bare {typed} is not an error");
        assert_eq!(app.ui.dialog.as_deref(), Some(dialog), "{typed}");
        assert!(!app.session.log[n..].iter().any(|l| l.contains("required")), "{typed}: {:?}", &app.session.log[n..]);
        draw(&mut app, &ctx);
        assert_eq!(app.ui.dialog.as_deref(), Some(dialog), "{typed} draws and stays open");
    }
    for (menu, dialog) in [
        ("dimstyle", "dimstyle"),
        ("dimstyle.dimension", "dimstyle"),
        ("color", "color"),
        ("lweight", "lweight"),
        ("find", "find"),
        ("layerstate.save", "layerstates"),
        ("mlstyle", "mlstyle"),
    ] {
        app.ui.dialog = None;
        menus::activate(&mut app, menu);
        assert_eq!(app.ui.dialog.as_deref(), Some(dialog), "menu {menu}");
    }
}

#[test]
fn dash_forms_and_json_calls_never_open_them() {
    let mut app = CadApp::new(Session::new(), Services::default());
    for typed in ["-DIMSTYLE", "-COLOR", "-LWEIGHT", "-WBLOCK"] {
        app.cmdline(typed);
        assert!(app.ui.dialog.is_none(), "{typed} stays on the command line");
        app.session.cancel();
    }
    app.run("dimstyle", json!({ "name": "Arch", "DIMTXT": 2.5 })).unwrap();
    app.run("color", json!({ "color": "red" })).unwrap();
    app.run("lweight", json!({ "lineweight": 0.5 })).unwrap();
    app.run("layerstate.save", json!({ "name": "base" })).unwrap();
    app.run("mlstyle", json!({})).unwrap();
    app.cmdline(r#"dimstyle {"name": "Mech", "current": false}"#);
    assert!(app.ui.dialog.is_none());
    let d = app.session.doc().unwrap();
    assert!(d.dim_style("Arch").is_some() && d.dim_style("Mech").is_some());
    assert_eq!((d.header.i64("CECOLOR", 0), d.header.i64("CELWEIGHT", 0)), (1, 50));
    assert_eq!(d.layer_states.len(), 1);
}

#[test]
fn the_dialogs_draw_in_korean_and_ukrainian() {
    for lang in ["uk", "ko"] {
        let mut app = CadApp::new(Session::new(), Services::default());
        app.run("ui.language", json!({ "lang": lang })).unwrap();
        let ctx = egui::Context::default();
        for dialog in ["dimstyle", "color", "lweight", "find", "wblock", "layerstates", "mlstyle"] {
            app.ui.dialog = Some(dialog.into());
            draw(&mut app, &ctx);
            assert_eq!(app.ui.dialog.as_deref(), Some(dialog), "{lang} {dialog}");
        }
    }
}
