//! Multiline Style (MLSTYLE; Format ▸ Multiline Style...): the styles in a list with Set Current,
//! New, Rename and Delete, a preview, and the selected style's description, elements (offset,
//! colour, linetype), caps, fill and joints. Every change runs `mlstyle`.

use cadcraft_color::Color;
use cadcraft_doc::{MLineCaps, MLineElement, MLineStyle};
use egui::{RichText, Sense, Stroke, pos2, vec2};
use serde_json::{Value, json};

use super::{Action, no_drawing, num_field, set_state, state, text_field};
use crate::CadApp;
use crate::theme::Tokens;

#[derive(Clone, Default)]
struct DlgState {
    sel: String,
    /// The selected element (index into the style's elements).
    element: usize,
    new_name: String,
    renaming: Option<String>,
}

fn caps_json(c: &MLineCaps) -> Value {
    json!({ "line": c.line, "outerArc": c.outer_arc, "innerArcs": c.inner_arcs, "angle": c.angle.to_degrees() })
}

/// The `mlstyle` parameters that make `name` this style (not current).
fn style_params(name: &str, s: &MLineStyle) -> Value {
    let elements: Vec<Value> = s.elements.iter().map(|e| json!({ "offset": e.offset, "color": e.color.name(), "linetype": e.linetype })).collect();
    json!({
        "name": name,
        "description": s.description,
        "elements": elements,
        "fill": s.fill.map_or(json!(false), |c| json!(c.name())),
        "joints": s.joints,
        "startCaps": caps_json(&s.start_caps),
        "endCaps": caps_json(&s.end_caps),
        "current": false,
    })
}

/// The style's elements as lines across the preview, with line caps and fill.
fn preview(ui: &mut egui::Ui, s: &MLineStyle, size: egui::Vec2) {
    let t = Tokens::get();
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, t.canvas);
    p.rect_stroke(rect, 2.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let offsets: Vec<f64> = s.elements.iter().map(|e| e.offset).filter(|o| o.is_finite()).collect();
    let (lo, hi) = offsets.iter().fold((0.0f64, 0.0f64), |(lo, hi), o| (lo.min(*o), hi.max(*o)));
    let k = if hi - lo > 1e-12 { f64::from(rect.height() - 30.0) / (hi - lo) } else { 0.0 };
    let y = |o: f64| rect.center().y - ((o - (lo + hi) / 2.0) * k) as f32;
    let (x0, x1) = (rect.left() + 30.0, rect.right() - 30.0);
    if let Some(fill) = s.fill {
        let r = egui::Rect::from_x_y_ranges(x0..=x1, y(hi)..=y(lo));
        p.rect_filled(r, 0.0, super::dimstyle::shown(fill, &t).gamma_multiply(0.35));
    }
    for e in s.elements.iter().filter(|e| e.offset.is_finite()) {
        p.line_segment([pos2(x0, y(e.offset)), pos2(x1, y(e.offset))], Stroke::new(1.5, super::dimstyle::shown(e.color, &t)));
    }
    for (x, caps) in [(x0, &s.start_caps), (x1, &s.end_caps)] {
        if caps.line {
            p.line_segment([pos2(x, y(hi)), pos2(x, y(lo))], Stroke::new(1.5, t.text));
        }
    }
}

pub(super) fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) -> Action {
    let t = Tokens::get();
    let Ok(d) = app.session.doc() else {
        no_drawing(ctx, "Multiline Style", open);
        return None;
    };
    let current = d.header.str("CMLSTYLE", cadcraft_doc::STANDARD_MLINE_STYLE);
    let mut styles = d.mline_styles.clone();
    if styles.is_empty() {
        styles.push(MLineStyle::default());
    }
    let mut linetypes = vec!["ByLayer".to_string(), "ByBlock".to_string()];
    linetypes.extend(d.linetypes.iter().map(|l| l.name.clone()).filter(|n| !n.eq_ignore_ascii_case("ByLayer") && !n.eq_ignore_ascii_case("ByBlock")));
    let mut st = state(ctx, "mlstyle_dlg", || DlgState { sel: current.clone(), ..Default::default() });
    if !styles.iter().any(|s| s.name == st.sel) {
        st.sel = styles.iter().find(|s| s.name.eq_ignore_ascii_case(&current)).map_or_else(|| current.clone(), |s| s.name.clone());
    }
    let mut action: Action = None;
    let mut close = false;
    egui::Window::new(crate::tl!("Multiline Style"))
        .id(egui::Id::new("Multiline Style"))
        .open(open)
        .default_size(vec2(700.0, 460.0))
        .resizable(true)
        .show(ctx, |ui| {
            ui.label(RichText::new(crate::tf!("Current multiline style: {name}", name = current)).color(t.text_dim));
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(200.0);
                    ui.label(RichText::new(crate::tl!("Styles")).strong());
                    egui::ScrollArea::vertical().id_salt("mlstyle_list").max_height(200.0).auto_shrink([false, true]).show(ui, |ui| {
                        for s in &styles {
                            let mut label = RichText::new(&s.name);
                            if s.name.eq_ignore_ascii_case(&current) {
                                label = label.strong();
                            }
                            let r = ui.selectable_label(s.name == st.sel, label);
                            if r.clicked() && s.name != st.sel {
                                st.sel = s.name.clone();
                                st.element = 0;
                                st.renaming = None;
                            }
                            if r.double_clicked() {
                                action = Some(("mlstyle", json!({ "name": s.name, "current": true })));
                            }
                        }
                    });
                    let is_current = st.sel.eq_ignore_ascii_case(&current);
                    let standard = st.sel.eq_ignore_ascii_case(cadcraft_doc::STANDARD_MLINE_STYLE);
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!is_current, egui::Button::new(crate::tl!("Set Current"))).clicked() {
                            action = Some(("mlstyle", json!({ "name": st.sel, "current": true })));
                        }
                        if ui.add_enabled(!standard, egui::Button::new(crate::tl!("Rename"))).clicked() {
                            st.renaming = Some(st.sel.clone());
                        }
                        if ui.add_enabled(!standard && !is_current, egui::Button::new(crate::tl!("Delete"))).clicked() {
                            action = Some(("mlstyle", json!({ "delete": st.sel })));
                        }
                    });
                    if let Some(mut to) = st.renaming.take() {
                        ui.horizontal(|ui| {
                            let r = ui.add(egui::TextEdit::singleline(&mut to).desired_width(130.0));
                            let name = to.trim().to_string();
                            let ok = !name.is_empty() && !styles.iter().any(|s| s.name.eq_ignore_ascii_case(&name) && s.name != st.sel);
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if ui.add_enabled(ok, egui::Button::new(crate::tl!("OK"))).clicked() || (enter && ok) {
                                action = Some(("mlstyle", json!({ "name": st.sel, "rename": name })));
                                st.sel = name;
                            } else if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                st.renaming = Some(to);
                            }
                        });
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut st.new_name).hint_text(crate::tl!("New style name")).desired_width(120.0));
                        let name = st.new_name.trim().to_string();
                        let ok = !name.is_empty() && !styles.iter().any(|s| s.name.eq_ignore_ascii_case(&name));
                        if ui
                            .add_enabled(ok, egui::Button::new(crate::tl!("New")))
                            .on_hover_text(crate::tl!("A copy of the selected style"))
                            .clicked()
                            && let Some(base) = styles.iter().find(|s| s.name == st.sel)
                        {
                            action = Some(("mlstyle", style_params(&name, base)));
                            st.sel = name;
                            st.new_name.clear();
                        }
                    });
                    ui.add_space(8.0);
                    if ui.button(crate::tl!("Close")).clicked() {
                        close = true;
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    let Some(style) = styles.iter().find(|s| s.name == st.sel) else { return };
                    let mut s = style.clone();
                    ui.label(RichText::new(crate::tf!("Preview of: {name}", name = s.name)).strong());
                    preview(ui, &s, vec2(ui.available_width().clamp(240.0, 440.0), 90.0));
                    let mut changed = false;
                    egui::Grid::new(("mlstyle_desc", &s.name)).num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                        ui.label(crate::tl!("Description"));
                        if let Some(v) = text_field(ui, egui::Id::new(("mlstyle_desc", &s.name)), &s.description) {
                            s.description = v;
                            changed = true;
                        }
                        ui.end_row();
                    });
                    ui.separator();
                    ui.label(RichText::new(crate::tl!("Elements")).strong());
                    st.element = st.element.min(s.elements.len().saturating_sub(1));
                    egui::Grid::new(("mlstyle_elements", &s.name)).num_columns(4).striped(true).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
                        for h in ["", crate::tl!("Offset"), crate::tl!("Color"), crate::tl!("Linetype")] {
                            ui.label(RichText::new(h).strong());
                        }
                        ui.end_row();
                        for (i, e) in s.elements.iter_mut().enumerate() {
                            if ui.radio(st.element == i, "").clicked() {
                                st.element = i;
                            }
                            if let Some(v) = num_field(ui, ("mlstyle_offset", st.sel.as_str(), i), e.offset) {
                                e.offset = v;
                                changed = true;
                            }
                            if let Some(c) =
                                crate::layers::color_button(ui, egui::Id::new(("mlstyle_color", st.sel.as_str(), i)), e.color, true, 120.0)
                            {
                                e.color = c;
                                changed = true;
                            }
                            egui::ComboBox::from_id_salt(("mlstyle_lt", st.sel.as_str(), i)).selected_text(e.linetype.as_str()).width(110.0).show_ui(
                                ui,
                                |ui| {
                                    for l in &linetypes {
                                        if ui.selectable_label(l.eq_ignore_ascii_case(&e.linetype), l).clicked()
                                            && !l.eq_ignore_ascii_case(&e.linetype)
                                        {
                                            e.linetype = l.clone();
                                            changed = true;
                                        }
                                    }
                                },
                            );
                            ui.end_row();
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.add_enabled(s.elements.len() < cadcraft_doc::MAX_MLINE_ELEMENTS, egui::Button::new(crate::tl!("Add"))).clicked() {
                            s.elements.push(MLineElement::default());
                            changed = true;
                        }
                        if ui.add_enabled(s.elements.len() > 1, egui::Button::new(crate::tl!("Delete"))).clicked() && st.element < s.elements.len() {
                            s.elements.remove(st.element);
                            changed = true;
                        }
                    });
                    ui.separator();
                    egui::Grid::new(("mlstyle_caps", &s.name)).num_columns(3).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                        ui.label(RichText::new(crate::tl!("Caps")).strong());
                        ui.label(crate::tl!("Start"));
                        ui.label(crate::tl!("End"));
                        ui.end_row();
                        let rows: [(&str, fn(&mut MLineCaps) -> &mut bool); 3] = [
                            (crate::tl!("Line"), |c| &mut c.line),
                            (crate::tl!("Outer arc"), |c| &mut c.outer_arc),
                            (crate::tl!("Inner arcs"), |c| &mut c.inner_arcs),
                        ];
                        for (label, field) in rows {
                            ui.label(label);
                            for caps in [&mut s.start_caps, &mut s.end_caps] {
                                changed |= ui.checkbox(field(caps), "").changed();
                            }
                            ui.end_row();
                        }
                        ui.label(crate::tl!("Angle"));
                        for (end, caps) in [("start", &mut s.start_caps), ("end", &mut s.end_caps)] {
                            if let Some(v) =
                                num_field(ui, ("mlstyle_angle", st.sel.as_str(), end), caps.angle.to_degrees()).filter(|v| (10.0..=170.0).contains(v))
                            {
                                caps.angle = v.to_radians();
                                changed = true;
                            }
                        }
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        let mut fill = s.fill.is_some();
                        if ui.checkbox(&mut fill, crate::tl!("Fill")).changed() {
                            s.fill = fill.then_some(Color::ByLayer);
                            changed = true;
                        }
                        if let Some(c) = s.fill
                            && let Some(c) = crate::layers::color_button(ui, egui::Id::new(("mlstyle_fill", st.sel.as_str())), c, true, 120.0)
                        {
                            s.fill = Some(c);
                            changed = true;
                        }
                        changed |= ui.checkbox(&mut s.joints, crate::tl!("Display joints")).changed();
                    });
                    if changed {
                        action = Some(("mlstyle", style_params(&s.name, &s)));
                    }
                });
            });
        });
    set_state(ctx, "mlstyle_dlg", st);
    if close {
        *open = false;
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_params_rebuild_the_style() {
        let mut s = cadcraft_engine::Session::new();
        let style = MLineStyle {
            name: "WALL".into(),
            description: "Wall".into(),
            elements: vec![
                MLineElement { offset: 1.5, color: Color::Index(1), linetype: "ByLayer".into() },
                MLineElement { offset: -1.5, color: Color::ByBlock, linetype: "ByLayer".into() },
            ],
            start_caps: MLineCaps { line: true, angle: 45f64.to_radians(), ..Default::default() },
            end_caps: MLineCaps { outer_arc: true, ..Default::default() },
            joints: true,
            fill: Some(Color::Index(3)),
        };
        s.execute("mlstyle", &style_params("WALL", &style)).unwrap();
        let d = s.doc().unwrap();
        let got = d.mline_style("WALL").cloned().unwrap();
        assert_eq!(got.elements, style.elements);
        assert_eq!((got.fill, got.joints, got.description.as_str()), (style.fill, true, "Wall"));
        assert!(got.start_caps.line && got.end_caps.outer_arc);
        assert!((got.start_caps.angle - style.start_caps.angle).abs() < 1e-9);
        assert_ne!(d.header.str("CMLSTYLE", ""), "WALL", "edits never make it current");
    }
}
