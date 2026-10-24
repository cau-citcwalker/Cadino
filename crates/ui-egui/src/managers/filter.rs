//! Object Selection Filters (FILTER, FI): a filter list of property conditions (object type,
//! layer, colour, linetype, text height… compared with =, !=, >, < or a wildcard) and grouping
//! operators (**Begin AND / **End AND, **Begin OR / **End OR, **Begin XOR / **End XOR,
//! **Begin NOT / **End NOT) as AutoCAD's filter list has them, named filter lists kept for the
//! session, and Apply, which selects the objects that pass: each condition's objects come from
//! QSELECT, the groups combine them (top level: AND), and SELECT makes the result the selection.
// ponytail: Apply filters the drawing or the current selection instead of asking for objects
// afterwards; named filter lists last until the app closes (AutoCAD keeps them in filter.nfl).

use std::collections::HashSet;

use cadcraft_doc::Handle;
use egui::{RichText, vec2};
use serde_json::{Value, json};

use super::{Action, set_state, set_temp, state, temp};
use crate::CadApp;
use crate::theme::Tokens;

/// The condition picking objects by their type.
const OBJECT_TYPE: &str = "Object Type";
const OPERATORS: &[&str] = &["=", "!=", ">", "<", "*"];
/// The grouping operators, as the filter list shows them.
const GROUPS: &[&str] = &["**Begin AND", "**End AND", "**Begin OR", "**End OR", "**Begin XOR", "**End XOR", "**Begin NOT", "**End NOT"];
/// egui memory: the named filter lists, kept until the app closes.
const NAMED: &str = "filter_named";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Group {
    And,
    Or,
    Xor,
    Not,
}

/// `**Begin OR` → (true, Or); `**End NOT` → (false, Not).
fn group(property: &str) -> Option<(bool, Group)> {
    let rest = property.strip_prefix("**")?;
    let (begin, op) = rest.split_once(' ')?;
    let g = match op {
        "AND" => Group::And,
        "OR" => Group::Or,
        "XOR" => Group::Xor,
        "NOT" => Group::Not,
        _ => return None,
    };
    match begin {
        "Begin" => Some((true, g)),
        "End" => Some((false, g)),
        _ => None,
    }
}

/// A property as the list shows it: `centerX` → "Center X", translated.
fn label(property: &str) -> String {
    if group(property).is_some() || property == OBJECT_TYPE {
        return crate::i18n::t(property).to_string();
    }
    crate::i18n::t(&crate::quick::pretty(property)).to_string()
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Cond {
    /// A QSELECT property key, [`OBJECT_TYPE`] or a grouping operator ([`GROUPS`]).
    property: String,
    op: String,
    value: String,
}

impl Cond {
    fn text(&self) -> String {
        if group(&self.property).is_some() {
            return label(&self.property);
        }
        format!("{} {} {}", label(&self.property), self.op, self.value)
    }
    /// QSELECT's parameters for this condition, over the whole drawing.
    fn qselect(&self) -> Value {
        let property = if self.property == OBJECT_TYPE { "objecttype" } else { self.property.as_str() };
        json!({ "property": property, "operator": self.op, "value": self.value, "mode": "new", "applyTo": "drawing" })
    }
}

/// The objects of `all` that pass `list`: each condition's objects from `cond`, combined by the
/// grouping operators (the list itself is an AND group). Unbalanced groups are an error.
fn evaluate(list: &[Cond], all: &HashSet<Handle>, mut cond: impl FnMut(&Cond) -> Result<HashSet<Handle>, String>) -> Result<HashSet<Handle>, String> {
    let combine = |g: Group, sets: Vec<HashSet<Handle>>| -> HashSet<Handle> {
        let and = |sets: Vec<HashSet<Handle>>| sets.into_iter().reduce(|a, b| &a & &b).unwrap_or_else(|| all.clone());
        match g {
            Group::And => and(sets),
            Group::Or => sets.into_iter().flatten().collect(),
            Group::Xor => sets.into_iter().fold(HashSet::new(), |a, b| &a ^ &b),
            Group::Not => all - &and(sets),
        }
    };
    let mut stack: Vec<(Group, Vec<HashSet<Handle>>)> = vec![(Group::And, Vec::new())];
    for c in list {
        match group(&c.property) {
            Some((true, g)) => stack.push((g, Vec::new())),
            Some((false, g)) => {
                let (open, sets) =
                    stack.pop().filter(|_| !stack.is_empty()).ok_or_else(|| crate::tf!("{op} has no matching Begin.", op = c.text()))?;
                if open != g {
                    return Err(crate::tf!("{op} has no matching Begin.", op = c.text()));
                }
                let set = combine(g, sets);
                if let Some((_, parent)) = stack.last_mut() {
                    parent.push(set);
                }
            }
            None => {
                let set: HashSet<Handle> = cond(c)?.intersection(all).copied().collect();
                if let Some((_, top)) = stack.last_mut() {
                    top.push(set);
                }
            }
        }
    }
    match stack.pop() {
        Some((Group::And, sets)) if stack.is_empty() => Ok(combine(Group::And, sets)),
        _ => Err(crate::tl!("The filter list is unbalanced: every Begin needs its End.").to_string()),
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

/// Select what passes `list` (from the current selection only, or the drawing), through QSELECT
/// and SELECT. Returns how many objects were selected.
fn apply(app: &mut CadApp, list: &[Cond], selection_only: bool) -> Result<usize, String> {
    let before = app.session.selection();
    let order: Vec<Handle> = if selection_only {
        before.clone()
    } else {
        let space = app.session.space();
        app.session.doc().map_err(|e| e.to_string())?.space(&space).map(|st| st.iter().map(|e| e.handle).collect()).unwrap_or_default()
    };
    let all: HashSet<Handle> = order.iter().copied().collect();
    let session = &mut app.session;
    let picked = evaluate(list, &all, |c| {
        let r = session.execute("qselect", &c.qselect()).map_err(|e| e.to_string())?;
        Ok(r["handles"].as_array().into_iter().flatten().filter_map(|h| h.as_str().and_then(Handle::parse_hex)).collect())
    });
    let picked = match picked {
        Ok(p) => p,
        Err(e) => {
            app.session.set_selection(before);
            app.session.echo(e.clone());
            return Err(e);
        }
    };
    let handles: Vec<String> = order.iter().filter(|h| picked.contains(h)).map(|h| h.hex()).collect();
    let n = handles.len();
    app.run("select", json!({ "handles": handles }))?;
    Ok(n)
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
            egui::ScrollArea::vertical().id_salt("filter_list").max_height(130.0).auto_shrink([false, false]).show(ui, |ui| {
                if st.list.is_empty() {
                    ui.label(RichText::new(crate::tl!("No conditions yet: add one below.")).color(t.text_dim));
                }
                // Conditions inside a group are indented, as AutoCAD lists them.
                let mut depth = 0usize;
                for (i, c) in st.list.iter().enumerate() {
                    let g = group(&c.property);
                    if matches!(g, Some((false, _))) {
                        depth = depth.saturating_sub(1);
                    }
                    ui.horizontal(|ui| {
                        ui.add_space(16.0 * depth.min(8) as f32);
                        if ui.selectable_label(st.sel == Some(i), c.text()).clicked() {
                            st.sel = Some(i);
                        }
                    });
                    if matches!(g, Some((true, _))) {
                        depth += 1;
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
            let is_group = group(&st.edit.property).is_some();
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("filter_property").selected_text(label(&st.edit.property)).width(150.0).height(360.0).show_ui(
                    ui,
                    |ui| {
                        for p in &st.properties {
                            if ui.selectable_label(st.edit.property == *p, label(p)).clicked() {
                                st.edit.property = p.clone();
                                st.edit.value = match p.as_str() {
                                    OBJECT_TYPE => st.types.first().cloned().unwrap_or_default(),
                                    "layer" => layers.first().cloned().unwrap_or_default(),
                                    _ => String::new(),
                                };
                            }
                        }
                        ui.separator();
                        for g in GROUPS {
                            if ui.selectable_label(st.edit.property == *g, label(g)).clicked() {
                                st.edit = Cond { property: (*g).to_string(), ..Cond::default() };
                            }
                        }
                    },
                );
                if is_group {
                    return;
                }
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
                let ok = is_group || !st.edit.value.trim().is_empty();
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
    // An unbalanced list is echoed and keeps the dialog open to fix it.
    if go && apply(app, &st.list, st.selection_only && has_selection).is_ok() {
        *open = false;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(p: &str, op: &str, v: &str) -> Cond {
        Cond { property: p.into(), op: op.into(), value: v.into() }
    }

    fn g(p: &str) -> Cond {
        Cond { property: p.into(), ..Cond::default() }
    }

    #[test]
    fn apply_selects_what_meets_every_condition() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.run("layer.new", json!({ "name": "Walls" })).unwrap();
        app.run("line", json!({ "points": [[0, 0], [10, 0]] })).unwrap();
        app.run("circle", json!({ "center": [0, 0], "radius": 2 })).unwrap();
        app.run("circle", json!({ "center": [0, 0], "radius": 5 })).unwrap();
        app.run("layer.current", json!({ "name": "Walls" })).unwrap();
        app.run("circle", json!({ "center": [9, 9], "radius": 5 })).unwrap();
        assert_eq!(apply(&mut app, &[c(OBJECT_TYPE, "=", "Circle"), c("radius", ">", "3")], false), Ok(2));
        apply(&mut app, &[c(OBJECT_TYPE, "=", "Circle"), c("radius", ">", "3"), c("layer", "=", "Walls")], false).unwrap();
        assert_eq!(app.session.selection().len(), 1);
        // Within the current selection: the Walls circle is no line.
        assert_eq!(apply(&mut app, &[c(OBJECT_TYPE, "=", "Line")], true), Ok(0));
        assert_eq!(apply(&mut app, &[c(OBJECT_TYPE, "!=", "Circle")], false), Ok(1));
        // The first selected object gives its type, layer, colour and linetype.
        let conds = from_selected(&app);
        assert_eq!(conds.first().map(|c| c.value.as_str()), Some("Line"));
        assert_eq!(apply(&mut app, &conds, false), Ok(1));

        // OR: the line or the small circle; NOT: everything but circles on Walls.
        let or = [g("**Begin OR"), c(OBJECT_TYPE, "=", "Line"), c("radius", "<", "3"), g("**End OR")];
        assert_eq!(apply(&mut app, &or, false), Ok(2));
        let not = [g("**Begin NOT"), c(OBJECT_TYPE, "=", "Circle"), c("layer", "=", "Walls"), g("**End NOT")];
        assert_eq!(apply(&mut app, &not, false), Ok(3));
        // Nested: circles that are (small XOR on Walls).
        let xor = [c(OBJECT_TYPE, "=", "Circle"), g("**Begin XOR"), c("radius", "<", "3"), c("layer", "=", "Walls"), g("**End XOR")];
        assert_eq!(apply(&mut app, &xor, false), Ok(2));
        // Unbalanced lists are refused and leave the selection as it was.
        let kept = app.session.selection();
        for bad in [vec![g("**Begin OR"), c(OBJECT_TYPE, "=", "Line")], vec![g("**End AND")], vec![g("**Begin OR"), g("**End AND")]] {
            assert!(apply(&mut app, &bad, false).is_err());
            assert_eq!(app.session.selection(), kept);
        }
    }

    #[test]
    fn labels_are_readable() {
        assert_eq!(label("centerX"), "Center X");
        assert_eq!(c("layer", "=", "Walls").text(), "Layer = Walls");
        assert_eq!(group("**Begin OR"), Some((true, Group::Or)));
        assert_eq!(group("**End NOT"), Some((false, Group::Not)));
        assert_eq!(group("layer"), None);
    }
}
