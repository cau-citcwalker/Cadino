//! CAD-rhino's 3D modelling kernel.
//!
//! A data-only API over OpenCASCADE: shapes go in and out as B-rep text plus a [`Mesh3`]
//! (triangles and edges) for drawing, so the document and the renderer never see kernel types.
//! Built without the `occt` feature (the web build), every operation returns
//! [`KernelError::Unavailable`].
//!
//! The apps call [`worker_main`] first: from then on every operation runs in a child process (the
//! app's own executable), so an OpenCASCADE crash fails that operation instead of ending the app.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use cadcraft_geom::{Mat4, Mesh3, Vec3};
use serde::{Deserialize, Serialize};

mod curve;
pub use curve::Curve;
use curve::{JOIN, Run, closed, newell, unit};

#[cfg(feature = "occt")]
mod occt;
#[cfg(feature = "occt")]
use occt as backend;

#[cfg(not(feature = "occt"))]
mod none;
#[cfg(not(feature = "occt"))]
use none as backend;

#[cfg(feature = "occt")]
mod worker;

#[derive(Clone, Debug, PartialEq, thiserror::Error, Serialize, Deserialize)]
pub enum KernelError {
    #[error("3D modelling is not available in this build (no OpenCASCADE kernel)")]
    Unavailable,
    #[error("{0}")]
    Invalid(String),
    #[error("the modelling kernel failed: {0}")]
    Failed(String),
}

pub type Result<T> = std::result::Result<T, KernelError>;

/// A shape as the document stores it: B-rep text in model coordinates and its tessellation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Built {
    pub brep: String,
    pub mesh: Mesh3,
}

/// A stored solid placed in the world.
#[derive(Clone, Copy, Debug)]
pub struct Placed<'a> {
    pub brep: &'a str,
    pub xform: &'a Mat4,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Primitive {
    /// Corners of an axis-aligned box.
    Box {
        a: Vec3,
        b: Vec3,
    },
    /// Base centre on the XY plane, radius and height along +Z (negative = downward).
    Cylinder {
        base: Vec3,
        radius: f64,
        height: f64,
    },
    /// Base radius, top radius (0 = pointed) and height along Z.
    Cone {
        base: Vec3,
        base_radius: f64,
        top_radius: f64,
        height: f64,
    },
    Sphere {
        center: Vec3,
        radius: f64,
    },
    /// Torus about a Z axis through `center`: `radius` to the tube centre, `tube` the tube radius.
    Torus {
        center: Vec3,
        radius: f64,
        tube: f64,
    },
    /// A box cut diagonally, from the corners of its bounding box (`b.z - a.z` is the height):
    /// full height at the smaller x, sloping down to nothing at the larger.
    Wedge {
        a: Vec3,
        b: Vec3,
    },
    /// A regular pyramid: base centre, 3 to 32 sides, base radius to a vertex, top radius (0 =
    /// pointed) and height along Z (negative = downward). Its first base edge runs along X.
    Pyramid {
        base: Vec3,
        sides: u32,
        radius: f64,
        top_radius: f64,
        height: f64,
    },
}

/// One piece of a profile, cross section or path.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Seg {
    Line {
        a: Vec3,
        b: Vec3,
    },
    /// A circular arc through `mid`.
    Arc {
        a: Vec3,
        mid: Vec3,
        b: Vec3,
    },
    Circle {
        center: Vec3,
        radius: f64,
        normal: Vec3,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoolOp {
    Union,
    Subtract,
    Intersect,
}

/// Most pieces one profile may have (hostile parameters).
pub const MAX_PROFILE_SEGS: usize = 100_000;
/// Most points a spline curve may have.
pub const MAX_SPLINE_POINTS: usize = 10_000;
/// Most cross sections one loft may have.
pub const MAX_SECTIONS: usize = 1_000;
/// Most sides a pyramid may have.
pub const MAX_PYRAMID_SIDES: u32 = 32;

/// The shortest length the kernel models: OpenCASCADE takes lengths under 1e-7 for zero and
/// throws on them.
const MIN_LEN: f64 = 1e-6;

/// Started with this environment variable set, a program that calls [`worker_main`] serves one
/// kernel request instead of starting.
pub const WORKER_ENV: &str = "CADRHINO_KERNEL_WORKER";

/// Call first in `main`. Started as a kernel worker ([`WORKER_ENV`] set), this reads one request
/// from standard input, runs it, writes the reply to standard output and exits. Otherwise it
/// registers this executable as the worker and returns.
pub fn worker_main() {
    #[cfg(feature = "occt")]
    {
        if std::env::var_os(WORKER_ENV).is_some() {
            worker::serve();
        }
        if let Ok(exe) = std::env::current_exe() {
            worker::register(exe, Vec::new());
        }
    }
}

/// Runs kernel operations in `exe args…`, started with [`WORKER_ENV`] set; it must call
/// [`worker_main`]. The first registration stands. Apps call [`worker_main`] instead; tests use
/// this to make the test binary its own worker.
#[cfg(feature = "occt")]
pub fn set_worker(exe: PathBuf, args: Vec<String>) {
    worker::register(exe, args);
}

/// Whether this build can model (has the OpenCASCADE kernel).
pub fn available() -> bool {
    cfg!(feature = "occt")
}

pub fn primitive(p: &Primitive) -> Result<Built> {
    let pos = |v: f64, what: &str| {
        if v.is_finite() && v >= MIN_LEN { Ok(()) } else { Err(KernelError::Invalid(format!("{what} must be at least {MIN_LEN}"))) }
    };
    let fin = |v: Vec3| if v.is_finite() { Ok(()) } else { Err(KernelError::Invalid("a point is not finite".into())) };
    match *p {
        Primitive::Box { a, b } | Primitive::Wedge { a, b } => {
            fin(a)?;
            fin(b)?;
            let d = b - a;
            let (lo, hi) = (d.x.abs().min(d.y.abs()).min(d.z.abs()), d.x.abs().max(d.y.abs()).max(d.z.abs()));
            // A side far below the box's size is lost to rounding.
            if !hi.is_finite() || lo < MIN_LEN.max(hi * 1e-9) {
                return Err(KernelError::Invalid("the box has no volume".into()));
            }
        }
        Primitive::Cylinder { base, radius, height } => {
            fin(base)?;
            pos(radius, "the radius")?;
            pos(height.abs(), "the height")?;
        }
        Primitive::Cone { base, base_radius, top_radius, height } => {
            fin(base)?;
            pos(base_radius.max(top_radius), "a radius")?;
            if ![base_radius, top_radius].iter().all(|r| *r == 0.0 || *r >= MIN_LEN) {
                return Err(KernelError::Invalid(format!("a radius must be 0 or at least {MIN_LEN}")));
            }
            pos(height.abs(), "the height")?;
        }
        Primitive::Sphere { center, radius } => {
            fin(center)?;
            pos(radius, "the radius")?;
        }
        Primitive::Torus { center, radius, tube } => {
            fin(center)?;
            pos(radius, "the torus radius")?;
            pos(tube, "the tube radius")?;
        }
        Primitive::Pyramid { base, sides, radius, top_radius, height } => {
            fin(base)?;
            if !(3..=MAX_PYRAMID_SIDES).contains(&sides) {
                return Err(KernelError::Invalid(format!("a pyramid has 3 to {MAX_PYRAMID_SIDES} sides")));
            }
            pos(radius, "the base radius")?;
            pos(height.abs(), "the height")?;
            // As for boxes: a size far below the pyramid's is lost to rounding.
            let hi = radius.max(top_radius).max(height.abs());
            let small = |v: f64| v < MIN_LEN.max(hi * 1e-9);
            if !(top_radius == 0.0 || top_radius.is_finite() && !small(top_radius)) || small(radius) || small(height.abs()) {
                return Err(KernelError::Invalid("the pyramid is too flat, or its top too small (make it 0 or larger)".into()));
            }
        }
    }
    let p = match *p {
        // OpenCASCADE can't make a cone with equal radii: that is a cylinder.
        Primitive::Cone { base, base_radius, top_radius, height } if (base_radius - top_radius).abs() < MIN_LEN => {
            Primitive::Cylinder { base, radius: base_radius.max(top_radius), height }
        }
        p => p,
    };
    run(Request::Primitive(p))?.built()
}

/// Whether OpenCASCADE can make the piece: finite, and not so small or flat that it can't tell
/// its points apart (it throws then).
fn seg_ok(s: &Seg) -> bool {
    match *s {
        Seg::Line { a, b } => a.is_finite() && b.is_finite() && (b - a).len() >= MIN_LEN,
        Seg::Arc { a, mid, b } => {
            // A sagitta under a millionth of the chord is a line to it (see the engine's loop_segs).
            let chord = (b - a).len();
            a.is_finite()
                && mid.is_finite()
                && b.is_finite()
                && chord >= MIN_LEN
                && (mid - a).len() >= MIN_LEN
                && (b - mid).len() >= MIN_LEN
                && (mid - a).cross(b - a).len() >= 1e-7 * chord * chord
        }
        Seg::Circle { center, radius, normal } => {
            center.is_finite() && normal.is_finite() && normal.len() > 0.0 && radius.is_finite() && radius >= MIN_LEN
        }
    }
}

/// The pieces must be ones the kernel can make, joined end to end (OpenCASCADE throws on a wire
/// it can't join); a circle stands alone.
fn check_profile(profile: &[Seg]) -> Result<()> {
    if profile.is_empty() || profile.len() > MAX_PROFILE_SEGS {
        return Err(KernelError::Invalid("the profile needs 1 to 100000 pieces".into()));
    }
    if !profile.iter().all(seg_ok) {
        return Err(KernelError::Invalid("the profile has a non-finite point, or a piece too small to model".into()));
    }
    let joined = profile.windows(2).all(|w| match w {
        [p, q] => matches!((curve::ends(p), curve::ends(q)), (Some((_, e)), Some((s, _))) if (e - s).len() <= JOIN),
        _ => true,
    });
    if joined { Ok(()) } else { Err(KernelError::Invalid("the pieces of a curve must meet end to end".into())) }
}

fn check_curve(c: &Curve) -> Result<()> {
    match c {
        Curve::Segs(s) => check_profile(s),
        Curve::Spline { points, tangents } => {
            if points.len() < 2 || points.len() > MAX_SPLINE_POINTS {
                return Err(KernelError::Invalid(format!("a spline needs 2 to {MAX_SPLINE_POINTS} points")));
            }
            let tangent_ok = |t: Vec3| t.is_finite() && t.len() > 0.0;
            if !points.iter().all(|p| p.is_finite()) || tangents.is_some_and(|(a, b)| !(tangent_ok(a) && tangent_ok(b))) {
                return Err(KernelError::Invalid("a spline has a non-finite point or tangent".into()));
            }
            // OpenCASCADE throws on two points in the same place.
            if points.windows(2).any(|w| matches!(w, [a, b] if (*b - *a).len() < MIN_LEN)) {
                return Err(KernelError::Invalid("a spline has two points in the same place".into()));
            }
            Ok(())
        }
    }
}

/// A closed curve that becomes a face must be flat and enclose some area: OpenCASCADE throws
/// on one that isn't. Its unit normal.
fn check_flat(c: &Curve) -> Result<Vec3> {
    if let Curve::Segs(s) = c
        && let [Seg::Circle { normal, .. }] = s.as_slice()
    {
        return unit(*normal).ok_or_else(|| KernelError::Invalid("a circle has no normal".into()));
    }
    let pts = c.points();
    let o = pts.first().copied().unwrap_or(Vec3::ZERO);
    let reach = pts.iter().map(|p| (*p - o).len()).fold(0.0, f64::max);
    let n = newell(&pts, o);
    let Some(n) = unit(n).filter(|_| n.len() > 1e-12 * reach * reach) else {
        return Err(KernelError::Invalid("the profile encloses no area".into()));
    };
    let tol = 1e-7_f64.max(reach * 1e-12);
    if pts.iter().any(|p| n.dot(*p - o).abs() > tol) {
        return Err(KernelError::Invalid("a closed profile must be flat".into()));
    }
    Ok(n)
}

/// A solid swept from a closed planar profile along `dir`, or a surface from an open one.
pub fn extrude(profile: &[Seg], dir: Vec3) -> Result<Built> {
    check_profile(profile)?;
    let Some(d) = unit(dir).filter(|_| dir.len() >= 1e-12) else {
        return Err(KernelError::Invalid("the extrusion needs a non-zero height or direction".into()));
    };
    if closed(profile) {
        // Along its own plane the profile sweeps no volume.
        if check_flat(&Curve::Segs(profile.to_vec()))?.dot(d).abs() < 1e-9 {
            return Err(KernelError::Invalid("the extrusion runs along the profile's plane".into()));
        }
    } else if profile.iter().any(|s| matches!(*s, Seg::Line { a, b } if (b - a).cross(d).len() < MIN_LEN.max((b - a).len() * 1e-9))) {
        return Err(KernelError::Invalid("a line extruded along itself makes no surface".into()));
    }
    run(Request::Extrude(profile.to_vec(), dir))?.built()
}

/// A solid of revolution of a closed planar profile about the axis through `origin` along `axis`.
pub fn revolve(profile: &[Seg], origin: Vec3, axis: Vec3, angle: f64) -> Result<Built> {
    check_profile(profile)?;
    if !closed(profile) {
        return Err(KernelError::Invalid("the profile must be closed".into()));
    }
    check_flat(&Curve::Segs(profile.to_vec()))?;
    if !origin.is_finite() || !axis.is_finite() || axis.len() < 1e-12 {
        return Err(KernelError::Invalid("the axis needs two different points".into()));
    }
    if !angle.is_finite() || angle.abs() < 1e-9 {
        return Err(KernelError::Invalid("the angle must not be zero".into()));
    }
    check_axis(profile, origin, axis)?;
    run(Request::Revolve(profile.to_vec(), origin, axis, angle.clamp(-std::f64::consts::TAU, std::f64::consts::TAU)))?.built()
}

/// The profile must stay on one side of the axis (touching it is fine): revolved across the axis
/// it would cut through itself, and OpenCASCADE throws on that.
fn check_axis(profile: &[Seg], origin: Vec3, axis: Vec3) -> Result<()> {
    let mut pts = Vec::new();
    let mut normal = Vec3::ZERO;
    for s in profile {
        match *s {
            Seg::Line { a, b } => pts.extend([a, b]),
            Seg::Arc { a, mid, b } => pts.extend([a, mid, b]),
            Seg::Circle { normal: n, .. } => normal = n,
        }
    }
    // The profile's plane: Newell's method over its points, or a circle's own normal.
    let o = pts.first().copied().unwrap_or(origin);
    let reach = pts.iter().map(|p| (*p - o).len()).fold(0.0, f64::max);
    let loop_normal = newell(&pts, o);
    if loop_normal.len() > 1e-12 * reach * reach {
        normal = loop_normal;
    }
    if normal.len() == 0.0 {
        return Err(KernelError::Invalid("the profile encloses no area".into()));
    }
    // Signed distances from the plane that holds the axis and stands square on the profile.
    let side = axis.cross(normal);
    if !side.is_finite() || side.len() <= 1e-9 * axis.len() * normal.len() {
        return Err(KernelError::Invalid("the axis must not be perpendicular to the profile".into()));
    }
    let side = side * (1.0 / side.len());
    let mut extremes = pts;
    for s in profile {
        match *s {
            Seg::Line { .. } => {}
            Seg::Arc { a, mid, b } => extremes.extend(arc_extremes(a, mid, b, side)),
            Seg::Circle { center, radius, normal } => extremes.extend(circle_extremes(center, radius, normal, side)),
        }
    }
    let d: Vec<f64> = extremes.iter().map(|p| side.dot(*p - origin)).collect();
    let tol = 1e-7_f64.max(d.iter().fold(0.0, |m: f64, v| m.max(v.abs())) * 1e-12);
    if d.iter().any(|v| *v < -tol) && d.iter().any(|v| *v > tol) {
        return Err(KernelError::Invalid("the axis must not cross the profile".into()));
    }
    Ok(())
}

/// The two points of a circle furthest along `dir` and against it.
fn circle_extremes(center: Vec3, radius: f64, normal: Vec3, dir: Vec3) -> Vec<Vec3> {
    let k = normal * (1.0 / normal.len());
    let d = dir - k * dir.dot(k);
    let l = d.len();
    if !l.is_finite() || l <= 1e-12 {
        return Vec::new();
    }
    let e = d * (radius / l);
    vec![center + e, center - e]
}

/// [`circle_extremes`] for the arc from `a` through `mid` to `b`, where they lie on it.
fn arc_extremes(a: Vec3, mid: Vec3, b: Vec3, dir: Vec3) -> Vec<Vec3> {
    let Some((c, k)) = curve::circumcentre(a, mid, b) else { return Vec::new() };
    let kk = k.dot(k);
    let r = (a - c).len();
    // Angles about the centre from `a`, counterclockwise about `k`.
    let x = (a - c) * (1.0 / r);
    let y = (k * (1.0 / kk.sqrt())).cross(x);
    let angle = |p: Vec3| y.dot(p - c).atan2(x.dot(p - c)).rem_euclid(std::f64::consts::TAU);
    let (m, e) = (angle(mid), angle(b));
    circle_extremes(c, r, k, dir).into_iter().filter(|p| if m <= e { angle(*p) <= e } else { angle(*p) >= e }).collect()
}

pub fn boolean(op: BoolOp, a: Placed, b: Placed) -> Result<Built> {
    check_brep(a.brep)?;
    check_brep(b.brep)?;
    run(Request::Boolean(op, Solid::of(a), Solid::of(b)))?.built()
}

/// Round every edge of the solid.
pub fn fillet(s: Placed, radius: f64) -> Result<Built> {
    if !(radius.is_finite() && radius > 0.0) {
        return Err(KernelError::Invalid("the radius must be positive".into()));
    }
    check_brep(s.brep)?;
    run(Request::Fillet(Solid::of(s), radius))?.built()
}

/// Bevel every edge of the solid by `distance` along both faces.
pub fn chamfer(s: Placed, distance: f64) -> Result<Built> {
    if !(distance.is_finite() && distance >= MIN_LEN) {
        return Err(KernelError::Invalid(format!("the distance must be at least {MIN_LEN}")));
    }
    check_brep(s.brep)?;
    run(Request::Chamfer(Solid::of(s), distance))?.built()
}

/// Hollow the solid inward to walls `thickness` thick: open where its top faces were (the flat
/// faces at its greatest height) when `remove_top`, else closed round an empty inside.
pub fn shell(s: Placed, thickness: f64, remove_top: bool) -> Result<Built> {
    if !(thickness.is_finite() && thickness >= MIN_LEN) {
        return Err(KernelError::Invalid(format!("the thickness must be at least {MIN_LEN}")));
    }
    check_brep(s.brep)?;
    run(Request::Shell(Solid::of(s), thickness, remove_top))?.built()
}

/// A flat surface bounded by a closed planar curve.
pub fn plane_surface(profile: &[Seg]) -> Result<Built> {
    check_profile(profile)?;
    if !closed(profile) {
        return Err(KernelError::Invalid("a planar surface needs a closed curve".into()));
    }
    check_flat(&Curve::Segs(profile.to_vec()))?;
    run(Request::PlaneSurface(profile.to_vec()))?.built()
}

/// The shape cut by the plane through `origin` square to `normal`: the piece on the normal's
/// side, then the one on the other; `None` where none of it lies.
pub fn slice(s: Placed, origin: Vec3, normal: Vec3) -> Result<[Option<Built>; 2]> {
    let Some(n) = unit(normal).filter(|_| origin.is_finite()) else {
        return Err(KernelError::Invalid("the cutting plane needs a point and a direction".into()));
    };
    check_brep(s.brep)?;
    let mut pieces = run(Request::Slice(Solid::of(s), origin, n))?.pieces()?.into_iter();
    match (pieces.next(), pieces.next(), pieces.next()) {
        (Some(a), Some(b), None) => Ok([a, b]),
        _ => Err(mismatch()),
    }
}

/// A solid through closed cross sections, or a surface through open ones, in their order:
/// smooth, or straight between neighbours when `ruled`.
pub fn loft(sections: &[Curve], ruled: bool) -> Result<Built> {
    if sections.len() < 2 || sections.len() > MAX_SECTIONS {
        return Err(KernelError::Invalid(format!("a loft needs 2 to {MAX_SECTIONS} cross sections")));
    }
    sections.iter().try_for_each(check_curve)?;
    let closed = sections.iter().filter(|c| c.is_closed()).count();
    if closed != 0 && closed != sections.len() {
        return Err(KernelError::Invalid("cross sections must be all open or all closed".into()));
    }
    // The end sections are capped with faces.
    if closed > 0 {
        sections.first().into_iter().chain(sections.last()).try_for_each(|c| check_flat(c).map(|_| ()))?;
    }
    // Two horizontal sections at one height have nothing between them.
    let levels: Vec<f64> = sections.iter().filter_map(level).collect();
    if levels.iter().enumerate().any(|(i, a)| levels.iter().skip(i + 1).any(|b| (a - b).abs() < MIN_LEN)) {
        return Err(KernelError::Invalid("cross sections must be at different elevations".into()));
    }
    // Open sections run the way the one before runs, or the surface twists through itself.
    let mut secs = sections.to_vec();
    if closed == 0 {
        for i in 1..secs.len() {
            if let (Some(prev), Some(cur)) = (secs.get(i - 1).and_then(Curve::ends), secs.get(i).and_then(Curve::ends))
                && (cur.0 - prev.0).len() + (cur.1 - prev.1).len() > (cur.0 - prev.1).len() + (cur.1 - prev.0).len()
                && let Some(c) = secs.get_mut(i)
            {
                *c = c.reversed();
            }
        }
    }
    run(Request::Loft(secs, ruled))?.built()
}

/// The height of a section that lies flat in a horizontal plane.
fn level(c: &Curve) -> Option<f64> {
    if let Curve::Segs(s) = c
        && let [Seg::Circle { center, normal, .. }] = s.as_slice()
    {
        return (normal.x.abs() < 1e-12 && normal.y.abs() < 1e-12).then_some(center.z);
    }
    let pts = c.points();
    let z = pts.first()?.z;
    pts.iter().all(|p| (p.z - z).abs() < MIN_LEN).then_some(z)
}

/// A solid from a closed profile, or a surface from an open one, swept along `path`. The profile
/// is first moved to the path's start and turned square to it; corners in the path are mitred.
pub fn sweep(profile: &Curve, path: &Curve) -> Result<Built> {
    check_curve(profile)?;
    check_curve(path)?;
    let solid = profile.is_closed();
    if solid {
        check_flat(profile)?;
    }
    run(Request::Sweep(curve::sweep_runs(profile, path)?, solid))?.built()
}

/// The tessellation of stored B-rep text (files read without a mesh).
pub fn tessellate(brep: &str) -> Result<Mesh3> {
    check_brep(brep)?;
    run(Request::Tessellate(brep.to_owned()))?.mesh()
}

/// [`tessellate`] for many B-reps (the solids of a file being opened) in one worker. If that
/// worker stops (one hostile B-rep can crash it), each is tried in a worker of its own, so the
/// good ones still get their meshes.
pub fn tessellate_all(breps: &[&str]) -> Vec<Result<Mesh3>> {
    let mut out: Vec<Result<Mesh3>> = breps.iter().map(|b| check_brep(b).map(|_| Mesh3::default())).collect();
    let todo: Vec<usize> = out.iter().enumerate().filter(|(_, r)| r.is_ok()).map(|(i, _)| i).collect();
    if todo.is_empty() {
        return out;
    }
    let batch: Vec<String> = todo.iter().filter_map(|i| breps.get(*i)).map(|b| (*b).to_owned()).collect();
    match run(Request::TessellateAll(batch)).and_then(Reply::meshes) {
        Ok(meshes) if meshes.len() == todo.len() => {
            for (i, m) in todo.into_iter().zip(meshes) {
                if let Some(slot) = out.get_mut(i) {
                    *slot = m.map_err(KernelError::Failed);
                }
            }
        }
        _ => {
            for i in todo {
                if let (Some(slot), Some(b)) = (out.get_mut(i), breps.get(i)) {
                    *slot = tessellate(b);
                }
            }
        }
    }
    out
}

/// Text that at least starts like OpenCASCADE's B-rep format, before the kernel parses it.
/// ponytail: a header check only; crafted data past it reaches the C++ reader, which runs in the
/// worker process (see `worker`): it can crash that, not the app.
fn check_brep(brep: &str) -> Result<()> {
    let head: String = brep.chars().take(256).collect();
    if head.contains("CASCADE Topology") { Ok(()) } else { Err(KernelError::Invalid("not OpenCASCADE B-rep text".into())) }
}

pub fn export_step(solids: &[Placed], path: &Path) -> Result<()> {
    run(Request::ExportStep(export_set(solids)?, path.to_owned()))?.done()
}

pub fn export_stl(solids: &[Placed], path: &Path) -> Result<()> {
    run(Request::ExportStl(export_set(solids)?, path.to_owned()))?.done()
}

fn export_set(solids: &[Placed]) -> Result<Vec<Solid>> {
    if solids.is_empty() {
        return Err(KernelError::Invalid("no 3D solids to export".into()));
    }
    solids.iter().map(|s| check_brep(s.brep).map(|_| Solid::of(*s))).collect()
}

/// The shape in a STEP file, as one solid (a compound when the file holds several).
pub fn import_step(path: &Path) -> Result<Built> {
    run(Request::ImportStep(path.to_owned()))?.built()
}

/// Solids, surfaces and curves to one IGES file (as Rhino reads them). Curves the kernel can't
/// make (too small, pieces that don't meet) are left out. How many curves were written.
pub fn export_iges(solids: &[Placed], curves: &[Curve], path: &Path) -> Result<usize> {
    let ss = solids.iter().map(|s| check_brep(s.brep).map(|_| Solid::of(*s))).collect::<Result<Vec<_>>>()?;
    let cs: Vec<Curve> = curves.iter().filter(|c| check_curve(c).is_ok()).cloned().collect();
    if ss.is_empty() && cs.is_empty() {
        return Err(KernelError::Invalid("no 3D solids or curves to export".into()));
    }
    let n = cs.len();
    run(Request::ExportIges(ss, cs, path.to_owned()))?.done()?;
    Ok(n)
}

/// What an IGES file holds: its surfaces and solids, one shape per object the file groups (faces
/// it leaves loose joined along their edges), and its free curves (on no face) as 3D polylines; a
/// closed one ends exactly where it starts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Imported {
    pub shapes: Vec<Built>,
    pub curves: Vec<Vec<Vec3>>,
}

pub fn import_iges(path: &Path) -> Result<Imported> {
    run(Request::ImportIges(path.to_owned()))?.imported()
}

/// A solid's tessellation for drawings, with which of its edges are smooth (parallel to
/// `mesh.edges`): a seam, or an edge whose two faces meet tangentially (a fillet running into a
/// flat face). A drawing shows a smooth edge only where it is a silhouette; any other edge, however
/// shallow the angle its faces meet at, is a real edge.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Outline {
    pub mesh: Mesh3,
    pub smooth: Vec<bool>,
}

/// The [`Outline`] of each stored B-rep (in its model coordinates), in one worker; `None` for one
/// that can't be read.
pub fn outlines(breps: &[&str]) -> Result<Vec<Option<Outline>>> {
    if !available() {
        return Err(KernelError::Unavailable);
    }
    if breps.is_empty() {
        return Ok(Vec::new());
    }
    run(Request::Outlines(breps.iter().map(|b| check_brep(b).ok().map(|_| (*b).to_owned())).collect()))?.outlines()
}

/// The bounding box (min, max) of each placed solid's B-rep, from its geometry rather than its
/// tessellation (a round face measures its full size), in one worker; `None` for one that can't
/// be read or placed.
/// ponytail: B-spline curves and surfaces are bounded by their control points (a little large);
/// exact extrema (BRepBndLib::AddOptimal) when free-form parts go in cut lists.
pub fn boxes(solids: &[Placed]) -> Result<Vec<Option<(Vec3, Vec3)>>> {
    if !available() {
        return Err(KernelError::Unavailable);
    }
    if solids.is_empty() {
        return Ok(Vec::new());
    }
    run(Request::Boxes(solids.iter().map(|s| check_brep(s.brep).ok().map(|_| Solid::of(*s))).collect()))?.boxes()
}

/// One kernel operation with its data, as the worker process receives it.
#[derive(Serialize, Deserialize)]
enum Request {
    Primitive(Primitive),
    Extrude(Vec<Seg>, Vec3),
    Revolve(Vec<Seg>, Vec3, Vec3, f64),
    Boolean(BoolOp, Solid, Solid),
    Fillet(Solid, f64),
    Chamfer(Solid, f64),
    Shell(Solid, f64, bool),
    PlaneSurface(Vec<Seg>),
    Slice(Solid, Vec3, Vec3),
    Loft(Vec<Curve>, bool),
    Sweep(Vec<Run>, bool),
    Tessellate(String),
    TessellateAll(Vec<String>),
    ExportStep(Vec<Solid>, PathBuf),
    ExportStl(Vec<Solid>, PathBuf),
    ImportStep(PathBuf),
    ExportIges(Vec<Solid>, Vec<Curve>, PathBuf),
    ImportIges(PathBuf),
    Outlines(Vec<Option<String>>),
    Boxes(Vec<Option<Solid>>),
}

/// [`Placed`], owning its data.
#[derive(Serialize, Deserialize)]
struct Solid {
    brep: String,
    xform: Mat4,
}

impl Solid {
    fn of(p: Placed) -> Solid {
        Solid { brep: p.brep.to_owned(), xform: *p.xform }
    }
    fn placed(&self) -> Placed<'_> {
        Placed { brep: &self.brep, xform: &self.xform }
    }
}

#[derive(Serialize, Deserialize)]
enum Reply {
    Built(Built),
    Mesh(Mesh3),
    /// One per B-rep of [`Request::TessellateAll`]: the mesh or why there is none.
    Meshes(Vec<std::result::Result<Mesh3, String>>),
    /// The two pieces of [`Request::Slice`], each `None` where the shape has nothing.
    Pieces(Vec<Option<Built>>),
    Imported(Imported),
    Outlines(Vec<Option<Outline>>),
    Boxes(Vec<Option<(Vec3, Vec3)>>),
    Done,
}

impl Request {
    /// What the operation does, for "it stopped while …".
    #[cfg_attr(not(feature = "occt"), allow(dead_code))]
    fn what(&self) -> &'static str {
        match self {
            Request::Primitive(_) => "making a solid",
            Request::Extrude(..) => "extruding",
            Request::Revolve(..) => "revolving",
            Request::Boolean(..) => "combining solids",
            Request::Fillet(..) => "rounding edges",
            Request::Chamfer(..) => "bevelling edges",
            Request::Shell(..) => "hollowing a solid",
            Request::PlaneSurface(_) => "making a surface",
            Request::Slice(..) => "slicing",
            Request::Loft(..) => "lofting",
            Request::Sweep(..) => "sweeping",
            Request::Tessellate(_) => "reading a solid",
            Request::TessellateAll(_) => "reading solids",
            Request::ExportStep(..) => "writing STEP",
            Request::ExportStl(..) => "writing STL",
            Request::ImportStep(_) => "reading STEP",
            Request::ExportIges(..) => "writing IGES",
            Request::ImportIges(_) => "reading IGES",
            Request::Outlines(_) => "outlining solids",
            Request::Boxes(_) => "measuring solids",
        }
    }

    /// Runs the operation in this process.
    fn serve(&self) -> Result<Reply> {
        fn placed(ss: &[Solid]) -> Vec<Placed<'_>> {
            ss.iter().map(Solid::placed).collect()
        }
        match self {
            Request::Primitive(p) => backend::primitive(p).map(Reply::Built),
            Request::Extrude(prof, dir) => backend::extrude(prof, *dir).map(Reply::Built),
            Request::Revolve(prof, origin, axis, angle) => backend::revolve(prof, *origin, *axis, *angle).map(Reply::Built),
            Request::Boolean(op, a, b) => backend::boolean(*op, a.placed(), b.placed()).map(Reply::Built),
            Request::Fillet(s, radius) => backend::fillet(s.placed(), *radius).map(Reply::Built),
            Request::Chamfer(s, distance) => backend::chamfer(s.placed(), *distance).map(Reply::Built),
            Request::Shell(s, thickness, remove_top) => backend::shell(s.placed(), *thickness, *remove_top).map(Reply::Built),
            Request::PlaneSurface(prof) => backend::plane_surface(prof).map(Reply::Built),
            Request::Slice(s, origin, normal) => backend::slice(s.placed(), *origin, *normal).map(Reply::Pieces),
            Request::Loft(secs, ruled) => backend::loft(secs, *ruled).map(Reply::Built),
            Request::Sweep(runs, solid) => backend::sweep(runs, *solid).map(Reply::Built),
            Request::Tessellate(brep) => backend::tessellate(brep).map(Reply::Mesh),
            Request::TessellateAll(breps) => Ok(Reply::Meshes(breps.iter().map(|b| backend::tessellate(b).map_err(|e| e.to_string())).collect())),
            Request::ExportStep(ss, path) => backend::export_step(&placed(ss), path).map(|_| Reply::Done),
            Request::ExportStl(ss, path) => backend::export_stl(&placed(ss), path).map(|_| Reply::Done),
            Request::ImportStep(path) => backend::import_step(path).map(Reply::Built),
            Request::ExportIges(ss, cs, path) => backend::export_iges(&placed(ss), cs, path).map(|_| Reply::Done),
            Request::ImportIges(path) => backend::import_iges(path).map(Reply::Imported),
            Request::Outlines(breps) => Ok(Reply::Outlines(breps.iter().map(|b| b.as_deref().and_then(|b| backend::outline(b).ok())).collect())),
            Request::Boxes(ss) => Ok(Reply::Boxes(ss.iter().map(|s| s.as_ref().and_then(|s| backend::bounding(s.placed()).ok())).collect())),
        }
    }
}

impl Reply {
    fn built(self) -> Result<Built> {
        if let Reply::Built(b) = self { Ok(b) } else { Err(mismatch()) }
    }
    fn mesh(self) -> Result<Mesh3> {
        if let Reply::Mesh(m) = self { Ok(m) } else { Err(mismatch()) }
    }
    fn meshes(self) -> Result<Vec<std::result::Result<Mesh3, String>>> {
        if let Reply::Meshes(m) = self { Ok(m) } else { Err(mismatch()) }
    }
    fn pieces(self) -> Result<Vec<Option<Built>>> {
        if let Reply::Pieces(p) = self { Ok(p) } else { Err(mismatch()) }
    }
    fn imported(self) -> Result<Imported> {
        if let Reply::Imported(i) = self { Ok(i) } else { Err(mismatch()) }
    }
    fn outlines(self) -> Result<Vec<Option<Outline>>> {
        if let Reply::Outlines(o) = self { Ok(o) } else { Err(mismatch()) }
    }
    fn boxes(self) -> Result<Vec<Option<(Vec3, Vec3)>>> {
        if let Reply::Boxes(b) = self { Ok(b) } else { Err(mismatch()) }
    }
    fn done(self) -> Result<()> {
        if let Reply::Done = self { Ok(()) } else { Err(mismatch()) }
    }
}

fn mismatch() -> KernelError {
    KernelError::Failed("the worker answered a different request".into())
}

/// Runs the operation in the worker process when one is registered, else in this one.
fn run(req: Request) -> Result<Reply> {
    #[cfg(feature = "occt")]
    if let Some(w) = worker::registered() {
        return w.call(&req);
    }
    req.serve()
}

/// The transform as uniform scale, any rotation, optional mirror across the XZ plane and
/// translation: what MOVE, ROTATE, 3DROTATE, SCALE, MIRROR and MIRROR3D make. `None` for
/// anything else (shear, non-uniform scale, perspective).
pub fn similarity(m: &Mat4) -> Option<Similarity> {
    let r = &m.m;
    let affine = r[3] == [0.0, 0.0, 0.0, 1.0];
    // Where the axes go.
    let (x, y, z) = (Vec3::new(r[0][0], r[1][0], r[2][0]), Vec3::new(r[0][1], r[1][1], r[2][1]), Vec3::new(r[0][2], r[1][2], r[2][2]));
    let det = x.dot(y.cross(z));
    if !affine || !det.is_finite() || det.abs() < 1e-36 {
        return None;
    }
    let scale = det.abs().cbrt();
    let mirror = det < 0.0;
    // The rotation's columns: mirrored, y → -y comes first, so the y column turns back.
    let (qx, qy, qz) = (x * (1.0 / scale), y * (if mirror { -1.0 } else { 1.0 } / scale), z * (1.0 / scale));
    let tol = 1e-9;
    let orthonormal = [qx.dot(qx) - 1.0, qy.dot(qy) - 1.0, qz.dot(qz) - 1.0, qx.dot(qy), qx.dot(qz), qy.dot(qz)].iter().all(|e| e.abs() < tol);
    if !orthonormal {
        return None;
    }
    let (axis, angle) = axis_angle(qx, qy, qz);
    Some(Similarity { scale, mirror, axis, angle, offset: Vec3::new(r[0][3], r[1][3], r[2][3]) })
}

/// The rotation with these columns as a unit axis and a right-handed angle in [0, π] (Rodrigues).
fn axis_angle(qx: Vec3, qy: Vec3, qz: Vec3) -> (Vec3, f64) {
    // Twice sin(angle) along the axis, from the antisymmetric part.
    let v = Vec3::new(qy.z - qz.y, qz.x - qx.z, qx.y - qy.x);
    let c = ((qx.x + qy.y + qz.z - 1.0) / 2.0).clamp(-1.0, 1.0);
    let angle = (v.len() / 2.0).atan2(c);
    let axis = if c < 0.0 {
        // Past a quarter turn v loses precision (it vanishes at π): the symmetric part is
        // cos·I + (1 − cos)·k kᵀ, so its row with the largest diagonal, less cos, is along k.
        let row = if qx.x >= qy.y && qx.x >= qz.z {
            Vec3::new(qx.x - c, (qy.x + qx.y) / 2.0, (qz.x + qx.z) / 2.0)
        } else if qy.y >= qz.z {
            Vec3::new((qx.y + qy.x) / 2.0, qy.y - c, (qz.y + qy.z) / 2.0)
        } else {
            Vec3::new((qx.z + qz.x) / 2.0, (qy.z + qz.y) / 2.0, qz.z - c)
        };
        unit(row).map(|k| if k.dot(v) < 0.0 { -k } else { k })
    } else {
        unit(v)
    };
    match axis {
        Some(k) => (k, angle),
        None => (Vec3::Z, 0.0),
    }
}

/// See [`similarity`]: applied as mirror (y → -y), then scale, then rotation by `angle` about the
/// unit `axis` through the origin (right-handed), then offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Similarity {
    pub scale: f64,
    pub mirror: bool,
    pub axis: Vec3,
    pub angle: f64,
    pub offset: Vec3,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use cadcraft_geom::{Mat3, Vec2};

    fn solid_xform(m: &Mat3) -> Mat4 {
        let mut x = Mat4::from_mat3(m);
        x.m[2][2] = m.det().abs().sqrt();
        x
    }

    /// Mirror (y → -y) when asked, scale, turn by `angle` about `axis`, then move by `offset`.
    pub(crate) fn xform(axis: Vec3, angle: f64, scale: f64, mirror: bool, offset: Vec3) -> Mat4 {
        let r = curve::Rot::about(unit(axis).unwrap(), angle);
        let col = |v: Vec3| r.apply(v) * scale;
        let (x, y, z) = (col(Vec3::new(1.0, 0.0, 0.0)), col(Vec3::new(0.0, if mirror { -1.0 } else { 1.0 }, 0.0)), col(Vec3::Z));
        Mat4 { m: [[x.x, y.x, z.x, offset.x], [x.y, y.y, z.y, offset.y], [x.z, y.z, z.z, offset.z], [0.0, 0.0, 0.0, 1.0]] }
    }

    /// The similarity's parts applied to `p` as the kernel applies them.
    fn rebuilt(s: &Similarity, p: Vec3) -> Vec3 {
        let q = Vec3::new(p.x, if s.mirror { -p.y } else { p.y }, p.z) * s.scale;
        curve::Rot::about(s.axis, s.angle).apply(q) + s.offset
    }

    fn round_trips(x: &Mat4) -> Similarity {
        let s = similarity(x).unwrap_or_else(|| panic!("not a similarity: {x:?}"));
        assert!((s.axis.len() - 1.0).abs() < 1e-12 && (0.0..=std::f64::consts::PI).contains(&s.angle), "{s:?}");
        for p in [Vec3::new(1.25, -0.5, 4.0), Vec3::new(-3.0, 7.0, 0.5), Vec3::new(0.0, 0.0, -9.0)] {
            assert!((rebuilt(&s, p) - x.apply(p)).len() < 1e-9 * s.scale.max(1.0) * 10.0, "{s:?} at {p:?}: {:?} vs {:?}", rebuilt(&s, p), x.apply(p));
        }
        s
    }

    #[test]
    fn similarity_round_trips_modify_transforms() {
        let m = Mat3::rotate_about(Vec2::new(3.0, 1.0), 0.7)
            .then(Mat3::scale_about(Vec2::new(-2.0, 5.0), 2.5))
            .then(Mat3::mirror(Vec2::new(0.0, 0.0), Vec2::new(1.0, 2.0)));
        let s = round_trips(&solid_xform(&m));
        assert!(s.mirror && (s.scale - 2.5).abs() < 1e-12);
    }

    #[test]
    fn similarity_takes_any_rotation_mirror_and_scale() {
        use std::f64::consts::{FRAC_PI_2, PI};
        let v = Vec3::new;
        let oblique = v(1.0, -2.0, 3.0);
        let off = v(5.0, -6.0, 7.0);
        let s = round_trips(&xform(v(1.0, 0.0, 0.0), FRAC_PI_2, 1.0, false, off));
        assert!((s.axis - v(1.0, 0.0, 0.0)).len() < 1e-12 && (s.angle - FRAC_PI_2).abs() < 1e-12, "{s:?}");
        round_trips(&xform(oblique, 1.1, 1.0, false, off));
        let s = round_trips(&xform(oblique, 2.0, 3.5, true, off));
        assert!(s.mirror && (s.scale - 3.5).abs() < 1e-12);
        round_trips(&xform(oblique, 0.4, 1e-3, true, Vec3::ZERO));
        // Past half a turn it comes back as the shorter turn the other way.
        let s = round_trips(&xform(oblique, 4.0, 1.0, false, off));
        assert!((s.angle - (std::f64::consts::TAU - 4.0)).abs() < 1e-12 && s.axis.dot(oblique) < 0.0, "{s:?}");
        // Near no turn and near half a turn, where the axis is hard to read.
        for a in [0.0, 1e-12, 1e-7, PI - 1e-7, PI - 1e-12, PI] {
            for axis in [oblique, v(0.0, 0.0, 1.0), v(0.0, -1.0, 0.0), v(1.0, 1e-9, 0.0)] {
                round_trips(&xform(axis, a, 2.0, false, off));
                round_trips(&xform(axis, a, 1.0, true, off));
            }
        }
        assert_eq!(similarity(&Mat4::IDENTITY).map(|s| (s.angle, s.scale, s.mirror)), Some((0.0, 1.0, false)));
    }

    #[test]
    fn non_uniform_scale_is_not_a_similarity() {
        let mut x = Mat4::IDENTITY;
        x.m[2][2] = 2.0;
        assert!(similarity(&x).is_none());
        let mut x = xform(Vec3::new(1.0, 2.0, 3.0), 0.5, 1.0, false, Vec3::ZERO);
        x.m[0][0] *= 1.001;
        assert!(similarity(&x).is_none());
        x.m[0][0] = f64::NAN;
        assert!(similarity(&x).is_none());
        assert!(similarity(&Mat4 { m: [[0.0; 4], [0.0; 4], [0.0; 4], [0.0, 0.0, 0.0, 1.0]] }).is_none());
    }

    #[test]
    fn garbage_brep_never_reaches_the_kernel() {
        assert!(matches!(tessellate("0 DXF garbage"), Err(KernelError::Invalid(_))));
        let p = Placed { brep: "nope", xform: &Mat4::IDENTITY };
        assert!(matches!(boolean(BoolOp::Union, p, p), Err(KernelError::Invalid(_))));
    }

    #[test]
    fn shear_is_not_a_similarity() {
        let mut x = Mat4::IDENTITY;
        x.m[0][1] = 0.5;
        assert!(similarity(&x).is_none());
    }

    #[test]
    fn perspective_is_not_a_similarity() {
        let mut x = Mat4::IDENTITY;
        x.m[3][2] = 0.1;
        assert!(similarity(&x).is_none());
    }

    #[test]
    fn primitives_too_small_to_model_are_refused() {
        let invalid = |p| matches!(primitive(&p), Err(KernelError::Invalid(_)));
        assert!(invalid(Primitive::Box { a: Vec3::ZERO, b: Vec3::new(10.0, 10.0, 1e-8) }));
        assert!(invalid(Primitive::Box { a: Vec3::ZERO, b: Vec3::new(1e10, 1e10, 1.0) }));
        assert!(invalid(Primitive::Cylinder { base: Vec3::ZERO, radius: 1e-8, height: 1.0 }));
        assert!(invalid(Primitive::Cone { base: Vec3::ZERO, base_radius: 5.0, top_radius: 1e-9, height: 10.0 }));
        assert!(invalid(Primitive::Cone { base: Vec3::ZERO, base_radius: 5.0, top_radius: 0.0, height: 1e-8 }));
        assert!(invalid(Primitive::Cone { base: Vec3::ZERO, base_radius: 1e-8, top_radius: 0.0, height: 10.0 }));
        // OpenCASCADE throws on a cone with equal radii: it is made as a cylinder.
        let cone = primitive(&Primitive::Cone { base: Vec3::ZERO, base_radius: 5.0, top_radius: 5.0, height: 10.0 });
        assert!(matches!(cone, Ok(_) | Err(KernelError::Unavailable)), "{cone:?}");
    }

    fn rect(x0: f64, x1: f64) -> Vec<Seg> {
        let p = [Vec3::new(x0, 0.0, 0.0), Vec3::new(x1, 0.0, 0.0), Vec3::new(x1, 10.0, 0.0), Vec3::new(x0, 10.0, 0.0)];
        (0..4).map(|i| Seg::Line { a: p[i], b: p[(i + 1) % 4] }).collect()
    }

    #[test]
    fn revolve_refuses_an_axis_across_the_profile() {
        // OpenCASCADE throws on this, so it must never get there.
        let r = revolve(&rect(0.0, 10.0), Vec3::new(5.0, -5.0, 0.0), Vec3::new(0.0, 1.0, 0.0), 1.0);
        assert!(matches!(r, Err(KernelError::Invalid(ref m)) if m.contains("cross")), "{r:?}");
        let crosses = |prof: &[Seg], x: f64| check_axis(prof, Vec3::new(x, -5.0, 0.0), Vec3::new(0.0, 1.0, 0.0)).is_err();
        assert!(!crosses(&rect(0.0, 10.0), 0.0), "an edge on the axis");
        assert!(!crosses(&rect(0.0, 10.0), -3.0));
        let circle = [Seg::Circle { center: Vec3::new(2.0, 0.0, 0.0), radius: 1.0, normal: Vec3::Z }];
        assert!(crosses(&circle, 2.0), "about its diameter");
        assert!(!crosses(&circle, 1.0), "touching");
        // Arcs on the circle about (0.5, 0) of radius 1, closed by a line; their ends and middle
        // points are right of the axis x = 0.
        let p = |deg: f64| Vec3::new(0.5 + deg.to_radians().cos(), deg.to_radians().sin(), 0.0);
        let arc = |a: f64, m: f64, b: f64| [Seg::Arc { a: p(a), mid: p(m), b: p(b) }, Seg::Line { a: p(b), b: p(a) }];
        assert!(crosses(&arc(110.0, 115.0, 250.0), 0.0), "bulges across through 180°");
        assert!(crosses(&arc(250.0, 245.0, 110.0), 0.0), "the same, drawn the other way");
        assert!(!crosses(&arc(110.0, 0.0, 250.0), 0.0), "the other way round the circle");
        // A half disc on the axis (a sphere).
        let half = [
            Seg::Arc { a: Vec3::new(0.0, -1.0, 0.0), mid: Vec3::new(1.0, 0.0, 0.0), b: Vec3::new(0.0, 1.0, 0.0) },
            Seg::Line { a: Vec3::new(0.0, 1.0, 0.0), b: Vec3::new(0.0, -1.0, 0.0) },
        ];
        assert!(!crosses(&half, 0.0));
        // Parallel to the profile's plane but off it, across its middle: the sweep overlaps itself.
        assert!(check_axis(&rect(0.0, 10.0), Vec3::new(5.0, 0.0, 3.0), Vec3::new(0.0, 1.0, 0.0)).is_err());
    }

    #[test]
    fn bad_parameters_are_errors_not_panics() {
        assert!(matches!(primitive(&Primitive::Sphere { center: Vec3::ZERO, radius: -1.0 }), Err(KernelError::Invalid(_))));
        assert!(matches!(primitive(&Primitive::Box { a: Vec3::ZERO, b: Vec3::new(1.0, 1.0, 0.0) }), Err(KernelError::Invalid(_))));
        assert!(matches!(extrude(&[], Vec3::Z), Err(KernelError::Invalid(_))));
        assert!(matches!(fillet(Placed { brep: "", xform: &Mat4::IDENTITY }, f64::NAN), Err(KernelError::Invalid(_))));
    }

    #[test]
    fn modelling_input_is_checked_before_the_kernel() {
        let invalid = |r: Result<Built>| matches!(r, Err(KernelError::Invalid(_)));
        let v = Vec3::new;
        let line = |a: Vec3, b: Vec3| Seg::Line { a, b };
        let pyramid = |sides, radius, top_radius, height| primitive(&Primitive::Pyramid { base: Vec3::ZERO, sides, radius, top_radius, height });
        assert!(invalid(pyramid(2, 1.0, 0.0, 1.0)) && invalid(pyramid(33, 1.0, 0.0, 1.0)));
        assert!(invalid(pyramid(4, 1.0, 1e-12, 1.0)), "a top too small to model");
        assert!(invalid(pyramid(4, 1.0, f64::NAN, 1.0)) && invalid(pyramid(4, 1.0, 0.0, 0.0)));
        assert!(invalid(primitive(&Primitive::Wedge { a: Vec3::ZERO, b: v(1.0, 0.0, 1.0) })));
        // Pieces the kernel throws on: a point-long line, pieces that don't meet, a flat arc.
        assert!(invalid(extrude(&[line(Vec3::ZERO, Vec3::ZERO)], Vec3::Z)));
        assert!(invalid(extrude(&[line(Vec3::ZERO, v(1.0, 0.0, 0.0)), line(v(2.0, 0.0, 0.0), v(2.0, 1.0, 0.0))], Vec3::Z)));
        assert!(invalid(extrude(&[Seg::Arc { a: Vec3::ZERO, mid: v(1.0, 0.0, 0.0), b: v(2.0, 0.0, 0.0) }], Vec3::Z)));
        // An open line along itself, an open profile revolved, a closed one along its plane.
        assert!(invalid(extrude(&[line(Vec3::ZERO, v(0.0, 0.0, 1.0))], Vec3::Z)));
        assert!(invalid(revolve(&[line(Vec3::ZERO, v(1.0, 0.0, 0.0))], v(0.0, 5.0, 0.0), v(1.0, 0.0, 0.0), 1.0)));
        let tri = [line(Vec3::ZERO, v(1.0, 0.0, 0.0)), line(v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0)), line(v(0.0, 1.0, 0.0), Vec3::ZERO)];
        assert!(invalid(extrude(&tri, v(1.0, 0.0, 0.0))));
        // A closed curve that isn't flat makes no face.
        let bent = [
            line(Vec3::ZERO, v(1.0, 0.0, 0.0)),
            line(v(1.0, 0.0, 0.0), v(1.0, 1.0, 1.0)),
            line(v(1.0, 1.0, 1.0), v(0.0, 1.0, 0.0)),
            line(v(0.0, 1.0, 0.0), Vec3::ZERO),
        ];
        assert!(invalid(plane_surface(&bent)) && invalid(plane_surface(&tri[..2])));
        // Lofts: too few, mixed, two at one elevation.
        let circle = |z: f64| Curve::Segs(vec![Seg::Circle { center: v(0.0, 0.0, z), radius: 1.0, normal: Vec3::Z }]);
        let open = Curve::Segs(vec![line(Vec3::ZERO, v(1.0, 0.0, 0.0))]);
        assert!(invalid(loft(&[circle(0.0)], false)));
        assert!(invalid(loft(&[circle(0.0), open.clone()], false)));
        assert!(invalid(loft(&[circle(0.0), circle(1.0), circle(0.0)], true)));
        // Sweeps: a spline with a repeated point, a path that doubles back.
        let twice = Curve::Spline { points: vec![Vec3::ZERO, Vec3::ZERO, v(1.0, 0.0, 0.0)], tangents: None };
        assert!(invalid(sweep(&circle(0.0), &twice)));
        let back = Curve::Segs(vec![line(Vec3::ZERO, v(5.0, 0.0, 0.0)), line(v(5.0, 0.0, 0.0), v(0.0, 0.0, 0.0))]);
        assert!(invalid(sweep(&circle(0.0), &back)));
        // Sizes and planes.
        let p = Placed { brep: "CASCADE Topology", xform: &Mat4::IDENTITY };
        assert!(invalid(chamfer(p, 0.0)) && invalid(shell(p, -1.0, true)) && invalid(shell(p, f64::INFINITY, false)));
        assert!(matches!(slice(p, Vec3::ZERO, Vec3::ZERO), Err(KernelError::Invalid(_))));
        assert!(matches!(slice(p, v(f64::NAN, 0.0, 0.0), Vec3::Z), Err(KernelError::Invalid(_))));
    }
}
