//! The Layer States Manager (LAYERSTATE, LAS, LMAN; Format ▸ Layer States Manager...): the saved
//! layer states with New, Update, Restore, Rename and Delete, through `layerstate.save`,
//! `layerstate.restore`, `layerstate.rename` and `layerstate.delete`.

use egui::{RichText, vec2};
use serde_json::json;

use super::{Action, no_drawing, set_state, state};
use crate::CadApp;
use crate::theme::Tokens;

#[derive(Clone, Default)]
struct DlgState {
    sel: Option<String>,
    new_name: String,
    /// The new name while renaming the selected state.
    renaming: Option<String>,
}

/// Whether `name` can name a new state: not empty and not a saved state's name in any case.
fn free(states: &[(String, usize)], name: &str) -> bool {
    !name.is_empty() && !states.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
}

pub(super) fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) -> Action {
    let t = Tokens::get();
    let Ok(d) = app.session.doc() else {
        no_drawing(ctx, "Layer States Manager", open);
        return None;
    };
    let states: Vec<(String, usize)> = d.layer_states.iter().map(|s| (s.name.clone(), s.layers.len())).collect();
    let mut st = state(ctx, "layerstates_dlg", DlgState::default);
    if st.sel.as_ref().is_some_and(|s| !states.iter().any(|x| x.0 == *s)) {
        st.sel = None;
        st.renaming = None;
    }
    let mut action: Action = None;
    let mut close = false;
    egui::Window::new(crate::tl!("Layer States Manager"))
        .id(egui::Id::new("Layer States Manager"))
        .open(open)
        .default_size(vec2(460.0, 320.0))
        .resizable(true)
        .show(ctx, |ui| {
            ui.label(RichText::new(crate::tl!("Layer states")).strong());
            egui::ScrollArea::vertical().id_salt("layerstates_list").max_height(200.0).auto_shrink([false, true]).show(ui, |ui| {
                if states.is_empty() {
                    ui.label(RichText::new(crate::tl!("No saved layer states")).color(t.text_dim));
                }
                egui::Grid::new("layerstates_grid").num_columns(2).striped(true).spacing(vec2(16.0, 4.0)).show(ui, |ui| {
                    for (name, n) in &states {
                        let r = ui.selectable_label(st.sel.as_ref() == Some(name), name);
                        if r.clicked() {
                            st.sel = Some(name.clone());
                            st.renaming = None;
                        }
                        if r.double_clicked() {
                            action = Some(("layerstate.restore", json!({ "name": name })));
                        }
                        ui.label(RichText::new(crate::i18n::count(*n as u64, "{n} layer", "{n} layers")).color(t.text_dim));
                        ui.end_row();
                    }
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut st.new_name).hint_text(crate::tl!("New state name")).desired_width(160.0));
                let name = st.new_name.trim().to_string();
                // An existing name is Update's job: New never overwrites a state silently.
                if ui
                    .add_enabled(free(&states, &name), egui::Button::new(crate::tl!("New")))
                    .on_hover_text(crate::tl!("Save the layers' current settings as a new layer state"))
                    .clicked()
                {
                    action = Some(("layerstate.save", json!({ "name": name })));
                    st.sel = Some(name);
                    st.new_name.clear();
                }
            });
            let sel = st.sel.clone();
            ui.add_enabled_ui(sel.is_some(), |ui| {
                ui.horizontal(|ui| {
                    // Disabled without a selection, so the names below are never empty when used.
                    let sel = sel.clone().unwrap_or_default();
                    if ui.button(crate::tl!("Restore")).clicked() {
                        action = Some(("layerstate.restore", json!({ "name": sel })));
                    }
                    if ui.button(crate::tl!("Update")).on_hover_text(crate::tl!("Save the layers' current settings in the selected state")).clicked()
                    {
                        action = Some(("layerstate.save", json!({ "name": sel })));
                    }
                    if ui.button(crate::tl!("Rename")).clicked() {
                        st.renaming = Some(sel.clone());
                    }
                    if ui.button(crate::tl!("Delete")).clicked() {
                        action = Some(("layerstate.delete", json!({ "name": sel })));
                        st.sel = None;
                    }
                });
            });
            if let (Some(mut to), Some(from)) = (st.renaming.take(), st.sel.clone()) {
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut to).desired_width(160.0));
                    let name = to.trim().to_string();
                    let ok = name != from && free(&states, &name);
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.add_enabled(ok, egui::Button::new(crate::tl!("OK"))).clicked() || (enter && ok) {
                        action = Some(("layerstate.rename", json!({ "from": from, "to": name })));
                        st.sel = Some(name);
                    } else if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        st.renaming = Some(to);
                    }
                });
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui.button(crate::tl!("Close")).clicked();
                });
            });
        });
    set_state(ctx, "layerstates_dlg", st);
    if close {
        *open = false;
    }
    action
}

#[cfg(test)]
mod tests {
    #[test]
    fn new_never_takes_a_saved_states_name() {
        let states = vec![("Plan".to_string(), 3)];
        assert!(super::free(&states, "Section"));
        assert!(!super::free(&states, "Plan"));
        assert!(!super::free(&states, "PLAN"));
        assert!(!super::free(&states, ""));
    }
}
