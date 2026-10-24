//! The Render window: RENDER or RR typed, or View ▸ Render ▸ Render, renders the current Model
//! view (perspective or parallel) with the engine's RENDER command and shows the picture, at a
//! size chosen here, with or without edges. Save Image... writes a PNG through RENDER too, so
//! the file limits hold. Calls with parameters run the command and open nothing.

use egui::TextureHandle;
use serde_json::{Value, json};

use crate::CadApp;

/// The sizes offered: label and pixels (0: the view's own size on screen).
const SIZES: [(&str, u32, u32); 7] = [
    ("View size", 0, 0),
    ("640 × 480", 640, 480),
    ("800 × 600", 800, 600),
    ("1024 × 768", 1024, 768),
    ("1280 × 720", 1280, 720),
    ("1920 × 1080", 1920, 1080),
    ("3840 × 2160", 3840, 2160),
];

/// What the window shows and how it renders, kept in egui's memory while it is open.
#[derive(Clone, Default)]
struct State {
    size: usize,
    edges: bool,
    /// Render again on the next frame.
    stale: bool,
    shown: Option<Result<(TextureHandle, String), String>>,
}

fn id() -> egui::Id {
    egui::Id::new("cadino_render_window")
}

/// RENDER or RR typed or picked (no parameters): open the window and render.
pub fn route(app: &mut CadApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !(matches!(id, "render" | "rr") && params.is_null()) {
        return None;
    }
    if let Err(e) = app
        .session
        .state()
        .map_err(|e| e.to_string())
        .and_then(|st| if st.space == cadcraft_doc::Space::Model { Ok(()) } else { Err(crate::tl!("RENDER renders the Model tab.").to_string()) })
    {
        return Some(Err(e));
    }
    RESTART.with(|r| r.set(true));
    app.ui.dialog = Some("render".into());
    Some(Ok(Value::Null))
}

thread_local! {
    /// Set when RENDER was asked for again: the window renders anew.
    static RESTART: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The pixels for size choice `i` (the view's size in pixels for the first).
fn pixels(app: &CadApp, i: usize) -> (u32, u32) {
    match SIZES.get(i) {
        Some(&(_, w, h)) if w > 0 => (w, h),
        _ => {
            let (w, h) = app.session.viewport_px;
            let side = |v: f64| v.round().clamp(16.0, f64::from(cadcraft_render::shade::MAX_SIDE)) as u32;
            (side(w), side(h))
        }
    }
}

/// Render with the RENDER command: the picture as a texture and a caption.
fn render(app: &mut CadApp, ctx: &egui::Context, s: &State) -> Result<(TextureHandle, String), String> {
    let (w, h) = pixels(app, s.size);
    let t0 = crate::now_ms();
    let r = app.session.execute("render", &json!({ "width": w, "height": h, "edges": s.edges, "data": true })).map_err(|e| e.to_string())?;
    let png = r["data"].as_str().and_then(cadcraft_engine::cmd::file::base64_decode).ok_or("the render has no picture")?;
    let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).map_err(|e| e.to_string())?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let tex = ctx.load_texture("render", egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR);
    let projection = if r["perspective"].as_bool() == Some(true) { crate::tl!("Perspective") } else { crate::tl!("Parallel") };
    let info = format!("{w} × {h} · {projection} · {:.0} ms", crate::now_ms() - t0);
    Ok((tex, info))
}

/// The Render window.
pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let mut s: State = ctx.data(|d| d.get_temp(id())).unwrap_or_default();
    if RESTART.with(|r| r.replace(false)) {
        s.stale = true;
    }
    if s.stale || s.shown.is_none() {
        s.stale = false;
        s.shown = Some(render(app, ctx, &s));
    }
    let mut save = false;
    egui::Window::new(crate::tl!("Render")).id(id()).open(open).resizable(true).default_size([800.0, 560.0]).show(ctx, |ui| {
        ui.horizontal(|ui| {
            let label = SIZES.get(s.size).map_or("", |z| z.0);
            egui::ComboBox::from_id_salt("render_size").selected_text(crate::i18n::t(label)).show_ui(ui, |ui| {
                for (i, (name, ..)) in SIZES.iter().enumerate() {
                    if ui.selectable_value(&mut s.size, i, crate::i18n::t(name)).changed() {
                        s.stale = true;
                    }
                }
            });
            if ui.checkbox(&mut s.edges, crate::tl!("Edges")).changed() {
                s.stale = true;
            }
            if ui.button(crate::tl!("Render")).clicked() {
                s.stale = true;
            }
            save = ui.button(crate::tl!("Save Image...")).clicked();
        });
        match &s.shown {
            Some(Ok((tex, info))) => {
                ui.label(format!("{info} · {}", crate::tl!("shaded with the parts' materials, not photoreal")));
                let size = tex.size_vec2();
                let room = ui.available_size();
                let k = (room.x / size.x).min(room.y / size.y).clamp(0.05, 1.0);
                ui.image((tex.id(), size * k));
            }
            Some(Err(e)) => {
                ui.label(e);
            }
            None => {}
        }
    });
    if save {
        let (w, h) = pixels(app, s.size);
        let title = app.session.state().map(|st| st.title.clone()).unwrap_or_default();
        let stem = std::path::Path::new(&title).file_stem().map(|t| t.to_string_lossy().to_string()).filter(|t| !t.is_empty());
        let name = format!("{}.png", stem.unwrap_or_else(|| "Render".into()));
        if let Some(path) = app.services.pick_save.as_ref().and_then(|pick| pick(&name)) {
            let _ = app.run("render", json!({ "path": path, "width": w, "height": h, "edges": s.edges }));
        }
    }
    if *open {
        ctx.data_mut(|d| d.insert_temp(id(), s));
    } else {
        ctx.data_mut(|d| d.remove::<State>(id()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Typed RENDER opens the window (a JSON RENDER doesn't), and the window renders the view.
    #[test]
    fn typed_render_opens_the_render_window() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.session.viewport_px = (320.0, 200.0);
        app.session.execute("line", &json!({ "points": [[0, 0], [100, 50]] })).unwrap();
        let r = app.run("render", json!({ "width": 32, "height": 32 })).unwrap();
        assert_eq!(r["width"], 32);
        assert!(app.ui.dialog.is_none(), "JSON RENDER opens nothing");
        app.start("RR");
        assert_eq!(app.ui.dialog.as_deref(), Some("render"));
        let ctx = egui::Context::default();
        let mut open = true;
        let mut out = ctx.run_ui(Default::default(), |ui| dialog(&mut app, ui.ctx(), &mut open));
        out.textures_delta.clear();
        let s: State = ctx.data(|d| d.get_temp(id())).unwrap();
        assert!(matches!(&s.shown, Some(Ok((tex, _))) if tex.size() == [320, 200]), "the view's size by default");
    }
}
