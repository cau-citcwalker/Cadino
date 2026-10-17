//! EXPORT, IMPORT, PUBLISH and PREVIEW from a menu, the tool bar or the command line: the first
//! three ask for a file, PREVIEW shows the plot in a window. So do the commands that read or
//! write one file (STEPIN, STEPOUT, STLOUT, DATALINK, APPLOAD, SCRIPT, ALIASEDIT), as AutoCAD's
//! do with FILEDIA 1. Calls with parameters (scripts, the control channel, MCP) never get here and
//! never open a dialog: they run the engine commands.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU32, Ordering};

use egui::TextureHandle;
use serde_json::{Value, json};

use crate::CadApp;

/// Bumped each time PREVIEW is started, so the window renders the drawing as it is now.
static PREVIEW_GEN: AtomicU32 = AtomicU32::new(0);

/// A kind of file to open: the file dialog's filter label and its extensions.
pub type OpenKind = (&'static str, &'static [&'static str]);

thread_local! {
    // ponytail: a hint beside `Services::pick_open` rather than a new picker signature, so hosts
    // and tests that build `Services` keep compiling; a typed picker if more kinds turn up.
    static OPEN_KIND: Cell<Option<OpenKind>> = const { Cell::new(None) };
    /// The file prompt the file dialog was shown for (the command's start count and the prompt).
    static ASKED: RefCell<Option<(u64, String)>> = const { RefCell::new(None) };
}

/// The kind of file the open dialog being shown asks for (`None`: a drawing), so the host can
/// list that kind first.
pub fn open_kind() -> Option<OpenKind> {
    OPEN_KIND.with(Cell::get)
}

/// Ask for a file of `kind` to open. `None`: cancelled.
fn pick_file(app: &CadApp, kind: OpenKind) -> Option<String> {
    let pick = app.services.pick_open.as_ref()?;
    OPEN_KIND.with(|k| k.set(Some(kind)));
    let picked = pick();
    OPEN_KIND.with(|k| k.set(None));
    picked
}

/// A command's file-name prompt (STLOUT's after its objects, DATALINK's after its table,
/// STEPIN's, IGESOUT's...) asks with the file dialog when FILEDIA is 1, as AutoCAD's do: the chosen
/// path is entered, and cancelling the dialog cancels the command. Without a file dialog, or with
/// FILEDIA 0, the name is typed. Called every frame.
pub fn file_prompt(app: &mut CadApp) {
    let Some(prompt) = app.session.current_prompt() else { return };
    let Some(cadcraft_engine::Picker::File { save, label, exts }) = prompt.picker else { return };
    let has_picker = if save { app.services.pick_save.is_some() } else { app.services.pick_open.is_some() };
    if !app.session.settings.filedia || !has_picker {
        return;
    }
    // Once per prompt: a path the command refuses leaves its prompt for typing.
    let key = (app.session.starts, prompt.message.clone());
    if ASKED.with(|a| a.borrow().as_ref() == Some(&key)) {
        return;
    }
    ASKED.with(|a| *a.borrow_mut() = Some(key));
    let picked = if save {
        let title = app.session.state().map(|s| s.title.clone()).unwrap_or_default();
        let stem = std::path::Path::new(&title).file_stem().map(|s| s.to_string_lossy().to_string()).filter(|s| !s.is_empty());
        let name = format!("{}.{}", stem.unwrap_or_else(|| "Drawing".into()), exts.first().copied().unwrap_or("dat"));
        app.services.pick_save.as_ref().and_then(|pick| pick(&name))
    } else {
        pick_file(app, (label, exts))
    };
    let input = match picked {
        Some(path) => cadcraft_engine::Input::Text(path),
        None => cadcraft_engine::Input::Cancel,
    };
    if let Err(e) = app.session.input(input) {
        app.session.echo(e.to_string());
    }
}

/// Handle a typed or menu-invoked file exchange command (`id` may be an alias).
pub fn route(app: &mut CadApp, id: &str) -> Result<Value, String> {
    let title = app.session.state().map(|s| s.title.clone()).unwrap_or_default();
    let stem = std::path::Path::new(&title).file_stem().map(|s| s.to_string_lossy().to_string()).filter(|s| !s.is_empty());
    let stem = stem.unwrap_or_else(|| "Drawing".into());
    match id {
        // STLOUT exports the objects selected first; with none, it asks for them, then (as its
        // file prompt) for the file.
        "stlout" if app.session.selection().is_empty() => start(app, "stlout"),
        "stepout" | "stlout" => {
            let (name, ext) = if id == "stepout" { ("STEPOUT", "stp") } else { ("STLOUT", "stl") };
            let Some(pick) = app.services.pick_save.as_ref() else { return no_picker(app, name, &format!(r#"{id} {{"path": "part.{ext}"}}"#)) };
            match pick(&format!("{stem}.{ext}")) {
                Some(path) => app.run(id, json!({ "path": path })),
                None => Ok(Value::Null),
            }
        }
        "stepin" | "datalink" | "appload" | "script" | "aliasedit" => {
            let (name, kind, example): (&str, OpenKind, &str) = match id {
                "stepin" => ("STEPIN", ("STEP (STP, STEP)", &["stp", "step"]), r#"stepin {"path": "part.stp"}"#),
                "datalink" => ("DATALINK", ("CSV (CSV, TXT, TSV)", &["csv", "txt", "tsv"]), r#"datalink {"path": "data.csv"}"#),
                "appload" => ("APPLOAD", ("LISP (LSP)", &["lsp"]), r#"appload {"path": "tools.lsp"}"#),
                "script" => ("SCRIPT", ("Script (SCR)", &["scr"]), r#"script {"path": "setup.scr"}"#),
                _ => ("ALIASEDIT", ("PGP", &["pgp"]), r#"aliasedit {"file": "acad.pgp"}"#),
            };
            if app.services.pick_open.is_none() {
                return no_picker(app, name, example);
            }
            // DATALINK links the selected table's cells to the file; with no table selected, it asks
            // for one, then (as its file prompt) for the file.
            if id == "datalink" && !table_selected(app) {
                return start(app, "datalink");
            }
            let Some(path) = pick_file(app, kind) else { return Ok(Value::Null) };
            match id {
                // As a LISP file dropped on the window loads: its routine may wait for the user.
                "appload" => match cadcraft_engine::lisp::builtins::read_text_file(&path) {
                    Some(text) => {
                        app.load_lisp(&path, &text);
                        Ok(Value::Null)
                    }
                    None => {
                        let msg = format!("APPLOAD: can't read {path}");
                        app.session.echo(msg.clone());
                        Err(msg)
                    }
                },
                "aliasedit" => {
                    let r = app.run("aliasedit", json!({ "file": path }));
                    if let Ok(v) = &r {
                        app.session.echo(format!("{} command alias(es) loaded from {path}.", v["loaded"].as_u64().unwrap_or(0)));
                    }
                    r
                }
                _ => app.run(id, json!({ "path": path })),
            }
        }
        "preview" | "pre" => {
            PREVIEW_GEN.fetch_add(1, Ordering::Relaxed);
            app.ui.dialog = Some("preview".into());
            Ok(Value::Null)
        }
        "import" | "imp" => {
            let Some(pick) = app.services.pick_open.as_ref() else { return no_picker(app, "IMPORT", r#"import {"path": "parts.dxf"}"#) };
            match pick() {
                Some(path) => app.run("import", json!({ "path": path })),
                None => Ok(Value::Null),
            }
        }
        "publish" => {
            let Some(pick) = app.services.pick_save.as_ref() else { return no_picker(app, "PUBLISH", "publish {}") };
            match pick(&format!("{stem}.pdf")) {
                Some(path) => app.run("publish", json!({ "path": path })),
                None => Ok(Value::Null),
            }
        }
        _ => {
            let Some(pick) = app.services.pick_save.as_ref() else { return no_picker(app, "EXPORT", r#"export {"format": "svg"}"#) };
            match pick(&format!("{stem}.svg")) {
                Some(path) => app.run("export", json!({ "path": path })),
                None => Ok(Value::Null),
            }
        }
    }
}

/// Start the command's prompts; its file prompt then asks with the file dialog ([`file_prompt`]).
fn start(app: &mut CadApp, id: &str) -> Result<Value, String> {
    app.session.start(id).map(|()| Value::Null).map_err(|e| {
        app.session.echo(e.to_string());
        e.to_string()
    })
}

fn table_selected(app: &CadApp) -> bool {
    let Ok(d) = app.session.doc() else { return false };
    app.session.selection().iter().any(|h| d.entity(*h).is_some_and(|e| matches!(e.kind, cadcraft_doc::EntityKind::Table(_))))
}

fn no_picker(app: &mut CadApp, name: &str, example: &str) -> Result<Value, String> {
    let msg = format!("{name}: there is no file dialog here; run it with parameters, e.g. {example}");
    app.session.echo(msg.clone());
    Err(msg)
}

#[derive(Clone)]
struct Preview {
    generation: u32,
    shown: Result<(TextureHandle, String), String>,
}

/// A preview the Plot dialog asked for: its PLOT parameters (PNG output) and a caption.
#[derive(Clone)]
struct FromPlot {
    params: Value,
    info: String,
}

const FROM_PLOT: &str = "cc_plot_preview_from_plot";

/// Show the Plot dialog's settings as they plot (PLOT with `params`, a PNG); Print... and Close
/// return to the dialog.
pub(crate) fn preview_plot(app: &mut CadApp, ctx: &egui::Context, params: Value, info: String) {
    PREVIEW_GEN.fetch_add(1, Ordering::Relaxed);
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(FROM_PLOT), FromPlot { params, info }));
    app.ui.dialog = Some("preview".into());
}

/// Render the plot preview of the current layout (or model), or of the Plot dialog's settings,
/// as a texture.
fn render(app: &mut CadApp, ctx: &egui::Context) -> Result<(TextureHandle, String), String> {
    if let Some(f) = ctx.data(|d| d.get_temp::<FromPlot>(egui::Id::new(FROM_PLOT))) {
        let r = app.session.execute("plot", &f.params).map_err(|e| e.to_string())?;
        let png = r["data"].as_str().and_then(cadcraft_engine::cmd::file::base64_decode).ok_or("the preview has no image")?;
        let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).map_err(|e| e.to_string())?.to_rgba8();
        let size = [img.width() as usize, img.height() as usize];
        let tex = ctx.load_texture("plot-preview", egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR);
        return Ok((tex, f.info));
    }
    let r = app.session.execute("preview", &json!({ "width": 1600 })).map_err(|e| e.to_string())?;
    let png = r["data"].as_str().and_then(cadcraft_engine::cmd::file::base64_decode).ok_or("the preview has no image")?;
    let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).map_err(|e| e.to_string())?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let tex = ctx.load_texture("plot-preview", egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR);
    let mut info = format!("{} · {}", r["layout"].as_str().unwrap_or(""), r["paper"].as_str().unwrap_or(""));
    if r["landscape"].as_bool() == Some(true) {
        info.push_str(" · landscape");
    }
    Ok((tex, info))
}

/// The Plot Preview window: the sheet as it plots, with Print and Close.
pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let id = egui::Id::new("cc_plot_preview");
    let generation = PREVIEW_GEN.load(Ordering::Relaxed);
    let cached = ctx.data(|d| d.get_temp::<Preview>(id)).filter(|p| p.generation == generation);
    let preview = match cached {
        Some(p) => p,
        None => {
            let p = Preview { generation, shown: render(app, ctx) };
            ctx.data_mut(|d| d.insert_temp(id, p.clone()));
            p
        }
    };
    let (mut close, mut print) = (false, false);
    egui::Window::new("Plot Preview").open(open).collapsible(false).resizable(false).show(ctx, |ui| {
        match &preview.shown {
            Ok((tex, info)) => {
                ui.label(info);
                let size = tex.size_vec2();
                let k = (760.0 / size.x).min(540.0 / size.y).min(1.0);
                ui.image((tex.id(), size * k));
            }
            Err(e) => {
                ui.label(e);
            }
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            print = ui.button("Print...").clicked();
            close = ui.button("Close").clicked();
        });
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }
    if close || print {
        *open = false;
    }
    let from_plot = ctx.data(|d| d.get_temp::<FromPlot>(egui::Id::new(FROM_PLOT))).is_some();
    if !*open {
        ctx.data_mut(|d| {
            d.remove::<Preview>(id);
            d.remove::<FromPlot>(egui::Id::new(FROM_PLOT));
        });
    }
    if from_plot && !*open {
        // Back to the Plot dialog with its settings; Print... plots them.
        crate::managers::set_flag(ctx, crate::managers::PLOT_NOW, print);
        app.ui.dialog = Some("plot".into());
    } else if print {
        app.start("plot");
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use cadcraft_engine::Session;
    use serde_json::json;

    use crate::{CadApp, Services};

    /// An app whose file dialogs record what they were asked and cancel.
    fn app() -> (CadApp, Rc<RefCell<Vec<String>>>) {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let (a, b) = (asked.clone(), asked.clone());
        let services = Services {
            pick_open: Some(Box::new(move || {
                a.borrow_mut().push("open".into());
                None
            })),
            pick_save: Some(Box::new(move |name: &str| {
                b.borrow_mut().push(name.to_string());
                None
            })),
        };
        (CadApp::new(Session::new(), services), asked)
    }

    #[test]
    fn typed_and_menu_commands_ask_json_calls_never_do() {
        let (mut app, asked) = app();
        app.start("export");
        app.cmdline("PUBLISH");
        app.start("imp");
        assert_eq!(*asked.borrow(), ["Drawing1.svg", "Drawing1.pdf", "open"]);
        assert!(app.ui.dialog.is_none());
        app.cmdline("pre");
        assert_eq!(app.ui.dialog.as_deref(), Some("preview"));
        app.ui.dialog = None;
        app.start("preview");
        assert_eq!(app.ui.dialog.as_deref(), Some("preview"));
        app.ui.dialog = None;

        let _ = app.run("export", json!({ "format": "svg" }));
        let _ = app.run("import", json!({}));
        let _ = app.run("publish", json!({ "layouts": [] }));
        let r = app.run("preview", json!({ "width": 64 })).unwrap();
        assert!(r["data"].is_string());
        assert_eq!(asked.borrow().len(), 3, "JSON calls never open a file dialog");
        assert!(app.ui.dialog.is_none(), "nor the preview window");

        let e = app.run("share", json!({})).unwrap_err();
        assert!(e.contains("not available yet"), "{e}");
    }
}
