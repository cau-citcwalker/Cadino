//! Object Selection Filters (FILTER, FI): a list of property conditions (object type, layer,
//! colour, linetype, text height… compared with =, !=, >, < or a wildcard), named filter lists
//! kept for the session, and Apply, which selects the objects meeting every condition through
//! QSELECT (the first condition picks from the drawing or the current selection, each next one
//! narrows the selection).
// ponytail: conditions are joined with AND only (no OR/NOT grouping operators), and Apply
// filters the drawing or the current selection instead of asking for objects afterwards.

use egui::{RichText, vec2};
use serde_json::{Value, json};

use super::{Action, set_state, set_temp, state, temp};
use crate::CadApp;
use crate::theme::Tokens;

/// The condition picking objects by their type.
const OBJECT_TYPE: &str = "Object Type";
const OPERATORS: &[&str] = &["=", "!=", ">", "<", "*"];
/// egui memory: the named filter lists, kept until the app closes.
const NAMED: &str = "filter_named";

#[derive(Clone, Debug, Default, PartialEq)]
struct Cond {
    property: String,
    op: String,
    value: String,
}

impl Cond {
    fn text(&self) -> String {
        format!("{} {} {}", self.property, self.op, self.value)
    }
    /// QSELECT's parameters for this condition; `first` picks from `apply_to`, later ones narrow
    /// the selection.
    fn qselect(&self, first: bool, apply_to: &str) -> Value {
        let property = if self.property == OBJECT_TYPE { "objecttype" } else { self.property.as_str() };
        json!({ "property": property, "operator": self.op, "value": self.value, "mode": "new", "applyTo": if first { apply_to } else { "selection" } })
    }
}

#[derive(Clone)]
struct St {
    list: Vec<Cond>,
    sel: Option<usize>,
    edit: Cond,
    save_name: String,
    current: String,
    selection_only: bool,
    /// From QSELECT.INFO when the dialog opened: object types and properties in the drawing.
    types: Vec<String>,
    properties: Vec<String>,
}

fn fresh(app: &mut CadApp) -> St {
    let info = app.session.execute("qselect.info", &json!({ "applyTo": "drawing" })).unwrap_or_default();
    let types: Vec<String> = info["types"].as_array().into_iter().flatten().filter_map(|t| t["type"].as_str().map(str::to_string)).collect();
    let mut properties = vec![OBJECT_TYPE.to_string()];
    properties.extend(info["properties"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|p| *p != "type").map(str::to_string));
    St {
        list: Vec::new(),
        sel: None,
        edit: Cond { property: OBJECT_TYPE.into(), op: "=".into(), value: types.first().cloned().unwrap_or_default() },
        save_name: String::new(),
        current: String::new(),
        selection_only: !app.session.selection().is_empty(),
        types,
        properties,
    }
}

/// Conditions matching the first selected object: its type, layer, colour and linetype.
fn from_selected(app: &CadApp) -> Vec<Cond> {
    let Ok(d) = app.session.doc() else { return Vec::new() };
    let Some(e) = app.session.selection().first().and_then(|h| d.entity(*h)) else { return Vec::new() };
    let c = |p: &str, v: String| Cond { property: p.into(), op: "=".into(), value: v };
    vec![
        c(OBJECT_TYPE, e.kind.type_name().into()),
        c("layer", e.common.layer.clone()),
        c("color", e.common.color.name()),
        c("linetype", e.common.linetype.clone()),
    ]
}

/// Select what meets every condition of `list`, through QSELECT.
fn apply(app: &mut CadApp, list: &[Cond], selection_only: bool) {
    let apply_to = if selection_only { "selection" } else { "drawing" };
    for (i, c) in list.iter().enumerate() {
        if app.run("qselect", c.qselect(i == 0, apply_to)).is_err() {
            return;
        }
    }
}

pub(super) fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) -> Action {
    let t = Tokens::get();
    if app.session.state().is_err() {
        super::no_drawing(ctx, "Object Selection Filters", open);
        return None;
    }
    let mut st = state(ctx, "filter_dlg", || fresh(app));
    let mut named: Vec<(String, Vec<Cond>)> = temp(ctx, NAMED).unwrap_or_default();
    let layers: Vec<String> = app.session.doc().map(|d| d.layers.iter().map(|l| l.name.clone()).collect()).unwrap_or_default();
    let has_selection = !app.session.selection().is_empty();
    let (mut go, mut cancel) = (false, false);
    egui::Window::new(crate::tl!("Object Selection Filters"))
        .id(egui::Id::new("Object Selection Filters"))
        .open(open)
        .collapsible(false)
        .default_size(vec2(520.0, 420.0))
        .resizable(true)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().id_salt("filter_list").max_height(110.0).auto_shrink([false, false]).show(ui, |ui| {
                if st.list.is_empty() {
                    ui.label(RichText::new(crate::tl!("No conditions yet: add one below.")).color(t.text_dim));
                }
                for (i, c) in st.list.iter().enumerate() {
                    if ui.selectable_label(st.sel == Some(i), c.text()).clicked() {
                        st.sel = Some(i);
                    }
                }
            });
            ui.horizontal(|ui| {
                let sel = st.sel.filter(|i| *i < st.list.len());
                if ui.add_enabled(sel.is_some(), egui::Button::new(crate::tl!("Edit Item"))).clicked()
                    && let Some(c) = sel.and_then(|i| st.list.get(i))
                {
                    st.edit = c.clone();
                }
                if ui.add_enabled(sel.is_some(), egui::Button::new(crate::tl!("Delete"))).clicked()
                    && let Some(i) = sel
                {
                    st.list.remove(i);
                    st.sel = None;
                }
                if ui.add_enabled(!st.list.is_empty(), egui::Button::new(crate::tl!("Clear List"))).clicked() {
                    st.list.clear();
                    st.sel = None;
                }
            });
            ui.separator();
            ui.label(RichText::new(crate::tl!("Select Filter")).strong());
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("filter_property").selected_text(st.edit.property.clone()).width(150.0).show_ui(ui, |ui| {
                    for p in &st.properties {
                        if ui.selectable_label(st.edit.property == *p, p).clicked() {
                            st.edit.property = p.clone();
                            st.edit.value = match p.as_str() {
                                OBJECT_TYPE => st.types.first().cloned().unwrap_or_default(),
                                "layer" => layers.first().cloned().unwrap_or_default(),
                                _ => String::new(),
                            };
                        }
                    }
                });
                egui::ComboBox::from_id_salt("filter_op").selected_text(st.edit.op.clone()).width(40.0).show_ui(ui, |ui| {
                    for o in OPERATORS {
                        ui.selectable_value(&mut st.edit.op, (*o).to_string(), *o);
                    }
                });
                let choices: Option<&[String]> = match st.edit.property.as_str() {
                    OBJECT_TYPE => Some(&st.types),
                    "layer" => Some(&layers),
                    _ => None,
                };
                match choices.filter(|_| st.edit.op != "*") {
                    Some(list) => {
                        egui::ComboBox::from_id_salt("filter_value").selected_text(st.edit.value.clone()).width(160.0).show_ui(ui, |ui| {
                            for v in list {
                                ui.selectable_value(&mut st.edit.value, v.clone(), v);
                            }
                        });
                    }
                    None => {
                        ui.add(egui::TextEdit::singleline(&mut st.edit.value).hint_text(crate::tl!("Value")).desired_width(160.0));
                    }
                }
            });
            ui.horizontal(|ui| {
                let ok = !st.edit.value.trim().is_empty();
                if ui.add_enabled(ok, egui::Button::new(crate::tl!("Add to List"))).clicked() {
                    let at = st.sel.map_or(st.list.len(), |i| (i + 1).min(st.list.len()));
                    st.list.insert(at, st.edit.clone());
                    st.sel = Some(at);
                }
                let sel = st.sel.filter(|i| *i < st.list.len());
                if ui.add_enabled(ok && sel.is_some(), egui::Button::new(crate::tl!("Substitute"))).clicked()
                    && let Some(slot) = sel.and_then(|i| st.list.get_mut(i))
                {
                    *slot = st.edit.clone();
                }
                if ui
                    .add_enabled(has_selection, egui::Button::new(crate::tl!("Add Selected Object")))
                    .on_hover_text(crate::tl!("Add the type, layer, colour and linetype of the first selected object"))
                    .clicked()
                {
                    st.list.extend(from_selected(app));
                }
            });
            ui.separator();
            ui.label(RichText::new(crate::tl!("Named Filters")).strong());
            egui::Grid::new("filter_named_grid").num_columns(2).spacing(vec2(8.0, 4.0)).show(ui, |ui| {
                ui.label(crate::tl!("Current:"));
                ui.horizontal(|ui| {
                    let shown = if st.current.is_empty() { crate::tl!("*unnamed").to_string() } else { st.current.clone() };
                    egui::ComboBox::from_id_salt("filter_current").selected_text(shown).width(160.0).show_ui(ui, |ui| {
                        for (n, conds) in &named {
                            if ui.selectable_label(st.current == *n, n).clicked() {
                                st.current = n.clone();
                                st.list = conds.clone();
                                st.sel = None;
                            }
                        }
                    });
                    if ui.add_enabled(!st.current.is_empty(), egui::Button::new(crate::tl!("Delete Current Filter List"))).clicked() {
                        named.retain(|(n, _)| *n != st.current);
                        st.current.clear();
                    }
                });
                ui.end_row();
                ui.label(crate::tl!("Save As:"));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut st.save_name).desired_width(160.0));
                    let name = st.save_name.trim().to_string();
                    if ui.add_enabled(!name.is_empty() && !st.list.is_empty(), egui::Button::new(crate::tl!("Save"))).clicked() {
                        named.retain(|(n, _)| !n.eq_ignore_ascii_case(&name));
                        named.push((name.clone(), st.list.clone()));
                        st.current = name;
                        st.save_name.clear();
                    }
                });
                ui.end_row();
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.add_enabled_ui(has_selection, |ui| ui.checkbox(&mut st.selection_only, crate::tl!("Within the current selection only")));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    cancel = ui.button(crate::tl!("Cancel")).clicked();
                    go = ui.add_enabled(!st.list.is_empty(), egui::Button::new(crate::tl!("Apply"))).clicked();
                });
            });
        });
    set_temp(ctx, NAMED, named);
    set_state(ctx, "filter_dlg", st.clone());
    if cancel {
        *open = false;
    }
    if go {
        *open = false;
        apply(app, &st.list, st.selection_only && has_selection);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_selects_what_meets_every_condition() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.run("layer.new", json!({ "name": "Walls" })).unwrap();
        app.run("line", json!({ "points": [[0, 0], [10, 0]] })).unwrap();
        app.run("circle", json!({ "center": [0, 0], "radius": 2 })).unwrap();
        app.run("circle", json!({ "center": [0, 0], "radius": 5 })).unwrap();
        app.run("layer.current", json!({ "name": "Walls" })).unwrap();
        app.run("circle", json!({ "center": [9, 9], "radius": 5 })).unwrap();
        let c = |p: &str, op: &str, v: &str| Cond { property: p.into(), op: op.into(), value: v.into() };
        apply(&mut app, &[c(OBJECT_TYPE, "=", "Circle"), c("radius", ">", "3")], false);
        assert_eq!(app.session.selection().len(), 2);
        apply(&mut app, &[c(OBJECT_TYPE, "=", "Circle"), c("radius", ">", "3"), c("layer", "=", "Walls")], false);
        assert_eq!(app.session.selection().len(), 1);
        // Within the current selection: the Walls circle is no line.
        apply(&mut app, &[c(OBJECT_TYPE, "=", "Line")], true);
        assert!(app.session.selection().is_empty());
        apply(&mut app, &[c(OBJECT_TYPE, "!=", "Circle")], false);
        assert_eq!(app.session.selection().len(), 1);
        // The first selected object gives its type, layer, colour and linetype.
        let conds = from_selected(&app);
        assert_eq!(conds.first().map(|c| c.value.as_str()), Some("Line"));
        apply(&mut app, &conds, false);
        assert_eq!(app.session.selection().len(), 1);
    }
}
