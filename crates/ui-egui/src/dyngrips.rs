//! Dynamic block grips on the canvas, as AutoCAD shows them on a selected dynamic block
//! reference: an arrow at a distance's end (drag it to change the distance), a square at a
//! position (drag it to move it) and a down-pointing triangle for a list (click it for the
//! visibility states or lookup values). Dragging runs the engine's dynamic grip edit
//! ([`cadcraft_engine::Session::start_dyn_grip`]); a list choice runs `blocks.reference.set`.
//! Grips sit where they are in the world (on the reference's plane), whatever the construction
//! plane.

use std::sync::Arc;

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, vec2};
use serde_json::json;

use crate::CadApp;
use crate::canvas::Xf;
use crate::theme::Tokens;
use cadcraft_engine::doc::dynblock::{DynGrip, GripShape};
use cadcraft_engine::doc::{Entity, Handle};
use cadcraft_engine::geom::{Vec2, Vec3};

/// Most selected references whose grips are shown.
const MAX_REFERENCES: usize = 200;

/// The dynamic grips of the selection, worked out once per drawing revision and selection.
#[derive(Clone, Default)]
pub struct Cache {
    key: (u64, u64, Vec<Handle>),
    grips: Arc<Vec<(Handle, DynGrip, f64)>>,
}

/// The selected references' dynamic grips, each with its reference's height (cached).
pub(crate) fn of_selection(app: &mut CadApp) -> Arc<Vec<(Handle, DynGrip, f64)>> {
    let sel = app.session.selection();
    let Ok(st) = app.session.state() else { return Arc::default() };
    let key = (st.uid, st.revision, sel.clone());
    if app.canvas.dyn_grips.key != key {
        let grips: Vec<(Handle, DynGrip, f64)> = sel
            .iter()
            .filter(|h| matches!(st.doc.entity(**h).map(|e| &e.kind), Some(cadcraft_engine::doc::EntityKind::Insert(_))))
            .filter(|h| st.doc.entity(**h).is_some_and(|e| !st.doc.layer(&e.common.layer).is_some_and(|l| l.locked)))
            .take(MAX_REFERENCES)
            .flat_map(|h| {
                let e = st.doc.entity(*h);
                cadcraft_engine::doc::dynblock::grips(&st.doc, *h).into_iter().map(move |g| {
                    let z = e.map_or(0.0, |e| height(e, g.at));
                    (*h, g, z)
                })
            })
            .collect();
        app.canvas.dyn_grips = Cache { key, grips: Arc::new(grips) };
    }
    app.canvas.dyn_grips.grips.clone()
}

/// The height of reference `e`'s plane over world plan point `p`: where a grip `dynblock::grips`
/// places at `p` (seen from above) really is. A level reference's elevation.
/// ponytail: a reference standing on end (its plane upright) keeps its grips on its plan line at
/// z = 0; place them from its own coordinates if anyone stands dynamic blocks up.
fn height(e: &Entity, p: Vec2) -> f64 {
    let Some(pl) = e.plane() else { return 0.0 };
    let (o, n) = (pl.apply(Vec3::ZERO), pl.apply_vec(Vec3::Z));
    let z = o.z - (n.x * (p.x - o.x) + n.y * (p.y - o.y)) / n.z;
    if n.z.abs() > 1e-6 * n.len() && z.is_finite() { z } else { 0.0 }
}

/// Where world plan point `p` at height `z` (a grip's, in drawing coordinates) is on screen,
/// through any construction plane.
fn spot(xf: &Xf, p: Vec2, z: f64) -> Pos2 {
    xf.world_to_screen(p.to3(z))
}

/// The screen outline of grip `g` at height `z` (size `gs`, half the side of a square grip).
fn outline(xf: &Xf, g: &DynGrip, z: f64, gs: f32) -> Vec<Pos2> {
    let c = spot(xf, g.at, z);
    match g.shape {
        GripShape::Linear => {
            // The arrow points the way the distance grows, on screen.
            let ahead = spot(xf, g.at + g.dir, z) - c;
            let u = if ahead.length() > 1e-6 { ahead / ahead.length() } else { vec2(1.0, 0.0) };
            let n = vec2(-u.y, u.x);
            let (len, half) = (gs * 1.6, gs * 1.1);
            vec![c - u * (len * 0.5) + n * half, c + u * (len * 0.8), c - u * (len * 0.5) - n * half]
        }
        GripShape::Point => {
            vec![c + vec2(-gs, -gs), c + vec2(gs, -gs), c + vec2(gs, gs), c + vec2(-gs, gs)]
        }
        GripShape::List => {
            let w = gs * 1.3;
            vec![c + vec2(-w, -w * 0.6), c + vec2(w, -w * 0.6), c + vec2(0.0, w * 0.9)]
        }
    }
}

/// Whether `p` is on grip `g` at height `z` (its box, a little larger, as for other grips).
fn hit(xf: &Xf, g: &DynGrip, z: f64, gs: f32, p: Pos2) -> bool {
    let r = Rect::from_points(&outline(xf, g, z, gs)).expand(3.0);
    r.contains(p)
}

/// Draw the grips of the selected dynamic references; the one under the cursor warm, the one
/// being dragged hot.
pub(crate) fn draw(app: &mut CadApp, painter: &Painter, xf: &Xf, rect: Rect, hover: Option<Pos2>) {
    let t = Tokens::get();
    let gs = app.session.settings.gripsize as f32;
    let hot = app.canvas.dyn_hot;
    for (h, g, z) in of_selection(app).iter() {
        if !rect.contains(spot(xf, g.at, *z)) {
            continue;
        }
        let pts = outline(xf, g, *z, gs);
        let is_hot = hot.as_ref().is_some_and(|(hh, n, p)| hh == h && *n == g.node && *p == g.part);
        let warm = !is_hot && hover.is_some_and(|p| hit(xf, g, *z, gs, p));
        let fill = if is_hot {
            t.grip_hot
        } else if warm {
            Color32::from_rgb(0xff, 0x7f, 0x9f)
        } else {
            t.grip_dyn
        };
        painter.add(Shape::convex_polygon(pts, fill, Stroke::new(1.0, Color32::from_rgb(0x10, 0x30, 0x80))));
    }
}

/// The dynamic grip under `hover`, if any: (reference, grip).
pub(crate) fn at(app: &mut CadApp, xf: &Xf, hover: Option<Pos2>) -> Option<(Handle, DynGrip)> {
    let p = hover?;
    let gs = app.session.settings.gripsize as f32;
    of_selection(app).iter().find(|(_, g, z)| hit(xf, g, *z, gs, p)).map(|(h, g, _)| (*h, g.clone()))
}

/// A click on dynamic grip `g` of reference `h`: drag it, or open its list.
pub(crate) fn click(app: &mut CadApp, (h, g): (Handle, DynGrip)) {
    if g.shape == GripShape::List {
        app.canvas.dyn_menu = Some((h, g));
        app.canvas.dyn_menu_fresh = true;
        return;
    }
    // The drag takes the construction plane's points (a tilted plane refuses it, the drawing
    // unchanged).
    match app.session.start_dyn_grip(h, g.node, g.part) {
        Ok(()) => {
            app.canvas.hot_grips.clear();
            app.canvas.dyn_hot = Some((h, g.node, g.part));
            // In a 3D view the cursor moves the grip in its reference's plane (the engine takes the
            // plan point into it), or level through it, so it follows the cursor.
            app.canvas.grip_plane = app.canvas.xf.and_then(|xf| {
                let d = app.session.doc().ok()?;
                let e = d.entity(h)?;
                let tilted = e.plan_xf().and(e.plane()).map(|pl| pl.apply_vec(Vec3::Z));
                crate::canvas::drag_plane(&xf, g.at.to3(height(e, g.at)), tilted)
            });
        }
        Err(e) => app.session.echo(e.to_string()),
    }
}

/// The open list of a list grip: its choices under the grip, the current one checked. A choice
/// sets the property (one undo step); a click elsewhere closes it.
pub(crate) fn menu(app: &mut CadApp, ctx: &egui::Context, xf: &Xf) {
    let Some((h, g)) = app.canvas.dyn_menu.clone() else { return };
    let current = app
        .session
        .state()
        .ok()
        .and_then(|st| cadcraft_engine::doc::dynblock::custom_properties(&st.doc, h))
        .and_then(|(_, ps)| ps.into_iter().find(|p| p.name.eq_ignore_ascii_case(&g.property)))
        .map(|p| p.value)
        .unwrap_or_default();
    let gs = app.session.settings.gripsize as f32;
    let z = of_selection(app).iter().find(|(r, k, _)| *r == h && k.node == g.node && k.part == g.part).map_or(0.0, |(.., z)| *z);
    let at = spot(xf, g.at, z) + vec2(-gs, gs * 1.5);
    let mut chosen: Option<String> = None;
    let area = egui::Area::new(egui::Id::new(("dyn-grip-list", h, g.node))).order(egui::Order::Foreground).fixed_pos(at).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            for c in &g.choices {
                if ui.selectable_label(c.eq_ignore_ascii_case(&current), c.as_str()).clicked() {
                    chosen = Some(c.clone());
                }
            }
        });
    });
    // The click that opened the list isn't a click elsewhere.
    let fresh = std::mem::take(&mut app.canvas.dyn_menu_fresh);
    let clicked_elsewhere = !fresh && ctx.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer();
    if let Some(value) = chosen {
        app.canvas.dyn_menu = None;
        if let Err(e) = app.run("blocks.reference.set", json!({ "handle": h.hex(), "name": g.property, "value": value })) {
            app.session.echo(e);
        }
    } else if clicked_elsewhere || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.canvas.dyn_menu = None;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use cadcraft_engine::geom::Mat4;

    /// Dynamic grips sit where they are in the world through any construction plane (a UCS moved
    /// in plan doesn't carry them along).
    #[test]
    fn dynamic_grips_are_placed_through_the_construction_plane() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0));
        let plan = Xf::new(rect, Vec2::ZERO, 10.0);
        let ucs = plan.on(Mat4::translate(Vec3::new(5.0, 5.0, 0.0)));
        let p = Vec2::new(1.0, 2.0);
        assert!(spot(&ucs, p, 0.0).distance(plan.to_screen(p)) < 1e-3, "world (1, 2), not the UCS's");
        assert!(spot(&ucs, p, 0.0).distance(ucs.to_screen(p)) > 10.0);
    }

    /// A tilted reference's dynamic grips sit on its plane, over the plan point `dynblock::grips`
    /// gives (not on its shadow at z = 0); a level one's at its height; one standing on end on its
    /// plan line at 0.
    #[test]
    fn dynamic_grips_sit_on_a_tilted_references_plane() {
        let ins = cadcraft_engine::doc::Insert {
            block: "B".into(),
            insert: Vec3::new(100.0, 0.0, 5.0),
            scale: Vec3::new(1.0, 1.0, 1.0),
            rotation: 0.0,
            attribs: Vec::new(),
            cols: 1,
            rows: 1,
            col_spacing: 0.0,
            row_spacing: 0.0,
        };
        let reference = |extrusion| Entity {
            handle: Handle(1),
            common: cadcraft_engine::doc::Common { extrusion, ..Default::default() },
            kind: cadcraft_engine::doc::EntityKind::Insert(ins.clone()),
        };
        let p = Vec2::new(136.0, -3.2);
        assert_eq!(height(&reference(Vec3::Z), p), 5.0);
        let tilted = reference(Vec3::new(0.0, -0.6, 0.8));
        let z = height(&tilted, p);
        // On its plane: in its own coordinates the point has no height.
        let own = tilted.plane().unwrap().inverse().unwrap().apply(p.to3(z));
        assert!(own.z.abs() < 1e-9 && z.abs() > 1.0, "{z} {own:?}");
        assert_eq!(height(&reference(Vec3::X), p), 0.0);
    }
}
