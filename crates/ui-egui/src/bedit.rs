//! The Edit Block Definition dialog: BEDIT typed or started from a menu or the Tool Sets with no
//! block reference selected. It lists the drawing's blocks with a preview and description; a name
//! no block has yet starts a new block. OK runs BEDIT with that name; the dialog itself never
//! changes the drawing. JSON calls (`bedit {name}`) and a selected block reference skip it.
//!
//! Also the Visibility States dialog (BVSTATE typed bare in the Block Editor), which runs
//! `bvstate` for each change, and the dynamic block the editor's canvas shows
//! ([`authored`]).

use std::sync::Arc;

use cadcraft_color::Rgb;
use cadcraft_engine::blockedit::CURRENT_DRAWING;
use cadcraft_engine::doc::dynblock::{DynamicBlock, ParameterKind};
use cadcraft_engine::doc::{Drawing, EntityKind};
use cadcraft_geom::Vec2;
use cadcraft_render::DisplayList;
use egui::{RichText, Stroke, vec2};
use serde_json::{Value, json};

use crate::CadApp;
use crate::canvas::Xf;
use crate::theme::Tokens;

/// The dialog's state while it is open.
#[derive(Default)]
pub struct State {
    /// "Block to create or edit".
    pub name: String,
    /// Why the last OK failed, shown until the name changes.
    pub error: Option<String>,
    /// The preview of the block named, for (drawing uid, revision, block name).
    preview: Option<((u64, u64, String), Arc<DisplayList>)>,
    /// The dynamic block the Block Editor shows and its base point, for (drawing uid, revision).
    authored: Option<((u64, u64), Option<(Arc<DynamicBlock>, Vec2)>)>,
    /// The Visibility States dialog, while it is open.
    vstates: Option<VStates>,
}

/// The Visibility States dialog's state.
#[derive(Default)]
struct VStates {
    /// The state selected in the list.
    selected: String,
    /// New or Rename asking for a name: (renaming, name typed, how a new state shows the
    /// objects: hide, show or current).
    form: Option<(bool, String, &'static str)>,
    error: Option<String>,
}

/// The blocks the dialog offers: named blocks that aren't external references, sorted by name.
pub fn editable_blocks(d: &Drawing) -> Vec<String> {
    let mut names: Vec<String> =
        d.blocks.values().filter(|b| !b.anonymous && !b.name.starts_with('*') && b.xref_path.is_none()).map(|b| b.name.clone()).collect();
    names.sort_by_key(|n| n.to_lowercase());
    // The drawing itself comes first, as in AutoCAD.
    names.insert(0, CURRENT_DRAWING.to_string());
    names
}

/// Opening a drawing with authoring elements (a dynamic block's own drawing) asks whether to
/// open it in the Block Editor, as AutoCAD does: Yes edits the drawing itself in the Block
/// Editor, No opens it as usual, Cancel closes it again.
pub fn authoring_dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let name = app
        .session
        .state()
        .ok()
        .and_then(|st| st.path.clone())
        .and_then(|p| std::path::Path::new(&p).file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_else(|| crate::i18n::t("This drawing").to_string());
    let mut choice: Option<&str> = None;
    egui::Window::new(crate::tl!("Open in Block Editor"))
        .id(egui::Id::new("Open in Block Editor"))
        .open(open)
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(format!("{name} {}", crate::i18n::t("contains authoring elements. Open in Block Editor?")));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(crate::tl!("Yes")).clicked() {
                    choice = Some("yes");
                }
                if ui.button(crate::tl!("No")).clicked() {
                    choice = Some("no");
                }
                if ui.button(crate::tl!("Cancel")).clicked() {
                    choice = Some("cancel");
                }
            });
        });
    let Some(c) = choice else { return };
    *open = false;
    let r = match c {
        "yes" => app.run("bedit", json!({ "name": CURRENT_DRAWING })).map(|_| ()),
        "cancel" => app.run("close", Value::Null).map(|_| ()),
        _ => Ok(()),
    };
    if let Err(e) = r {
        app.session.echo(e);
    }
}

/// BEDIT typed or from a menu without parameters opens the dialog, unless exactly one block
/// reference is selected (it opens at once) or the Block Editor is open (BEDIT says why not).
/// BVSTATE typed bare in the Block Editor opens the Visibility States dialog (with no visibility
/// parameter, the command says why not).
pub fn route(app: &mut CadApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id == "bvstate" && params.is_null() {
        visibility_states(app)?;
        open_visibility_states(app);
        return Some(Ok(Value::Null));
    }
    if !matches!(id, "bedit" | "be") || !params.is_null() {
        return None;
    }
    let st = app.session.state().ok()?;
    if st.block_edit.is_some() {
        return None;
    }
    let refs = st.selection.iter().filter(|h| st.doc.entity(**h).is_some_and(|e| matches!(e.kind, EntityKind::Insert(_)))).count();
    if refs == 1 && st.selection.len() == 1 {
        return None;
    }
    // Start on the block last edited if it is still there, else the first one.
    let names = editable_blocks(&st.doc);
    let last = app.bedit.name.clone();
    let name = if names.iter().any(|n| n.eq_ignore_ascii_case(&last)) { last } else { names.first().cloned().unwrap_or_default() };
    app.bedit = State { name, ..State::default() };
    app.ui.dialog = Some("bedit".into());
    Some(Ok(Value::Null))
}

/// The preview of block `name` in `d`, built once per drawing revision.
fn preview(app: &mut CadApp, name: &str) -> Option<Arc<DisplayList>> {
    let st = app.session.state().ok()?;
    let key = (st.uid, st.revision, name.to_ascii_uppercase());
    if let Some((k, list)) = &app.bedit.preview
        && *k == key
    {
        return Some(list.clone());
    }
    // The drawing itself shows its model space.
    let entities = if name.eq_ignore_ascii_case(CURRENT_DRAWING) { &st.doc.model } else { &st.doc.block(name)?.entities };
    let rough = cadcraft_render::build_entities(&st.doc, entities.iter().map(|e| e.as_ref()), &cadcraft_render::Options::default());
    let size = rough.bounds.width().max(rough.bounds.height());
    // About half a pixel of the preview's 200 points.
    let opts = cadcraft_render::Options { tolerance: (size / 400.0).max(1e-9), ..Default::default() };
    let list =
        Arc::new(if rough.bounds.is_empty() { rough } else { cadcraft_render::build_entities(&st.doc, entities.iter().map(|e| e.as_ref()), &opts) });
    app.bedit.preview = Some((key, list.clone()));
    Some(list)
}

/// Draw `list` fitted into `rect`, or `note` when there is nothing to draw.
fn draw_preview(ui: &mut egui::Ui, list: Option<&DisplayList>, note: &str) {
    let t = Tokens::get();
    let (rect, _) = ui.allocate_exact_size(vec2(220.0, 170.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, t.canvas);
    p.rect_stroke(rect, 4.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    match list.filter(|l| !l.bounds.is_empty() && !l.prims.is_empty()) {
        Some(l) => {
            let b = l.bounds;
            let inner = rect.shrink(12.0);
            let (w, h) = (b.width().max(1e-9), b.height().max(1e-9));
            let scale = (f64::from(inner.width()) / w).min(f64::from(inner.height()) / h);
            let xf = Xf::new(rect, Vec2::new((b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0), scale);
            crate::canvas::draw_list(&p, &xf, l, Rgb(t.canvas.r(), t.canvas.g(), t.canvas.b()), false);
        }
        None => {
            p.text(rect.center(), egui::Align2::CENTER_CENTER, note, crate::theme::small(), t.text_dim);
        }
    }
}

/// The Edit Block Definition window.
pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let t = Tokens::get();
    let Ok(d) = app.session.state().map(|st| st.doc.clone()) else {
        *open = false;
        return;
    };
    let names = editable_blocks(&d);
    let typed = app.bedit.name.trim().to_string();
    let existing = names.iter().find(|n| n.eq_ignore_ascii_case(&typed)).cloned();
    let list = existing.as_deref().and_then(|n| preview(app, n));
    let description = existing.as_deref().and_then(|n| d.block(n)).map(|b| b.description.clone()).unwrap_or_default();
    let (mut ok, mut cancel) = (false, false);
    egui::Window::new(crate::tl!("Edit Block Definition"))
        .id(egui::Id::new("Edit Block Definition"))
        .open(open)
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            ui.label(crate::tl!("Block to create or edit"));
            let edit = ui.add(egui::TextEdit::singleline(&mut app.bedit.name).desired_width(460.0));
            if edit.changed() {
                app.bedit.error = None;
            }
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                ok = true;
            }
            ui.add_space(4.0);
            ui.horizontal_top(|ui| {
                egui::Frame::new().fill(t.field).stroke(Stroke::new(1.0, t.border)).corner_radius(4.0).inner_margin(4.0).show(ui, |ui| {
                    ui.set_min_size(vec2(222.0, 250.0));
                    egui::ScrollArea::vertical().id_salt("bedit-blocks").max_height(250.0).min_scrolled_height(250.0).show(ui, |ui| {
                        ui.set_width(222.0);
                        ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                            for n in &names {
                                let row = ui.selectable_label(existing.as_deref() == Some(n.as_str()), n);
                                if row.clicked() {
                                    app.bedit.name = n.clone();
                                    app.bedit.error = None;
                                }
                                if row.double_clicked() {
                                    ok = true;
                                }
                            }
                        });
                    });
                });
                ui.vertical(|ui| {
                    ui.label(RichText::new(crate::tl!("Preview")).color(t.text_dim));
                    let note = if typed.is_empty() {
                        String::new()
                    } else if existing.is_none() {
                        crate::tl!("New block").to_string()
                    } else {
                        String::new()
                    };
                    draw_preview(ui, list.as_deref(), &note);
                    ui.add_space(4.0);
                    ui.label(RichText::new(crate::tl!("Description")).color(t.text_dim));
                    let text = if description.is_empty() { "—".to_string() } else { description.clone() };
                    ui.add(egui::Label::new(text).wrap());
                });
            });
            if let Some(e) = &app.bedit.error {
                ui.colored_label(t.warn, e);
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(crate::tl!("Cancel")).clicked() {
                        cancel = true;
                    }
                    if ui.add_enabled(!typed.is_empty(), egui::Button::new(crate::tl!("OK"))).clicked() {
                        ok = true;
                    }
                });
            });
        });
    if cancel || (ok && confirm(app)) {
        *open = false;
    }
}

/// OK: open the Block Editor on the name given (a new name starts a new block). Returns whether
/// it opened; if not, the dialog shows why and stays open.
pub fn confirm(app: &mut CadApp) -> bool {
    let name = app.bedit.name.trim().to_string();
    if name.is_empty() {
        return false;
    }
    match app.run("bedit", json!({ "name": name, "create": true })) {
        Ok(_) => true,
        Err(e) => {
            app.bedit.error = Some(e);
            false
        }
    }
}

/// The dynamic block the Block Editor authors (its parameters and actions) and the editor's
/// offset (the block's base point), worked out once per drawing revision. `None` outside the
/// editor or for a block without dynamic data.
pub(crate) fn authored(app: &mut CadApp) -> Option<(Arc<DynamicBlock>, Vec2)> {
    let st = app.session.state().ok()?;
    let key = (st.uid, st.revision);
    if let Some((k, v)) = &app.bedit.authored
        && *k == key
    {
        return v.clone();
    }
    // The test block window shows a reference, not the authoring.
    let v = st.block_edit.as_ref().filter(|be| be.testing.is_none()).and_then(|be| {
        let m = cadcraft_engine::doc::dynblock::dynamic_block(&st.doc, be.dynamic_name())?;
        Some((Arc::new(m), be.base()))
    });
    app.bedit.authored = Some((key, v.clone()));
    v
}

/// The block's visibility states and the one the Block Editor shows, when it has a visibility
/// parameter.
pub fn visibility_states(app: &mut CadApp) -> Option<(Vec<String>, Option<String>)> {
    let (m, _) = authored(app)?;
    let names: Vec<String> = m.parameters.iter().find_map(|p| match &p.kind {
        ParameterKind::Visibility { states, .. } => Some(states.iter().map(|s| s.name.clone()).collect()),
        _ => None,
    })?;
    let vstate = app.session.state().ok()?.block_edit.as_ref()?.vstate.clone();
    let current = vstate.and_then(|v| names.iter().find(|n| n.eq_ignore_ascii_case(&v)).cloned()).or_else(|| names.first().cloned());
    Some((names, current))
}

/// Open the Visibility States dialog on the state the editor shows.
pub(crate) fn open_visibility_states(app: &mut CadApp) {
    let selected = visibility_states(app).and_then(|(_, c)| c).unwrap_or_default();
    app.bedit.vstates = Some(VStates { selected, ..VStates::default() });
}

/// The Visibility States dialog: the block's states (the one shown in bold, the first is a new
/// reference's), Set Current, New, Rename, Delete, Move Up and Move Down; each runs `bvstate`.
pub(crate) fn visibility_states_dialog(app: &mut CadApp, ctx: &egui::Context) {
    if app.bedit.vstates.is_none() {
        return;
    }
    let Some((names, current)) = visibility_states(app) else {
        app.bedit.vstates = None;
        return;
    };
    let t = Tokens::get();
    let mut open = true;
    let mut run: Option<Value> = None;
    let mut close = false;
    let Some(v) = app.bedit.vstates.as_mut() else { return };
    if !names.contains(&v.selected) {
        v.selected = current.clone().unwrap_or_default();
    }
    egui::Window::new(crate::tl!("Visibility States"))
        .id(egui::Id::new("Visibility States"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            ui.set_max_width(400.0);
            ui.horizontal_top(|ui| {
                egui::Frame::new().fill(t.field).stroke(Stroke::new(1.0, t.border)).corner_radius(4.0).inner_margin(4.0).show(ui, |ui| {
                    ui.set_min_size(vec2(222.0, 200.0));
                    egui::ScrollArea::vertical().id_salt("bvstate-list").max_height(200.0).min_scrolled_height(200.0).show(ui, |ui| {
                        ui.set_width(222.0);
                        ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                            for n in &names {
                                let text = if current.as_deref() == Some(n.as_str()) { RichText::new(n).strong() } else { RichText::new(n) };
                                let row = ui.selectable_label(v.selected == *n, text);
                                if row.clicked() {
                                    v.selected = n.clone();
                                    v.error = None;
                                }
                                if row.double_clicked() {
                                    run = Some(json!({ "operation": "set", "name": n }));
                                }
                            }
                        });
                    });
                });
                ui.vertical(|ui| {
                    ui.set_min_width(120.0);
                    let sel = v.selected.clone();
                    if ui.button(crate::tl!("Set Current")).clicked() {
                        run = Some(json!({ "operation": "set", "name": sel }));
                    }
                    if ui.button(crate::tl!("New...")).clicked() {
                        v.form = Some((false, String::new(), "current"));
                    }
                    if ui.button(crate::tl!("Rename...")).clicked() {
                        v.form = Some((true, sel.clone(), "current"));
                    }
                    if ui.add_enabled(names.len() > 1, egui::Button::new(crate::tl!("Delete"))).clicked() {
                        run = Some(json!({ "operation": "delete", "name": sel }));
                    }
                    if ui.button(crate::tl!("Move Up")).clicked() {
                        run = Some(json!({ "operation": "up", "name": sel }));
                    }
                    if ui.button(crate::tl!("Move Down")).clicked() {
                        run = Some(json!({ "operation": "down", "name": sel }));
                    }
                });
            });
            let mut cancel_form = false;
            if let Some((renaming, name, how)) = v.form.as_mut() {
                ui.separator();
                ui.label(if *renaming { crate::tl!("New name") } else { crate::tl!("Visibility state name") });
                ui.text_edit_singleline(name);
                if !*renaming {
                    ui.radio_value(how, "hide", crate::tl!("Hide all existing objects in new state"));
                    ui.radio_value(how, "show", crate::tl!("Show all existing objects in new state"));
                    ui.radio_value(how, "current", crate::tl!("Leave visibility of existing objects unchanged in new state"));
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(!name.trim().is_empty(), egui::Button::new(crate::tl!("OK"))).clicked() {
                        run = Some(if *renaming {
                            json!({ "operation": "rename", "name": v.selected, "newName": name.trim() })
                        } else {
                            json!({ "operation": "new", "name": name.trim(), "visibility": how })
                        });
                    }
                    if ui.button(crate::tl!("Cancel")).clicked() {
                        cancel_form = true;
                    }
                });
            }
            if cancel_form {
                v.form = None;
            }
            if let Some(e) = &v.error {
                ui.colored_label(t.warn, e);
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(crate::tl!("OK")).clicked() {
                        close = true;
                    }
                });
            });
        });
    if let Some(p) = run {
        let made = p.get("operation").and_then(Value::as_str).is_some_and(|o| o == "new" || o == "rename");
        let name = p.get("newName").or_else(|| p.get("name")).and_then(Value::as_str).unwrap_or_default().to_string();
        match app.run("bvstate", p) {
            Ok(_) => {
                if let Some(v) = app.bedit.vstates.as_mut() {
                    v.error = None;
                    if made {
                        v.form = None;
                        v.selected = name;
                    }
                }
            }
            Err(e) => {
                if let Some(v) = app.bedit.vstates.as_mut() {
                    v.error = Some(e);
                }
            }
        }
    }
    if close || !open {
        app.bedit.vstates = None;
    }
}
