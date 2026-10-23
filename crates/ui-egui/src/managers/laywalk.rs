//! LayerWalk (LAYWALK; Format ▸ Layer Tools ▸ Layer Walk...): the drawing's layers; selecting
//! some shows only those (click, Ctrl+click to add, Shift+click for a range, or a wildcard
//! filter). On close the layers' on and frozen states come back unless "Restore on exit" is
//! cleared, also when another dialog takes its place. It works through `layer.set`, each walk
//! one undo step.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

use egui::{RichText, vec2};
use serde_json::json;

use super::{Action, OPEN_GEN, set_state, state, temp};
use crate::CadApp;
use crate::theme::Tokens;

const KEY: &str = "laywalk_dlg";

#[derive(Clone)]
struct St {
    /// The drawing walked (its uid).
    uid: u64,
    /// Each layer's (name, on, frozen) when the dialog opened.
    original: Vec<(String, bool, bool)>,
    selected: BTreeSet<String>,
    anchor: Option<usize>,
    filter: String,
    restore: bool,
    /// Layers were changed by the walk.
    changed: bool,
}

fn fresh(app: &CadApp) -> St {
    let uid = app.session.state().map(|s| s.uid).unwrap_or_default();
    let original = app.session.doc().map(|d| d.layers.iter().map(|l| (l.name.clone(), l.on, l.frozen)).collect()).unwrap_or_default();
    St { uid, original, selected: BTreeSet::new(), anchor: None, filter: String::new(), restore: true, changed: false }
}

/// Run `f` on the walked drawing (also when another one is active now) as one undo step; nothing
/// when that drawing is closed.
fn on_walked(app: &mut CadApp, uid: u64, f: impl FnOnce(&mut CadApp)) {
    let Some(i) = app.session.docs.iter().position(|d| d.uid == uid) else { return };
    let was = std::mem::replace(&mut app.session.active, i);
    let _ = app.session.execute("undo", &json!({ "mode": "begin" }));
    f(app);
    let _ = app.session.execute("undo", &json!({ "mode": "end" }));
    app.session.active = was.min(app.session.docs.len().saturating_sub(1));
}

/// End a walk the dialog no longer shows (`shown` false: another dialog took its place) or one
/// that LAYWALK opened again replaces: restore it as its Close would.
pub(super) fn end_stale(app: &mut CadApp, ctx: &egui::Context, shown: bool) {
    let Some((generation, st)) = temp::<(u32, St)>(ctx, KEY) else { return };
    if shown && generation == OPEN_GEN.load(Ordering::Relaxed) {
        return;
    }
    ctx.data_mut(|d| d.remove::<(u32, St)>(egui::Id::new(KEY)));
    close(app, &st);
}

/// Wildcard match, any case (`*`, `?`, `#`, comma-separated patterns).
fn matches(pattern: &str, name: &str) -> bool {
    cadcraft_engine::lisp::builtins::wild_match(&name.to_uppercase(), &pattern.trim().to_uppercase())
}

/// Bring each layer to the (on, frozen) `want` gives it, changing only what differs.
fn show(app: &mut CadApp, want: impl Fn(&str) -> Option<(bool, bool)>) {
    let now: Vec<(String, bool, bool)> =
        app.session.doc().map(|d| d.layers.iter().map(|l| (l.name.clone(), l.on, l.frozen)).collect()).unwrap_or_default();
    let current = app.session.doc().map(|d| d.header.str("CLAYER", "0")).unwrap_or_default();
    for (name, on, frozen) in now {
        let Some((w_on, w_frozen)) = want(&name) else { continue };
        // The current layer can't be frozen.
        let w_frozen = w_frozen && !name.eq_ignore_ascii_case(&current);
        if (on, frozen) != (w_on, w_frozen) {
            let _ = app.run("layer.set", json!({ "name": name, "on": w_on, "frozen": w_frozen }));
        }
    }
}

/// Show the selected layers only (thawed), the others off.
fn walk(app: &mut CadApp, st: &mut St) {
    st.changed = true;
    let sel = st.selected.clone();
    let original = st.original.clone();
    on_walked(app, st.uid, |app| {
        show(app, |n| {
            let frozen = original.iter().find(|(o, _, _)| o == n).is_some_and(|(_, _, f)| *f);
            Some(if sel.contains(n) { (true, false) } else { (false, frozen) })
        });
    });
}

/// Closing: put every layer back as it was when the dialog opened (unless cleared).
fn close(app: &mut CadApp, st: &St) {
    if st.changed && st.restore {
        let original = st.original.clone();
        on_walked(app, st.uid, |app| show(app, |n| original.iter().find(|(o, _, _)| o == n).map(|(_, on, f)| (*on, *f))));
    }
}

pub(super) fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) -> Action {
    let t = Tokens::get();
    if app.session.state().is_err() {
        super::no_drawing(ctx, "LayerWalk", open);
        return None;
    }
    // LAYWALK opened again while walking: the earlier walk ends first.
    end_stale(app, ctx, true);
    let mut st = state(ctx, KEY, || fresh(app));
    let names: Vec<String> = st.original.iter().map(|(n, _, _)| n.clone()).collect();
    let mut changed = false;
    let mut done = false;
    let title = crate::tf!("LayerWalk - Layers: {n}", n = names.len());
    egui::Window::new(title).id(egui::Id::new("LayerWalk")).open(open).collapsible(false).default_size(vec2(300.0, 420.0)).resizable(true).show(
        ctx,
        |ui| {
            ui.horizontal(|ui| {
                ui.label(crate::tl!("Filter"));
                let r = ui.add(egui::TextEdit::singleline(&mut st.filter).hint_text("*").desired_width(160.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !st.filter.trim().is_empty() {
                    st.selected = names.iter().filter(|n| matches(&st.filter, n)).cloned().collect();
                    changed = true;
                }
            });
            ui.label(RichText::new(crate::tl!("Select layers to show only them. Ctrl+click adds, Shift+click adds a range.")).color(t.text_dim));
            egui::ScrollArea::vertical().id_salt("laywalk_list").max_height(220.0).auto_shrink([false, false]).show(ui, |ui| {
                for (i, n) in names.iter().enumerate() {
                    let r = ui.selectable_label(st.selected.contains(n), n);
                    if r.clicked() {
                        let m = ui.input(|i| i.modifiers);
                        match (m.shift, st.anchor) {
                            (true, Some(a)) => {
                                let (lo, hi) = (a.min(i), a.max(i));
                                st.selected.extend(names.iter().skip(lo).take(hi - lo + 1).cloned());
                            }
                            _ if m.command || m.ctrl => {
                                if !st.selected.remove(n) {
                                    st.selected.insert(n.clone());
                                }
                                st.anchor = Some(i);
                            }
                            _ => {
                                st.selected = BTreeSet::from([n.clone()]);
                                st.anchor = Some(i);
                            }
                        }
                        changed = true;
                    }
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(crate::tl!("Select All")).clicked() {
                    st.selected = names.iter().cloned().collect();
                    changed = true;
                }
                ui.checkbox(&mut st.restore, crate::tl!("Restore on exit"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    done = ui.button(crate::tl!("Close")).clicked();
                });
            });
        },
    );
    if changed {
        walk(app, &mut st);
    }
    if done {
        *open = false;
    }
    if *open {
        set_state(ctx, KEY, st);
    } else {
        close(app, &st);
        // The next LAYWALK starts from the drawing again.
        ctx.data_mut(|d| d.remove::<(u32, St)>(egui::Id::new(KEY)));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layers(app: &CadApp) -> Vec<(String, bool, bool)> {
        app.session.doc().unwrap().layers.iter().map(|l| (l.name.clone(), l.on, l.frozen)).collect()
    }

    #[test]
    fn walking_shows_only_the_selected_layers_and_restores_them() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        for n in ["A-WALL", "A-DOOR", "E-LITE"] {
            app.run("layer.new", json!({ "name": n })).unwrap();
        }
        app.run("layer.set", json!({ "name": "E-LITE", "frozen": true })).unwrap();
        let before = layers(&app);
        let undo_depth = app.session.state().unwrap().undo.len();
        let mut st = fresh(&app);
        st.selected = ["A-WALL", "E-LITE"].map(String::from).into();
        walk(&mut app, &mut st);
        let now = layers(&app);
        let get = |n: &str| now.iter().find(|l| l.0 == n).map(|l| (l.1, l.2)).unwrap();
        assert_eq!(get("A-WALL"), (true, false));
        assert_eq!(get("E-LITE"), (true, false), "a selected frozen layer is shown");
        assert_eq!(get("A-DOOR"), (false, false));
        assert!(!get("0").0);
        st.selected = names_matching(&st, "A-*");
        walk(&mut app, &mut st);
        assert!(!layers(&app).iter().find(|l| l.0 == "E-LITE").unwrap().1);
        close(&mut app, &st);
        assert_eq!(layers(&app), before);
        // One undo step a walk (and the restore); no undo group is left open.
        assert!(app.session.state().unwrap().undo.len() <= undo_depth + 3);
        assert!(app.session.state().unwrap().undo_ctl.group.is_none());
    }

    #[test]
    fn a_walk_ends_when_another_dialog_takes_its_place_or_its_drawing_is_left() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.run("layer.new", json!({ "name": "A-WALL" })).unwrap();
        let before = layers(&app);
        let ctx = egui::Context::default();
        let mut st = fresh(&app);
        st.selected = ["A-WALL"].map(String::from).into();
        walk(&mut app, &mut st);
        assert!(app.session.state().unwrap().undo_ctl.group.is_none(), "no group held across frames");
        assert_ne!(layers(&app), before);
        set_state(&ctx, KEY, st.clone());
        // Typed LA: the Layer dialog replaces LayerWalk, whose layers come back.
        app.ui.dialog = Some("layers".into());
        end_stale(&mut app, &ctx, false);
        assert_eq!(layers(&app), before);
        assert!(temp::<(u32, St)>(&ctx, KEY).is_none());

        // Walked in one drawing, ended while another is active: the walked one is restored.
        walk(&mut app, &mut st);
        set_state(&ctx, KEY, st);
        app.run("new", json!({})).unwrap();
        assert_eq!(app.session.docs.len(), 2);
        let active = app.session.active;
        end_stale(&mut app, &ctx, false);
        assert_eq!(app.session.active, active);
        app.session.active = 0;
        assert_eq!(layers(&app), before);
    }

    fn names_matching(st: &St, pattern: &str) -> BTreeSet<String> {
        st.original.iter().map(|(n, _, _)| n.clone()).filter(|n| matches(pattern, n)).collect()
    }

    #[test]
    fn wildcards_match_any_case() {
        assert!(matches("a-*", "A-WALL"));
        assert!(matches("*lite,*door", "A-DOOR"));
        assert!(!matches("A-*", "E-LITE"));
    }
}
