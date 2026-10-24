//! The Render window: RENDER or RR typed, or View ▸ Render ▸ Render, renders the current Model
//! view (perspective or parallel) with the engine's RENDER ([`RenderJob`]: a snapshot of the
//! drawing and the view) on a worker thread, so the app goes on meanwhile; the window shows a
//! spinner with the time taken and Cancel, then the picture, at a size chosen here, with or
//! without edges. Save Image... writes a PNG through RENDER, so the file limits hold. Calls with
//! parameters run the command and open nothing.
//! Cancel, Render again or closing the window stops the worker running ([`RenderJob::cancel`]),
//! and only one worker runs at a time ([`Busy`]): a new render starts once the one stopping has.
// ponytail: Save Image... renders again on the UI thread; save the picture shown if that matters.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cadcraft_engine::cmd::rendering::RenderJob;
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

/// A finished render: the picture and its caption, or why there is none.
type Done = Result<(egui::ColorImage, String), String>;

/// A render on its way: the worker puts its result in `slot`.
#[derive(Clone)]
struct Pending {
    slot: Arc<Mutex<Option<Done>>>,
    started: f64,
    /// The job being rendered (`None`: it couldn't be made), to stop it.
    job: Option<RenderJob>,
}

impl Pending {
    /// Stop the worker: it gives up soon, and its result goes with this.
    fn stop(&self) {
        if let Some(j) = &self.job {
            j.cancel();
        }
    }
}

/// Whether a render worker is running (a stopped one too, until it has given up).
static BUSY: AtomicBool = AtomicBool::new(false);

/// The right to run the one render worker, given back when dropped: clicking Render again and
/// again never piles up workers (each with a full-size canvas and all the CPU).
struct Busy;

impl Busy {
    fn claim() -> Option<Busy> {
        BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).ok().map(|_| Busy)
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

/// What the window shows and how it renders, kept in egui's memory while it is open.
#[derive(Clone, Default)]
struct State {
    size: usize,
    edges: bool,
    /// Render again on the next frame.
    stale: bool,
    shown: Option<Result<(TextureHandle, String), String>>,
    pending: Option<Pending>,
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

/// Render `job` and decode it: the picture and its caption. Runs on the worker.
fn run(job: &RenderJob) -> Done {
    let t0 = crate::now_ms();
    let png = job.png().map_err(|e| e.to_string())?;
    let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).map_err(|e| e.to_string())?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let projection = if job.perspective() { crate::tl!("Perspective") } else { crate::tl!("Parallel") };
    let info = format!("{} × {} · {projection} · {:.0} ms", size[0], size[1], crate::now_ms() - t0);
    Ok((egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), info))
}

/// Start rendering the view at the window's settings: on a worker thread, which asks `ctx` for a
/// frame when it is done; where there are no threads (the web build), here and now. `None` while
/// another worker still runs ([`Busy`]).
fn start(app: &CadApp, ctx: &egui::Context, s: &State) -> Option<Pending> {
    let busy = Busy::claim()?;
    let (w, h) = pixels(app, s.size);
    let slot = Arc::new(Mutex::new(None));
    let mut pending = Pending { slot: slot.clone(), started: crate::now_ms(), job: None };
    let job = match RenderJob::new(&app.session, &json!({ "width": w, "height": h, "edges": s.edges })) {
        Ok(job) => job,
        Err(e) => {
            put(&slot, Err(e.to_string()));
            return Some(pending);
        }
    };
    pending.job = Some(job.clone());
    let language = app.language();
    let work = move || {
        crate::i18n::with_language(language, || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&job))).unwrap_or_else(|_| Err(crate::tl!("The render failed.").to_string()))
        })
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (out, ctx) = (slot.clone(), ctx.clone());
        // A worker that can't start gives `busy` back with its closure.
        let spawned = std::thread::Builder::new().name("cadino-render".into()).spawn(move || {
            put(&out, work());
            drop(busy);
            ctx.request_repaint();
        });
        if let Err(e) = spawned {
            put(&slot, Err(e.to_string()));
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = ctx;
        put(&slot, work());
        drop(busy);
    }
    Some(pending)
}

/// Cancel: the worker stops, and with no picture yet the window says so (it doesn't start again).
fn cancel(s: &mut State) {
    if let Some(p) = s.pending.take() {
        p.stop();
    }
    if s.shown.is_none() {
        s.shown = Some(Err(crate::tl!("The render was cancelled.").to_string()));
    }
}

fn put(slot: &Mutex<Option<Done>>, done: Done) {
    if let Ok(mut s) = slot.lock() {
        *s = Some(done);
    }
}

/// The Render window.
pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let mut s: State = ctx.data(|d| d.get_temp(id())).unwrap_or_default();
    if RESTART.with(|r| r.replace(false)) {
        s.stale = true;
    }
    if s.stale || (s.shown.is_none() && s.pending.is_none()) {
        // A render still running stops: its picture would be out of date.
        if let Some(p) = s.pending.take() {
            p.stop();
        }
        s.pending = start(app, ctx, &s);
        // While the one stopping finishes, this one waits.
        s.stale = s.pending.is_none();
        if s.stale {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    // A finished render: its picture.
    if let Some(p) = &s.pending
        && let Some(done) = p.slot.lock().ok().and_then(|mut d| d.take())
    {
        s.shown = Some(done.map(|(img, info)| (ctx.load_texture("render", img, egui::TextureOptions::LINEAR), info)));
        s.pending = None;
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
            save = ui.add_enabled(s.pending.is_none(), egui::Button::new(crate::tl!("Save Image..."))).clicked();
        });
        if let Some(p) = &s.pending {
            let mut cancel = false;
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(crate::tf!("Rendering... {seconds} s", seconds = format!("{:.1}", (crate::now_ms() - p.started) / 1000.0)));
                cancel = ui.button(crate::tl!("Cancel")).clicked();
            });
            if cancel {
                self::cancel(&mut s);
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
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
        if let Some(p) = &s.pending {
            p.stop();
        }
        ctx.data_mut(|d| d.remove::<State>(id()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
        let mut out = ctx.run_ui(Default::default(), |ui| dialog(app, ui.ctx(), open));
        out.textures_delta.clear();
    }

    /// Typed RENDER opens the window (a JSON RENDER doesn't), and the window renders the view on
    /// a worker: the frame comes back at once, the picture a little later.
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
        frame(&mut app, &ctx, &mut open);
        let state = |ctx: &egui::Context| -> State { ctx.data(|d| d.get_temp(id())).unwrap() };
        for _ in 0..4000 {
            if state(&ctx).shown.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            frame(&mut app, &ctx, &mut open);
        }
        let s = state(&ctx);
        assert!(matches!(&s.shown, Some(Ok((tex, _))) if tex.size() == [320, 200]), "the view's size by default");
    }

    /// [`start`] once no other worker runs (another test's may).
    fn started(app: &CadApp, ctx: &egui::Context, s: &State) -> Pending {
        for _ in 0..4000 {
            if let Some(p) = start(app, ctx, s) {
                return p;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("a render worker never finished");
    }

    /// The UI thread hands the worker a snapshot: the drawing can change (and the window be
    /// drawn) while it renders.
    #[test]
    fn the_render_runs_on_a_worker() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.session.viewport_px = (320.0, 200.0);
        app.session.execute("line", &json!({ "points": [[0, 0], [100, 50]] })).unwrap();
        let ctx = egui::Context::default();
        let s = State { size: 6, ..State::default() };
        let p = started(&app, &ctx, &s);
        // The session goes on while the 4K picture renders.
        app.session.execute("circle", &json!({ "center": [0, 0], "radius": 5 })).unwrap();
        let mut done = None;
        for _ in 0..4000 {
            done = p.slot.lock().unwrap().take();
            if done.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let (img, info) = done.expect("the worker finished").unwrap();
        assert_eq!(img.size, [3840, 2160]);
        assert!(info.starts_with("3840 × 2160"), "{info}");
        // A layout refuses to render: the job says why.
        app.run("layout.set", json!({ "name": "Layout1" })).ok();
        if app.session.layout_space() != cadcraft_doc::Space::Model {
            let p = started(&app, &ctx, &State::default());
            assert!(matches!(p.slot.lock().unwrap().take(), Some(Err(_))));
        }
    }

    /// Cancel stops the worker and the window doesn't start again; one worker runs at a time, so
    /// Render clicked again waits for the one stopping.
    #[test]
    fn cancel_stops_the_worker_and_renders_never_pile_up() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.session.viewport_px = (320.0, 200.0);
        app.session.execute("line", &json!({ "points": [[0, 0], [100, 50]] })).unwrap();
        let ctx = egui::Context::default();
        let mut st = State { size: 6, ..State::default() };
        st.pending = Some(started(&app, &ctx, &st));
        let job = st.pending.as_ref().and_then(|p| p.job.clone()).unwrap();
        cancel(&mut st);
        assert!(job.png().is_err_and(|e| e.to_string().contains("cancelled")), "the job is stopped");
        assert!(st.pending.is_none() && matches!(st.shown, Some(Err(_))));
        ctx.data_mut(|d| d.insert_temp(id(), st));
        let mut open = true;
        frame(&mut app, &ctx, &mut open);
        let st: State = ctx.data(|d| d.get_temp(id())).unwrap();
        assert!(st.pending.is_none() && !st.stale, "a cancelled render doesn't start again");
        // While a worker runs, no other starts: Render waits for it.
        let held = loop {
            if let Some(b) = Busy::claim() {
                break b;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(start(&app, &ctx, &State::default()).is_none());
        ctx.data_mut(|d| d.insert_temp(id(), State { stale: true, ..State::default() }));
        frame(&mut app, &ctx, &mut open);
        let st: State = ctx.data(|d| d.get_temp(id())).unwrap();
        assert!(st.pending.is_none() && st.stale, "Render waits for the worker running");
        drop(held);
        let mut began = false;
        for _ in 0..4000 {
            frame(&mut app, &ctx, &mut open);
            let st: State = ctx.data(|d| d.get_temp(id())).unwrap();
            if st.pending.is_some() || st.shown.is_some() {
                began = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(began, "and starts once it has finished");
    }
}
