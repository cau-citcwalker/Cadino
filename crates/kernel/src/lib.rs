//! CAD-rhino's 3D modelling kernel.
//!
//! A data-only API over OpenCASCADE: shapes go in and out as B-rep text plus a [`Mesh3`]
//! (triangles and edges) for drawing, so the document and the renderer never see kernel types.
//! Built without the `occt` feature (the web build), every operation returns
//! [`KernelError::Unavailable`].
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

use std::path::Path;

use cadcraft_geom::{Mat4, Mesh3, Vec3};

#[cfg(feature = "occt")]
mod occt;
#[cfg(feature = "occt")]
use occt as backend;

#[cfg(not(feature = "occt"))]
mod none;
#[cfg(not(feature = "occt"))]
use none as backend;

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
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
#[derive(Clone, Debug, Default, PartialEq)]
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

#[derive(Clone, Copy, Debug, PartialEq)]
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
#[derive(Clone, Copy, Debug, PartialEq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoolOp {
    Union,
    Subtract,
    Intersect,
}

/// Most pieces one profile may have (hostile parameters).
pub const MAX_PROFILE_SEGS: usize = 100_000;

/// Whether this build can model (has the OpenCASCADE kernel).
pub fn available() -> bool {
    cfg!(feature = "occt")
}

pub fn primitive(p: &Primitive) -> Result<Built> {
    let pos = |v: f64, what: &str| if v.is_finite() && v > 0.0 { Ok(()) } else { Err(KernelError::Invalid(format!("{what} must be positive"))) };
    let fin = |v: Vec3| if v.is_finite() { Ok(()) } else { Err(KernelError::Invalid("a point is not finite".into())) };
    match *p {
        Primitive::Box { a, b } => {
            fin(a)?;
            fin(b)?;
            let d = b - a;
            if d.x.abs() < 1e-12 || d.y.abs() < 1e-12 || d.z.abs() < 1e-12 {
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
            if !(base_radius >= 0.0 && top_radius >= 0.0) {
                return Err(KernelError::Invalid("radii must not be negative".into()));
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
    backend::primitive(p)
}

fn check_profile(profile: &[Seg]) -> Result<()> {
    if profile.is_empty() || profile.len() > MAX_PROFILE_SEGS {
        return Err(KernelError::Invalid("the profile needs 1 to 100000 pieces".into()));
    }
    let ok = profile.iter().all(|s| match *s {
        Seg::Line { a, b } => a.is_finite() && b.is_finite(),
        Seg::Arc { a, mid, b } => a.is_finite() && mid.is_finite() && b.is_finite(),
        Seg::Circle { center, radius, normal } => center.is_finite() && normal.is_finite() && radius.is_finite() && radius > 0.0,
    });
    if ok { Ok(()) } else { Err(KernelError::Invalid("the profile has a non-finite point or radius".into())) }
}

/// A solid swept from a closed planar profile along `dir`.
pub fn extrude(profile: &[Seg], dir: Vec3) -> Result<Built> {
    check_profile(profile)?;
    if !dir.is_finite() || dir.len() < 1e-12 {
        return Err(KernelError::Invalid("the extrusion needs a non-zero height or direction".into()));
    }
    backend::extrude(profile, dir)
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
    backend::revolve(profile, origin, axis, angle.clamp(-std::f64::consts::TAU, std::f64::consts::TAU))
}

pub fn boolean(op: BoolOp, a: Placed, b: Placed) -> Result<Built> {
    check_brep(a.brep)?;
    check_brep(b.brep)?;
    backend::boolean(op, a, b)
}

/// Round every edge of the solid.
pub fn fillet(s: Placed, radius: f64) -> Result<Built> {
    if !(radius.is_finite() && radius > 0.0) {
        return Err(KernelError::Invalid("the radius must be positive".into()));
    }
    check_brep(s.brep)?;
    backend::fillet(s, radius)
}

/// The tessellation of stored B-rep text (files read without a mesh).
pub fn tessellate(brep: &str) -> Result<Mesh3> {
    check_brep(brep)?;
    backend::tessellate(brep)
}

/// Text that at least starts like OpenCASCADE's B-rep format, before the kernel parses it.
/// ponytail: a header check only; crafted data past it reaches the C++ reader (see `occt`).
fn check_brep(brep: &str) -> Result<()> {
    let head: String = brep.chars().take(256).collect();
    if head.contains("CASCADE Topology") { Ok(()) } else { Err(KernelError::Invalid("not OpenCASCADE B-rep text".into())) }
}

pub fn export_step(solids: &[Placed], path: &Path) -> Result<()> {
    if solids.is_empty() {
        return Err(KernelError::Invalid("no 3D solids to export".into()));
    }
    for s in solids {
        check_brep(s.brep)?;
    }
    backend::export_step(solids, path)
}

pub fn export_stl(solids: &[Placed], path: &Path) -> Result<()> {
    if solids.is_empty() {
        return Err(KernelError::Invalid("no 3D solids to export".into()));
    }
    for s in solids {
        check_brep(s.brep)?;
    }
    backend::export_stl(solids, path)
}

/// The shape in a STEP file, as one solid (a compound when the file holds several).
pub fn import_step(path: &Path) -> Result<Built> {
    backend::import_step(path)
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
    let affine = r[3][0] == 0.0 && r[3][1] == 0.0 && r[3][3] == 1.0;
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
    fn bad_parameters_are_errors_not_panics() {
        assert!(matches!(primitive(&Primitive::Sphere { center: Vec3::ZERO, radius: -1.0 }), Err(KernelError::Invalid(_))));
        assert!(matches!(primitive(&Primitive::Box { a: Vec3::ZERO, b: Vec3::new(1.0, 1.0, 0.0) }), Err(KernelError::Invalid(_))));
        assert!(matches!(extrude(&[], Vec3::Z), Err(KernelError::Invalid(_))));
        assert!(matches!(fillet(Placed { brep: "", xform: &Mat4::IDENTITY }, f64::NAN), Err(KernelError::Invalid(_))));
    }
}
