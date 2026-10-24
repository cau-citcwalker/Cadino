//! Layer Translator (LAYTRANS; Tools ▸ CAD Standards ▸ Layer Translator...): map this
//! drawing's layers to target layers (loaded from another drawing or defined here) with their
//! colour, linetype and lineweight, then Translate: a layer mapped to a new name is renamed, one
//! mapped to an existing layer is merged into it, and the target's properties are set, through
//! `layer.set`, `laymrg` and `linetype` in one undo step.
//!
//! Settings: Force object color and linetype to BYLAYER (the translated objects' own colour and
//! linetype become ByLayer, through `properties.set`), Translate objects in blocks (off: only the
//! objects of model space and the layouts move, through `layer.new`/`layer.set` and `laymch`; a
//! source layer block definitions still use stays) and Write transaction log (`<drawing>.log`
//! beside the saved drawing: each mapping and how many objects it took).
//!
//! Save... writes the targets and mappings to a mapping file, Load... reads one back (or takes a
//! drawing's layers as targets). A mapping file is JSON:
//! `{"format": "cadino.laytrans", "version": 1, "targets": [{"name", "color" ("Red", "Color 34",
//! "255,128,0"…), "linetype", "lineweight" (mm, or "Default")}], "mappings": [{"from": old layer,
//! "to": a target's name}]}`. Reading and writing files is refused while file access is off.
// ponytail: forcing ByLayer reaches objects in model space and the layouts, not inside blocks.

use std::collections::BTreeSet;

use cadcraft_color::Color;
use cadcraft_doc::{Handle, Lineweight};
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

/// The Settings dialog's choices.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Settings {
    force_bylayer: bool,
    in_blocks: bool,
    log: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { force_bylayer: false, in_blocks: true, log: false }
    }
}

#[derive(Clone, Default)]
struct St {
    settings: Settings,
    show_settings: bool,
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

/// Largest mapping file read (a hostile file can be huge).
const MAX_FILE: u64 = 16 << 20;
/// Most targets or mappings taken from a mapping file.
const MAX_ENTRIES: usize = 10_000;

/// `Err` while file access is denied (the AI assistant's setting).
fn files_allowed() -> Result<(), String> {
    cadcraft_engine::limits::files_denied().map_or(Ok(()), |why| Err(why.to_string()))
}

/// The layers of the drawing file at `path`, as targets.
fn load(path: &str) -> Result<Vec<Target>, String> {
    files_allowed()?;
    let hooks = cadcraft_engine::cmd::file::io().ok_or("no drawing reader")?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let d = (hooks.read)(&bytes, path)?;
    Ok(d.layers.iter().map(|l| Target { name: l.name.clone(), color: l.color, linetype: l.linetype.clone(), lineweight: l.lineweight }).collect())
}

/// A mapping file's contents (see the module documentation).
fn mapping_file(targets: &[Target], maps: &[(String, Target)]) -> Value {
    let mut all: Vec<&Target> = targets.iter().collect();
    for (_, t) in maps {
        if !all.iter().any(|x| x.name.eq_ignore_ascii_case(&t.name)) {
            all.push(t);
        }
    }
    let target = |t: &Target| {
        let lw = match t.lineweight {
            Lineweight::Mm100(v) => json!(f64::from(v) / 100.0),
            _ => json!("Default"),
        };
        json!({ "name": t.name, "color": t.color.name(), "linetype": t.linetype, "lineweight": lw })
    };
    json!({
        "format": "cadino.laytrans",
        "version": 1,
        "targets": all.into_iter().map(target).collect::<Vec<_>>(),
        "mappings": maps.iter().map(|(from, to)| json!({ "from": from, "to": to.name })).collect::<Vec<_>>(),
    })
}

/// Targets and mappings from a mapping file's text. Mappings to a target the file doesn't
/// define are left out.
fn read_mapping_file(text: &str) -> Result<(Vec<Target>, Vec<(String, Target)>), String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if v["format"] != "cadino.laytrans" {
        return Err(crate::tl!("This is not a layer translation mapping file.").into());
    }
    let mut targets: Vec<Target> = Vec::new();
    for t in v["targets"].as_array().into_iter().flatten().take(MAX_ENTRIES) {
        let Some(name) = t["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) else { continue };
        let lineweight = match t["lineweight"].as_f64() {
            Some(mm) if mm.is_finite() && (0.0..=2.11).contains(&mm) => Lineweight::Mm100((mm * 100.0).round() as u16),
            _ => Lineweight::Default,
        };
        let target = Target {
            name: name.to_string(),
            color: t["color"].as_str().and_then(Color::parse).filter(|c| matches!(c, Color::Index(_) | Color::True(_))).unwrap_or(Color::Index(7)),
            linetype: t["linetype"].as_str().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Continuous").to_string(),
            lineweight,
        };
        targets.retain(|x| !x.name.eq_ignore_ascii_case(name));
        targets.push(target);
    }
    let mut maps = Vec::new();
    for m in v["mappings"].as_array().into_iter().flatten().take(MAX_ENTRIES) {
        let (Some(from), Some(to)) = (m["from"].as_str(), m["to"].as_str()) else { continue };
        if let Some(t) = targets.iter().find(|t| t.name.eq_ignore_ascii_case(to)) {
            map(&mut maps, &BTreeSet::from([from.trim().to_string()]), t);
        }
    }
    Ok((targets, maps))
}

fn load_mappings(path: &str) -> Result<(Vec<Target>, Vec<(String, Target)>), String> {
    files_allowed()?;
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_FILE {
        return Err(crate::tl!("The mapping file is too large.").into());
    }
    read_mapping_file(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
}

fn save_mappings(path: &str, targets: &[Target], maps: &[(String, Target)]) -> Result<(), String> {
    files_allowed()?;
    let text = serde_json::to_string_pretty(&mapping_file(targets, maps)).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// Write the transaction log beside the saved drawing (`<drawing>.log`). Returns its path.
fn write_log(app: &CadApp, lines: &[String]) -> Result<String, String> {
    files_allowed()?;
    let path = app
        .session
        .state()
        .ok()
        .and_then(|s| s.path.clone())
        .ok_or_else(|| crate::tl!("Save the drawing first: the log is written beside it.").to_string())?;
    let log = std::path::Path::new(&path).with_extension("log");
    let mut text = format!("Layer Translator: {}\n", crate::menus::file_name(&path));
    for l in lines {
        text.push_str(l);
        text.push('\n');
    }
    std::fs::write(&log, text).map_err(|e| e.to_string())?;
    Ok(log.to_string_lossy().to_string())
}

/// The objects of model space and the layouts on layer `name`.
fn on_layer(app: &CadApp, name: &str) -> Vec<Handle> {
    let Ok(d) = app.session.doc() else { return Vec::new() };
    d.model
        .iter()
        .chain(d.layouts.iter().flat_map(|l| l.entities.iter()))
        .filter(|e| e.common.layer.eq_ignore_ascii_case(name))
        .map(|e| e.handle)
        .collect()
}

/// Map each layer of `from` to `to`.
fn map(maps: &mut Vec<(String, Target)>, from: &BTreeSet<String>, to: &Target) {
    for f in from {
        maps.retain(|(o, _)| !o.eq_ignore_ascii_case(f));
        maps.push((f.clone(), to.clone()));
    }
    maps.sort_by_key(|a| a.0.to_lowercase());
}

/// Translate the mapped layers with `set`, in one undo step. Returns the transaction log's lines.
fn translate(app: &mut CadApp, maps: &[(String, Target)], set: Settings) -> Vec<String> {
    // Each source's objects, gathered first: forcing ByLayer, the log and (without blocks) the
    // move all take exactly what was on the source, also in swaps and chains.
    let objects: Vec<Vec<Handle>> = maps.iter().map(|(from, _)| on_layer(app, from)).collect();
    let _ = app.session.execute("undo", &json!({ "mode": "begin" }));
    if set.in_blocks {
        rename_and_merge(app, maps);
    } else {
        move_objects(app, maps, &objects);
    }
    let moved: Vec<String> = objects.iter().flatten().map(|h| h.hex()).collect();
    if set.force_bylayer && !moved.is_empty() {
        let _ = app.run("properties.set", json!({ "handles": moved, "color": "ByLayer", "linetype": "ByLayer" }));
    }
    let _ = app.session.execute("undo", &json!({ "mode": "end" }));
    maps.iter()
        .zip(&objects)
        .map(|((from, to), hs)| {
            let n = hs.len();
            format!(
                "{from} -> {} (color {}, linetype {}, lineweight {}): {n} object(s)",
                to.name,
                to.color.name(),
                to.linetype,
                lw_text(to.lineweight)
            )
        })
        .collect()
}

/// The layer properties of `to` for `layer.set`, loading a library linetype the drawing doesn't
/// have yet (another one is left as is).
fn props(app: &mut CadApp, to: &Target) -> Value {
    let has_lt = app.session.doc().is_ok_and(|d| d.linetype(&to.linetype).is_some());
    let has_lt = has_lt || app.run("linetype", json!({ "load": to.linetype })).is_ok();
    let mut props = json!({ "color": to.color.name(), "lineweight": lw_param(to.lineweight) });
    if has_lt && let Some(o) = props.as_object_mut() {
        o.insert("linetype".into(), json!(to.linetype));
    }
    props
}

/// Without objects in blocks: each target layer is made (or restyled) and the gathered objects
/// move onto it; a source layer nothing uses any more goes, as a merge would remove it.
fn move_objects(app: &mut CadApp, maps: &[(String, Target)], objects: &[Vec<Handle>]) {
    for ((_, to), hs) in maps.iter().zip(objects) {
        if app.session.doc().is_ok_and(|d| d.layer(&to.name).is_none()) && app.run("layer.new", json!({ "name": to.name })).is_err() {
            continue;
        }
        let mut p = props(app, to);
        p["name"] = json!(to.name);
        let _ = app.run("layer.set", p);
        if !hs.is_empty() {
            let _ = app.run("laymch", json!({ "handles": hs.iter().map(|h| h.hex()).collect::<Vec<_>>(), "layer": to.name }));
        }
    }
    for (from, _) in maps {
        // Layer 0, the current layer and layers block definitions use are refused and stay.
        if !maps.iter().any(|(_, t)| t.name.eq_ignore_ascii_case(from)) {
            let _ = app.session.execute("layer.delete", &json!({ "name": from }));
        }
    }
}

/// Rename, merge and restyle the mapped layers (objects in blocks follow their layer). A source
/// that is also another mapping's target (a swap A→B, B→A or a chain A→B, B→C) first steps
/// aside under a temporary name, so every mapping takes the objects of its own source only.
fn rename_and_merge(app: &mut CadApp, maps: &[(String, Target)]) {
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
        let Ok(exists) = app.session.doc().map(|d| d.layer(&to.name).is_some()) else { break };
        let props = props(app, to);
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
    let can_save = app.services.pick_save.is_some();
    let mut st = state(ctx, "laytrans_dlg", St::default);
    let (mut go, mut cancel, mut pick, mut save) = (false, false, false, false);
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
                    if can_load
                        && ui
                            .button(crate::tl!("Load..."))
                            .on_hover_text(crate::tl!("Use the layers of another drawing, or a saved mapping file (.json)"))
                            .clicked()
                    {
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
                if can_save
                    && ui
                        .add_enabled(!st.maps.is_empty(), egui::Button::new(crate::tl!("Save...")))
                        .on_hover_text(crate::tl!("Save the mappings to a mapping file"))
                        .clicked()
                {
                    save = true;
                }
                ui.toggle_value(&mut st.show_settings, crate::tl!("Settings..."));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    cancel = ui.button(crate::tl!("Cancel")).clicked();
                    go = ui.add_enabled(!st.maps.is_empty(), egui::Button::new(crate::tl!("Translate"))).clicked();
                });
            });
            if st.show_settings {
                ui.group(|ui| {
                    let s = &mut st.settings;
                    ui.checkbox(&mut s.force_bylayer, crate::tl!("Force object color and linetype to BYLAYER"));
                    ui.checkbox(&mut s.in_blocks, crate::tl!("Translate objects in blocks"));
                    ui.checkbox(&mut s.log, crate::tl!("Write transaction log"))
                        .on_hover_text(crate::tl!("Written beside the saved drawing, with the .log extension"));
                });
            }
        });
    if pick && let Some(path) = app.services.pick_open.as_ref().and_then(|f| f()) {
        let file = if path.to_ascii_lowercase().ends_with(".json") { load_mappings(&path) } else { load(&path).map(|ts| (ts, Vec::new())) };
        match file {
            Ok((ts, maps)) => {
                for tg in ts {
                    st.targets.retain(|x| !x.name.eq_ignore_ascii_case(&tg.name));
                    st.targets.push(tg);
                }
                for (from, to) in maps {
                    map(&mut st.maps, &BTreeSet::from([from]), &to);
                }
            }
            Err(e) => status = Some(e),
        }
    }
    if save && let Some(path) = app.services.pick_save.as_ref().and_then(|f| f("layer-translations.json")) {
        status = Some(match save_mappings(&path, &st.targets, &st.maps) {
            Ok(()) => crate::tf!("Mappings saved to {path}.", path = path),
            Err(e) => e,
        });
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
        let lines = translate(app, &st.maps, st.settings);
        if st.settings.log {
            let msg = match write_log(app, &lines) {
                Ok(path) => crate::tf!("Transaction log written to {path}.", path = path),
                Err(e) => e,
            };
            app.session.echo(msg);
        }
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
        let log = translate(&mut app, &maps, Settings::default());
        assert!(log.iter().any(|l| l == "WALLS-OLD -> A-WALL (color Red, linetype DASHED, lineweight 0.50 mm): 1 object(s)"), "{log:?}");
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
        // With objects in blocks (rename and merge) and without (objects move).
        for in_blocks in [true, false] {
            swaps_and_chains(Settings { in_blocks, ..Settings::default() });
        }
    }

    fn swaps_and_chains(set: Settings) {
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
        translate(&mut app, &maps, set);
        for (l, y) in [("B", 0.0), ("A", 1.0), ("D", 2.0), ("E", 3.0)] {
            assert_eq!(on(&app, l), vec![y], "{l}");
        }
        let d = app.session.doc().unwrap();
        assert!(d.layer("C").is_none() && d.layers.iter().all(|l| !l.name.starts_with("$LAYTRANS$")));
        assert_eq!(d.layer("B").unwrap().color, Color::Index(1));
        assert_eq!(d.layer("E").unwrap().color, Color::Index(4));
    }

    #[test]
    fn settings_force_bylayer_and_leave_blocks_alone() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.run("layer.new", json!({ "name": "old" })).unwrap();
        app.run("layer.current", json!({ "name": "old" })).unwrap();
        app.run("line", json!({ "points": [[0, 0], [1, 0]] })).unwrap();
        let h = app.session.doc().unwrap().model.iter().next().unwrap().handle.hex();
        app.run("block", json!({ "name": "B", "base": [0, 0], "handles": [h], "keep": "delete" })).unwrap();
        app.run("circle", json!({ "center": [0, 0], "radius": 1 })).unwrap();
        let c = app.session.doc().unwrap().model.iter().next().unwrap().handle.hex();
        app.run("properties.set", json!({ "handles": [c], "color": "blue", "linetype": "Continuous" })).unwrap();
        app.run("layer.current", json!({ "name": "0" })).unwrap();
        let mut maps = Vec::new();
        map(&mut maps, &BTreeSet::from(["old".to_string()]), &target("new", 1, 50));
        let undo = app.session.state().unwrap().undo.len();
        translate(&mut app, &maps, Settings { force_bylayer: true, in_blocks: false, log: false });
        let d = app.session.doc().unwrap();
        let circle = d.model.iter().next().unwrap();
        assert_eq!((circle.common.layer.as_str(), circle.common.color, circle.common.linetype.as_str()), ("new", Color::ByLayer, "ByLayer"));
        // The block's line stays on its layer, which the block still uses.
        assert_eq!(d.block("B").unwrap().entities.iter().next().unwrap().common.layer, "old");
        assert!(d.layer("old").is_some() && d.layer("new").is_some_and(|l| l.color == Color::Index(1)));
        assert_eq!(app.session.state().unwrap().undo.len(), undo + 1, "one undo step");
    }

    #[test]
    fn mapping_files_round_trip_and_respect_file_access() {
        let mut maps = Vec::new();
        let wall = Target {
            name: "A-WALL".into(),
            color: Color::True(cadcraft_color::Rgb(10, 20, 30)),
            linetype: "DASHED".into(),
            lineweight: Lineweight::Default,
        };
        map(&mut maps, &BTreeSet::from(["wall".to_string(), "Walls".to_string()]), &wall);
        map(&mut maps, &BTreeSet::from(["door".to_string()]), &target("A-DOOR", 3, 25));
        let text = mapping_file(&[target("A-GLAZ", 4, 18)], &maps).to_string();
        let (targets, back) = read_mapping_file(&text).unwrap();
        assert_eq!(targets.len(), 3);
        assert_eq!(back, maps);
        // Not a mapping file, or hostile values: refused or made safe.
        assert!(read_mapping_file("{}").is_err() && read_mapping_file("[1,").is_err());
        let odd = json!({ "format": "cadino.laytrans", "targets": [{ "name": " X ", "color": "ByLayer", "lineweight": 1e300 }, { "name": "" }, 7],
            "mappings": [{ "from": "a", "to": "missing" }, { "from": "b", "to": "x" }] });
        let (targets, back) = read_mapping_file(&odd.to_string()).unwrap();
        assert_eq!(
            targets,
            vec![Target { name: "X".into(), color: Color::Index(7), linetype: "Continuous".into(), lineweight: Lineweight::Default }]
        );
        assert_eq!(back.len(), 1);
        let _deny = cadcraft_engine::limits::deny_files("files are off");
        assert_eq!(save_mappings("never-written.json", &[], &maps), Err("files are off".to_string()));
        assert!(load_mappings("never-written.json").is_err() && load("never.dxf").is_err());
        let app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        assert_eq!(write_log(&app, &[]), Err("files are off".to_string()));
    }
}
