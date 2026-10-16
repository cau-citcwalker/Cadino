//! 3D solids (CAD-rhino): BOX, CYLINDER, CONE, SPHERE, TORUS, EXTRUDE, REVOLVE, FILLETEDGE,
//! the solid side of UNION, SUBTRACT and INTERSECT, and STEP/STL exchange. The later modelling
//! commands are in `solids2`.
//!
//! Shapes come from [`cadcraft_kernel`]; the document keeps each as a [`Solid3d`] (a surface is
//! one whose mesh is open). A boolean's result replaces the first solid (keeping its properties)
//! and erases the others.

use std::collections::HashSet;
use std::f64::consts::{PI, TAU};
use std::path::Path;
use std::sync::Arc;

use cadcraft_doc::{EntityKind, Handle, Helix, Solid3d};
use cadcraft_geom::{EPS, Mat4, PolyVertex, Polyline, Spline, Vec2, Vec3};
use cadcraft_kernel::{BoolOp, Built, Curve, MAX_SPLINE_POINTS, Placed, Primitive, Seg};
use serde_json::{Map, Value, json};

use super::machines::{SelOutcome, SelectPhase, number};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

/// One prompt of a primitive: the JSON key it fills and its message.
#[derive(Clone, Copy)]
pub(super) enum Q {
    Point(&'static str, &'static str),
    /// A distance: typed, or picked as the distance from the first point.
    Dist(&'static str, &'static str),
}

pub(super) const BOX: &[Q] =
    &[Q::Point("corner1", "Specify first corner"), Q::Point("corner2", "Specify other corner"), Q::Dist("height", "Specify height")];
const CYLINDER: &[Q] =
    &[Q::Point("center", "Specify center point of base"), Q::Dist("radius", "Specify base radius"), Q::Dist("height", "Specify height")];
const CONE: &[Q] =
    &[Q::Point("center", "Specify center point of base"), Q::Dist("radius", "Specify base radius"), Q::Dist("height", "Specify height")];
const SPHERE: &[Q] = &[Q::Point("center", "Specify center point"), Q::Dist("radius", "Specify radius")];
const TORUS: &[Q] = &[Q::Point("center", "Specify center point"), Q::Dist("radius", "Specify radius"), Q::Dist("tube", "Specify tube radius")];

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("box", "Box", run_box)
            .menu(&["Draw", "Modeling", "Box"])
            .params("{corner1: [x,y,z?], corner2: [x,y,z?], height?} | {corner1, length, width, height}")
            .interactive(|_| Ok(Box::new(Ask::new("box", "BOX", BOX)))),
        CommandSpec::new("cylinder", "Cylinder", run_cylinder)
            .menu(&["Draw", "Modeling", "Cylinder"])
            .alias(&["cyl"])
            .params("{center: [x,y,z?], radius, height}")
            .interactive(|_| Ok(Box::new(Ask::new("cylinder", "CYLINDER", CYLINDER)))),
        CommandSpec::new("cone", "Cone", run_cone)
            .menu(&["Draw", "Modeling", "Cone"])
            .params("{center: [x,y,z?], radius, topRadius?, height}")
            .interactive(|_| Ok(Box::new(Ask::new("cone", "CONE", CONE)))),
        CommandSpec::new("sphere", "Sphere", run_sphere)
            .menu(&["Draw", "Modeling", "Sphere"])
            .params("{center: [x,y,z?], radius}")
            .interactive(|_| Ok(Box::new(Ask::new("sphere", "SPHERE", SPHERE)))),
        CommandSpec::new("torus", "Torus", run_torus)
            .menu(&["Draw", "Modeling", "Torus"])
            .alias(&["tor"])
            .params("{center: [x,y,z?], radius, tube}")
            .interactive(|_| Ok(Box::new(Ask::new("torus", "TORUS", TORUS)))),
        CommandSpec::new("extrude", "Extrude", run_extrude)
            .menu(&["Draw", "Modeling", "Extrude"])
            .alias(&["ext"])
            .params("{handles?, height}: closed polylines, circles and single-loop regions make solids (and are erased); lines, arcs and open polylines make surfaces")
            .interactive(|_| Ok(Box::new(ProfileM::new(ProfileOp::Extrude)))),
        CommandSpec::new("revolve", "Revolve", run_revolve)
            .menu(&["Draw", "Modeling", "Revolve"])
            .alias(&["rev"])
            .params("{handles?, axis: [[x,y],[x,y]], angle? (degrees, default 360)}")
            .interactive(|_| Ok(Box::new(ProfileM::new(ProfileOp::Revolve)))),
        CommandSpec::new("filletedge", "Fillet Edge", run_filletedge)
            .menu(&["Modify", "Solid Editing", "Fillet Edges"])
            .params("{handle | handles?, radius}: rounds every edge of each solid")
            .interactive(|_| Ok(Box::new(ProfileM::new(ProfileOp::Fillet)))),
        CommandSpec::new("stepout", "Export STEP", run_stepout)
            .menu(&["File", "Export", "STEP"])
            .params("{path, handles?} (3D solids: those given or selected, else all of them)")
            .noundo(),
        CommandSpec::new("stlout", "Export STL", run_stlout)
            .menu(&["File", "Export", "STL"])
            .params("{path, handles?} (3D solids: those given or selected, else all of them)")
            .noundo(),
        CommandSpec::new("stepin", "Import STEP", run_stepin).menu(&["File", "Import", "STEP"]).params("{path}"),
    ]
}

pub(super) fn kernel(e: cadcraft_kernel::KernelError) -> EngineError {
    EngineError::Other(e.to_string())
}

pub(super) fn solid(b: Built) -> EntityKind {
    EntityKind::Solid3d(Solid3d { brep: Arc::from(b.brep), mesh: Arc::new(b.mesh), xform: Mat4::IDENTITY })
}

pub(super) fn add(s: &mut Session, b: Built) -> Result<Value> {
    let h = s.add_entity(solid(b))?;
    Ok(json!({ "handle": h.hex() }))
}

/// A point as `[x, y]` or `[x, y, z]` (z = 0 when left out; `None` when z is not a number).
pub(super) fn p3(p: &Value, key: &str) -> Option<Vec3> {
    let v = p.get(key)?;
    let z = match v.as_array().and_then(|a| a.get(2)) {
        Some(z) => z.as_f64()?,
        None => 0.0,
    };
    let q = point_value(v)?.to3(z);
    q.is_finite().then_some(q)
}

pub(super) fn p3_req(cmd: &str, p: &Value, key: &str) -> Result<Vec3> {
    p3(p, key).ok_or_else(|| bad(cmd, format!("`{key}` ([x, y] or [x, y, z]) is required")))
}

/// BOX's and WEDGE's corners: `corner1` and `corner2` (with `height` above the first, or the
/// second's own height), or `corner1` and `length`, `width` and `height`.
pub(super) fn box_corners(cmd: &str, p: &Value) -> Result<(Vec3, Vec3)> {
    let a = p3_req(cmd, p, "corner1")?;
    let b = match p3(p, "corner2") {
        Some(c) => match p.get("height").and_then(Value::as_f64) {
            Some(h) => Vec3::new(c.x, c.y, a.z + h),
            None if (c.z - a.z).abs() > 1e-12 => c,
            None => return Err(bad(cmd, "`height` is required when both corners are on one plane")),
        },
        None => a + Vec3::new(f64_req(cmd, p, "length")?, f64_req(cmd, p, "width")?, f64_req(cmd, p, "height")?),
    };
    Ok((a, b))
}

fn run_box(s: &mut Session, p: &Value) -> Result<Value> {
    let (a, b) = box_corners("box", p)?;
    add(s, cadcraft_kernel::primitive(&Primitive::Box { a, b }).map_err(kernel)?)
}

fn run_cylinder(s: &mut Session, p: &Value) -> Result<Value> {
    let base = p3_req("cylinder", p, "center")?;
    let (radius, height) = (f64_req("cylinder", p, "radius")?, f64_req("cylinder", p, "height")?);
    add(s, cadcraft_kernel::primitive(&Primitive::Cylinder { base, radius, height }).map_err(kernel)?)
}

fn run_cone(s: &mut Session, p: &Value) -> Result<Value> {
    let base = p3_req("cone", p, "center")?;
    let (base_radius, height) = (f64_req("cone", p, "radius")?, f64_req("cone", p, "height")?);
    let top_radius = f64_or(p, "topRadius", 0.0);
    add(s, cadcraft_kernel::primitive(&Primitive::Cone { base, base_radius, top_radius, height }).map_err(kernel)?)
}

fn run_sphere(s: &mut Session, p: &Value) -> Result<Value> {
    let center = p3_req("sphere", p, "center")?;
    add(s, cadcraft_kernel::primitive(&Primitive::Sphere { center, radius: f64_req("sphere", p, "radius")? }).map_err(kernel)?)
}

fn run_torus(s: &mut Session, p: &Value) -> Result<Value> {
    let center = p3_req("torus", p, "center")?;
    let (radius, tube) = (f64_req("torus", p, "radius")?, f64_req("torus", p, "tube")?);
    add(s, cadcraft_kernel::primitive(&Primitive::Torus { center, radius, tube }).map_err(kernel)?)
}

/// A closed loop of polyline vertices as kernel pieces at height `z` (see [`poly_segs`]).
fn loop_segs(vs: &[PolyVertex], z: f64) -> Result<Vec<Seg>> {
    poly_segs(vs, z, true)
}

/// Polyline vertices as kernel pieces at height `z`, round a closed loop or along an open run.
/// Pieces end exactly on the vertices (the kernel joins them by position), coincident vertices
/// are dropped, and arcs too flat for the kernel to tell from their chord are lines. An error
/// when a loop encloses no area, or a run has no length.
fn poly_segs(vs: &[PolyVertex], z: f64, closed: bool) -> Result<Vec<Seg>> {
    // A vertex on the one before it keeps that position and takes the later bulge.
    let mut pts: Vec<PolyVertex> = Vec::with_capacity(vs.len());
    for v in vs {
        match pts.last_mut() {
            Some(l) if l.p.near(v.p, EPS) => l.bulge = v.bulge,
            _ => pts.push(*v),
        }
    }
    if closed && pts.len() > 1 && pts.first().zip(pts.last()).is_some_and(|(a, b)| a.p.near(b.p, EPS)) {
        pts.pop();
    }
    // A nearly flat arc is a line: below a sagitta (|bulge| * chord / 2) of a millionth of the
    // chord its three points are collinear to the kernel, and past a radius of 1e8 the ends it
    // recomputes on the circle miss the vertices by more than it joins pieces across (1e-7).
    let n = pts.len();
    for i in 0..n {
        let next = pts.get((i + 1) % n).map(|w| w.p);
        if let (Some(v), Some(w)) = (pts.get_mut(i), next) {
            let radius = v.p.dist(w) * (1.0 + v.bulge * v.bulge) / (4.0 * v.bulge.abs());
            if !(v.bulge.abs() >= 1e-6 && radius <= 1e8) {
                v.bulge = 0.0;
            }
        }
    }
    let pl = Polyline { vertices: pts, closed };
    let b = pl.bounds();
    let size = b.width().max(b.height());
    if closed {
        // NaN (non-finite vertices) encloses nothing either.
        let encloses = pl.area().abs() > 1e-12 * size * size;
        if !encloses {
            return Err(EngineError::Other("A profile encloses no area.".into()));
        }
    } else if n < 2 || !size.is_finite() {
        return Err(EngineError::Other("A curve has no length.".into()));
    }
    Ok(pl
        .vertices
        .iter()
        .take(if closed { n } else { n - 1 })
        .enumerate()
        .filter_map(|(i, v)| {
            let (a, b) = (v.p, pl.vertices.get((i + 1) % n)?.p);
            Some(if v.bulge == 0.0 {
                Seg::Line { a: a.to3(z), b: b.to3(z) }
            } else {
                // The arc's middle, a sagitta from the chord's.
                let mid = a.mid(b) - (b - a).perp() * (v.bulge / 2.0);
                Seg::Arc { a: a.to3(z), mid: mid.to3(z), b: b.to3(z) }
            })
        })
        .collect())
}

/// A curve object as a kernel curve: lines, arcs, circles, 2D and 3D polylines, single-loop
/// regions, splines and helices; `None` for other objects.
pub(super) fn curve_of(k: &EntityKind) -> Result<Option<Curve>> {
    Ok(Some(match k {
        EntityKind::Line(l) => Curve::Segs(vec![Seg::Line { a: l.a, b: l.b }]),
        EntityKind::Arc(a) => {
            // Counterclockwise from start to end with the angles normalised (files can hold ones
            // past a turn), as the arc draws.
            let g = cadcraft_geom::Arc::new(a.center.xy(), a.radius, a.start, a.end);
            let z = a.center.z;
            Curve::Segs(vec![Seg::Arc { a: g.start_point().to3(z), mid: g.mid_point().to3(z), b: g.end_point().to3(z) }])
        }
        EntityKind::Circle(c) => Curve::Segs(vec![Seg::Circle { center: c.center, radius: c.radius, normal: Vec3::Z }]),
        EntityKind::LwPolyline(p) if p.vertices.len() >= 2 => Curve::Segs(poly_segs(&p.vertices, p.elevation, p.closed)?),
        EntityKind::Polyline3d(p) => Curve::Segs(points_segs(&p.points, p.closed)?),
        EntityKind::Region(r) => match r.loops.as_slice() {
            [l] => Curve::Segs(loop_segs(&l.vertices, 0.0)?),
            _ => return Ok(None),
        },
        EntityKind::Spline(sp) => spline_curve(sp)?,
        EntityKind::Helix(h) => helix_curve(h),
        _ => return Ok(None),
    }))
}

/// A circle as a path: two half circles from its rightmost point, so the path has a start.
pub(super) fn circle_path(center: Vec3, radius: f64) -> Curve {
    let at = |t: f64| Vec3::new(center.x + radius * t.cos(), center.y + radius * t.sin(), center.z);
    let (start, half) = (at(0.0), at(PI));
    Curve::Segs(vec![Seg::Arc { a: start, mid: at(PI / 2.0), b: half }, Seg::Arc { a: half, mid: at(1.5 * PI), b: start }])
}

/// A 3D polyline's lines, without repeated points.
fn points_segs(pts: &[Vec3], closed: bool) -> Result<Vec<Seg>> {
    let mut ps: Vec<Vec3> = Vec::with_capacity(pts.len());
    for p in pts {
        if !ps.last().is_some_and(|l| (*l - *p).len() <= EPS) {
            ps.push(*p);
        }
    }
    if closed && ps.len() > 2 && ps.first().zip(ps.last()).is_some_and(|(a, b)| (*a - *b).len() <= EPS) {
        ps.pop();
    }
    if ps.len() < 2 {
        return Err(EngineError::Other("A curve has no length.".into()));
    }
    let mut segs: Vec<Seg> = ps.windows(2).filter_map(|w| Some(Seg::Line { a: *w.first()?, b: *w.get(1)? })).collect();
    if let (true, Some(a), Some(b)) = (closed && ps.len() > 2, ps.last(), ps.first()) {
        segs.push(Seg::Line { a: *a, b: *b });
    }
    Ok(segs)
}

/// A spline as the smooth curve through points on it, with its end tangents.
/// ponytail: sampled and fitted again rather than passed as NURBS (the bindings take no knots);
/// within a thousandth of its size.
fn spline_curve(sp: &Spline) -> Result<Curve> {
    let b = sp.bounds();
    let size = b.width().max(b.height());
    if !size.is_finite() || size <= EPS {
        return Err(EngineError::Other("A spline has no length.".into()));
    }
    let gap = (size * 1e-9).max(1e-6);
    let mut pts: Vec<Vec3> = Vec::new();
    for p in sp.tessellate(size * 1e-3) {
        if !pts.last().is_some_and(|l| (l.xy() - p).len() <= gap) {
            pts.push(p.to3(0.0));
        }
    }
    // Keep a closed spline's end on its start.
    if let (true, Some(first), Some(last)) = (sp.closed, pts.first().copied(), pts.last_mut()) {
        *last = first;
    }
    let step = pts.len().div_ceil(MAX_SPLINE_POINTS - 1).max(1);
    if step > 1 {
        let last = pts.last().copied();
        pts = pts.into_iter().step_by(step).collect();
        if let (Some(l), Some(end)) = (last, pts.last())
            && *end != l
        {
            pts.push(l);
        }
    }
    let (lo, hi) = sp.domain();
    let (t0, t1) = (sp.derivative(lo).to3(0.0), sp.derivative(hi).to3(0.0));
    let tangents = (t0.len() > 0.0 && t1.len() > 0.0 && t0.is_finite() && t1.is_finite()).then_some((t0, t1));
    Ok(Curve::Spline { points: pts, tangents })
}

/// A helix as the smooth curve through 16 points a turn, with its exact end tangents.
fn helix_curve(h: &Helix) -> Curve {
    let h = h.clone().sanitized();
    // turns is within (0, 500] once sanitized: at most 8 000 points.
    let n = (1..MAX_SPLINE_POINTS).find(|n| *n as f64 >= h.turns * 16.0).unwrap_or(MAX_SPLINE_POINTS - 1).max(8);
    let points = (0..=n).map(|i| h.point_at(i as f64 / n as f64)).collect();
    let spin = if h.ccw { TAU } else { -TAU } * h.turns;
    let grow = h.top_radius - h.base_radius;
    let tangent = |t: f64| {
        let (a, r) = (h.start_angle + spin * t, h.base_radius + grow * t);
        Vec3::new(grow * a.cos() - r * spin * a.sin(), grow * a.sin() + r * spin * a.cos(), h.height)
    };
    Curve::Spline { points, tangents: Some((tangent(0.0), tangent(1.0))) }
}

/// A curve object as a kernel profile of lines and arcs (closed or, unless `closed_only`, open);
/// `None` for other objects (splines and helices too: a profile is lines and arcs).
fn profile(k: &EntityKind, closed_only: bool) -> Result<Option<Vec<Seg>>> {
    let open = match k {
        EntityKind::Line(_) | EntityKind::Arc(_) => true,
        EntityKind::LwPolyline(p) => !p.closed,
        EntityKind::Polyline3d(p) => !p.closed,
        EntityKind::Circle(_) | EntityKind::Region(_) => false,
        _ => return Ok(None),
    };
    if closed_only && open {
        return Ok(None);
    }
    Ok(match curve_of(k)? {
        Some(Curve::Segs(s)) => Some(s),
        _ => None,
    })
}

pub(super) fn locked_err() -> EngineError {
    EngineError::Other("The objects are on a locked layer.".into())
}

/// One object's shape in a command over a selection: `None` when the kernel refused that object
/// (a degenerate or unsuitable curve: the object is skipped, and the first reason is kept in
/// `why` for when nothing is made), an error when the kernel failed (nothing then changes).
pub(super) fn or_skip<T>(r: cadcraft_kernel::Result<T>, why: &mut Option<EngineError>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e @ cadcraft_kernel::KernelError::Invalid(_)) => {
            why.get_or_insert(kernel(e));
            Ok(None)
        }
        Err(e) => Err(kernel(e)),
    }
}

/// Each profile among `hs` with the solid or surface `make` builds from it, in the profile's space
/// (keeping its layer and other properties). A profile that made a solid is erased; one that made
/// a surface stays (as AutoCAD's default DELOBJ does). Objects that aren't curves (or closed ones
/// when `closed_only`), degenerate ones (no length or area, not flat) and those on locked layers
/// are skipped; when nothing is made, the first degenerate one's reason is the error. Every shape
/// is made before the drawing changes, so an error leaves it as it was.
fn from_profiles(
    s: &mut Session,
    cmd: &str,
    hs: &[Handle],
    closed_only: bool,
    make: impl Fn(&[Seg]) -> cadcraft_kernel::Result<Built>,
) -> Result<Value> {
    let mut built = Vec::new();
    let (mut skipped, mut locked, mut why) = (0usize, 0usize, None);
    let d = s.doc()?;
    let mut seen = HashSet::with_capacity(hs.len());
    for h in hs {
        // A handle given twice is one profile.
        if !seen.insert(*h) {
            continue;
        }
        let (Some(e), Some(space)) = (d.entity(*h), d.space_of(*h)) else { continue };
        if super::curves::is_locked(s, *h) {
            locked += 1;
            continue;
        }
        let shape = match profile(&e.kind, closed_only) {
            Ok(Some(prof)) => or_skip(make(&prof), &mut why)?,
            Ok(None) => None,
            Err(err) => {
                why.get_or_insert(err);
                None
            }
        };
        match shape {
            Some(b) => built.push((*h, space, e.common.clone(), b)),
            None => skipped += 1,
        }
    }
    if built.is_empty() {
        let what = if closed_only {
            "select closed polylines, circles or single-loop regions"
        } else {
            "select lines, arcs, circles, polylines or single-loop regions"
        };
        return Err(why.unwrap_or_else(|| if locked > 0 { locked_err() } else { bad(cmd, what) }));
    }
    let d = s.doc_mut()?;
    let mut made = Vec::with_capacity(built.len());
    for (h, space, common, b) in built {
        let surface = b.mesh.open;
        made.push(d.add(&space, common, solid(b))?.hex());
        if !surface {
            d.remove_entity(h);
        }
    }
    Ok(json!({ "handles": made, "skipped": skipped, "locked": locked }))
}

fn run_extrude(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let h = f64_req("extrude", p, "height")?;
    from_profiles(s, "extrude", &hs, false, |prof| cadcraft_kernel::extrude(prof, Vec3::new(0.0, 0.0, h)))
}

fn run_revolve(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let axis = points_param(p, "axis").filter(|a| a.len() == 2).ok_or_else(|| bad("revolve", "`axis` ([[x,y],[x,y]]) is required"))?;
    let (Some(a), Some(b)) = (axis.first().copied(), axis.get(1).copied()) else { return Err(bad("revolve", "`axis` needs two points")) };
    let angle = f64_or(p, "angle", 360.0).to_radians();
    from_profiles(s, "revolve", &hs, true, |prof| cadcraft_kernel::revolve(prof, a.to3(0.0), (b - a).to3(0.0), angle))
}

/// The 3D solids (and surfaces) among `hs`, in order, each once.
pub(super) fn solids(s: &Session, hs: &[Handle]) -> Result<Vec<(Handle, Solid3d)>> {
    let d = s.doc()?;
    let mut seen = HashSet::with_capacity(hs.len());
    Ok(hs
        .iter()
        .filter(|h| seen.insert(**h))
        .filter_map(|h| match &d.entity(*h)?.kind {
            EntityKind::Solid3d(x) => Some((*h, x.clone())),
            _ => None,
        })
        .collect())
}

pub(super) fn placed(x: &Solid3d) -> Placed<'_> {
    Placed { brep: &x.brep, xform: &x.xform }
}

/// Each 3D solid among `hs` (not surfaces, nor those on locked layers) replaced by what `op` makes
/// of it; every result is made before the drawing changes. How many changed, and how many were
/// left on locked layers.
pub(super) fn per_solid(s: &mut Session, cmd: &str, hs: &[Handle], op: impl Fn(Placed) -> cadcraft_kernel::Result<Built>) -> Result<(usize, usize)> {
    let mut xs = solids(s, hs)?;
    xs.retain(|(_, x)| !x.mesh.open);
    let all = xs.len();
    xs.retain(|(h, _)| !super::curves::is_locked(s, *h));
    if xs.is_empty() {
        return Err(if all > 0 { locked_err() } else { bad(cmd, "select 3D solids") });
    }
    let built = xs.iter().map(|(h, x)| Ok((*h, op(placed(x)).map_err(kernel)?))).collect::<Result<Vec<_>>>()?;
    let n = built.len();
    let d = s.doc_mut()?;
    for (h, b) in built {
        d.modify_entity(h, |e| e.kind = solid(b))?;
    }
    Ok((n, all - n))
}

fn run_filletedge(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let r = f64_req("filletedge", p, "radius")?;
    let (n, locked) = per_solid(s, "filletedge", &hs, |x| cadcraft_kernel::fillet(x, r))?;
    Ok(json!({ "filleted": n, "locked": locked }))
}

/// UNION and INTERSECT of the solids among `hs`; `None` when there are none (regions then).
pub(crate) fn fold(s: &mut Session, hs: &[Handle], op: BoolOp) -> Result<Option<Value>> {
    let xs = solids(s, hs)?;
    let Some(((h0, first), rest)) = xs.split_first() else { return Ok(None) };
    if rest.is_empty() {
        return Err(EngineError::Other("Select at least two 3D solids.".into()));
    }
    let mut acc = first.clone();
    for (_, x) in rest {
        acc = combine(&acc, x, op)?;
    }
    finish(s, *h0, acc, &xs.iter().map(|(h, _)| *h).collect::<Vec<_>>())
}

/// SUBTRACT on solids; `None` when `from` holds none (regions then).
pub(crate) fn subtract(s: &mut Session, from: &[Handle], remove: &[Handle]) -> Result<Option<Value>> {
    let (a, b) = (solids(s, from)?, solids(s, remove)?);
    let Some(((h0, first), rest)) = a.split_first() else { return Ok(None) };
    if b.is_empty() {
        return Err(EngineError::Other("Select 3D solids to subtract.".into()));
    }
    if b.iter().any(|(h, _)| a.iter().any(|(x, _)| x == h)) {
        return Err(EngineError::Other("A solid can't be subtracted from itself.".into()));
    }
    let mut acc = first.clone();
    for (_, x) in rest {
        acc = combine(&acc, x, BoolOp::Union)?;
    }
    for (_, x) in &b {
        acc = combine(&acc, x, BoolOp::Subtract)?;
    }
    let all: Vec<Handle> = a.iter().chain(&b).map(|(h, _)| *h).collect();
    finish(s, *h0, acc, &all)
}

fn combine(a: &Solid3d, b: &Solid3d, op: BoolOp) -> Result<Solid3d> {
    let r = cadcraft_kernel::boolean(op, placed(a), placed(b)).map_err(kernel)?;
    Ok(Solid3d { brep: Arc::from(r.brep), mesh: Arc::new(r.mesh), xform: Mat4::IDENTITY })
}

fn finish(s: &mut Session, keep: Handle, r: Solid3d, all: &[Handle]) -> Result<Option<Value>> {
    let d = s.doc_mut()?;
    for h in all.iter().filter(|h| **h != keep) {
        d.remove_entity(*h);
    }
    d.modify_entity(keep, |e| e.kind = EntityKind::Solid3d(r))?;
    Ok(Some(json!({ "handle": keep.hex(), "message": "" })))
}

/// The solids to export: those among `handle`/`handles`, else the selected ones, else every
/// solid in model space.
fn export_set(s: &Session, p: &Value) -> Result<Vec<Solid3d>> {
    // An empty list (as MCP clients send optional arrays) counts as none given.
    let given = ["handle", "handles"].iter().any(|k| p.get(k).is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty())));
    let mut hs = targets(s, p)?;
    if !given && solids(s, &hs)?.is_empty() {
        hs = s.doc()?.model.iter().map(|e| e.handle).collect();
    }
    let xs: Vec<Solid3d> = solids(s, &hs)?.into_iter().map(|(_, x)| x).collect();
    if xs.is_empty() { Err(EngineError::Other("There are no 3D solids to export.".into())) } else { Ok(xs) }
}

fn path_req<'a>(cmd: &str, p: &'a Value) -> Result<&'a str> {
    str_param(p, "path").filter(|x| !x.trim().is_empty()).ok_or_else(|| bad(cmd, "`path` is required"))
}

fn run_stepout(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_req("stepout", p)?;
    let xs = export_set(s, p)?;
    cadcraft_kernel::export_step(&xs.iter().map(placed).collect::<Vec<_>>(), Path::new(path)).map_err(kernel)?;
    Ok(json!({ "exported": xs.len(), "path": path }))
}

fn run_stlout(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_req("stlout", p)?;
    let xs = export_set(s, p)?;
    cadcraft_kernel::export_stl(&xs.iter().map(placed).collect::<Vec<_>>(), Path::new(path)).map_err(kernel)?;
    Ok(json!({ "exported": xs.len(), "path": path }))
}

fn run_stepin(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_req("stepin", p)?;
    add(s, cadcraft_kernel::import_step(Path::new(path)).map_err(kernel)?)
}

/// The prompts of a primitive, then its JSON form.
pub(super) struct Ask {
    id: &'static str,
    name: &'static str,
    qs: &'static [Q],
    vals: Map<String, Value>,
    base: Option<Vec2>,
}

impl Ask {
    pub(super) fn new(id: &'static str, name: &'static str, qs: &'static [Q]) -> Self {
        Ask { id, name, qs, vals: Map::new(), base: None }
    }
    /// The values given so far.
    #[cfg(test)]
    pub(super) fn values(&self) -> &Map<String, Value> {
        &self.vals
    }
}

/// A typed distance.
pub(super) fn distance(t: &str) -> Result<f64> {
    crate::units::parse_distance(t).or_else(|| number(t)).filter(|v| v.is_finite()).ok_or_else(|| EngineError::Other("Requires a distance.".into()))
}

impl Interactive for Ask {
    fn name(&self) -> &'static str {
        self.name
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.qs.get(self.vals.len()) {
            Some(Q::Point(_, m)) => Prompt::new(*m, Accept::POINT).base_opt(self.base),
            Some(Q::Dist(_, m)) => Prompt::new(*m, Accept::POINT_OR_NUMBER).base_opt(self.base),
            None => Prompt::new("", Accept::POINT),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let Some(q) = self.qs.get(self.vals.len()).copied() else { return Ok(Step::Done) };
        let (key, v) = match (q, i) {
            (_, Input::Cancel) => return Ok(Step::Cancel),
            (Q::Point(k, _), Input::Point(p)) => {
                // A first point snapped to a 3D object starts the shape at its height.
                let z = if self.vals.is_empty() { s.point_z.filter(|z| z.is_finite()) } else { None };
                self.base.get_or_insert(p);
                (k, z.map_or_else(|| json!([p.x, p.y]), |z| json!([p.x, p.y, z])))
            }
            (Q::Dist(k, _), Input::Point(p)) => (k, json!(self.base.map_or(0.0, |b| b.dist(p)))),
            (Q::Dist(k, _), Input::Text(t)) => (k, json!(distance(&t)?)),
            _ => return Ok(Step::Continue),
        };
        self.vals.insert(key.into(), v);
        if self.vals.len() < self.qs.len() {
            return Ok(Step::Continue);
        }
        let spec = find_command(self.id).ok_or_else(|| EngineError::UnknownCommand(self.id.into()))?;
        if let Err(e) = (spec.run)(s, &Value::Object(self.vals.clone())) {
            // Any value after the first point may be the bad one (a flat BOX corner, a zero
            // height): ask for them again from the second prompt.
            let first = self.qs.first().map(|q| match q {
                Q::Point(k, _) | Q::Dist(k, _) => *k,
            });
            self.vals.retain(|k, _| Some(k.as_str()) == first);
            return Err(e);
        }
        Ok(Step::Done)
    }
    fn preview(&self, _s: &Session, cursor: Vec2) -> Vec<EntityKind> {
        // The base outline as the cursor moves: BOX's rectangle, the others' base circle.
        // ponytail: drawn at z = 0 even when the first point is on a 3D object (`point_z`); 3D
        // preview objects (an elevation, or the solid itself) when that misleads in 3D views.
        let Some(b) = self.base else { return Vec::new() };
        match (self.id, self.vals.len()) {
            ("box" | "wedge", 1) => vec![super::helpers::lwpoly(super::helpers::rect_vertices(b, cursor), true)],
            (_, 1) if b.dist(cursor) > 1e-12 => vec![super::helpers::circle(b, b.dist(cursor))],
            _ => Vec::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ProfileOp {
    Extrude,
    Revolve,
    Fillet,
    Chamfer,
    Shell,
}

/// EXTRUDE, REVOLVE, FILLETEDGE, CHAMFEREDGE and SHELL at the command line: select objects, then
/// the height, the axis and angle, the radius, the distance or the wall thickness.
pub(super) struct ProfileM {
    op: ProfileOp,
    sel: SelectPhase,
    hs: Vec<Handle>,
    axis: Vec<Vec2>,
}

impl ProfileM {
    pub(super) fn new(op: ProfileOp) -> Self {
        ProfileM { op, sel: SelectPhase::default(), hs: Vec::new(), axis: Vec::new() }
    }
    fn run(&self, s: &mut Session, extra: Value) -> Result<Step> {
        let mut p = json!({ "handles": self.hs.iter().map(|h| h.hex()).collect::<Vec<_>>() });
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        let id = match self.op {
            ProfileOp::Extrude => "extrude",
            ProfileOp::Revolve => "revolve",
            ProfileOp::Fillet => "filletedge",
            ProfileOp::Chamfer => "chamferedge",
            ProfileOp::Shell => "shell",
        };
        let spec = find_command(id).ok_or_else(|| EngineError::UnknownCommand(id.into()))?;
        (spec.run)(s, &p)?;
        s.set_selection(Vec::new());
        Ok(Step::Done)
    }
}

impl Interactive for ProfileM {
    fn name(&self) -> &'static str {
        match self.op {
            ProfileOp::Extrude => "EXTRUDE",
            ProfileOp::Revolve => "REVOLVE",
            ProfileOp::Fillet => "FILLETEDGE",
            ProfileOp::Chamfer => "CHAMFEREDGE",
            ProfileOp::Shell => "SHELL",
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.hs = self.sel.picked.clone();
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.hs.is_empty() {
            return self.sel.prompt();
        }
        match (self.op, self.axis.len()) {
            (ProfileOp::Extrude, _) => Prompt::new("Specify height of extrusion", Accept::NUMBER),
            (ProfileOp::Revolve, 0) => Prompt::new("Specify axis start point", Accept::POINT),
            (ProfileOp::Revolve, 1) => Prompt::new("Specify axis endpoint", Accept::POINT).base_opt(self.axis.first().copied()),
            (ProfileOp::Revolve, _) => Prompt::new("Specify angle of revolution", Accept::NUMBER).default("360"),
            (ProfileOp::Fillet, _) => Prompt::new("Specify fillet radius", Accept::NUMBER),
            (ProfileOp::Chamfer, _) => Prompt::new("Specify distance", Accept::NUMBER),
            (ProfileOp::Shell, _) => Prompt::new("Enter the shell offset distance", Accept::NUMBER),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_empty() {
            return match self.sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Done),
                SelOutcome::Done(hs) => {
                    self.hs = hs;
                    Ok(Step::Continue)
                }
            };
        }
        match (self.op, i) {
            (ProfileOp::Revolve, Input::Point(p)) if self.axis.len() < 2 => {
                if self.axis.first().is_some_and(|a| a.near(p, EPS)) {
                    return Err(EngineError::Other("The axis needs two different points.".into()));
                }
                self.axis.push(p);
                Ok(Step::Continue)
            }
            (ProfileOp::Revolve, Input::Enter) if self.axis.len() == 2 => self.run(s, json!({ "axis": axis_json(&self.axis) })),
            (ProfileOp::Revolve, Input::Text(t)) if self.axis.len() == 2 => {
                let a = number(&t).ok_or_else(|| EngineError::Other("Requires an angle.".into()))?;
                self.run(s, json!({ "axis": axis_json(&self.axis), "angle": a }))
            }
            (ProfileOp::Extrude, Input::Text(t)) => self.run(s, json!({ "height": distance(&t)? })),
            (ProfileOp::Fillet, Input::Text(t)) => self.run(s, json!({ "radius": distance(&t)? })),
            (ProfileOp::Chamfer, Input::Text(t)) => self.run(s, json!({ "distance": distance(&t)? })),
            (ProfileOp::Shell, Input::Text(t)) => self.run(s, json!({ "thickness": distance(&t)? })),
            _ => Ok(Step::Continue),
        }
    }
}

fn axis_json(a: &[Vec2]) -> Value {
    Value::Array(a.iter().map(|p| json!([p.x, p.y])).collect())
}

#[cfg(test)]
#[cfg(feature = "occt")]
mod tests {
    use cadcraft_doc::Space;

    use super::*;

    fn solids_in(s: &Session) -> Vec<Solid3d> {
        s.doc()
            .unwrap()
            .model
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Solid3d(x) => Some(x.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn box_cylinder_and_subtract() {
        let mut s = Session::new();
        let b = s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [10, 10], "height": 10 })).unwrap();
        let c = s.execute("cylinder", &json!({ "center": [5, 5, -1], "radius": 3, "height": 12 })).unwrap();
        assert_eq!(solids_in(&s).len(), 2);
        s.execute("subtract", &json!({ "from": [b["handle"]], "remove": [c["handle"]] })).unwrap();
        let left = solids_in(&s);
        assert_eq!(left.len(), 1);
        assert!(left[0].mesh.tris.len() > 12, "the hole adds faces");
        s.undo().unwrap();
        assert_eq!(solids_in(&s).len(), 2);
    }

    #[test]
    fn moved_solids_union_where_they_are() {
        let mut s = Session::new();
        let a = s.execute("box", &json!({ "corner1": [0, 0, 0], "length": 2, "width": 2, "height": 2 })).unwrap();
        let b = s.execute("box", &json!({ "corner1": [10, 0, 0], "length": 2, "width": 2, "height": 2 })).unwrap();
        // Disjoint: their union is two lumps; moved onto the first it overlaps.
        s.execute("move", &json!({ "handles": [b["handle"]], "from": [10, 0], "to": [1, 1] })).unwrap();
        s.execute("union", &json!({ "handles": [a["handle"], b["handle"]] })).unwrap();
        let u = solids_in(&s);
        assert_eq!(u.len(), 1);
        let bb = u[0].bounds();
        assert!((bb.max.x - 3.0).abs() < 1e-6 && (bb.max.y - 3.0).abs() < 1e-6, "{bb:?}");
    }

    #[test]
    fn extrude_circle_replaces_the_profile() {
        let mut s = Session::new();
        s.cmdline("circle 0,0 5").unwrap();
        let h = s.doc().unwrap().model.iter().next().unwrap().handle;
        s.execute("extrude", &json!({ "handles": [h.hex()], "height": 8 })).unwrap();
        let xs = solids_in(&s);
        assert_eq!(xs.len(), 1);
        assert_eq!(s.doc().unwrap().model.iter().count(), 1, "the circle is gone");
    }

    #[test]
    fn iso_view_zooms_to_projected_extents() {
        let mut s = Session::new();
        s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [4, 4, 4] })).unwrap();
        s.execute("view.swiso", &json!({})).unwrap();
        assert!(!s.state().unwrap().view_dir.is_plan());
        let ext = s.view_extents(&Space::Model).unwrap();
        // The cube's iso silhouette is taller than its 4-unit footprint.
        assert!(ext.height() > 4.5, "{ext:?}");
        s.execute("plan", &json!({})).unwrap();
        assert!(s.state().unwrap().view_dir.is_plan());
    }

    #[test]
    fn bad_sizes_are_errors() {
        let mut s = Session::new();
        assert!(s.execute("sphere", &json!({ "center": [0, 0], "radius": -2 })).is_err());
        assert!(s.execute("box", &json!({ "corner1": [0, 0], "corner2": [1, 1] })).is_err());
        assert!(solids_in(&s).is_empty());
    }
}

/// Kernel-free checks: profiles, prompts, locked layers and selections (no solid is modelled).
#[cfg(test)]
mod plain_tests {
    use std::cell::Cell;

    use cadcraft_doc::{Region, Space};

    use super::*;

    fn pv(x: f64, y: f64, bulge: f64) -> PolyVertex {
        PolyVertex::with_bulge(Vec2::new(x, y), bulge)
    }

    fn ends(s: &Seg) -> (Vec3, Vec3) {
        match *s {
            Seg::Line { a, b } | Seg::Arc { a, b, .. } => (a, b),
            Seg::Circle { center, .. } => (center, center),
        }
    }

    fn arc_mid(s: &Seg) -> Vec2 {
        match *s {
            Seg::Arc { mid, .. } => mid.xy(),
            _ => panic!("not an arc: {s:?}"),
        }
    }

    #[test]
    fn near_flat_arcs_are_lines_and_pieces_meet_exactly() {
        // A tiny bulge (DXF noise) on an axis-aligned chord: three collinear arc points used to
        // reach the kernel.
        let segs = loop_segs(&[pv(0.0, 0.0, 1e-10), pv(10.0, 0.0, 0.0), pv(10.0, 10.0, 0.5), pv(0.0, 10.0, 0.0)], 2.0).unwrap();
        assert!(matches!(segs[0], Seg::Line { .. }), "{segs:?}");
        // So is a long chord's slight arc (its radius would put the kernel's ends off the vertices).
        let long = loop_segs(&[pv(0.0, 0.0, 1e-5), pv(1e4, 0.0, 0.0), pv(1e4, 1e4, 0.0)], 0.0).unwrap();
        assert!(matches!(long[0], Seg::Line { .. }), "{long:?}");
        // Every piece starts exactly where the one before ends.
        for (i, s) in segs.iter().enumerate() {
            let next = &segs[(i + 1) % segs.len()];
            assert_eq!(ends(s).1, ends(next).0, "piece {i}");
        }
        // An arc's middle point is the one the polyline draws.
        for (bulge, i) in [(0.5, 2), (-0.3, 0), (2.5, 0), (-4.0, 0)] {
            let vs = if i == 2 {
                vec![pv(0.0, 0.0, 0.0), pv(10.0, 0.0, 0.0), pv(10.0, 10.0, bulge), pv(0.0, 10.0, 0.0)]
            } else {
                vec![pv(1.0, 2.0, bulge), pv(4.0, -1.0, 0.0), pv(4.0, 5.0, 0.0)]
            };
            let pl = Polyline { vertices: vs.clone(), closed: true };
            let segs = loop_segs(&vs, 0.0).unwrap();
            assert!(arc_mid(&segs[i]).near(pl.segments()[i].mid(), 1e-9), "bulge {bulge}: {segs:?}");
        }
    }

    #[test]
    fn coincident_vertices_are_dropped_and_flat_loops_refused() {
        // A vertex repeated with a bulge, and a closing vertex on the first one.
        let segs = loop_segs(&[pv(0.0, 0.0, 0.5), pv(0.0, 0.0, 0.0), pv(10.0, 0.0, 0.0), pv(10.0, 10.0, 0.0), pv(0.0, 0.0, 0.3)], 0.0).unwrap();
        assert_eq!(segs.len(), 3, "{segs:?}");
        assert!(segs.iter().all(|s| ends(s).0 != ends(s).1));
        // Two vertices with bulges enclose a disc.
        assert_eq!(loop_segs(&[pv(0.0, 0.0, 1.0), pv(2.0, 0.0, 1.0)], 0.0).unwrap().len(), 2);
        // No area: collinear, two points, all on one spot.
        assert!(loop_segs(&[pv(0.0, 0.0, 0.0), pv(5.0, 0.0, 0.0), pv(10.0, 0.0, 0.0)], 0.0).is_err());
        assert!(loop_segs(&[pv(0.0, 0.0, 0.0), pv(5.0, 0.0, 0.0)], 0.0).is_err());
        assert!(loop_segs(&[pv(1.0, 1.0, 0.7), pv(1.0, 1.0, 0.2)], 0.0).is_err());
        // Arcs too round to send flatten to lines over one chord: nothing left.
        assert!(loop_segs(&[pv(0.0, 0.0, 1e9), pv(2.0, 0.0, 1e9)], 0.0).is_err());
    }

    fn circle(s: &mut Session, x: f64) -> Handle {
        s.add_entity(super::super::helpers::circle(Vec2::new(x, 0.0), 1.0)).unwrap()
    }

    #[test]
    fn a_failing_profile_leaves_the_drawing_unchanged() {
        let mut s = Session::new();
        let (a, b) = (circle(&mut s, 0.0), circle(&mut s, 10.0));
        let before = s.doc().unwrap().clone();
        let calls = Cell::new(0);
        let r = from_profiles(&mut s, "extrude", &[a, b], false, |_| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 { Err(cadcraft_kernel::KernelError::Failed("no".into())) } else { Ok(Built::default()) }
        });
        assert!(r.is_err());
        assert_eq!(*s.doc().unwrap(), before);
        // A profile with no area alone: its reason is the error, and nothing changes.
        let flat = s.add_entity(super::super::helpers::lwpoly(vec![pv(0.0, 0.0, 0.0), pv(5.0, 0.0, 0.0)], true)).unwrap();
        let before = s.doc().unwrap().clone();
        let r = from_profiles(&mut s, "extrude", &[flat], false, |_| Ok(Built::default()));
        assert!(r.err().is_some_and(|e| e.to_string().contains("no area")));
        assert_eq!(*s.doc().unwrap(), before);
    }

    #[test]
    fn degenerate_objects_in_a_selection_are_skipped() {
        let mut s = Session::new();
        let good = circle(&mut s, 0.0);
        let flat = s.add_entity(super::super::helpers::lwpoly(vec![pv(0.0, 0.0, 0.0), pv(5.0, 0.0, 0.0)], true)).unwrap();
        let point = s.add_entity(EntityKind::Line(cadcraft_doc::Line { a: Vec3::ZERO, b: Vec3::ZERO })).unwrap();
        let bent = s
            .add_entity(EntityKind::Polyline3d(cadcraft_doc::Polyline3d {
                points: vec![Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0), Vec3::new(0.0, 1.0, 0.0)],
                closed: true,
            }))
            .unwrap();
        let spline = s.add_entity(EntityKind::Spline(Spline::default())).unwrap();
        // The kernel refuses the zero-length line and the bent 3D polyline before modelling.
        let make = |prof: &[Seg]| match prof {
            [Seg::Circle { .. }] => Ok(Built::default()),
            _ => Err(cadcraft_kernel::KernelError::Invalid("not flat".into())),
        };
        let r = from_profiles(&mut s, "extrude", &[good, flat, point, bent, spline], false, make).unwrap();
        assert_eq!((r["handles"].as_array().map(Vec::len), r["skipped"].as_u64()), (Some(1), Some(4)), "{r}");
        let d = s.doc().unwrap();
        assert!(d.entity(good).is_none() && [flat, point, bent, spline].iter().all(|h| d.entity(*h).is_some()));
        // A kernel failure (not a refusal) still fails the whole command.
        let c = circle(&mut s, 9.0);
        let before = s.doc().unwrap().clone();
        assert!(from_profiles(&mut s, "extrude", &[c], false, |_| Err(cadcraft_kernel::KernelError::Failed("x".into()))).is_err());
        assert_eq!(*s.doc().unwrap(), before);
    }

    #[test]
    fn arcs_past_a_turn_are_normalised() {
        // 0 to 450 degrees is the quarter from 0 to 90, not the three quarters the other way.
        let a = cadcraft_doc::Arc { center: Vec3::new(1.0, 1.0, 2.0), radius: 2.0, start: 0.0, end: 2.5 * PI };
        let Some(Curve::Segs(segs)) = curve_of(&EntityKind::Arc(a)).unwrap() else { panic!("not segments") };
        let [Seg::Arc { a, mid, b }] = segs.as_slice() else { panic!("{segs:?}") };
        let h = 2.0 * std::f64::consts::FRAC_1_SQRT_2;
        assert!((*a - Vec3::new(3.0, 1.0, 2.0)).len() < 1e-9 && (*b - Vec3::new(1.0, 3.0, 2.0)).len() < 1e-9, "{segs:?}");
        assert!((*mid - Vec3::new(1.0 + h, 1.0 + h, 2.0)).len() < 1e-9, "{segs:?}");
        // Negative angles too: -90 to 0 is the quarter below the centre.
        let a = cadcraft_doc::Arc { center: Vec3::ZERO, radius: 1.0, start: -0.5 * PI, end: 0.0 };
        let Some(Curve::Segs(segs)) = curve_of(&EntityKind::Arc(a)).unwrap() else { panic!("not segments") };
        assert!(matches!(segs.as_slice(), [Seg::Arc { mid, .. }] if mid.y < -0.5 && mid.x > 0.5), "{segs:?}");
    }

    #[test]
    fn dedup_is_linear_in_the_handles_given() {
        // Hostile: the same handle a hundred thousand times, and as many that hold nothing.
        let mut s = Session::new();
        let x = solid(&mut s);
        let mut hs = vec![x; 100_000];
        hs.extend((1..100_000u64).map(|i| Handle(0x10_0000 + i)));
        assert_eq!(solids(&s, &hs).unwrap().len(), 1);
    }

    #[test]
    fn profiles_on_locked_layers_stay_and_solids_go_to_the_profiles_space() {
        let mut s = Session::new();
        s.new_drawing(true);
        let free = circle(&mut s, 0.0);
        s.execute("layout.set", &json!({ "name": "Layout1" })).unwrap();
        let paper = circle(&mut s, 5.0);
        s.execute("layout.set", &json!({ "name": "Model" })).unwrap();
        let locked = circle(&mut s, 10.0);
        s.doc_mut().unwrap().modify_entity(locked, |e| e.common.layer = "Defpoints".into()).unwrap();
        s.doc_mut().unwrap().layer_mut("Defpoints").unwrap().locked = true;
        let r = from_profiles(&mut s, "extrude", &[free, locked, paper], false, |_| Ok(Built::default())).unwrap();
        assert_eq!(r["locked"], 1, "{r}");
        let d = s.doc().unwrap();
        assert!(d.entity(locked).is_some(), "the locked circle stays");
        assert!(d.entity(free).is_none() && d.entity(paper).is_none());
        let solids_in = |sp: &Space| d.space(sp).unwrap().iter().filter(|e| matches!(e.kind, EntityKind::Solid3d(_))).count();
        assert_eq!(solids_in(&Space::Model), 1);
        assert_eq!(solids_in(&Space::Paper("Layout1".into())), 1);
    }

    #[test]
    fn a_rejected_value_is_asked_again() {
        let mut s = Session::new();
        s.start("sphere").unwrap();
        s.input(Input::Point(Vec2::new(1.0, 1.0))).unwrap();
        let _ = s.input(Input::Text("0".into()));
        let p = s.current_prompt().unwrap();
        assert!(p.message.contains("radius"), "{}", p.message);
        s.input(Input::Cancel).unwrap();
    }

    #[test]
    fn a_flat_box_asks_for_its_corner_again() {
        // The height is fine; the other corner made the box flat.
        let mut s = Session::new();
        s.start("box").unwrap();
        s.input(Input::Point(Vec2::new(0.0, 0.0))).unwrap();
        s.input(Input::Point(Vec2::new(5.0, 0.0))).unwrap();
        let _ = s.input(Input::Text("10".into()));
        assert!(s.doc().unwrap().model.iter().next().is_none(), "no solid was made");
        let p = s.current_prompt().unwrap();
        assert!(p.message.contains("other corner"), "{}", p.message);
        s.input(Input::Cancel).unwrap();
    }

    #[test]
    fn a_profile_given_twice_is_extruded_once() {
        let mut s = Session::new();
        let c = circle(&mut s, 5.0);
        // Without the kernel the extrusion fails, but only after the handles are deduplicated:
        // the error is the kernel's, not a second solid.
        let r = s.execute("extrude", &json!({ "handles": [c.hex(), c.hex()], "height": 5 }));
        if cadcraft_kernel::available() {
            assert_eq!(r.unwrap()["handles"].as_array().map(Vec::len), Some(1));
        } else {
            assert!(r.is_err());
        }
    }

    #[test]
    fn revolve_refuses_an_axis_of_one_point() {
        let mut s = Session::new();
        let c = circle(&mut s, 5.0);
        s.set_selection(vec![c]);
        s.start("revolve").unwrap();
        s.input(Input::Point(Vec2::ZERO)).unwrap();
        let _ = s.input(Input::Point(Vec2::ZERO));
        let p = s.current_prompt().unwrap();
        assert!(p.message.contains("axis endpoint"), "{}", p.message);
        s.input(Input::Cancel).unwrap();
    }

    fn solid(s: &mut Session) -> Handle {
        s.add_entity(EntityKind::Solid3d(Solid3d::default())).unwrap()
    }

    #[test]
    fn locked_solids_and_mixed_selections_are_refused_before_the_kernel() {
        let mut s = Session::new();
        let (a, b) = (solid(&mut s), solid(&mut s));
        let r = s.add_entity(EntityKind::Region(Region::from_loop(vec![pv(0.0, 0.0, 0.0), pv(1.0, 0.0, 0.0), pv(1.0, 1.0, 0.0)]))).unwrap();
        let err = |r: Result<Value>| r.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err(s.execute("union", &json!({ "handles": [a.hex(), b.hex(), r.hex()] }))).contains("can't be combined"));
        assert!(err(s.execute("subtract", &json!({ "from": [a.hex()], "remove": [r.hex()] }))).contains("can't be combined"));
        let l = s.execute("line", &json!({ "points": [[0, 0], [1, 1]] })).unwrap()["handles"][0].clone();
        assert!(err(s.execute("subtract", &json!({ "from": [a.hex()], "remove": [l] }))).contains("Select 3D solids"));
        s.doc_mut().unwrap().layer_mut("0").unwrap().locked = true;
        assert!(err(s.execute("union", &json!({ "handles": [a.hex(), b.hex()] }))).contains("locked"));
        assert!(err(s.execute("filletedge", &json!({ "handles": [a.hex()], "radius": 1 }))).contains("locked"));
        assert_eq!(s.doc().unwrap().model.iter().filter(|e| matches!(e.kind, EntityKind::Solid3d(_))).count(), 2);
    }

    #[test]
    fn export_falls_back_to_every_solid_when_none_is_selected() {
        let mut s = Session::new();
        solid(&mut s);
        solid(&mut s);
        let l = s.execute("line", &json!({ "points": [[0, 0], [1, 1]] })).unwrap()["handles"][0].clone();
        s.set_selection(vec![Handle::parse_hex(l.as_str().unwrap()).unwrap()]);
        assert_eq!(export_set(&s, &json!({})).unwrap().len(), 2);
        // Handles given that hold no solid are an error.
        assert!(export_set(&s, &json!({ "handles": [l] })).is_err());
    }
}
