//! Rhino's gumball. With objects selected in a 3D view and no command running, handles at the
//! centre of the selection's bounding box move it along the world axes (arrows: X red, Y green, Z
//! blue), turn it about them (arcs) and scale it (the centre square). The handle under the cursor
//! lights up; a drag shows the objects where they will go, a number typed during it sets the
//! exact distance, angle (degrees) or factor, and letting go runs 3DMOVE, 3DROTATE or 3DSCALE with
//! world values (one undo step). Esc cancels the drag; every other key during it is the
//! gumball's (none reaches the command line). A grip under a handle wins: the gumball never hides
//! AutoCAD's grips. `ui.gumball` turns it off; plan views keep AutoCAD's grips.
//!
//! ponytail: world axes only (no gumball aligned to the object or the CPlane), drags don't snap to
//! objects or steps, and an arc whose plane the view sees edge-on is hidden (an axis pointing at
//! the viewer has no arrow); add Rhino's alignment and snapping options when users ask.

use std::sync::Arc;

use cadcraft_color::Rgb;
use cadcraft_doc::{Drawing, Entity, EntityKind, Handle, Space};
use cadcraft_geom::{Mat4, Vec3};
use egui::{Pos2, Shape, Stroke, vec2};
use serde_json::{Value, json};

use super::{Xf, draw_list, rebuilt_preview, snap3d};
use crate::CadApp;
use crate::theme::Tokens;

/// Arrow length and arc radius, in points.
const ARROW: f32 = 80.0;
const ARC: f32 = 55.0;
/// Half the side of the centre square, and how near the cursor takes a handle, in points.
const SQUARE: f32 = 5.0;
const NEAR: f32 = 6.0;
/// Points dragged right (or up) that double the scale.
const DOUBLING: f32 = 100.0;

/// Each world axis with the two that span the plane it turns in.
const AXES: [(Vec3, Vec3, Vec3); 3] = [(Vec3::X, Vec3::Y, Vec3::Z), (Vec3::Y, Vec3::Z, Vec3::X), (Vec3::Z, Vec3::X, Vec3::Y)];

/// A gumball handle: an axis's arrow or arc (by index into [`AXES`]), or the centre square.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Part {
    Move(usize),
    Rotate(usize),
    Scale,
}

/// A handle being dragged.
#[derive(Clone, Debug)]
pub(crate) struct Drag {
    part: Part,
    /// The gumball's centre when the drag started (world).
    origin: Vec3,
    from: Pos2,
    to: Pos2,
    /// A number typed during the drag (it wins over the dragged value).
    typed: String,
    cancelled: bool,
}

/// What the selection's centre was worked out for: drawing uid, revision and copy, selection.
type CenterKey = (u64, u64, usize, Vec<Handle>);

#[derive(Default)]
pub(crate) struct Gumball {
    drag: Option<Drag>,
    /// The selection's centre in the world ([`center`]), kept while its key holds.
    center: Option<(CenterKey, Option<Vec3>)>,
}

impl Gumball {
    /// A handle is held down: the keyboard is the gumball's ([`input`]).
    pub(crate) fn dragging(&self) -> bool {
        self.drag.is_some()
    }
}

/// Whether the gumball shows in the view `xf` (see the module docs).
fn shown(app: &CadApp, xf: &Xf) -> bool {
    app.ui.gumball
        && !xf.is_plan()
        && app.session.running.is_none()
        && !app.session.prompting()
        && app.session.pending_window.is_none()
        && app.session.layout_space() == Space::Model
        && !app.session.selection().is_empty()
}

/// The centre of the selection's bounding box in the world ([`center`]), worked out once per
/// drawing revision and selection.
fn origin(app: &mut CadApp) -> Option<Vec3> {
    let st = app.session.state().ok()?;
    let key = (st.uid, st.revision, Arc::as_ptr(&st.doc) as usize, app.session.selection());
    if let Some((k, o)) = &app.canvas.gumball.center
        && *k == key
    {
        return *o;
    }
    let o = center(&st.doc, &key.3);
    app.canvas.gumball.center = Some((key, o));
    o
}

/// The centre of the box round objects `hs` of drawing `d` in the world, each object's box where
/// it is ([`corners`]).
fn center(d: &Drawing, hs: &[Handle]) -> Option<Vec3> {
    let mut b: Option<(Vec3, Vec3)> = None;
    for h in hs {
        let Some(e) = d.entity(*h) else { continue };
        for p in corners(d, e).into_iter().filter(|p| p.is_finite()) {
            b = Some(match b {
                None => (p, p),
                Some((lo, hi)) => (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z))),
            });
        }
    }
    b.map(|(lo, hi)| (lo + hi) * 0.5).filter(|o| o.is_finite())
}

/// Corners of object `e`'s box in the world: a solid's mesh points; a planar object's box in its
/// own plane (the drawing's bounds of it laid in the XY plane, [`cadcraft_doc::entity_bounds`]:
/// text, blocks, hatches and splines whole) placed through that plane; a 3D object's points.
/// ponytail: a block reference's box is its contents' plan box at its plane (contents raised
/// inside the block reach past it), as the drawing's extents are.
fn corners(d: &Drawing, e: &Entity) -> Vec<Vec3> {
    match (&e.kind, e.plane()) {
        (EntityKind::Solid3d(s), _) => s.mesh.points().map(|p| s.world(p)).collect(),
        (_, Some(pl)) => {
            let mut own = e.clone();
            own.common.extrusion = Vec3::Z;
            let b = cadcraft_doc::entity_bounds(d, &own, 0);
            if b.is_empty() {
                return snap3d::grips3(e);
            }
            b.corners().iter().map(|c| pl.apply(c.to3(0.0))).collect()
        }
        _ => snap3d::grips3(e),
    }
}

/// World vector `v` on screen (one unit: one point).
fn screen_dir(xf: &Xf, v: Vec3) -> egui::Vec2 {
    let p = xf.dir().project(v);
    vec2(p.x as f32, -p.y as f32)
}

/// Axis `i`'s arrow from the centre `o` (base clear of the centre square, tip); none when the
/// axis points nearly at the viewer.
fn arrow(xf: &Xf, o: Vec3, i: usize) -> Option<(Pos2, Pos2)> {
    let (a, ..) = *AXES.get(i)?;
    let v = screen_dir(xf, a) * ARROW;
    let so = xf.world_to_screen(o);
    (v.length() >= 0.3 * ARROW).then(|| (so + v * 0.2, so + v))
}

/// The quarter arc turning about axis `i` round the centre `o`; none when the view sees its plane
/// edge-on.
fn arc(xf: &Xf, o: Vec3, i: usize) -> Option<Vec<Pos2>> {
    let (a, u, w) = *AXES.get(i)?;
    if a.dot(xf.dir().dir).abs() < 0.25 {
        return None;
    }
    let (su, sw, so) = (screen_dir(xf, u), screen_dir(xf, w), xf.world_to_screen(o));
    let t = |k: u8| 0.2 + (std::f32::consts::FRAC_PI_2 - 0.4) * f32::from(k) / 16.0;
    Some((0..=16).map(|k| so + (su * t(k).cos() + sw * t(k).sin()) * ARC).collect())
}

/// The distance from `p` to segment `ab`.
fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t).distance(p)
}

/// The handle of the gumball at `o` under screen point `at`.
fn hit(xf: &Xf, o: Vec3, at: Pos2) -> Option<Part> {
    let so = xf.world_to_screen(o);
    if (at.x - so.x).abs() <= SQUARE + 2.0 && (at.y - so.y).abs() <= SQUARE + 2.0 {
        return Some(Part::Scale);
    }
    let near_arc = |pts: Vec<Pos2>| pts.windows(2).any(|w| matches!(w, [a, b] if seg_dist(at, *a, *b) <= NEAR));
    (0..3)
        .find(|&i| arrow(xf, o, i).is_some_and(|(a, b)| seg_dist(at, a, b) <= NEAR))
        .map(Part::Move)
        .or_else(|| (0..3).find(|&i| arc(xf, o, i).is_some_and(near_arc)).map(Part::Rotate))
}

impl Drag {
    /// The distance (world units), angle (degrees) or factor dragging gives: the number typed if
    /// there is one.
    fn value(&self, xf: &Xf) -> f64 {
        if let Some(v) = self.typed.trim().parse::<f64>().ok().filter(|v| v.is_finite()) {
            return v;
        }
        let d = self.to - self.from;
        match self.part {
            Part::Move(i) => {
                // Points per world unit along the axis.
                let s = AXES.get(i).map_or(egui::Vec2::ZERO, |(a, ..)| screen_dir(xf, *a) * xf.scale as f32);
                let v = f64::from(d.dot(s)) / f64::from(s.length_sq()).max(1e-12);
                if v.is_finite() { v } else { 0.0 }
            }
            Part::Rotate(i) => {
                let Some((a, ..)) = AXES.get(i) else { return 0.0 };
                match (xf.ray_on(self.from, self.origin, *a), xf.ray_on(self.to, self.origin, *a)) {
                    (Some(p), Some(q)) => {
                        let (u, v) = (p - self.origin, q - self.origin);
                        let deg = a.dot(u.cross(v)).atan2(u.dot(v)).to_degrees();
                        if deg.is_finite() { deg } else { 0.0 }
                    }
                    _ => 0.0,
                }
            }
            Part::Scale => 2f64.powf(f64::from((d.x - d.y) / DOUBLING)),
        }
    }

    /// The world transform of value `v`; `None` when it changes nothing.
    fn matrix(&self, v: f64) -> Option<Mat4> {
        let o = self.origin;
        match self.part {
            Part::Move(i) => AXES.get(i).filter(|_| v != 0.0).map(|(a, ..)| Mat4::translate(*a * v)),
            Part::Rotate(i) => AXES.get(i).filter(|_| v != 0.0).map(|(a, ..)| Mat4::rotate_about(o, *a, v.to_radians())),
            Part::Scale => (v != 1.0).then(|| Mat4::translate(o).mul(&Mat4::scale(Vec3::new(v, v, v))).mul(&Mat4::translate(-o))),
        }
    }

    /// The command that does value `v` to the objects `handles`.
    fn command(&self, v: f64, handles: Vec<String>) -> Option<(&'static str, Value)> {
        let o = self.origin;
        let p = |q: Vec3| json!([q.x, q.y, q.z]);
        Some(match self.part {
            Part::Move(i) => ("3dmove", json!({ "handles": handles, "delta": p(AXES.get(i)?.0 * v) })),
            Part::Rotate(i) => ("3drotate", json!({ "handles": handles, "axis": [p(o), p(o + AXES.get(i)?.0)], "angle": v })),
            Part::Scale => ("3dscale", json!({ "handles": handles, "base": p(o), "factor": v })),
        })
    }
}

/// Whether a grip (an object's or a dynamic block's) is under screen point `at`: grips win over
/// the gumball's handles there.
fn on_grip(app: &mut CadApp, xf: &Xf, at: Pos2) -> bool {
    super::grip_at(app, xf, Some(at)).is_some() || crate::dyngrips::at(app, xf, Some(at)).is_some()
}

/// The gumball's share of this frame's pointer and keys: a press on a handle (not on a grip)
/// starts a drag; during it, Esc cancels it, digits typed set its value, Enter runs it at once
/// and every other key and text is swallowed (Space must not repeat the last command under the
/// drag); letting go runs the command. Returns whether the gumball has the pointer (the canvas
/// then neither picks nor opens a window with it).
pub(super) fn input(app: &mut CadApp, ctx: &egui::Context, xf: &Xf, hover: Option<Pos2>, inside: bool) -> bool {
    let (pressed, down, press_at, latest) =
        ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.press_origin(), i.pointer.latest_pos()));
    let Some(mut d) = app.canvas.gumball.drag.take() else {
        if !(pressed && inside && shown(app, xf)) {
            return false;
        }
        let Some(at) = press_at.or(hover) else { return false };
        if on_grip(app, xf, at) {
            return false;
        }
        let Some(o) = origin(app) else { return false };
        let Some(part) = hit(xf, o, at) else { return false };
        app.canvas.gumball.drag = Some(Drag { part, origin: o, from: at, to: at, typed: String::new(), cancelled: false });
        return true;
    };
    let mut enter = false;
    ctx.input_mut(|i| {
        i.events.retain(|e| match e {
            egui::Event::Key { key: egui::Key::Enter, pressed: true, .. } => {
                enter = true;
                false
            }
            egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                d.cancelled = true;
                false
            }
            egui::Event::Key { key: egui::Key::Backspace, pressed: true, .. } => {
                d.typed.pop();
                false
            }
            egui::Event::Text(t) => {
                if t.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+')) {
                    d.typed.push_str(t);
                }
                false
            }
            egui::Event::Key { .. } | egui::Event::Paste(_) | egui::Event::Copy | egui::Event::Cut => false,
            _ => true,
        })
    });
    if let Some(p) = latest {
        d.to = p;
    }
    if enter && !d.cancelled {
        // Done: letting go later changes nothing.
        finish(app, xf, &d);
        d.cancelled = true;
    }
    if down {
        app.canvas.gumball.drag = Some(d);
    } else if !d.cancelled {
        finish(app, xf, &d);
    }
    true
}

/// Run the command of a drag let go (not while a command started meanwhile runs: its undo step
/// and this one would interleave).
fn finish(app: &mut CadApp, xf: &Xf, d: &Drag) {
    let v = d.value(xf);
    if d.matrix(v).is_none() || app.session.running.is_some() {
        return;
    }
    let handles = app.session.selection().iter().map(|h| h.hex()).collect();
    if let Some((id, params)) = d.command(v, handles) {
        // A refusal (locked layers, a transform an object can't take) is echoed by `run`.
        let _ = app.run(id, params);
    }
}

/// Draw the gumball (and during a drag the objects where they will go, and the value beside the
/// cursor); `hover`: the cursor, which lights the handle under it (unless a grip is there).
pub(super) fn paint(app: &mut CadApp, p: &egui::Painter, xf: &Xf, hover: Option<Pos2>, bg: Rgb) {
    let dragged = app.canvas.gumball.drag.as_ref().filter(|d| !d.cancelled).map(|d| d.origin);
    if dragged.is_none() && !shown(app, xf) {
        return;
    }
    let Some(o) = dragged.or_else(|| origin(app)) else { return };
    let hover = hover.filter(|h| !on_grip(app, xf, *h));
    let app = &*app;
    let drag = app.canvas.gumball.drag.as_ref().filter(|d| !d.cancelled);
    let t = Tokens::get();
    let mut at = o;
    if let Some(d) = drag {
        let v = d.value(xf);
        if let (Some(m), Ok(st)) = (d.matrix(v), app.session.state()) {
            let moved: Vec<cadcraft_doc::Entity> = app
                .session
                .selection()
                .iter()
                .filter_map(|h| st.doc.entity(*h))
                .filter(|e| !st.doc.layer(&e.common.layer).is_some_and(|l| l.locked))
                .filter_map(|e| {
                    let mut e = e.as_ref().clone();
                    e.transform3(&m).then_some(e)
                })
                .collect();
            draw_list(p, xf, &rebuilt_preview(&st.doc, moved.iter(), xf.scale, xf.dir()), bg, false);
            at = m.apply(o);
        }
        let label = match d.part {
            Part::Move(_) => crate::tl!("Distance"),
            Part::Rotate(_) => crate::tl!("Angle"),
            Part::Scale => crate::tl!("Scale"),
        };
        let value = if d.typed.is_empty() { format!("{:.4}", (v * 1e4).round() / 1e4) } else { d.typed.clone() };
        super::tooltip(p, d.to + vec2(16.0, 18.0), &format!("{label}: {value}"));
    }
    let hot = drag.map(|d| d.part).or_else(|| hover.and_then(|h| hit(xf, o, h)));
    let color = |part: Part, i: usize| if hot == Some(part) { t.gumball_hot } else { t.gumball.get(i).copied().unwrap_or(t.canvas_ink) };
    for i in 0..3 {
        if let Some((a, b)) = arrow(xf, at, i) {
            let c = color(Part::Move(i), i);
            let (dir, side) = ((b - a).normalized(), (b - a).normalized().rot90());
            p.line_segment([a, b - dir * 8.0], Stroke::new(2.0, c));
            p.add(Shape::convex_polygon(vec![b, b - dir * 11.0 + side * 4.5, b - dir * 11.0 - side * 4.5], c, Stroke::NONE));
        }
        if let Some(pts) = arc(xf, at, i) {
            p.add(Shape::line(pts, Stroke::new(2.0, color(Part::Rotate(i), i))));
        }
    }
    let so = xf.world_to_screen(at);
    let sq = egui::Rect::from_center_size(so, vec2(SQUARE * 2.0, SQUARE * 2.0));
    p.rect_filled(sq, 1.0, if hot == Some(Part::Scale) { t.gumball_hot } else { t.canvas_ink });
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn xf(dir: [f64; 3]) -> Xf {
        Xf::new(egui::Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0)), cadcraft_geom::Vec2::ZERO, 10.0)
            .looking(cadcraft_geom::ViewDir::new(Vec3::new(dir[0], dir[1], dir[2])))
    }

    /// The arrows, arcs and centre square are found where they are drawn; an axis pointing at the
    /// viewer has no arrow, and an axis whose plane is seen edge-on has no arc.
    #[test]
    fn handles_are_hit_where_they_are_drawn() {
        let iso = xf([-1.0, -1.0, 1.0]);
        let o = Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(hit(&iso, o, iso.world_to_screen(o)), Some(Part::Scale));
        for i in 0..3 {
            let (_, tip) = arrow(&iso, o, i).unwrap();
            assert_eq!(hit(&iso, o, tip), Some(Part::Move(i)));
            // A point of the arc clear of the arrows (which take the cursor first).
            let clear = |q: &&Pos2| (0..3).all(|j| arrow(&iso, o, j).is_none_or(|(a, b)| seg_dist(**q, a, b) > NEAR));
            let pts = arc(&iso, o, i).unwrap();
            assert_eq!(hit(&iso, o, *pts.iter().find(clear).unwrap()), Some(Part::Rotate(i)));
        }
        let front = xf([0.0, -1.0, 0.0]);
        assert!(arrow(&front, o, 1).is_none() && arrow(&front, o, 0).is_some());
        assert!(arc(&front, o, 0).is_none() && arc(&front, o, 2).is_none() && arc(&front, o, 1).is_some());
    }

    /// The gumball sits at the middle of what is drawn: a text's whole box (not its insertion
    /// point) at its height, a circle standing upright whole.
    #[test]
    fn the_centre_is_the_middle_of_the_objects_boxes() {
        let mut d = Drawing::default();
        let text = cadcraft_doc::Text {
            insert: Vec3::new(0.0, 0.0, 7.0),
            align_pt: None,
            height: 10.0,
            value: "ABCD".into(),
            rotation: 0.0,
            width_factor: 1.0,
            oblique: 0.0,
            style: "Standard".into(),
            halign: Default::default(),
            valign: Default::default(),
        };
        let t = d.add(&Space::Model, cadcraft_doc::Common::default(), EntityKind::Text(text)).unwrap();
        // 36 wide (0.9 of the height a letter), 10 high, from the insertion point.
        let c = center(&d, &[t]).unwrap();
        assert!((c - Vec3::new(18.0, 5.0, 7.0)).len() < 1e-9, "{c:?}");
        // Standing in world YZ (extrusion +X: its OCS x along world Y, y along Z) at x = 3: its
        // centre (0, 4) in its plane is world (3, 0, 4).
        let common = cadcraft_doc::Common { extrusion: Vec3::X, ..Default::default() };
        let circle = cadcraft_doc::Circle { center: Vec3::new(0.0, 4.0, 3.0), radius: 2.0 };
        let s = d.add(&Space::Model, common, EntityKind::Circle(circle)).unwrap();
        let c = center(&d, &[s]).unwrap();
        assert!((c - Vec3::new(3.0, 0.0, 4.0)).len() < 1e-9, "{c:?}");
        assert_eq!(center(&d, &[]), None);
    }

    /// A typed number wins over the drag; a drag along an arrow is the distance along its axis, and
    /// a turn about the Y axis seen from the front is the angle turned on screen (clockwise there,
    /// counter-clockwise about +Y).
    #[test]
    fn drags_give_distances_angles_and_factors() {
        let front = xf([0.0, -1.0, 0.0]);
        let o = Vec3::ZERO;
        let so = front.world_to_screen(o);
        let mut d = Drag { part: Part::Move(0), origin: o, from: so, to: so + vec2(50.0, 30.0), typed: String::new(), cancelled: false };
        assert!((d.value(&front) - 5.0).abs() < 1e-6);
        d.typed = "12.5".into();
        assert_eq!(d.value(&front), 12.5);
        d.typed = "-".into();
        assert!((d.value(&front) - 5.0).abs() < 1e-6, "an unfinished number is no number");
        let r = Drag { part: Part::Rotate(1), from: so + vec2(40.0, 0.0), to: so + vec2(0.0, 40.0), typed: String::new(), ..d.clone() };
        // From +X (right) to -Z (down on screen): +X turns to -Z, a quarter turn about +Y.
        assert!((r.value(&front) - 90.0).abs() < 1e-6, "{}", r.value(&front));
        let s = Drag { part: Part::Scale, from: so, to: so + vec2(100.0, 0.0), ..r.clone() };
        assert!((s.value(&front) - 2.0).abs() < 1e-9);
        assert!(s.matrix(1.0).is_none() && d.matrix(0.0).is_none());
    }
}
