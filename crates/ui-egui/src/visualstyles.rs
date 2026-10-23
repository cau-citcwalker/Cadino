//! The Visual Styles Manager (VISUALSTYLES or VSM typed, View ▸ Visual Styles ▸ Visual Styles
//! Manager...): a sample of each visual style with the current one marked. A double-click or
//! Apply to Current Viewport makes the chosen style current through VSCURRENT, so it is the same
//! as typing it.

use egui::{Color32, Pos2, Sense, Shape, Stroke, pos2, vec2};
use serde_json::json;

use crate::CadApp;
use crate::theme::Tokens;

/// The styles: VSCURRENT's name, the label, what it shows.
const STYLES: [(&str, &str, &str); 7] = [
    ("2dwireframe", "2D Wireframe", "Objects drawn with their lines and curves only."),
    ("wireframe", "Wireframe", "Objects drawn with their lines and curves only, in 3D."),
    ("hidden", "Hidden", "Lines and curves; those behind faces are hidden."),
    ("realistic", "Realistic", "Faces shaded, without edges."),
    ("shaded", "Shaded", "Faces shaded smoothly."),
    ("shadedwithedges", "Shaded with Edges", "Faces shaded, with their visible edges."),
    ("conceptual", "Conceptual", "Faces shaded, with their visible edges."),
];

/// The style a look (shaded, with edges) shows as: one of [`STYLES`].
fn current(shaded: bool, edges: bool) -> &'static str {
    match (shaded, edges) {
        (false, false) => "2dwireframe",
        (false, true) => "hidden",
        (true, false) => "shaded",
        (true, true) => "shadedwithedges",
    }
}

/// A small box seen from above and to the side, drawn in style `style` in `r`.
fn sample(p: &egui::Painter, r: egui::Rect, style: &str, ink: Color32, bg: Color32) {
    p.rect_filled(r, 2.0, bg);
    let c = r.center() + vec2(0.0, 4.0);
    let s = r.height() * 0.28;
    // Corners: the top face (back, right, front, left), then the bottom ones below them.
    let top = [c + vec2(0.0, -1.6 * s), c + vec2(1.4 * s, -0.9 * s), c + vec2(0.0, -0.2 * s), c + vec2(-1.4 * s, -0.9 * s)];
    let down = |q: Pos2| q + vec2(0.0, 1.4 * s);
    let faces: [([Pos2; 4], f32); 3] =
        [(top, 1.0), ([top[3], top[2], down(top[2]), down(top[3])], 0.75), ([top[2], top[1], down(top[1]), down(top[2])], 0.55)];
    let edge = Stroke::new(1.0, ink);
    let shaded = matches!(style, "realistic" | "shaded" | "shadedwithedges" | "conceptual");
    let edges = !matches!(style, "realistic" | "shaded");
    let base = if style == "conceptual" { Color32::from_rgb(214, 196, 150) } else { Color32::from_rgb(170, 180, 196) };
    if shaded || style == "hidden" {
        for (pts, light) in faces {
            let k = |v: u8| (f32::from(v) * light) as u8;
            let fill = if shaded { Color32::from_rgb(k(base.r()), k(base.g()), k(base.b())) } else { bg };
            p.add(Shape::convex_polygon(pts.to_vec(), fill, Stroke::NONE));
        }
    }
    if edges {
        for (pts, _) in faces {
            p.add(Shape::closed_line(pts.to_vec(), edge));
        }
    }
    // Wireframes show the hidden corner's edges too.
    if matches!(style, "2dwireframe" | "wireframe") {
        let back = down(top[0]);
        for q in [down(top[1]), down(top[3]), top[0]] {
            p.line_segment([back, q], edge);
        }
    }
}

/// The Visual Styles Manager window.
pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let t = Tokens::get();
    let now = app.session.state().map_or("2dwireframe", |st| current(st.shaded, st.shaded_edges));
    let id = egui::Id::new("visual_styles_chosen");
    let mut chosen = ctx.data_mut(|d| d.get_temp::<usize>(id)).unwrap_or_else(|| STYLES.iter().position(|s| s.0 == now).unwrap_or(0));
    let mut apply = false;
    egui::Window::new(crate::tl!("Visual Styles Manager"))
        .id(egui::Id::new("Visual Styles Manager"))
        .open(open)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.label(crate::tl!("Available Visual Styles in Drawing"));
            egui::Grid::new("visual_styles_grid").spacing(vec2(6.0, 6.0)).show(ui, |ui| {
                for (i, (name, label, _)) in STYLES.iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(vec2(92.0, 82.0), Sense::click());
                    let swatch = egui::Rect::from_min_size(r.min, vec2(92.0, 64.0));
                    sample(ui.painter(), swatch.shrink(2.0), name, t.canvas_ink, t.canvas);
                    let frame = if i == chosen { Stroke::new(2.0, t.accent) } else { Stroke::new(1.0, t.border) };
                    ui.painter().rect_stroke(swatch, 3.0, frame, egui::StrokeKind::Inside);
                    // The current style is marked in the corner, as AutoCAD's manager does.
                    if *name == now {
                        ui.painter().circle_filled(pos2(swatch.right() - 8.0, swatch.top() + 8.0), 4.0, t.accent);
                    }
                    ui.painter().text(
                        pos2(r.center().x, r.bottom() - 8.0),
                        egui::Align2::CENTER_CENTER,
                        crate::i18n::t(label),
                        crate::theme::small(),
                        t.text,
                    );
                    if resp.clicked() {
                        chosen = i;
                    }
                    if resp.double_clicked() {
                        chosen = i;
                        apply = true;
                    }
                    if i % 4 == 3 {
                        ui.end_row();
                    }
                }
            });
            ui.separator();
            if let Some((_, label, about)) = STYLES.get(chosen) {
                ui.strong(crate::i18n::t(label));
                ui.label(crate::i18n::t(about));
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(crate::tl!("Apply to Current Viewport")).clicked() {
                    apply = true;
                }
            });
        });
    if apply && let Some((name, ..)) = STYLES.get(chosen) {
        let _ = app.run("vscurrent", json!({ "style": name }));
    }
    if *open {
        ctx.data_mut(|d| d.insert_temp(id, chosen));
    } else {
        ctx.data_mut(|d| d.remove::<usize>(id));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Typed bare, VISUALSTYLES and VSM open the manager; with parameters VISUALSTYLES is VSCURRENT.
    /// The current style is read back from the look, Hidden included.
    #[test]
    fn the_manager_opens_typed_and_reads_the_look() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.start("vsm");
        assert_eq!(app.ui.dialog.as_deref(), Some("visualstyles"));
        app.ui.dialog = None;
        app.cmdline("VISUALSTYLES");
        assert_eq!(app.ui.dialog.as_deref(), Some("visualstyles"));
        app.run("visualstyles", json!({ "style": "hidden" })).unwrap();
        let st = app.session.state().unwrap();
        assert_eq!(current(st.shaded, st.shaded_edges), "hidden");
    }

    /// NAVVCUBE (the engine's command) turns the ViewCube the UI draws off and on.
    #[test]
    fn navvcube_turns_the_viewcube_off_and_on() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.run("navvcube", json!({ "on": false })).unwrap();
        assert!(!app.ui.show_viewcube);
        app.run("navvcube", json!({ "option": "on" })).unwrap();
        assert!(app.ui.show_viewcube);
    }
}
