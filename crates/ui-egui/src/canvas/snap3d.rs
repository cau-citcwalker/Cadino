//! Object snaps in 3D views: on solids (the ends and midpoints of their edges, the centres of
//! their circular edges) and on curves (lines, polylines, 3D polylines, arcs, circles, ellipses
//! and splines: ends, midpoints, centres and quadrants), where they are in the world. The engine
//! snaps on the construction plane, where a raised object has only its shadow; in a 3D view the
//! canvas finds these points on screen and gives the command the point in CPlane coordinates with
//! its height ([`cadcraft_engine::Session::point_z`]).

use std::sync::{Arc, Weak};

use cadcraft_doc::{Drawing, Entity, EntityKind, Prim, Solid3d, Space};
use cadcraft_engine::Input;
use cadcraft_engine::pointmod::Frame;
use cadcraft_engine::snap::{SnapHit, mode};
use cadcraft_geom::{Vec2, Vec3};
use egui::Pos2;

use super::Xf;
use crate::CadApp;

/// The object snaps found in 3D.
const MODES: u32 = mode::END | mode::MID | mode::CEN | mode::QUA;

/// A closed edge is a circle when all its points are this close (relative to the radius) to the
/// circle through three of them.
const ROUND: f64 = 1e-4;

/// The snap points of solid `s` in the world for snap modes `modes`, with their mode: the ends of
/// its edges, the points halfway along open edges, and the centres of closed edges that are
/// circles. Each polyline of `mesh.edges` is one B-rep edge (the kernel's tessellation emits them
/// so).
/// ponytail: END, MID and CEN only; CEN only for whole circles (an arc edge has no centre) and a
/// closed edge has no midpoint; add arc centres (and QUA, PER …) from the B-rep's curves when
/// users ask.
pub(super) fn candidates(s: &Solid3d, modes: u32) -> Vec<(Vec3, u32)> {
    let mut out = Vec::new();
    for e in &s.mesh.edges {
        let w: Vec<Vec3> = e.iter().map(|p| s.world(*p)).filter(|p| p.is_finite()).collect();
        let (Some(&a), Some(&b)) = (w.first(), w.last()) else { continue };
        let len: f64 = w.windows(2).map(seg_len).sum();
        let closed = w.len() > 3 && (b - a).len() <= 1e-9 * len;
        if modes & mode::END != 0 {
            out.push((a, mode::END));
            if !closed {
                out.push((b, mode::END));
            }
        }
        // A closed edge has no midpoint: its seam is anywhere.
        if modes & mode::MID != 0
            && !closed
            && let Some(m) = halfway(&w, len)
        {
            out.push((m, mode::MID));
        }
        if modes & mode::CEN != 0
            && closed
            && let Some(c) = circle_center(&w)
        {
            out.push((c, mode::CEN));
        }
    }
    out
}

fn seg_len(s: &[Vec3]) -> f64 {
    match s {
        [a, b] => (*b - *a).len(),
        _ => 0.0,
    }
}

/// The point halfway along polyline `w` of length `len`.
fn halfway(w: &[Vec3], len: f64) -> Option<Vec3> {
    if !(len > 0.0 && len.is_finite()) {
        return None;
    }
    let mut left = len / 2.0;
    for s in w.windows(2) {
        let &[a, b] = s else { continue };
        let l = (b - a).len();
        if l > 0.0 && l >= left {
            return Some(a + (b - a) * (left / l));
        }
        left -= l;
    }
    // Rounding left a sliver past the last segment.
    w.last().copied()
}

/// The centre of the circle closed polyline `w` (its last point repeating the first) is on, if
/// it is a circle: the one through three points a third of the way round from each other, which
/// every point is on.
fn circle_center(w: &[Vec3]) -> Option<Vec3> {
    // Fewer points make any triangle or rectangle a circle.
    let n = w.len().checked_sub(1).filter(|n| *n >= 6)?;
    let (p, q, r) = (*w.first()?, *w.get(n / 3)?, *w.get(2 * n / 3)?);
    let (a, b) = (p - r, q - r);
    let axb = a.cross(b);
    let d = 2.0 * axb.dot(axb);
    if !(d > 0.0 && d.is_finite()) {
        return None;
    }
    let c = r + (b * a.dot(a) - a * b.dot(b)).cross(axb) * (1.0 / d);
    let (radius, normal) = ((p - c).len(), axb.normalized());
    let on = |x: &Vec3| ((*x - c).len() - radius).abs() <= ROUND * radius && (*x - c).dot(normal).abs() <= ROUND * radius;
    (radius > 0.0 && c.is_finite() && w.iter().all(on)).then_some(c)
}

/// Object `e`'s grips ([`EntityKind::grips`], in that order) where they are in the world: a
/// planar object's lifted from its plane, a true 3D object's at their height.
pub(super) fn grips3(e: &Entity) -> Vec<Vec3> {
    if let Some(pl) = e.plane() {
        return e.kind.grips().into_iter().map(|g| pl.apply(g.to3(0.0))).collect();
    }
    match &e.kind {
        EntityKind::Line(l) => vec![l.a, l.a + (l.b - l.a) * 0.5, l.b],
        EntityKind::Point(p) => vec![p.p],
        EntityKind::Polyline3d(p) => p.points.clone(),
        EntityKind::Face3d(f) => f.corners.to_vec(),
        EntityKind::Ray(r) | EntityKind::XLine(r) => vec![r.base, r.base + r.dir],
        EntityKind::Helix(h) => vec![h.center, h.start_point(), h.end_point()],
        EntityKind::Solid3d(s) => s.corners(),
        k => k.grips().into_iter().map(|g| g.to3(0.0)).collect(),
    }
}

/// The snap points of object `e` in the world for snap modes `modes`, with their mode, into
/// `out`: a solid's ([`candidates`]); the ends and midpoints of lines, polyline segments and 3D
/// polyline segments; the centres of arcs, circles and ellipses and the quadrants of circles and
/// whole ellipses; the ends of open curves. Other objects have none here.
/// ponytail: no NEA, PER, TAN or INT in 3D (the engine's still work on the CPlane); add them from
/// the curves' 3D geometry when users ask.
pub(super) fn snaps_of(e: &Entity, modes: u32, out: &mut Vec<(Vec3, u32)>) {
    let mut add = |p: Vec3, m: u32| {
        if modes & m != 0 && p.is_finite() {
            out.push((p, m));
        }
    };
    match &e.kind {
        EntityKind::Solid3d(s) => {
            for (p, m) in candidates(s, modes) {
                add(p, m);
            }
            return;
        }
        EntityKind::Line(l) => {
            add(l.a, mode::END);
            add(l.b, mode::END);
            add(l.a + (l.b - l.a) * 0.5, mode::MID);
            return;
        }
        EntityKind::Polyline3d(p) => {
            let n = p.points.len();
            for (i, q) in p.points.iter().enumerate() {
                add(*q, mode::END);
                let next = if i + 1 < n { p.points.get(i + 1) } else { p.points.first().filter(|_| p.closed && n > 2) };
                if let Some(r) = next {
                    add(*q + (*r - *q) * 0.5, mode::MID);
                }
            }
            return;
        }
        EntityKind::Circle(_) | EntityKind::Arc(_) | EntityKind::Ellipse(_) | EntityKind::LwPolyline(_) | EntityKind::Spline(_) => {}
        _ => return,
    }
    let Some(pl) = e.plane() else { return };
    let mut add2 = |p: Vec2, m: u32| add(pl.apply(p.to3(0.0)), m);
    for prim in e.kind.prims() {
        match prim {
            Prim::Seg(seg) => {
                add2(seg.start(), mode::END);
                add2(seg.end(), mode::END);
                add2(seg.mid(), mode::MID);
                if let cadcraft_geom::Segment::Arc { arc, .. } = seg {
                    add2(arc.center, mode::CEN);
                }
            }
            Prim::Circle(c) => {
                add2(c.center, mode::CEN);
                for d in [Vec2::X, Vec2::Y, -Vec2::X, -Vec2::Y] {
                    add2(c.center + d * c.radius, mode::QUA);
                }
            }
            Prim::Ellipse(el) => {
                add2(el.center, mode::CEN);
                if el.is_full() {
                    for d in [el.major, el.minor(), -el.major, -el.minor()] {
                        add2(el.center + d, mode::QUA);
                    }
                } else {
                    add2(el.at_param(el.start), mode::END);
                    add2(el.at_param(el.end), mode::END);
                }
            }
            Prim::Spline(sp) if !sp.closed => {
                let (t0, t1) = sp.domain();
                add2(sp.eval(t0), mode::END);
                add2(sp.eval(t1), mode::END);
            }
            _ => {}
        }
    }
}

/// The drawing a 3D view's engine object snaps run on ([`ground_doc`]), kept while the drawing
/// the construction plane sees stays the same.
#[derive(Default)]
pub(crate) struct Ground {
    of: Weak<Drawing>,
    /// `None`: no object is off the plane (the engine's own drawing will do).
    doc: Option<Arc<Drawing>>,
}

/// Whether object `e` (in construction-plane coordinates) lies in the plane: a planar object
/// whose plane is the CPlane's XY plane, or one whose points all have no height. Solids count
/// as in it: the engine leaves them out of 3D views' snaps itself (`NO_SOLIDS`).
fn in_cplane(e: &Entity) -> bool {
    let flat = |p: Vec3| p.z.abs() <= 1e-9 * (1.0 + p.x.abs() + p.y.abs());
    match e.plane() {
        Some(pl) => [Vec2::ZERO, Vec2::X, Vec2::Y].into_iter().all(|q| flat(pl.apply(q.to3(0.0)))),
        None => matches!(e.kind, EntityKind::Solid3d(_)) || grips3(e).into_iter().all(flat),
    }
}

/// In a 3D view, the drawing the construction plane sees ([`cadcraft_engine::Session::pick_doc`])
/// without the objects off the plane, when there are any: the engine snaps in the plane, where
/// such an object has only its shadow, so its snaps there would be where nothing is drawn (this
/// module snaps it where it is). `None` in plan views (AutoCAD snaps to what plan shows) and when
/// every object is in the plane.
/// ponytail: copies the plane's objects after every edit while a 3D view snaps (O(objects));
/// give the engine's snap a mode bit that skips them, as `NO_SOLIDS` does solids, if large
/// models lag.
pub(crate) fn ground_doc(app: &mut CadApp, xf: &Xf) -> Option<Arc<Drawing>> {
    if xf.is_plan() {
        return None;
    }
    let d = app.session.pick_doc().ok()?;
    let g = &mut app.canvas.ground;
    if !Weak::ptr_eq(&g.of, &Arc::downgrade(&d)) {
        g.doc = d.model.iter().any(|e| !in_cplane(e)).then(|| {
            let mut out = (*d).clone();
            let mut model = cadcraft_doc::EntityStore::new();
            for e in d.model.iter().filter(|e| in_cplane(e)) {
                model.push(e.as_ref().clone());
            }
            out.model = model;
            Arc::new(out)
        });
        g.of = Arc::downgrade(&d);
    }
    g.doc.clone()
}

/// Where `xf` draws world point `p` of a top-level object.
pub(super) fn screen_of(xf: &Xf, p: Vec3) -> Pos2 {
    xf.world_to_screen(p)
}

/// The snap point of the objects `xf` shows (top level, visible) nearest screen point `at` within
/// `aperture` points, and its mode; of two in the same place, the nearer to the viewer.
/// ponytail: scans every object's snap points each time the cursor moves at a point prompt (a
/// layer is looked up only for a point within the aperture); keep the candidates with the display
/// list if large drawings lag. Objects in block references aren't snapped to.
pub(super) fn nearest(doc: &Drawing, space: &Space, xf: &Xf, at: Pos2, aperture: f64, modes: u32) -> Option<(Vec3, u32)> {
    let mut best: Option<(f64, f64, Vec3, u32)> = None;
    let mut cands = Vec::new();
    for e in doc.space(space)?.iter() {
        cands.clear();
        snaps_of(e, modes, &mut cands);
        // The visibility check looks the layer up by name: only for a point that would win.
        let mut visible = None;
        for &(p, m) in &cands {
            let (d, depth) = (f64::from(screen_of(xf, p).distance(at)), xf.dir().depth(p));
            if d <= aperture && best.is_none_or(|(bd, bz, ..)| d < bd || (d == bd && depth > bz)) && *visible.get_or_insert_with(|| doc.is_visible(e))
            {
                best = Some((d, depth, p, m));
            }
        }
    }
    best.map(|(.., p, m)| (p, m))
}

/// In a 3D view at a point prompt, snap the cursor at screen point `at` to the objects where they
/// are drawn (see [`nearest`]) with the running object snaps or the override: a snap nearer on
/// screen than the engine's (on the construction plane) replaces it, in CPlane coordinates with
/// its height in `canvas.snap_z`. Plan views keep the engine's snaps, and so does Tab cycling
/// when it picked a candidate.
/// ponytail: Tab cycles only the engine's candidates, not these; add these to the cycle if users
/// want to Tab through them.
pub(super) fn apply(app: &mut CadApp, xf: &Xf, at: Pos2) {
    app.canvas.snap_z = None;
    let wants_point = app.session.current_prompt().is_some_and(|p| cadcraft_engine::pointmod::accepts(&p));
    // Tab cycling keeps the engine candidate it picked; with none near, the 3D points still snap.
    let cycled = app.canvas.snap_cycle.is_some() && app.canvas.snap.is_some();
    if xf.is_plan() || !wants_point || app.session.pending_window.is_some() || cycled {
        return;
    }
    let osmode = app.session.snap_override().unwrap_or(app.session.settings.osmode);
    let modes = if osmode & mode::OFF != 0 { 0 } else { osmode & MODES };
    if modes == 0 {
        return;
    }
    let Ok(st) = app.session.state() else { return };
    let Some((p, m)) = nearest(&st.doc, &st.edit_space(), xf, at, app.session.settings.aperture, modes) else { return };
    if app.canvas.snap.is_some_and(|h| xf.to_screen(h.point).distance(at) < screen_of(xf, p).distance(at)) {
        return;
    }
    let local = xf.cplane().inverse().map_or(p, |ci| ci.apply(p));
    let name = match m {
        mode::MID => "Midpoint",
        mode::CEN => "Center",
        mode::QUA => "Quadrant",
        _ => "Endpoint",
    };
    app.canvas.snap = Some(SnapHit { point: local.xy(), mode: m, name, viewport: None, deferred: None });
    app.canvas.snap_z = Some(local.z);
    app.canvas.track = None;
    app.canvas.cursor = Some(local.xy());
    app.session.cursor = local.xy();
    app.session.cursor_deferred = None;
}

/// Give the prompt the point the cursor is on: its snap, else `p`. A 3D snap's height goes with
/// it in [`cadcraft_engine::Session::point_z`]; every other point is on the construction plane.
/// ponytail: the command's rubber band from a 3D-snapped point still draws from its point on the
/// CPlane (previews are 2D); draw previews in 3D when commands preview solids.
pub(super) fn send_point(app: &mut CadApp, p: Vec2) -> cadcraft_engine::Result<()> {
    app.session.point_z = None;
    match (app.canvas.snap, app.canvas.snap_z) {
        (Some(hit), Some(z)) => {
            // The snap override (END …) found its point here: the engine would look again on the
            // CPlane.
            if matches!(app.session.point_mods.last(), Some(Frame::Snap { .. })) {
                app.session.point_mods.pop();
            }
            app.session.point_z = Some(z);
            app.session.input(Input::Point(hit.point))
        }
        (Some(hit), None) => app.session.input_snapped(hit),
        (None, _) => app.session.input(Input::Point(p)),
    }
}
