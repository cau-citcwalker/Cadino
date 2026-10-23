//! Faces and edges of 3D solids (AutoCAD's subobjects, picked with Ctrl+click there): picking
//! them at a prompt, naming them in JSON, and drawing them. FILLETEDGE, CHAMFEREDGE, SOLIDEDIT,
//! SHELL and PRESSPULL build on this.
//!
//! The kernel names faces and edges by index in one exact B-rep text ([`cadcraft_kernel::topology`]);
//! an edit makes new text, so a command asks again after each one. A click in a 3D view is the
//! eye's line through the cursor ([`Session::view_at`]); a click in plan is the line straight down
//! through it; a point with a height (typed `x,y,z`, a 3D object snap) names the face or edge
//! nearest it. The solid the line meets first is found on its mesh here, then the kernel says
//! which face or edge it is.

use std::f64::consts::TAU;

use cadcraft_doc::{Common, Entity, EntityKind, Handle, Solid3d};
use cadcraft_geom::{Mat4, Vec3, ViewDir};
use cadcraft_kernel::{EdgeCurve, Hit, Probe, Seg, Topology};
use serde_json::Value;

use super::bad;
use super::curves::{handle_param, is_locked, pick_aperture};
use super::solids::{kernel, placed};
use crate::{Accept, EngineError, Input, Prompt, Result, Session};

/// Which subobjects a prompt picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Sub {
    Face,
    Edge,
}

impl Sub {
    fn plural(self) -> &'static str {
        match self {
            Sub::Face => "faces",
            Sub::Edge => "edges",
        }
    }
    fn one(self) -> &'static str {
        match self {
            Sub::Face => "face",
            Sub::Edge => "edge",
        }
    }
}

/// What an input points at: a line (a click; the whole line, looking along the direction) or a
/// point (one with a height).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Aim {
    Ray(Vec3, Vec3),
    At(Vec3),
}

/// The world point of a point given at a prompt (construction-plane coordinates, at the height
/// it was given at).
pub(super) fn world(s: &Session, p: cadcraft_geom::Vec2) -> Vec3 {
    s.cplane().apply(p.to3(s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0)))
}

/// What a click or a point picks with: in a view that picks what it draws, the eye's line through
/// the cursor (an edge snapped to in 3D: that point); a point with a height: the point; a click in
/// plan: the line down through it.
pub(super) fn aim(s: &Session, i: &Input, kind: Sub) -> Option<Aim> {
    let z = s.point_z.filter(|z| z.is_finite());
    if let (Input::Point(p), Some(z)) = (i, z)
        && (kind == Sub::Edge || s.view_at.is_none())
    {
        return Some(Aim::At(s.cplane().apply(p.to3(z))));
    }
    if let (Some((dir, v)), Input::Point(_) | Input::Pick(_)) = (s.view_at, i) {
        let (x, y, d) = dir.axes();
        return Some(Aim::Ray(x * v.x + y * v.y, -d));
    }
    let Input::Point(p) = i else { return None };
    Some(Aim::Ray(s.cplane().apply(p.to3(0.0)), -s.state().ok()?.screen_dir().dir))
}

/// Where the line from `o` along unit `d` first meets the solid's mesh (any side, any distance
/// along the whole line): the smallest distance along it.
fn ray_hit(x: &Solid3d, o: Vec3, d: Vec3) -> Option<f64> {
    let m = &x.mesh;
    let mut best: Option<f64> = None;
    for t in &m.tris {
        let Some([a, b, c]) = m.tri(t) else { continue };
        let (a, b, c) = (x.world(a), x.world(b), x.world(c));
        // Möller–Trumbore.
        let (e1, e2) = (b - a, c - a);
        let p = d.cross(e2);
        let det = e1.dot(p);
        // Edge-on (or not finite): no crossing.
        let crosses = det.abs() > 1e-14 * e1.len() * e2.len();
        if !crosses {
            continue;
        }
        let inv = 1.0 / det;
        let tv = o - a;
        let u = tv.dot(p) * inv;
        let q = tv.cross(e1);
        let v = d.dot(q) * inv;
        let eps = 1e-9;
        if u < -eps || v < -eps || u + v > 1.0 + eps {
            continue;
        }
        let t = e2.dot(q) * inv;
        if t.is_finite() && best.is_none_or(|b| t < b) {
            best = Some(t);
        }
    }
    best
}

/// How near the line's view of the solid's drawn edges comes to the line (in the plane square to
/// it).
fn ray_gap(x: &Solid3d, o: Vec3, d: Vec3) -> f64 {
    let view = ViewDir::new(-d);
    let at = view.project(o);
    x.mesh
        .edges
        .iter()
        .flat_map(|l| l.windows(2))
        .filter_map(|w| match w {
            [a, b] => Some(cadcraft_geom::Line::new(view.project(x.world(*a)), view.project(x.world(*b))).closest(at).dist(at)),
            _ => None,
        })
        .fold(f64::INFINITY, f64::min)
}

/// The corners of the solid's box in the world.
fn world_box(x: &Solid3d) -> Option<(Vec3, Vec3)> {
    let mut it = x.mesh.points().map(|p| x.world(p));
    let first = it.next()?;
    Some(it.fold((first, first), |(lo, hi), p| {
        (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
    }))
}

/// How far along the line from `o` (unit `d`) the solid's box comes nearest the eye: nothing of
/// the solid lies nearer.
fn nearest_along(x: &Solid3d, o: Vec3, d: Vec3) -> f64 {
    world_box(x).map_or(f64::NEG_INFINITY, |(lo, hi)| {
        (0..8)
            .map(|i| Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z }))
            .map(|c| (c - o).dot(d))
            .fold(f64::INFINITY, f64::min)
    })
}

/// The length of the solid's box diagonal (0 for an empty mesh).
pub(super) fn size(x: &Solid3d) -> f64 {
    world_box(x).map_or(0.0, |(lo, hi)| (hi - lo).len())
}

/// The face or edge an input picks: on the solid `only`, or on any 3D solid or surface in the
/// space being edited (the one the line meets first; for an edge, the visible one nearest the
/// cursor). The solid, the index and the point picked; `None` when the input picks none.
pub(super) fn pick(s: &Session, i: &Input, kind: Sub, only: Option<Handle>) -> Result<Option<(Handle, u32, Vec3)>> {
    let Some(aim) = aim(s, i, kind) else { return Ok(None) };
    let d = s.doc()?;
    let ap = pick_aperture(s).max(1e-9);
    let all: Vec<(Handle, &Solid3d)> = match only {
        Some(h) => match d.entity(h).map(|e| &e.kind) {
            Some(EntityKind::Solid3d(x)) => vec![(h, x)],
            _ => Vec::new(),
        },
        None => d
            .space(&s.space())
            .into_iter()
            .flat_map(|st| st.iter())
            .filter(|e| d.is_visible(e))
            .filter_map(|e| match &e.kind {
                EntityKind::Solid3d(x) if !x.brep.is_empty() => Some((e.handle, x)),
                _ => None,
            })
            .collect(),
    };
    // The faces or edges on the probe of each solid, asked of the kernel in one call.
    let found = |of: &[(Handle, &Solid3d)], probe: Probe| -> Result<Vec<(Handle, Hit)>> {
        let got = cadcraft_kernel::picks(&of.iter().map(|(_, x)| placed(x)).collect::<Vec<_>>(), probe, ap).map_err(kernel)?;
        let mut out = Vec::new();
        for ((h, _), g) in of.iter().zip(got) {
            let g = g.map_err(kernel)?;
            out.extend(
                match kind {
                    Sub::Face => g.faces,
                    Sub::Edge => g.edges,
                }
                .into_iter()
                .map(|hit| (*h, hit)),
            );
        }
        Ok(out)
    };
    let best = match aim {
        Aim::Ray(o, dir) => {
            let dir = dir.normalized();
            let hits: Vec<(Handle, &Solid3d, Option<f64>)> = all.iter().map(|(h, x)| (*h, *x, ray_hit(x, o, dir))).collect();
            let front = hits.iter().filter_map(|(_, _, t)| *t).fold(f64::INFINITY, f64::min);
            match kind {
                Sub::Face => {
                    let Some((h, x, _)) = hits.iter().find(|(.., t)| *t == Some(front)) else { return Ok(None) };
                    found(&[(*h, *x)], Probe::Ray { origin: o, dir })?.into_iter().next()
                }
                Sub::Edge => {
                    // Solids the line meets or passes by, less those wholly behind the nearest face.
                    let near: Vec<(Handle, &Solid3d)> = hits
                        .iter()
                        .filter(|(_, x, _)| !front.is_finite() || nearest_along(x, o, dir) <= front + ap)
                        .filter(|(_, x, t)| t.is_some() || ray_gap(x, o, dir) <= ap)
                        .map(|(h, x, _)| (*h, *x))
                        .collect();
                    let cands = if near.is_empty() { Vec::new() } else { found(&near, Probe::Ray { origin: o, dir })? };
                    // Not behind the nearest face; of those, the nearest the cursor.
                    let seen = |hit: &Hit| !front.is_finite() || hit.along <= front + ap;
                    let near = |a: &&(Handle, Hit), b: &&(Handle, Hit)| a.1.distance.total_cmp(&b.1.distance).then(a.1.along.total_cmp(&b.1.along));
                    let pick = cands.iter().filter(|(_, hit)| seen(hit)).min_by(near).or_else(|| cands.iter().min_by(near));
                    pick.copied()
                }
            }
        }
        Aim::At(p) => {
            let inside: Vec<(Handle, &Solid3d)> = all
                .iter()
                .filter(|(_, x)| {
                    world_box(x).is_some_and(|(lo, hi)| {
                        p.x >= lo.x - ap && p.y >= lo.y - ap && p.z >= lo.z - ap && p.x <= hi.x + ap && p.y <= hi.y + ap && p.z <= hi.z + ap
                    })
                })
                .copied()
                .collect();
            let cands = if inside.is_empty() { Vec::new() } else { found(&inside, Probe::Point(p))? };
            cands.into_iter().min_by(|a, b| a.1.distance.total_cmp(&b.1.distance))
        }
    };
    Ok(best.map(|(h, hit)| (h, hit.index, hit.point)))
}

/// The 3D solid `h` (or surface), refused when it isn't one or is on a locked layer.
pub(super) fn solid_of(s: &Session, cmd: &str, h: Handle) -> Result<Solid3d> {
    let x = match s.doc()?.entity(h).map(|e| &e.kind) {
        Some(EntityKind::Solid3d(x)) if !x.brep.is_empty() => x.clone(),
        Some(EntityKind::Solid3d(_)) => return Err(bad(cmd, "the object has no shape the kernel can edit (a mesh only)")),
        _ => return Err(bad(cmd, format!("{} is not a 3D solid or surface", h.hex()))),
    };
    if is_locked(s, h) {
        return Err(super::solids::locked_err());
    }
    Ok(x)
}

/// The one solid a JSON form edits: `handle`, else the one 3D solid selected.
pub(super) fn target(s: &Session, cmd: &str, p: &Value) -> Result<(Handle, Solid3d)> {
    let h = match p.get("handle") {
        Some(v) if !v.is_null() => handle_param(p, "handle").ok_or_else(|| bad(cmd, format!("`handle`: {v} is not an object handle")))?,
        _ => {
            let xs = super::solids::solids(s, &s.selection())?;
            match xs.as_slice() {
                [(h, _)] => *h,
                _ => return Err(bad(cmd, "`handle` (a 3D solid) is required")),
            }
        }
    };
    Ok((h, solid_of(s, cmd, h)?))
}

/// Faces or edges named in JSON under `key`: indices, or points `[x, y, z]` on or next to them
/// (the nearest face or edge to each). Empty when the key is absent.
pub(super) fn named(cmd: &str, x: &Solid3d, p: &Value, key: &str, kind: Sub) -> Result<Vec<u32>> {
    let Some(v) = p.get(key).filter(|v| !v.is_null()) else { return Ok(Vec::new()) };
    let items = v.as_array().ok_or_else(|| bad(cmd, format!("`{key}` must be an array of {} (indices or points [x, y, z])", kind.plural())))?;
    if items.len() > cadcraft_kernel::MAX_PICKS {
        return Err(bad(cmd, format!("at most {} {}", cadcraft_kernel::MAX_PICKS, kind.plural())));
    }
    let tol = 1e-4 * size(x) + 1e-6;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if let Some(i) = item.as_u64() {
            out.push(u32::try_from(i).map_err(|_| bad(cmd, format!("{i} is not a face or edge index")))?);
            continue;
        }
        let q = super::solids::p3_value(item).ok_or_else(|| bad(cmd, format!("`{key}`: {item} is not an index or a point [x, y, z]")))?;
        let got = cadcraft_kernel::pick(placed(x), Probe::Point(q), tol).map_err(kernel)?;
        let hit = match kind {
            Sub::Face => got.faces.first(),
            Sub::Edge => got.edges.first(),
        };
        out.push(hit.ok_or_else(|| bad(cmd, format!("there is no {} at {item}", kind.one())))?.index);
    }
    Ok(out)
}

/// The solid's faces and edges.
pub(super) fn topology(x: &Solid3d) -> Result<Topology> {
    cadcraft_kernel::topology(placed(x)).map_err(kernel)
}

/// Points along an arc from `a` through `mid` to `b`, or round the whole circle when `full`.
fn arc_points(a: Vec3, mid: Vec3, b: Vec3) -> Vec<Vec3> {
    let Some((c, n)) = circumcentre(a, mid, b) else { return vec![a, b] };
    let (x, e) = (a - c, b - c);
    let y = n.cross(x);
    let sweep = y.dot(e).atan2(x.dot(e)).rem_euclid(TAU);
    let sweep = if sweep < 1e-12 { TAU } else { sweep };
    let n_pts = ((sweep / TAU * 64.0).ceil() as usize).clamp(2, 64);
    (0..=n_pts).map(|i| sweep * i as f64 / n_pts as f64).map(|t| c + x * t.cos() + y * t.sin()).collect()
}

/// The circle through three points: its centre and the unit normal about which `a → mid → b`
/// turns counterclockwise. `None` when they are in a line.
pub(super) fn circumcentre(a: Vec3, mid: Vec3, b: Vec3) -> Option<(Vec3, Vec3)> {
    let (ab, ac) = (mid - a, b - a);
    let n = ab.cross(ac);
    let nn = n.dot(n);
    if !(nn.is_finite() && nn > 1e-24 * ab.dot(ab) * ac.dot(ac)) {
        return None;
    }
    let c = a + (n.cross(ab) * ac.dot(ac) + ac.cross(n) * ab.dot(ab)) * (0.5 / nn);
    Some((c, n.normalized()))
}

/// Points round a circle or an ellipse's part from `t0` to `t1` (radians from `major`, counterclockwise about `normal`).
fn conic_points(center: Vec3, major: Vec3, minor_len: f64, normal: Vec3, t0: f64, t1: f64) -> Vec<Vec3> {
    let minor = normal.normalized().cross(major.normalized()) * minor_len;
    let sweep = if t1 > t0 { t1 - t0 } else { t1 - t0 + TAU };
    let n_pts = ((sweep / TAU * 64.0).ceil() as usize).clamp(2, 64);
    (0..=n_pts).map(|i| t0 + sweep * i as f64 / n_pts as f64).map(|t| center + major * t.cos() + minor * t.sin()).collect()
}

/// A kernel edge as points along it (to draw it, or to tell what it encloses).
pub(super) fn edge_points(c: &EdgeCurve) -> Vec<Vec3> {
    match c {
        EdgeCurve::Line { a, b } => vec![*a, *b],
        EdgeCurve::Arc { a, mid, b } => arc_points(*a, *mid, *b),
        EdgeCurve::Circle { center, radius, normal } => {
            let x = Mat4::ocs(*normal).col(0);
            conic_points(*center, x * *radius, *radius, *normal, 0.0, TAU)
        }
        EdgeCurve::Ellipse { center, major, ratio, normal, start, end } => conic_points(*center, *major, major.len() * ratio, *normal, *start, *end),
        EdgeCurve::Spline { points, .. } => points.clone(),
    }
}

/// A profile piece as points along it.
pub(super) fn seg_points(s: &Seg) -> Vec<Vec3> {
    match *s {
        Seg::Line { a, b } => vec![a, b],
        Seg::Arc { a, mid, b } => arc_points(a, mid, b),
        Seg::Circle { center, radius, normal } => edge_points(&EdgeCurve::Circle { center, radius, normal }),
    }
}

/// The ends of an edge with the unit direction it runs in at each; `None` for a closed one.
pub(super) fn edge_ends(c: &EdgeCurve) -> Option<((Vec3, Vec3), (Vec3, Vec3))> {
    let pts = match c {
        EdgeCurve::Circle { .. } => return None,
        EdgeCurve::Arc { a, mid, b } => {
            let (ctr, n) = circumcentre(*a, *mid, *b)?;
            return Some(((*a, n.cross(*a - ctr).normalized()), (*b, n.cross(*b - ctr).normalized())));
        }
        _ => edge_points(c),
    };
    let (first, second) = (*pts.first()?, *pts.get(1)?);
    let (last, before) = (*pts.last()?, *pts.get(pts.len().checked_sub(2)?)?);
    if (first - last).len() <= 1e-9 * (1.0 + first.len()) {
        return None;
    }
    Some(((first, (second - first).normalized()), (last, (last - before).normalized())))
}

/// The edges joined to `e` end to end and running on smoothly from it (AutoCAD's edge chain),
/// `e` first.
pub(super) fn chain(t: &Topology, e: u32, tol: f64) -> Vec<u32> {
    let usable = |i: u32| t.edges.get(i as usize).is_some_and(|x| !x.seam && !x.degenerate);
    if !usable(e) {
        return Vec::new();
    }
    let mut out = vec![e];
    let mut todo = vec![e];
    while let Some(cur) = todo.pop() {
        let Some(((a, ta), (b, tb))) = t.edges.get(cur as usize).and_then(|x| edge_ends(&x.curve)) else { continue };
        for (i, other) in t.edges.iter().enumerate() {
            let Ok(i) = u32::try_from(i) else { break };
            if out.contains(&i) || !usable(i) {
                continue;
            }
            let Some(((c, tc), (d, td))) = edge_ends(&other.curve) else { continue };
            // Joined at an end, running on in either direction.
            let smooth = |p: Vec3, tp: Vec3, q: Vec3, tq: Vec3| (p - q).len() <= tol && tp.cross(tq).len() < 1e-3;
            if smooth(a, ta, c, tc) || smooth(a, ta, d, td) || smooth(b, tb, c, tc) || smooth(b, tb, d, td) {
                out.push(i);
                todo.push(i);
            }
        }
    }
    out
}

/// The loop of face `f`'s edges that holds `e` (the outline, or a hole's): its edges joined end to
/// end with `e`.
pub(super) fn face_loop(t: &Topology, f: u32, e: u32, tol: f64) -> Vec<u32> {
    let Some(face) = t.faces.get(f as usize) else { return Vec::new() };
    let ends = |i: u32| t.edges.get(i as usize).filter(|x| !x.seam && !x.degenerate).map(|x| edge_ends(&x.curve));
    if !face.edges.contains(&e) || ends(e).is_none() {
        return Vec::new();
    }
    let mut out = vec![e];
    let mut todo = vec![e];
    while let Some(cur) = todo.pop() {
        let Some(Some(((a, _), (b, _)))) = ends(cur) else { continue };
        for &i in &face.edges {
            if out.contains(&i) {
                continue;
            }
            if let Some(Some(((c, _), (d, _)))) = ends(i)
                && [(a, c), (a, d), (b, c), (b, d)].iter().any(|(p, q)| (*p - *q).len() <= tol)
            {
                out.push(i);
                todo.push(i);
            }
        }
    }
    out
}

/// The picked faces' or edges' outlines to show at the prompt (a face: the edges round it).
pub(super) fn highlight(t: &Topology, kind: Sub, picked: &[u32]) -> Vec<EntityKind> {
    let edges: Vec<u32> = match kind {
        Sub::Edge => picked.to_vec(),
        Sub::Face => picked.iter().filter_map(|f| t.faces.get(*f as usize)).flat_map(|f| f.edges.iter().copied()).collect(),
    };
    edges
        .iter()
        .filter_map(|i| t.edges.get(*i as usize))
        .filter(|x| !x.degenerate)
        .map(|x| EntityKind::Polyline3d(cadcraft_doc::Polyline3d { points: edge_points(&x.curve), closed: false }))
        .collect()
}

/// A kernel edge as a drawing object (XEDGES): a line, an arc, circle or ellipse in its plane, a
/// spline in its plane, or a 3D polyline along a curve that isn't flat. Made with `common` (the
/// solid's layer and properties) and the plane's extrusion direction.
pub(super) fn edge_entity(c: &EdgeCurve, common: &Common) -> Option<Entity> {
    let mut common = common.clone();
    common.extrusion = Vec3::Z;
    let in_plane = |n: Vec3| {
        let ocs = Mat4::ocs(n);
        ocs.inverse().map(|inv| (ocs, inv))
    };
    let kind = match c {
        EdgeCurve::Line { a, b } => EntityKind::Line(cadcraft_doc::Line { a: *a, b: *b }),
        EdgeCurve::Arc { a, mid, b } => {
            let (ctr, n) = circumcentre(*a, *mid, *b)?;
            let (_, inv) = in_plane(n)?;
            let (lc, la, lb) = (inv.apply(ctr), inv.apply(*a), inv.apply(*b));
            common.extrusion = n;
            EntityKind::Arc(cadcraft_doc::Arc {
                center: lc,
                radius: (*a - ctr).len(),
                start: (la.y - lc.y).atan2(la.x - lc.x),
                end: (lb.y - lc.y).atan2(lb.x - lc.x),
            })
        }
        EdgeCurve::Circle { center, radius, normal } => {
            let (_, inv) = in_plane(*normal)?;
            common.extrusion = normal.normalized();
            EntityKind::Circle(cadcraft_doc::Circle { center: inv.apply(*center), radius: *radius })
        }
        EdgeCurve::Ellipse { center, major, ratio, normal, start, end } => {
            let (_, inv) = in_plane(*normal)?;
            common.extrusion = normal.normalized();
            EntityKind::Ellipse(cadcraft_doc::Ellipse {
                center: inv.apply(*center),
                major: inv.apply_vec(*major),
                ratio: *ratio,
                start: *start,
                end: *end,
            })
        }
        EdgeCurve::Spline { degree, poles, weights, knots, points } => {
            let o = poles.first().copied().unwrap_or_default();
            let n = poles.iter().zip(poles.iter().cycle().skip(1)).fold(Vec3::ZERO, |n, (p, q)| n + (*p - o).cross(*q - o));
            let size = poles.iter().map(|p| (*p - o).len()).fold(0.0, f64::max);
            let flat = n.len() > 1e-12 * size * size && poles.iter().all(|p| n.normalized().dot(*p - o).abs() <= 1e-9 * (1.0 + size));
            match in_plane(n).filter(|_| flat && knots.len() == poles.len() + *degree as usize + 1) {
                Some((_, inv)) => {
                    let local: Vec<Vec3> = poles.iter().map(|p| inv.apply(*p)).collect();
                    common.extrusion = n.normalized();
                    EntityKind::Spline(cadcraft_geom::Spline {
                        degree: *degree as usize,
                        knots: knots.clone(),
                        control: local.iter().map(|p| p.xy()).collect(),
                        weights: weights.clone(),
                        elevation: local.first().map_or(0.0, |p| p.z),
                        ..Default::default()
                    })
                }
                None => EntityKind::Polyline3d(cadcraft_doc::Polyline3d { points: points.clone(), closed: false }),
            }
        }
    };
    Some(Entity { handle: Handle(0), common, kind })
}

/// Picking faces or edges of one solid at a "Select faces"/"Select edges" prompt: what is picked,
/// in Remove mode or not, with Undo.
pub(super) struct Picks {
    pub kind: Sub,
    /// The solid, once the first pick names it (or given at the start).
    pub solid: Option<(Handle, Solid3d)>,
    pub topo: Option<Topology>,
    pub picked: Vec<u32>,
    /// Picks take faces or edges away from `picked`.
    pub removing: bool,
    /// `picked` are faces to remove from the solid (SHELL): the prompt asks to "Remove faces",
    /// and Add takes them back.
    invert: bool,
    /// What each pick changed, for Undo: (indices, whether they were removed).
    history: Vec<(Vec<u32>, bool)>,
}

/// What a pick prompt's input did.
pub(super) enum Picked {
    /// Keep asking.
    More,
    /// Enter: the picking is over.
    Done,
}

impl Picks {
    pub(super) fn new(kind: Sub) -> Self {
        Picks { kind, solid: None, topo: None, picked: Vec::new(), removing: false, invert: false, history: Vec::new() }
    }
    /// Faces of a solid already chosen, to remove from it (SHELL's).
    pub(super) fn removal(s: &Session, cmd: &str, h: Handle) -> Result<Self> {
        let x = solid_of(s, cmd, h)?;
        let topo = topology(&x)?;
        Ok(Picks { solid: Some((h, x)), topo: Some(topo), invert: true, ..Picks::new(Sub::Face) })
    }
    /// Whether the prompt reads "Remove".
    fn shows_remove(&self) -> bool {
        self.removing != self.invert
    }
    fn sub(&self) -> Sub {
        self.kind
    }
    pub(super) fn handle(&self) -> Option<Handle> {
        self.solid.as_ref().map(|(h, _)| *h)
    }
    /// AutoCAD's "Select faces or [Undo/Remove]" (ALL once one is picked), "Remove faces or
    /// [Undo/Add/ALL]".
    pub(super) fn prompt(&self) -> Prompt {
        let kws: &[&str] = match (self.shows_remove(), self.picked.is_empty()) {
            (true, _) => &["Undo", "Add", "ALL"],
            (false, true) => &["Undo", "Remove"],
            (false, false) => &["Undo", "Remove", "ALL"],
        };
        let message = match (self.sub(), self.shows_remove()) {
            (Sub::Face, false) => "Select faces",
            (Sub::Face, true) => "Remove faces",
            (Sub::Edge, false) => "Select edges",
            (Sub::Edge, true) => "Remove edges",
        };
        Prompt::new(message, Accept::POINT).kw(kws)
    }
    /// The solid's faces and edges (asked of the kernel once).
    pub(super) fn topo(&mut self) -> Result<&Topology> {
        if self.topo.is_none() {
            let Some((_, x)) = &self.solid else { return Err(EngineError::Other("No solid is picked yet.".into())) };
            self.topo = Some(topology(x)?);
        }
        self.topo.as_ref().ok_or_else(|| EngineError::Other("No solid is picked yet.".into()))
    }
    /// Add (or in Remove mode take away) `ids`, remembering the change for Undo.
    pub(super) fn take(&mut self, ids: &[u32]) -> usize {
        let changed: Vec<u32> = if self.removing {
            let gone: Vec<u32> = self.picked.iter().copied().filter(|i| ids.contains(i)).collect();
            self.picked.retain(|i| !ids.contains(i));
            gone
        } else {
            let mut added = Vec::new();
            for i in ids {
                if !self.picked.contains(i) && !added.contains(i) {
                    added.push(*i);
                }
            }
            self.picked.extend(&added);
            added
        };
        let n = changed.len();
        if n > 0 {
            self.history.push((changed, self.removing));
        }
        n
    }
    pub(super) fn undo(&mut self) -> bool {
        match self.history.pop() {
            Some((ids, true)) => self.picked.extend(ids),
            Some((ids, false)) => self.picked.retain(|i| !ids.contains(i)),
            None => return false,
        }
        true
    }
    /// The face or edge an input picks on this solid (any solid before the first pick), making it
    /// the solid. `None`, with a message, when it picks none or one on another solid.
    pub(super) fn hit(&mut self, s: &mut Session, cmd: &str, i: &Input) -> Result<Option<(u32, Vec3)>> {
        let only = self.handle();
        let Some((h, idx, at)) = pick(s, i, self.sub(), only)? else {
            s.echo(match self.sub() {
                Sub::Face => "No face found there.",
                Sub::Edge => "No edge found there.",
            });
            return Ok(None);
        };
        // The first pick names the solid; its faces and edges are listed for the outlines.
        if self.solid.is_none() {
            let x = solid_of(s, cmd, h)?;
            self.topo = Some(topology(&x)?);
            self.solid = Some((h, x));
        }
        Ok(Some((idx, at)))
    }
    /// One input at the prompt: a pick, Undo, Remove, Add, ALL or Enter.
    pub(super) fn feed(&mut self, s: &mut Session, cmd: &str, i: &Input) -> Result<Picked> {
        match i {
            Input::Enter => return Ok(Picked::Done),
            Input::Text(t) | Input::Keyword(t) => match self.prompt().match_keyword(t).as_deref() {
                Some("Undo") => {
                    if !self.undo() {
                        s.echo("Nothing to undo.");
                    }
                }
                Some("Remove") => self.removing = !self.invert,
                Some("Add") => self.removing = self.invert,
                Some("ALL") => {
                    let n = match self.sub() {
                        Sub::Face => self.topo()?.faces.len(),
                        Sub::Edge => self.topo()?.edges.len(),
                    };
                    let all: Vec<u32> = (0..n).filter_map(|i| u32::try_from(i).ok()).collect();
                    self.take(&all);
                }
                _ => return Err(EngineError::Other("Invalid option keyword.".into())),
            },
            Input::Point(_) | Input::Pick(_) => {
                if let Some((idx, _)) = self.hit(s, cmd, i)? {
                    // One at a time: 1, or 0 when it was picked already.
                    let one = self.take(&[idx]) == 1;
                    s.echo(match (one, self.sub(), self.shows_remove()) {
                        (false, ..) => "0 found",
                        (true, Sub::Face, false) => "1 face found.",
                        (true, Sub::Face, true) => "1 face removed.",
                        (true, Sub::Edge, false) => "1 edge found.",
                        (true, Sub::Edge, true) => "1 edge removed.",
                    });
                }
            }
            _ => {}
        }
        Ok(Picked::More)
    }
    /// The picked faces or edges outlined.
    pub(super) fn preview(&self) -> Vec<EntityKind> {
        self.topo.as_ref().map(|t| highlight(t, self.sub(), &self.picked)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circumcentre_and_arcs() {
        let (c, n) = circumcentre(Vec3::new(1.0, 0.0, 5.0), Vec3::new(0.0, 1.0, 5.0), Vec3::new(-1.0, 0.0, 5.0)).unwrap();
        assert!((c - Vec3::new(0.0, 0.0, 5.0)).len() < 1e-12 && (n - Vec3::Z).len() < 1e-12, "{c:?} {n:?}");
        assert!(circumcentre(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), Vec3::new(2.0, 0.0, 0.0)).is_none());
        // A half circle, counterclockwise about +Z through the top.
        let pts = edge_points(&EdgeCurve::Arc { a: Vec3::new(1.0, 0.0, 0.0), mid: Vec3::new(0.0, 1.0, 0.0), b: Vec3::new(-1.0, 0.0, 0.0) });
        assert!(pts.iter().all(|p| (p.len() - 1.0).abs() < 1e-9 && p.y >= -1e-9), "{pts:?}");
        assert!((*pts.last().unwrap() - Vec3::new(-1.0, 0.0, 0.0)).len() < 1e-9);
        // Its ends run up out of the start and down into the end.
        let ((_, ta), (_, tb)) =
            edge_ends(&EdgeCurve::Arc { a: Vec3::new(1.0, 0.0, 0.0), mid: Vec3::new(0.0, 1.0, 0.0), b: Vec3::new(-1.0, 0.0, 0.0) }).unwrap();
        assert!((ta - Vec3::Y).len() < 1e-9 && (tb + Vec3::Y).len() < 1e-9, "{ta:?} {tb:?}");
    }

    /// XEDGES' objects: an arc standing in the XZ plane stays where it is.
    #[test]
    fn edges_become_objects_in_their_planes() {
        let c = EdgeCurve::Arc { a: Vec3::new(2.0, 3.0, 0.0), mid: Vec3::new(1.0, 3.0, 1.0), b: Vec3::new(0.0, 3.0, 0.0) };
        let e = edge_entity(&c, &Common::default()).unwrap();
        let EntityKind::Arc(a) = &e.kind else { panic!("{:?}", e.kind) };
        assert!((a.radius - 1.0).abs() < 1e-12);
        let pl = e.plane().unwrap();
        let ga = cadcraft_geom::Arc::new(a.center.xy(), a.radius, a.start, a.end);
        let (s, m, t) = (pl.apply(ga.start_point().to3(0.0)), pl.apply(ga.mid_point().to3(0.0)), pl.apply(ga.end_point().to3(0.0)));
        assert!((s - Vec3::new(2.0, 3.0, 0.0)).len() < 1e-9 && (t - Vec3::new(0.0, 3.0, 0.0)).len() < 1e-9, "{s:?} {t:?}");
        assert!((m - Vec3::new(1.0, 3.0, 1.0)).len() < 1e-9, "{m:?}");
        let circle =
            edge_entity(&EdgeCurve::Circle { center: Vec3::new(1.0, 2.0, 3.0), radius: 2.0, normal: Vec3::new(1.0, 0.0, 0.0) }, &Common::default())
                .unwrap();
        let EntityKind::Circle(k) = &circle.kind else { panic!() };
        assert!((circle.plane().unwrap().apply(Vec3::new(k.center.x, k.center.y, 0.0)) - Vec3::new(1.0, 2.0, 3.0)).len() < 1e-9);
        // A spline that isn't flat is a 3D polyline through its points.
        let pts = vec![Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0), Vec3::new(0.0, 1.0, 3.0)];
        let sp = EdgeCurve::Spline {
            degree: 3,
            poles: pts.clone(),
            weights: Vec::new(),
            knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
            points: pts,
        };
        assert!(matches!(edge_entity(&sp, &Common::default()).unwrap().kind, EntityKind::Polyline3d(_)));
    }

    /// A solid whose box starts beyond the nearest face is left out of an edge pick (it was asked
    /// of the kernel, one call per solid, and its edges thrown away).
    #[test]
    fn a_solid_behind_starts_further_along() {
        let m = cadcraft_geom::Mesh3 { verts: vec![Vec3::ZERO, Vec3::new(10.0, 10.0, 10.0)], ..Default::default() };
        let x = Solid3d { mesh: std::sync::Arc::new(m), ..Solid3d::default() };
        assert!((nearest_along(&x, Vec3::new(5.0, 5.0, 100.0), -Vec3::Z) - 90.0).abs() < 1e-12);
        assert!((nearest_along(&x, Vec3::new(5.0, 5.0, -100.0), Vec3::Z) - 100.0).abs() < 1e-12);
        assert_eq!(nearest_along(&Solid3d::default(), Vec3::ZERO, Vec3::Z), f64::NEG_INFINITY);
    }

    fn line(a: [f64; 3], b: [f64; 3], faces: Vec<u32>) -> cadcraft_kernel::EdgeInfo {
        let (a, b) = (Vec3::new(a[0], a[1], a[2]), Vec3::new(b[0], b[1], b[2]));
        cadcraft_kernel::EdgeInfo {
            curve: EdgeCurve::Line { a, b },
            length: (b - a).len(),
            mid: (a + b) * 0.5,
            faces,
            seam: false,
            degenerate: false,
        }
    }

    /// A chain runs on through smooth joins only; a loop is the face's edges joined to the edge.
    #[test]
    fn chains_and_loops() {
        let t = Topology {
            faces: vec![cadcraft_kernel::FaceInfo {
                kind: cadcraft_kernel::SurfaceKind::Plane,
                area: 1.0,
                centroid: Vec3::ZERO,
                normal: Vec3::Z,
                bbox: (Vec3::ZERO, Vec3::ZERO),
                axis: (Vec3::ZERO, Vec3::Z),
                radius: 0.0,
                edges: vec![0, 1, 2, 3, 4],
            }],
            edges: vec![
                line([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], vec![0]),
                line([1.0, 0.0, 0.0], [2.0, 0.0, 0.0], vec![0]),
                line([2.0, 0.0, 0.0], [2.0, 1.0, 0.0], vec![0]),
                line([2.0, 1.0, 0.0], [0.0, 0.0, 0.0], vec![0]),
                // A hole's edge, apart.
                line([5.0, 5.0, 0.0], [6.0, 5.0, 0.0], vec![0]),
            ],
        };
        let mut c = chain(&t, 0, 1e-9);
        c.sort();
        assert_eq!(c, vec![0, 1]);
        let mut l = face_loop(&t, 0, 2, 1e-9);
        l.sort();
        assert_eq!(l, vec![0, 1, 2, 3]);
        assert_eq!(face_loop(&t, 0, 4, 1e-9), vec![4]);
        assert!(face_loop(&t, 1, 0, 1e-9).is_empty() && chain(&t, 9, 1e-9).is_empty());
    }

    #[test]
    fn picks_add_remove_and_undo() {
        let mut p = Picks::new(Sub::Face);
        assert_eq!(p.take(&[3, 3, 4]), 2);
        p.removing = true;
        assert_eq!(p.take(&[4, 9]), 1);
        assert_eq!(p.picked, vec![3]);
        assert!(p.undo() && p.picked == vec![3, 4]);
        assert!(p.undo() && p.picked.is_empty() && !p.undo());
        assert!(p.prompt().display().starts_with("Remove faces or [Undo/Add/ALL]"), "{}", p.prompt().display());
    }
}
