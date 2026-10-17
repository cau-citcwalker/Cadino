//! Select Color (COLOR, COL, COLOUR; Format ▸ Color...) and Lineweight Settings (LWEIGHT, LW,
//! LINEWEIGHT; Format ▸ Lineweight...): the colour and lineweight of new objects. OK runs `color`,
//! `lweight`, `lwdisplay` and `setvar LWDEFAULT`; Cancel changes nothing.

use cadcraft_color::{Color, Rgb};
use cadcraft_doc::Lineweight;
use egui::{RichText, Sense, Stroke, vec2};
use serde_json::{Value, json};

use super::{Action, no_drawing, set_state, state};
use crate::CadApp;
use crate::theme::Tokens;

/// The colour of new objects (CECOLOR, and CECOLOR_RGB for a true colour).
pub(super) fn current_color(d: &cadcraft_doc::Drawing) -> Color {
    if d.header.vars.contains_key("CECOLOR_RGB") {
        Color::True(Rgb::from_u32(u32::try_from(d.header.i64("CECOLOR_RGB", 0)).unwrap_or(0)))
    } else {
        Color::from_aci(i16::try_from(d.header.i64("CECOLOR", 256)).unwrap_or(256))
    }
}

/// OK and Cancel at the bottom right: (ok, cancel).
fn ok_cancel(ui: &mut egui::Ui) -> (bool, bool) {
    let (mut ok, mut cancel) = (false, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        cancel = ui.button(crate::tl!("Cancel")).clicked();
        ok = ui.button(crate::tl!("OK")).clicked();
    });
    (ok, cancel)
}

pub(super) fn color(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) -> Action {
    let Ok(d) = app.session.doc() else {
        no_drawing(ctx, "Select Color", open);
        return None;
    };
    let mut picked = state(ctx, "color_dlg", || current_color(d));
    let (mut ok, mut cancel) = (false, false);
    egui::Window::new(crate::tl!("Select Color")).id(egui::Id::new("Select Color")).open(open).collapsible(false).resizable(false).show(ctx, |ui| {
        if let Some(c) = crate::layers::color_palette(ui, egui::Id::new("color_dlg_palette"), picked, true) {
            picked = c;
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(crate::tl!("Color:"));
            let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
            crate::layers::swatch(ui.painter(), r, picked);
            ui.label(crate::i18n::t(&picked.name()));
            (ok, cancel) = ok_cancel(ui);
        });
    });
    set_state(ctx, "color_dlg", picked);
    if cancel {
        *open = false;
    }
    if ok {
        *open = false;
        return Some(("color", json!({ "color": picked.name() })));
    }
    None
}

/// The Lineweight Settings dialog's choices until OK.
#[derive(Clone)]
struct Lw {
    /// CELWEIGHT: hundredths of a mm, or -1 ByLayer, -2 ByBlock, -3 Default.
    current: i64,
    inches: bool,
    display: bool,
    /// LWDEFAULT in hundredths of a mm.
    default: i64,
}

/// A lineweight in the dialog's units.
fn lw_label(code: i64, inches: bool) -> String {
    match code {
        -1 => crate::tl!("ByLayer").into(),
        -2 => crate::tl!("ByBlock").into(),
        -3 => crate::tl!("Default").into(),
        v if inches => format!("{:.3} in", v as f64 / 2540.0),
        v => format!("{:.2} mm", v as f64 / 100.0),
    }
}

/// The `lweight` parameter for a CELWEIGHT code.
fn lw_param(code: i64) -> Value {
    match code {
        -1 => json!("ByLayer"),
        -2 => json!("ByBlock"),
        -3 => json!("Default"),
        v => json!(v as f64 / 100.0),
    }
}

pub(super) fn lweight(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) -> Action {
    let t = Tokens::get();
    let Ok(d) = app.session.doc() else {
        no_drawing(ctx, "Lineweight Settings", open);
        return None;
    };
    let saved = Lw {
        current: d.header.i64("CELWEIGHT", -1),
        // LWUNITS.
        inches: !app.session.settings.lwunits_mm,
        display: app.session.settings.lwdisplay,
        default: cadcraft_engine::sysvars::get(&app.session, "LWDEFAULT").and_then(|v| v.as_i64()).unwrap_or(25),
    };
    let mut st = state(ctx, "lweight_dlg", || saved.clone());
    let (mut ok, mut cancel) = (false, false);
    let codes: Vec<i64> = [-1, -2, -3].into_iter().chain(Lineweight::STANDARD.iter().map(|v| i64::from(*v))).collect();
    egui::Window::new(crate::tl!("Lineweight Settings"))
        .id(egui::Id::new("Lineweight Settings"))
        .open(open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(crate::tl!("Lineweights")).strong());
                    egui::ScrollArea::vertical().id_salt("lweight_list").max_height(220.0).show(ui, |ui| {
                        ui.set_width(170.0);
                        for code in &codes {
                            ui.horizontal(|ui| {
                                let (r, _) = ui.allocate_exact_size(vec2(40.0, 14.0), Sense::hover());
                                let w = if *code > 0 { (*code as f32 / 100.0 * 3.0).clamp(1.0, 7.0) } else { 1.0 };
                                ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(w, t.text));
                                if ui.selectable_label(st.current == *code, lw_label(*code, st.inches)).clicked() {
                                    st.current = *code;
                                }
                            });
                        }
                    });
                });
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(crate::tl!("Units for Listing")).strong());
                    ui.radio_value(&mut st.inches, false, crate::tl!("Millimeters (mm)"));
                    ui.radio_value(&mut st.inches, true, crate::tl!("Inches (in)"));
                    ui.add_space(8.0);
                    ui.checkbox(&mut st.display, crate::tl!("Display Lineweight"));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(crate::tl!("Default"));
                        egui::ComboBox::from_id_salt("lweight_default").selected_text(lw_label(st.default, st.inches)).show_ui(ui, |ui| {
                            for v in Lineweight::STANDARD {
                                let v = i64::from(v);
                                ui.selectable_value(&mut st.default, v, lw_label(v, st.inches));
                            }
                        });
                    });
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(crate::tf!("Current Lineweight: {lineweight}", lineweight = lw_label(st.current, st.inches))).color(t.text_dim),
                );
                (ok, cancel) = ok_cancel(ui);
            });
        });
    set_state(ctx, "lweight_dlg", st.clone());
    if cancel {
        *open = false;
    }
    if !ok {
        return None;
    }
    *open = false;
    if st.display != saved.display {
        let _ = app.run("lwdisplay", json!({ "on": st.display }));
    }
    if st.default != saved.default {
        let _ = app.run("setvar", json!({ "name": "LWDEFAULT", "value": st.default }));
    }
    if st.inches != saved.inches {
        let _ = app.run("setvar", json!({ "name": "LWUNITS", "value": i32::from(!st.inches) }));
    }
    Some(("lweight", json!({ "lineweight": lw_param(st.current) })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lineweight_codes_round_trip_through_lweight() {
        let mut s = cadcraft_engine::Session::new();
        for code in [-1, -2, -3, 0, 5, 25, 211] {
            s.execute("lweight", &json!({ "lineweight": lw_param(code) })).unwrap();
            assert_eq!(s.doc().unwrap().header.i64("CELWEIGHT", 99), code);
        }
        assert_eq!(lw_label(25, false), "0.25 mm");
        assert_eq!(lw_label(254, true), "0.100 in");
    }

    /// The units for listing are LWUNITS: the dialog opens in them.
    #[test]
    fn lineweight_units_follow_lwunits() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.cmdline("lwunits 0");
        let ctx = egui::Context::default();
        let mut open = true;
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let _ = lweight(&mut app, ui.ctx(), &mut open);
        });
        out.textures_delta.clear();
        let st = crate::managers::temp::<(u32, Lw)>(&ctx, "lweight_dlg").map(|(_, st)| st);
        assert!(st.is_some_and(|st| st.inches));
    }

    #[test]
    fn color_names_round_trip_through_color() {
        let mut s = cadcraft_engine::Session::new();
        for c in [Color::ByLayer, Color::ByBlock, Color::Index(1), Color::Index(34), Color::True(Rgb(10, 200, 30))] {
            s.execute("color", &json!({ "color": c.name() })).unwrap();
            assert_eq!(current_color(s.doc().unwrap()), c, "{}", c.name());
        }
    }
}
