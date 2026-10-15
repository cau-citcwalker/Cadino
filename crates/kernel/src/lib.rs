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
}

/// One piece of a closed planar profile.
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
        Primitive::Box { a, b } => {
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

fn check_profile(profile: &[Seg]) -> Result<()> {
    if profile.is_empty() || profile.len() > MAX_PROFILE_SEGS {
        return Err(KernelError::Invalid("the profile needs 1 to 100000 pieces".into()));
    }
    let ok = profile.iter().all(|s| match *s {
        Seg::Line { a, b } => a.is_finite() && b.is_finite(),
        Seg::Arc { a, mid, b } => a.is_finite() && mid.is_finite() && b.is_finite(),
        Seg::Circle { center, radius, normal } => {
            center.is_finite() && normal.is_finite() && normal.len() > 0.0 && radius.is_finite() && radius > 0.0
        }
    });
    if ok { Ok(()) } else { Err(KernelError::Invalid("the profile has a non-finite point or radius".into())) }
}

/// A solid swept from a closed planar profile along `dir`.
pub fn extrude(profile: &[Seg], dir: Vec3) -> Result<Built> {
    check_profile(profile)?;
    if !dir.is_finite() || dir.len() < 1e-12 {
        return Err(KernelError::Invalid("the extrusion needs a non-zero height or direction".into()));
    }
    run(Request::Extrude(profile.to_vec(), dir))?.built()
}

/// A solid of revolution of a closed planar profile about the axis through `origin` along `axis`.
pub fn revolve(profile: &[Seg], origin: Vec3, axis: Vec3, angle: f64) -> Result<Built> {
    check_profile(profile)?;
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
    let newell = pts.iter().zip(pts.iter().cycle().skip(1)).fold(Vec3::ZERO, |n, (p, q)| n + (*p - o).cross(*q - o));
    if newell.len() > 1e-12 * reach * reach {
        normal = newell;
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
    let (u, v) = (a - b, mid - b);
    let k = u.cross(v);
    let kk = k.dot(k);
    if !kk.is_finite() || kk <= 0.0 {
        return Vec::new();
    }
    let c = b + (v * u.dot(u) - u * v.dot(v)).cross(k) * (0.5 / kk);
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

/// One kernel operation with its data, as the worker process receives it.
#[derive(Serialize, Deserialize)]
enum Request {
    Primitive(Primitive),
    Extrude(Vec<Seg>, Vec3),
    Revolve(Vec<Seg>, Vec3, Vec3, f64),
    Boolean(BoolOp, Solid, Solid),
    Fillet(Solid, f64),
    Tessellate(String),
    TessellateAll(Vec<String>),
    ExportStep(Vec<Solid>, PathBuf),
    ExportStl(Vec<Solid>, PathBuf),
    ImportStep(PathBuf),
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
            Request::Tessellate(_) => "reading a solid",
            Request::TessellateAll(_) => "reading solids",
            Request::ExportStep(..) => "writing STEP",
            Request::ExportStl(..) => "writing STL",
            Request::ImportStep(_) => "reading STEP",
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
            Request::Tessellate(brep) => backend::tessellate(brep).map(Reply::Mesh),
            Request::TessellateAll(breps) => Ok(Reply::Meshes(breps.iter().map(|b| backend::tessellate(b).map_err(|e| e.to_string())).collect())),
            Request::ExportStep(ss, path) => backend::export_step(&placed(ss), path).map(|_| Reply::Done),
            Request::ExportStl(ss, path) => backend::export_stl(&placed(ss), path).map(|_| Reply::Done),
            Request::ImportStep(path) => backend::import_step(path).map(Reply::Built),
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

/// The transform as uniform scale, rotation about Z, optional mirror across the XZ plane and
/// translation: the transforms MOVE, ROTATE, SCALE and MIRROR make. `None` for anything else.
pub fn similarity(m: &Mat4) -> Option<Similarity> {
    let r = &m.m;
    let (a, b, c, d) = (r[0][0], r[1][0], r[0][1], r[1][1]);
    let det = a * d - b * c;
    if !det.is_finite() || det.abs() < 1e-24 {
        return None;
    }
    let scale = det.abs().sqrt();
    let mirror = det < 0.0;
    let rotation = b.atan2(a);
    // The second column must be the first turned by +90° (or -90° when mirrored).
    let (ec, ed) = if mirror { (rotation.sin() * scale, -rotation.cos() * scale) } else { (-rotation.sin() * scale, rotation.cos() * scale) };
    let tol = 1e-9 * scale.max(1.0);
    let planar = r[0][2].abs() < tol && r[1][2].abs() < tol && r[2][0].abs() < tol && r[2][1].abs() < tol;
    let uniform = (r[2][2] - scale).abs() < tol && (c - ec).abs() < tol && (d - ed).abs() < tol;
    let affine = r[3][0] == 0.0 && r[3][1] == 0.0 && r[3][2] == 0.0 && r[3][3] == 1.0;
    (planar && uniform && affine).then_some(Similarity { scale, rotation, mirror, offset: Vec3::new(r[0][3], r[1][3], r[2][3]) })
}

/// See [`similarity`]: applied as mirror (y → -y), then scale, then rotation, then offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Similarity {
    pub scale: f64,
    pub rotation: f64,
    pub mirror: bool,
    pub offset: Vec3,
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_geom::{Mat3, Vec2};

    fn solid_xform(m: &Mat3) -> Mat4 {
        let mut x = Mat4::from_mat3(m);
        x.m[2][2] = m.det().abs().sqrt();
        x
    }

    #[test]
    fn similarity_round_trips_modify_transforms() {
        let m = Mat3::rotate_about(Vec2::new(3.0, 1.0), 0.7)
            .then(Mat3::scale_about(Vec2::new(-2.0, 5.0), 2.5))
            .then(Mat3::mirror(Vec2::new(0.0, 0.0), Vec2::new(1.0, 2.0)));
        let x = solid_xform(&m);
        let s = similarity(&x).unwrap();
        assert!(s.mirror && (s.scale - 2.5).abs() < 1e-12);
        // Rebuild the transform from the parts and compare on a point.
        let p = Vec3::new(1.25, -0.5, 4.0);
        let q = Vec3::new(p.x, if s.mirror { -p.y } else { p.y }, p.z) * s.scale;
        let (sn, cs) = s.rotation.sin_cos();
        let q = Vec3::new(cs * q.x - sn * q.y, sn * q.x + cs * q.y, q.z) + s.offset;
        assert!((q - x.apply(p)).len() < 1e-9);
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
}
