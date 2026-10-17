//! Write Block (WBLOCK, W): a block, the entire drawing or objects written to a drawing file, with
//! a base point and the destination file. "Select objects <" and "Pick point <" hide the dialog
//! while SELECT or ID runs and bring it back ([`resume`]). OK runs `wblock`; -WBLOCK stays the
//! command-line form.

use egui::RichText;
use serde_json::{Value, json};

use super::{Action, OPEN_GEN, no_drawing, num_field, set_temp, temp};
use crate::CadApp;
use crate::theme::Tokens;
use std::sync::atomic::Ordering;

const KEY: &str = "wblock_dlg";

/// The dialog's settings until OK, kept while it is hidden for a pick.
#[derive(Clone, Debug, PartialEq)]
struct DlgState {
    /// 0 a block, 1 the entire drawing, 2 objects.
    source: u8,
    block: String,
    base: [f64; 2],
    path: String,
    /// What the hidden dialog waits for: 1 objects (SELECT), 2 the base point (ID).
    picking: u8,
    /// The last point before "Pick point <", to tell a picked point from a cancelled pick.
    last: [f64; 2],
}

/// Every frame: once the SELECT or ID started by the dialog ends, show the dialog again; a picked
/// base point fills the base point.
pub(super) fn resume(app: &mut CadApp, ctx: &egui::Context) {
    let Some((generation, mut st)) = temp::<(u32, DlgState)>(ctx, KEY) else { return };
    if st.picking == 0 || app.session.running.is_some() {
        return;
    }
    let p = app.session.last_point;
    if st.picking == 2 && [p.x, p.y] != st.last {
        st.base = [p.x, p.y];
    }
    st.picking = 0;
    set_temp(ctx, KEY, (generation, st));
    app.ui.dialog = Some("wblock".into());
}

/// The `wblock` parameters, or why there are none.
fn params(st: &DlgState, d: &cadcraft_doc::Drawing, selection: &[String]) -> Result<Value, &'static str> {
    match st.source {
        0 if d.block(&st.block).is_none() => Err("Choose a block."),
        0 => Ok(json!({ "path": st.path, "name": st.block })),
        1 => {
            let base = d.header.point("INSBASE").map_or([0.0, 0.0], |p| [p.x, p.y]);
            let all: Vec<String> = d.model.handles().iter().map(|h| h.hex()).collect();
            Ok(json!({ "path": st.path, "handles": all, "base": base }))
        }
        _ if selection.is_empty() => Err("Select objects first."),
        _ => Ok(json!({ "path": st.path, "handles": selection, "base": st.base })),
    }
}

pub(super) fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) -> Action {
    let t = Tokens::get();
    let Ok(d) = app.session.doc() else {
        no_drawing(ctx, "Write Block", open);
        return None;
    };
    let mut blocks: Vec<String> = d.blocks.iter().filter(|(n, b)| !b.anonymous && !n.starts_with('*')).map(|(n, _)| n.clone()).collect();
    blocks.sort();
    let selection: Vec<String> = app.session.selection().iter().map(|h| h.hex()).collect();
    let generation = OPEN_GEN.load(Ordering::Relaxed);
    let mut st = temp::<(u32, DlgState)>(ctx, KEY).filter(|(g, _)| *g == generation).map(|(_, s)| s).unwrap_or_else(|| DlgState {
        // Objects selected before WBLOCK are what it writes.
        source: if selection.is_empty() { 1 } else { 2 },
        block: blocks.first().cloned().unwrap_or_default(),
        base: [0.0, 0.0],
        path: String::new(),
        picking: 0,
        last: [0.0, 0.0],
    });
    let (mut ok, mut cancel, mut browse) = (false, false, false);
    let mut pick: u8 = 0;
    let suggested = app.ui.save_format.suggested_name(Some(if st.source == 0 && !st.block.is_empty() { st.block.as_str() } else { "new block" }));
    egui::Window::new(crate::tl!("Write Block")).id(egui::Id::new("Write Block")).open(open).collapsible(false).resizable(false).show(ctx, |ui| {
        ui.label(RichText::new(crate::tl!("Source")).strong());
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!blocks.is_empty(), |ui| ui.radio_value(&mut st.source, 0, crate::tl!("Block")));
            ui.radio_value(&mut st.source, 1, crate::tl!("Entire drawing"));
            ui.radio_value(&mut st.source, 2, crate::tl!("Objects"));
        });
        if st.source == 0 {
            egui::ComboBox::from_id_salt("wblock_block").selected_text(st.block.as_str()).width(220.0).show_ui(ui, |ui| {
                for b in &blocks {
                    ui.selectable_value(&mut st.block, b.clone(), b);
                }
            });
        }
        if st.source == 2 {
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(crate::tl!("Base point")).strong());
                    if ui.button(crate::tl!("Pick point <")).clicked() {
                        pick = 2;
                    }
                    egui::Grid::new("wblock_base").num_columns(2).show(ui, |ui| {
                        for (i, axis) in ["X", "Y"].iter().enumerate() {
                            ui.label(*axis);
                            if let Some(slot) = st.base.get_mut(i)
                                && let Some(v) = num_field(ui, ("wblock_base", i), *slot)
                            {
                                *slot = v;
                            }
                            ui.end_row();
                        }
                    });
                });
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(crate::tl!("Objects")).strong());
                    if ui.button(crate::tl!("Select objects <")).clicked() {
                        pick = 1;
                    }
                    let n = selection.len();
                    let text = if n == 0 {
                        crate::tl!("No objects selected").to_string()
                    } else {
                        crate::i18n::count(n as u64, "{n} object selected", "{n} objects selected")
                    };
                    ui.label(RichText::new(text).color(if n == 0 { t.warn } else { t.text_dim }));
                });
            });
        }
        ui.separator();
        ui.label(RichText::new(crate::tl!("Destination")).strong());
        ui.horizontal(|ui| {
            ui.label(crate::tl!("File name and path:"));
            ui.add(egui::TextEdit::singleline(&mut st.path).hint_text(suggested.as_str()).desired_width(260.0));
            if app.services.pick_save.is_some() {
                browse = ui.button("...").on_hover_text(crate::tl!("Choose the file")).clicked();
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                cancel = ui.button(crate::tl!("Cancel")).clicked();
                ok = ui.button(crate::tl!("OK")).clicked();
            });
        });
    });
    if cancel {
        *open = false;
    }
    if browse && let Some(path) = app.services.pick_save.as_ref().and_then(|f| f(&suggested)) {
        st.path = path;
    }
    if pick != 0 {
        st.picking = pick;
        let p = app.session.last_point;
        st.last = [p.x, p.y];
        set_temp(ctx, KEY, (generation, st));
        *open = false;
        app.start(if pick == 1 { "select" } else { "id" });
        return None;
    }
    if ok && st.path.trim().is_empty() {
        // No file named yet: ask for one where the host can.
        match app.services.pick_save.as_ref().and_then(|f| f(&suggested)) {
            Some(path) => st.path = path,
            None => ok = false,
        }
    }
    if ok {
        match app.session.doc().map_err(|_| "Open a drawing first.").and_then(|d| params(&st, d, &selection)) {
            // `run` echoes an error; the dialog stays open to fix it.
            Ok(p) => {
                if let Ok(r) = app.run("wblock", p) {
                    let n = r.get("entities").and_then(Value::as_u64).unwrap_or(0);
                    app.session.echo(format!("Wrote {n} object(s) to {}", st.path.trim()));
                    *open = false;
                }
            }
            Err(why) => app.session.echo(why.to_string()),
        }
    }
    set_temp(ctx, KEY, (generation, st));
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(source: u8) -> DlgState {
        DlgState { source, block: "B".into(), base: [1.0, 2.0], path: "out.dxf".into(), picking: 0, last: [0.0, 0.0] }
    }

    #[test]
    fn picks_hide_the_dialog_and_bring_it_back() {
        let ctx = egui::Context::default();
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.run("line", json!({ "points": [[0, 0], [10, 0]] })).unwrap();
        let generation = OPEN_GEN.load(Ordering::Relaxed);
        // Pick point <: ID, then the base point.
        set_temp(&ctx, KEY, (generation, DlgState { picking: 2, ..st(2) }));
        app.start("id");
        resume(&mut app, &ctx);
        assert_eq!(app.ui.dialog, None, "still picking");
        app.session.cmdline("3,4").unwrap();
        resume(&mut app, &ctx);
        assert_eq!(app.ui.dialog.as_deref(), Some("wblock"));
        let (_, s) = temp::<(u32, DlgState)>(&ctx, KEY).unwrap();
        assert_eq!((s.picking, s.base), (0, [3.0, 4.0]));
        // Select objects <: SELECT, and the selection is what Objects writes.
        app.ui.dialog = None;
        set_temp(&ctx, KEY, (generation, DlgState { picking: 1, ..s }));
        app.start("select");
        app.session.cmdline("-1,-1").unwrap();
        app.session.cmdline("11,1").unwrap();
        resume(&mut app, &ctx);
        assert_eq!(app.ui.dialog, None, "still selecting");
        app.session.cmdline("").unwrap();
        resume(&mut app, &ctx);
        assert_eq!(app.ui.dialog.as_deref(), Some("wblock"));
        assert_eq!(app.session.selection().len(), 1);
        let (_, s) = temp::<(u32, DlgState)>(&ctx, KEY).unwrap();
        assert_eq!((s.picking, s.base), (0, [3.0, 4.0]), "a selection keeps the base point");
    }

    #[test]
    fn each_source_names_what_to_write() {
        let mut s = cadcraft_engine::Session::new();
        s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
        let d = s.doc().unwrap();
        assert!(params(&st(0), d, &[]).is_err(), "no block B");
        let all = params(&st(1), d, &[]).unwrap();
        assert_eq!(all["handles"].as_array().map(Vec::len), Some(1));
        assert!(params(&st(2), d, &[]).is_err(), "objects need a selection");
        let p = params(&st(2), d, &["1F".into()]).unwrap();
        assert_eq!((p["handles"].clone(), p["base"].clone()), (json!(["1F"]), json!([1.0, 2.0])));
    }
}
