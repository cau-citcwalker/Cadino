//! DesignCenter (ADCENTER, ADC, DC; Ctrl+2): a palette to browse drawings and bring their
//! named content into the current one. Folders: Favorites and the drives (or the file system
//! root and the home folder) as a tree, drawings (.dwg, .dxf, .dwt) in their folders; Open
//! Drawings: the drawings open in this session. A drawing opens into its content types (Blocks,
//! Dimstyles, Layers, Layouts, Linetypes, Multileaderstyles, Tablestyles, Textstyles, Xrefs);
//! a content type lists its items, blocks as thumbnails drawn by the render crate. Double-click,
//! Insert or Add to Drawing copies the definition through the `adcenter` command (one undo step;
//! a name this drawing has keeps its own), then a block is placed with INSERT's prompts. Files
//! are read through the io hooks and never while file access is off. DesignCenter Online is not
//! part of it.
// ponytail: Favorites last while the app runs; no History tab, search or drag and drop.

use std::collections::HashMap;
use std::sync::Arc;

use cadcraft_color::Rgb;
use cadcraft_doc::Drawing;
use cadcraft_engine::cmd::adcenter::{KINDS, names};
use cadcraft_geom::Vec2;
use cadcraft_render::DisplayList;
use egui::{RichText, Sense, Stroke, vec2};
use serde_json::{Value, json};

use crate::CadApp;
use crate::canvas::Xf;
use crate::theme::Tokens;

/// Most entries listed per folder (a huge folder stays usable).
const MAX_ENTRIES: usize = 2000;
/// Thumbnails built per frame; the rest come on the next frames.
const THUMBS_PER_FRAME: usize = 6;
/// Thumbnails kept.
const MAX_THUMBS: usize = 2000;

#[derive(Clone, Copy, Default, PartialEq)]
enum Tab {
    #[default]
    Folders,
    Open,
}

/// The drawing whose content is shown.
#[derive(Clone, Debug, PartialEq)]
enum Source {
    File(String),
    /// An open drawing, by its session uid.
    Open(u64),
}

#[derive(Clone, Default)]
struct St {
    tab: Tab,
    source: Option<Source>,
    kind: Option<&'static str>,
    sel: Option<String>,
    favorites: Vec<String>,
    /// Folder listings: (name, is a folder), folders first.
    dirs: HashMap<String, Arc<Vec<(String, bool)>>>,
    /// Drawings read from files (or why they could not be).
    files: HashMap<String, Result<Arc<Drawing>, String>>,
    /// Block thumbnails by (source, block).
    thumbs: HashMap<(String, String), Arc<DisplayList>>,
}

fn id() -> egui::Id {
    egui::Id::new("designcenter")
}

/// The palette's label for a content type.
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "block" => "Blocks",
        "dimstyle" => "Dimstyles",
        "layer" => "Layers",
        "layout" => "Layouts",
        "linetype" => "Linetypes",
        "mleaderstyle" => "Multileaderstyles",
        "tablestyle" => "Tablestyles",
        "textstyle" => "Textstyles",
        _ => "Xrefs",
    }
}

fn is_drawing(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.ends_with(".dwg") || l.ends_with(".dxf") || l.ends_with(".dwt")
}

/// The subfolders and drawings of `dir`, folders first, each sorted by name (hidden ones left out).
fn list_dir(dir: &str) -> Vec<(String, bool)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(String, bool)> = rd
        .filter_map(Result::ok)
        .take(MAX_ENTRIES * 4)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let folder = e.file_type().is_ok_and(|t| t.is_dir());
            (!name.starts_with('.') && (folder || is_drawing(&name))).then_some((name, folder))
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    out.truncate(MAX_ENTRIES);
    out
}

/// Where the folder tree starts: the drives (Windows) or the root, and the home folder.
fn roots() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if cfg!(windows) {
        out.extend((b'A'..=b'Z').map(|c| format!("{}:\\", c as char)).filter(|d| std::path::Path::new(d).is_dir()));
    } else if std::path::Path::new("/").is_dir() {
        out.push("/".into());
    }
    out.extend(std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok().filter(|h| std::path::Path::new(h).is_dir()));
    out
}

fn join(dir: &str, name: &str) -> String {
    std::path::Path::new(dir).join(name).to_string_lossy().to_string()
}

fn read_drawing(path: &str) -> Result<Arc<Drawing>, String> {
    if let Some(why) = cadcraft_engine::limits::files_denied() {
        return Err(why.into());
    }
    let hooks = cadcraft_engine::cmd::file::io().ok_or_else(|| crate::tl!("Drawing files can't be read in this build.").to_string())?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    (hooks.read)(&bytes, path).map(Arc::new)
}

impl St {
    fn dir(&mut self, path: &str) -> Arc<Vec<(String, bool)>> {
        self.dirs.entry(path.to_string()).or_insert_with(|| Arc::new(list_dir(path))).clone()
    }
    /// The source's drawing, reading a file the first time.
    fn drawing(&mut self, app: &CadApp, src: &Source) -> Result<Arc<Drawing>, String> {
        match src {
            Source::Open(uid) => app
                .session
                .docs
                .iter()
                .find(|d| d.uid == *uid)
                .map(|d| d.doc.clone())
                .ok_or_else(|| crate::tl!("That drawing is closed.").to_string()),
            Source::File(path) => self.files.entry(path.clone()).or_insert_with(|| read_drawing(path)).clone(),
        }
    }
    fn select(&mut self, src: Source, kind: Option<&'static str>) {
        if self.source.as_ref() != Some(&src) || self.kind != kind {
            self.sel = None;
        }
        self.source = Some(src);
        self.kind = kind;
    }
}

fn key(src: &Source) -> String {
    match src {
        Source::File(p) => p.clone(),
        Source::Open(uid) => format!("#{uid}"),
    }
}

/// Open (or bring forward) the DesignCenter palette, floating the first time.
pub fn open(app: &mut CadApp) -> Result<Value, String> {
    let panel = crate::docking::Panel::DesignCenter;
    let first = !app.ui.docking.layout.contains(&panel) && !app.ui.docking.hidden.iter().any(|(p, _)| *p == panel);
    let r = crate::docking::command(app, false, &json!({ "operation": "open", "panel": panel.id() }))?;
    if !first {
        return Ok(r);
    }
    let area = app.canvas.rect.unwrap_or(egui::Rect::from_min_size(egui::pos2(220.0, 80.0), vec2(1000.0, 800.0)));
    let (w, h) = ((area.width() - 48.0).clamp(360.0, 680.0), (area.height() - 48.0).clamp(280.0, 480.0));
    let rect = [area.left() + 24.0, area.top() + 24.0, w, h];
    crate::docking::command(app, false, &json!({ "operation": "float", "panel": panel.id(), "rect": rect }))
}

/// The UI side of ADCENTER, ADC and DC typed or picked bare: the palette.
pub fn route(app: &mut CadApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    (matches!(id, "adcenter" | "adc" | "dc") && params.is_null()).then(|| open(app))
}

/// Copy `name` of `kind` from `src` into the current drawing; a block is then placed with
/// INSERT's prompts.
fn add(app: &mut CadApp, src: &Source, kind: &str, name: &str) {
    let mut p = match src {
        Source::File(path) => json!({ "file": path }),
        Source::Open(uid) => match app.session.docs.iter().position(|d| d.uid == *uid) {
            Some(i) => json!({ "document": i }),
            None => return,
        },
    };
    p["kind"] = json!(kind);
    p["name"] = json!(name);
    // A command still running would take this undo step into its own.
    if app.session.running.is_some() {
        app.session.cancel();
    }
    if app.run("adcenter", p).is_ok() && kind == "block" {
        crate::dialogs::insert_block(app, name);
    }
}

/// Build the thumbnail of block `name` of `d` (fitted later; about half a pixel of 64 points).
fn thumbnail(d: &Drawing, name: &str) -> Option<Arc<DisplayList>> {
    let b = d.block(name)?;
    let rough = cadcraft_render::build_entities(d, b.entities.iter().map(|e| e.as_ref()), &cadcraft_render::Options::default());
    if rough.bounds.is_empty() {
        return Some(Arc::new(rough));
    }
    let size = rough.bounds.width().max(rough.bounds.height());
    let opts = cadcraft_render::Options { tolerance: (size / 128.0).max(1e-9), ..Default::default() };
    Some(Arc::new(cadcraft_render::build_entities(d, b.entities.iter().map(|e| e.as_ref()), &opts)))
}

/// Draw `list` fitted into `rect`.
fn draw_fitted(ui: &egui::Ui, rect: egui::Rect, list: &DisplayList) {
    let t = Tokens::get();
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, t.canvas);
    let b = list.bounds;
    if b.is_empty() || list.prims.is_empty() {
        return;
    }
    let inner = rect.shrink(6.0);
    let (w, h) = (b.width().max(1e-9), b.height().max(1e-9));
    let scale = (f64::from(inner.width()) / w).min(f64::from(inner.height()) / h);
    let xf = Xf::new(rect, Vec2::new((b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0), scale);
    crate::canvas::draw_list(&p, &xf, list, Rgb(t.canvas.r(), t.canvas.g(), t.canvas.b()), false);
}

/// A drawing's node: selecting it shows its content types; open, it lists them.
fn drawing_node(ui: &mut egui::Ui, st: &mut St, src: Source, label: String) {
    // The same drawing can show under Favorites and under its drive: ids come from the branch.
    let node = ui.make_persistent_id(("dc_node", key(&src)));
    let mut clicked = false;
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), node, false)
        .show_header(ui, |ui| {
            clicked = ui.selectable_label(st.source.as_ref() == Some(&src) && st.kind.is_none(), label).clicked();
            if clicked {
                st.select(src.clone(), None);
            }
        })
        .body(|ui| {
            for k in KINDS {
                if ui.selectable_label(st.source.as_ref() == Some(&src) && st.kind == Some(k), crate::i18n::t(kind_label(k))).clicked() {
                    st.select(src.clone(), Some(k));
                }
            }
        });
    // Choosing a drawing opens it in the tree too, so its content types show there.
    if clicked {
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), node, false);
        state.set_open(true);
        state.store(ui.ctx());
    }
}

/// A folder's node, listing its subfolders and drawings when opened.
fn folder_node(ui: &mut egui::Ui, st: &mut St, path: &str, label: &str, depth: usize) {
    let node = ui.make_persistent_id(("dc_dir", path));
    let mut clicked = false;
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), node, false)
        .show_header(ui, |ui| {
            // The name opens and closes the folder as its arrow does.
            clicked = ui.selectable_label(false, label).clicked();
        })
        .body(|ui| {
            // Each level opens only on a click, but a deep tree stays bounded.
            if depth > 64 {
                return;
            }
            let entries = st.dir(path);
            if entries.is_empty() {
                ui.label(RichText::new(crate::tl!("(no folders or drawings)")).color(Tokens::get().text_dim));
            }
            for (name, folder) in entries.iter() {
                let full = join(path, name);
                if *folder {
                    folder_node(ui, st, &full, name, depth + 1);
                } else {
                    drawing_node(ui, st, Source::File(full), name.clone());
                }
            }
        });
    if clicked {
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), node, false);
        state.toggle(ui);
        state.store(ui.ctx());
    }
}

fn tree(app: &CadApp, ui: &mut egui::Ui, st: &mut St) {
    match st.tab {
        Tab::Folders => {
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), egui::Id::new("dc_favorites"), true)
                .show_header(ui, |ui| {
                    ui.label(RichText::new(crate::tl!("Favorites")).strong());
                })
                .body(|ui| {
                    if st.favorites.is_empty() {
                        ui.label(RichText::new(crate::tl!("Add a folder with Add to Favorites.")).color(Tokens::get().text_dim));
                    }
                    for f in st.favorites.clone() {
                        let name = std::path::Path::new(&f).file_name().map_or_else(|| f.clone(), |n| n.to_string_lossy().to_string());
                        folder_node(ui, st, &f, &name, 0);
                    }
                });
            let roots = roots();
            if roots.is_empty() {
                ui.label(RichText::new(crate::tl!("Folders can't be browsed here: use Open Drawings.")).color(Tokens::get().text_dim));
            }
            for r in roots {
                folder_node(ui, st, &r, &r, 0);
            }
        }
        Tab::Open => {
            for d in &app.session.docs {
                drawing_node(ui, st, Source::Open(d.uid), d.title.clone());
            }
        }
    }
}

/// The content of the selected source: its content types, or the items of one.
fn content(app: &mut CadApp, ui: &mut egui::Ui, st: &mut St) {
    let t = Tokens::get();
    let Some(src) = st.source.clone() else {
        ui.label(RichText::new(crate::tl!("Choose a drawing in the tree.")).color(t.text_dim));
        return;
    };
    let d = match st.drawing(app, &src) {
        Ok(d) => d,
        Err(e) => {
            ui.label(RichText::new(e).color(t.text_dim));
            return;
        }
    };
    let Some(kind) = st.kind else {
        for k in KINDS {
            let n = names(&d, k).len();
            if ui.selectable_label(false, format!("{}  ({n})", crate::i18n::t(kind_label(k)))).clicked() {
                st.select(src.clone(), Some(k));
            }
        }
        return;
    };
    let items = names(&d, kind);
    let mut chosen: Option<String> = None;
    let mut built = 0;
    egui::ScrollArea::vertical().id_salt("dc_content").auto_shrink([false, false]).max_height((ui.available_height() - 64.0).max(80.0)).show(
        ui,
        |ui| {
            if items.is_empty() {
                ui.label(RichText::new(crate::tl!("Nothing of this type in the drawing.")).color(t.text_dim));
            }
            if kind == "block" {
                ui.horizontal_wrapped(|ui| {
                    for n in &items {
                        let (rect, r) = ui.allocate_exact_size(vec2(88.0, 100.0), Sense::click());
                        let selected = st.sel.as_deref() == Some(n.as_str());
                        if selected || r.hovered() {
                            ui.painter().rect_filled(rect, 4.0, if selected { t.accent.gamma_multiply(0.35) } else { t.control_hover });
                        }
                        let thumb = egui::Rect::from_min_size(rect.min + vec2(8.0, 4.0), vec2(72.0, 72.0));
                        let k = (key(&src), n.clone());
                        if !st.thumbs.contains_key(&k) && built < THUMBS_PER_FRAME && st.thumbs.len() < MAX_THUMBS && ui.is_rect_visible(rect) {
                            built += 1;
                            if let Some(list) = thumbnail(&d, n) {
                                st.thumbs.insert(k.clone(), list);
                            }
                        }
                        match st.thumbs.get(&k) {
                            Some(list) => draw_fitted(ui, thumb, list),
                            None => {
                                ui.painter().rect_filled(thumb, 3.0, t.canvas);
                            }
                        }
                        let short: String = if n.chars().count() > 13 { n.chars().take(12).chain(['…']).collect() } else { n.clone() };
                        ui.painter_at(rect).text(
                            egui::pos2(rect.center().x, rect.bottom() - 12.0),
                            egui::Align2::CENTER_CENTER,
                            short,
                            crate::theme::small(),
                            t.text,
                        );
                        let r = r.on_hover_text(n);
                        if r.clicked() {
                            st.sel = Some(n.clone());
                        }
                        if r.double_clicked() {
                            chosen = Some(n.clone());
                        }
                    }
                });
            } else {
                for n in &items {
                    ui.horizontal(|ui| {
                        if kind == "layer"
                            && let Some(l) = d.layer(n)
                        {
                            let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                            crate::layers::swatch(ui.painter(), r, l.color);
                        }
                        let r = ui.selectable_label(st.sel.as_deref() == Some(n.as_str()), n);
                        if r.clicked() {
                            st.sel = Some(n.clone());
                        }
                        if r.double_clicked() {
                            chosen = Some(n.clone());
                        }
                    });
                }
            }
        },
    );
    if built == THUMBS_PER_FRAME {
        ui.ctx().request_repaint();
    }
    ui.separator();
    let sel = st.sel.clone().filter(|s| items.contains(s));
    ui.horizontal(|ui| {
        let label = match kind {
            "block" => crate::tl!("Insert"),
            "xref" => crate::tl!("Attach"),
            _ => crate::tl!("Add to Drawing"),
        };
        if ui.add_enabled(sel.is_some(), egui::Button::new(label)).clicked() {
            chosen = sel.clone();
        }
        if kind == "block"
            && let Some(desc) = sel.as_ref().and_then(|n| d.block(n)).map(|b| b.description.clone()).filter(|s| !s.is_empty())
        {
            ui.label(RichText::new(desc).color(t.text_dim));
        }
    });
    if let Some(n) = chosen {
        add(app, &src, kind, &n);
    }
}

pub fn show(app: &mut CadApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    let mut st: St = ui.ctx().data_mut(|d| d.remove_temp(id())).unwrap_or_default();
    ui.horizontal(|ui| {
        ui.selectable_value(&mut st.tab, Tab::Folders, crate::tl!("Folders"));
        ui.selectable_value(&mut st.tab, Tab::Open, crate::tl!("Open Drawings"));
        ui.separator();
        if app.services.pick_open.is_some()
            && ui.button(crate::tl!("Load...")).on_hover_text(crate::tl!("Show the content of a drawing file")).clicked()
            && let Some(path) = app.services.pick_open.as_ref().and_then(|f| f())
        {
            st.files.remove(&path);
            st.select(Source::File(path), None);
        }
        let folder = match &st.source {
            Some(Source::File(p)) => std::path::Path::new(p).parent().map(|d| d.to_string_lossy().to_string()),
            _ => None,
        };
        if let Some(f) = folder.filter(|f| !f.is_empty() && !st.favorites.contains(f))
            && ui.button(crate::tl!("Add to Favorites")).on_hover_text(f.clone()).clicked()
        {
            st.favorites.push(f);
        }
        if ui.button(crate::tl!("Refresh")).on_hover_text(crate::tl!("Read the folders and drawings again")).clicked() {
            st.dirs.clear();
            st.files.clear();
            st.thumbs.clear();
        }
    });
    ui.separator();
    let wide = ui.available_width() >= 480.0;
    let tree_size = if wide {
        vec2((ui.available_width() * 0.38).clamp(160.0, 320.0), ui.available_height())
    } else {
        vec2(ui.available_width(), (ui.available_height() * 0.4).max(100.0))
    };
    let layout = if wide { egui::Layout::left_to_right(egui::Align::Min) } else { egui::Layout::top_down(egui::Align::Min) };
    ui.with_layout(layout, |ui| {
        ui.allocate_ui(tree_size, |ui| {
            egui::ScrollArea::both().id_salt("dc_tree").auto_shrink([false, false]).show(ui, |ui| {
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| tree(app, ui, &mut st));
            });
        });
        if wide {
            ui.painter().vline(ui.cursor().left(), ui.max_rect().y_range(), Stroke::new(1.0, t.border));
            ui.add_space(6.0);
        } else {
            ui.separator();
        }
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| content(app, ui, &mut st));
    });
    ui.ctx().data_mut(|d| d.insert_temp(id(), st));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> CadApp {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.run("layer.new", json!({ "name": "Furniture", "color": 3 })).unwrap();
        app.run("layer.current", json!({ "name": "Furniture" })).unwrap();
        app.run("circle", json!({ "center": [0, 0], "radius": 2 })).unwrap();
        let h = app.session.doc().unwrap().model.iter().next().unwrap().handle.hex();
        app.run("block", json!({ "name": "Stool", "base": [0, 0], "handles": [h], "keep": "delete" })).unwrap();
        app.run("new", json!({})).unwrap();
        app
    }

    #[test]
    fn typed_names_open_the_palette() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        for typed in ["adcenter", "ADC", "dc"] {
            app.ui.docking = crate::docking::Workspace::default();
            app.cmdline(typed);
            assert!(app.ui.docking.layout.contains(&crate::docking::Panel::DesignCenter), "{typed}");
        }
        // With parameters it is the command: it lists, it opens nothing.
        app.ui.docking = crate::docking::Workspace::default();
        let r = app.run("adcenter", json!({})).unwrap();
        assert!(r["layers"].as_array().is_some_and(|l| !l.is_empty()));
        assert!(!app.ui.docking.layout.contains(&crate::docking::Panel::DesignCenter));
    }

    #[test]
    fn adding_a_block_from_an_open_drawing_starts_insert() {
        let mut app = library();
        let src = Source::Open(app.session.docs[0].uid);
        add(&mut app, &src, "layer", "Furniture");
        assert!(app.session.doc().unwrap().layer("Furniture").is_some());
        add(&mut app, &src, "block", "Stool");
        assert!(app.session.doc().unwrap().block("Stool").is_some());
        assert!(app.session.running.as_ref().is_some_and(|r| r.id == "insert"), "INSERT asks where");
        assert_eq!(app.session.current_prompt().map(|p| p.message), Some("Specify insertion point".to_string()));
        // The library's thumbnail has the circle's extent.
        let lib = app.session.docs[0].doc.clone();
        let list = thumbnail(&lib, "Stool").unwrap();
        assert!((list.bounds.width() - 4.0).abs() < 0.1, "{:?}", list.bounds);
        assert!(thumbnail(&lib, "Nope").is_none());
    }

    #[test]
    fn folders_list_drawings_and_files_respect_file_access() {
        let dir = std::env::temp_dir().join(format!("cadino-dc-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for f in ["b.dxf", "A.DWG", "notes.txt", ".hidden.dxf"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let listed = list_dir(&dir.to_string_lossy());
        assert_eq!(listed, vec![("sub".to_string(), true), ("A.DWG".to_string(), false), ("b.dxf".to_string(), false)]);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(list_dir("/no/such/folder").is_empty());
        let _deny = cadcraft_engine::limits::deny_files("files are off");
        assert_eq!(read_drawing("x.dxf").err(), Some("files are off".to_string()));
    }

    #[test]
    fn the_palette_draws_every_view() {
        let mut app = library();
        let ctx = egui::Context::default();
        let src = Source::Open(app.session.docs[0].uid);
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 600.0))), ..Default::default() };
        for (tab, kind) in [(Tab::Folders, None), (Tab::Open, None), (Tab::Open, Some("layer")), (Tab::Open, Some("block"))] {
            ctx.data_mut(|d| d.insert_temp(id(), St { tab, source: Some(src.clone()), kind, sel: Some("Stool".into()), ..St::default() }));
            for _ in 0..2 {
                let mut out = ctx.run_ui(input.clone(), |ui| show(&mut app, ui));
                out.textures_delta.clear();
            }
        }
        let st: St = ctx.data_mut(|d| d.get_temp(id())).unwrap_or_default();
        assert!(st.thumbs.contains_key(&(key(&src), "Stool".to_string())), "the block's thumbnail was built");
    }
}
