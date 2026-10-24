//! The UCS dialog (UCSMAN, UC, DDUCS; Tools ▸ Named UCS...): Named UCSs (World, Previous, the
//! unnamed current UCS and the saved ones: Set Current, Rename — on Unnamed it saves the current
//! UCS under that name —, Delete and Details), Orthographic UCSs (Top … Right relative to World or
//! a named UCS, each with its depth) and Settings (UCS icon on, UCSFOLLOW). Every change goes
//! through `ucsman`, `ucsicon` or `setvar`, so it is undoable and scriptable.
// ponytail: depths last while the app runs (AutoCAD keeps them in the drawing); "Display at UCS
// origin point" and "Save UCS with viewport" are shown off and on, unavailable: the UCS icon
// draws at the corner, and every viewport keeps its own UCS.

use cadcraft_engine::cmd::ucsman::{ORTHOGRAPHIC, list, orthographic, plane};
use cadcraft_geom::{Mat4, Vec3};
use egui::{RichText, vec2};
use serde_json::{Value, json};

use super::{Action, fmt_num, no_drawing, num_field, set_state, state};
use crate::CadApp;
use crate::theme::Tokens;

const WORLD: &str = "World";
const PREVIOUS: &str = "Previous";
const UNNAMED: &str = "Unnamed";

#[derive(Clone, Copy, Default, PartialEq)]
enum Tab {
    #[default]
    Named,
    Orthographic,
    Settings,
}

#[derive(Clone, Default)]
struct St {
    tab: Tab,
    sel: Option<String>,
    ortho: usize,
    relative: String,
    depths: [f64; 6],
    details: bool,
    /// The new name while renaming the selected UCS (or naming Unnamed).
    renaming: Option<String>,
}

fn v3(v: &Value) -> Vec3 {
    let n = |i: usize| v.get(i).and_then(Value::as_f64).unwrap_or(0.0);
    Vec3::new(n(0), n(1), n(2))
}

fn details(ui: &mut egui::Ui, c: Option<Mat4>) {
    let t = Tokens::get();
    let Some(c) = c else {
        ui.label(RichText::new(crate::tl!("No details for this UCS.")).color(t.text_dim));
        return;
    };
    egui::Grid::new("ucsman_details").num_columns(4).spacing(vec2(12.0, 2.0)).show(ui, |ui| {
        for (label, v) in
            [(crate::tl!("Origin"), c.col(3)), (crate::tl!("X Axis"), c.col(0)), (crate::tl!("Y Axis"), c.col(1)), (crate::tl!("Z Axis"), c.col(2))]
        {
            ui.label(RichText::new(label).color(t.text_dim));
            for x in [v.x, v.y, v.z] {
                // + 0.0: no "-0".
                ui.label(fmt_num(x + 0.0));
            }
            ui.end_row();
        }
    });
}

pub(super) fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) -> Action {
    let t = Tokens::get();
    let (Ok(info), Ok(d)) = (list(&app.session), app.session.doc()) else {
        no_drawing(ctx, "UCS", open);
        return None;
    };
    let current = info["current"].as_str().unwrap_or(WORLD).to_string();
    let saved: Vec<(String, Option<Mat4>)> = info["ucs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| Some((u["name"].as_str()?.to_string(), Mat4::frame(v3(&u["origin"]), v3(&u["xaxis"]), v3(&u["yaxis"])))))
        .collect();
    let mut names = vec![WORLD.to_string()];
    if info["previous"].as_bool() == Some(true) {
        names.push(PREVIOUS.into());
    }
    if current == UNNAMED {
        names.push(UNNAMED.into());
    }
    names.extend(saved.iter().map(|(n, _)| n.clone()));
    let cplane = app.session.cplane();
    let ucsicon = app.session.settings.ucsicon;
    let follow = d.header.i64("UCSFOLLOW", 0) != 0;
    let mut st = state(ctx, "ucsman_dlg", || St { relative: WORLD.into(), ..St::default() });
    if st.sel.as_ref().is_none_or(|s| !names.contains(s)) {
        st.sel = Some(current.clone());
        st.renaming = None;
    }
    if st.relative != WORLD && !saved.iter().any(|(n, _)| *n == st.relative) {
        st.relative = WORLD.into();
    }
    let base = plane(d, &st.relative).unwrap_or(Mat4::IDENTITY);
    let mut action: Action = None;
    let mut close = false;
    egui::Window::new(crate::tl!("UCS")).id(egui::Id::new("UCS dialog")).open(open).default_size(vec2(420.0, 360.0)).resizable(true).show(
        ctx,
        |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut st.tab, Tab::Named, crate::tl!("Named UCSs"));
                ui.selectable_value(&mut st.tab, Tab::Orthographic, crate::tl!("Orthographic UCSs"));
                ui.selectable_value(&mut st.tab, Tab::Settings, crate::tl!("Settings"));
            });
            ui.separator();
            match st.tab {
                Tab::Named => {
                    ui.label(crate::tf!("Current UCS: {name}", name = crate::i18n::t(&current)));
                    egui::ScrollArea::vertical().id_salt("ucsman_named").max_height(170.0).auto_shrink([false, true]).show(ui, |ui| {
                        for n in &names {
                            let shown = if matches!(n.as_str(), WORLD | PREVIOUS | UNNAMED) { crate::i18n::t(n).to_string() } else { n.clone() };
                            let mut label = RichText::new(shown);
                            if *n == current {
                                label = label.strong();
                            }
                            let r = ui.selectable_label(st.sel.as_ref() == Some(n), label);
                            if r.clicked() {
                                st.sel = Some(n.clone());
                                st.renaming = None;
                            }
                            if r.double_clicked() {
                                action = set_named(n);
                            }
                        }
                    });
                    let sel = st.sel.clone().unwrap_or_default();
                    let named = saved.iter().any(|(n, _)| *n == sel);
                    ui.horizontal(|ui| {
                        if ui.add_enabled(sel != current || sel == PREVIOUS, egui::Button::new(crate::tl!("Set Current"))).clicked() {
                            action = set_named(&sel);
                        }
                        let rename = if sel == UNNAMED { crate::tl!("Save As...") } else { crate::tl!("Rename") };
                        if ui.add_enabled(named || sel == UNNAMED, egui::Button::new(rename)).clicked() {
                            st.renaming = Some(if named { sel.clone() } else { String::new() });
                        }
                        if ui.add_enabled(named, egui::Button::new(crate::tl!("Delete"))).clicked() {
                            action = Some(("ucsman", json!({ "option": "delete", "name": sel })));
                        }
                        ui.toggle_value(&mut st.details, crate::tl!("Details"));
                    });
                    if let Some(mut to) = st.renaming.take() {
                        ui.horizontal(|ui| {
                            let r = ui.add(egui::TextEdit::singleline(&mut to).hint_text(crate::tl!("UCS name")).desired_width(180.0));
                            let name = to.trim().to_string();
                            let ok = !name.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(&name));
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if ui.add_enabled(ok, egui::Button::new(crate::tl!("OK"))).clicked() || (enter && ok) {
                                action = Some(if sel == UNNAMED {
                                    ("ucsman", json!({ "option": "save", "name": name }))
                                } else {
                                    ("ucsman", json!({ "option": "rename", "name": sel, "to": name }))
                                });
                                st.sel = Some(name);
                            } else if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                st.renaming = Some(to);
                            }
                        });
                    }
                    if st.details {
                        ui.separator();
                        let c = match sel.as_str() {
                            WORLD => Some(Mat4::IDENTITY),
                            UNNAMED => Some(cplane),
                            PREVIOUS => None,
                            n => saved.iter().find(|(s, _)| s == n).and_then(|(_, c)| *c),
                        };
                        details(ui, c);
                    }
                }
                Tab::Orthographic => {
                    egui::Grid::new("ucsman_ortho").num_columns(2).striped(true).spacing(vec2(24.0, 4.0)).show(ui, |ui| {
                        ui.label(RichText::new(crate::tl!("Name")).strong());
                        ui.label(RichText::new(crate::tl!("Depth")).strong());
                        ui.end_row();
                        for (i, n) in ORTHOGRAPHIC.iter().enumerate() {
                            let depth = st.depths.get(i).copied().unwrap_or(0.0);
                            let here = orthographic(&base, n, depth).is_some_and(|c| c == cplane);
                            let mut label = RichText::new(crate::i18n::t(n));
                            if here {
                                label = label.strong();
                            }
                            let r = ui.selectable_label(st.ortho == i, label);
                            if r.clicked() {
                                st.ortho = i;
                            }
                            if r.double_clicked() {
                                action = set_ortho(n, &st.relative, depth);
                            }
                            ui.label(RichText::new(fmt_num(depth)).color(t.text_dim));
                            ui.end_row();
                        }
                    });
                    let name = ORTHOGRAPHIC.get(st.ortho).copied().unwrap_or("Top");
                    let depth = st.depths.get(st.ortho).copied().unwrap_or(0.0);
                    ui.horizontal(|ui| {
                        ui.label(crate::tl!("Depth:"));
                        if let Some(v) = num_field(ui, ("ucsman_depth", st.ortho), depth)
                            && let Some(slot) = st.depths.get_mut(st.ortho)
                        {
                            *slot = v;
                        }
                        if ui.button(crate::tl!("Set Current")).clicked() {
                            action = set_ortho(name, &st.relative, depth);
                        }
                        ui.toggle_value(&mut st.details, crate::tl!("Details"));
                    });
                    ui.horizontal(|ui| {
                        ui.label(crate::tl!("Relative to:"));
                        let shown = if st.relative == WORLD { crate::i18n::t(WORLD).to_string() } else { st.relative.clone() };
                        egui::ComboBox::from_id_salt("ucsman_relative").selected_text(shown).width(160.0).show_ui(ui, |ui| {
                            ui.selectable_value(&mut st.relative, WORLD.to_string(), crate::i18n::t(WORLD));
                            for (n, _) in &saved {
                                ui.selectable_value(&mut st.relative, n.clone(), n);
                            }
                        });
                    });
                    if st.details {
                        ui.separator();
                        details(ui, orthographic(&base, name, depth));
                    }
                }
                Tab::Settings => {
                    ui.label(RichText::new(crate::tl!("UCS Icon settings")).strong());
                    let mut on = ucsicon;
                    if ui.checkbox(&mut on, crate::tl!("On")).changed() {
                        action = Some(("ucsicon", json!({ "on": on })));
                    }
                    ui.add_enabled_ui(false, |ui| {
                        ui.checkbox(&mut false, crate::tl!("Display at UCS origin point"))
                            .on_disabled_hover_text(crate::tl!("The UCS icon is drawn at the corner of the view."));
                    });
                    ui.add_space(6.0);
                    ui.label(RichText::new(crate::tl!("UCS settings")).strong());
                    ui.add_enabled_ui(false, |ui| {
                        ui.checkbox(&mut true, crate::tl!("Save UCS with viewport"))
                            .on_disabled_hover_text(crate::tl!("Every viewport keeps its own UCS."));
                    });
                    let mut f = follow;
                    if ui.checkbox(&mut f, crate::tl!("Update view to Plan when UCS is changed")).changed() {
                        action = Some(("setvar", json!({ "name": "UCSFOLLOW", "value": i32::from(f) })));
                    }
                }
            }
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.button(crate::tl!("Close")).clicked();
            });
        },
    );
    set_state(ctx, "ucsman_dlg", st);
    if close {
        *open = false;
    }
    action
}

/// Make the Named UCSs entry `n` current.
fn set_named(n: &str) -> Action {
    match n {
        UNNAMED => None,
        PREVIOUS => Some(("ucsman", json!({ "option": "previous" }))),
        _ => Some(("ucsman", json!({ "option": "restore", "name": n }))),
    }
}

fn set_ortho(n: &str, relative: &str, depth: f64) -> Action {
    Some(("ucsman", json!({ "option": "orthographic", "ortho": n, "relativeTo": relative, "depth": depth })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_actions_reach_the_ucs() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.run("ucs", json!({ "origin": [3, 4, 0] })).unwrap();
        let (cmd, p) = set_ortho("Front", WORLD, 0.0).unwrap();
        app.run(cmd, p).unwrap();
        assert_eq!(app.session.cplane(), cadcraft_engine::cmd::ucsman::orthographic(&Mat4::IDENTITY, "front", 0.0).unwrap());
        // Previous goes back to the moved UCS; Unnamed is already current.
        let (cmd, p) = set_named(PREVIOUS).unwrap();
        app.run(cmd, p).unwrap();
        assert_eq!(app.session.cplane().col(3), Vec3::new(3.0, 4.0, 0.0));
        assert!(set_named(UNNAMED).is_none());
        let (cmd, p) = set_named(WORLD).unwrap();
        app.run(cmd, p).unwrap();
        assert_eq!(app.session.cplane(), Mat4::IDENTITY);
    }

    #[test]
    fn typed_names_open_the_dialog_and_json_does_not() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        for typed in ["ucsman", "UC", "dducs"] {
            app.ui.dialog = None;
            app.cmdline(typed);
            assert_eq!(app.ui.dialog.as_deref(), Some("ucsman"), "{typed}");
        }
        app.ui.dialog = None;
        let r = app.run("ucsman", json!({ "option": "list" })).unwrap();
        assert_eq!(r["current"], json!("World"));
        assert!(app.ui.dialog.is_none());
        // The dialog draws (each tab) without a panic.
        let ctx = egui::Context::default();
        for tab in [Tab::Named, Tab::Orthographic, Tab::Settings] {
            super::super::set_state(&ctx, "ucsman_dlg", St { tab, details: true, relative: WORLD.into(), ..St::default() });
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let mut open = true;
                dialog(&mut app, ui.ctx(), &mut open);
            });
            out.textures_delta.clear();
        }
    }
}
