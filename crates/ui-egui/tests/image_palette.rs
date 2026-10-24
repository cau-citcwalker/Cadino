//! Attached images in the External References palette: IMAGE (typed, by its alias) opens it; the
//! image's row shows its status and file type, and its buttons act through -IMAGE.

use cadcraft_engine::Session;
use cadcraft_engine::doc::EntityKind;
use cadcraft_ui_egui::{CadApp, Services, docking::Panel};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

fn photo_file() -> String {
    let dir = std::env::temp_dir().join(format!("cadcraft-image-palette-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("photo.png");
    image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 200, 30, 255])).save(&p).unwrap();
    p.to_string_lossy().into_owned()
}

fn image(app: &CadApp) -> Option<cadcraft_engine::doc::Image> {
    app.session.doc().ok()?.model.iter().find_map(|e| match &e.kind {
        EntityKind::Image(i) => Some(i.clone()),
        _ => None,
    })
}

#[test]
fn image_opens_the_palette_and_its_buttons_manage_the_image() {
    let mut app = CadApp::new(Session::default(), Services::default());
    app.run("imageattach", json!({ "path": photo_file(), "scale": 40 })).unwrap();
    assert!(image(&app).is_some_and(|i| i.pixels.is_some()));
    assert!(app.cmdline("IM").is_none());
    assert!(app.ui.docking.layout.contains(&Panel::ExternalRefs));
    app.ui.in_window_menu = false;
    let mut h = Harness::builder().with_size(vec2(1400.0, 900.0)).build_ui_state(
        |ui, app: &mut CadApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(4);
    h.get_by_label("photo");
    h.get_by_label("Loaded");
    h.get_by_label("PNG");
    h.get_by_label("Unload").click();
    h.run_steps(3);
    assert!(image(h.state()).is_some_and(|i| i.unloaded && i.pixels.is_none()));
    h.get_by_label("Unloaded");
    h.get_by_label("Reload").click();
    h.run_steps(3);
    assert!(image(h.state()).is_some_and(|i| !i.unloaded && i.pixels.is_some()));
    h.get_by_label("Detach").click();
    h.run_steps(3);
    assert!(image(h.state()).is_none());
    h.get_by_label("No external references are attached.");
}
