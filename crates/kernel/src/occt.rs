//! The OpenCASCADE backend (through the `opencascade` crate).
//!
//! ponytail: OpenCASCADE runs in this process. An operation it can't do usually comes back as
//! an error or an empty shape, but a C++ exception the bindings don't catch (some fillets do
//! this) ends the process. Upgrade path: run these functions in a worker process; the API is
//! data-only for that reason.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cadcraft_geom::{Mesh3, Vec3};
use glam::{DVec3, dvec3};
use opencascade::angle::Angle;
use opencascade::bounding_box;
use opencascade::mesh::Mesher;
use opencascade::primitives::{Compound, Edge, Face, Shape, Wire};

use crate::{BoolOp, Built, KernelError, Placed, Primitive, Result, Seg, similarity};

fn dv(p: Vec3) -> DVec3 {
    dvec3(p.x, p.y, p.z)
}

fn v3(p: DVec3) -> Vec3 {
    Vec3::new(p.x, p.y, p.z)
}

fn failed(what: &str) -> impl Fn(opencascade::Error) -> KernelError + '_ {
    move |e| KernelError::Failed(format!("{what}: {e}"))
}

pub fn primitive(p: &Primitive) -> Result<Built> {
    let shape = match *p {
        Primitive::Box { a, b } => Shape::box_from_corners(dv(a), dv(b)),
        Primitive::Cylinder { base, radius, height } => {
            let base = if height < 0.0 { base + Vec3::new(0.0, 0.0, height) } else { base };
            Shape::cylinder(dv(base), radius, DVec3::Z, height.abs())
        }
        Primitive::Cone { base, base_radius, top_radius, height } => {
            let (base, r1, r2) =
                if height < 0.0 { (base + Vec3::new(0.0, 0.0, height), top_radius, base_radius) } else { (base, base_radius, top_radius) };
            Shape::cone().at(dv(base)).bottom_radius(r1).top_radius(r2).height(height.abs()).build()
        }
        Primitive::Sphere { center, radius } => Shape::sphere(radius).at(dv(center)).build(),
        Primitive::Torus { center, radius, tube } => Shape::torus().at(dv(center)).radius_1(radius).radius_2(tube).build(),
    };
    built(shape)
}

fn face(profile: &[Seg]) -> Result<Face> {
    let edges: Vec<Edge> = profile
        .iter()
        .map(|s| match *s {
            Seg::Line { a, b } => Edge::segment(dv(a), dv(b)),
            Seg::Arc { a, mid, b } => Edge::arc(dv(a), dv(mid), dv(b)),
            Seg::Circle { center, radius, normal } => Edge::circle(dv(center), dv(normal), radius),
        })
        .collect();
    let wire = Wire::from_edges(&edges);
    Ok(Face::from_wire(&wire))
}

pub fn extrude(profile: &[Seg], dir: Vec3) -> Result<Built> {
    built(face(profile)?.extrude(dv(dir)).into())
}

pub fn revolve(profile: &[Seg], origin: Vec3, axis: Vec3, angle: f64) -> Result<Built> {
    built(face(profile)?.revolve(dv(origin), dv(axis), Some(Angle::Radians(angle))).into())
}

pub fn boolean(op: BoolOp, a: Placed, b: Placed) -> Result<Built> {
    let (sa, sb) = (placed(a)?, placed(b)?);
    let r: Shape = match op {
        BoolOp::Union => sa.union(&sb).into(),
        BoolOp::Subtract => sa.subtract(&sb).into(),
        BoolOp::Intersect => sa.intersect(&sb).into(),
    };
    if r.faces().next().is_none() {
        return Err(KernelError::Failed("the result is empty".into()));
    }
    built(r)
}

pub fn fillet(s: Placed, radius: f64) -> Result<Built> {
    let shape = placed(s)?;
    // ponytail: a radius as large as the smallest box dimension makes OpenCASCADE throw; keep
    // well under it. Upgrade path: per-edge radii and a worker process.
    let bb = bounding_box::aabb(&shape);
    let size = bb.max() - bb.min();
    if radius >= 0.5 * size.x.min(size.y).min(size.z) {
        return Err(KernelError::Invalid("the radius is too large for this solid".into()));
    }
    built(shape.fillet(radius))
}

pub fn tessellate(brep: &str) -> Result<Mesh3> {
    mesh(&read_brep(brep)?)
}

pub fn export_step(solids: &[Placed], path: &Path) -> Result<()> {
    let shapes = solids.iter().map(|s| placed(*s)).collect::<Result<Vec<_>>>()?;
    Shape::write_all_step(&shapes, path).map_err(failed("STEP export"))
}

pub fn export_stl(solids: &[Placed], path: &Path) -> Result<()> {
    let shapes = solids.iter().map(|s| placed(*s)).collect::<Result<Vec<_>>>()?;
    let all: Shape = Compound::from_shapes(&shapes).into();
    all.write_stl_with_tolerance(path, deflection(&all)).map_err(failed("STL export"))
}

pub fn import_step(path: &Path) -> Result<Built> {
    built(Shape::read_step(path).map_err(failed("STEP import"))?)
}

/// B-rep text first (written before meshing, so it carries no triangles), then the mesh.
fn built(shape: Shape) -> Result<Built> {
    if shape.faces().next().is_none() {
        return Err(KernelError::Failed("the shape has no faces".into()));
    }
    let brep = write_brep(&shape)?;
    Ok(Built { brep, mesh: mesh(&shape)? })
}

/// A stored solid in world coordinates.
fn placed(p: Placed) -> Result<Shape> {
    let s = read_brep(p.brep)?;
    let t = similarity(p.xform)
        .ok_or_else(|| KernelError::Invalid("the solid has a transform the kernel can't apply (only move, rotate, scale and mirror)".into()))?;
    let mut s = if t.mirror { s.scaled(DVec3::ZERO, -1.0).rotated(DVec3::Y, std::f64::consts::PI) } else { s };
    if (t.scale - 1.0).abs() > 1e-12 {
        s = s.scaled(DVec3::ZERO, t.scale);
    }
    if t.rotation.abs() > 1e-15 {
        s = s.rotated(DVec3::Z, t.rotation);
    }
    if t.offset != Vec3::ZERO {
        s = s.translated(dv(t.offset));
    }
    Ok(s)
}

static TEMP: AtomicU64 = AtomicU64::new(0);

/// A temporary file path unique to this process and call; removed when dropped.
struct Temp(PathBuf);

impl Temp {
    fn new(ext: &str) -> Temp {
        let n = TEMP.fetch_add(1, Ordering::Relaxed);
        Temp(std::env::temp_dir().join(format!("cadrhino-{}-{n}.{ext}", std::process::id())))
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn write_brep(s: &Shape) -> Result<String> {
    let t = Temp::new("brep");
    s.write_brep_text(&t.0).map_err(failed("B-rep write"))?;
    std::fs::read_to_string(&t.0).map_err(|e| KernelError::Failed(format!("B-rep write: {e}")))
}

fn read_brep(text: &str) -> Result<Shape> {
    if text.trim().is_empty() {
        return Err(KernelError::Invalid("the solid has no B-rep data".into()));
    }
    let t = Temp::new("brep");
    std::fs::write(&t.0, text).map_err(|e| KernelError::Failed(format!("B-rep read: {e}")))?;
    Shape::read_brep_text(&t.0).map_err(failed("B-rep read"))
}

/// Chord deflection for meshing: a thousandth of the shape's size.
fn deflection(s: &Shape) -> f64 {
    let bb = bounding_box::aabb(s);
    let d = (bb.max() - bb.min()).length();
    if d.is_finite() && d > 0.0 { (d * 1e-3).max(1e-7) } else { 1e-3 }
}

/// Triangles face by face, and the edges as the boundaries of each face's triangles (shared
/// edges once). The whole shape is triangulated first so neighbouring faces agree on edges.
fn mesh(shape: &Shape) -> Result<Mesh3> {
    let tol = deflection(shape);
    Mesher::try_new(shape, tol).map_err(failed("meshing"))?;
    let q = tol * 1e-3;
    let key = |p: Vec3| ((p.x / q).round() as i64, (p.y / q).round() as i64, (p.z / q).round() as i64);
    let mut out = Mesh3::default();
    let mut seen = HashSet::new();
    let mut segs = Vec::new();
    for f in shape.faces() {
        let m = Mesher::try_new(&Shape::from(f), tol).map_err(failed("meshing"))?.mesh().map_err(failed("meshing"))?;
        let base = out.verts.len();
        out.verts.extend(m.vertices.iter().map(|p| v3(*p)));
        let mut uses: HashMap<(usize, usize), u32> = HashMap::new();
        for t in m.indices.chunks_exact(3) {
            let idx = |i: usize| u32::try_from(base + i).map_err(|_| KernelError::Failed("the mesh is too large".into()));
            out.tris.push([idx(t[0])?, idx(t[1])?, idx(t[2])?]);
            for (i, j) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *uses.entry((i.min(j), i.max(j))).or_default() += 1;
            }
        }
        for ((i, j), n) in uses {
            let (Some(a), Some(b)) = (m.vertices.get(i), m.vertices.get(j)) else { continue };
            let (a, b) = (v3(*a), v3(*b));
            let (ka, kb) = (key(a), key(b));
            if n == 1 && ka != kb && seen.insert((ka.min(kb), ka.max(kb))) {
                segs.push((a, b));
            }
        }
    }
    out.edges = chain(&segs, key);
    Ok(out)
}

/// Join segments that share end points into polylines.
fn chain<K: std::hash::Hash + Eq + Copy>(segs: &[(Vec3, Vec3)], key: impl Fn(Vec3) -> K) -> Vec<Vec<Vec3>> {
    let mut at: HashMap<K, Vec<usize>> = HashMap::new();
    for (i, (a, b)) in segs.iter().enumerate() {
        at.entry(key(*a)).or_default().push(i);
        at.entry(key(*b)).or_default().push(i);
    }
    let mut used = vec![false; segs.len()];
    let mut out = Vec::new();
    for (i, (a, b)) in segs.iter().enumerate() {
        if used.get(i).copied().unwrap_or(true) {
            continue;
        }
        if let Some(u) = used.get_mut(i) {
            *u = true;
        }
        let mut line = VecDeque::from([*a, *b]);
        for forward in [true, false] {
            loop {
                let Some(end) = (if forward { line.back() } else { line.front() }).copied() else { break };
                let k = key(end);
                let next = at.get(&k).and_then(|v| v.iter().copied().find(|j| !used.get(*j).copied().unwrap_or(true)));
                let Some(j) = next else { break };
                if let Some(u) = used.get_mut(j) {
                    *u = true;
                }
                let Some((c, d)) = segs.get(j).copied() else { break };
                let other = if key(c) == k { d } else { c };
                if forward {
                    line.push_back(other);
                } else {
                    line.push_front(other);
                }
            }
        }
        out.push(line.into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_geom::Mat4;

    fn placed(b: &Built) -> Placed<'_> {
        Placed { brep: &b.brep, xform: &Mat4::IDENTITY }
    }

    #[test]
    fn box_has_twelve_edges_and_twelve_triangles() {
        let b = primitive(&Primitive::Box { a: Vec3::ZERO, b: Vec3::new(10.0, 20.0, 30.0) }).unwrap();
        assert_eq!(b.mesh.tris.len(), 12);
        assert_eq!(b.mesh.edges.iter().map(|e| e.len() - 1).sum::<usize>(), 12);
        assert!(b.brep.contains("CASCADE Topology"));
    }

    #[test]
    fn subtract_and_round_trip_brep() {
        let a = primitive(&Primitive::Box { a: Vec3::ZERO, b: Vec3::new(10.0, 10.0, 10.0) }).unwrap();
        let c = primitive(&Primitive::Cylinder { base: Vec3::new(5.0, 5.0, -1.0), radius: 3.0, height: 12.0 }).unwrap();
        let r = boolean(BoolOp::Subtract, placed(&a), placed(&c)).unwrap();
        assert!(r.mesh.tris.len() > 12);
        let again = tessellate(&r.brep).unwrap();
        assert_eq!(again.tris.len(), r.mesh.tris.len());
    }

    #[test]
    fn moved_solid_is_placed() {
        let a = primitive(&Primitive::Box { a: Vec3::ZERO, b: Vec3::new(1.0, 1.0, 1.0) }).unwrap();
        let mut x = Mat4::IDENTITY;
        x.m[0][3] = 5.0;
        let b = Placed { brep: &a.brep, xform: &x };
        // Disjoint after the move: the intersection is empty.
        assert!(boolean(BoolOp::Intersect, placed(&a), b).is_err());
        assert!(boolean(BoolOp::Union, placed(&a), b).is_ok());
    }

    #[test]
    fn extrude_circle_profile() {
        let b = extrude(&[Seg::Circle { center: Vec3::ZERO, radius: 2.0, normal: Vec3::Z }], Vec3::new(0.0, 0.0, 5.0)).unwrap();
        assert!(b.mesh.tris.len() > 20);
    }
}
