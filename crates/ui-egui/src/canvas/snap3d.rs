//! Object snaps in 3D views: on solids (the ends and midpoints of their edges, the centres of
//! their circular edges) and on curves (lines, polylines, 3D polylines, arcs, circles, ellipses
//! and splines: ends, midpoints, centres and quadrants), where they are in the world; along them
//! the nearest point, the perpendicular and the tangents from the base point, and where two of
//! them cross on screen (an intersection where they meet in the world, an apparent intersection
//! where one passes behind the other); nodes, insertion points and geometric centres of raised
//! objects; extensions of line ends and parallels to lines the cursor rested on. With the 3D
//! object snaps on (F4, 3DOSMODE) also AutoCAD's Vertex, Midpoint on edge, Center of face, Knot,
//! Perpendicular (to a face) and Nearest to face. The engine snaps on the construction plane,
//! where a raised object has only its shadow; in a 3D view the canvas finds these points on
//! screen and gives the command the point in CPlane coordinates with its height
//! ([`cadcraft_engine::Session::point_z`]), or on the plane with OSNAPZ.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use cadcraft_doc::{DimKind, Drawing, Entity, EntityKind, Prim, Solid3d, Space};
use cadcraft_engine::Input;
use cadcraft_engine::cmd::osnap3d::mode3d;
use cadcraft_engine::kernel::{Placed, SurfaceKind, Topology};
use cadcraft_engine::pointmod::Frame;
use cadcraft_engine::snap::{SnapHit, mode};
use cadcraft_geom::{Bounds2, Mat4, Segment, Vec2, Vec3, ViewDir};
use egui::Pos2;

use super::Xf;
use crate::CadApp;

/// The object snaps found in 3D.
const MODES: u32 = mode::END | mode::MID | mode::CEN | mode::QUA | mode::NOD | mode::INS | mode::GCEN | ALONG | PATHS;

/// The snaps found along objects rather than at their fixed points.
const ALONG: u32 = mode::NEA | mode::PER | mode::INT | mode::APP | mode::TAN;

/// The snaps along paths from what the cursor rested on (acquired): a line end's extension, a
/// parallel to a line through the base point.
const PATHS: u32 = mode::EXT | mode::PAR;

/// Marks a 3D object snap (3DOSMODE) in a snap's mode; the rest is the object snap whose marker
/// it shows.
const Z3D: u32 = 1 << 24;

/// The snap mode of 3DOSMODE bit `bit`: [`Z3D`] and the object snap whose marker it shows.
fn zmode(bit: u32) -> u32 {
    Z3D | mode3d::ALL.iter().find(|(b, ..)| *b == bit).map_or(0, |(.., m)| *m)
}

/// The marker a snap of mode `m` shows.
fn marker(m: u32) -> u32 {
    m & !Z3D
}

/// The name of snap mode `m` (the tooltip's).
fn name_of(m: u32) -> &'static str {
    if m & Z3D != 0 {
        return mode3d::ALL.iter().find(|(.., k)| *k == marker(m)).map_or("Vertex", |(_, n, ..)| *n);
    }
    mode::ALL.iter().find(|(b, _)| *b == m).map_or("Endpoint", |(_, n)| *n)
}

/// At most this many curves, and pieces of them, near the cursor are looked at for crossings
/// (crossings pair the pieces up: a zoomed-out view of meshed solids puts thousands under it).
/// ponytail: the pieces past the cap are left out, nearest-first order not kept; decimate solid
/// edges to the screen tolerance if a crossing goes missing in a crowded view.
const MAX_NEAR: usize = 200;
const MAX_SEGS: usize = 1000;

/// At most this many acquired line ends (EXT) and lines (PAR), newest first, as AutoCAD keeps.
const MAX_ACQUIRED: usize = 7;

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
/// whole ellipses; the ends of open curves; points and dimension definition points (NOD), the
/// insertion points AutoCAD's INS snaps to and the centroids of closed polylines and splines
/// (GCEN), each where it is in its plane. The snaps along objects (NEA, PER, TAN, INT, APP) are
/// [`curves_of`]'s.
pub(super) fn snaps_of(e: &Entity, modes: u32, out: &mut Vec<(Vec3, u32)>) {
    let mut add = |p: Vec3, m: u32| {
        if modes & m != 0 && p.is_finite() {
            out.push((p, m));
        }
    };
    if let EntityKind::Point(p) = &e.kind {
        add(p.p, mode::NOD);
    }
    if let Some(pl) = e.plane().filter(|_| modes & (mode::NOD | mode::INS | mode::GCEN) != 0) {
        let lift = |q: Vec2| pl.apply(q.to3(0.0));
        if let EntityKind::Dimension(dm) = &e.kind {
            for q in dim_points(dm) {
                add(lift(q), mode::NOD);
            }
        }
        for q in insertion_points(&e.kind) {
            add(lift(q), mode::INS);
        }
        if modes & mode::GCEN != 0
            && let Some(c) = gcen(&e.kind)
        {
            add(lift(c), mode::GCEN);
        }
    }
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

/// The definition points a dimension's kind uses (Node snaps to them), in its plane.
fn dim_points(dm: &cadcraft_doc::Dimension) -> Vec<Vec2> {
    let pts = match dm.kind {
        DimKind::Linear { .. } | DimKind::Aligned | DimKind::ArcLength => vec![dm.defpt, dm.p13, dm.p14],
        DimKind::Radius | DimKind::Diameter => vec![dm.defpt, dm.p15],
        DimKind::Angular3P => vec![dm.defpt, dm.p13, dm.p14, dm.p15],
        DimKind::Angular => vec![dm.defpt, dm.p13, dm.p14, dm.p15, dm.p16],
        DimKind::Ordinate { .. } => vec![dm.p13, dm.p14],
    };
    pts.into_iter().map(|p| p.xy()).collect()
}

/// The points AutoCAD's Insertion snap takes on an object, in its plane: a text's alignment
/// point, a block reference's insertion point and its visible attributes', a multileader's text
/// or block content.
fn insertion_points(k: &EntityKind) -> Vec<Vec2> {
    match k {
        EntityKind::Text(t) => vec![t.justify_point()],
        EntityKind::MText(t) => vec![t.insert.xy()],
        EntityKind::Tolerance(t) => vec![t.insert.xy()],
        EntityKind::AttDef(a) => vec![a.text.justify_point()],
        EntityKind::Insert(i) => {
            std::iter::once(i.insert.xy()).chain(i.attribs.iter().filter(|a| !a.invisible).map(|a| a.text.justify_point())).collect()
        }
        EntityKind::MLeader(m) => m.text.iter().map(|t| t.insert.xy()).chain(m.block.iter().map(|b| b.insert.xy())).collect(),
        _ => Vec::new(),
    }
}

/// The centroid of the area a closed polyline or spline encloses, in its plane.
fn gcen(k: &EntityKind) -> Option<Vec2> {
    let pts = match k {
        EntityKind::LwPolyline(p) if p.closed => {
            let b = Bounds2::from_points(p.vertices.iter().map(|v| v.p));
            let size = b.width().hypot(b.height());
            cadcraft_geom::Polyline { vertices: p.vertices.clone(), closed: true }.tessellate((size * 1e-3).max(1e-9))
        }
        EntityKind::Spline(s) if s.closed => s.tessellate(1e-3),
        _ => return None,
    };
    let (mut a, mut c) = (0.0, Vec2::ZERO);
    for (p, q) in pts.iter().zip(pts.iter().cycle().skip(1)) {
        let cr = p.cross(*q);
        a += cr;
        c += (*p + *q) * cr;
    }
    let c = c / (3.0 * a);
    (a.abs() > 1e-12 && c.is_finite()).then_some(c)
}

/// The 3D object snaps (3DOSMODE bits `z`) of object `e` in the world, into `out`: a solid's
/// vertices, the midpoints of its edges and the centres of its flat faces (from the kernel's
/// topology once `topo` has it; until then the midpoints of its drawn edges); a 3D face's or
/// mesh's vertices, edge midpoints and face centres; a spline's control vertices and knots.
/// ponytail: no knots on a solid's spline edges; evaluate the B-rep's NURBS edges
/// (`EdgeCurve::Spline`) at their knots if users ask.
fn zsnaps_of(e: &Entity, z: u32, topo: Option<&Topos>, out: &mut Vec<(Vec3, u32)>) {
    let mut add = |p: Vec3, bit: u32| {
        if z & bit != 0 && p.is_finite() {
            out.push((p, zmode(bit)));
        }
    };
    // The centre of a face: the mean of its distinct corners.
    let centre = |ps: &[Vec3]| {
        let mut u: Vec<Vec3> = Vec::with_capacity(ps.len());
        for p in ps {
            if !u.iter().any(|q| (*q - *p).len() <= 1e-12 * (1.0 + p.len())) {
                u.push(*p);
            }
        }
        (!u.is_empty()).then(|| u.iter().fold(Vec3::ZERO, |a, b| a + *b) * (1.0 / u.len() as f64))
    };
    match &e.kind {
        EntityKind::Solid3d(s) => {
            for p in s.corners() {
                add(p, mode3d::VERTEX);
            }
            let t = if z & (mode3d::MIDPOINT | mode3d::CENTER) != 0 { topo.and_then(|t| t.get(s)) } else { None };
            match &t {
                Some(t) => {
                    for edge in t.edges.iter().filter(|e| !e.seam && !e.degenerate) {
                        add(s.world(edge.mid), mode3d::MIDPOINT);
                    }
                    for f in t.faces.iter().filter(|f| f.kind == SurfaceKind::Plane) {
                        add(s.world(f.centroid), mode3d::CENTER);
                    }
                }
                None => {
                    for (p, _) in candidates(s, mode::MID) {
                        add(p, mode3d::MIDPOINT);
                    }
                }
            }
        }
        EntityKind::Face3d(f) => {
            let c = f.corners;
            for (i, p) in c.iter().enumerate() {
                add(*p, mode3d::VERTEX);
                let q = c.get((i + 1) % 4).copied().unwrap_or(*p);
                if (q - *p).len() > 0.0 {
                    add(*p + (q - *p) * 0.5, mode3d::MIDPOINT);
                }
            }
            if let Some(m) = centre(&c) {
                add(m, mode3d::CENTER);
            }
        }
        EntityKind::PolyMesh(m) => {
            for p in &m.vertices {
                add(*p, mode3d::VERTEX);
            }
            for w in m.lines().iter().flat_map(|l| l.windows(2)) {
                if let [a, b] = w {
                    add(*a + (*b - *a) * 0.5, mode3d::MIDPOINT);
                }
            }
            if z & mode3d::CENTER != 0 {
                for (f, _) in m.faces4() {
                    if let Some(c) = centre(&f.iter().filter_map(|i| m.vertices.get(*i).copied()).collect::<Vec<_>>()) {
                        add(c, mode3d::CENTER);
                    }
                }
            }
        }
        EntityKind::Spline(sp) => {
            let Some(pl) = e.plane() else { return };
            let lift = |q: Vec2| pl.apply(q.to3(0.0));
            for q in &sp.control {
                add(lift(*q), mode3d::VERTEX);
            }
            if z & mode3d::KNOT != 0 {
                let (lo, hi) = sp.domain();
                let mut last = f64::NAN;
                for k in sp.knots.iter().copied().filter(|k| k.is_finite() && *k >= lo && *k <= hi).take(10_000) {
                    if k != last {
                        add(lift(sp.eval(k)), mode3d::KNOT);
                        last = k;
                    }
                }
            }
        }
        _ => {}
    }
}

/// Solid `s`'s box on screen (seen through `view`): its model box's corners, fewer to look at
/// than its vertices.
fn screen_box(s: &Solid3d, view: &dyn Fn(Vec3) -> Vec2) -> Bounds2 {
    let inf = f64::INFINITY;
    let (mut lo, mut hi) = (Vec3::new(inf, inf, inf), Vec3::new(-inf, -inf, -inf));
    for p in s.mesh.points().filter(|p| p.is_finite()) {
        lo = Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
        hi = Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
    }
    let pick = |i: u32, a: f64, b: f64| if i == 0 { a } else { b };
    Bounds2::from_points((0..8u32).map(|i| view(s.world(Vec3::new(pick(i & 1, lo.x, hi.x), pick(i & 2, lo.y, hi.y), pick(i & 4, lo.z, hi.z))))))
}

/// The triangles of object `e`'s faces in the world (a solid's, a mesh's, a 3D face's) when its
/// box on screen (seen through `view`) holds `cur`.
/// ponytail: the box is of its corners' projections, approximate for an object reaching behind a
/// perspective view's eye; cut the box at the near plane if faces there fail to snap.
fn triangles(e: &Entity, view: &dyn Fn(Vec3) -> Vec2, cur: Vec2) -> Vec<[Vec3; 3]> {
    let under = |ps: &mut dyn Iterator<Item = Vec3>| Bounds2::from_points(ps.map(view)).contains(cur);
    match &e.kind {
        EntityKind::Solid3d(s) if screen_box(s, view).contains(cur) => {
            s.mesh.tris.iter().filter_map(|t| s.mesh.tri(t)).map(|t| t.map(|p| s.world(p))).collect()
        }
        EntityKind::PolyMesh(m) if under(&mut m.vertices.iter().copied()) => {
            let mm = m.mesh3();
            mm.tris.iter().filter_map(|t| mm.tri(t)).collect()
        }
        EntityKind::Face3d(f) if under(&mut f.corners.iter().copied()) => {
            let [a, b, c, d] = f.corners;
            vec![[a, b, c], [a, c, d]]
        }
        _ => Vec::new(),
    }
}

/// A face under the cursor: the point there, how near the viewer, the face's unit normal and the
/// object's triangles in its plane (the face, for a flat one).
struct FaceHit {
    p: Vec3,
    depth: f64,
    n: Vec3,
    plane: Vec<[Vec3; 3]>,
}

/// Where the line of sight under view point `cur` meets triangles `tris` (seen along `dir`, in
/// front of the eye in perspective), nearest the viewer.
fn face_hit(tris: &[[Vec3; 3]], dir: &ViewDir, cur: Vec2) -> Option<FaceHit> {
    let (o, d) = dir.ray(cur);
    let mut best: Option<(f64, Vec3, [Vec3; 3])> = None;
    for t in tris {
        let [a, b, c] = *t;
        // The ray against the triangle (Möller–Trumbore).
        let (e1, e2) = (b - a, c - a);
        let pv = d.cross(e2);
        let det = e1.dot(pv);
        // Seen edge-on, it covers nothing.
        if !(det.abs() > 1e-12 * e1.len() * e2.len() && det.is_finite()) {
            continue;
        }
        let tv = o - a;
        let u = tv.dot(pv) / det;
        let qv = tv.cross(e1);
        let v = d.dot(qv) / det;
        if u < -1e-9 || v < -1e-9 || u + v > 1.0 + 1e-9 {
            continue;
        }
        let p = a + e1 * u + e2 * v;
        let depth = dir.depth(p);
        if p.is_finite() && dir.sees(p) && best.as_ref().is_none_or(|(k, ..)| depth > *k) {
            best = Some((depth, p, *t));
        }
    }
    let (depth, p, [a, b, c]) = best?;
    let n = (b - a).cross(c - a).normalized();
    if n == Vec3::ZERO {
        return None;
    }
    let size = (b - a).len().max((c - a).len());
    let coplanar = |t: &&[Vec3; 3]| {
        let m = (t[1] - t[0]).cross(t[2] - t[0]).normalized();
        m.cross(n).len() <= 1e-9 && t.iter().all(|q| (*q - a).dot(n).abs() <= 1e-9 * (1.0 + size))
    };
    Some(FaceHit { p, depth, n, plane: tris.iter().filter(coplanar).copied().collect() })
}

/// Whether point `p` (in the plane of triangles `tris`) is on one of them.
fn on_face(tris: &[[Vec3; 3]], p: Vec3) -> bool {
    tris.iter().any(|[a, b, c]| {
        let n = (*b - *a).cross(*c - *a);
        [(*a, *b), (*b, *c), (*c, *a)].iter().all(|(u, v)| (*v - *u).cross(p - *u).dot(n) >= -1e-9 * n.len() * (*v - *u).len())
    })
}

/// The faces and edges of solids, found by the kernel in the background (each call starts a
/// worker process: too slow to wait for under the cursor), by B-rep text.
#[derive(Clone, Default)]
pub(crate) struct Topos(Arc<Mutex<HashMap<usize, Topo>>>);

/// A solid's topology: asked for, then found (or not).
struct Topo {
    /// The B-rep text it is for (its address is the key; held weakly, so the address can't be
    /// reused while the entry lasts).
    brep: Weak<str>,
    /// `None` while the kernel works; then what it found (`None`: nothing, as without a kernel).
    found: Option<Option<Arc<Topology>>>,
}

/// At most this many topologies are asked for at once.
const MAX_PENDING: usize = 4;

impl Topos {
    /// Solid `s`'s faces and edges (in model coordinates) once found; until then `None`, having
    /// asked the kernel for them in the background.
    fn get(&self, s: &Solid3d) -> Option<Arc<Topology>> {
        if s.brep.is_empty() {
            return None;
        }
        let key = Arc::as_ptr(&s.brep).cast::<u8>().addr();
        let me = Arc::downgrade(&s.brep);
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(t) = map.get(&key)
            && Weak::ptr_eq(&t.brep, &me)
        {
            return t.found.clone().flatten();
        }
        // Forget solids that are gone (a weak reference keeps their text's memory).
        map.retain(|_, t| t.brep.strong_count() > 0);
        if map.values().filter(|t| t.found.is_none()).count() >= MAX_PENDING {
            return None;
        }
        map.insert(key, Topo { brep: me, found: None });
        drop(map);
        let (all, brep) = (self.clone(), Arc::clone(&s.brep));
        let spawned = std::thread::Builder::new().name("snap topology".into()).spawn(move || {
            let t = cadcraft_engine::kernel::topology(Placed { brep: &brep, xform: &Mat4::IDENTITY }).ok().map(Arc::new);
            all.found(key, &brep, t);
        });
        if spawned.is_err() {
            self.found(key, &s.brep, None);
        }
        None
    }

    fn found(&self, key: usize, brep: &Arc<str>, t: Option<Arc<Topology>>) {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(e) = map.get_mut(&key)
            && Weak::ptr_eq(&e.brep, &Arc::downgrade(brep))
        {
            e.found = Some(t);
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
/// It also keeps what the 3D snaps remember between cursor moves: the solids' topologies, and the
/// line ends (EXT) and lines (PAR) the cursor rested on.
#[derive(Default)]
pub(crate) struct Ground {
    of: Weak<Drawing>,
    /// `None`: no object is off the plane (the engine's own drawing will do).
    doc: Option<Arc<Drawing>>,
    topo: Topos,
    /// Acquired line ends with the way their lines run on past them, newest first.
    ext: Vec<(Vec3, Vec3)>,
    /// Acquired lines' unit directions, newest first.
    par: Vec<Vec3>,
}

/// What the 3D snaps look at besides the running object snaps.
#[derive(Default)]
pub(super) struct Extra<'a> {
    /// The 3D object snap modes (3DOSMODE bits) when they are on (F4), else 0.
    z: u32,
    /// Acquired line ends (EXT) and lines (PAR), as [`Ground`] keeps them.
    ext: &'a [(Vec3, Vec3)],
    par: &'a [Vec3],
    /// The solids' topologies found so far.
    topo: Option<&'a Topos>,
}

/// Remember the straight pieces `lines` under the cursor at view point `cur` (`ap` view units
/// round it): the ends there for EXT, the lines for PAR (when `par`), newest first.
fn acquire(g: &mut Ground, lines: &[[Vec3; 2]], view: &dyn Fn(Vec3) -> Vec2, cur: Vec2, ap: f64, par: bool) {
    for &[a, b] in lines {
        for (end, from) in [(a, b), (b, a)] {
            let way = (end - from).normalized();
            if view(end).dist(cur) <= ap && way != Vec3::ZERO {
                g.ext.retain(|(e, _)| (*e - end).len() > 1e-9 * (1.0 + end.len()));
                g.ext.insert(0, (end, way));
            }
        }
        let way = (b - a).normalized();
        let ends_near = view(a).dist(cur) <= ap || view(b).dist(cur) <= ap;
        if par && !ends_near && way != Vec3::ZERO {
            g.par.retain(|d| d.cross(way).len() > 1e-9);
            g.par.insert(0, way);
        }
    }
    g.ext.truncate(MAX_ACQUIRED);
    g.par.truncate(MAX_ACQUIRED);
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
    d - if marker(m) == mode::INT || marker(m) == mode::END { aperture * 0.15 } else { 0.0 }
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
/// points of screen point `at` ([`gather`] without the 3D object snaps and acquired paths).
pub(super) fn candidates_at(doc: &Drawing, space: &Space, xf: &Xf, at: Pos2, aperture: f64, modes: u32, base: Option<Vec3>) -> Vec<Cand> {
    gather(doc, space, xf, at, aperture, modes, base, &Extra::default()).0
}

/// Every snap of modes `modes` on the objects `xf` shows (top level, visible) within `aperture`
/// points of screen point `at`: their fixed points ([`snaps_of`], and with `x.z` the 3D object
/// snaps' [`zsnaps_of`]); along the curves passing there ([`curves_of`]) the nearest point, the
/// perpendicular and the tangents from `base` (world) and where two cross on screen (an
/// intersection where they meet, an apparent intersection on the nearer one where one passes
/// behind the other); along the extensions of the acquired line ends and the parallels through
/// `base` to the acquired lines; on the face under the cursor nearest the viewer, the point there
/// and the foot of the perpendicular from `base`. Also the straight pieces under the cursor (to
/// acquire).
/// ponytail: scans every object each time the cursor moves at a point prompt (a layer is looked
/// up only for a point within the aperture, a planar curve tessellated only near the cursor, a
/// solid's faces only under it); keep the candidates with the display list if large drawings lag.
/// Objects in block references aren't snapped to, APP doesn't extend objects to where they would
/// cross, and only straight ends extend (no arc extensions in 3D); EXT and PAR show their marker
/// but no dotted path (the tracking paths are drawn on the CPlane): draw it in 3D if users ask.
#[allow(clippy::too_many_arguments)]
pub(super) fn gather(
    doc: &Drawing,
    space: &Space,
    xf: &Xf,
    at: Pos2,
    aperture: f64,
    modes: u32,
    base: Option<Vec3>,
    x: &Extra,
) -> (Vec<Cand>, Vec<[Vec3; 2]>) {
    let (dir, cur) = (xf.dir(), xf.to_view(at));
    // Where the view shows a point (in perspective, through the eye: only what is in front of
    // it is hit-tested, as only that is drawn).
    let view = |p: Vec3| dir.project(p);
    // View units: the view plane is the world's scale.
    let (ap, tol) = (aperture / xf.scale, 0.25 / xf.scale);
    let mut out = Vec::new();
    let mut push = |p: Vec3, m: u32| {
        let d = view(p).dist(cur);
        if p.is_finite() && dir.sees(p) && d <= ap {
            out.push(Cand { score: score(m, d * xf.scale, aperture), depth: dir.depth(p), p, m });
        }
    };
    let Some(objects) = doc.space(space) else { return (Vec::new(), Vec::new()) };
    let mut near: Vec<Near> = Vec::new();
    let mut segs_near = 0;
    let mut pts = Vec::new();
    let faces = x.z & (mode3d::NEAREST | mode3d::PERPENDICULAR) != 0;
    let mut face: Option<FaceHit> = None;
    for e in objects.iter() {
        pts.clear();
        snaps_of(e, modes, &mut pts);
        if x.z != 0 {
            // A solid's topology is asked for only once the cursor is near it.
            let topo = x.topo.filter(|_| matches!(&e.kind, EntityKind::Solid3d(s) if screen_box(s, &view).expand(ap).contains(cur)));
            zsnaps_of(e, x.z, topo, &mut pts);
        }
        // The visibility check looks the layer up by name: only for an object with a snap here.
        let mut visible = None;
        let mut shown = || *visible.get_or_insert_with(|| doc.is_visible(e));
        for &(p, m) in &pts {
            if dir.sees(p) && view(p).dist(cur) <= ap && shown() {
                push(p, m);
            }
        }
        if faces
            && let Some(h) = face_hit(&triangles(e, &view, cur), &dir, cur)
            && face.as_ref().is_none_or(|f| h.depth > f.depth)
            && shown()
        {
            face = Some(h);
        }
        if modes & (ALONG | PATHS) == 0 || near.len() >= MAX_NEAR || segs_near >= MAX_SEGS {
            continue;
        }
        let mut found = Vec::new();
        let mut budget = MAX_SEGS.saturating_sub(segs_near);
        curves_of(e, xf, cur, ap, tol, &mut |w, round, chord| {
            // In perspective each piece is cut at the near plane first: what is behind the eye
            // isn't drawn.
            let segs: Vec<[Vec3; 2]> = w
                .windows(2)
                .filter_map(|s| match s {
                    [a, b] => dir.clip(*a, *b).map(|(a, b)| [a, b]),
                    _ => None,
                })
                .filter(|[a, b]| cadcraft_geom::Line::new(view(*a), view(*b)).dist(cur) <= ap)
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
    if modes & mode::TAN != 0
        && let Some(b) = base
    {
        // From the base point (seen in each circle's plane) to the circle, square to its radius.
        for n in &near {
            let Some((c, nn, r)) = n.round else { continue };
            let d = b - c;
            let d = d - nn * d.dot(nn);
            let l = d.len();
            // A base inside the circle (or on it) has no tangent.
            if l.is_nan() || l <= r * (1.0 + 1e-9) {
                continue;
            }
            let (e1, cos) = (d * (1.0 / l), r / l);
            let (e2, sin) = (nn.cross(e1), (1.0 - cos * cos).max(0.0).sqrt());
            for s in [sin, -sin] {
                let p = c + (e1 * cos + e2 * s) * r;
                // On the arc, not only on its circle: by one of its pieces here.
                let on = |&[a, q]: &[Vec3; 2]| {
                    let ab = q - a;
                    let t = if ab.dot(ab) > 0.0 { ((p - a).dot(ab) / ab.dot(ab)).clamp(0.0, 1.0) } else { 0.0 };
                    (a + ab * t - p).len() <= 2.0 * tol + r * 1e-6
                };
                if n.segs.iter().any(on) {
                    push(p, mode::TAN);
                }
            }
        }
    }
    // Along the extension of each acquired line end, and parallel to each acquired line through
    // the base point: the point of the path nearest the cursor on screen. The path's picture is a
    // line; secant steps find the point of the path drawn at the picture's point nearest the
    // cursor (the first step is exact in a parallel view, a few more converge in perspective).
    let along = |o: Vec3, u: Vec3| {
        let (a, ab) = (view(o), view(o + u) - view(o));
        let l2 = ab.len2();
        if !(l2 > 1e-24 && l2.is_finite()) {
            return None;
        }
        let at = |t: f64| (view(o + u * t) - a).dot(ab) / l2;
        let want = (cur - a).dot(ab) / l2;
        let (mut t0, mut f0, mut t1, mut f1) = (0.0, -want, 1.0, 1.0 - want);
        for _ in 0..8 {
            let d = f1 - f0;
            if f1.abs() <= 1e-12 || d.abs() <= 1e-15 {
                break;
            }
            (t0, f0, t1) = (t1, f1, t1 - f1 * (t1 - t0) / d);
            f1 = at(t1) - want;
        }
        t1.is_finite().then_some((o + u * t1, t1))
    };
    if modes & mode::EXT != 0 {
        for (p, _) in x.ext.iter().filter_map(|(o, u)| along(*o, *u)).filter(|(_, t)| *t > 0.0) {
            push(p, mode::EXT);
        }
    }
    if modes & mode::PAR != 0
        && let Some(b) = base
    {
        for (p, _) in x.par.iter().filter_map(|u| along(b, *u)) {
            push(p, mode::PAR);
        }
    }
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
    if let Some(f) = &face {
        if x.z & mode3d::NEAREST != 0 {
            push(f.p, zmode(mode3d::NEAREST));
        }
        // The foot of the perpendicular from the base point to the face's plane, on the face: it
        // snaps while the cursor is over the face, wherever it is.
        if x.z & mode3d::PERPENDICULAR != 0
            && let Some(b) = base
        {
            let foot = b - f.n * (b - f.p).dot(f.n);
            if foot.is_finite() && on_face(&f.plane, foot) {
                let m = zmode(mode3d::PERPENDICULAR);
                out.push(Cand { score: score(m, aperture * 0.5, aperture), depth: dir.depth(foot), p: foot, m });
            }
        }
    }
    let lines = near.iter().filter(|n| !n.chord).flat_map(|n| n.segs.iter().copied()).collect();
    (out, lines)
}

/// The best of `cands`: the lowest [`score`], of two alike the nearer to the viewer; a nearest
/// point only where no other snap is (as the engine).
fn best(cands: &[Cand]) -> Option<(Vec3, u32)> {
    let pick = |nea: bool| {
        cands.iter().filter(|c| (marker(c.m) == mode::NEA) == nea).min_by(|a, b| a.score.total_cmp(&b.score).then(b.depth.total_cmp(&a.depth)))
    };
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

/// The 3D object snap modes (3DOSMODE) snapped to now: none while they are off (F4) or an
/// override (END …) asks for one object snap.
pub(super) fn running3d(app: &CadApp) -> u32 {
    let z = app.session.settings.osmode3d;
    if app.session.snap_override().is_some() || z & mode3d::OFF != 0 { 0 } else { z & mode3d::MODES }
}

/// The prompt's base point in the world (for PER), with the height it was given at when it is the
/// last point.
fn base_of(app: &CadApp, xf: &Xf) -> Option<Vec3> {
    let b = app.session.current_prompt()?.base?;
    let z = if b.near(app.session.last_point, 1e-9) { app.session.last_point_z.unwrap_or(0.0) } else { 0.0 };
    Some(xf.cplane().apply(b.to3(z)))
}

/// In a 3D view at a point prompt, snap the cursor at screen point `at` to the objects where they
/// are drawn (see [`gather`], [`best`]) with the running object snaps or the override and the 3D
/// object snaps: a snap better on screen than the engine's (on the construction plane) replaces
/// it, in CPlane coordinates with its height in `canvas.snap_z`; a nearest point only where the
/// engine has nothing better. The line ends and lines under the cursor are acquired for EXT and
/// PAR (until the point prompts end). Plan views keep the engine's snaps; Tab cycling keeps what
/// it picked ([`cycle_list`]).
pub(super) fn apply(app: &mut CadApp, xf: &Xf, at: Pos2) {
    if app.canvas.snap_cycle.is_some() && app.canvas.snap.is_some() {
        return;
    }
    app.canvas.snap_z = None;
    let wants_point = app.session.current_prompt().is_some_and(|p| cadcraft_engine::pointmod::accepts(&p));
    if !wants_point {
        app.canvas.ground.ext.clear();
        app.canvas.ground.par.clear();
    }
    if xf.is_plan() || !wants_point || app.session.pending_window.is_some() {
        return;
    }
    let (modes, z) = (running(app), running3d(app));
    if modes == 0 && z == 0 {
        return;
    }
    let base = base_of(app, xf);
    let Ok(st) = app.session.state() else { return };
    let aperture = app.session.settings.aperture;
    let g = &app.canvas.ground;
    let (ext, par, topo) = (g.ext.clone(), g.par.clone(), g.topo.clone());
    let x = Extra { z, ext: &ext, par: &par, topo: Some(&topo) };
    let (cands, lines) = gather(&st.doc, &st.edit_space(), xf, at, aperture, modes, base, &x);
    if modes & PATHS != 0 {
        let dir = xf.dir();
        let view = |p: Vec3| dir.project(p);
        acquire(&mut app.canvas.ground, &lines, &view, xf.to_view(at), aperture / xf.scale, modes & mode::PAR != 0 && base.is_some());
    }
    let Some((p, m)) = best(&cands) else { return };
    if let Some(h) = app.canvas.snap {
        let theirs = score(h.mode, f64::from(xf.to_screen(h.point).distance(at)), aperture);
        let keep = match (h.mode == mode::NEA, marker(m) == mode::NEA) {
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
/// `canvas.snap_z`; with OSNAPZ, the point of the construction plane under it (height 0).
pub(super) fn set(app: &mut CadApp, xf: &Xf, p: Vec3, m: u32) {
    let local = xf.cplane().inverse().map_or(p, |ci| ci.apply(p));
    app.canvas.snap = Some(SnapHit { point: local.xy(), mode: marker(m), name: name_of(m), viewport: None, deferred: None });
    app.canvas.snap_z = Some(if app.session.settings.osnapz { 0.0 } else { local.z });
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
    let (modes, z) = (running(app), running3d(app));
    let Ok(st) = app.session.state() else { return Vec::new() };
    if xf.is_plan() || (modes == 0 && z == 0) {
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
    let g = &app.canvas.ground;
    let x = Extra { z, ext: &g.ext, par: &g.par, topo: Some(&g.topo) };
    for k in gather(&st.doc, &st.edit_space(), xf, at, aperture, modes, base_of(app, xf), &x).0 {
        let mode_of = |p: &Pick| match p {
            Pick::Plane(h) => h.mode,
            Pick::World(_, m) => *m,
        };
        if !all.iter().any(|(.., w, p)| mode_of(p) == k.m && (*w - k.p).len() <= 1e-9 * (1.0 + k.p.len())) {
            all.push((marker(k.m) == mode::NEA, k.score, k.depth, k.p, Pick::World(k.p, k.m)));
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
        // A grip dragged in its object's own plane: the height it is at there.
        (None, _) => {
            app.session.point_z = app.canvas.grip_z;
            app.session.input(Input::Point(p))
        }
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::{Circle, Common, Face3d, Insert, LwPolyline, Point};
    use cadcraft_geom::{Mesh3, PolyVertex};
    use egui::{Rect, vec2};

    use super::*;

    fn g(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    /// Looking down from the front left, 5 points to a unit.
    fn xf() -> Xf {
        Xf::new(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0)), Vec2::ZERO, 5.0).looking(ViewDir::new(g(-1.0, -1.0, 1.0)))
    }

    fn drawing(kinds: Vec<EntityKind>) -> Drawing {
        let mut d = Drawing::new_metric();
        for k in kinds {
            d.add(&Space::Model, Common::default(), k).unwrap();
        }
        d
    }

    /// The snaps under world point `p` (10-point aperture).
    fn at(d: &Drawing, p: Vec3, modes: u32, base: Option<Vec3>, x: &Extra) -> Vec<Cand> {
        gather(d, &Space::Model, &xf(), screen_of(&xf(), p), 10.0, modes, base, x).0
    }

    /// Whether `c` has a snap of mode `m` at `p` (as near as the cursor's screen position, in
    /// whole f32 points, puts a point found under it).
    fn has(c: &[Cand], p: Vec3, m: u32) -> bool {
        c.iter().any(|k| k.m == m && (k.p - p).len() < 1e-4)
    }

    /// A cube from the origin to (s, s, s): its faces as triangles and its edges.
    fn cube(s: f64) -> Solid3d {
        let verts: Vec<Vec3> =
            (0..8).map(|i| g(if i & 1 == 0 { 0.0 } else { s }, if i & 2 == 0 { 0.0 } else { s }, if i & 4 == 0 { 0.0 } else { s })).collect();
        let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
        let tris = quads.iter().flat_map(|[a, b, c, d]| [[*a, *b, *c], [*a, *c, *d]]).collect();
        let edges = [(0, 1), (1, 3), (3, 2), (2, 0), (4, 5), (5, 7), (7, 6), (6, 4), (0, 4), (1, 5), (2, 6), (3, 7)]
            .iter()
            .map(|(a, b)| vec![verts[*a], verts[*b]])
            .collect();
        Solid3d { mesh: Arc::new(Mesh3 { verts, tris, edges, open: false }), ..Default::default() }
    }

    /// Raised objects snap where they are: a point's node, a block reference's insertion point and
    /// a closed polyline's centroid at their heights; the tangents from a base point above a
    /// raised circle touch it in its plane.
    #[test]
    fn raised_objects_snap_to_nodes_insertion_points_centroids_and_tangents() {
        let square = [(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)].map(|(x, y)| PolyVertex::new(Vec2::new(x, y)));
        let d = drawing(vec![
            EntityKind::Point(Point { p: g(1.0, 2.0, 7.0), angle: 0.0 }),
            EntityKind::Insert(Insert {
                block: "Nope".into(),
                insert: g(10.0, 0.0, 5.0),
                scale: g(1.0, 1.0, 1.0),
                rotation: 0.0,
                attribs: vec![],
                cols: 1,
                rows: 1,
                col_spacing: 0.0,
                row_spacing: 0.0,
            }),
            EntityKind::LwPolyline(LwPolyline { vertices: square.to_vec(), closed: true, const_width: 0.0, elevation: -6.0, plinegen: false }),
            EntityKind::Circle(Circle { center: g(-15.0, 0.0, 10.0), radius: 5.0 }),
        ]);
        let none = Extra::default();
        assert!(has(&at(&d, g(1.0, 2.0, 7.0), mode::NOD, None, &none), g(1.0, 2.0, 7.0), mode::NOD));
        assert!(has(&at(&d, g(10.0, 0.0, 5.0), mode::INS, None, &none), g(10.0, 0.0, 5.0), mode::INS));
        assert!(has(&at(&d, g(2.0, 2.0, -6.0), mode::GCEN, None, &none), g(2.0, 2.0, -6.0), mode::GCEN));
        // From (-5, 0) in the circle's plane (the base is 20 higher): 60° either side.
        let t = g(-15.0 + 2.5, 5.0 * 60f64.to_radians().sin(), 10.0);
        let c = at(&d, t, mode::TAN, Some(g(-5.0, 0.0, 30.0)), &none);
        assert!(c.iter().any(|k| k.m == mode::TAN && (k.p - t).len() < 1e-9), "{c:?}");
        assert!(at(&d, t, mode::TAN, None, &none).is_empty());
    }

    /// EXT snaps along an acquired line end's extension (past the end only); PAR along the
    /// parallel through the base point to an acquired line. The cursor resting on a line end
    /// acquires it; on the line away from its ends, the line.
    #[test]
    fn extensions_and_parallels_of_acquired_lines() {
        let d = drawing(vec![EntityKind::Line(cadcraft_doc::Line { a: g(0.0, 0.0, 10.0), b: g(10.0, 0.0, 10.0) })]);
        let ext = [(g(10.0, 0.0, 10.0), g(1.0, 0.0, 0.0))];
        let par = [g(1.0, 0.0, 0.0)];
        let x = Extra { ext: &ext, par: &par, ..Extra::default() };
        assert!(has(&at(&d, g(16.0, 0.0, 10.0), mode::EXT, None, &x), g(16.0, 0.0, 10.0), mode::EXT));
        assert!(!at(&d, g(-6.0, 0.0, 10.0), mode::EXT, None, &x).iter().any(|k| k.m == mode::EXT));
        let b = g(0.0, 8.0, 10.0);
        assert!(has(&at(&d, g(7.0, 8.0, 10.0), mode::PAR, Some(b), &x), g(7.0, 8.0, 10.0), mode::PAR));
        // Acquiring.
        let view = |p: Vec3| xf().dir().project(p);
        let (_, lines) = gather(&d, &Space::Model, &xf(), screen_of(&xf(), g(10.0, 0.0, 10.0)), 10.0, mode::EXT, None, &Extra::default());
        let mut gr = Ground::default();
        acquire(&mut gr, &lines, &view, view(g(10.0, 0.0, 10.0)), 2.0, true);
        assert_eq!(gr.ext.len(), 1);
        assert!((gr.ext[0].0 - g(10.0, 0.0, 10.0)).len() < 1e-9 && (gr.ext[0].1 - g(1.0, 0.0, 0.0)).len() < 1e-9, "{:?}", gr.ext);
        assert!(gr.par.is_empty(), "an end, not the line");
        acquire(&mut gr, &lines, &view, view(g(5.0, 0.0, 10.0)), 2.0, true);
        assert_eq!(gr.par.len(), 1);
    }

    /// The 3D object snaps: a solid's vertices and edge midpoints (from its drawn edges without the
    /// kernel); on the face under the cursor nearest the viewer, the point there and the foot of
    /// the perpendicular from the base point (only on the face); a 3D face's centre and edge
    /// midpoints; a raised spline's knots.
    #[test]
    fn the_3d_object_snaps() {
        let fit = [(0.0, 20.0), (5.0, 25.0), (10.0, 20.0), (15.0, 25.0), (20.0, 20.0), (25.0, 25.0)].map(|(x, y)| Vec2::new(x, y));
        let mut sp = cadcraft_geom::Spline::from_fit_points(&fit);
        sp.elevation = 5.0;
        let d = drawing(vec![
            EntityKind::Solid3d(cube(10.0)),
            EntityKind::Face3d(Face3d {
                corners: [g(-20.0, 0.0, 20.0), g(-16.0, 0.0, 20.0), g(-16.0, 4.0, 20.0), g(-20.0, 4.0, 20.0)],
                hidden_edges: 0,
            }),
            EntityKind::Spline(sp.clone()),
        ]);
        let x = Extra { z: mode3d::MODES, ..Extra::default() };
        let z = |bit| zmode(bit);
        assert!(has(&at(&d, g(10.0, 10.0, 10.0), 0, None, &x), g(10.0, 10.0, 10.0), z(mode3d::VERTEX)));
        assert!(has(&at(&d, g(5.0, 0.0, 10.0), 0, None, &x), g(5.0, 0.0, 10.0), z(mode3d::MIDPOINT)));
        // The top face, in front of the bottom one.
        assert!(has(&at(&d, g(3.0, 4.0, 10.0), 0, None, &x), g(3.0, 4.0, 10.0), z(mode3d::NEAREST)));
        let c = at(&d, g(7.0, 7.0, 10.0), 0, Some(g(3.0, 4.0, 30.0)), &x);
        assert!(has(&c, g(3.0, 4.0, 10.0), z(mode3d::PERPENDICULAR)), "{c:?}");
        let c = at(&d, g(7.0, 7.0, 10.0), 0, Some(g(30.0, 4.0, 30.0)), &x);
        assert!(!c.iter().any(|k| k.m == z(mode3d::PERPENDICULAR)), "off the face: {c:?}");
        assert!(has(&at(&d, g(-18.0, 2.0, 20.0), 0, None, &x), g(-18.0, 2.0, 20.0), z(mode3d::CENTER)));
        assert!(has(&at(&d, g(-18.0, 0.0, 20.0), 0, None, &x), g(-18.0, 0.0, 20.0), z(mode3d::MIDPOINT)));
        let (lo, hi) = sp.domain();
        let k = sp.knots.iter().copied().find(|k| *k > lo && *k < hi).unwrap_or(lo);
        let p = sp.eval(k).to3(5.0);
        assert!(has(&at(&d, p, 0, None, &x), p, z(mode3d::KNOT)));
        // Only the modes asked for; named as AutoCAD names them.
        let only = Extra { z: mode3d::VERTEX, ..Extra::default() };
        assert!(at(&d, g(-18.0, 2.0, 20.0), 0, None, &only).iter().all(|k| k.m == z(mode3d::VERTEX)));
        assert_eq!((name_of(z(mode3d::CENTER)), marker(z(mode3d::CENTER))), ("Center of face", mode::CEN));
        assert_eq!(name_of(mode::END), "Endpoint");
    }

    /// With the kernel, a solid's flat faces snap at their centres and its edges at their exact
    /// midpoints once its topology arrives from the background.
    #[test]
    fn solid_face_centres_come_from_the_kernel() {
        if !cadcraft_engine::kernel::available() {
            return;
        }
        let mut s = cadcraft_engine::Session::new();
        s.execute("box", &serde_json::json!({ "corner1": [0, 0, 0], "corner2": [10, 10, 10] })).unwrap();
        let e = s.doc().unwrap().model.iter().last().unwrap().as_ref().clone();
        let EntityKind::Solid3d(solid) = &e.kind else { panic!("{:?}", e.kind) };
        let topo = Topos::default();
        let start = std::time::Instant::now();
        while topo.get(solid).is_none() {
            assert!(start.elapsed().as_secs() < 120, "no topology");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let mut out = Vec::new();
        zsnaps_of(&e, mode3d::CENTER | mode3d::MIDPOINT, Some(&topo), &mut out);
        let has = |p: Vec3, bit: u32| out.iter().any(|(q, m)| *m == zmode(bit) && (*q - p).len() < 1e-6);
        assert!(has(g(5.0, 5.0, 10.0), mode3d::CENTER) && has(g(0.0, 5.0, 5.0), mode3d::CENTER), "{out:?}");
        assert!(has(g(5.0, 0.0, 10.0), mode3d::MIDPOINT), "{out:?}");
    }

    /// A 3D snap gives the command its point with its height, or with OSNAPZ the point of the
    /// construction plane under it; its tooltip names the 3D mode, its marker is the object snap's.
    #[test]
    fn osnapz_puts_the_snap_on_the_construction_plane() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        let xf = xf();
        set(&mut app, &xf, g(3.0, 4.0, 10.0), zmode(mode3d::CENTER));
        let hit = app.canvas.snap.unwrap();
        assert_eq!((hit.point, hit.mode, hit.name, app.canvas.snap_z), (Vec2::new(3.0, 4.0), mode::CEN, "Center of face", Some(10.0)));
        app.session.settings.osnapz = true;
        set(&mut app, &xf, g(3.0, 4.0, 10.0), mode::END);
        assert_eq!((app.canvas.snap.map(|h| h.point), app.canvas.snap_z), (Some(Vec2::new(3.0, 4.0)), Some(0.0)));
    }
}
