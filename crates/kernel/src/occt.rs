//! The OpenCASCADE backend (through the `opencascade` crate).
//!
//! An operation OpenCASCADE can't do usually comes back as an error or an empty shape, but a C++
//! exception the bindings don't catch (some fillets do this) ends the process: the apps run
//! these functions in a worker process (see `worker`).

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, VecDeque};
use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::hash::BuildHasher;
use std::io::Write;
use std::path::{Path, PathBuf};

use cadcraft_geom::{Mesh3, Vec3};
use glam::{DVec3, dvec3};
use opencascade::angle::Angle;
use opencascade::bounding_box;
use opencascade::mesh::Mesher;
use opencascade::primitives::{Compound, Edge, Face, Shape, ShapeType, Shell, Solid, Wire};

use crate::curve::{Run, closed};
use crate::{BoolOp, Built, Curve, KernelError, Placed, Primitive, Result, Seg, similarity};

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
        Primitive::Wedge { a, b } => {
            // The right triangle at a.y (high side at the smaller x), drawn out to b.y.
            let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
            let (lo, hi, top) = (Vec3::new(x0, a.y, a.z), Vec3::new(x1, a.y, a.z), Vec3::new(x0, a.y, b.z));
            let tri = [Seg::Line { a: lo, b: hi }, Seg::Line { a: hi, b: top }, Seg::Line { a: top, b: lo }];
            face(&tri).extrude(dvec3(0.0, b.y - a.y, 0.0)).into()
        }
        Primitive::Pyramid { base, sides, radius, top_radius, height } => pyramid(base, sides, radius, top_radius, height),
    };
    built(shape)
}

/// A slab round the pyramid trimmed by each side's plane, so every face is flat and the apex is
/// where the side planes meet. A pointed one's slab reaches past the apex: none of its own faces
/// has to shrink to a point (OpenCASCADE handles that badly).
fn pyramid(base: Vec3, sides: u32, r: f64, rt: f64, h: f64) -> Shape {
    let n = f64::from(sides);
    let corner = |radius: f64, k: f64, z: f64| {
        let t = -FRAC_PI_2 + PI / n + TAU * k / n;
        base + Vec3::new(radius * t.cos(), radius * t.sin(), z)
    };
    let wide = 2.0 * r.max(rt);
    let top = if rt == 0.0 { 2.0 * h } else { h };
    let mut s = Shape::box_from_corners(dv(base - Vec3::new(wide, wide, 0.0)), dv(base + Vec3::new(wide, wide, top)));
    let inside = base + Vec3::new(0.0, 0.0, h / 2.0);
    for k in 0..sides {
        let k = f64::from(k);
        let (p, q) = (corner(r, k, 0.0), corner(r, k + 1.0, 0.0));
        let t = if rt == 0.0 { base + Vec3::new(0.0, 0.0, h) } else { corner(rt, k, h) };
        let out = (q - p).cross(t - p);
        let out = if out.dot(inside - p) > 0.0 { -out } else { out };
        s = s.subtract(&half_space(&s, p, out)).into();
    }
    s
}

/// The half-space on `n`'s side of the plane through `p`: a box large enough to hold `s`.
fn half_space(s: &Shape, p: Vec3, n: Vec3) -> Shape {
    let size = match bounds(s) {
        Some((lo, hi)) => 2.0 * (v3((lo + hi) * 0.5) - p).len() + 2.0 * (hi - lo).length(),
        None => 0.0,
    }
    .max(1e-3);
    let b = Shape::box_from_corners(dvec3(-size, -size, 0.0), dvec3(size, size, size));
    let n = n * (1.0 / n.len());
    // Turn its bottom face (on the XY plane, facing +Z inside) square to n.
    let axis = Vec3::Z.cross(n);
    let b = if axis.len() > 1e-12 {
        b.rotated(dv(axis), n.z.clamp(-1.0, 1.0).acos())
    } else if n.z < 0.0 {
        b.rotated(DVec3::X, PI)
    } else {
        b
    };
    b.translated(dv(p))
}

fn edge(s: &Seg) -> Edge {
    match *s {
        Seg::Line { a, b } => Edge::segment(dv(a), dv(b)),
        Seg::Arc { a, mid, b } => Edge::arc(dv(a), dv(mid), dv(b)),
        Seg::Circle { center, radius, normal } => Edge::circle(dv(center), dv(normal), radius),
    }
}

fn wire(c: &Curve) -> Wire {
    match c {
        Curve::Segs(s) => Wire::from_edges(&s.iter().map(edge).collect::<Vec<_>>()),
        Curve::Spline { points, tangents } => {
            let e = Edge::spline_from_points(points.iter().map(|p| dv(*p)), tangents.map(|(a, b)| (dv(a), dv(b))));
            Wire::from_edges([&e])
        }
    }
}

fn face(profile: &[Seg]) -> Face {
    Face::from_wire(&wire(&Curve::Segs(profile.to_vec())))
}

pub fn extrude(profile: &[Seg], dir: Vec3) -> Result<Built> {
    if closed(profile) {
        return built(face(profile).extrude(dv(dir)).into());
    }
    // An open curve makes a surface: swept along the straight path from its start.
    let start = crate::curve::ends(profile.first().ok_or_else(|| KernelError::Invalid("an empty profile".into()))?).map_or(Vec3::ZERO, |e| e.0);
    let path = Wire::from_edges([&Edge::segment(dv(start), dv(start + dir))]);
    built(wire(&Curve::Segs(profile.to_vec())).sweep_along(&path).into())
}

pub fn revolve(profile: &[Seg], origin: Vec3, axis: Vec3, angle: f64) -> Result<Built> {
    built(face(profile).revolve(dv(origin), dv(axis), Some(Angle::Radians(angle))).into())
}

pub fn plane_surface(profile: &[Seg]) -> Result<Built> {
    built(face(profile).into())
}

/// The pieces as one shape: solids fused, surfaces gathered.
fn join(pieces: Vec<Shape>, solid: bool) -> Shape {
    if !solid || pieces.len() < 2 {
        return if pieces.len() == 1 { pieces.into_iter().next().unwrap_or_else(Shape::empty) } else { Compound::from_shapes(pieces).into() };
    }
    let mut it = pieces.into_iter();
    let first = it.next().unwrap_or_else(Shape::empty);
    it.fold(first, |acc, p| acc.union(&p).into())
}

pub fn loft(sections: &[Curve], ruled: bool) -> Result<Built> {
    let wires: Vec<Wire> = sections.iter().map(wire).collect();
    let solid = sections.iter().all(Curve::is_closed);
    let make = |ws: &[Wire]| -> Shape { if solid { Solid::loft(ws).into() } else { Shell::loft(ws).into() } };
    // Ruled: each neighbouring pair lofted on its own (two sections loft straight), then joined.
    let shape = if ruled && wires.len() > 2 { join(wires.windows(2).map(make).collect(), solid) } else { make(&wires) };
    built(shape)
}

pub fn sweep(runs: &[Run], solid: bool) -> Result<Built> {
    let mut pieces = Vec::with_capacity(runs.len());
    for r in runs {
        let path = wire(&r.path);
        let mut s: Shape =
            if solid { Face::from_wire(&wire(&r.profile)).sweep_along(&path).into() } else { wire(&r.profile).sweep_along(&path).into() };
        for (p, n) in &r.cuts {
            let cut = half_space(&s, *p, *n);
            s = s.subtract(&cut).into();
        }
        pieces.push(s);
    }
    built(join(pieces, solid))
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

pub fn slice(s: Placed, origin: Vec3, normal: Vec3) -> Result<Vec<Option<Built>>> {
    let shape = placed(s)?;
    let h = half_space(&shape, origin, normal);
    let piece = |r: Shape| if r.faces().next().is_none() { Ok(None) } else { built(r).map(Some) };
    Ok(vec![piece(shape.intersect(&h).into())?, piece(shape.subtract(&h).into())?])
}

/// The solid to round or bevel, refused where OpenCASCADE would throw.
fn edged(s: Placed, size: f64, what: &str) -> Result<Shape> {
    let shape = placed(s)?;
    // A sphere or a torus (one face) has no edge, and OpenCASCADE throws on that.
    if shape.faces().nth(1).is_none() {
        return Err(KernelError::Invalid(format!("this solid has no edges to {what}")));
    }
    // ponytail: a size as large as the smallest box dimension makes OpenCASCADE throw; keep
    // well under it. Upgrade path: per-edge sizes.
    let d = self::size(&shape).unwrap_or(DVec3::ZERO);
    if size >= 0.5 * d.x.min(d.y).min(d.z) {
        return Err(KernelError::Invalid(format!("the {} is too large for this solid", if what == "round" { "radius" } else { "distance" })));
    }
    Ok(shape)
}

pub fn fillet(s: Placed, radius: f64) -> Result<Built> {
    built(edged(s, radius, "round")?.fillet(radius))
}

pub fn chamfer(s: Placed, distance: f64) -> Result<Built> {
    built(edged(s, distance, "bevel")?.chamfer(distance))
}

pub fn shell(s: Placed, thickness: f64, remove_top: bool) -> Result<Built> {
    let shape = placed(s)?;
    let (lo, hi) = bounds(&shape).ok_or_else(|| KernelError::Invalid("the solid is empty".into()))?;
    let d = hi - lo;
    // ponytail: as for fillets, walls as thick as half the smallest side make OpenCASCADE throw.
    if thickness >= 0.5 * d.x.min(d.y).min(d.z) {
        return Err(KernelError::Invalid("the shell is too thick for this solid".into()));
    }
    let r = if remove_top {
        // The faces lying flat at the top: their own box has no height and sits at the solid's.
        let tol = 1e-5 * d.length().max(1.0);
        let tops: Vec<Face> =
            shape.faces().filter(|f| bounds(&Shape::from(f)).is_some_and(|(flo, fhi)| fhi.z - flo.z <= tol && (hi.z - fhi.z).abs() <= tol)).collect();
        if tops.is_empty() {
            return Err(KernelError::Invalid("this solid has no flat top face to remove".into()));
        }
        shape.hollow(-thickness, tops)
    } else {
        // Closed: the solid less its inward offset, walls round an empty inside.
        shape.subtract(&shape.offset_surface(-thickness)).into()
    };
    built(r)
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

/// A new private folder in the temporary directory for the B-rep files OpenCASCADE reads and
/// writes, removed with them when dropped. Made fresh (never an existing path, so nothing planted
/// at a guessed name in a shared temporary directory is followed) and, on Unix, owner-only.
struct Temp(PathBuf);

impl Temp {
    fn new() -> Result<Temp> {
        let mut last = None;
        for n in 0..8u64 {
            let salt = RandomState::new().hash_one(n);
            let dir = std::env::temp_dir().join(format!("cadrhino-{}-{salt:016x}", std::process::id()));
            #[cfg(unix)]
            let made = std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700).create(&dir);
            #[cfg(not(unix))]
            let made = std::fs::create_dir(&dir);
            match made {
                Ok(()) => return Ok(Temp(dir)),
                Err(e) => last = Some(e),
            }
        }
        Err(KernelError::Failed(format!("no temporary folder: {}", last.map(|e| e.to_string()).unwrap_or_default())))
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_brep(s: &Shape) -> Result<String> {
    let t = Temp::new()?;
    let path = t.0.join("out.brep");
    s.write_brep_text(&path).map_err(failed("B-rep write"))?;
    std::fs::read_to_string(&path).map_err(|e| KernelError::Failed(format!("B-rep write: {e}")))
}

/// A stored shape; one without faces (an empty compound, a null shape) is refused, as nothing
/// can be built on it.
fn read_brep(text: &str) -> Result<Shape> {
    if text.trim().is_empty() {
        return Err(KernelError::Invalid("the solid has no B-rep data".into()));
    }
    let t = Temp::new()?;
    let path = t.0.join("in.brep");
    std::fs::File::create_new(&path).and_then(|mut f| f.write_all(text.as_bytes())).map_err(|e| KernelError::Failed(format!("B-rep read: {e}")))?;
    let s = Shape::read_brep_text(&path).map_err(failed("B-rep read"))?;
    if s.faces().next().is_none() {
        return Err(KernelError::Invalid("the solid has no faces".into()));
    }
    Ok(s)
}

/// The corners of the shape's bounding box; `None` when the box is empty (OpenCASCADE throws on
/// reading an empty box's corners).
fn bounds(s: &Shape) -> Option<(DVec3, DVec3)> {
    let bb = bounding_box::aabb(s);
    (!bb.is_void()).then(|| (bb.min(), bb.max()))
}

/// The size of the shape's bounding box (see [`bounds`]).
fn size(s: &Shape) -> Option<DVec3> {
    bounds(s).map(|(lo, hi)| hi - lo)
}

/// Chord deflection for meshing: 5e-4 of the shape's size, so spheres and tori look round.
/// ponytail: one fixed ratio for every size; triangles grow as 1/deflection (painter sort and
/// picking are CPU-side), a view-dependent level of detail if big models get slow.
fn deflection(s: &Shape) -> f64 {
    let d = size(s).map_or(0.0, |v| v.length());
    if d.is_finite() && d > 0.0 { (d * 5e-4).max(1e-7) } else { 1e-3 }
}

/// Triangles face by face, and the edges as the boundaries of each face's triangles (shared
/// edges once). The whole shape is triangulated first so neighbouring faces agree on edges.
/// It is open (a surface) unless it is a solid or every face boundary is shared: a closed
/// shell's boundaries pair up (a seam with itself), a surface's own edges don't.
fn mesh(shape: &Shape) -> Result<Mesh3> {
    let tol = deflection(shape);
    Mesher::try_new(shape, tol).map_err(failed("meshing"))?;
    let q = tol * 1e-3;
    let key = |p: Vec3| ((p.x / q).round() as i64, (p.y / q).round() as i64, (p.z / q).round() as i64);
    let mut out = Mesh3::default();
    let mut shared: HashMap<_, u32> = HashMap::new();
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
            if n == 1 && ka != kb {
                let uses = shared.entry((ka.min(kb), ka.max(kb))).or_default();
                if *uses == 0 {
                    segs.push((a, b));
                }
                *uses += 1;
            }
        }
    }
    out.edges = chain(&segs, key);
    out.open = !matches!(shape.shape_type(), ShapeType::Solid | ShapeType::CompoundSolid) && shared.values().any(|n| n % 2 == 1);
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
            while let Some(end) = (if forward { line.back() } else { line.front() }).copied() {
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
    fn sphere_and_torus_are_not_filleted() {
        // OpenCASCADE throws on them: they have no edge to round.
        let s = primitive(&Primitive::Sphere { center: Vec3::ZERO, radius: 5.0 }).unwrap();
        assert!(matches!(fillet(placed(&s), 1.0), Err(KernelError::Invalid(_))));
        let t = primitive(&Primitive::Torus { center: Vec3::ZERO, radius: 10.0, tube: 2.0 }).unwrap();
        assert!(matches!(fillet(placed(&t), 0.5), Err(KernelError::Invalid(_))));
    }

    #[test]
    fn empty_shapes_are_refused() {
        // Reading the corners of an empty bounding box throws in OpenCASCADE.
        let empty = Shape::empty();
        assert_eq!(deflection(&empty), 1e-3);
        let text = write_brep(&empty).unwrap();
        assert!(text.contains("CASCADE Topology"));
        assert!(matches!(tessellate(&text), Err(KernelError::Invalid(_))));
        let b = Placed { brep: &text, xform: &Mat4::IDENTITY };
        assert!(fillet(b, 1.0).is_err());
        assert!(export_stl(&[b], &std::env::temp_dir().join("cadrhino-never-written.stl")).is_err());
    }

    #[test]
    fn temporary_folders_are_new_and_removed() {
        let (a, b) = (Temp::new().unwrap(), Temp::new().unwrap());
        assert!(a.0.is_dir() && b.0.is_dir() && a.0 != b.0);
        let p = a.0.clone();
        std::fs::write(p.join("x"), "x").unwrap();
        drop(a);
        assert!(!p.exists());
    }

    #[test]
    fn spheres_are_meshed_finely() {
        let s = primitive(&Primitive::Sphere { center: Vec3::ZERO, radius: 5.0 }).unwrap();
        // About 5 800 at 5e-4 of the size; a thousandth gave half of that, visibly faceted.
        assert!(s.mesh.tris.len() > 4000, "{} triangles", s.mesh.tris.len());
    }

    #[test]
    fn extrude_circle_profile() {
        let b = extrude(&[Seg::Circle { center: Vec3::ZERO, radius: 2.0, normal: Vec3::Z }], Vec3::new(0.0, 0.0, 5.0)).unwrap();
        assert!(b.mesh.tris.len() > 20);
    }
}

/// The modelling operations through the crate's API (checks, then this backend in-process).
#[cfg(test)]
mod modelling_tests {
    use cadcraft_geom::{Mat4, Mesh3, Vec3};

    use crate::{Built, Curve, KernelError, Placed, Primitive, Seg};

    fn placed(b: &Built) -> Placed<'_> {
        Placed { brep: &b.brep, xform: &Mat4::IDENTITY }
    }

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    /// The mesh's bounding box.
    fn bounds(m: &Mesh3) -> (Vec3, Vec3) {
        m.verts.iter().fold((v(f64::MAX, f64::MAX, f64::MAX), v(f64::MIN, f64::MIN, f64::MIN)), |(lo, hi), p| {
            (v(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), v(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
        })
    }

    fn near(a: Vec3, b: Vec3, tol: f64) -> bool {
        (a - b).len() < tol
    }

    fn polygon(pts: &[Vec3]) -> Vec<Seg> {
        (0..pts.len()).map(|i| Seg::Line { a: pts[i], b: pts[(i + 1) % pts.len()] }).collect()
    }

    fn square(half: f64, z: f64) -> Vec<Seg> {
        polygon(&[v(-half, -half, z), v(half, -half, z), v(half, half, z), v(-half, half, z)])
    }

    fn circle(r: f64, z: f64) -> Curve {
        Curve::Segs(vec![Seg::Circle { center: v(0.0, 0.0, z), radius: r, normal: Vec3::Z }])
    }

    fn line(a: Vec3, b: Vec3) -> Curve {
        Curve::Segs(vec![Seg::Line { a, b }])
    }

    #[test]
    fn solids_are_closed_and_surfaces_open() {
        let b = crate::primitive(&Primitive::Box { a: Vec3::ZERO, b: v(10.0, 10.0, 10.0) }).unwrap();
        let c = crate::primitive(&Primitive::Cylinder { base: v(5.0, 5.0, -1.0), radius: 3.0, height: 12.0 }).unwrap();
        let s = crate::primitive(&Primitive::Sphere { center: Vec3::ZERO, radius: 3.0 }).unwrap();
        assert!(!b.mesh.open && !c.mesh.open && !s.mesh.open);
        // A boolean's compound is checked edge by edge.
        let holed = crate::boolean(crate::BoolOp::Subtract, placed(&b), placed(&c)).unwrap();
        assert!(!holed.mesh.open);
        assert!(!crate::tessellate(&holed.brep).unwrap().open, "read back the same");
        let f = crate::fillet(placed(&b), 1.0).unwrap();
        assert!(!crate::tessellate(&f.brep).unwrap().open);
        let flat = crate::plane_surface(&square(5.0, 2.0)).unwrap();
        assert!(flat.mesh.open);
        assert_eq!(flat.mesh.tris.len(), 2);
        assert!(crate::tessellate(&flat.brep).unwrap().open, "a surface read from a file stays open");
    }

    #[test]
    fn wedge_slopes_down_along_x() {
        let w = crate::primitive(&Primitive::Wedge { a: v(10.0, 0.0, 1.0), b: v(0.0, 5.0, 4.0) }).unwrap();
        assert!(!w.mesh.open);
        assert_eq!(w.mesh.tris.len(), 8, "two rectangles, a slope and two triangles");
        let (lo, hi) = bounds(&w.mesh);
        assert!(near(lo, v(0.0, 0.0, 1.0), 1e-9) && near(hi, v(10.0, 5.0, 4.0), 1e-9), "{lo:?} {hi:?}");
        // Full height only at the smaller x.
        assert!(w.mesh.verts.iter().all(|p| p.z < 1.0 + 1e-9 || p.x < 1e-9), "{:?}", w.mesh.verts);
        // Downward.
        let d = crate::primitive(&Primitive::Wedge { a: v(0.0, 0.0, 0.0), b: v(4.0, 4.0, -2.0) }).unwrap();
        assert!((bounds(&d.mesh).0.z + 2.0).abs() < 1e-9);
    }

    #[test]
    fn pyramids_have_flat_faces_and_a_point() {
        let p = crate::primitive(&Primitive::Pyramid { base: v(1.0, 2.0, 3.0), sides: 4, radius: 5.0, top_radius: 0.0, height: 8.0 }).unwrap();
        assert!(!p.mesh.open);
        assert_eq!(p.mesh.tris.len(), 6, "a square and four triangles");
        let (lo, hi) = bounds(&p.mesh);
        let h = 5.0 * std::f64::consts::FRAC_1_SQRT_2;
        assert!(near(lo, v(1.0 - h, 2.0 - h, 3.0), 1e-6) && near(hi, v(1.0 + h, 2.0 + h, 11.0), 1e-6), "{lo:?} {hi:?}");
        // Edges: four round the base, four up to the apex.
        assert_eq!(p.mesh.edges.iter().map(|e| e.len() - 1).sum::<usize>(), 8, "{:?}", p.mesh.edges);
        assert!(p.mesh.verts.iter().any(|q| near(*q, v(1.0, 2.0, 11.0), 1e-6)), "the apex");
        // Many sides; a flat top, downward; a top wider than the base.
        let many = crate::primitive(&Primitive::Pyramid { base: Vec3::ZERO, sides: 32, radius: 5.0, top_radius: 0.0, height: 3.0 }).unwrap();
        assert_eq!(many.mesh.tris.len(), 30 + 32);
        let frustum = crate::primitive(&Primitive::Pyramid { base: Vec3::ZERO, sides: 6, radius: 5.0, top_radius: 2.0, height: -4.0 }).unwrap();
        assert_eq!(frustum.mesh.tris.len(), 4 + 4 + 12);
        assert!((bounds(&frustum.mesh).0.z + 4.0).abs() < 1e-6);
        let up = crate::primitive(&Primitive::Pyramid { base: Vec3::ZERO, sides: 3, radius: 1.0, top_radius: 4.0, height: 2.0 }).unwrap();
        assert!(bounds(&up.mesh).1.y > 3.9);
    }

    #[test]
    fn closed_sections_loft_into_a_solid() {
        let l = crate::loft(&[circle(5.0, 0.0), Curve::Segs(square(3.0, 10.0))], false).unwrap();
        assert!(!l.mesh.open);
        let (lo, hi) = bounds(&l.mesh);
        assert!(lo.z.abs() < 1e-6 && (hi.z - 10.0).abs() < 1e-6);
        // Ruled: straight between sections, with a crease at the middle one.
        let three = [circle(5.0, 0.0), circle(8.0, 5.0), circle(5.0, 10.0)];
        let smooth = crate::loft(&three, false).unwrap();
        let ruled = crate::loft(&three, true).unwrap();
        assert!(!ruled.mesh.open);
        // Halfway up the lower part the ruled loft is halfway between the radii.
        let r = |m: &Mesh3, z: f64| m.verts.iter().filter(|p| (p.z - z).abs() < 0.3).map(|p| p.xy().len()).fold(0.0, f64::max);
        assert!((r(&ruled.mesh, 2.5) - 6.5).abs() < 0.3, "{}", r(&ruled.mesh, 2.5));
        assert!(r(&smooth.mesh, 2.5) > 6.8, "{}", r(&smooth.mesh, 2.5));
    }

    #[test]
    fn open_sections_loft_into_a_surface_without_a_twist() {
        let a = line(v(0.0, 0.0, 0.0), v(10.0, 0.0, 0.0));
        // Drawn the other way: lofted as drawn it would twist.
        let b = line(v(10.0, 5.0, 5.0), v(0.0, 5.0, 5.0));
        let s = crate::loft(&[a.clone(), b], false).unwrap();
        assert!(s.mesh.open);
        assert!(s.mesh.verts.iter().all(|p| (p.y - p.z).abs() < 1e-6), "flat: y = z");
        let arc = Curve::Segs(vec![Seg::Arc { a: v(0.0, 0.0, 5.0), mid: v(5.0, 3.0, 5.0), b: v(10.0, 0.0, 5.0) }]);
        assert!(crate::loft(&[a.clone(), arc, line(v(0.0, 0.0, 9.0), v(10.0, 0.0, 9.0))], true).unwrap().mesh.open);
        // Refused before the kernel: mixed, too few, at one elevation.
        let bad = |r: crate::Result<Built>| matches!(r, Err(KernelError::Invalid(_)));
        assert!(bad(crate::loft(&[a.clone(), circle(2.0, 5.0)], false)));
        assert!(bad(crate::loft(std::slice::from_ref(&a), false)));
        assert!(bad(crate::loft(&[circle(2.0, 1.0), circle(3.0, 1.0)], false)));
    }

    #[test]
    fn sweep_along_an_l_mitres_the_corner() {
        let path =
            Curve::Segs(vec![Seg::Line { a: v(0.0, 0.0, 0.0), b: v(10.0, 0.0, 0.0) }, Seg::Line { a: v(10.0, 0.0, 0.0), b: v(10.0, 10.0, 0.0) }]);
        // A circle lying on the XY plane anywhere: moved to the path's start, turned to face +X.
        let s = crate::sweep(&circle(1.0, 7.0), &path).unwrap();
        assert!(!s.mesh.open);
        let (lo, hi) = bounds(&s.mesh);
        assert!(near(lo, v(0.0, -1.0, -1.0), 1e-3) && near(hi, v(11.0, 10.0, 1.0), 1e-3), "{lo:?} {hi:?}");
        // A square tube is mitred: its outer corner is sharp at (11, -1).
        let tube = crate::sweep(&Curve::Segs(square(1.0, 0.0)), &path).unwrap();
        assert!(!tube.mesh.open);
        assert!(tube.mesh.verts.iter().any(|p| near(*p, v(11.0, -1.0, 1.0), 1e-6)), "{:?}", bounds(&tube.mesh));
        // An open profile round the corner: two trimmed surfaces.
        let wall = crate::sweep(&line(v(0.0, 0.0, 0.0), v(0.0, 0.0, 2.0)), &path).unwrap();
        assert!(wall.mesh.open);
        let (lo, hi) = bounds(&wall.mesh);
        assert!(near(lo, v(0.0, 0.0, -1.0), 1e-6) && near(hi, v(10.0, 10.0, 1.0), 1e-6), "{lo:?} {hi:?}");
    }

    #[test]
    fn sweep_round_a_closed_path_mitres_where_it_closes() {
        // A square frame: a square tube round a 20 × 20 square.
        let sq = [v(0.0, 0.0, 0.0), v(20.0, 0.0, 0.0), v(20.0, 20.0, 0.0), v(0.0, 20.0, 0.0)];
        let frame = crate::sweep(&Curve::Segs(square(1.0, 0.0)), &Curve::Segs(polygon(&sq))).unwrap();
        assert!(!frame.mesh.open);
        let (lo, hi) = bounds(&frame.mesh);
        assert!(near(lo, v(-1.0, -1.0, -1.0), 1e-6) && near(hi, v(21.0, 21.0, 1.0), 1e-6), "{lo:?} {hi:?}");
        // The inner corner where the path closes is sharp too.
        assert!(frame.mesh.verts.iter().any(|p| near(*p, v(1.0, 1.0, 1.0), 1e-6)));
    }

    #[test]
    fn sweep_along_a_helix_and_an_open_profile() {
        // Two turns of radius 10 rising 10, sampled as the engine samples a helix.
        let n = 64;
        let turn = std::f64::consts::TAU * 2.0;
        let pt = |t: f64| v(10.0 * (turn * t).cos(), 10.0 * (turn * t).sin(), 10.0 * t);
        let d = |t: f64| v(-10.0 * turn * (turn * t).sin(), 10.0 * turn * (turn * t).cos(), 10.0);
        let helix = Curve::Spline { points: (0..=n).map(|i| pt(f64::from(i) / f64::from(n))).collect(), tangents: Some((d(0.0), d(1.0))) };
        let spring = crate::sweep(&circle(1.0, 0.0), &helix).unwrap();
        assert!(!spring.mesh.open);
        let (lo, hi) = bounds(&spring.mesh);
        assert!(lo.z > -1.5 && hi.z < 11.5 && hi.x > 10.5 && hi.x < 11.5, "{lo:?} {hi:?}");
        // An open profile sweeps a surface, centred on the path.
        let wall = crate::sweep(&line(v(0.0, 0.0, 0.0), v(2.0, 0.0, 0.0)), &line(Vec3::ZERO, v(0.0, 0.0, 5.0))).unwrap();
        assert!(wall.mesh.open);
        let (lo, hi) = bounds(&wall.mesh);
        assert!(near(lo, v(-1.0, 0.0, 0.0), 1e-6) && near(hi, v(1.0, 0.0, 5.0), 1e-6), "{lo:?} {hi:?}");
    }

    #[test]
    fn open_curves_extrude_into_surfaces() {
        let l = crate::extrude(&[Seg::Line { a: v(0.0, 0.0, 0.0), b: v(4.0, 0.0, 0.0) }], v(0.0, 0.0, 3.0)).unwrap();
        assert!(l.mesh.open);
        assert_eq!(l.mesh.tris.len(), 2);
        let (lo, hi) = bounds(&l.mesh);
        assert!(near(lo, Vec3::ZERO, 1e-9) && near(hi, v(4.0, 0.0, 3.0), 1e-9));
        let bent =
            [Seg::Line { a: v(0.0, 0.0, 0.0), b: v(4.0, 0.0, 0.0) }, Seg::Arc { a: v(4.0, 0.0, 0.0), mid: v(6.0, 2.0, 0.0), b: v(4.0, 4.0, 0.0) }];
        assert!(crate::extrude(&bent, v(0.0, 0.0, -2.0)).unwrap().mesh.open);
        // A closed profile still makes a solid; a line along itself makes nothing.
        assert!(!crate::extrude(&square(1.0, 0.0), v(0.0, 0.0, 2.0)).unwrap().mesh.open);
        assert!(matches!(crate::extrude(&[Seg::Line { a: Vec3::ZERO, b: v(0.0, 0.0, 1.0) }], v(0.0, 0.0, 3.0)), Err(KernelError::Invalid(_))));
    }

    #[test]
    fn chamfer_and_shell_a_box() {
        let b = crate::primitive(&Primitive::Box { a: Vec3::ZERO, b: v(10.0, 10.0, 10.0) }).unwrap();
        let c = crate::chamfer(placed(&b), 1.0).unwrap();
        assert!(!c.mesh.open);
        // Six faces, twelve bevels and eight corner triangles.
        assert_eq!(c.mesh.tris.len(), 12 + 24 + 8);
        assert!(matches!(crate::chamfer(placed(&b), 6.0), Err(KernelError::Invalid(_))));
        let s = crate::primitive(&Primitive::Sphere { center: Vec3::ZERO, radius: 2.0 }).unwrap();
        assert!(matches!(crate::chamfer(placed(&s), 0.5), Err(KernelError::Invalid(_))));
        // Hollowed with the top open: a cup as tall as the box.
        let cup = crate::shell(placed(&b), 1.0, true).unwrap();
        assert!(!cup.mesh.open);
        let (lo, hi) = bounds(&cup.mesh);
        assert!(near(lo, Vec3::ZERO, 1e-6) && near(hi, v(10.0, 10.0, 10.0), 1e-6));
        assert!(cup.mesh.verts.iter().any(|p| near(*p, v(1.0, 1.0, 1.0), 1e-6)), "the inside floor");
        // Closed: walls round an empty inside (the inner box's faces are there too).
        let closed = crate::shell(placed(&b), 1.0, false).unwrap();
        assert_eq!(closed.mesh.tris.len(), 24);
        assert!(matches!(crate::shell(placed(&b), 6.0, true), Err(KernelError::Invalid(_))));
        assert!(matches!(crate::shell(placed(&s), 0.5, true), Err(KernelError::Invalid(m)) if m.contains("top")));
    }

    #[test]
    fn slice_cuts_in_two() {
        let b = crate::primitive(&Primitive::Box { a: Vec3::ZERO, b: v(10.0, 4.0, 4.0) }).unwrap();
        let [plus, minus] = crate::slice(placed(&b), v(3.0, 0.0, 0.0), v(1.0, 0.0, 0.0)).unwrap();
        let (plus, minus) = (plus.unwrap(), minus.unwrap());
        assert!(!plus.mesh.open && !minus.mesh.open);
        let ((plo, phi), (mlo, mhi)) = (bounds(&plus.mesh), bounds(&minus.mesh));
        assert!(near(plo, v(3.0, 0.0, 0.0), 1e-6) && near(phi, v(10.0, 4.0, 4.0), 1e-6));
        assert!(near(mlo, Vec3::ZERO, 1e-6) && near(mhi, v(3.0, 4.0, 4.0), 1e-6));
        // A plane that misses leaves one side empty.
        let [plus, minus] = crate::slice(placed(&b), v(20.0, 0.0, 0.0), v(1.0, 0.0, 0.0)).unwrap();
        assert!(plus.is_none() && minus.is_some());
    }
}
