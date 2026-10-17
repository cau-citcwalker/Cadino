//! Find and Replace (FIND; Edit ▸ Find..., the Find and Replace tool and the shortcut menu): text
//! to find and to replace it with, Match case and Whole words only. Find selects the matches and
//! lists them; Replace All replaces them. Both run `find`.

use egui::{RichText, vec2};
use serde_json::{Value, json};

use super::{no_drawing, set_state, state};
use crate::CadApp;
use crate::theme::Tokens;

#[derive(Clone, Default)]
struct DlgState {
    find: String,
    replace: String,
    case: bool,
    whole: bool,
    /// The last result: (handle, object type, matches) and its message.
    found: Vec<(String, String, u64)>,
    message: String,
}

/// The `find` parameters; with `replace` the matches are replaced.
fn params(st: &DlgState, replace: bool) -> Value {
    let mut p = json!({ "find": st.find, "matchCase": st.case, "wholeWord": st.whole });
    if replace {
        p["replace"] = json!(st.replace);
    }
    p
}

pub(super) fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) -> super::Action {
    let t = Tokens::get();
    if app.session.doc().is_err() {
        no_drawing(ctx, "Find and Replace", open);
        return None;
    }
    let mut st = state(ctx, "find_dlg", DlgState::default);
    let (mut find, mut replace_all, mut done) = (false, false, false);
    let mut pick: Option<String> = None;
    egui::Window::new(crate::tl!("Find and Replace")).id(egui::Id::new("Find and Replace")).open(open).collapsible(false).resizable(false).show(
        ctx,
        |ui| {
            egui::Grid::new("find_grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                ui.label(crate::tl!("Find what:"));
                let r = ui.add(egui::TextEdit::singleline(&mut st.find).desired_width(240.0));
                find |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.end_row();
                ui.label(crate::tl!("Replace with:"));
                ui.add(egui::TextEdit::singleline(&mut st.replace).desired_width(240.0));
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut st.case, crate::tl!("Match case"));
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut st.whole, crate::tl!("Find whole words only"));
                ui.end_row();
            });
            if !st.message.is_empty() {
                ui.separator();
                ui.label(RichText::new(&st.message).color(t.text_dim));
                egui::ScrollArea::vertical().id_salt("find_results").max_height(140.0).show(ui, |ui| {
                    for (h, ty, n) in &st.found {
                        if ui.selectable_label(false, format!("{ty}  ({n})")).on_hover_text(crate::tl!("Select this object")).clicked() {
                            pick = Some(h.clone());
                        }
                    }
                });
            }
            ui.separator();
            ui.horizontal(|ui| {
                let ok = !st.find.is_empty();
                find |= ui.add_enabled(ok, egui::Button::new(crate::tl!("Find"))).clicked();
                replace_all = ui.add_enabled(ok, egui::Button::new(crate::tl!("Replace All"))).clicked();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    done = ui.button(crate::tl!("Done")).clicked();
                });
            });
        },
    );
    if (find || replace_all) && !st.find.is_empty() {
        match app.run("find", params(&st, replace_all)) {
            Ok(r) => {
                st.message = r.get("message").and_then(Value::as_str).unwrap_or_default().to_string();
                st.found = if replace_all {
                    Vec::new()
                } else {
                    r.get("matches")
                        .and_then(Value::as_array)
                        .map(|m| {
                            m.iter()
                                .take(1000)
                                .map(|x| {
                                    let s = |k: &str| x.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                                    (s("handle"), s("type"), x.get("count").and_then(Value::as_u64).unwrap_or(0))
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                };
            }
            Err(e) => st.message = e,
        }
    }
    if let Some(h) = pick {
        let _ = app.run("select", json!({ "handles": [h] }));
    }
    set_state(ctx, "find_dlg", st);
    if done {
        *open = false;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_then_replace_all_through_the_command() {
        let mut s = cadcraft_engine::Session::new();
        s.execute("text", &json!({ "at": [0, 0], "height": 1, "text": "Door D1" })).unwrap();
        s.execute("text", &json!({ "at": [0, 5], "height": 1, "text": "door" })).unwrap();
        let mut st = DlgState { find: "Door".into(), replace: "Window".into(), case: true, ..Default::default() };
        let r = s.execute("find", &params(&st, false)).unwrap();
        assert_eq!(r["total"], json!(1), "Match case finds one: {r}");
        st.case = false;
        let r = s.execute("find", &params(&st, true)).unwrap();
        assert_eq!(r["total"], json!(2), "{r}");
    }
}
