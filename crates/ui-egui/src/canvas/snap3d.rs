//! Object snaps in 3D views: on solids (the ends and midpoints of their edges, the centres of
//! their circular edges) and on curves (lines, polylines, 3D polylines, arcs, circles, ellipses
//! and splines: ends, midpoints, centres and quadrants), where they are in the world; along them
//! the nearest point, the perpendicular from the base point, and where two of them cross on screen
//! (an intersection where they meet in the world, an apparent intersection where one passes
//! behind the other). The engine snaps on the construction plane, where a raised object has only
//! its shadow; in a 3D view the canvas finds these points on screen and gives the command the
//! point in CPlane coordinates with its height ([`cadcraft_engine::Session::point_z`]).

use std::sync::{Arc, Weak};

use cadcraft_doc::{Drawing, Entity, EntityKind, Prim, Solid3d, Space};
use cadcraft_engine::Input;
use cadcraft_engine::pointmod::Frame;
use cadcraft_engine::snap::{SnapHit, mode};
use cadcraft_geom::{Bounds2, Segment, Vec2, Vec3};
use egui::Pos2;

use super::Xf;
use crate::CadApp;

/// The object snaps found in 3D.
const MODES: u32 = mode::END | mode::MID | mode::CEN | mode::QUA | ALONG;

/// The snaps found along objects rather than at their fixed points.
const ALONG: u32 = mode::NEA | mode::PER | mode::INT | mode::APP;

/// At most this many curves, and pieces of them, near the cursor are looked at for crossings
/// (crossings pair the pieces up: a zoomed-out view of meshed solids puts thousands under it).
/// ponytail: the pieces past the cap are left out, nearest-first order not kept; decimate solid
/// edges to the screen tolerance if a crossing goes missing in a crowded view.
const MAX_NEAR: usize = 200;
const MAX_SEGS: usize = 1000;

/// A closed edge is a circle when all its points are this close (relative to the radius) to the
/// circle through three of them.
const ROUND: f64 = 1e-4;

/// The circle a curve is on, in the world: centre, unit normal, radius.
type Round = (Vec3, Vec3, f64);

/// The snap points of solid `s` in the world for snap modes `modes`, with their mode: the ends of
/// its edges, the points halfway along open edges, and the centres of closed edges that are
/// circles. Each polyline of `mesh.edges` is one B-rep edge (the kernel's tessellation emits them
/// so).
/// ponytail: END, MID and CEN only; CEN only for whole circles (an arc edge has no centre) and a
/// closed edge has no midpoint; add arc centres (and QUA, TAN …) from the B-rep's curves when
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
            && let Some((c, ..)) = circle_of(&w)
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

/// The circle closed polyline `w` (its last point repeating the first) is on, if it is a circle:
/// the one through three points a third of the way round from each other, which every point is
/// on.
fn circle_of(w: &[Vec3]) -> Option<Round> {
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
    (radius > 0.0 && c.is_finite() && w.iter().all(on)).then_some((c, normal, radius))
}

/// Point `p` (near the circle `round`, if any) moved onto it: a point found on a chord of a
/// tessellated arc or circle, put on the curve itself.
fn onto(round: Option<Round>, p: Vec3) -> Vec3 {
    let Some((c, n, r)) = round else { return p };
    let d = p - c;
    let d = d - n * d.dot(n);
    let l = d.len();
    let q = c + d * (r / l);
    if l > 1e-12 * r && q.is_finite() { q } else { p }
}

/// Where the line through `a` and `b` meets circle `round`, the meeting nearest `near`: where it
/// passes through the circle's plane, or (a line in that plane) at the radius from the centre.
fn line_round(a: Vec3, b: Vec3, (c, n, r): Round, near: Vec3) -> Option<Vec3> {
    let d = b - a;
    let (dn, dd) = (d.dot(n), d.dot(d));
    if !(dd > 0.0 && dd.is_finite()) {
        return None;
    }
    if dn.abs() > 1e-9 * dd.sqrt() {
        return Some(a + d * ((c - a).dot(n) / dn));
    }
    // |a + d s - c|² = r²
    let f = a - c;
    let (h, k) = (f.dot(d) / dd, (f.dot(f) - r * r) / dd);
    let disc = h * h - k;
    if disc < 0.0 {
        return None;
    }
    let s = disc.sqrt();
    [-h - s, -h + s].into_iter().map(|s| a + d * s).min_by(|p, q| (*p - near).len().total_cmp(&(*q - near).len()))
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
        EntityKind::PolyMesh(m) => m.vertices.clone(),
        EntityKind::Ray(r) | EntityKind::XLine(r) => vec![r.base, r.base + r.dir],
        EntityKind::Helix(h) => vec![h.center, h.start_point(), h.end_point()],
        EntityKind::Solid3d(s) => s.corners(),
        k => k.grips().into_iter().map(|g| g.to3(0.0)).collect(),
    }
}

/// The snap points of object `e` in the world for snap modes `modes`, with their mode, into
/// `out`: a solid's ([`candidates`]); the ends and midpoints of lines, polyline segments and 3D
/// polyline segments; the centres of arcs, circles and ellipses and the quadrants of circles and
/// whole ellipses; the ends of open curves. Other objects have none here. The snaps along objects
/// (NEA, PER, INT, APP) are [`curves_of`]'s.
/// ponytail: no TAN, NOD, INS, GCEN, EXT or PAR in 3D (the engine's still work on the CPlane);
/// add them from the objects' 3D geometry when users ask.
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
        // A mesh's vertices are its ends.
        EntityKind::PolyMesh(m) => {
            for q in &m.vertices {
                add(*q, mode::END);
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
                if let Segment::Arc { arc, .. } = seg {
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

/// The curves of object `e` in the world, each as its points, the circle it is on (an arc or
/// circle, put back on it after a chord) and whether its points are a tessellation: lines, 3D
/// polyline segments, solid edges and the pieces of planar curves. A planar curve whose box on
/// screen is farther than `ap` (view units) from `cur` (a view-plane point) is left out, and
/// `tol` is the chord tolerance (world units).
fn curves_of(e: &Entity, xf: &Xf, cur: Vec2, ap: f64, tol: f64, f: &mut dyn FnMut(&[Vec3], Option<Round>, bool)) {
    match &e.kind {
        EntityKind::Solid3d(s) => {
            for edge in &s.mesh.edges {
                let w: Vec<Vec3> = edge.iter().map(|p| s.world(*p)).filter(|p| p.is_finite()).collect();
                let closed =
                    w.len() > 3 && w.first().zip(w.last()).is_some_and(|(a, b)| (*b - *a).len() <= 1e-9 * w.windows(2).map(seg_len).sum::<f64>());
                f(&w, if closed { circle_of(&w) } else { None }, w.len() > 2);
            }
        }
        EntityKind::Line(l) => f(&[l.a, l.b], None, false),
        EntityKind::PolyMesh(m) => {
            for w in m.lines().iter().flat_map(|l| l.windows(2)) {
                f(w, None, false);
            }
        }
        EntityKind::Polyline3d(p) => {
            let n = p.points.len();
            for (i, q) in p.points.iter().enumerate() {
                let next = if i + 1 < n { p.points.get(i + 1) } else { p.points.first().filter(|_| p.closed && n > 2) };
                if let Some(r) = next {
                    f(&[*q, *r], None, false);
                }
            }
        }
        EntityKind::Circle(_) | EntityKind::Arc(_) | EntityKind::Ellipse(_) | EntityKind::LwPolyline(_) | EntityKind::Spline(_) => {
            let Some(pl) = e.plane() else { return };
            let lift = |q: Vec2| pl.apply(q.to3(0.0));
            let dir = xf.dir();
            let near = |b: Bounds2| Bounds2::from_points(b.corners().map(|q| dir.project(lift(q)))).expand(ap).contains(cur);
            let round = |c: Vec2, r: f64| (lift(c), pl.apply_vec(Vec3::Z).normalized(), (lift(c + Vec2::X * r) - lift(c)).len());
            let mut flat = Vec::new();
            for prim in e.kind.prims() {
                flat.clear();
                let (on, chord) = match &prim {
                    Prim::Seg(Segment::Line(l)) => {
                        flat.extend([l.a, l.b]);
                        (None, false)
                    }
                    Prim::Seg(seg @ Segment::Arc { arc, .. }) if near(seg.bounds()) => {
                        seg.tessellate(tol, &mut flat);
                        (Some(round(arc.center, arc.radius)), true)
                    }
                    Prim::Circle(c) if near(c.bounds()) => {
                        c.tessellate(tol, &mut flat);
                        (Some(round(c.center, c.radius)), true)
                    }
                    Prim::Ellipse(el) if near(el.bounds()) => {
                        el.tessellate(tol, &mut flat);
                        (None, true)
                    }
                    // The control polygon's box holds the curve.
                    Prim::Spline(sp) if near(Bounds2::from_points(sp.control.iter().copied())) => {
                        flat = sp.tessellate(tol);
                        (None, true)
                    }
                    _ => continue,
                };
                let w: Vec<Vec3> = flat.iter().map(|q| lift(*q)).collect();
                f(&w, on, chord);
            }
        }
        _ => {}
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

/// How good a snap `d` points from the cursor is (lower is better), as the engine ranks them: an
/// intersection or an end a little ahead of the rest.
fn score(m: u32, d: f64, aperture: f64) -> f64 {
    d - if m == mode::INT || m == mode::END { aperture * 0.15 } else { 0.0 }
}

/// A snap found in 3D: its [`score`], how near the viewer it is, the world point and its mode.
#[derive(Clone, Copy, Debug)]
pub(super) struct Cand {
    score: f64,
    depth: f64,
    p: Vec3,
    m: u32,
}

/// The pieces of one curve passing within the aperture of the cursor on screen, and their box
/// in the view plane.
struct Near {
    segs: Vec<[Vec3; 2]>,
    round: Option<Round>,
    chord: bool,
    bx: Bounds2,
}

/// Every snap of modes `modes` on the objects `xf` shows (top level, visible) within `aperture`
/// points of screen point `at`: their fixed points ([`snaps_of`]) and, along the curves passing
/// there ([`curves_of`]), the nearest point, the perpendicular from `base` (world) and where two
/// cross on screen: an intersection where they meet, an apparent intersection on the nearer one
/// where one passes behind the other.
/// ponytail: scans every object each time the cursor moves at a point prompt (a layer is looked
/// up only for a point within the aperture, a planar curve tessellated only near the cursor);
/// keep the candidates with the display list if large drawings lag. Objects in block references
/// aren't snapped to, and APP doesn't extend objects to where they would cross.
pub(super) fn candidates_at(doc: &Drawing, space: &Space, xf: &Xf, at: Pos2, aperture: f64, modes: u32, base: Option<Vec3>) -> Vec<Cand> {
    let (dir, cur) = (xf.dir(), xf.to_view(at));
    let (vx, vy, _) = dir.axes();
    let view = |p: Vec3| Vec2::new(vx.dot(p), vy.dot(p));
    // View units: the view plane is the world's scale.
    let (ap, tol) = (aperture / xf.scale, 0.25 / xf.scale);
    let mut out = Vec::new();
    let mut push = |p: Vec3, m: u32| {
        let d = view(p).dist(cur);
        if p.is_finite() && d <= ap {
            out.push(Cand { score: score(m, d * xf.scale, aperture), depth: dir.depth(p), p, m });
        }
    };
    let Some(objects) = doc.space(space) else { return Vec::new() };
    let mut near: Vec<Near> = Vec::new();
    let mut segs_near = 0;
    let mut pts = Vec::new();
    for e in objects.iter() {
        pts.clear();
        snaps_of(e, modes, &mut pts);
        // The visibility check looks the layer up by name: only for an object with a snap here.
        let mut visible = None;
        let mut shown = || *visible.get_or_insert_with(|| doc.is_visible(e));
        for &(p, m) in &pts {
            if view(p).dist(cur) <= ap && shown() {
                push(p, m);
            }
        }
        if modes & ALONG == 0 || near.len() >= MAX_NEAR || segs_near >= MAX_SEGS {
            continue;
        }
        let mut found = Vec::new();
        let mut budget = MAX_SEGS.saturating_sub(segs_near);
        curves_of(e, xf, cur, ap, tol, &mut |w, round, chord| {
            let segs: Vec<[Vec3; 2]> = w
                .windows(2)
                .filter_map(|s| match s {
                    [a, b] if cadcraft_geom::Line::new(view(*a), view(*b)).dist(cur) <= ap => Some([*a, *b]),
                    _ => None,
                })
                .take(budget)
                .collect();
            budget -= segs.len();
            if !segs.is_empty() {
                let bx = Bounds2::from_points(segs.iter().flatten().map(|p| view(*p))).expand(tol);
                found.push(Near { segs, round, chord, bx });
            }
        });
        if !found.is_empty() && shown() {
            segs_near += found.iter().map(|n| n.segs.len()).sum::<usize>();
            near.extend(found);
        }
    }
    near.truncate(MAX_NEAR);
    if modes & mode::PER != 0
        && let Some(b) = base
    {
        for n in &near {
            match n.round {
                // The points of a circle square to the base: nearest and farthest from it.
                Some((c, nn, r)) => {
                    let d = b - c;
                    let d = d - nn * d.dot(nn);
                    // A base on the circle's axis is square to every point of it.
                    if d.len() > 1e-12 * r {
                        for s in [r, -r] {
                            push(c + d.normalized() * s, mode::PER);
                        }
                    }
                }
                None => {
                    for &[a, q] in &n.segs {
                        let (ab, l2) = (q - a, (q - a).dot(q - a));
                        let t = if l2 > 0.0 { (b - a).dot(ab) / l2 } else { continue };
                        // A line's perpendicular may land on its extension; a chord's may not.
                        if !n.chord || (0.0..=1.0).contains(&t) {
                            push(a + ab * t, mode::PER);
                        }
                    }
                }
            }
        }
    }
    if modes & (mode::INT | mode::APP) != 0 {
        let real = if modes & mode::INT != 0 { mode::INT } else { mode::APP };
        for (i, ni) in near.iter().enumerate() {
            // Curves whose pieces don't overlap on screen can't cross there.
            for nj in near.iter().skip(i + 1).filter(|nj| ni.bx.intersects(&nj.bx)) {
                for &[a1, a2] in &ni.segs {
                    for &[b1, b2] in &nj.segs {
                        let Some((_, t, u)) = cadcraft_geom::line_line_infinite(view(a1), view(a2), view(b1), view(b2)) else { continue };
                        let on = |t: f64| (-1e-9..=1.0 + 1e-9).contains(&t);
                        if !(on(t) && on(u)) {
                            continue;
                        }
                        let (p, q) = (a1 + (a2 - a1) * t, b1 + (b2 - b1) * u);
                        // Chords stand off their curves by up to the tolerance each.
                        let slack = |n: &Near| if n.chord { tol + n.round.map_or(0.0, |r| r.2 * 1e-6) } else { 0.0 };
                        let meet = slack(ni) + slack(nj) + 1e-9 * (1.0 + p.len().max(q.len()));
                        if (p - q).len() <= meet {
                            // A line meets a circle exactly where it is; other chords halfway.
                            let mid = (p + q) * 0.5;
                            let exact = match (ni.round, nj.round) {
                                (None, Some(r)) if !ni.chord => line_round(a1, a2, r, mid),
                                (Some(r), None) if !nj.chord => line_round(b1, b2, r, mid),
                                _ => None,
                            };
                            push(exact.filter(|x| (*x - mid).len() <= 2.0 * meet).unwrap_or(mid), real);
                        } else if modes & mode::APP != 0 {
                            push(if dir.depth(p) >= dir.depth(q) { onto(ni.round, p) } else { onto(nj.round, q) }, mode::APP);
                        }
                    }
                }
            }
        }
    }
    if modes & mode::NEA != 0 {
        for n in &near {
            let closest = n.segs.iter().map(|&[a, b]| a + (b - a) * cadcraft_geom::Line::new(view(a), view(b)).param_of(cur).clamp(0.0, 1.0));
            if let Some(p) = closest.min_by(|p, q| view(*p).dist(cur).total_cmp(&view(*q).dist(cur))) {
                push(onto(n.round, p), mode::NEA);
            }
        }
    }
    out
}

/// The best of `cands`: the lowest [`score`], of two alike the nearer to the viewer; a nearest
/// point only where no other snap is (as the engine).
fn best(cands: &[Cand]) -> Option<(Vec3, u32)> {
    let pick =
        |nea: bool| cands.iter().filter(|c| (c.m == mode::NEA) == nea).min_by(|a, b| a.score.total_cmp(&b.score).then(b.depth.total_cmp(&a.depth)));
    pick(false).or_else(|| pick(true)).map(|c| (c.p, c.m))
}

/// The snap of the objects `xf` shows nearest screen point `at` within `aperture` points, and its
/// mode ([`candidates_at`], [`best`]).
pub(super) fn nearest(doc: &Drawing, space: &Space, xf: &Xf, at: Pos2, aperture: f64, modes: u32, base: Option<Vec3>) -> Option<(Vec3, u32)> {
    best(&candidates_at(doc, space, xf, at, aperture, modes, base))
}

/// The modes snapped to in 3D now: the override (END …), else the running snaps unless they are
/// off (F3).
pub(super) fn running(app: &CadApp) -> u32 {
    let osmode = app.session.snap_override().unwrap_or(app.session.settings.osmode);
    if osmode & mode::OFF != 0 { 0 } else { osmode & MODES }
}

/// The prompt's base point in the world (for PER), with the height it was given at when it is the
/// last point.
fn base_of(app: &CadApp, xf: &Xf) -> Option<Vec3> {
    let b = app.session.current_prompt()?.base?;
    let z = if b.near(app.session.last_point, 1e-9) { app.session.last_point_z.unwrap_or(0.0) } else { 0.0 };
    Some(xf.cplane().apply(b.to3(z)))
}

/// In a 3D view at a point prompt, snap the cursor at screen point `at` to the objects where they
/// are drawn (see [`nearest`]) with the running object snaps or the override: a snap better on
/// screen than the engine's (on the construction plane) replaces it, in CPlane coordinates with
/// its height in `canvas.snap_z`; a nearest point only where the engine has nothing better. Plan
/// views keep the engine's snaps; Tab cycling keeps what it picked ([`cycle_list`]).
pub(super) fn apply(app: &mut CadApp, xf: &Xf, at: Pos2) {
    if app.canvas.snap_cycle.is_some() && app.canvas.snap.is_some() {
        return;
    }
    app.canvas.snap_z = None;
    let wants_point = app.session.current_prompt().is_some_and(|p| cadcraft_engine::pointmod::accepts(&p));
    if xf.is_plan() || !wants_point || app.session.pending_window.is_some() {
        return;
    }
    let modes = running(app);
    if modes == 0 {
        return;
    }
    let base = base_of(app, xf);
    let Ok(st) = app.session.state() else { return };
    let aperture = app.session.settings.aperture;
    let Some((p, m)) = nearest(&st.doc, &st.edit_space(), xf, at, aperture, modes, base) else { return };
    if let Some(h) = app.canvas.snap {
        let theirs = score(h.mode, f64::from(xf.to_screen(h.point).distance(at)), aperture);
        let keep = match (h.mode == mode::NEA, m == mode::NEA) {
            (false, true) => true,
            (true, false) => false,
            _ => theirs < score(m, f64::from(screen_of(xf, p).distance(at)), aperture),
        };
        if keep {
            return;
        }
    }
    set(app, xf, p, m);
}

/// Snap the cursor to world point `p` (mode `m`): in CPlane coordinates, its height in
/// `canvas.snap_z`.
pub(super) fn set(app: &mut CadApp, xf: &Xf, p: Vec3, m: u32) {
    let local = xf.cplane().inverse().map_or(p, |ci| ci.apply(p));
    let name = mode::ALL.iter().find(|(b, _)| *b == m).map_or("Endpoint", |(_, n)| *n);
    app.canvas.snap = Some(SnapHit { point: local.xy(), mode: m, name, viewport: None, deferred: None });
    app.canvas.snap_z = Some(local.z);
    app.canvas.track = None;
    app.canvas.cursor = Some(local.xy());
    app.session.cursor = local.xy();
    app.session.cursor_deferred = None;
}

/// A snap Tab can cycle to: the engine's (on the construction plane) or one found in 3D.
pub(super) enum Pick {
    Plane(SnapHit),
    World(Vec3, u32),
}

/// What Tab cycles through at screen point `at`: the engine's candidates `plane` and, in a 3D
/// view, every snap found where objects are drawn (a vertex hidden behind another included), best
/// first; a point found twice is listed once.
pub(super) fn cycle_list(app: &CadApp, xf: &Xf, at: Pos2, plane: Vec<SnapHit>) -> Vec<Pick> {
    let modes = running(app);
    let Ok(st) = app.session.state() else { return Vec::new() };
    if xf.is_plan() || modes == 0 {
        return plane.into_iter().map(Pick::Plane).collect();
    }
    let aperture = app.session.settings.aperture;
    let c = xf.cplane();
    // (nearest last, score, nearer the viewer first, the pick)
    let mut all: Vec<(bool, f64, f64, Vec3, Pick)> = plane
        .into_iter()
        .map(|h| {
            let w = c.apply(h.point.to3(0.0));
            (h.mode == mode::NEA, score(h.mode, f64::from(xf.to_screen(h.point).distance(at)), aperture), xf.dir().depth(w), w, Pick::Plane(h))
        })
        .collect();
    for k in candidates_at(&st.doc, &st.edit_space(), xf, at, aperture, modes, base_of(app, xf)) {
        let mode_of = |p: &Pick| match p {
            Pick::Plane(h) => h.mode,
            Pick::World(_, m) => *m,
        };
        if !all.iter().any(|(.., w, p)| mode_of(p) == k.m && (*w - k.p).len() <= 1e-9 * (1.0 + k.p.len())) {
            all.push((k.m == mode::NEA, k.score, k.depth, k.p, Pick::World(k.p, k.m)));
        }
    }
    all.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(b.2.total_cmp(&a.2)));
    all.into_iter().map(|(.., p)| p).collect()
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
