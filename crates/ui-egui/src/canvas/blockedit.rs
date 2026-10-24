//! The Block Editor on the canvas (BEDIT): a tint over the drawing area so the editor can't be
//! mistaken for the drawing, the block's base point marked at the origin, the dynamic block's
//! parameters (with their labels and grips) and actions (tags with their names), and a bar with
//! the block's name, Parameter, Action, the visibility state shown, Save Block, Save Block As and
//! Close Block Editor.

use cadcraft_engine::doc::dynblock::ParameterKind;
use cadcraft_engine::geom::{Vec2, Vec3};
use egui::{Pos2, Rect, RichText, Shape, Stroke, vec2};
use serde_json::json;

use super::Xf;
use crate::CadApp;
use crate::theme::Tokens;

fn id() -> egui::Id {
    egui::Id::new("cadcraft-block-editor-bar")
}

/// The block the Block Editor is editing in the drawing shown, and whether it has unsaved changes.
fn editing(app: &CadApp) -> Option<(String, bool)> {
    let st = app.session.state().ok()?;
    st.block_editor().map(|n| (n.to_string(), st.block_unsaved()))
}

/// Tint the drawing area while the Block Editor is open (under the drawing).
pub(super) fn wash(app: &CadApp, p: &egui::Painter, rect: Rect) {
    if editing(app).is_some() {
        p.rect_filled(rect, 0.0, Tokens::get().accent.gamma_multiply(0.12));
    }
}

/// The base point marker and the Block Editor bar (over the drawing).
pub(super) fn paint(app: &mut CadApp, ctx: &egui::Context, p: &egui::Painter, xf: &Xf, rect: Rect) {
    let Some((name, unsaved)) = editing(app) else { return };
    let t = Tokens::get();
    // The base point: the origin of the editor's coordinates.
    let o = xf.to_screen(Vec2::ZERO);
    if rect.expand(16.0).contains(o) {
        let st = Stroke::new(1.5, t.accent);
        p.circle_stroke(o, 6.0, st);
        p.line_segment([o - vec2(12.0, 0.0), o + vec2(12.0, 0.0)], st);
        p.line_segment([o - vec2(0.0, 12.0), o + vec2(0.0, 12.0)], st);
    }
    let testing = app.session.state().is_ok_and(|st| st.block_testing());
    if testing {
        test_bar(app, ctx, rect);
        return;
    }
    authoring(app, p, xf, rect);
    let states = crate::bedit::visibility_states(app);
    let mut action: Option<(&str, bool)> = None;
    let mut show_state: Option<String> = None;
    let mut open_states = false;
    egui::Area::new(id()).order(egui::Order::Foreground).pivot(egui::Align2::CENTER_TOP).fixed_pos(rect.center_top() + vec2(0.0, 8.0)).show(
        ctx,
        |ui| {
            egui::Frame::new().fill(t.cmd_bg).stroke(Stroke::new(1.0, t.accent)).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::tl!("Block Editor")).color(t.accent).strong());
                    ui.label(RichText::new(&name).color(t.text));
                    if unsaved {
                        ui.label(RichText::new("●").color(t.warn)).on_hover_text(crate::tl!("Unsaved changes"));
                    }
                    ui.separator();
                    if ui.button(crate::tl!("Parameter")).on_hover_text("BPARAMETER").clicked() {
                        action = Some(("bparameter", true));
                    }
                    if ui.button(crate::tl!("Action")).on_hover_text("BACTION").clicked() {
                        action = Some(("baction", true));
                    }
                    if let Some((names, current)) = &states {
                        let shown = current.clone().unwrap_or_default();
                        egui::ComboBox::from_id_salt("bedit-visibility-state").selected_text(shown.clone()).show_ui(ui, |ui| {
                            for n in names {
                                if ui.selectable_label(*n == shown, n.as_str()).clicked() {
                                    show_state = Some(n.clone());
                                }
                            }
                        });
                        if ui.button(crate::tl!("Visibility States")).on_hover_text("BVSTATE").clicked() {
                            open_states = true;
                        }
                        if ui.button(crate::tl!("Make Visible")).on_hover_text("BVSHOW").clicked() {
                            action = Some(("bvshow", true));
                        }
                        if ui.button(crate::tl!("Make Invisible")).on_hover_text("BVHIDE").clicked() {
                            action = Some(("bvhide", true));
                        }
                    }
                    if ui.button(crate::tl!("Test Block")).on_hover_text("BTESTBLOCK").clicked() {
                        action = Some(("btestblock", false));
                    }
                    ui.separator();
                    if ui.add_enabled(unsaved, egui::Button::new(crate::tl!("Save Block"))).clicked() {
                        action = Some(("bsave", false));
                    }
                    // The drawing itself has no block to save under another name.
                    let current = name == cadcraft_engine::blockedit::CURRENT_DRAWING;
                    if ui.add_enabled(!current, egui::Button::new(crate::tl!("Save Block As"))).clicked() {
                        action = Some(("bsaveas", true));
                    }
                    if ui.button(crate::tl!("Close Block Editor")).clicked() {
                        action = Some(("bclose", true));
                    }
                });
            });
        },
    );
    if let Some(n) = show_state
        && let Err(e) = app.run("bvstate", json!({ "operation": "set", "name": n }))
    {
        app.session.echo(e);
    }
    if open_states {
        crate::bedit::open_visibility_states(app);
    }
    crate::bedit::visibility_states_dialog(app, ctx);
    match action {
        // Prompting commands start like a menu item; BSAVE needs no answer.
        Some((id, true)) => app.start(id),
        Some((id, false)) => {
            let _ = app.run(id, json!({}));
        }
        None => {}
    }
}

/// The test block window's bar (BTESTBLOCK): its name and Close Test Block Window.
fn test_bar(app: &mut CadApp, ctx: &egui::Context, rect: Rect) {
    let t = Tokens::get();
    let mut close = false;
    egui::Area::new(id()).order(egui::Order::Foreground).pivot(egui::Align2::CENTER_TOP).fixed_pos(rect.center_top() + vec2(0.0, 8.0)).show(
        ctx,
        |ui| {
            egui::Frame::new().fill(t.cmd_bg).stroke(Stroke::new(1.0, t.warn)).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::tl!("Test Block Window")).color(t.warn).strong());
                    ui.separator();
                    if ui.button(crate::tl!("Close Test Block Window")).clicked() {
                        close = true;
                    }
                });
            });
        },
    );
    if close && let Err(e) = app.run("bclose", json!({})) {
        app.session.echo(e);
    }
}

/// The dynamic block's parameters and actions, as the Block Editor shows them: a parameter as a
/// line between its points (dashed for a reflection line) with its label and its grips, an
/// action as a tag with a lightning mark and its name.
fn authoring(app: &mut CadApp, p: &egui::Painter, xf: &Xf, rect: Rect) {
    let Some((m, base)) = crate::bedit::authored(app) else { return };
    let t = Tokens::get();
    let (ink, tag) = (t.grip_dyn, t.warn);
    let gs = app.session.settings.gripsize as f32;
    let at = |q: Vec3| xf.to_screen(q.xy() - base);
    let st = Stroke::new(1.2, ink);
    let label = |pos: Pos2, text: &str| {
        if rect.expand(40.0).contains(pos) {
            p.text(pos, egui::Align2::CENTER_BOTTOM, text, crate::theme::small(), ink);
        }
    };
    let square = |c: Pos2| {
        p.add(Shape::convex_polygon(vec![c + vec2(-gs, -gs), c + vec2(gs, -gs), c + vec2(gs, gs), c + vec2(-gs, gs)], ink, Stroke::NONE));
    };
    let arrow = |c: Pos2, toward: egui::Vec2| {
        let u = if toward.length() > 1e-3 { toward / toward.length() } else { vec2(1.0, 0.0) };
        let n = vec2(-u.y, u.x);
        p.add(Shape::convex_polygon(vec![c - u * gs * 0.8 + n * gs, c + u * gs * 1.3, c - u * gs * 0.8 - n * gs], ink, Stroke::NONE));
    };
    for q in m.parameters.iter() {
        let has = |i: usize| q.grips.get(i).is_some_and(|g| *g != 0);
        let any = q.grips.iter().any(|g| *g != 0);
        match &q.kind {
            ParameterKind::Linear { base: a, end: b, label: l, .. } | ParameterKind::Polar { base: a, end: b, label: l, .. } => {
                let (sa, sb) = (at(*a), at(*b));
                let d = sb - sa;
                let n = if d.length() > 1e-3 { vec2(-d.y, d.x) / d.length() } else { vec2(0.0, -1.0) };
                p.line_segment([sa, sb], st);
                p.line_segment([sa - n * 5.0, sa + n * 5.0], st);
                p.line_segment([sb - n * 5.0, sb + n * 5.0], st);
                label(sa + d * 0.5 - n * 4.0, l);
                let polar = matches!(q.kind, ParameterKind::Polar { .. });
                if has(0) {
                    if polar { square(sa) } else { arrow(sa, -d) }
                }
                if has(1) {
                    if polar { square(sb) } else { arrow(sb, d) }
                }
            }
            ParameterKind::Alignment { base: a, end: b, .. } => {
                let (sa, sb) = (at(*a), at(*b));
                p.line_segment([sa, sb], st);
                label(sa - vec2(0.0, gs * 2.0), crate::tl!("Alignment"));
                if any {
                    arrow(sa, sb - sa);
                }
            }
            ParameterKind::Xy { base: a, end: b, label: l, label2: l2, .. } => {
                let corner = at(Vec3::new(b.x, a.y, a.z));
                let (sa, sb) = (at(*a), at(*b));
                p.line_segment([sa, corner], st);
                p.line_segment([corner, sb], st);
                label(sa + (corner - sa) * 0.5 - vec2(0.0, 4.0), l);
                label(corner + (sb - corner) * 0.5 + vec2(gs * 2.0 + 24.0, 0.0), l2);
                if any {
                    square(sb);
                }
            }
            ParameterKind::Rotation { base: a, end: b, label: l, .. } => {
                let (sa, sb) = (at(*a), at(*b));
                p.line_segment([sa, sb], st);
                p.circle_stroke(sa, (sb - sa).length(), Stroke::new(0.6, ink.gamma_multiply(0.6)));
                label(sa + (sb - sa) * 0.5 - vec2(0.0, 4.0), l);
                if any {
                    p.circle_filled(sb, gs * 1.1, ink);
                }
            }
            ParameterKind::Flip { base: a, end: b, label: l, label_at, .. } => {
                let (sa, sb, sl) = (at(*a), at(*b), at(*label_at));
                p.extend(Shape::dashed_line(&[sa, sb], st, 6.0, 4.0));
                label(sl - vec2(0.0, gs * 2.0), l);
                if any {
                    let d = sb - sa;
                    let u = if d.length() > 1e-3 { d / d.length() } else { vec2(1.0, 0.0) };
                    let n = vec2(-u.y, u.x);
                    p.add(Shape::convex_polygon(vec![sl + n * gs * 1.8, sl + u * gs * 0.8, sl - n * gs * 1.8, sl - u * gs * 0.8], ink, Stroke::NONE));
                }
            }
            ParameterKind::Point { at: a, label: l, .. } => {
                let sa = at(*a);
                label(sa - vec2(0.0, gs * 2.0), l);
                if any {
                    square(sa);
                } else {
                    p.circle_stroke(sa, gs, st);
                }
            }
            ParameterKind::Visibility { at: a, label: l, .. } | ParameterKind::Lookup { at: a, label: l, .. } => {
                let sa = at(*a);
                label(sa - vec2(0.0, gs * 2.0), l);
                let w = gs * 1.3;
                p.add(Shape::convex_polygon(vec![sa + vec2(-w, -w * 0.6), sa + vec2(w, -w * 0.6), sa + vec2(0.0, w * 0.9)], ink, Stroke::NONE));
            }
            ParameterKind::BasePoint { at: a } => {
                let sa = at(*a);
                p.circle_stroke(sa, gs * 1.4, st);
                p.line_segment([sa - vec2(gs * 2.0, 0.0), sa + vec2(gs * 2.0, 0.0)], st);
                p.line_segment([sa - vec2(0.0, gs * 2.0), sa + vec2(0.0, gs * 2.0)], st);
            }
            ParameterKind::Other { .. } => {}
        }
    }
    for a in m.actions.iter() {
        let c = at(a.at);
        if !rect.expand(60.0).contains(c) {
            continue;
        }
        let g = p.layout_no_wrap(a.name.clone(), crate::theme::small(), t.text);
        let r = Rect::from_min_size(c, vec2(g.size().x + 20.0, g.size().y + 6.0));
        p.rect_filled(r, 3.0, t.cmd_bg);
        p.rect_stroke(r, 3.0, Stroke::new(1.0, tag), egui::StrokeKind::Inside);
        let b = r.left_center() + vec2(8.0, 0.0);
        let bolt = vec![b + vec2(1.5, -5.0), b + vec2(-2.5, 0.5), b + vec2(1.0, 0.5), b + vec2(-1.5, 5.0)];
        p.add(Shape::line(bolt, Stroke::new(1.5, tag)));
        p.galley(r.left_top() + vec2(16.0, 3.0), g, t.text);
    }
}
