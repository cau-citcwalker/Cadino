//! The OpenCASCADE backend (through the `opencascade` crate).
//!
//! An operation OpenCASCADE can't do usually comes back as an error or an empty shape, but a C++
//! exception the bindings don't catch (some fillets do this) ends the process: the apps run
//! these functions in a worker process (see `worker`).

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, HashSet, VecDeque};
use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::hash::BuildHasher;
use std::io::Write;
use std::path::{Path, PathBuf};

use cadcraft_geom::{Mat4, Mesh3, Vec3};
use glam::{DVec3, dvec3};
use opencascade::angle::Angle;
use opencascade::bounding_box;
use opencascade::mesh::Mesher;
use opencascade::primitives::{Compound, Edge, Face, Shape, ShapeType, Shell, Solid, Wire};

use crate::curve::{Cut, Run, closed, unit};
use crate::{BoolOp, Built, Curve, Imported, KernelError, Outline, Placed, Primitive, Result, Seg, similarity};

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
        s = s.subtract(&half_space(p, out, holding(&s, p))).into();
    }
    s
}

/// How far from `p` a [`half_space`] must reach to take in all of `s`.
fn holding(s: &Shape, p: Vec3) -> f64 {
    match bounds(s) {
        Some((lo, hi)) => 2.0 * (v3((lo + hi) * 0.5) - p).len() + 2.0 * (hi - lo).length(),
        None => 1e-3,
    }
}

/// The half-space on `n`'s side of the plane through `p`, as far as `size` from `p` across the
/// plane and along `n`: a box, so it takes in everything on that side within `size` of `p`.
fn half_space(p: Vec3, n: Vec3, size: f64) -> Shape {
    let size = if size.is_finite() { size.max(1e-6) } else { 1e-3 };
    // Its bottom face (on the XY plane, facing +Z inside) turned square to n.
    turned(Shape::box_from_corners(dvec3(-size, -size, 0.0), dvec3(size, size, size)), n).translated(dv(p))
}

/// The shape turned about the origin so that +Z points along `n`.
fn turned(b: Shape, n: Vec3) -> Shape {
    let Some(n) = unit(n) else { return b };
    let axis = Vec3::Z.cross(n);
    if axis.len() > 1e-12 {
        b.rotated(dv(axis), n.z.clamp(-1.0, 1.0).acos())
    } else if n.z < 0.0 {
        b.rotated(DVec3::X, PI)
    } else {
        b
    }
}

/// What a sweep stretch loses at a corner: past the mitre plane, only where the stretch
/// overshoots the corner. That is a box along the stretch from just before the corner (its inner
/// side pokes past the plane there, at most reach × tan(turn / 2) back) to the end of its
/// extension, and two profile reaches either side of the path.
fn cutter(c: &Cut) -> Shape {
    let (w, back, on) = (2.0 * c.reach, c.past, c.past + c.reach);
    let b = turned(Shape::box_from_corners(dvec3(-w, -w, -back), dvec3(w, w, on)), c.along).translated(dv(c.at));
    b.intersect(&half_space(c.at, c.normal, 2.0 * (w + back + on))).into()
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

/// ponytail: a ruled loft of more than two sections is pairwise lofts fused, with a crease at
/// each inner section (as AutoCAD's Ruled); one ruled loft call if the bindings expose it.
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
        for c in &r.cuts {
            s = s.subtract(&cutter(c)).into();
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
    let h = half_space(origin, normal, holding(&shape, origin));
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
    let _one = exchanging();
    Shape::write_all_step(&shapes, path).map_err(failed("STEP export"))
}

pub fn export_stl(solids: &[Placed], path: &Path) -> Result<()> {
    let shapes = solids.iter().map(|s| placed(*s)).collect::<Result<Vec<_>>>()?;
    let all: Shape = Compound::from_shapes(&shapes).into();
    all.write_stl_with_tolerance(path, deflection(&all)).map_err(failed("STL export"))
}

pub fn import_step(path: &Path) -> Result<Built> {
    let shape = {
        let _one = exchanging();
        Shape::read_step(path).map_err(failed("STEP import"))?
    };
    built(shape)
}

/// One STEP or IGES file at a time in this process: OpenCASCADE's data exchange keeps global
/// state (two at once crash). The apps run each in a worker process of its own anyway.
fn exchanging() -> std::sync::MutexGuard<'static, ()> {
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Solids (as trimmed surfaces: OpenCASCADE's default IGES mode, in millimetres) and curves.
pub fn export_iges(solids: &[Placed], curves: &[Curve], path: &Path) -> Result<()> {
    let mut shapes = solids.iter().map(|s| placed(*s)).collect::<Result<Vec<_>>>()?;
    shapes.extend(curves.iter().map(|c| Shape::from(wire(c))));
    let all: Shape = Compound::from_shapes(&shapes).into();
    let _one = exchanging();
    all.write_iges(path).map_err(failed("IGES export"))
}

/// Most points the free curves of one IGES file may come back with (hostile files).
const MAX_CURVE_POINTS: usize = 1_000_000;

/// The faces in lumps, and the free edges chained into polylines. A lump is one object as
/// OpenCASCADE read it (a solid, a shell, or a group of faces: IGES writers group each solid's
/// trimmed surfaces) when its faces make one piece ([`objects`], [`shells`]), so objects that
/// touch along a whole face stay apart and whole.
/// ponytail: opencascade 0.3 has no sewing and no sub-shape iterator, so the grouping is read
/// from the shape's own B-rep text, and a lump is a compound of faces (booleans on it may fail).
/// Faces a file leaves ungrouped (or in groups of one face, or of pieces apart) are joined along
/// edges whose ends match that exactly two of them share, so such objects that touch along a face
/// come in split. Free edges that meet end to end are joined into one polyline, even when the
/// file kept them as separate curves. Upgrade path: TopoDS_Iterator and BRepBuilderAPI_Sewing in
/// the bindings.
pub fn import_iges(path: &Path) -> Result<Imported> {
    let shape = {
        let _one = exchanging();
        Shape::read_iges(path).map_err(failed("IGES import"))?
    };
    imported(&shape)
}

/// Largest step of the grid IGES import matches points on (a micron in millimetres, which
/// OpenCASCADE reads files in): far above the copies' differences, far below any feature.
const MAX_MATCH: f64 = 1e-3;

/// [`import_iges`] of the shape read.
fn imported(shape: &Shape) -> Result<Imported> {
    // Points this close are one (a hundred-thousandth of the model's size, at most a micron, so an
    // object far from the rest can't coarsen everything else's): faces written as trimmed
    // surfaces each have their own copy of a shared edge.
    // ponytail: one grid for the whole file; each edge's own tolerance (BRep_Tool::Tolerance)
    // when the bindings expose it.
    let q = (deflection(shape) * 0.02).min(MAX_MATCH);
    let key = |p: Vec3| ((p.x / q).round() as i64, (p.y / q).round() as i64, (p.z / q).round() as i64);
    let ends = |e: &Edge| {
        let (a, b) = (key(v3(e.start_point())), key(v3(e.end_point())));
        (a.min(b), a.max(b))
    };
    let faces: Vec<Face> = shape.faces().collect();
    // Each face's edges by their ends.
    let mut bounding: HashMap<_, Vec<(usize, Edge)>> = HashMap::new();
    for (i, f) in faces.iter().enumerate() {
        for e in f.edges() {
            bounding.entry(ends(&e)).or_default().push((i, e));
        }
    }
    // The objects the file grouped are one lump each (their first face is the lump's root), when
    // their faces make one piece.
    let (owner, solids) = write_brep(shape).ok().and_then(|t| objects(&t, faces.len())).unwrap_or_else(|| (vec![None; faces.len()], HashSet::new()));
    let on_edges: Vec<Vec<usize>> = bounding.values().map(|v| v.iter().map(|(f, _)| *f).collect()).collect();
    let owner = shells(owner, &on_edges, &solids);
    let mut lump: Vec<usize> = (0..faces.len()).collect();
    let mut first: HashMap<usize, usize> = HashMap::new();
    for (i, o) in owner.iter().enumerate() {
        if let (Some(o), Some(l)) = (o, lump.get_mut(i)) {
            *l = *first.entry(*o).or_insert(i);
        }
    }
    // Ungrouped faces meeting along an edge two of them share are one lump; an edge more share
    // (two panels butted flush) joins none of them.
    let loose = |f: &usize| owner.get(*f).is_some_and(Option::is_none);
    for on in bounding.values() {
        let on: Vec<usize> = on.iter().map(|(f, _)| *f).filter(loose).collect();
        if let [a, rest @ ..] = on.as_slice()
            && let Some(b) = rest.iter().find(|b| *b != a)
            && on.iter().all(|f| f == a || f == b)
        {
            let (ra, rb) = (root(&mut lump, *a), root(&mut lump, *b));
            if let Some(r) = lump.get_mut(ra) {
                *r = rb;
            }
        }
    }
    let mut groups: Vec<Vec<Shape>> = Vec::new();
    let mut slot: HashMap<usize, usize> = HashMap::new();
    for (i, f) in faces.iter().enumerate() {
        let n = *slot.entry(root(&mut lump, i)).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        if let Some(g) = groups.get_mut(n) {
            g.push(Shape::from(f));
        }
    }
    let shapes = enclose(groups.into_iter().map(|g| built(Compound::from_shapes(&g).into())).collect::<Result<Vec<_>>>()?)?;
    // The free edges: every edge, less one per face's edge with the same ends; where a free edge
    // has the ends of a face's edge (an arc drawn corner to corner over a panel), by its points.
    let points = |e: &Edge| -> Vec<Vec3> { e.approximation_segments().map(v3).collect() };
    let shape_of = |e: &Edge| {
        let mut k: Vec<_> = points(e).into_iter().map(key).collect();
        k.sort_unstable();
        k
    };
    let all: Vec<Edge> = shape.edges().collect();
    let mut count: HashMap<_, usize> = HashMap::new();
    for e in &all {
        *count.entry(ends(e)).or_default() += 1;
    }
    let mut left: HashMap<_, Vec<Vec<(i64, i64, i64)>>> = HashMap::new();
    let (mut segs, mut total) = (Vec::new(), 0usize);
    for e in all {
        let k = ends(&e);
        let on_faces = bounding.get(&k).map_or(&[][..], Vec::as_slice);
        if !on_faces.is_empty() {
            if count.get(&k).copied().unwrap_or(0) <= on_faces.len() {
                continue;
            }
            let shapes_left = left.entry(k).or_insert_with(|| on_faces.iter().map(|(_, f)| shape_of(f)).collect());
            let mine = shape_of(&e);
            if let Some(i) = shapes_left.iter().position(|s| *s == mine) {
                shapes_left.swap_remove(i);
                continue;
            }
        }
        let pts = points(&e);
        total = total.saturating_add(pts.len());
        if total > MAX_CURVE_POINTS {
            return Err(KernelError::Invalid("the file's curves are too large to import".into()));
        }
        segs.extend(pts.windows(2).filter_map(|w| match *w {
            [a, b] if key(a) != key(b) => Some((a, b)),
            _ => None,
        }));
    }
    let mut curves = chain(&segs, key, &std::collections::HashSet::new());
    // A closed one ends exactly where it starts.
    for c in &mut curves {
        let long = c.len() > 2;
        if let (Some(a), Some(b)) = (c.first().copied(), c.last_mut())
            && long
            && key(a) == key(*b)
        {
            *b = a;
        }
    }
    if shapes.is_empty() && curves.is_empty() {
        return Err(KernelError::Invalid("the file has no surfaces or curves".into()));
    }
    Ok(Imported { shapes, curves })
}

/// Most void-against-lump checks [`enclose`] makes (hostile files).
const MAX_ENCLOSE: usize = 100_000_000;

/// The signed volume a closed mesh encloses: negative when its faces point inward.
fn volume(m: &Mesh3) -> f64 {
    m.tris.iter().filter_map(|t| m.tri(t)).map(|[a, b, c]| a.dot(b.cross(c))).sum::<f64>() / 6.0
}

/// The box of a mesh's vertices; `None` without finite ones.
fn mesh_box(m: &Mesh3) -> Option<(Vec3, Vec3)> {
    let mut pts = m.verts.iter().filter(|p| p.is_finite());
    let first = *pts.next()?;
    Some(pts.fold((first, first), |(lo, hi), p| {
        (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
    }))
}

/// The lumps with each void (a closed lump facing inward: a hollow solid's inner shell, which an
/// IGES file written in faces mode keeps apart from its outer one) put back into the smallest
/// closed lump around it, so a hollow solid comes back as one solid, facing outward.
/// ponytail: "around" by bounding boxes, every void against every lump (capped); a void inside no
/// lump stays apart, inside-out (reversing faces needs TopoDS_Shape::Reverse in the bindings).
fn enclose(mut lumps: Vec<Built>) -> Result<Vec<Built>> {
    let info: Vec<(f64, Option<(Vec3, Vec3)>)> = lumps.iter().map(|b| (if b.mesh.open { 0.0 } else { volume(&b.mesh) }, mesh_box(&b.mesh))).collect();
    let voids: Vec<usize> = info.iter().enumerate().filter(|(_, (v, _))| *v < 0.0).map(|(i, _)| i).collect();
    if voids.is_empty() || voids.len().saturating_mul(lumps.len()) > MAX_ENCLOSE {
        return Ok(lumps);
    }
    let within =
        |a: (Vec3, Vec3), b: (Vec3, Vec3)| a.0.x >= b.0.x && a.0.y >= b.0.y && a.0.z >= b.0.z && a.1.x <= b.1.x && a.1.y <= b.1.y && a.1.z <= b.1.z;
    let mut into: HashMap<usize, Vec<usize>> = HashMap::new();
    for v in voids {
        let Some(vb) = info.get(v).and_then(|i| i.1) else { continue };
        let holder = info
            .iter()
            .enumerate()
            .filter(|(_, (vol, b))| *vol > 0.0 && b.is_some_and(|b| within(vb, b)))
            .min_by(|a, b| a.1.0.total_cmp(&b.1.0))
            .map(|(i, _)| i);
        if let Some(h) = holder {
            into.entry(h).or_default().push(v);
        }
    }
    // Each holder made again with its voids' faces, read from their B-rep text (written before
    // meshing, so it carries no triangulation).
    for (h, vs) in &into {
        let mut faces: Vec<Shape> = Vec::new();
        for i in std::iter::once(h).chain(vs) {
            let Some(b) = lumps.get(*i) else { continue };
            faces.extend(read_brep(&b.brep)?.faces().map(|f| Shape::from(&f)));
        }
        let whole = built(Compound::from_shapes(&faces).into())?;
        if let Some(l) = lumps.get_mut(*h) {
            *l = whole;
        }
    }
    let gone: HashSet<usize> = into.values().flatten().copied().collect();
    Ok(lumps.into_iter().enumerate().filter(|(i, _)| !gone.contains(i)).map(|(_, b)| b).collect())
}

/// `owner` (see [`objects`]) keeping only the groups of two or more faces that join along shared
/// edges (`on_edges`: the faces on each edge) into one piece, a shell: a group of one face, or of
/// pieces apart (a writer that groups each trimmed surface on its own, or unrelated surfaces
/// together), gives its faces up to be joined along edges as ungrouped faces are.
/// ponytail: a polysurface written as one group per surface comes back whole only where its
/// surfaces share exact edges with no third face on them (ungrouped-face joining); sewing in the
/// bindings (BRepBuilderAPI_Sewing) when such files touch other objects along a face.
fn shells(mut owner: Vec<Option<usize>>, on_edges: &[Vec<usize>], solids: &HashSet<usize>) -> Vec<Option<usize>> {
    let mut piece: Vec<usize> = (0..owner.len()).collect();
    for on in on_edges {
        // Each face of a group on this edge joins the group's first face on it.
        let mut first: HashMap<usize, usize> = HashMap::new();
        for f in on {
            let Some(Some(o)) = owner.get(*f) else { continue };
            let g = *first.entry(*o).or_insert(*f);
            let (ra, rb) = (root(&mut piece, g), root(&mut piece, *f));
            if let Some(r) = piece.get_mut(ra) {
                *r = rb;
            }
        }
    }
    // Each group's face count, and whether its faces are one piece.
    let mut groups: HashMap<usize, (usize, usize, bool)> = HashMap::new();
    for f in 0..owner.len() {
        let Some(Some(o)) = owner.get(f).copied() else { continue };
        let r = root(&mut piece, f);
        let g = groups.entry(o).or_insert((0, r, true));
        g.0 += 1;
        g.2 &= g.1 == r;
    }
    for o in &mut owner {
        // A solid stays one object whatever its shells (a hollow one has two that don't touch).
        if o.is_some_and(|g| !solids.contains(&g) && !groups.get(&g).is_some_and(|(n, _, one)| *n >= 2 && *one)) {
            *o = None;
        }
    }
    owner
}

/// The lump `i` is in (see [`import_iges`]), halving the path there as it goes.
fn root(lump: &mut [usize], mut i: usize) -> usize {
    // Each step goes to a different entry; a cycle can't form, but stop after every entry anyway.
    for _ in 0..lump.len() {
        let Some(&p) = lump.get(i) else { break };
        if p == i {
            break;
        }
        let up = lump.get(p).copied().unwrap_or(p);
        if let Some(l) = lump.get_mut(i) {
            *l = up;
        }
        i = up;
    }
    i
}

/// Which object each of a shape's `faces` faces (in [`Shape::faces`] order) is in, read from the
/// shape's B-rep text (its shape table lists every sub-shape with its children, in the order the
/// face explorer walks them): `Some(n)` for the n-th solid, shell or compound of faces under the
/// top of the shape (single-child compounds round it unwrapped), `None` for a face on its own.
/// `None` when the text doesn't read that way or doesn't account for exactly `faces` faces.
fn objects(text: &str, faces: usize) -> Option<(Vec<Option<usize>>, HashSet<usize>)> {
    const KINDS: [&str; 8] = ["Ve", "Ed", "Wi", "Fa", "Sh", "So", "CS", "Co"];
    let mut lines = text.lines().map(str::trim);
    let n: usize = lines.find_map(|l| l.strip_prefix("TShapes"))?.trim().parse().ok()?;
    // A reference counts back from the end of the table (1 is the last shape written).
    let at = |r: &str, done: usize| -> Option<usize> {
        let k: usize = r.trim_start_matches(['+', '-', 'i', 'e']).parse().ok()?;
        n.checked_sub(k).filter(|p| *p < done)
    };
    // Each shape: its kind, then its geometry up to a blank line, its flags, and its children
    // (written before it) up to `*`. Hostile sizes: every shape takes a few bytes of the text.
    let mut recs: Vec<(&str, Vec<usize>)> = Vec::with_capacity(n.min(text.len() / 8));
    for _ in 0..n {
        let kind = lines.next().filter(|k| KINDS.contains(k))?;
        lines.find(|l| l.is_empty())?;
        lines.next()?;
        let mut kids = Vec::new();
        'kids: loop {
            let mut toks = lines.next()?.split_whitespace();
            while let Some(t) = toks.next() {
                if t == "*" {
                    break 'kids;
                }
                kids.push(at(t, recs.len())?);
                toks.next()?;
            }
        }
        recs.push((kind, kids));
    }
    let top = lines.find(|l| !l.is_empty())?.split_whitespace().next()?;
    let mut top = at(top, recs.len())?;
    // Faces under each shape, counted as the explorer meets them (a shared one each time).
    let mut count: Vec<usize> = Vec::with_capacity(recs.len());
    for (kind, kids) in &recs {
        let c = if *kind == "Fa" { 1 } else { kids.iter().fold(0usize, |a, k| a.saturating_add(count.get(*k).copied().unwrap_or(0))) };
        count.push(c);
    }
    // Children come before their parent, so this ends.
    while let Some(("Co", [kid])) = recs.get(top).map(|(k, v)| (*k, v.as_slice()))
        && recs.get(*kid).is_some_and(|(k, _)| *k != "Fa")
    {
        top = *kid;
    }
    let parts = match recs.get(top)? {
        ("Co", kids) => kids.clone(),
        _ => vec![top],
    };
    if parts.iter().fold(0usize, |a, p| a.saturating_add(count.get(*p).copied().unwrap_or(0))) != faces {
        return None;
    }
    let mut out = Vec::with_capacity(faces);
    // Objects that are solids: one object even when their shells don't touch (a hollow solid).
    let mut solids = HashSet::new();
    let mut object = 0;
    for p in parts {
        match (recs.get(p)?.0, count.get(p).copied().unwrap_or(0)) {
            (_, 0) => {}
            ("Fa", _) => out.push(None),
            (kind, c) => {
                if kind == "So" {
                    solids.insert(object);
                }
                out.extend(std::iter::repeat_n(Some(object), c));
                object += 1;
            }
        }
    }
    Some((out, solids))
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
    place_owned(read_brep(p.brep)?, p.xform)
}

/// A copy of `s` moved by `xform`.
/// [`place`] for a shape used once: with no scale or mirror to apply it is moved as it is,
/// without the full copy a unit scale would make (every placed solid goes through here).
fn place_owned(s: Shape, xform: &Mat4) -> Result<Shape> {
    let t = similarity(xform)
        .ok_or_else(|| KernelError::Invalid("the solid has a transform the kernel can't apply (only move, rotate, scale and mirror)".into()))?;
    if t.mirror || (t.scale - 1.0).abs() > 1e-12 {
        return place(&s, xform);
    }
    let mut s = s;
    if t.angle.abs() > 1e-15 {
        s = s.rotated(dv(t.axis), t.angle);
    }
    if t.offset != Vec3::ZERO {
        s = s.translated(dv(t.offset));
    }
    Ok(s)
}

fn place(s: &Shape, xform: &Mat4) -> Result<Shape> {
    let t = similarity(xform)
        .ok_or_else(|| KernelError::Invalid("the solid has a transform the kernel can't apply (only move, rotate, scale and mirror)".into()))?;
    // Mirrored across the XZ plane: through the origin, then half a turn about Y.
    let k = if (t.scale - 1.0).abs() > 1e-12 { t.scale } else { 1.0 };
    let mut s = s.scaled(DVec3::ZERO, if t.mirror { -k } else { k });
    if t.mirror {
        s = s.rotated(DVec3::Y, PI);
    }
    if t.angle.abs() > 1e-15 {
        s = s.rotated(dv(t.axis), t.angle);
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

/// [`crate::Outline`] of stored B-rep text.
pub fn outline(brep: &str) -> Result<Outline> {
    let (mesh, smooth) = meshed(&read_brep(brep)?, true)?;
    Ok(Outline { mesh, smooth })
}

/// OpenCASCADE's box of the shape's geometry, without the tolerance it pads a box with: exact
/// for planes, cylinders, cones, spheres and tori, but a free-form (B-spline) face's or curve's
/// is its control points', too large. Taken before the shape is meshed, or it boxes the mesh.
fn geometry_box(s: &Shape) -> Option<(Vec3, Vec3)> {
    let bb = bounding_box::aabb(s);
    if bb.is_void() {
        return None;
    }
    let (lo, hi) = (bb.min(), bb.max());
    // Never more than half the box off (a tiny shape keeps a box).
    let pad = bb.gap_vec().min((hi - lo) * 0.5).max(DVec3::ZERO);
    let (lo, hi) = (v3(lo + pad), v3(hi - pad));
    (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

/// How fine, against the shape's size, the mesh is that [`boxes`] checks OpenCASCADE's boxes
/// with: each side of a free-form part 300 mm across within about 0.015 mm (its mesh takes a few
/// seconds; finer gets slow fast).
const BOX_DEFLECTION: f64 = 1e-4;

/// The box of the stored shape under each transform (see [`crate::boxes`]); `None` for one that
/// isn't affine or can't be measured. OpenCASCADE's box ([`geometry_box`], under a transform that
/// isn't a similarity the box round its turned corners) holds the shape, and so does a fine
/// mesh's box grown by its deflection (the mesh's points lie on the shape, which lies within the
/// deflection of it). Each side is OpenCASCADE's where it lies within the mesh's reach, so
/// analytic faces measure exactly; else the middle of that reach, so free-form ones measure within
/// the deflection (half of it unless their control points lie close), never by their control
/// points.
/// ponytail: one mesh of the whole shape at 1e-4 of its size, as BRepBndLib's AddOptimal isn't
/// in the bindings: a free-form part a metre across within about 0.1 mm a side, and most of a
/// second for a lofted seat (once per B-rep and turn: [`crate::boxes`] keeps what it measured).
/// Extrema refined on the surface (Newton from the mesh's furthest point) when that is too coarse
/// or too slow.
pub fn boxes(brep: &str, xforms: &[Mat4]) -> Vec<Option<(Vec3, Vec3)>> {
    let got = measure(brep, xforms).unwrap_or_default();
    (0..xforms.len()).map(|i| got.get(i).copied().flatten()).collect()
}

fn measure(brep: &str, xforms: &[Mat4]) -> Result<Vec<Option<(Vec3, Vec3)>>> {
    let s = read_brep(brep)?;
    let Some(model) = geometry_box(&s) else { return Err(KernelError::Invalid("the solid is empty".into())) };
    let outer: Vec<Option<(Vec3, Vec3)>> = xforms
        .iter()
        .map(|x| match similarity(x) {
            Some(_) => place(&s, x).ok().and_then(|p| geometry_box(&p)),
            None if x.is_affine() => {
                let (lo, hi) = model;
                points_box((0..8).map(|i| x.apply(Vec3::new(pick(i & 1, lo.x, hi.x), pick(i & 2, lo.y, hi.y), pick(i & 4, lo.z, hi.z)))))
            }
            None => None,
        })
        .collect();
    let d = ((model.1 - model.0).len() * BOX_DEFLECTION).max(1e-7);
    let pts = Mesher::try_new(&s, d).map_err(failed("meshing"))?.mesh().map_err(failed("meshing"))?.vertices;
    Ok(xforms
        .iter()
        .zip(outer)
        .map(|(x, o)| {
            let (olo, ohi) = o?;
            let (mlo, mhi) = points_box(pts.iter().map(|p| x.apply(v3(*p))))?;
            // The deflection as the transform stretches it along each axis (its row's length).
            let e = |i: usize| d * x.m.get(i).map_or(0.0, |r| r.iter().take(3).map(|v| v * v).sum::<f64>().sqrt());
            // OpenCASCADE's side where the mesh allows it (with some slack, so an exact side
            // never loses to rounding); else (its control points') the middle of where the mesh
            // says the side is.
            let low = |o: f64, m: f64, e: f64| if o >= m - 1.5 * e { o.min(m) } else { m - e / 2.0 };
            let high = |o: f64, m: f64, e: f64| if o <= m + 1.5 * e { o.max(m) } else { m + e / 2.0 };
            let lo = Vec3::new(low(olo.x, mlo.x, e(0)), low(olo.y, mlo.y, e(1)), low(olo.z, mlo.z, e(2)));
            let hi = Vec3::new(high(ohi.x, mhi.x, e(0)), high(ohi.y, mhi.y, e(1)), high(ohi.z, mhi.z, e(2)));
            (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
        })
        .collect())
}

fn pick(bit: usize, lo: f64, hi: f64) -> f64 {
    if bit == 0 { lo } else { hi }
}

/// The box of `pts`; `None` when there are none or one isn't finite.
fn points_box(mut pts: impl Iterator<Item = Vec3>) -> Option<(Vec3, Vec3)> {
    let first = pts.next()?;
    pts.try_fold((first, first), |(lo, hi), p| {
        p.is_finite().then(|| (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z))))
    })
    .filter(|(lo, hi)| lo.is_finite() && hi.is_finite())
}

/// Two faces whose surface normals at a point of their edge are closer than this (half a degree)
/// meet tangentially: a smooth edge.
const TANGENT_COS: f64 = 0.999_96;

/// Triangles face by face, and the edges as the boundaries of each face's triangles (shared
/// edges once). The whole shape is triangulated first so neighbouring faces agree on edges.
/// It is open (a surface) unless it is a solid or every face boundary is shared: a closed
/// shell's boundaries pair up (a seam with itself), a surface's own edges don't.
fn mesh(shape: &Shape) -> Result<Mesh3> {
    meshed(shape, false).map(|(m, _)| m)
}

/// [`mesh`], and with `smooth` which of its edges are smooth (parallel to them, see
/// [`crate::Outline`]): a seam (one face on both sides), or faces whose surface normals agree at
/// a point inside the edge.
fn meshed(shape: &Shape, smooth: bool) -> Result<(Mesh3, Vec<bool>)> {
    let tol = deflection(shape);
    Mesher::try_new(shape, tol).map_err(failed("meshing"))?;
    let q = tol * 1e-3;
    let key = |p: Vec3| ((p.x / q).round() as i64, (p.y / q).round() as i64, (p.z / q).round() as i64);
    let mut out = Mesh3::default();
    let mut shared: HashMap<_, u32> = HashMap::new();
    // The faces on each boundary piece, when judging smoothness.
    let mut sides: HashMap<_, Vec<usize>> = HashMap::new();
    let faces: Vec<Face> = shape.faces().collect();
    let mut segs = Vec::new();
    for (fi, f) in faces.iter().enumerate() {
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
                let k = (ka.min(kb), ka.max(kb));
                let uses = shared.entry(k).or_default();
                if *uses == 0 {
                    segs.push((a, b));
                }
                *uses += 1;
                if smooth {
                    sides.entry(k).or_default().push(fi);
                }
            }
        }
    }
    // Each polyline is one B-rep edge: chains stop at the shape's vertices.
    let corners: HashSet<_> = shape.edges().flat_map(|e| [key(v3(e.start_point())), key(v3(e.end_point()))]).collect();
    out.edges = chain(&segs, key, &corners);
    out.open = !matches!(shape.shape_type(), ShapeType::Solid | ShapeType::CompoundSolid) && shared.values().any(|n| n % 2 == 1);
    if !smooth {
        return Ok((out, Vec::new()));
    }
    // Judged at a point inside each edge: an inner vertex of a curved one (on the true edge), the
    // middle of a straight one.
    let tangent = |e: &Vec<Vec3>| {
        let i = e.len() / 2;
        let (Some(a), Some(b)) = (i.checked_sub(1).and_then(|j| e.get(j)), e.get(i)) else { return false };
        let (ka, kb) = (key(*a), key(*b));
        let at = if e.len() > 2 { *b } else { (*a + *b) * 0.5 };
        let normal = |f: usize| faces.get(f).and_then(|f| unit(v3(f.normal_at(dv(at)))));
        match sides.get(&(ka.min(kb), ka.max(kb))).map(Vec::as_slice) {
            Some([f, g]) if f == g => true,
            Some([f, g]) => normal(*f).zip(normal(*g)).is_some_and(|(n, m)| n.dot(m) > TANGENT_COS),
            _ => false,
        }
    };
    let flags = out.edges.iter().map(tangent).collect();
    Ok((out, flags))
}

/// Join segments that share end points into polylines, one per B-rep edge: a chain runs on only
/// through a point exactly two segments meet at that is not one of the `corners` (the shape's
/// vertices), so it ends where edges meet (three at a box corner, two at a flat face's corner) and
/// a closed edge (a circle) is a closed polyline from its vertex round to it.
fn chain<K: std::hash::Hash + Eq + Copy>(segs: &[(Vec3, Vec3)], key: impl Fn(Vec3) -> K, corners: &HashSet<K>) -> Vec<Vec<Vec3>> {
    let mut at: HashMap<K, Vec<usize>> = HashMap::new();
    for (i, (a, b)) in segs.iter().enumerate() {
        at.entry(key(*a)).or_default().push(i);
        at.entry(key(*b)).or_default().push(i);
    }
    let through = |k: &K| !corners.contains(k) && at.get(k).is_some_and(|v| v.len() == 2);
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
                if !through(&k) {
                    break;
                }
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
    fn edges_are_one_polyline_per_brep_edge() {
        let b = primitive(&Primitive::Box { a: Vec3::ZERO, b: Vec3::new(10.0, 20.0, 30.0) }).unwrap();
        assert_eq!(b.mesh.edges.len(), 12, "{:?}", b.mesh.edges);
        assert!(b.mesh.edges.iter().all(|e| e.len() == 2), "{:?}", b.mesh.edges);
        // A cylinder's circles are closed polylines of their own, apart from its seam.
        let c = primitive(&Primitive::Cylinder { base: Vec3::ZERO, radius: 3.0, height: 5.0 }).unwrap();
        let circles: Vec<&Vec<Vec3>> = c.mesh.edges.iter().filter(|e| e.len() > 2).collect();
        assert_eq!(circles.len(), 2, "{:?}", c.mesh.edges);
        for e in circles {
            assert!(e.first() == e.last(), "closed: {e:?}");
            assert!(e.iter().all(|p| (p.z - e[0].z).abs() < 1e-9 && (p.xy().len() - 3.0).abs() < 1e-6), "{e:?}");
        }
        // A flat rectangle's corners join only two segments: still four edges, not one loop.
        let pts = [Vec3::ZERO, Vec3::new(4.0, 0.0, 0.0), Vec3::new(4.0, 2.0, 0.0), Vec3::new(0.0, 2.0, 0.0)];
        let rect: Vec<Seg> = (0..4).map(|i| Seg::Line { a: pts[i], b: pts[(i + 1) % 4] }).collect();
        assert_eq!(plane_surface(&rect).unwrap().mesh.edges.len(), 4);
        // Chained by hand: a junction of three ends every chain there; so does a corner.
        let key = |p: Vec3| ((p.x * 1e6).round() as i64, (p.y * 1e6).round() as i64, (p.z * 1e6).round() as i64);
        let v = |x: f64, y: f64| Vec3::new(x, y, 0.0);
        let t = [(v(0.0, 0.0), v(1.0, 0.0)), (v(1.0, 0.0), v(2.0, 0.0)), (v(1.0, 1.0), v(1.0, 0.0))];
        assert_eq!(chain(&t, key, &std::collections::HashSet::new()).len(), 3);
        let bend = [(v(0.0, 0.0), v(1.0, 0.0)), (v(1.0, 0.0), v(1.0, 1.0)), (v(1.0, 1.0), v(2.0, 1.0))];
        assert_eq!(chain(&bend, key, &std::collections::HashSet::new()), vec![vec![v(0.0, 0.0), v(1.0, 0.0), v(1.0, 1.0), v(2.0, 1.0)]]);
        assert_eq!(chain(&bend, key, &HashSet::from([key(v(1.0, 0.0))])).len(), 2);
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

    /// The volume a closed mesh encloses.
    fn volume(m: &Mesh3) -> f64 {
        m.tris.iter().filter_map(|t| m.tri(t)).map(|[a, b, c]| a.dot(b.cross(c)) / 6.0).sum::<f64>().abs()
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
    fn sweep_mitres_trim_only_near_their_corners() {
        use std::f64::consts::PI;
        // A mitred tube holds its profile's area (a unit square here) times the path's length.
        let check = |path: Curve, len: f64| {
            let s = crate::sweep(&Curve::Segs(square(0.5, 0.0)), &path).unwrap();
            assert!(!s.mesh.open);
            let vol = volume(&s.mesh);
            assert!((vol / len - 1.0).abs() < 0.01, "volume {vol}, expected {len}");
            s
        };
        // A slot: along X, a half circle, back along -X and down to the start. Each long side
        // runs back across the mitre plane of a corner 4 away, which must not trim it there.
        check(
            Curve::Segs(vec![
                Seg::Line { a: v(0.0, 0.0, 0.0), b: v(10.0, 0.0, 0.0) },
                Seg::Arc { a: v(10.0, 0.0, 0.0), mid: v(12.0, 2.0, 0.0), b: v(10.0, 4.0, 0.0) },
                Seg::Line { a: v(10.0, 4.0, 0.0), b: v(0.0, 4.0, 0.0) },
                Seg::Line { a: v(0.0, 4.0, 0.0), b: v(0.0, 0.0, 0.0) },
            ]),
            24.0 + 2.0 * PI,
        );
        // A D: up the Y axis and a half circle back; the arc lies across both corners' mitres.
        check(
            Curve::Segs(vec![
                Seg::Line { a: v(0.0, 0.0, 0.0), b: v(0.0, 10.0, 0.0) },
                Seg::Arc { a: v(0.0, 10.0, 0.0), mid: v(5.0, 5.0, 0.0), b: v(0.0, 0.0, 0.0) },
            ]),
            10.0 + 5.0 * PI,
        );
        // Line, quarter bend, line, corner, line (the engine's test path): the start stays square,
        // though its corner at (0, 0.5) is across the last corner's mitre plane (y = x).
        let bent = check(
            Curve::Segs(vec![
                Seg::Line { a: v(0.0, 0.0, 0.0), b: v(10.0, 0.0, 0.0) },
                Seg::Arc { a: v(10.0, 0.0, 0.0), mid: v(10.0 + 5.0 * 0.5_f64.sqrt(), 5.0 - 5.0 * 0.5_f64.sqrt(), 0.0), b: v(15.0, 5.0, 0.0) },
                Seg::Line { a: v(15.0, 5.0, 0.0), b: v(15.0, 15.0, 0.0) },
                Seg::Line { a: v(15.0, 15.0, 0.0), b: v(5.0, 15.0, 0.0) },
            ]),
            30.0 + 2.5 * PI,
        );
        assert!(bent.mesh.verts.iter().any(|p| near(*p, v(0.0, 0.5, 0.5), 1e-6)), "{:?}", bounds(&bent.mesh));
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

    /// A cylinder's seam is smooth and its circles are edges; a filleted box's flat faces run
    /// into the fillets smoothly; a leg tapering 1.4° has every edge a real one.
    #[test]
    fn outlines_tell_smooth_edges_from_shallow_creases() {
        let c = crate::primitive(&Primitive::Cylinder { base: Vec3::ZERO, radius: 20.0, height: 700.0 }).unwrap();
        let o = crate::outlines(&[c.brep.as_str()]).unwrap().remove(0).unwrap();
        assert_eq!(o.smooth.len(), o.mesh.edges.len());
        for (e, smooth) in o.mesh.edges.iter().zip(&o.smooth) {
            let seam = e.len() == 2;
            assert_eq!(*smooth, seam, "{e:?}");
        }
        let b = crate::primitive(&Primitive::Box { a: Vec3::ZERO, b: v(100.0, 60.0, 20.0) }).unwrap();
        let f = crate::fillet(placed(&b), 2.0).unwrap();
        let o = crate::outlines(&[f.brep.as_str()]).unwrap().remove(0).unwrap();
        assert!(o.smooth.iter().filter(|s| **s).count() >= 24, "{:?}", o.smooth);
        let leg = [v(0.0, 0.0, 0.0), v(30.0, 0.0, 0.0), v(45.0, 0.0, 600.0), v(45.0, 0.0, 700.0), v(0.0, 0.0, 700.0)];
        let t = crate::extrude(&polygon(&leg), v(0.0, 18.0, 0.0)).unwrap();
        let o = crate::outlines(&[t.brep.as_str(), "not a B-rep"]).unwrap();
        let first = o.first().cloned().flatten().unwrap();
        assert!(first.smooth.iter().all(|s| !s) && first.smooth.len() == 15, "{:?}", first.smooth);
        assert!(o.get(1).is_some_and(Option::is_none), "a bad one is none, the rest still come");
    }

    /// Boxes come from the geometry: a slender round leg measures its full diameter (its mesh's
    /// points don't reach it), and a board turned in the world measures its turned box.
    #[test]
    fn boxes_measure_geometry_not_facets() {
        let c = crate::primitive(&Primitive::Cylinder { base: Vec3::ZERO, radius: 20.0, height: 700.0 }).unwrap();
        let id = Mat4::IDENTITY;
        let got = crate::boxes(&[Placed { brep: &c.brep, xform: &id }]).unwrap();
        let (lo, hi) = got[0].unwrap();
        assert!(near(lo, v(-20.0, -20.0, 0.0), 1e-6) && near(hi, v(20.0, 20.0, 700.0), 1e-6), "{lo:?} {hi:?}");
        let (mlo, mhi) = bounds(&c.mesh);
        assert!(mhi.y - mlo.y < 40.0 - 1e-3 || mhi.x - mlo.x < 40.0 - 1e-3, "the mesh falls short: {mlo:?} {mhi:?}");
        let b = crate::primitive(&Primitive::Box { a: Vec3::ZERO, b: v(600.0, 400.0, 18.0) }).unwrap();
        let turn = crate::tests::xform(Vec3::Z, 30f64.to_radians(), 1.0, false, Vec3::ZERO);
        let mut shear = Mat4::IDENTITY;
        shear.m[0][1] = 0.5;
        let got = crate::boxes(&[Placed { brep: &b.brep, xform: &turn }, Placed { brep: &b.brep, xform: &shear }, Placed { brep: "x", xform: &id }])
            .unwrap();
        let (lo, hi) = got[0].unwrap();
        let (c30, s30) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
        assert!(near(hi - lo, v(600.0 * c30 + 400.0 * s30, 600.0 * s30 + 400.0 * c30, 18.0), 1e-6), "{lo:?} {hi:?}");
        // Sheared (no similarity): the sheared board's box.
        let (lo, hi) = got[1].unwrap();
        assert!(near(lo, Vec3::ZERO, 1e-6) && near(hi, v(800.0, 400.0, 18.0), 1e-6), "{lo:?} {hi:?}");
        assert!(got[2].is_none(), "{got:?}");
    }

    /// The box's size under `x`.
    fn size_under(b: &Built, x: &Mat4) -> Vec3 {
        let (lo, hi) = crate::boxes(&[Placed { brep: &b.brep, xform: x }]).unwrap()[0].unwrap();
        hi - lo
    }

    /// Each transform's box size, in one call (one mesh), and how far OpenCASCADE's own box (its
    /// control points') reaches past it at the worst.
    fn sizes_under(b: &Built, xs: &[Mat4]) -> (Vec<Vec3>, f64) {
        let placed: Vec<Placed> = xs.iter().map(|x| Placed { brep: &b.brep, xform: x }).collect();
        let got: Vec<Vec3> = crate::boxes(&placed).unwrap().into_iter().map(|b| b.map(|(lo, hi)| hi - lo).unwrap()).collect();
        let s = super::read_brep(&b.brep).unwrap();
        let reach = |x: &Mat4, g: &Vec3| {
            let (lo, hi) = super::geometry_box(&super::place(&s, x).unwrap()).unwrap();
            let d = (hi - lo) - *g;
            d.x.max(d.y).max(d.z)
        };
        let over = xs.iter().zip(&got).map(|(x, g)| reach(x, g)).fold(0.0, f64::max);
        (got, over)
    }

    /// Free-form parts measure their true size, not their control points', however they are
    /// turned: a smooth loft (a tapered round leg) and a tube swept along a spline.
    #[test]
    fn boxes_measure_free_form_parts_truly() {
        let turns: Vec<Mat4> =
            [0.0, 17.0, 30.0, 45.0].iter().map(|d: &f64| crate::tests::xform(Vec3::Z, d.to_radians(), 1.0, false, v(5.0, 6.0, 7.0))).collect();
        let leg = crate::loft(&[circle(20.0, 0.0), circle(16.0, 150.0), circle(12.0, 300.0)], false).unwrap();
        let (got, over) = sizes_under(&leg, &turns);
        assert!(over > 0.1, "the control points reach out: {over}");
        assert!(got.iter().all(|g| near(*g, v(40.0, 40.0, 300.0), 0.1) && g.x > 39.95 && g.y > 39.95), "{got:?}");
        let path = Curve::Spline { points: (0..=4).map(|i| v(0.0, 0.0, 50.0 * f64::from(i))).collect(), tangents: None };
        let tube = crate::sweep(&circle(10.0, 0.0), &path).unwrap();
        let (got, over) = sizes_under(&tube, &turns);
        assert!(over > 0.1, "the control points reach out: {over}");
        assert!(got.iter().all(|g| near(*g, v(20.0, 20.0, 200.0), 0.1) && g.x > 19.95 && g.y > 19.95), "{got:?}");
    }

    /// A block reference scaled unevenly stretches the solid: it measures stretched, round faces
    /// too, and copies moved apart measure moved apart.
    #[test]
    fn boxes_take_uneven_scales_and_moves() {
        let c = crate::primitive(&Primitive::Cylinder { base: Vec3::ZERO, radius: 20.0, height: 100.0 }).unwrap();
        let scale = |x: f64, y: f64, z: f64| Mat4 { m: [[x, 0.0, 0.0, 0.0], [0.0, y, 0.0, 0.0], [0.0, 0.0, z, 0.0], [0.0, 0.0, 0.0, 1.0]] };
        assert!(near(size_under(&c, &scale(1.0, 1.0, 1.5)), v(40.0, 40.0, 150.0), 1e-3));
        assert!(near(size_under(&c, &scale(2.0, 1.0, 1.0)), v(80.0, 40.0, 100.0), 1e-3));
        // Stretched along X, then turned 30° about Z: an ellipse of half axes 40 and 20, turned.
        let turned = crate::tests::xform(Vec3::Z, 30f64.to_radians(), 1.0, false, Vec3::ZERO).mul(&scale(2.0, 1.0, 1.0));
        let got = size_under(&c, &turned);
        assert!(near(got, v(2.0 * 1300f64.sqrt(), 2.0 * 700f64.sqrt(), 100.0), 0.1), "{got:?}");
        // The same turn at two places: one box, moved.
        let (a, b) = (Mat4::translate(v(1000.0, 0.0, 0.0)).mul(&turned), Mat4::translate(v(0.0, -50.0, 3.0)).mul(&turned));
        let got = crate::boxes(&[Placed { brep: &c.brep, xform: &a }, Placed { brep: &c.brep, xform: &b }]).unwrap();
        let ((alo, ahi), (blo, bhi)) = (got[0].unwrap(), got[1].unwrap());
        assert!(near(alo - blo, v(1000.0, 50.0, -3.0), 1e-9) && near(ahi - bhi, v(1000.0, 50.0, -3.0), 1e-9), "{got:?}");
        // No box through a perspective.
        let mut p = Mat4::IDENTITY;
        p.m[3][2] = 0.1;
        assert_eq!(crate::boxes(&[Placed { brep: &c.brep, xform: &p }]).unwrap(), vec![None]);
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

/// Placing solids under any similarity, and IGES out and in (in-process).
#[cfg(test)]
mod exchange_tests {
    use cadcraft_geom::{Mat4, Vec3};

    use crate::tests::xform;
    use crate::{Built, Curve, KernelError, Placed, Primitive, Seg};

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    fn bounds(pts: &[Vec3]) -> (Vec3, Vec3) {
        pts.iter().fold((v(f64::MAX, f64::MAX, f64::MAX), v(f64::MIN, f64::MIN, f64::MIN)), |(lo, hi), p| {
            (v(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), v(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
        })
    }

    fn cube() -> Built {
        crate::primitive(&Primitive::Box { a: v(1.0, 2.0, 3.0), b: v(11.0, 22.0, 33.0) }).unwrap()
    }

    /// The box placed by the kernel has the corners of its mesh transformed in Rust.
    fn check_placed(b: &Built, x: &Mat4) {
        let m = super::mesh(&super::placed(Placed { brep: &b.brep, xform: x }).unwrap()).unwrap();
        let want: Vec<Vec3> = b.mesh.verts.iter().map(|p| x.apply(*p)).collect();
        let ((lo, hi), (wlo, whi)) = (bounds(&m.verts), bounds(&want));
        assert!((lo - wlo).len() < 1e-6 && (hi - whi).len() < 1e-6, "{lo:?} {hi:?} vs {wlo:?} {whi:?}");
        for p in &want {
            assert!(m.verts.iter().any(|q| (*q - *p).len() < 1e-6), "corner {p:?} missing");
        }
    }

    #[test]
    fn placed_applies_any_similarity() {
        use std::f64::consts::{FRAC_PI_2, PI};
        let b = cube();
        let off = v(-4.0, 9.0, 2.5);
        check_placed(&b, &xform(v(1.0, 0.0, 0.0), FRAC_PI_2, 1.0, false, off));
        check_placed(&b, &xform(v(1.0, -2.0, 3.0), 1.1, 1.0, false, off));
        check_placed(&b, &xform(v(1.0, -2.0, 3.0), 2.4, 1.0, true, off));
        check_placed(&b, &xform(v(0.0, 1.0, 1.0), PI, 2.5, false, off));
        check_placed(&b, &xform(v(0.3, 0.2, -1.0), 0.0, 0.5, true, Vec3::ZERO));
        // A box turned a quarter about X stands its 20-long side up the Z axis.
        let m = super::mesh(&super::placed(Placed { brep: &b.brep, xform: &xform(v(1.0, 0.0, 0.0), FRAC_PI_2, 1.0, false, Vec3::ZERO) }).unwrap())
            .unwrap();
        let (lo, hi) = bounds(&m.verts);
        assert!((hi - lo - v(10.0, 30.0, 20.0)).len() < 1e-6, "{lo:?} {hi:?}");
        // Shear is still refused.
        let mut shear = Mat4::IDENTITY;
        shear.m[0][2] = 0.5;
        assert!(matches!(super::placed(Placed { brep: &b.brep, xform: &shear }), Err(KernelError::Invalid(_))));
    }

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("cadrhino-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn iges_round_trips_solids_and_curves() {
        let b = cube();
        let turned = xform(v(0.0, 0.0, 1.0), 0.5, 1.0, false, v(100.0, 0.0, 0.0));
        let line = Curve::Segs(vec![Seg::Line { a: v(0.0, 0.0, 0.0), b: v(0.0, 50.0, 5.0) }]);
        let arc = Curve::Segs(vec![Seg::Arc { a: v(-10.0, 0.0, 0.0), mid: v(-20.0, 10.0, 0.0), b: v(-30.0, 0.0, 0.0) }]);
        // A rectangle comes back as one closed polyline.
        let r = [v(0.0, -10.0, 0.0), v(40.0, -10.0, 0.0), v(40.0, -30.0, 0.0), v(0.0, -30.0, 0.0)];
        let rect = Curve::Segs((0..4).map(|i| Seg::Line { a: r[i], b: r[(i + 1) % 4] }).collect());
        // An arc from corner to corner over the box's bottom face: free, though its ends are a face edge's.
        let over = Curve::Segs(vec![Seg::Arc { a: v(1.0, 2.0, 3.0), mid: v(6.0, 0.0, 3.0), b: v(11.0, 2.0, 3.0) }]);
        let too_small = Curve::Segs(vec![Seg::Line { a: Vec3::ZERO, b: Vec3::ZERO }]);
        let path = temp("round.igs");
        let solids = [Placed { brep: &b.brep, xform: &Mat4::IDENTITY }, Placed { brep: &b.brep, xform: &turned }];
        let written = crate::export_iges(&solids, &[line, arc, rect, over, too_small], &path).unwrap();
        assert_eq!(written, 4, "the zero-length line is left out");
        let got = crate::import_iges(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        // Two boxes, each a lump of six faces, closed.
        assert_eq!(got.shapes.len(), 2, "{} shapes", got.shapes.len());
        for s in &got.shapes {
            assert_eq!(s.mesh.tris.len(), 12);
            assert!(!s.mesh.open);
        }
        let (lo, hi) = bounds(&got.shapes[0].mesh.verts);
        assert!((lo - v(1.0, 2.0, 3.0)).len() < 1e-6 && (hi - v(11.0, 22.0, 33.0)).len() < 1e-6, "{lo:?} {hi:?}");
        let want: Vec<Vec3> = b.mesh.verts.iter().map(|p| turned.apply(*p)).collect();
        let (wlo, whi) = bounds(&want);
        let (lo, hi) = bounds(&got.shapes[1].mesh.verts);
        assert!((lo - wlo).len() < 1e-6 && (hi - whi).len() < 1e-6, "{lo:?} {hi:?}");
        // The curves: the line, the arc, the closed rectangle and the arc over the face.
        assert_eq!(got.curves.len(), 4, "{:?}", got.curves);
        let has = |a: Vec3, b: Vec3| {
            got.curves.iter().find(|c| {
                let (f, l) = (c[0], c[c.len() - 1]);
                ((f - a).len() < 1e-6 && (l - b).len() < 1e-6) || ((f - b).len() < 1e-6 && (l - a).len() < 1e-6)
            })
        };
        assert_eq!(has(v(0.0, 0.0, 0.0), v(0.0, 50.0, 5.0)).map(Vec::len), Some(2), "the line");
        let arc = has(v(-10.0, 0.0, 0.0), v(-30.0, 0.0, 0.0)).expect("the arc");
        assert!(arc.len() > 5 && arc.iter().all(|p| ((*p - v(-20.0, 0.0, 0.0)).len() - 10.0).abs() < 1e-6), "{arc:?}");
        let over = has(v(1.0, 2.0, 3.0), v(11.0, 2.0, 3.0)).expect("the arc over the face");
        assert!(over.iter().any(|p| p.y < 1.0), "an arc, not the face's edge: {over:?}");
        let rect = got.curves.iter().find(|c| c.len() == 5 && c[0] == c[4]).expect("the closed rectangle");
        assert!(r.iter().all(|p| rect.iter().any(|q| (*q - *p).len() < 1e-6)), "{rect:?}");
    }

    /// Cabinets side by side: objects touching along a whole face come back whole and apart.
    #[test]
    fn iges_objects_touching_face_to_face_stay_whole() {
        let boxes: Vec<Built> = [(0.0, 10.0), (10.0, 20.0), (20.0, 30.0)]
            .iter()
            .map(|(a, b)| crate::primitive(&Primitive::Box { a: v(*a, 0.0, 0.0), b: v(*b, 20.0, 30.0) }).unwrap())
            .collect();
        let path = temp("touching.igs");
        let solids: Vec<Placed> = boxes.iter().map(|b| Placed { brep: &b.brep, xform: &Mat4::IDENTITY }).collect();
        crate::export_iges(&solids, &[], &path).unwrap();
        let got = crate::import_iges(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(got.shapes.len(), 3, "{} shapes", got.shapes.len());
        for (i, s) in got.shapes.iter().enumerate() {
            assert_eq!(s.mesh.tris.len(), 12, "box {i}");
            assert!(!s.mesh.open, "box {i} is closed");
            let (lo, hi) = bounds(&s.mesh.verts);
            let x = 10.0 * i as f64;
            assert!((lo - v(x, 0.0, 0.0)).len() < 1e-6 && (hi - v(x + 10.0, 20.0, 30.0)).len() < 1e-6, "box {i}: {lo:?} {hi:?}");
        }
        assert!(got.curves.is_empty(), "{:?}", got.curves);
    }

    /// The grouping read from B-rep text: a solid, a compound of faces, and text that isn't a
    /// shape table (or doesn't account for the faces) gives no grouping.
    #[test]
    fn objects_are_read_from_the_shape_table() {
        let b = cube();
        let text = super::write_brep(&super::read_brep(&b.brep).unwrap()).unwrap();
        assert_eq!(super::objects(&text, 6).map(|(o, _)| o), Some(vec![Some(0); 6]));
        assert_eq!(super::objects(&text, 7).map(|(o, _)| o), None);
        let cut = &text[..text.len() / 2];
        for bad in ["", "TShapes 3\nZz\n", "TShapes 99999999999\nCo\n\n1\n*\n", "TShapes 1\nCo\n\n1100000\n+5 0 *\n\n+1 0\n", cut] {
            assert_eq!(super::objects(bad, 6).map(|(o, _)| o), None, "{bad:?}");
        }
        // A loose face, a group of one face and another loose face under a compound, itself the
        // only child of the top compound.
        let fa = |i: usize| format!("Fa\n0 1e-07 {i} 0\n\n0111000\n*\n");
        let t =
            format!("TShapes 6\n{}{}{}Co\n\n0100000\n+4 0 *\nCo\n\n0100000\n+6 0 +3 0 +5 0 *\nCo\n\n1100000\n+2 0 *\n\n+1 0\n", fa(1), fa(2), fa(3));
        assert_eq!(super::objects(&t, 3).map(|(o, _)| o), Some(vec![None, Some(0), None]));
    }

    /// Groups are objects only when their faces make one piece: a group of one face, or of faces
    /// apart, gives them up to the edge joining of ungrouped faces.
    #[test]
    fn only_groups_in_one_piece_are_objects() {
        // Faces 0-2 one group joined by edges; face 3 a group alone; faces 4 and 5 a group whose
        // only link is through face 2 of another group; face 6 ungrouped.
        let owner = vec![Some(0), Some(0), Some(0), Some(1), Some(2), Some(2), None];
        let on_edges = vec![vec![0, 1], vec![1, 2], vec![3, 6], vec![2, 4], vec![2, 5], vec![4], vec![99, 0]];
        assert_eq!(
            super::shells(owner.clone(), &on_edges, &std::collections::HashSet::new()),
            vec![Some(0), Some(0), Some(0), None, None, None, None]
        );
        // Two pieces of a group linked only through a face between them on one edge (a, x, b).
        assert_eq!(super::shells(vec![Some(0), Some(1), Some(0)], &[vec![0, 1, 2]], &std::collections::HashSet::new()), vec![Some(0), None, Some(0)]);
        assert!(super::shells(Vec::new(), &[vec![0, 1]], &std::collections::HashSet::new()).is_empty());
        // A solid stays whole even in pieces (a hollow one: outer and inner shells apart).
        let mut solid = std::collections::HashSet::new();
        solid.insert(0);
        assert_eq!(super::shells(vec![Some(0), Some(0), Some(0), Some(0)], &[vec![0, 1], vec![2, 3]], &solid), vec![Some(0); 4]);
    }

    /// A box read with each trimmed surface in a group of its own (as some writers group them)
    /// still comes back as one closed box, not six surfaces; written to a file the same way,
    /// OpenCASCADE drops the groups of one and it comes back whole too.
    #[test]
    fn iges_one_group_per_surface_comes_back_whole() {
        use opencascade::primitives::{Compound, Shape};
        let b = cube();
        let shape = super::read_brep(&b.brep).unwrap();
        let wrapped: Vec<Shape> = shape.faces().map(|f| Compound::from_shapes(&[Shape::from(&f)]).into()).collect();
        let all: Shape = Compound::from_shapes(&wrapped).into();
        let text = super::write_brep(&all).unwrap();
        assert_eq!(super::objects(&text, 6).map(|(o, _)| o), Some((0..6).map(Some).collect()), "six groups of one face");
        let path = temp("per-surface.igs");
        {
            let _one = super::exchanging();
            all.write_iges(&path).unwrap();
        }
        let read = crate::import_iges(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        for got in [super::imported(&all).unwrap(), read] {
            assert_eq!(got.shapes.len(), 1, "{} shapes", got.shapes.len());
            assert_eq!(got.shapes[0].mesh.tris.len(), 12);
            assert!(!got.shapes[0].mesh.open);
        }
    }

    /// The signed volume a closed mesh encloses.
    fn signed_volume(m: &cadcraft_geom::Mesh3) -> f64 {
        m.tris.iter().filter_map(|t| m.tri(t)).map(|[a, b, c]| a.dot(b.cross(c)) / 6.0).sum()
    }

    /// A hollow box (closed shell: an outer and an inner void shell, which faces-mode IGES writes
    /// apart) comes back as one solid facing outward, alone or beside another box.
    #[test]
    fn iges_hollow_solids_come_back_whole() {
        let b = crate::primitive(&Primitive::Box { a: Vec3::ZERO, b: v(10.0, 10.0, 10.0) }).unwrap();
        let hollow = crate::shell(Placed { brep: &b.brep, xform: &Mat4::IDENTITY }, 1.0, false).unwrap();
        let other = crate::primitive(&Primitive::Box { a: v(20.0, 0.0, 0.0), b: v(25.0, 5.0, 5.0) }).unwrap();
        for with_other in [false, true] {
            let path = temp(&format!("hollow-{with_other}.igs"));
            let mut solids = vec![Placed { brep: &hollow.brep, xform: &Mat4::IDENTITY }];
            if with_other {
                solids.push(Placed { brep: &other.brep, xform: &Mat4::IDENTITY });
            }
            crate::export_iges(&solids, &[], &path).unwrap();
            let got = crate::import_iges(&path).unwrap();
            let _ = std::fs::remove_file(&path);
            assert_eq!(got.shapes.len(), solids.len(), "{with_other}");
            let h = got.shapes.iter().find(|s| s.mesh.tris.len() == 24).expect("the hollow box, whole");
            assert!(!h.mesh.open);
            assert!((signed_volume(&h.mesh) - (1000.0 - 512.0)).abs() < 1e-6, "{}", signed_volume(&h.mesh));
            assert!(got.shapes.iter().all(|s| signed_volume(&s.mesh) > 0.0));
        }
    }

    /// A stray object a kilometre away doesn't wipe out a shelf-pin hole's 5 mm circle.
    #[test]
    fn iges_far_away_objects_keep_small_curves() {
        let cabinet = crate::primitive(&Primitive::Box { a: Vec3::ZERO, b: v(600.0, 560.0, 720.0) }).unwrap();
        let pin = Curve::Segs(vec![Seg::Circle { center: v(50.2, 50.2, 0.0), radius: 2.5, normal: Vec3::Z }]);
        let stray = Curve::Segs(vec![Seg::Line { a: v(1e6, 0.0, 0.0), b: v(1e6 + 10.0, 0.0, 0.0) }]);
        let path = temp("far.igs");
        crate::export_iges(&[Placed { brep: &cabinet.brep, xform: &Mat4::IDENTITY }], &[pin, stray], &path).unwrap();
        let got = crate::import_iges(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let circle = got.curves.iter().find(|c| c.len() > 8 && c.first() == c.last()).expect("the pin hole");
        assert!(circle.iter().all(|p| ((*p - v(50.2, 50.2, 0.0)).len() - 2.5).abs() < 1e-6), "{circle:?}");
        assert_eq!(got.curves.len(), 2, "{:?}", got.curves.len());
    }

    #[test]
    fn iges_curves_alone_and_bad_files() {
        let path = temp("curves.igs");
        let circle = Curve::Segs(vec![Seg::Circle { center: v(5.0, 5.0, 2.0), radius: 3.0, normal: Vec3::Z }]);
        let spline = Curve::Spline { points: vec![v(0.0, 0.0, 0.0), v(5.0, 3.0, 1.0), v(10.0, 0.0, 2.0)], tangents: None };
        assert_eq!(crate::export_iges(&[], &[circle, spline], &path).unwrap(), 2);
        let got = crate::import_iges(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(got.shapes.is_empty());
        assert_eq!(got.curves.len(), 2, "{:?}", got.curves);
        let circle = got.curves.iter().find(|c| c.first() == c.last()).expect("the circle, closed");
        assert!(circle.iter().all(|p| (p.z - 2.0).abs() < 1e-9 && ((*p - v(5.0, 5.0, 2.0)).len() - 3.0).abs() < 1e-6));
        // Nothing to write; nothing to read; not IGES.
        assert!(matches!(crate::export_iges(&[], &[], &path), Err(KernelError::Invalid(_))));
        assert!(crate::import_iges(&temp("missing.igs")).is_err());
        let junk = temp("junk.igs");
        std::fs::write(&junk, "not an IGES file\n\0\u{1}garbage").unwrap();
        assert!(crate::import_iges(&junk).is_err());
        let _ = std::fs::remove_file(&junk);
    }
}
