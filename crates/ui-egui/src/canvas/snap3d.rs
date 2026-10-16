//! Object snaps on 3D solids in a 3D view: the ends and midpoints of their edges and the centres
//! of their circular edges, where they are in the world. The engine snaps in plan, on the XY
//! plane, where a raised solid has only its shadow; in a 3D view the canvas finds these points
//! on screen and gives the command the point with its height
//! ([`cadcraft_engine::Session::point_z`]).

use cadcraft_doc::{Drawing, EntityKind, Solid3d, Space};
use cadcraft_engine::Input;
use cadcraft_engine::pointmod::Frame;
use cadcraft_engine::snap::{SnapHit, mode};
use cadcraft_geom::{Vec2, Vec3};
use egui::Pos2;

use super::Xf;
use crate::CadApp;

/// The object snaps found on solids.
const MODES: u32 = mode::END | mode::MID | mode::CEN;

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

/// Where `xf` draws world point `p` of a top-level object.
pub(super) fn screen_of(xf: &Xf, p: Vec3) -> Pos2 {
    xf.view_to_screen(xf.dir.project(p))
}

/// The snap point of the solids `xf` shows (top level, visible) nearest screen point `at` within
/// `aperture` points, and its mode; of two in the same place, the nearer to the viewer.
/// ponytail: scans every solid's edges each time the cursor moves at a point prompt; keep the
/// candidates with the display list if drawings with many solids lag. Solids in block
/// references aren't snapped to.
pub(super) fn nearest(doc: &Drawing, space: &Space, xf: &Xf, at: Pos2, aperture: f64, modes: u32) -> Option<(Vec3, u32)> {
    let mut best: Option<(f64, f64, Vec3, u32)> = None;
    for e in doc.space(space)?.iter() {
        // The kind first: the visibility check looks the layer up by name.
        let EntityKind::Solid3d(s) = &e.kind else { continue };
        if !doc.is_visible(e) {
            continue;
        }
        for (p, m) in candidates(s, modes) {
            let (d, depth) = (f64::from(screen_of(xf, p).distance(at)), xf.dir.depth(p));
            if d <= aperture && best.is_none_or(|(bd, bz, ..)| d < bd || (d == bd && depth > bz)) {
                best = Some((d, depth, p, m));
            }
        }
    }
    best.map(|(.., p, m)| (p, m))
}

/// In a 3D view at a point prompt, snap the cursor at screen point `at` to the solids (see
/// [`nearest`]) with the running object snaps or the override: a snap nearer on screen than the
/// engine's plan one replaces it, its height in `canvas.snap_z`. Plan views keep the engine's
/// snaps, and so does Tab cycling when it picked a candidate.
/// ponytail: Tab cycles only the engine's (plan) candidates, not these; add the solids' points to
/// the cycle if users want to Tab through them.
pub(super) fn apply(app: &mut CadApp, xf: &Xf, at: Pos2) {
    app.canvas.snap_z = None;
    let wants_point = app.session.current_prompt().is_some_and(|p| cadcraft_engine::pointmod::accepts(&p));
    // Tab cycling keeps the engine candidate it picked; with none near, the solids still snap.
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
    let name = match m {
        mode::MID => "Midpoint",
        mode::CEN => "Center",
        _ => "Endpoint",
    };
    app.canvas.snap = Some(SnapHit { point: p.xy(), mode: m, name, viewport: None, deferred: None });
    app.canvas.snap_z = Some(p.z);
    app.canvas.track = None;
    app.canvas.cursor = Some(p.xy());
    app.session.cursor = p.xy();
    app.session.cursor_deferred = None;
}

/// Give the prompt the point the cursor is on: its snap, else `p`. A 3D snap's height goes with
/// it in [`cadcraft_engine::Session::point_z`]; every other point is on the XY plane.
/// ponytail: the command's rubber band from a 3D-snapped point still draws from its plan point
/// (previews are 2D, at z = 0); draw previews in 3D when commands preview solids.
pub(super) fn send_point(app: &mut CadApp, p: Vec2) -> cadcraft_engine::Result<()> {
    app.session.point_z = None;
    match (app.canvas.snap, app.canvas.snap_z) {
        (Some(hit), Some(z)) => {
            // The snap override (END …) found its point here: the engine would look again in plan.
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
