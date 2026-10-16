//! The AI Assistant palette: a chat in which a model drafts and models through the tools the MCP
//! server exposes ([`cadcraft_ai`]). Each message runs on a worker thread; the tools reach the
//! drawing as control requests the app serves between frames ([`InApp`]), so everything the
//! assistant does is an ordinary, undoable command and the window stays live meanwhile.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use cadcraft_ai::Event;
use egui::text::LayoutJob;
use egui::{Align, Layout, RichText, TextFormat};
use serde_json::{Value, json};

use crate::theme::Tokens;
use crate::{CadApp, ControlRequest};

/// How long one tool call waits for the app to serve it.
const TOOL_TIMEOUT: Duration = Duration::from_secs(180);
/// The widest tool image, in points.
const THUMB: f32 = 260.0;
/// Result text shown for one tool call; the model gets it all.
const MAX_SHOWN_RESULT: usize = 4000;
const INPUT_ID: &str = "cadcraft-assistant-input";

/// The tools' way into the running app: each call is a control request on the app's assistant
/// channel, served by [`CadApp::logic`] like the control channel's.
pub struct InApp {
    pub tx: Sender<ControlRequest>,
    pub ctx: egui::Context,
}

impl cadcraft_ai::tools::Backend for InApp {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let (req, pending) = ControlRequest::new(method, params);
        self.tx.send(req).map_err(|_| "Cadino has closed".to_string())?;
        self.ctx.request_repaint();
        let v = pending.wait(TOOL_TIMEOUT);
        if v.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
        let e = v.get("error").and_then(Value::as_str).unwrap_or("error");
        // A timeout says whether the request can still have run (#345).
        Err(match v.get("state").and_then(Value::as_str) {
            Some("not-run") => format!("{e}: Cadino did not start `{method}` and won't; it is safe to send again"),
            Some("may-have-run") => format!("{e}: `{method}` may or may not have been applied. Inspect the drawing first"),
            _ => e.to_string(),
        })
    }
    fn has_ui(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "Cadino (in-app)".into()
    }
}

/// One entry of the transcript.
enum Item {
    User(String),
    Text(String),
    Thinking(String),
    Tool {
        id: String,
        name: String,
        input: Value,
        /// Whether it worked and what it said; `None` while it runs.
        result: Option<(bool, String)>,
        /// The image it returned, until it becomes `texture`.
        png: Option<String>,
        texture: Option<egui::TextureHandle>,
    },
    Notice(String),
    Error(String),
    /// Tokens one message used: input, output, cache reads.
    Usage([u64; 3]),
}

/// A message being answered.
struct Run {
    /// The worker's events; it hangs up when it is done.
    rx: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    usage: [u64; 3],
}

/// The chat: its transcript, the conversation sent to the model and the message being answered.
#[derive(Default)]
pub struct Assistant {
    items: Vec<Item>,
    pub input: String,
    conv: Arc<Mutex<cadcraft_ai::Conversation>>,
    run: Option<Run>,
    /// Put the keyboard in the input on the next frame.
    focus: bool,
}

/// The conversation, also after a worker panicked while holding it (then it starts over, since
/// that turn may be half written).
fn lock(conv: &Mutex<cadcraft_ai::Conversation>) -> MutexGuard<'_, cadcraft_ai::Conversation> {
    conv.lock().unwrap_or_else(|poisoned| {
        conv.clear_poison();
        let mut c = poisoned.into_inner();
        c.clear();
        c
    })
}

impl Assistant {
    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    /// Start over: an empty transcript and conversation. Not while a message is answered.
    pub fn clear(&mut self) {
        if self.run.is_none() {
            self.items.clear();
            lock(&self.conv).clear();
        }
    }

    pub fn stop(&self) {
        if let Some(run) = &self.run {
            run.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Take the worker's events into the transcript. Called every frame.
    pub fn poll(&mut self) {
        let Some(run) = &mut self.run else { return };
        loop {
            match run.rx.try_recv() {
                Ok(e) => apply(&mut self.items, &mut run.usage, e),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        if let Some(run) = self.run.take()
            && run.usage != [0; 3]
        {
            self.items.push(Item::Usage(run.usage));
        }
    }
}

fn apply(items: &mut Vec<Item>, usage: &mut [u64; 3], e: Event) {
    match e {
        Event::Text(d) => match items.last_mut() {
            Some(Item::Text(s)) => s.push_str(&d),
            _ => items.push(Item::Text(d)),
        },
        Event::Thinking(d) => match items.last_mut() {
            Some(Item::Thinking(s)) => s.push_str(&d),
            _ => items.push(Item::Thinking(d)),
        },
        Event::ToolCall { id, name, input } => items.push(Item::Tool { id, name, input, result: None, png: None, texture: None }),
        Event::ToolResult { id, ok, text, image_png_base64 } => {
            if let Some(Item::Tool { result, png, .. }) = items.iter_mut().rev().find(|i| matches!(i, Item::Tool { id: x, .. } if *x == id)) {
                *result = Some((ok, text));
                *png = image_png_base64;
            }
        }
        Event::TurnDone { usage: u, .. } => {
            for (n, key) in usage.iter_mut().zip(["input_tokens", "output_tokens", "cache_read_input_tokens"]) {
                *n = n.saturating_add(u.get(key).and_then(Value::as_u64).unwrap_or(0));
            }
        }
        Event::Notice(s) => items.push(Item::Notice(s)),
        Event::Error(s) => items.push(Item::Error(s)),
    }
}

/// Open the palette (ASSISTANT, AI, Tools ▸ AI Assistant) with the keyboard in its input. The
/// first time it floats by the right edge of the drawing, as QuickCalc does; afterwards it
/// returns where it was closed.
pub fn open(app: &mut CadApp) -> Result<Value, String> {
    app.assistant.focus = true;
    let panel = crate::docking::Panel::Assistant;
    let first = !app.ui.docking.layout.contains(&panel) && !app.ui.docking.hidden.iter().any(|(p, _)| *p == panel);
    let r = crate::docking::command(app, false, &json!({ "operation": "open", "panel": panel.id() }))?;
    if !first {
        return Ok(r);
    }
    let area = app.canvas.rect.unwrap_or(egui::Rect::from_min_size(egui::pos2(220.0, 80.0), egui::vec2(1000.0, 800.0)));
    let (w, h) = (380.0, (area.height() - 48.0).clamp(300.0, 900.0));
    let rect = [area.right() - w - 16.0, area.top() + 24.0, w, h];
    crate::docking::command(app, false, &json!({ "operation": "float", "panel": panel.id(), "rect": rect }))
}

/// Send the typed message: the model answers on a worker thread.
fn send(app: &mut CadApp, ctx: &egui::Context) {
    let text = app.assistant.input.trim().to_string();
    if text.is_empty() || app.assistant.run.is_some() {
        return;
    }
    app.assistant.input.clear();
    app.assistant.items.push(Item::User(text.clone()));
    let backend = InApp { tx: app.assistant_tx.clone(), ctx: ctx.clone() };
    match spawn(app.ai.config(), Arc::clone(&app.assistant.conv), text, backend) {
        Ok(run) => app.assistant.run = Some(run),
        Err(e) => app.assistant.items.push(Item::Error(e)),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn(mut cfg: cadcraft_ai::Config, conv: Arc<Mutex<cadcraft_ai::Conversation>>, text: String, mut backend: InApp) -> Result<Run, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&cancel);
    std::thread::Builder::new()
        .name("cadino-assistant".into())
        .spawn(move || {
            let ctx = backend.ctx.clone();
            // A panic in the agent loop ends this message with an error, not the app.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if cfg.api_key.is_empty() {
                    cfg.api_key = cadcraft_ai::keys::load(cfg.provider).unwrap_or_default();
                }
                let mut conv = lock(&conv);
                cadcraft_ai::run(&cfg, &mut conv, &text, &mut backend, &stop, &mut |e| {
                    let _ = tx.send(e);
                    ctx.request_repaint();
                })
            }));
            let error = match r {
                Ok(r) => r.err(),
                Err(p) => Some(format!(
                    "The assistant stopped on an internal error: {}",
                    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default()
                )),
            };
            if let Some(e) = error {
                let _ = tx.send(Event::Error(e));
            }
            // Dropping `tx` tells the palette the message is done.
            drop(tx);
            ctx.request_repaint();
        })
        .map_err(|e| format!("The assistant could not start: {e}"))?;
    Ok(Run { rx, cancel, usage: [0; 3] })
}

#[cfg(target_arch = "wasm32")]
fn spawn(_cfg: cadcraft_ai::Config, _conv: Arc<Mutex<cadcraft_ai::Conversation>>, _text: String, _backend: InApp) -> Result<Run, String> {
    Err(crate::tl!("The AI assistant needs the desktop app.").into())
}

/// Enter without Shift, taken before the input sees it (Shift+Enter starts a new line there).
fn take_enter(i: &mut egui::InputState) -> bool {
    let n = i.events.len();
    i.events.retain(|e| !matches!(e, egui::Event::Key { key: egui::Key::Enter, pressed: true, modifiers, .. } if !modifiers.shift));
    i.events.len() != n
}

pub fn show(app: &mut CadApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    if cfg!(target_arch = "wasm32") {
        ui.add_space(8.0);
        ui.label(RichText::new(crate::tl!("The AI assistant needs the desktop app.")).color(t.text_dim));
        return;
    }
    let running = app.assistant.running();
    let margin = egui::Frame::NONE.fill(t.panel).inner_margin(egui::Margin::symmetric(6, 4));
    egui::Panel::top(egui::Id::new("cadcraft-assistant-header")).frame(margin).show(ui, |ui| {
        ui.horizontal(|ui| {
            let cfg = app.ai.config();
            let model = if cfg.model.trim().is_empty() { cfg.provider.default_model() } else { cfg.model.trim() };
            let provider = crate::i18n::t(cfg.provider.label());
            let title = if model.is_empty() { provider.to_string() } else { format!("{provider} · {model}") };
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if crate::icons::button(ui, crate::icons::Icon::Gear, 20.0, "AI Settings", false).clicked() {
                    app.ui.dialog = Some("options:ai".into());
                }
                if ui.add_enabled(!running, egui::Button::new(crate::tl!("New chat"))).clicked() {
                    app.assistant.clear();
                }
                ui.add(egui::Label::new(RichText::new(title).color(t.text_dim).font(crate::theme::small())).truncate());
            });
        });
    });
    egui::Panel::bottom(egui::Id::new("cadcraft-assistant-input")).frame(margin).show(ui, |ui| {
        ui.add_space(2.0);
        if !app.ai.is_configured() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(crate::tl!("No model is set up yet.")).color(t.text_dim));
                if ui.button(crate::tl!("Set up a model…")).clicked() {
                    app.ui.dialog = Some("options:ai".into());
                }
            });
        }
        // Enter sends; the message goes after the input took this frame's typing, so a syllable an
        // input method commits with that Enter is part of it. A focused input keeps the keyboard
        // from the command line and the shortcuts (`egui_wants_keyboard_input`).
        let id = egui::Id::new(INPUT_ID);
        let enter = ui.memory(|m| m.has_focus(id)) && ui.input_mut(take_enter);
        let field = egui::TextEdit::multiline(&mut app.assistant.input)
            .id(id)
            .hint_text(crate::tl!("Describe what to draw or model…"))
            .desired_rows(3)
            .desired_width(f32::INFINITY);
        let r = ui.add(field);
        crate::control::record_widget(ui, &r, "AI Assistant message", "textbox", None);
        if std::mem::take(&mut app.assistant.focus) {
            r.request_focus();
        }
        ui.horizontal(|ui| {
            if running {
                if ui.button(crate::tl!("Stop")).clicked() {
                    app.assistant.stop();
                }
            } else if ui.add_enabled(!app.assistant.input.trim().is_empty(), egui::Button::new(crate::tl!("Send"))).clicked() || enter {
                send(app, ui.ctx());
            }
        });
    });
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.panel).inner_margin(egui::Margin::symmetric(8, 6))).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("dock-assistant").auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            if app.assistant.items.is_empty() {
                welcome(app, ui);
            }
            for (i, item) in app.assistant.items.iter_mut().enumerate() {
                item_ui(ui, i, item);
                ui.add_space(6.0);
            }
            if running {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(crate::tl!("Working…")).color(t.text_dim));
                });
            }
        });
    });
}

/// An empty chat: a welcome and example requests; picking one puts it in the input.
fn welcome(app: &mut CadApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    ui.label(
        RichText::new(crate::tl!("Describe what you want, in any language, and the assistant drafts and models it in this drawing."))
            .color(t.text_dim),
    );
    ui.add_space(6.0);
    // In the interface language: the model answers in the language it is asked in.
    for example in cadcraft_ai::prompt::EXAMPLES.iter().map(|e| crate::i18n::t(e)) {
        if ui.add(egui::Button::new(RichText::new(example).font(crate::theme::small())).wrap()).clicked() {
            app.assistant.input = example.to_string();
            app.assistant.focus = true;
        }
    }
}

fn item_ui(ui: &mut egui::Ui, i: usize, item: &mut Item) {
    let t = Tokens::get();
    let small = |s: &str, c| RichText::new(s).font(crate::theme::small()).color(c);
    match item {
        Item::User(text) => {
            egui::Frame::NONE.fill(t.tab_active).corner_radius(6).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(text.as_str()).color(t.text)).wrap());
            });
        }
        Item::Text(text) => markdown(ui, text),
        Item::Thinking(text) => {
            egui::CollapsingHeader::new(small(crate::tl!("Thinking…"), t.text_faint)).id_salt(("assistant-item", i)).show(ui, |ui| {
                ui.add(egui::Label::new(small(text, t.text_faint)).wrap());
            });
        }
        Item::Tool { name, input, result, png, texture, .. } => {
            let (mark, color) = match result {
                None => ("…", t.text_faint),
                Some((true, _)) => ("✓", t.text_dim),
                Some((false, _)) => ("✗", t.warn),
            };
            // What it worked on: the command, the typed text or the view.
            let what = ["command", "text", "view", "filter"].iter().find_map(|k| input.get(*k).and_then(Value::as_str));
            let what = what.and_then(|w| w.lines().next()).map(|w| format!(" · {}", w.chars().take(48).collect::<String>())).unwrap_or_default();
            let head = RichText::new(format!("{mark} {name}{what}")).font(crate::theme::mono()).color(color);
            egui::CollapsingHeader::new(head).id_salt(("assistant-item", i)).show(ui, |ui| {
                let pretty = serde_json::to_string_pretty(input).unwrap_or_default();
                ui.add(egui::Label::new(RichText::new(pretty).font(crate::theme::mono()).color(t.text_dim)).wrap());
                if let Some((_, text)) = result {
                    ui.separator();
                    let mut shown: String = text.chars().take(MAX_SHOWN_RESULT).collect();
                    if shown.len() < text.len() {
                        shown.push('…');
                    }
                    ui.add(egui::Label::new(RichText::new(shown).font(crate::theme::mono()).color(t.text)).wrap());
                }
            });
            if let Some(b64) = png.take() {
                *texture = thumbnail(ui.ctx(), &b64);
            }
            if let Some(tex) = texture {
                ui.add(egui::Image::new(&*tex).max_width(THUMB.min(ui.available_width())));
            }
        }
        Item::Notice(text) => {
            ui.add(egui::Label::new(small(text, t.text_dim).italics()).wrap());
        }
        Item::Error(text) => {
            ui.add(egui::Label::new(RichText::new(text.as_str()).color(t.warn)).wrap());
        }
        Item::Usage([input, output, cached]) => {
            let s = crate::tf!("Tokens: {input} in · {output} out · {cached} cached", input = input, output = output, cached = cached);
            ui.label(small(&s, t.text_faint));
        }
    }
}

/// A tool's PNG as a texture no larger than twice the shown width.
fn thumbnail(ctx: &egui::Context, b64: &str) -> Option<egui::TextureHandle> {
    let bytes = cadcraft_engine::cmd::file::base64_decode(b64)?;
    let mut img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).ok()?;
    let max = 2 * THUMB as u32;
    if img.width() > max || img.height() > max {
        img = img.thumbnail(max, max);
    }
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(ctx.load_texture("assistant-image", egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()), egui::TextureOptions::LINEAR))
}

/// The model's Markdown, plainly: paragraphs, headings, bullets, code blocks and `inline code`.
fn markdown(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get();
    let body = TextFormat { font_id: crate::theme::body(), color: t.text, ..Default::default() };
    let code = TextFormat { font_id: crate::theme::mono(), color: t.text, background: t.field, ..Default::default() };
    let mut fenced = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            ui.label(RichText::new(line).font(crate::theme::mono()).color(t.text).background_color(t.field));
            continue;
        }
        if trimmed.is_empty() {
            ui.add_space(4.0);
            continue;
        }
        let indent = (line.len() - trimmed.len()) as f32 * 4.0;
        let (bullet, rest, format) = if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            ("• ", rest, body.clone())
        } else if trimmed.starts_with('#') {
            ("", trimmed.trim_start_matches('#').trim_start(), TextFormat { color: t.accent, ..body.clone() })
        } else {
            ("", trimmed, body.clone())
        };
        let mut job = LayoutJob::default();
        if !bullet.is_empty() || indent > 0.0 {
            job.append(bullet, indent, format.clone());
        }
        // Bold markers are dropped; text between backticks is code.
        for (k, part) in rest.replace("**", "").split('`').enumerate() {
            job.append(part, 0.0, if k % 2 == 1 { code.clone() } else { format.clone() });
        }
        ui.add(egui::Label::new(job).wrap());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_ai::ProviderKind;
    use cadcraft_engine::Session;

    use crate::Services;
    use crate::docking::Panel;

    fn app() -> CadApp {
        let mut app = CadApp::new(Session::new(), Services::default());
        // A local model needs no key: the tests never touch the keychain's other entries.
        app.ai.provider = ProviderKind::Custom;
        app.ai.custom_model = "test-model".into();
        app
    }

    fn png_base64() -> String {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([200, 40, 40, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        cadcraft_engine::cmd::file::base64_encode(&png)
    }

    #[test]
    fn assistant_and_ai_open_the_palette() {
        let mut a = app();
        assert!(!a.ui.docking.layout.contains(&Panel::Assistant));
        a.cmdline("AI");
        assert!(a.ui.docking.layout.contains(&Panel::Assistant));
        assert!(a.session.running.is_none());
        let _ = a.run("ui.dock", json!({"operation": "close", "panel": "assistant"}));
        assert!(!a.ui.docking.layout.contains(&Panel::Assistant));
        a.cmdline("assistant");
        assert!(a.ui.docking.layout.contains(&Panel::Assistant), "it comes back where it was");
        a.ui.docking = crate::docking::Workspace::default();
        crate::menus::activate(&mut a, "assistant");
        assert!(a.ui.docking.layout.contains(&Panel::Assistant));
        let tools = crate::menus::tree(&a).into_iter().find(|(m, _)| m == "Tools").map(|(_, e)| e).unwrap_or_default();
        assert!(tools.iter().any(|e| matches!(e, crate::menus::Entry::Item { id, label, .. } if id == "assistant" && label == "AI Assistant")));
    }

    /// The tools reach the drawing through the app's own channel, served between frames.
    #[test]
    fn in_app_backend_round_trips_through_drain_control() {
        let mut a = app();
        let _ = a.run("circle", json!({"center": [0, 0], "radius": 5}));
        let ctx = egui::Context::default();
        let mut backend = InApp { tx: a.assistant_tx.clone(), ctx: ctx.clone() };
        assert!(cadcraft_ai::tools::Backend::has_ui(&backend));
        assert_eq!(cadcraft_ai::tools::Backend::describe(&backend), "Cadino (in-app)");
        let worker = std::thread::spawn(move || {
            let inspect = cadcraft_ai::tools::call_tool(&mut backend, "inspect_drawing", &json!({"entities": false}));
            let render = cadcraft_ai::tools::call_tool(&mut backend, "render", &json!({"view": "iso", "width": 64, "height": 48}));
            let bad = cadcraft_ai::tools::Backend::call(&mut backend, "no.such.method", json!({}));
            (inspect, render, bad)
        });
        for _ in 0..2000 {
            if worker.is_finished() {
                break;
            }
            a.drain_control(&ctx);
            std::thread::sleep(Duration::from_millis(2));
        }
        let (inspect, render, bad) = worker.join().unwrap();
        let inspect = inspect.unwrap().to_string();
        assert!(inspect.contains("Circle") && !inspect.contains("isError\":true"), "{inspect}");
        let render = render.unwrap();
        assert_eq!(render["content"][0]["type"], "image", "{render}");
        assert!(bad.unwrap_err().contains("unknown method"));
    }

    #[test]
    fn scripted_events_render_without_panic() {
        let mut a = app();
        let mut usage = [0; 3];
        let events = [
            Event::Thinking("Plan the desk ".into()),
            Event::Thinking("top first.".into()),
            Event::Text("I'll draw a **1400 × 700** top:\n\n- corners `R30`\n  - nested\n# Done\n```\nline 0,0 1400,0\n```\n".into()),
            Event::ToolCall { id: "t1".into(), name: "execute".into(), input: json!({"command": "rectang", "params": {}}) },
            Event::ToolResult { id: "t1".into(), ok: true, text: "{\"handle\": \"2A\"}".into(), image_png_base64: None },
            Event::ToolCall { id: "t2".into(), name: "render".into(), input: json!({"view": "iso"}) },
            Event::ToolResult { id: "t2".into(), ok: true, text: String::new(), image_png_base64: Some(png_base64()) },
            Event::ToolCall { id: "t3".into(), name: "command_line".into(), input: json!("{not json") },
            Event::ToolResult { id: "t3".into(), ok: false, text: "x".repeat(MAX_SHOWN_RESULT + 10), image_png_base64: Some("@@not base64".into()) },
            Event::TurnDone {
                stop_reason: "tool_use".into(),
                usage: json!({"input_tokens": 1200, "output_tokens": 80, "cache_read_input_tokens": 1000}),
            },
            Event::TurnDone { stop_reason: "end_turn".into(), usage: Value::Null },
            Event::Notice("Stopped".into()),
            Event::Error("The model declined".into()),
            Event::Text("한국어 답변".into()),
        ];
        for e in events {
            apply(&mut a.assistant.items, &mut usage, e);
        }
        assert_eq!(usage, [1200, 80, 1000]);
        a.assistant.items.push(Item::Usage(usage));
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let raw =
                egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 700.0))), ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| show(&mut a, ui));
            out.textures_delta.clear();
        }
        let images: Vec<bool> =
            a.assistant.items.iter().filter_map(|i| if let Item::Tool { texture, .. } = i { Some(texture.is_some()) } else { None }).collect();
        assert_eq!(images, [false, true, false], "the render's PNG became a texture; a broken one is left out");
        // The thinking deltas joined; text after a tool call is a new entry.
        assert!(matches!(a.assistant.items.first(), Some(Item::Thinking(s)) if s == "Plan the desk top first."));
        assert!(matches!(a.assistant.items.iter().rev().nth(1), Some(Item::Text(s)) if s == "한국어 답변"));
        a.assistant.clear();
        assert!(a.assistant.items.is_empty() && lock(&a.assistant.conv).is_empty());
    }

    /// Enter in the chat input sends the message; the command line never sees the typing.
    #[test]
    fn enter_in_the_chat_input_does_not_reach_the_command_line() {
        use egui_kittest::Harness;
        let mut a = app();
        // No model: the message fails at once, without a network.
        a.ai.custom_model.clear();
        a.integrated_titlebar = true;
        a.ui.in_window_menu = false;
        let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_ui_state(
            |ui, app: &mut CadApp| {
                app.logic(&ui.ctx().clone());
                app.ui(ui);
            },
            a,
        );
        h.run_steps(3);
        h.state_mut().cmdline("AI");
        h.run_steps(3);
        assert!(h.ctx.memory(|m| m.has_focus(egui::Id::new(INPUT_ID))), "opening it puts the keyboard in the input");
        h.event(egui::Event::Text("line".into()));
        h.run_steps(2);
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Enter);
        h.event(egui::Event::Text("2".into()));
        h.run_steps(2);
        assert_eq!(h.state().assistant.input, "line\n2");
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        for _ in 0..500 {
            if !h.state().assistant.running() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
            h.run_steps(1);
        }
        let st = h.state();
        assert!(st.session.running.is_none(), "LINE did not start");
        assert!(st.cmd.buffer.is_empty(), "{:?}", st.cmd.buffer);
        assert!(st.assistant.input.is_empty());
        assert!(matches!(st.assistant.items.first(), Some(Item::User(s)) if s == "line\n2"));
        assert!(st.assistant.items.iter().any(|i| matches!(i, Item::Error(e) if e.contains("model"))), "the missing model is reported");
    }
}
