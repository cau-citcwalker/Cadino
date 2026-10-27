//! The backend of [`crate::ops`]: our C++ (the `cadcraft-kernel-occt` crate) on B-rep text; what
//! comes back is read and meshed here like every other result.

use std::path::Path;

use cadcraft_geom::{Mat4, Vec3};
use cadcraft_kernel_occt as x;

use super::{exchanging, mesh, read_brep, swept, write_brep};
use crate::ops::{
    Check, Clash, EdgeCurve, EdgeInfo, FaceInfo, Flat, Hit, Op, Out, Picked, Probe, Region, Section, SectionRegion, SurfaceKind, Topology, Wire3,
};
use crate::{BoolOp, Built, Curve, KernelError, Label, Placed, Result, Seg, Solid};

pub fn op(o: &Op) -> Result<Out> {
    Ok(match o {
        Op::Topology(s) => Out::Topology(topology(&x::topology(&shape(s)).map_err(failed)?)),
        Op::Pick(s, probe, tol) => Out::Picked(pick(s, *probe, *tol)?),
        Op::Picks(ss, probe, tol) => Out::Pickeds(ss.iter().map(|s| pick(s, *probe, *tol)).collect()),
        Op::FilletEdges(s, edges) => {
            let (at, radii): (Vec<u32>, Vec<f64>) = edges.iter().copied().unzip();
            Out::Built(text(x::fillet_edges(&shape(s), &at, &radii))?)
        }
        Op::ChamferEdges(s, bevels) => {
            let at: Vec<u32> = bevels.iter().map(|b| b.edge).collect();
            let (d1, d2): (Vec<f64>, Vec<f64>) = bevels.iter().map(|b| (b.base, b.other)).unzip();
            let faces: Vec<i32> = bevels.iter().map(|b| b.face.map_or(-1, |f| i32::try_from(f).unwrap_or(i32::MAX))).collect();
            Out::Built(text(x::chamfer_edges(&shape(s), &at, &d1, &d2, &faces))?)
        }
        Op::ExtrudeFaces(s, faces, d, taper) => Out::Built(text(x::extrude_faces(&shape(s), faces, *d, *taper))?),
        Op::OffsetFaces(s, faces, d) => Out::Built(text(x::offset_faces(&shape(s), faces, *d))?),
        Op::TransformFaces(s, faces, m) => Out::Built(text(x::transform_faces(&shape(s), faces, &rows(m)))?),
        Op::TaperFaces(s, faces, base, dir, angle) => Out::Built(text(x::taper_faces(&shape(s), faces, &xyz(*base), &xyz(*dir), *angle))?),
        Op::DeleteFaces(s, faces) => Out::Built(text(x::delete_faces(&shape(s), faces))?),
        Op::CopyFaces(s, faces) => Out::Builts(texts(x::copy_faces(&shape(s), faces))?),
        Op::Imprint(s, tools, curves) => Out::Built(text(x::imprint(&shape(s), &shapes(tools), &wires(curves)))?),
        Op::Separate(s) => Out::Builts(texts(x::separate(&shape(s)))?),
        Op::ShellFaces(s, faces, t) => Out::Built(text(x::shell_faces(&shape(s), faces, *t))?),
        Op::Clean(s) => Out::Built(text(x::clean(&shape(s)))?),
        Op::Check(s) => {
            let c = x::check(&shape(s)).map_err(failed)?;
            Out::Check(Check { valid: c.valid, problems: c.messages })
        }
        Op::PressPull(s, Region::Face(f), d) => Out::Built(text(x::extrude_faces(&shape(s), &[*f], *d, 0.0))?),
        Op::PressPull(s, Region::Profile { outer, holes }, d) => Out::Built(text(x::press_pull(&shape(s), &wire(outer), &wires(holes), *d))?),
        Op::Thicken(s, t, both) => Out::Built(text(x::thicken(&shape(s), *t, *both))?),
        Op::Section(s, origin, normal) => Out::Section(section(x::section(&shape(s), &xyz(*origin), &xyz(*normal)).map_err(failed)?)?),
        Op::Sections(ss, origin, normal, behind) => Out::Sections(
            ss.iter()
                .map(|s| {
                    let cut = section(x::section(&shape(s), &xyz(*origin), &xyz(*normal)).map_err(failed)?)?;
                    let back = if *behind { super::slice(s.placed(), *origin, *normal)?.into_iter().nth(1).flatten() } else { None };
                    Ok((cut, back))
                })
                .collect(),
        ),
        Op::Flatshot(ss, toward, right) => {
            let f = x::flatshot(&shapes(ss), &xyz(*toward), &xyz(*right)).map_err(failed)?;
            Out::Flat(Flat { visible: f.visible.iter().map(curve).collect(), hidden: f.hidden.iter().map(curve).collect() })
        }
        Op::Interfere(a, b) => Out::Clashes(
            x::interfere(&shapes(a), &shapes(b))
                .map_err(failed)?
                .into_iter()
                .map(|c| Ok(Clash { a: c.a as usize, b: c.b as usize, solid: built(c.brep)?, volume: c.volume }))
                .collect::<Result<_>>()?,
        ),
        Op::Polysolid(runs) => {
            let brep = write_brep(&swept(runs, true))?;
            Out::Built(text(x::clean(&x::ShapeIn { brep, xf: rows(&Mat4::IDENTITY) }))?)
        }
        Op::Sew(ss, solid) => Out::Built(text(x::sew(&shapes(ss), *solid))?),
        Op::MeshShape(verts, tris, solid, merge) => {
            let v: Vec<f64> = verts.iter().flat_map(|p| [p.x, p.y, p.z]).collect();
            let t: Vec<u32> = tris.iter().flatten().copied().collect();
            let brep = x::mesh_shape(&v, &t, *solid, *merge).map_err(failed)?;
            Out::Built(Built { mesh: faceted(verts, tris, *solid, *merge), brep })
        }
        Op::ToSurface(s) => Out::Built(text(x::to_surface(&shape(s)))?),
        Op::Split(s, tools) => Out::Builts(texts(x::split(&shape(s), &shapes(tools)))?),
        Op::Ruled(a, b) => Out::Built(text(x::ruled(&wire(a), &wire(b)))?),
        Op::Tabulated(c, dir) => Out::Built(text(x::tabulated(&wire(c), &xyz(*dir)))?),
        Op::Revolved(c, origin, axis, start, angle) => Out::Built(text(x::revolved(&wire(c), &xyz(*origin), &xyz(*axis), *start, *angle))?),
        Op::Coons(edges) => Out::Built(text(x::coons(&wires(edges)))?),
        Op::PlanarFace(outer, holes) => Out::Built(text(x::planar_face(&wire(outer), &wires(holes)))?),
        Op::PipeLoft(sections, path, solid) => Out::Built(text(x::pipe_loft(&wires(sections), &wire(path), *solid))?),
        Op::GuidedLoft(sections, guides, solid) => Out::Built(text(x::guided_loft(&wires(sections), &wires(guides), *solid))?),
    })
}

/// The mesh of a converted mesh, from its own triangles (meshing a B-rep of thousands of faces
/// again takes seconds), as the C++ builds the shape: corners at one point one corner, triangles
/// without area left out, each piece turned one way round (a closed one outward). Its edges are
/// the sides the shape keeps: open or shared by more than two, and, with `merge`, only those
/// between triangles not in one plane.
fn faceted(verts: &[Vec3], tris: &[[u32; 3]], solid: bool, merge: bool) -> cadcraft_geom::Mesh3 {
    use std::collections::HashMap;
    let (lo, hi) = verts.iter().fold((Vec3::new(f64::MAX, f64::MAX, f64::MAX), Vec3::new(f64::MIN, f64::MIN, f64::MIN)), |(lo, hi), p| {
        (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
    });
    let size = if verts.is_empty() { 0.0 } else { (hi - lo).len() };
    let step = (size * 1e-9).max(1e-7);
    let mut at: HashMap<(i64, i64, i64), u32> = HashMap::new();
    let same: Vec<u32> = verts
        .iter()
        .enumerate()
        .map(|(i, p)| *at.entry(((p.x / step).round() as i64, (p.y / step).round() as i64, (p.z / step).round() as i64)).or_insert(i as u32))
        .collect();
    let corner = |i: u32| verts.get(i as usize).copied().unwrap_or(Vec3::ZERO);
    let normal = |t: &[u32; 3]| (corner(t[1]) - corner(t[0])).cross(corner(t[2]) - corner(t[0]));
    let mut t: Vec<[u32; 3]> = tris
        .iter()
        .filter_map(|c| {
            let c = [*same.get(c[0] as usize)?, *same.get(c[1] as usize)?, *same.get(c[2] as usize)?];
            (c[0] != c[1] && c[1] != c[2] && c[2] != c[0] && normal(&c).len() > 1e-12 * size * size).then_some(c)
        })
        .collect();
    let key = |a: u32, b: u32| (a.min(b), a.max(b));
    let mut round: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (i, c) in t.iter().enumerate() {
        for k in 0..3 {
            round.entry(key(c[k], c[(k + 1) % 3])).or_default().push(i);
        }
    }
    // Pieces turned one way round: across a side two triangles run it in opposite directions.
    let runs = |c: &[u32; 3], a: u32, b: u32| (0..3).any(|k| c[k] == a && c[(k + 1) % 3] == b);
    let mut piece = vec![usize::MAX; t.len()];
    let mut pieces = 0;
    for first in 0..t.len() {
        if piece[first] != usize::MAX {
            continue;
        }
        piece[first] = pieces;
        let mut todo = vec![first];
        while let Some(i) = todo.pop() {
            let c = t[i];
            for k in 0..3 {
                let (a, b) = (c[k], c[(k + 1) % 3]);
                let Some([x, y]) = round.get(&key(a, b)).map(Vec::as_slice) else { continue };
                let j = if *x == i { *y } else { *x };
                if piece[j] != usize::MAX {
                    continue;
                }
                if runs(&t[j], a, b) {
                    t[j].swap(1, 2);
                }
                piece[j] = pieces;
                todo.push(j);
            }
        }
        pieces += 1;
    }
    // A closed piece facing inward is turned out.
    let (mut vol, mut shut) = (vec![0.0; pieces], vec![true; pieces]);
    for (i, c) in t.iter().enumerate() {
        vol[piece[i]] += (corner(c[0]) - lo).dot((corner(c[1]) - lo).cross(corner(c[2]) - lo));
    }
    for by in round.values().filter(|by| by.len() != 2) {
        by.iter().for_each(|i| shut[piece[*i]] = false);
    }
    for (i, c) in t.iter_mut().enumerate() {
        if shut[piece[i]] && vol[piece[i]] < 0.0 {
            c.swap(1, 2);
        }
    }
    let flat = |a: usize, b: usize| match (t.get(a).map(|c| normal(c).normalized()), t.get(b).map(|c| normal(c).normalized())) {
        (Some(n), Some(m)) => n.dot(m) > 1.0 - 1e-12,
        _ => false,
    };
    let edges = round
        .iter()
        .filter(|(_, by)| !(merge && matches!(by.as_slice(), [a, b] if flat(*a, *b))))
        .map(|((a, b), _)| vec![corner(*a), corner(*b)])
        .collect();
    cadcraft_geom::Mesh3 { verts: verts.to_vec(), tris: t, edges, open: !solid }
}

/// [`crate::boolean`]: faces merged, else the boolean's own result ([`merged_else_raw`]).
pub(super) fn boolean(op: BoolOp, a: Placed, b: Placed) -> Result<Built> {
    let op = match op {
        BoolOp::Union => 0,
        BoolOp::Subtract => 1,
        BoolOp::Intersect => 2,
    };
    let (a, b) = (placed(a), placed(b));
    merged_else_raw(|merge| text(x::boolean(&a, &b, op, merge)))
}

/// `run(true)` (faces merged, a result that isn't valid refused), else `run(false)`: merging can
/// spoil a good result (an OpenCASCADE weakness), and booleans never refused a result the checker
/// finds fault with (an imported or older solid), so they don't start to.
/// ponytail: a boolean that fails outright (an empty result) runs twice; tell the two apart if
/// that ever costs.
fn merged_else_raw(run: impl Fn(bool) -> Result<Built>) -> Result<Built> {
    run(true).or_else(|_| run(false))
}

/// [`crate::export_step`] (`iges` false) and [`crate::export_iges`]: the file is made in memory,
/// then written.
pub(super) fn exchange(solids: &[Placed], labels: &[Label], curves: &[Curve], iges: bool, path: &Path) -> Result<()> {
    let objects: Vec<x::ObjectIn> = solids
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let l = labels.get(i).cloned().unwrap_or_default();
            let color = l.color.map_or([-1.0; 3], |c| c.map(|v| f64::from(v) / 255.0));
            x::ObjectIn { shape: placed(*s), name: l.name, layer: l.layer, color }
        })
        .collect();
    let text = {
        let _one = exchanging();
        x::exchange(&objects, &wires(curves), iges).map_err(failed)?
    };
    std::fs::write(path, text).map_err(|e| KernelError::Failed(format!("writing {}: {e}", path.display())))
}

fn placed(p: Placed) -> x::ShapeIn {
    x::ShapeIn { brep: p.brep.to_owned(), xf: rows(p.xform) }
}

fn pick(s: &Solid, probe: Probe, tol: f64) -> Result<Picked> {
    let (from, along) = match probe {
        Probe::Ray { origin, dir } => (origin, dir),
        Probe::Point(p) => (p, Vec3::ZERO),
    };
    Ok(picked(&x::pick(&shape(s), &xyz(from), &xyz(along), tol).map_err(failed)?))
}

fn failed(e: x::Exception) -> KernelError {
    KernelError::Failed(e.what().to_owned())
}

/// The shape in B-rep text from our C++, meshed.
fn built(brep: String) -> Result<Built> {
    let s = read_brep(&brep)?;
    Ok(Built { mesh: mesh(&s, &brep)?, brep })
}

fn text(r: std::result::Result<String, x::Exception>) -> Result<Built> {
    built(r.map_err(failed)?)
}

fn texts(r: std::result::Result<Vec<String>, x::Exception>) -> Result<Vec<Built>> {
    r.map_err(failed)?.into_iter().map(built).collect()
}

fn xyz(p: Vec3) -> [f64; 3] {
    [p.x, p.y, p.z]
}

fn v3(a: [f64; 3]) -> Vec3 {
    Vec3::new(a[0], a[1], a[2])
}

fn points(f: &[f64]) -> Vec<Vec3> {
    f.chunks_exact(3).map(|p| Vec3::new(p[0], p[1], p[2])).collect()
}

/// The transform's top three rows.
fn rows(m: &Mat4) -> [f64; 12] {
    std::array::from_fn(|i| m.m.get(i / 4).and_then(|r| r.get(i % 4)).copied().unwrap_or(0.0))
}

fn shape(s: &Solid) -> x::ShapeIn {
    x::ShapeIn { brep: s.brep.clone(), xf: rows(&s.xform) }
}

fn shapes(ss: &[Solid]) -> Vec<x::ShapeIn> {
    ss.iter().map(shape).collect()
}

fn seg(s: &Seg) -> x::SegIn {
    match *s {
        Seg::Line { a, b } => x::SegIn { kind: 0, v: [xyz(a), xyz(b)].concat() },
        Seg::Arc { a, mid, b } => x::SegIn { kind: 1, v: [xyz(a), xyz(mid), xyz(b)].concat() },
        Seg::Circle { center, radius, normal } => x::SegIn { kind: 2, v: [&xyz(center)[..], &xyz(normal), &[radius]].concat() },
    }
}

fn wire(c: &Curve) -> x::WireIn {
    match c {
        Curve::Segs(s) => x::WireIn { segs: s.iter().map(seg).collect() },
        Curve::Spline { points, tangents } => {
            let (t0, t1) = tangents.unwrap_or((Vec3::ZERO, Vec3::ZERO));
            let mut v = vec![if tangents.is_some() { 1.0 } else { 0.0 }];
            v.extend(xyz(t0).into_iter().chain(xyz(t1)));
            v.extend(points.iter().flat_map(|p| xyz(*p)));
            x::WireIn { segs: vec![x::SegIn { kind: 3, v }] }
        }
    }
}

fn wires(cs: &[Curve]) -> Vec<x::WireIn> {
    cs.iter().map(wire).collect()
}

fn curve(c: &x::CurveOut) -> EdgeCurve {
    match c.kind {
        0 => EdgeCurve::Line { a: v3(c.a), b: v3(c.b) },
        1 => EdgeCurve::Arc { a: v3(c.a), mid: v3(c.mid), b: v3(c.b) },
        2 => EdgeCurve::Circle { center: v3(c.center), radius: c.radius, normal: v3(c.normal) },
        3 => EdgeCurve::Ellipse { center: v3(c.center), major: v3(c.major), ratio: c.ratio, normal: v3(c.normal), start: c.start, end: c.end },
        _ => EdgeCurve::Spline {
            degree: c.degree,
            poles: points(&c.poles),
            weights: c.weights.clone(),
            knots: c.knots.clone(),
            points: points(&c.points),
        },
    }
}

fn wire3(w: &x::WireOut) -> Wire3 {
    Wire3 { curves: w.curves.iter().map(curve).collect(), closed: w.closed }
}

fn surface_kind(k: u8) -> SurfaceKind {
    use SurfaceKind::*;
    [Plane, Cylinder, Cone, Sphere, Torus, Bezier, BSpline, Revolution, Extrusion, Offset].get(k as usize).copied().unwrap_or(Other)
}

fn topology(t: &x::TopoOut) -> Topology {
    Topology {
        faces: t
            .faces
            .iter()
            .map(|f| FaceInfo {
                kind: surface_kind(f.kind),
                area: f.area,
                centroid: v3(f.centroid),
                normal: v3(f.normal),
                bbox: (v3(f.lo), v3(f.hi)),
                axis: (v3(f.origin), v3(f.axis)),
                radius: f.radius,
                edges: f.edges.clone(),
            })
            .collect(),
        edges: t
            .edges
            .iter()
            .map(|e| EdgeInfo {
                curve: curve(&e.curve),
                length: e.length,
                mid: v3(e.curve.mid),
                faces: e.faces.clone(),
                seam: e.seam,
                degenerate: e.degenerate,
            })
            .collect(),
    }
}

fn picked(p: &x::PickOut) -> Picked {
    let hit = |h: &x::HitOut| Hit { index: h.index, point: v3(h.point), along: h.along, distance: h.distance };
    Picked { faces: p.faces.iter().map(hit).collect(), edges: p.edges.iter().map(hit).collect() }
}

fn section(s: x::SectionOut) -> Result<Section> {
    let regions = s
        .regions
        .into_iter()
        .map(|r| {
            let mut ws = r.wires.iter().map(wire3);
            let outer = ws.next().unwrap_or_default();
            Ok(SectionRegion { outer, holes: ws.collect(), face: built(r.brep)? })
        })
        .collect::<Result<_>>()?;
    Ok(Section { regions, wires: s.wires.iter().map(wire3).collect() })
}

#[cfg(test)]
mod tests;
