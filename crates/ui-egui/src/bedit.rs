//! The Edit Block Definition dialog: BEDIT typed or started from a menu or the Tool Sets with no
//! block reference selected. It lists the drawing's blocks with a preview and description; a name
//! no block has yet starts a new block. OK runs BEDIT with that name; the dialog itself never
//! changes the drawing. JSON calls (`bedit {name}`) and a selected block reference skip it.
//!
//! Also the Visibility States dialog (BVSTATE typed bare in the Block Editor), which runs
//! `bvstate` for each change, the Property Lookup Table dialog (BLOOKUPTABLE typed bare), which
//! reads and sets a lookup action's table through `blookuptable`, the dynamic block the editor's
//! canvas shows ([`authored`]), and the names of dynamic blocks' default actions and states in
//! the interface language ([`shown_action`], [`shown_value`]).

use std::sync::Arc;

use cadcraft_color::Rgb;
use cadcraft_engine::blockedit::CURRENT_DRAWING;
use cadcraft_engine::doc::dynblock::{ActionKind, DynamicBlock, ParameterKind};
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
    /// The Property Lookup Table dialog, while it is open.
    pub lookup: Option<LookupTable>,
    /// Lookup actions with an empty table the dialog opened on by itself: (drawing uid, node).
    offered: Vec<(u64, i32)>,
}

/// One column of the Property Lookup Table dialog: a parameter property, as `blookuptable`
/// reports and takes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LookupCol {
    pub parameter: i64,
    pub label: String,
    pub property: String,
    /// A lookup property (else an input property).
    pub lookup: bool,
    /// What a reference shows when no row matches (input properties).
    pub unmatched: String,
    /// Allow reverse lookup (lookup properties), else Read only.
    pub reverse: bool,
}

/// The Property Lookup Table dialog's state: one lookup action's table being edited.
#[derive(Default)]
pub struct LookupTable {
    /// The lookup action (its graph id) and name.
    pub action: i64,
    pub name: String,
    /// Input properties first, then lookup properties.
    pub columns: Vec<LookupCol>,
    pub rows: Vec<Vec<String>>,
    /// The parameter properties a column can be.
    pub available: Vec<LookupCol>,
    /// The block's lookup actions: (node, name).
    actions: Vec<(i64, String)>,
    /// Add Properties is open, with the property picked (an index into `available`).
    adding: Option<Option<usize>>,
    pub error: Option<String>,
    /// What Audit found.
    pub note: Option<String>,
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
    // BLOOKUPTABLE typed bare in the Block Editor opens the Property Lookup Table dialog (with no
    // lookup action, the command says why not).
    if id == "blookuptable" && params.is_null() {
        return Some(open_lookup_table(app, None).map(|()| Value::Null));
    }
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

/// A dynamic block action's name as the Block Editor's tag shows it: a default name ("Stretch",
/// "Lookup1") in the interface language, any other as the block has it.
pub fn shown_action(name: &str) -> String {
    let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = name.get(base.len()..).unwrap_or_default();
    let t = match base {
        "Move" => crate::tl!("Move"),
        "Scale" => crate::tl!("Scale"),
        "Stretch" => crate::tl!("Stretch"),
        "Polar Stretch" => crate::tl!("Polar Stretch"),
        "Array" => crate::tl!("Array"),
        "Rotate" => crate::tl!("Rotate"),
        "Flip" => crate::tl!("Flip"),
        "Lookup" => crate::tl!("Lookup"),
        _ => return name.to_string(),
    };
    format!("{t}{digits}")
}

/// A dynamic block property's value as Properties and the grips show it: flip states, the usual
/// visibility state names and a lookup's "Custom" in the interface language; other values as the
/// block has them. What is typed or chosen stays the block's own (English) value.
pub fn shown_value(v: &str) -> String {
    match v {
        "Flipped" => crate::tl!("Flipped"),
        "Not Flipped" => crate::tl!("Not Flipped"),
        "Open" => crate::i18n::value("Open"),
        "Closed" => crate::i18n::value("Closed"),
        "Custom" => crate::tl!("Custom"),
        other => other,
    }
    .to_string()
}

/// A menu of a dynamic property's choices, shown with [`shown_value`]: the value chosen (the
/// block's own), if any.
pub(crate) fn value_menu(ui: &mut egui::Ui, current: &str, choices: &[String]) -> Option<String> {
    let mut out = None;
    ui.menu_button(shown_value(current), |ui| {
        for c in choices {
            if ui.button(shown_value(c)).clicked() {
                out = Some(c.clone());
                ui.close();
            }
        }
    });
    out
}

fn col(v: &Value) -> LookupCol {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    LookupCol {
        parameter: v.get("parameter").and_then(Value::as_i64).unwrap_or(0),
        label: s("label"),
        property: s("property"),
        lookup: b("lookup"),
        unmatched: s("unmatched"),
        reverse: b("reverse"),
    }
}

/// Open the Property Lookup Table dialog on lookup action `action` (the block's first when
/// `None`), as `blookuptable` reports it.
pub fn open_lookup_table(app: &mut CadApp, action: Option<i64>) -> Result<(), String> {
    let actions: Vec<(i64, String)> = authored(app)
        .map(|(m, _)| m.actions.iter().filter(|a| matches!(a.kind, ActionKind::Lookup { .. })).map(|a| (i64::from(a.node), a.name.clone())).collect())
        .unwrap_or_default();
    let action = action.or_else(|| actions.first().map(|(n, _)| *n));
    let r = app.run("blookuptable", action.map_or(json!({}), |a| json!({ "action": a })))?;
    let list = |k: &str| r.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    let read: Vec<LookupCol> = list("columns").iter().map(col).collect();
    // Input properties first, as the dialog shows them.
    let order: Vec<usize> = (0..read.len())
        .filter(|i| read.get(*i).is_some_and(|c| !c.lookup))
        .chain((0..read.len()).filter(|i| read.get(*i).is_some_and(|c| c.lookup)))
        .collect();
    let rows: Vec<Vec<String>> = list("rows")
        .iter()
        .map(|r| {
            let cells: Vec<String> = r.as_array().map(|a| a.iter().map(|c| c.as_str().unwrap_or_default().to_string()).collect()).unwrap_or_default();
            order.iter().map(|i| cells.get(*i).cloned().unwrap_or_default()).collect()
        })
        .collect();
    app.bedit.lookup = Some(LookupTable {
        action: r.get("node").and_then(Value::as_i64).unwrap_or(0),
        name: r.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
        columns: order.iter().filter_map(|i| read.get(*i).cloned()).collect(),
        rows,
        available: list("available").iter().map(col).collect(),
        actions,
        ..LookupTable::default()
    });
    Ok(())
}

/// The table as `blookuptable` takes it.
fn lookup_params(t: &LookupTable, check: bool) -> Value {
    let columns: Vec<Value> = t
        .columns
        .iter()
        .map(|c| json!({ "parameter": c.parameter, "property": c.property, "lookup": c.lookup, "unmatched": c.unmatched, "reverse": c.reverse }))
        .collect();
    json!({ "action": t.action, "columns": columns, "rows": t.rows, "check": check })
}

/// OK: set the table. Returns whether it was set; if not, the dialog shows why and stays open.
pub fn confirm_lookup_table(app: &mut CadApp) -> bool {
    let Some(p) = app.bedit.lookup.as_ref().map(|t| lookup_params(t, false)) else { return false };
    match app.run("blookuptable", p) {
        Ok(_) => {
            app.bedit.lookup = None;
            true
        }
        Err(e) => {
            if let Some(t) = app.bedit.lookup.as_mut() {
                t.error = Some(e);
            }
            false
        }
    }
}

/// Audit: check the table without changing it.
pub fn audit_lookup_table(app: &mut CadApp) {
    let Some(p) = app.bedit.lookup.as_ref().map(|t| lookup_params(t, true)) else { return };
    let r = app.run("blookuptable", p);
    if let Some(t) = app.bedit.lookup.as_mut() {
        match r {
            Ok(_) => {
                t.error = None;
                t.note = Some(crate::tl!("No errors found.").to_string());
            }
            Err(e) => {
                t.note = None;
                t.error = Some(e);
            }
        }
    }
}

/// Open the dialog by itself on a lookup action whose table is empty (BACTION Lookup has just
/// added it), once for each.
pub(crate) fn offer_lookup_table(app: &mut CadApp) {
    if app.bedit.lookup.is_some() {
        return;
    }
    let Some((m, _)) = authored(app) else { return };
    let Ok(uid) = app.session.state().map(|st| st.uid) else { return };
    let empty = m.actions.iter().find(|a| {
        matches!(&a.kind, ActionKind::Lookup { columns, rows } if rows.is_empty() && columns.iter().all(|c| c.lookup))
            && !app.bedit.offered.contains(&(uid, a.node))
    });
    let Some(node) = empty.map(|a| a.node) else { return };
    app.bedit.offered.push((uid, node));
    if let Err(e) = open_lookup_table(app, Some(i64::from(node))) {
        app.session.echo(e);
    }
}

/// The Property Lookup Table dialog: the lookup action's input properties and lookup properties
/// as columns, a row per set of values, the unmatched row (what a reference shows when no row
/// matches; a lookup property's Read only or Allow reverse lookup), Add Properties, Audit, Add
/// Row, and OK, which sets the table with `blookuptable`.
pub(crate) fn lookup_table_dialog(app: &mut CadApp, ctx: &egui::Context) {
    if app.bedit.lookup.is_none() {
        return;
    }
    if app.session.state().ok().and_then(|st| st.block_edit.as_ref()).is_none() {
        app.bedit.lookup = None;
        return;
    }
    let t = Tokens::get();
    let mut open = true;
    let (mut ok, mut cancel, mut audit) = (false, false, false);
    let mut switch: Option<i64> = None;
    let Some(lt) = app.bedit.lookup.as_mut() else { return };
    egui::Window::new(crate::tl!("Property Lookup Table"))
        .id(egui::Id::new("Property Lookup Table"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            ui.set_min_width(420.0);
            ui.horizontal(|ui| {
                ui.label(crate::tl!("Action name:"));
                if lt.actions.len() > 1 {
                    egui::ComboBox::from_id_salt("lookup-action").selected_text(shown_action(&lt.name)).show_ui(ui, |ui| {
                        for (n, name) in &lt.actions {
                            if ui.selectable_label(*n == lt.action, shown_action(name)).clicked() && *n != lt.action {
                                switch = Some(*n);
                            }
                        }
                    });
                } else {
                    ui.label(RichText::new(shown_action(&lt.name)).strong());
                }
                ui.separator();
                if ui.button(crate::tl!("Add Properties...")).clicked() {
                    lt.adding = Some(None);
                }
                if ui.button(crate::tl!("Audit")).clicked() {
                    audit = true;
                }
            });
            ui.add_space(4.0);
            lookup_grid(ui, lt);
            if ui.button(crate::tl!("Add Row")).clicked() && lt.rows.len() < 10_000 {
                lt.rows.push(vec![String::new(); lt.columns.len()]);
            }
            add_properties(ui, lt);
            if let Some(e) = &lt.error {
                ui.colored_label(t.warn, e);
            } else if let Some(n) = &lt.note {
                ui.colored_label(t.text_dim, n);
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(crate::tl!("Cancel")).clicked() {
                        cancel = true;
                    }
                    if ui.button(crate::tl!("OK")).clicked() {
                        ok = true;
                    }
                });
            });
        });
    if let Some(n) = switch
        && let Err(e) = open_lookup_table(app, Some(n))
    {
        app.session.echo(e);
    }
    if audit {
        audit_lookup_table(app);
    }
    if cancel || !open {
        app.bedit.lookup = None;
    } else if ok {
        confirm_lookup_table(app);
    }
}

/// The table: a heading row (Input Properties, Lookup Properties), the columns' properties (each
/// with a remove button, but the last lookup property), the rows (each with a delete button) and
/// the unmatched row.
fn lookup_grid(ui: &mut egui::Ui, lt: &mut LookupTable) {
    let t = Tokens::get();
    let first_lookup = lt.columns.iter().position(|c| c.lookup);
    let mut remove_col: Option<usize> = None;
    let mut remove_row: Option<usize> = None;
    egui::ScrollArea::both().id_salt("lookup-table").max_height(320.0).show(ui, |ui| {
        egui::Grid::new("lookup-table-grid").striped(true).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
            ui.label("");
            for i in 0..lt.columns.len() {
                let heading = if i == 0 && first_lookup != Some(0) {
                    crate::tl!("Input Properties")
                } else if Some(i) == first_lookup {
                    crate::tl!("Lookup Properties")
                } else {
                    ""
                };
                ui.label(RichText::new(heading).color(t.text_dim));
            }
            ui.end_row();
            ui.label("");
            let lookups = lt.columns.iter().filter(|c| c.lookup).count();
            for (i, c) in lt.columns.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&c.label).strong());
                    // The last lookup property stays: a table gives a lookup value.
                    if !(c.lookup && lookups <= 1) && ui.small_button("x").on_hover_text(crate::tl!("Remove Properties")).clicked() {
                        remove_col = Some(i);
                    }
                });
            }
            ui.end_row();
            for (r, row) in lt.rows.iter_mut().enumerate() {
                if ui.small_button("x").on_hover_text(crate::tl!("Delete Row")).clicked() {
                    remove_row = Some(r);
                }
                row.resize(lt.columns.len(), String::new());
                for cell in row.iter_mut() {
                    ui.add(egui::TextEdit::singleline(cell).desired_width(96.0));
                }
                ui.end_row();
            }
            ui.label(RichText::new(crate::tl!("<Unmatched>")).color(t.text_dim));
            for (i, c) in lt.columns.iter_mut().enumerate() {
                if c.lookup {
                    let shown = if c.reverse { crate::tl!("Allow reverse lookup") } else { crate::tl!("Read only") };
                    egui::ComboBox::from_id_salt(("lookup-reverse", i)).selected_text(shown).show_ui(ui, |ui| {
                        ui.selectable_value(&mut c.reverse, false, crate::tl!("Read only"));
                        ui.selectable_value(&mut c.reverse, true, crate::tl!("Allow reverse lookup"));
                    });
                } else {
                    ui.add(egui::TextEdit::singleline(&mut c.unmatched).desired_width(96.0));
                }
            }
            ui.end_row();
        });
    });
    if let Some(i) = remove_col.filter(|i| *i < lt.columns.len()) {
        lt.columns.remove(i);
        for r in &mut lt.rows {
            if i < r.len() {
                r.remove(i);
            }
        }
    }
    if let Some(r) = remove_row.filter(|r| *r < lt.rows.len()) {
        lt.rows.remove(r);
    }
}

/// Add Properties: the parameter properties not in the table yet; Add puts the one picked in as
/// an input property (before the lookup properties) or a lookup property.
fn add_properties(ui: &mut egui::Ui, lt: &mut LookupTable) {
    let Some(picked) = lt.adding else { return };
    ui.separator();
    ui.label(crate::tl!("Parameter properties"));
    let offered: Vec<usize> = (0..lt.available.len())
        .filter(|i| lt.available.get(*i).is_some_and(|a| !lt.columns.iter().any(|c| c.parameter == a.parameter && c.lookup == a.lookup)))
        .collect();
    let mut chosen = picked;
    if offered.is_empty() {
        ui.label(RichText::new(crate::tl!("Every parameter property is in the table.")).color(Tokens::get().text_dim));
    }
    for i in &offered {
        let Some(a) = lt.available.get(*i) else { continue };
        let kind = if a.lookup { crate::tl!("Lookup Properties") } else { crate::tl!("Input Properties") };
        if ui.selectable_label(chosen == Some(*i), format!("{} ({kind})", a.label)).clicked() {
            chosen = Some(*i);
        }
    }
    let mut close = false;
    ui.horizontal(|ui| {
        let ready = chosen.is_some_and(|i| offered.contains(&i));
        if ui.add_enabled(ready, egui::Button::new(crate::tl!("Add"))).clicked()
            && let Some(a) = chosen.and_then(|i| lt.available.get(i)).cloned()
        {
            let at = if a.lookup { lt.columns.len() } else { lt.columns.iter().position(|c| c.lookup).unwrap_or(lt.columns.len()) };
            let unmatched = if a.lookup { String::new() } else { "Custom".to_string() };
            lt.columns.insert(at, LookupCol { unmatched, ..a });
            for r in &mut lt.rows {
                r.insert(at.min(r.len()), String::new());
            }
            close = true;
        }
        if ui.button(crate::tl!("Cancel")).clicked() {
            close = true;
        }
    });
    lt.adding = if close { None } else { Some(chosen) };
}
