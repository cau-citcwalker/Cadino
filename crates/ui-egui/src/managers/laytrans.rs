//! Layer Translator (LAYTRANS; Tools ▸ CAD Standards ▸ Layer Translator...): map this
//! drawing's layers to target layers (loaded from another drawing or defined here) with their
//! colour, linetype and lineweight, then Translate: a layer mapped to a new name is renamed, one
//! mapped to an existing layer is merged into it, and the target's properties are set, through
//! `layer.set`, `laymrg` and `linetype` in one undo step.
// ponytail: no Settings (force object colours ByLayer, purge, write a log) and no saving or
// loading of mapping files; targets come from a drawing's layers or are made here.

use std::collections::BTreeSet;

use cadcraft_color::Color;
use cadcraft_doc::Lineweight;
use egui::{RichText, Sense, vec2};
use serde_json::{Value, json};

use super::{Action, set_state, state};
use crate::CadApp;
use crate::theme::Tokens;

#[derive(Clone, Debug, PartialEq)]
struct Target {
    name: String,
    color: Color,
    linetype: String,
    lineweight: Lineweight,
}

#[derive(Clone, Default)]
struct St {
    targets: Vec<Target>,
    from_sel: BTreeSet<String>,
    /// The selected target, by name (Load... reorders the list).
    to_sel: Option<String>,
    /// Old layer name → target.
    maps: Vec<(String, Target)>,
    map_sel: Option<usize>,
    /// The New... form while it is open.
    new: Option<Target>,
}

fn lw_text(lw: Lineweight) -> String {
    match lw {
        Lineweight::Mm100(v) => format!("{:.2} mm", f64::from(v) / 100.0),
        _ => crate::tl!("Default").into(),
    }
}

fn lw_param(lw: Lineweight) -> Value {
    match lw {
        Lineweight::Mm100(v) => json!(f64::from(v) / 100.0),
        _ => json!("Default"),
    }
}

/// The layers of the drawing file at `path`, as targets.
fn load(path: &str) -> Result<Vec<Target>, String> {
    let hooks = cadcraft_engine::cmd::file::io().ok_or("no drawing reader")?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let d = (hooks.read)(&bytes, path)?;
    Ok(d.layers.iter().map(|l| Target { name: l.name.clone(), color: l.color, linetype: l.linetype.clone(), lineweight: l.lineweight }).collect())
}

/// Map each layer of `from` to `to`.
fn map(maps: &mut Vec<(String, Target)>, from: &BTreeSet<String>, to: &Target) {
    for f in from {
        maps.retain(|(o, _)| !o.eq_ignore_ascii_case(f));
        maps.push((f.clone(), to.clone()));
    }
    maps.sort_by_key(|a| a.0.to_lowercase());
}

/// Translate: rename, merge and restyle the mapped layers. A source that is also another
/// mapping's target (a swap A→B, B→A or a chain A→B, B→C) first steps aside under a temporary
/// name, so every mapping takes the objects of its own source only.
fn translate(app: &mut CadApp, maps: &[(String, Target)]) {
    let _ = app.session.execute("undo", &json!({ "mode": "begin" }));
    let mut maps = maps.to_vec();
    for k in 0..maps.len() {
        let Some(from) = maps.get(k).map(|m| m.0.clone()) else { continue };
        let targeted = maps.iter().any(|(o, t)| !o.eq_ignore_ascii_case(&from) && t.name.eq_ignore_ascii_case(&from));
        if !targeted {
            continue;
        }
        let free = |n: &str| app.session.doc().is_ok_and(|d| d.layer(n).is_none());
        let Some(tmp) = (1..10_000).map(|i| format!("$LAYTRANS${i}")).find(|n| free(n)) else { continue };
        if app.run("layer.set", json!({ "name": from, "newName": tmp })).is_ok()
            && let Some(m) = maps.get_mut(k)
        {
            m.0 = tmp;
        }
    }
    for (from, to) in &maps {
        let (exists, has_lt) = match app.session.doc() {
            Ok(d) => (d.layer(&to.name).is_some(), d.linetype(&to.linetype).is_some()),
            Err(_) => break,
        };
        // A library linetype the drawing doesn't have yet is loaded; another one is left as is.
        let has_lt = has_lt || app.run("linetype", json!({ "load": to.linetype })).is_ok();
        let mut props = json!({ "color": to.color.name(), "lineweight": lw_param(to.lineweight) });
        if has_lt && let Some(o) = props.as_object_mut() {
            o.insert("linetype".into(), json!(to.linetype));
        }
        let with_name = |name: &str, extra: Option<(&str, &str)>| {
            let mut p = props.clone();
            if let Some(o) = p.as_object_mut() {
                o.insert("name".into(), json!(name));
                if let Some((k, v)) = extra {
                    o.insert(k.into(), json!(v));
                }
            }
            p
        };
        // `run` echoes a refusal (layer 0 can't be renamed…); the other mappings go on.
        let _ = if from.eq_ignore_ascii_case(&to.name) {
            app.run("layer.set", with_name(from, None))
        } else if exists {
            app.run("laymrg", json!({ "from": [from], "to": to.name })).and_then(|_| app.run("layer.set", with_name(&to.name, None)))
        } else {
            app.run("layer.set", with_name(from, Some(("newName", to.name.as_str()))))
        };
    }
    let _ = app.session.execute("undo", &json!({ "mode": "end" }));
}

fn swatch(ui: &mut egui::Ui, c: Color) {
    let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    crate::layers::swatch(ui.painter(), r, c);
}

pub(super) fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) -> Action {
    let t = Tokens::get();
    let Ok(d) = app.session.doc() else {
        super::no_drawing(ctx, "Layer Translator", open);
        return None;
    };
    let layers: Vec<String> = d.layers.iter().map(|l| l.name.clone()).collect();
    let linetypes: Vec<String> = {
        let mut v: Vec<String> =
            d.linetypes.iter().map(|l| l.name.clone()).filter(|n| !n.eq_ignore_ascii_case("ByLayer") && !n.eq_ignore_ascii_case("ByBlock")).collect();
        for n in cadcraft_doc::library::standard_linetypes().into_iter().map(|l| l.name) {
            if !v.iter().any(|x| x.eq_ignore_ascii_case(&n)) {
                v.push(n);
            }
        }
        v
    };
    let can_load = app.services.pick_open.is_some();
    let mut st = state(ctx, "laytrans_dlg", St::default);
    let (mut go, mut cancel, mut pick) = (false, false, false);
    let mut status: Option<String> = None;
    egui::Window::new(crate::tl!("Layer Translator"))
        .id(egui::Id::new("Layer Translator"))
        .open(open)
        .collapsible(false)
        .default_size(vec2(620.0, 480.0))
        .resizable(true)
        .show(ctx, |ui| {
            ui.columns(2, |cols| {
                let [left, right] = cols else { return };
                left.label(RichText::new(crate::tl!("Translate From")).strong());
                egui::ScrollArea::vertical().id_salt("laytrans_from").max_height(160.0).auto_shrink([false, false]).show(left, |ui| {
                    for n in &layers {
                        let mapped = st.maps.iter().any(|(o, _)| o == n);
                        let mut label = RichText::new(n);
                        if mapped {
                            label = label.color(t.text_dim);
                        }
                        if ui.selectable_label(st.from_sel.contains(n), label).clicked() {
                            if ui.input(|i| i.modifiers.command || i.modifiers.ctrl) {
                                if !st.from_sel.remove(n) {
                                    st.from_sel.insert(n.clone());
                                }
                            } else {
                                st.from_sel = BTreeSet::from([n.clone()]);
                            }
                        }
                    }
                });
                left.horizontal(|ui| {
                    let target = st.to_sel.as_ref().and_then(|n| st.targets.iter().find(|x| &x.name == n)).cloned();
                    if ui.add_enabled(target.is_some() && !st.from_sel.is_empty(), egui::Button::new(crate::tl!("Map"))).clicked()
                        && let Some(to) = target
                    {
                        let from = std::mem::take(&mut st.from_sel);
                        map(&mut st.maps, &from, &to);
                    }
                    if ui.button(crate::tl!("Map same")).on_hover_text(crate::tl!("Map every layer to the target layer of the same name")).clicked() {
                        for to in st.targets.clone() {
                            if let Some(f) = layers.iter().find(|l| l.eq_ignore_ascii_case(&to.name)) {
                                map(&mut st.maps, &BTreeSet::from([f.clone()]), &to);
                            }
                        }
                    }
                });
                right.label(RichText::new(crate::tl!("Translate To")).strong());
                egui::ScrollArea::vertical().id_salt("laytrans_to").max_height(160.0).auto_shrink([false, false]).show(right, |ui| {
                    if st.targets.is_empty() {
                        ui.label(RichText::new(crate::tl!("Load a drawing's layers or make new ones.")).color(t.text_dim));
                    }
                    for tg in &st.targets {
                        ui.horizontal(|ui| {
                            swatch(ui, tg.color);
                            if ui.selectable_label(st.to_sel.as_deref() == Some(tg.name.as_str()), &tg.name).clicked() {
                                st.to_sel = Some(tg.name.clone());
                            }
                        });
                    }
                });
                right.horizontal(|ui| {
                    if can_load && ui.button(crate::tl!("Load...")).on_hover_text(crate::tl!("Use the layers of another drawing")).clicked() {
                        pick = true;
                    }
                    if ui.button(crate::tl!("New...")).clicked() {
                        st.new = Some(Target {
                            name: String::new(),
                            color: Color::Index(7),
                            linetype: "Continuous".into(),
                            lineweight: Lineweight::Default,
                        });
                    }
                    let sel = st.to_sel.as_ref().and_then(|n| st.targets.iter().position(|x| &x.name == n));
                    if ui.add_enabled(sel.is_some(), egui::Button::new(crate::tl!("Remove"))).clicked()
                        && let Some(i) = sel
                    {
                        st.targets.remove(i);
                        st.to_sel = None;
                    }
                });
                if let Some(mut n) = st.new.take() {
                    let mut keep = true;
                    egui::Grid::new("laytrans_new").num_columns(2).show(right, |ui| {
                        ui.label(crate::tl!("Name:"));
                        ui.add(egui::TextEdit::singleline(&mut n.name).desired_width(140.0));
                        ui.end_row();
                        ui.label(crate::tl!("Color:"));
                        ui.horizontal(|ui| {
                            swatch(ui, n.color);
                            egui::ComboBox::from_id_salt("laytrans_color").selected_text(crate::i18n::t(&n.color.name())).show_ui(ui, |ui| {
                                for i in 1..=9u8 {
                                    ui.selectable_value(&mut n.color, Color::Index(i), crate::i18n::t(&Color::Index(i).name()));
                                }
                            });
                        });
                        ui.end_row();
                        ui.label(crate::tl!("Linetype:"));
                        egui::ComboBox::from_id_salt("laytrans_lt").selected_text(n.linetype.clone()).show_ui(ui, |ui| {
                            for l in &linetypes {
                                ui.selectable_value(&mut n.linetype, l.clone(), l);
                            }
                        });
                        ui.end_row();
                        ui.label(crate::tl!("Lineweight:"));
                        egui::ComboBox::from_id_salt("laytrans_lw").selected_text(lw_text(n.lineweight)).show_ui(ui, |ui| {
                            ui.selectable_value(&mut n.lineweight, Lineweight::Default, lw_text(Lineweight::Default));
                            for v in Lineweight::STANDARD {
                                ui.selectable_value(&mut n.lineweight, Lineweight::Mm100(v), lw_text(Lineweight::Mm100(v)));
                            }
                        });
                        ui.end_row();
                    });
                    right.horizontal(|ui| {
                        let name = n.name.trim().to_string();
                        let ok = !name.is_empty() && !st.targets.iter().any(|x| x.name.eq_ignore_ascii_case(&name));
                        if ui.add_enabled(ok, egui::Button::new(crate::tl!("OK"))).clicked() {
                            st.to_sel = Some(name.clone());
                            st.targets.push(Target { name, ..n.clone() });
                            keep = false;
                        }
                        if ui.button(crate::tl!("Cancel")).clicked() {
                            keep = false;
                        }
                    });
                    if keep {
                        st.new = Some(n);
                    }
                }
            });
            ui.separator();
            ui.label(RichText::new(crate::tl!("Layer Translation Mappings")).strong());
            egui::ScrollArea::vertical().id_salt("laytrans_maps").max_height(110.0).auto_shrink([false, false]).show(ui, |ui| {
                egui::Grid::new("laytrans_map_grid").num_columns(5).striped(true).spacing(vec2(14.0, 4.0)).show(ui, |ui| {
                    for h in [
                        crate::tl!("Old Layer Name"),
                        crate::tl!("New Layer Name"),
                        crate::tl!("Color"),
                        crate::tl!("Linetype"),
                        crate::tl!("Lineweight"),
                    ] {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();
                    for (i, (from, to)) in st.maps.iter().enumerate() {
                        if ui.selectable_label(st.map_sel == Some(i), from).clicked() {
                            st.map_sel = Some(i);
                        }
                        ui.label(&to.name);
                        ui.horizontal(|ui| {
                            swatch(ui, to.color);
                            ui.label(crate::i18n::t(&to.color.name()));
                        });
                        ui.label(&to.linetype);
                        ui.label(lw_text(to.lineweight));
                        ui.end_row();
                    }
                });
            });
            ui.horizontal(|ui| {
                let sel = st.map_sel.filter(|i| *i < st.maps.len());
                if ui.add_enabled(sel.is_some(), egui::Button::new(crate::tl!("Remove"))).clicked()
                    && let Some(i) = sel
                {
                    st.maps.remove(i);
                    st.map_sel = None;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    cancel = ui.button(crate::tl!("Cancel")).clicked();
                    go = ui.add_enabled(!st.maps.is_empty(), egui::Button::new(crate::tl!("Translate"))).clicked();
                });
            });
        });
    if pick && let Some(path) = app.services.pick_open.as_ref().and_then(|f| f()) {
        match load(&path) {
            Ok(ts) => {
                for tg in ts {
                    st.targets.retain(|x| !x.name.eq_ignore_ascii_case(&tg.name));
                    st.targets.push(tg);
                }
            }
            Err(e) => status = Some(e),
        }
    }
    if let Some(e) = status {
        app.session.echo(e);
    }
    set_state(ctx, "laytrans_dlg", st.clone());
    if cancel {
        *open = false;
    }
    if go {
        *open = false;
        translate(app, &st.maps);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str, color: u8, lw: u16) -> Target {
        Target { name: name.into(), color: Color::Index(color), linetype: "DASHED".into(), lineweight: Lineweight::Mm100(lw) }
    }

    #[test]
    fn translate_renames_merges_and_restyles() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        for n in ["wall", "WALLS-OLD", "A-WALL", "door"] {
            app.run("layer.new", json!({ "name": n })).unwrap();
        }
        app.run("layer.current", json!({ "name": "WALLS-OLD" })).unwrap();
        app.run("line", json!({ "points": [[0, 0], [1, 0]] })).unwrap();
        let mut maps = Vec::new();
        // Two old layers into an existing one, one renamed, one restyled in place.
        map(&mut maps, &BTreeSet::from(["wall".to_string(), "WALLS-OLD".to_string()]), &target("A-WALL", 1, 50));
        map(&mut maps, &BTreeSet::from(["door".to_string()]), &target("A-DOOR", 3, 25));
        map(&mut maps, &BTreeSet::from(["0".to_string()]), &target("0", 2, 18));
        assert_eq!(maps.len(), 4);
        let undo = app.session.state().unwrap().undo.len();
        translate(&mut app, &maps);
        let d = app.session.doc().unwrap();
        let names: Vec<&str> = d.layers.iter().map(|l| l.name.as_str()).collect();
        assert!(!names.contains(&"wall") && !names.contains(&"WALLS-OLD") && !names.contains(&"door"), "{names:?}");
        let wall = d.layer("A-WALL").unwrap();
        assert_eq!((wall.color, wall.lineweight, wall.linetype.as_str()), (Color::Index(1), Lineweight::Mm100(50), "DASHED"));
        assert_eq!(d.layer("A-DOOR").unwrap().color, Color::Index(3));
        assert_eq!(d.layer("0").unwrap().color, Color::Index(2));
        // The line moved with its layer.
        assert!(d.model.iter().all(|e| e.common.layer == "A-WALL"));
        assert_eq!(app.session.state().unwrap().undo.len(), undo + 1, "one undo step");
    }

    #[test]
    fn swaps_and_chains_keep_each_layers_objects_apart() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        for (y, n) in ["A", "B", "C", "D"].into_iter().enumerate() {
            app.run("layer.new", json!({ "name": n })).unwrap();
            app.run("layer.current", json!({ "name": n })).unwrap();
            app.run("line", json!({ "points": [[0, y], [1, y]] })).unwrap();
        }
        app.run("layer.current", json!({ "name": "0" })).unwrap();
        // The y of the lines on layer `l` (each started on its own layer at y 0, 1, 2, 3).
        let on = |app: &CadApp, l: &str| -> Vec<f64> {
            let d = app.session.doc().unwrap();
            d.model.iter().filter(|e| e.common.layer == l).filter_map(|e| e.kind.grips().first().map(|p| p.y)).collect()
        };
        let mut maps = Vec::new();
        // A and B swap; C → D → E chain.
        map(&mut maps, &BTreeSet::from(["A".to_string()]), &target("B", 1, 50));
        map(&mut maps, &BTreeSet::from(["B".to_string()]), &target("A", 2, 50));
        map(&mut maps, &BTreeSet::from(["C".to_string()]), &target("D", 3, 50));
        map(&mut maps, &BTreeSet::from(["D".to_string()]), &target("E", 4, 50));
        translate(&mut app, &maps);
        for (l, y) in [("B", 0.0), ("A", 1.0), ("D", 2.0), ("E", 3.0)] {
            assert_eq!(on(&app, l), vec![y], "{l}");
        }
        let d = app.session.doc().unwrap();
        assert!(d.layer("C").is_none() && d.layers.iter().all(|l| !l.name.starts_with("$LAYTRANS$")));
        assert_eq!(d.layer("B").unwrap().color, Color::Index(1));
        assert_eq!(d.layer("E").unwrap().color, Color::Index(4));
    }
}
