//! AutoCAD's mesh surfaces: EDGESURF (a Coons patch between four edges that meet end to end),
//! RULESURF (straight lines between two curves), REVSURF (a curve turned round an axis) and
//! TABSURF (a curve swept along a direction vector). Each makes a polygon mesh ([`PolyMesh`]) whose
//! density the SURFTAB1 and SURFTAB2 system variables set: the number of intervals a curve is
//! divided into (a polyline is divided at its vertices, its arcs into SURFTAB intervals each, where
//! REVSURF and TABSURF follow its shape).

use cadcraft_doc::{Entity, EntityKind, Handle, MAX_MESH_VERTICES, PolyMesh, Prim};
use cadcraft_geom::{Bounds2, Mat4, Segment, Vec2, Vec3};
use serde_json::{Value, json};

use super::curves::{handle_value, pick_at};
use super::solids::p3_value;
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("edgesurf", "Edge Mesh", run_edgesurf)
            .menu(&["Draw", "Modeling", "Meshes", "Edge Mesh"])
            .params("{edges: [h1, h2, h3, h4] (lines, arcs, splines or open polylines meeting end to end), pick?: [x,y,z] (on the first edge: the mesh's M direction runs from its end nearest this), surftab1?, surftab2?} → {handle}")
            .interactive(|_| Ok(Box::new(MeshM::new(Kind::Edge)))),
        CommandSpec::new("rulesurf", "Ruled Mesh", run_rulesurf)
            .menu(&["Draw", "Modeling", "Meshes", "Ruled Mesh"])
            .params("{curves: [h1, h2] (points, lines, arcs, circles, ellipses, polylines or splines; both open or both closed), picks?: [[x,y,z], [x,y,z]] (open curves run from their end nearest these), surftab1?} → {handle}")
            .interactive(|_| Ok(Box::new(MeshM::new(Kind::Ruled)))),
        CommandSpec::new("revsurf", "Revolved Mesh", run_revsurf)
            .menu(&["Draw", "Modeling", "Meshes", "Revolved Mesh"])
            .params("{curve: h, axis: h (a line or open polyline) | [[x,y,z], [x,y,z]], pick?: [x,y,z] (on the axis: it runs from its end nearest this), start?: degrees (0), angle?: degrees (360; + counterclockwise about the axis), surftab1?, surftab2?} → {handle}")
            .interactive(|_| Ok(Box::new(MeshM::new(Kind::Revolved)))),
        CommandSpec::new("tabsurf", "Tabulated Mesh", run_tabsurf)
            .menu(&["Draw", "Modeling", "Meshes", "Tabulated Mesh"])
            .params("{curve: h, vector: h (a line or open polyline) | [x,y,z], pick?: [x,y,z] (on the vector: it runs from its end nearest this), surftab1?} → {handle}")
            .interactive(|_| Ok(Box::new(MeshM::new(Kind::Tabulated)))),
    ]
}

fn other(m: impl Into<String>) -> EngineError {
    EngineError::Other(m.into())
}

/// SURFTAB1 or SURFTAB2: the parameter if given, else the drawing's (6 by default), 2 to 32766.
fn surftab(s: &Session, p: &Value, name: &str) -> Result<usize> {
    let key = name.to_ascii_lowercase();
    let v = match p.get(&key).filter(|v| !v.is_null()) {
        Some(v) => v.as_i64().ok_or_else(|| bad(&key, format!("`{key}` must be a whole number")))?,
        None => s.doc()?.header.i64(name, 6),
    };
    if !(2..=32766).contains(&v) {
        return Err(other(format!("{name} must be between 2 and 32766.")));
    }
    Ok(v as usize)
}

/// Refuse a mesh of more than [`MAX_MESH_VERTICES`] vertices before making it.
fn fits(rows: usize, cols: usize) -> Result<()> {
    if rows.checked_mul(cols).is_none_or(|n| n > MAX_MESH_VERTICES) {
        return Err(other(format!("The mesh would have more than {MAX_MESH_VERTICES} vertices: lower SURFTAB1 or SURFTAB2.")));
    }
    // A drawing file keeps M and N in 16-bit groups.
    if rows.max(cols) > MAX_MESH_SIDE {
        return Err(other(format!("The mesh would have more than {MAX_MESH_SIDE} vertices along one side: use a curve with fewer vertices.")));
    }
    Ok(())
}

/// Most vertices along one side of a polygon mesh (DXF 71/72 are 16-bit).
const MAX_MESH_SIDE: usize = i16::MAX as usize;

fn handle_req(cmd: &str, p: &Value, key: &str) -> Result<Handle> {
    p.get(key).and_then(handle_value).ok_or_else(|| bad(cmd, format!("`{key}` (an object's handle) is required")))
}

fn handles_req(cmd: &str, p: &Value, key: &str, n: usize) -> Result<Vec<Handle>> {
    let a = p.get(key).and_then(Value::as_array).filter(|a| a.len() == n);
    let hs: Option<Vec<Handle>> = a.and_then(|a| a.iter().map(handle_value).collect());
    hs.ok_or_else(|| bad(cmd, format!("`{key}` must be {n} object handles")))
}

fn entity(s: &Session, h: Handle) -> Result<Entity> {
    s.doc()?.entity(h).map(|e| (**e).clone()).ok_or_else(|| other(format!("No object with handle {}.", h.hex())))
}

// ---------------- curves as points ----------------

/// A curve object as world polylines, piece by piece (a polyline's segments, any other curve
/// whole), each dense enough to measure along; with whether the object is closed and whether its
/// pieces are a polyline's segments. `None` for objects that aren't curves.
struct Pieces {
    pieces: Vec<Vec<Vec3>>,
    closed: bool,
    polyline: bool,
}

/// [`Pieces`] of `e`; a point is one piece of one point.
fn pieces(e: &Entity) -> Option<Pieces> {
    let open = |pieces, polyline| Some(Pieces { pieces, closed: false, polyline });
    match &e.kind {
        EntityKind::Point(p) => return open(vec![vec![p.p]], false),
        EntityKind::Line(l) => return open(vec![vec![l.a, l.b]], false),
        EntityKind::Polyline3d(p) => {
            let mut segs: Vec<Vec<Vec3>> = p.points.windows(2).map(<[Vec3]>::to_vec).collect();
            if p.closed
                && let (Some(a), Some(b)) = (p.points.last(), p.points.first())
            {
                segs.push(vec![*a, *b]);
            }
            return Some(Pieces { pieces: segs, closed: p.closed, polyline: true });
        }
        EntityKind::Arc(_) | EntityKind::Circle(_) | EntityKind::Ellipse(_) | EntityKind::LwPolyline(_) | EntityKind::Spline(_) => {}
        _ => return None,
    }
    // A planar curve: its 2D shape in its own plane, lifted back.
    let pl = e.plane()?;
    let local = e.on(&pl)?;
    let b = Bounds2::from_points(local.kind.grips());
    let tol = (b.width().max(b.height()) * 1e-4).max(1e-9);
    let lift = |q: Vec2| pl.apply(q.to3(0.0));
    let mut out = Vec::new();
    for prim in local.kind.prims() {
        let mut pts = Vec::new();
        match prim {
            Prim::Seg(Segment::Line(l)) => pts = vec![l.a, l.b],
            Prim::Seg(seg) => seg.tessellate(tol, &mut pts),
            Prim::Circle(c) => c.tessellate(tol, &mut pts),
            Prim::Ellipse(el) => el.tessellate(tol, &mut pts),
            Prim::Spline(sp) => pts = sp.tessellate(tol),
            _ => continue,
        }
        out.push(pts.into_iter().map(lift).collect::<Vec<_>>());
    }
    let closed = match &local.kind {
        EntityKind::Circle(_) => true,
        EntityKind::Ellipse(el) => (el.end - el.start).abs() >= std::f64::consts::TAU - 1e-9,
        EntityKind::LwPolyline(p) => p.closed,
        EntityKind::Spline(sp) => sp.closed,
        _ => false,
    };
    (!out.is_empty()).then_some(Pieces { pieces: out, closed, polyline: matches!(local.kind, EntityKind::LwPolyline(_)) })
}

/// `n + 1` points dividing the polyline `pts` into `n` intervals of equal length.
fn divide(pts: &[Vec3], n: usize) -> Vec<Vec3> {
    let Some(&first) = pts.first() else { return Vec::new() };
    let lens: Vec<f64> = pts.windows(2).map(|w| (w[1] - w[0]).len()).collect();
    let total: f64 = lens.iter().sum();
    if !(total > 0.0 && total.is_finite()) {
        return vec![first; n + 1];
    }
    let mut out = Vec::with_capacity(n + 1);
    let (mut seg, mut at) = (0usize, 0.0);
    for k in 0..=n {
        let target = total * k as f64 / n as f64;
        while seg + 1 < lens.len() && at + lens[seg] < target {
            at += lens[seg];
            seg += 1;
        }
        let (Some(a), Some(b), Some(l)) = (pts.get(seg), pts.get(seg + 1), lens.get(seg)) else { break };
        let t = if *l > 0.0 { ((target - at) / l).clamp(0.0, 1.0) } else { 0.0 };
        out.push(*a + (*b - *a) * t);
    }
    if let (Some(last), Some(end)) = (out.last_mut(), pts.last()) {
        *last = *end;
    }
    out
}

/// Join pieces end to end (each next piece starts where the one before ends).
fn joined(pieces: &[Vec<Vec3>]) -> Vec<Vec3> {
    let mut out: Vec<Vec3> = Vec::new();
    for p in pieces {
        let skip = usize::from(!out.is_empty());
        out.extend(p.iter().skip(skip));
    }
    out
}

/// The points a mesh puts along curve `c`: `n` equal intervals, or with `by_vertices` a polyline's
/// vertices with each of its arcs divided into `n`. A closed curve's last point (its first again)
/// is left out.
fn samples(c: &Pieces, n: usize, by_vertices: bool) -> Vec<Vec3> {
    let mut pts = if by_vertices && c.polyline {
        let parts: Vec<Vec<Vec3>> = c.pieces.iter().map(|p| if p.len() == 2 { p.clone() } else { divide(p, n) }).collect();
        joined(&parts)
    } else {
        divide(&joined(&c.pieces), n)
    };
    if c.closed && pts.len() > 1 {
        pts.pop();
    }
    pts
}

/// The ends of curve `c`.
fn ends(c: &Pieces) -> Option<(Vec3, Vec3)> {
    Some((*c.pieces.first()?.first()?, *c.pieces.last()?.last()?))
}

fn reverse(c: &mut Pieces) {
    c.pieces.reverse();
    c.pieces.iter_mut().for_each(|p| p.reverse());
}

/// Whether point `at` (where an object was picked) is nearer `b` than `a` as the current view sees
/// them.
fn nearer_end(s: &Session, (a, b): (Vec3, Vec3), at: Vec3) -> bool {
    let dir = s.state().map(|st| st.view_dir).unwrap_or_default();
    let d = |p: Vec3| dir.project(p).dist(dir.project(at));
    d(b) < d(a)
}

/// Curve `h`, run from its end nearest `pick`; `refused`: the error when it is no curve.
fn curve(s: &Session, h: Handle, pick: Option<Vec3>, refused: &str) -> Result<Pieces> {
    let mut c = pieces(&entity(s, h)?).ok_or_else(|| other(refused))?;
    if let (Some(at), Some(e)) = (pick, ends(&c))
        && !c.closed
        && nearer_end(s, e, at)
    {
        reverse(&mut c);
    }
    Ok(c)
}

/// A direction from a line or open polyline (its first point to its last; from the end nearest
/// `pick`), or given as `[x,y,z]` (a vector) or `[[x,y,z], [x,y,z]]` (two points); `refused`: the
/// error for another object.
fn direction(s: &Session, cmd: &str, p: &Value, key: &str, pick: Option<Vec3>, refused: &str) -> Result<(Vec3, Vec3)> {
    let v = p.get(key).ok_or_else(|| bad(cmd, format!("`{key}` is required")))?;
    let (a, b) = if let Some(h) = handle_value(v) {
        let e = entity(s, h)?;
        let usable = matches!(&e.kind, EntityKind::Line(_))
            || matches!(&e.kind, EntityKind::LwPolyline(l) if !l.closed)
            || matches!(&e.kind, EntityKind::Polyline3d(l) if !l.closed);
        let ends = pieces(&e).filter(|_| usable).and_then(|c| ends(&c)).ok_or_else(|| other(refused))?;
        if pick.is_some_and(|at| nearer_end(s, ends, at)) { (ends.1, ends.0) } else { ends }
    } else if let Some([a, b]) = v.as_array().map(Vec::as_slice).filter(|a| a.len() == 2 && a.iter().all(Value::is_array)) {
        (p3_value(a).ok_or_else(|| bad(cmd, "a bad point"))?, p3_value(b).ok_or_else(|| bad(cmd, "a bad point"))?)
    } else {
        (Vec3::ZERO, p3_value(v).ok_or_else(|| bad(cmd, format!("`{key}` is a handle, [x,y,z] or [[x,y,z], [x,y,z]]")))?)
    };
    if !((b - a).len() > 1e-12 && a.is_finite() && b.is_finite()) {
        return Err(other("The direction has no length."));
    }
    Ok((a, b))
}

fn pick(p: &Value, key: &str) -> Option<Vec3> {
    p.get(key).and_then(p3_value)
}

fn add(s: &mut Session, m: Option<PolyMesh>) -> Result<Value> {
    let m = m.ok_or_else(|| other("The mesh has too few vertices."))?;
    let h = s.add_entity_world(EntityKind::PolyMesh(m))?;
    Ok(json!({ "handle": h.hex() }))
}

// ---------------- the four commands ----------------

fn run_rulesurf(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "rulesurf";
    let n = surftab(s, p, "SURFTAB1")?;
    fits(2, n + 1)?;
    let hs = handles_req(CMD, p, "curves", 2)?;
    let picks: Vec<Option<Vec3>> = (0..2).map(|i| p.get("picks").and_then(|a| a.get(i)).and_then(p3_value)).collect();
    let mut rows = Vec::new();
    let mut closed: Vec<Option<bool>> = Vec::new();
    for (h, at) in hs.iter().zip(&picks) {
        let c = curve(s, *h, *at, "Object not usable to define a ruled surface.")?;
        let point = matches!(entity(s, *h)?.kind, EntityKind::Point(_));
        closed.push((!point).then_some(c.closed));
        rows.push((c, point));
    }
    // A point pairs with an open or closed curve; two curves must both be open or both closed.
    let shut = match (closed.first().copied().flatten(), closed.get(1).copied().flatten()) {
        (Some(a), Some(b)) if a != b => return Err(other("One curve is open and the other closed: both must be open or both closed.")),
        (a, b) => a.or(b).unwrap_or(false),
    };
    let count = if shut { n } else { n + 1 };
    let mut verts = Vec::with_capacity(2 * count);
    for (c, point) in &rows {
        if *point {
            verts.extend(std::iter::repeat_n(ends(c).map_or(Vec3::ZERO, |e| e.0), count));
        } else {
            verts.extend(samples(c, n, false));
        }
    }
    add(s, PolyMesh::grid(2, count, verts, false, shut))
}

fn run_tabsurf(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "tabsurf";
    let n = surftab(s, p, "SURFTAB1")?;
    let c = curve(s, handle_req(CMD, p, "curve")?, None, "Object not usable to define a path curve.")?;
    if matches!(entity(s, handle_req(CMD, p, "curve")?)?.kind, EntityKind::Point(_)) {
        return Err(other("Object not usable to define a path curve."));
    }
    let (a, b) = direction(s, CMD, p, "vector", pick(p, "pick"), "Object not usable as a direction vector.")?;
    let path = samples(&c, n, true);
    fits(2, path.len())?;
    let v = b - a;
    let verts: Vec<Vec3> = path.iter().copied().chain(path.iter().map(|q| *q + v)).collect();
    add(s, PolyMesh::grid(2, path.len(), verts, false, c.closed))
}

fn run_revsurf(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "revsurf";
    let (n1, n2) = (surftab(s, p, "SURFTAB1")?, surftab(s, p, "SURFTAB2")?);
    let h = handle_req(CMD, p, "curve")?;
    if matches!(entity(s, h)?.kind, EntityKind::Point(_)) {
        return Err(other("Object not usable to define a curve of revolution."));
    }
    let c = curve(s, h, None, "Object not usable to define a curve of revolution.")?;
    let (o, b) = direction(s, CMD, p, "axis", pick(p, "pick"), "Object not usable as a rotation axis.")?;
    let start = f64_or(p, "start", 0.0);
    let angle = f64_or(p, "angle", 360.0);
    if !(start.is_finite() && angle.is_finite() && angle != 0.0) {
        return Err(bad(CMD, "`start` and `angle` (degrees, not 0) must be numbers"));
    }
    let angle = angle.clamp(-360.0, 360.0);
    let full = angle.abs() >= 360.0 - 1e-9;
    let rows = if full { n1 } else { n1 + 1 };
    let path = samples(&c, n2, true);
    fits(rows, path.len())?;
    let axis = (b - o).normalized();
    let mut verts = Vec::with_capacity(rows * path.len());
    for i in 0..rows {
        let turn = Mat4::rotate_about(o, axis, (start + angle * i as f64 / n1 as f64).to_radians());
        verts.extend(path.iter().map(|q| turn.apply(*q)));
    }
    add(s, PolyMesh::grid(rows, path.len(), verts, full, c.closed))
}

fn run_edgesurf(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "edgesurf";
    let (n1, n2) = (surftab(s, p, "SURFTAB1")?, surftab(s, p, "SURFTAB2")?);
    fits(n1 + 1, n2 + 1)?;
    let hs = handles_req(CMD, p, "edges", 4)?;
    let mut edges = Vec::with_capacity(4);
    for (i, h) in hs.iter().enumerate() {
        let c = curve(s, *h, if i == 0 { pick(p, "pick") } else { None }, "Object not usable to define a surface edge.")?;
        if c.closed || matches!(entity(s, *h)?.kind, EntityKind::Point(_)) {
            return Err(other("Object not usable to define a surface edge."));
        }
        edges.push(c);
    }
    // Ends meet within a millionth of the edges' size.
    let all: Vec<Vec3> = edges.iter().filter_map(ends).flat_map(|(a, b)| [a, b]).collect();
    let size = Bounds2::from_points(all.iter().map(|q| q.xy())).width().max(all.iter().fold(0.0f64, |m, q| m.max(q.z.abs()))).max(1.0);
    let meet = |a: Vec3, b: Vec3| (a - b).len() <= 1e-6 * size;
    let mut rest: Vec<Pieces> = edges.split_off(1);
    let Some(c0) = edges.pop() else { return Err(other("EDGESURF needs four edges.")) };
    let (p00, p10) = ends(&c0).ok_or_else(|| other("Edge 1 has no length."))?;
    // The edge from `at` (turned to start there), taken out of `rest`.
    let from = |at: Vec3, rest: &mut Vec<Pieces>| -> Option<Pieces> {
        let i = rest.iter().position(|c| ends(c).is_some_and(|(a, b)| meet(a, at) || meet(b, at)))?;
        let mut c = rest.remove(i);
        if ends(&c).is_some_and(|(a, _)| !meet(a, at)) {
            reverse(&mut c);
        }
        Some(c)
    };
    let not_touching = |k: usize| other(format!("Edge {k} does not touch another edge."));
    let d0 = from(p00, &mut rest).ok_or_else(|| not_touching(1))?;
    let d1 = from(p10, &mut rest).ok_or_else(|| not_touching(1))?;
    let (p01, p11) = (ends(&d0).map(|e| e.1), ends(&d1).map(|e| e.1));
    let (Some(p01), Some(p11)) = (p01, p11) else { return Err(not_touching(2)) };
    let c1 = from(p01, &mut rest).filter(|c| ends(c).is_some_and(|(_, b)| meet(b, p11))).ok_or_else(|| not_touching(4))?;
    let (u0, u1, v0, v1) = (samples(&c0, n1, false), samples(&c1, n1, false), samples(&d0, n2, false), samples(&d1, n2, false));
    // The Coons patch: the two pairs of opposite edges blended, less their corners blended.
    let mut verts = Vec::with_capacity((n1 + 1) * (n2 + 1));
    for i in 0..=n1 {
        let u = i as f64 / n1 as f64;
        for j in 0..=n2 {
            let v = j as f64 / n2 as f64;
            let (Some(a), Some(b), Some(c), Some(d)) = (u0.get(i), u1.get(i), v0.get(j), v1.get(j)) else {
                return Err(other("An edge could not be divided."));
            };
            let corners = p00 * ((1.0 - u) * (1.0 - v)) + p10 * (u * (1.0 - v)) + p01 * ((1.0 - u) * v) + p11 * (u * v);
            verts.push(*a * (1.0 - v) + *b * v + *c * (1.0 - u) + *d * u - corners);
        }
    }
    add(s, PolyMesh::grid(n1 + 1, n2 + 1, verts, false, false))
}

// ---------------- typed ----------------

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Edge,
    Ruled,
    Revolved,
    Tabulated,
}

/// The four commands at the command line: AutoCAD's object picks (the point each object was
/// picked at counts), then REVSURF's angles.
struct MeshM {
    kind: Kind,
    picks: Vec<(Handle, Vec3)>,
    start: Option<f64>,
}

impl MeshM {
    fn new(kind: Kind) -> Self {
        MeshM { kind, picks: Vec::new(), start: None }
    }
    /// Why object `e` can't be the next pick (as AutoCAD says it), if it can't.
    fn refuse(&self, e: &Entity) -> Option<&'static str> {
        let curve = pieces(e);
        let point = matches!(e.kind, EntityKind::Point(_));
        let closed = curve.as_ref().is_some_and(|c| c.closed);
        let vector = matches!(&e.kind, EntityKind::Line(_))
            || matches!(&e.kind, EntityKind::LwPolyline(l) if !l.closed)
            || matches!(&e.kind, EntityKind::Polyline3d(l) if !l.closed);
        let curve = curve.is_some();
        match (self.kind, self.picks.len()) {
            (Kind::Edge, _) => (!curve || point || closed).then_some("Object not usable to define a surface edge."),
            (Kind::Ruled, _) => (!curve).then_some("Object not usable to define a ruled surface."),
            (Kind::Tabulated, 0) => (!curve || point).then_some("Object not usable to define a path curve."),
            (Kind::Tabulated, _) => (!vector).then_some("Object not usable as a direction vector."),
            (Kind::Revolved, 0) => (!curve || point).then_some("Object not usable to define a curve of revolution."),
            (Kind::Revolved, _) => (!vector).then_some("Object not usable as a rotation axis."),
        }
    }
    /// How many objects the command picks.
    fn wanted(&self) -> usize {
        match self.kind {
            Kind::Edge => 4,
            _ => 2,
        }
    }
    fn json(&self, angle: f64) -> Value {
        let h = |i: usize| self.picks.get(i).map(|p| json!(p.0.hex()));
        let at = |i: usize| self.picks.get(i).map(|p| json!([p.1.x, p.1.y, p.1.z]));
        match self.kind {
            Kind::Edge => json!({ "edges": (0..4).map(h).collect::<Vec<_>>(), "pick": at(0) }),
            Kind::Ruled => json!({ "curves": [h(0), h(1)], "picks": [at(0), at(1)] }),
            Kind::Tabulated => json!({ "curve": h(0), "vector": h(1), "pick": at(1) }),
            Kind::Revolved => json!({ "curve": h(0), "axis": h(1), "pick": at(1), "start": self.start.unwrap_or(0.0), "angle": angle }),
        }
    }
    fn make(&self, s: &mut Session, angle: f64) -> Result<Step> {
        let p = self.json(angle);
        match self.kind {
            Kind::Edge => run_edgesurf(s, &p),
            Kind::Ruled => run_rulesurf(s, &p),
            Kind::Revolved => run_revsurf(s, &p),
            Kind::Tabulated => run_tabsurf(s, &p),
        }?;
        Ok(Step::Done)
    }
}

/// The object at point `p` given at a "Select" prompt and where on it (in the world) it was picked.
fn picked(s: &Session, p: Vec2) -> Option<(Handle, Vec3)> {
    let h = pick_at(s, p)?;
    let at = match s.view_pick {
        Some((v, w)) if v == h => w,
        _ => s.cplane().apply(p.to3(s.point_z.unwrap_or(0.0))),
    };
    Some((h, at))
}

impl Interactive for MeshM {
    fn name(&self) -> &'static str {
        match self.kind {
            Kind::Edge => "EDGESURF",
            Kind::Ruled => "RULESURF",
            Kind::Revolved => "REVSURF",
            Kind::Tabulated => "TABSURF",
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let d = s.doc()?;
        let (t1, t2) = (d.header.i64("SURFTAB1", 6), d.header.i64("SURFTAB2", 6));
        s.echo(match self.kind {
            Kind::Ruled | Kind::Tabulated => format!("Current wire frame density:  SURFTAB1={t1}"),
            Kind::Edge | Kind::Revolved => format!("Current wire frame density:  SURFTAB1={t1}  SURFTAB2={t2}"),
        });
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        let k = self.picks.len();
        let msg = match (self.kind, k) {
            (Kind::Edge, k) if k < 4 => format!("Select object {} for surface edge", k + 1),
            (Kind::Ruled, 0) => "Select first defining curve".into(),
            (Kind::Ruled, _) => "Select second defining curve".into(),
            (Kind::Tabulated, 0) => "Select object for path curve".into(),
            (Kind::Tabulated, _) => "Select object for direction vector".into(),
            (Kind::Revolved, 0) => "Select object to revolve".into(),
            (Kind::Revolved, 1) => "Select object that defines the axis of revolution".into(),
            (Kind::Revolved, _) if self.start.is_none() => return Prompt::new("Specify start angle", Accept::NUMBER).default("0"),
            _ => return Prompt::new("Specify included angle (+=ccw, -=cw)", Accept::NUMBER).default("360"),
        };
        Prompt::new(msg, Accept::POINT)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if self.picks.len() < self.wanted() {
            return match i {
                Input::Point(p) => {
                    let hit = picked(s, p);
                    let refused = hit.and_then(|(h, _)| s.doc().ok()?.entity(h).and_then(|e| self.refuse(e)));
                    match (hit, refused) {
                        (Some(_), Some(why)) => s.echo(why),
                        (Some(hit), None) => self.picks.push(hit),
                        (None, _) => s.echo("No object found."),
                    }
                    if self.picks.len() == self.wanted() && self.kind != Kind::Revolved {
                        return self.make(s, 360.0);
                    }
                    Ok(Step::Continue)
                }
                Input::Cancel | Input::Enter => Ok(Step::Cancel),
                _ => Ok(Step::Continue),
            };
        }
        // REVSURF's angles: Enter takes the default.
        let value = match i {
            Input::Enter => None,
            Input::Text(t) => Some(super::solids::distance(&t).map_err(|_| other("Requires a number."))?),
            Input::Cancel => return Ok(Step::Cancel),
            _ => return Ok(Step::Continue),
        };
        match self.start {
            None => {
                self.start = Some(value.unwrap_or(0.0));
                Ok(Step::Continue)
            }
            Some(_) => self.make(s, value.unwrap_or(360.0)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use cadcraft_doc::{Circle, Line, Point};

    use super::*;

    fn run(s: &mut Session, id: &str, p: Value) -> PolyMesh {
        let v = s.execute(id, &p).unwrap();
        let h = Handle::parse_hex(v["handle"].as_str().unwrap()).unwrap();
        match &s.doc().unwrap().entity(h).unwrap().kind {
            EntityKind::PolyMesh(m) => m.clone(),
            k => panic!("{k:?}"),
        }
    }

    fn add(s: &mut Session, k: EntityKind) -> String {
        s.add_entity_world(k).unwrap().hex()
    }

    fn line(s: &mut Session, a: [f64; 3], b: [f64; 3]) -> String {
        add(s, EntityKind::Line(Line { a: Vec3::new(a[0], a[1], a[2]), b: Vec3::new(b[0], b[1], b[2]) }))
    }

    fn near(a: Vec3, b: Vec3) -> bool {
        (a - b).len() < 1e-9
    }

    /// EDGESURF over four lines round a square, picked in any order and direction: a 7 × 7 mesh
    /// (SURFTAB 6) whose M direction runs along the first edge from the end picked; a corner that
    /// doesn't meet is refused.
    #[test]
    fn edgesurf_makes_a_coons_patch() {
        let mut s = Session::new();
        let a = line(&mut s, [0.0, 0.0, 0.0], [10.0, 0.0, 0.0]);
        let b = line(&mut s, [10.0, 10.0, 5.0], [10.0, 0.0, 0.0]);
        let c = line(&mut s, [0.0, 10.0, 0.0], [10.0, 10.0, 5.0]);
        let d = line(&mut s, [0.0, 0.0, 0.0], [0.0, 10.0, 0.0]);
        let m = run(&mut s, "edgesurf", json!({ "edges": [a, c, b, d] }));
        assert_eq!((m.m, m.n, m.vertices.len()), (7, 7, 49));
        assert!(near(m.vertices[0], Vec3::ZERO) && near(m.vertices[6 * 7], Vec3::new(10.0, 0.0, 0.0)), "M runs along the first edge");
        assert!(near(m.vertices[48], Vec3::new(10.0, 10.0, 5.0)));
        // The middle of a bilinear patch.
        assert!(near(m.vertices[3 * 7 + 3], Vec3::new(5.0, 5.0, 1.25)), "{:?}", m.vertices[24]);
        // Picked near its far end, the first edge runs the other way.
        let m = run(&mut s, "edgesurf", json!({ "edges": [a, b, c, d], "pick": [9, 0, 0], "surftab1": 2, "surftab2": 3 }));
        assert_eq!((m.m, m.n), (3, 4));
        assert!(near(m.vertices[0], Vec3::new(10.0, 0.0, 0.0)));
        let e = line(&mut s, [0.0, 0.5, 0.0], [0.0, 10.0, 0.0]);
        assert!(s.execute("edgesurf", &json!({ "edges": [a, b, c, e] })).is_err());
    }

    /// RULESURF between two parallel lines picked at opposite ends twists; between a circle and a
    /// point it is a closed cone; an open and a closed curve are refused. TABSURF sweeps a circle
    /// along a vector; REVSURF turns a line round an axis, a full turn closed in M.
    #[test]
    fn ruled_tabulated_and_revolved_meshes() {
        let mut s = Session::new();
        let a = line(&mut s, [0.0, 0.0, 0.0], [10.0, 0.0, 0.0]);
        let b = line(&mut s, [0.0, 5.0, 2.0], [10.0, 5.0, 2.0]);
        let m = run(&mut s, "rulesurf", json!({ "curves": [a, b] }));
        assert_eq!((m.m, m.n, m.closed_n), (2, 7, false));
        assert!(near(m.vertices[7], Vec3::new(0.0, 5.0, 2.0)));
        let m = run(&mut s, "rulesurf", json!({ "curves": [a, b], "picks": [[1, 0, 0], [9, 5, 2]] }));
        assert!(near(m.vertices[7], Vec3::new(10.0, 5.0, 2.0)), "the second curve starts at its end nearest the pick");
        let c = add(&mut s, EntityKind::Circle(Circle { center: Vec3::ZERO, radius: 3.0 }));
        let pt = add(&mut s, EntityKind::Point(Point { p: Vec3::new(0.0, 0.0, 8.0), angle: 0.0 }));
        let m = run(&mut s, "rulesurf", json!({ "curves": [c, pt], "surftab1": 12 }));
        assert_eq!((m.n, m.closed_n), (12, true));
        assert!(near(m.vertices[0], Vec3::new(3.0, 0.0, 0.0)), "a circle starts at 0 degrees: {:?}", m.vertices[0]);
        assert!(m.vertices[12..].iter().all(|q| near(*q, Vec3::new(0.0, 0.0, 8.0))));
        assert!(s.execute("rulesurf", &json!({ "curves": [a, c] })).is_err());

        let m = run(&mut s, "tabsurf", json!({ "curve": c, "vector": b }));
        assert_eq!((m.m, m.n, m.closed_n), (2, 6, true));
        assert!(near(m.vertices[6], Vec3::new(13.0, 0.0, 0.0)), "{:?}", m.vertices[6]);
        let m = run(&mut s, "tabsurf", json!({ "curve": c, "vector": b, "pick": [9, 5, 2] }));
        assert!(near(m.vertices[6], Vec3::new(-7.0, 0.0, 0.0)), "picked at its far end, the vector points back: {:?}", m.vertices[6]);

        let axis = line(&mut s, [0.0, 0.0, 0.0], [0.0, 0.0, 10.0]);
        let prof = line(&mut s, [5.0, 0.0, 0.0], [2.0, 0.0, 10.0]);
        let m = run(&mut s, "revsurf", json!({ "curve": prof, "axis": axis, "surftab1": 8, "surftab2": 4 }));
        assert_eq!((m.m, m.n, m.closed_m, m.closed_n), (8, 5, true, false));
        assert!(near(m.vertices[2 * 5], Vec3::new(0.0, 5.0, 0.0)), "a quarter turn counterclockwise about +Z: {:?}", m.vertices[10]);
        let m = run(&mut s, "revsurf", json!({ "curve": prof, "axis": axis, "start": 90, "angle": -90, "surftab1": 2, "surftab2": 2 }));
        assert_eq!((m.m, m.closed_m), (3, false));
        assert!(near(m.vertices[0], Vec3::new(0.0, 5.0, 0.0)) && near(m.vertices[2 * 3], Vec3::new(5.0, 0.0, 0.0)), "{:?}", m.vertices);
        assert!(s.execute("revsurf", &json!({ "curve": prof, "axis": c })).is_err(), "a circle is no axis");
    }

    /// SURFTAB1 and SURFTAB2 come from the drawing; SETVAR refuses values outside 2–32766, and a
    /// mesh too large to make is refused before it is made.
    #[test]
    fn surftab_sets_the_density() {
        let mut s = Session::new();
        s.execute("setvar", &json!({ "name": "SURFTAB1", "value": 3 })).unwrap();
        assert!(s.execute("setvar", &json!({ "name": "SURFTAB2", "value": 1 })).is_err());
        let a = line(&mut s, [0.0, 0.0, 0.0], [10.0, 0.0, 0.0]);
        let b = line(&mut s, [0.0, 5.0, 0.0], [10.0, 5.0, 0.0]);
        let m = run(&mut s, "rulesurf", json!({ "curves": [a, b] }));
        assert_eq!(m.n, 4);
        assert!(s.execute("rulesurf", &json!({ "curves": [a, b], "surftab1": 32766 })).is_ok());
        assert!(s.execute("edgesurf", &json!({ "edges": [a, b, a, b], "surftab1": 32766, "surftab2": 32766 })).is_err());
        assert!(s.execute("rulesurf", &json!({ "curves": [a, b], "surftab1": 1 })).is_err());
        // A side longer than a drawing file's 16-bit count (TABSURF along a 40,000-point path).
        assert!(fits(2, 32_767).is_ok());
        assert!(fits(2, 40_000).unwrap_err().to_string().contains("along one side"));
    }

    /// A mesh's grips are its vertices (a grip edit moves one); MOVE moves it in 3D, EXPLODE
    /// makes its 3D faces.
    #[test]
    fn a_mesh_takes_grips_moves_and_explodes() {
        let mut s = Session::new();
        let a = line(&mut s, [0.0, 0.0, 0.0], [10.0, 0.0, 0.0]);
        let b = line(&mut s, [0.0, 5.0, 2.0], [10.0, 5.0, 2.0]);
        let v = s.execute("rulesurf", &json!({ "curves": [a, b], "surftab1": 3 })).unwrap();
        let h = Handle::parse_hex(v["handle"].as_str().unwrap()).unwrap();
        assert_eq!(s.grips_of(h).unwrap().len(), 8);
        let k = crate::grips::stretch_grip(&s.doc().unwrap().entity(h).unwrap().kind, 4, Vec2::new(-1.0, 6.0)).unwrap();
        let EntityKind::PolyMesh(m) = k else { panic!() };
        assert!(near(m.vertices[4], Vec3::new(-1.0, 6.0, 2.0)), "the vertex moves across, keeping its height");
        s.execute("3dmove", &json!({ "handles": [h.hex()], "delta": [0, 0, 1] })).unwrap();
        let EntityKind::PolyMesh(m) = &s.doc().unwrap().entity(h).unwrap().kind else { panic!() };
        assert!(near(m.vertices[7], Vec3::new(10.0, 5.0, 3.0)));
        s.execute("explode", &json!({ "handles": [h.hex()] })).unwrap();
        let faces = s.doc().unwrap().model.iter().filter(|e| matches!(e.kind, EntityKind::Face3d(_))).count();
        assert_eq!(faces, 3);
    }

    /// Typed, the commands echo the density and ask AutoCAD's questions; an object that can't be
    /// used is refused at its prompt; REVSURF's angles take their defaults on Enter.
    #[test]
    fn typed_prompts() {
        let setup = || {
            let mut s = Session::new();
            line(&mut s, [0.0, 0.0, 0.0], [0.0, 10.0, 0.0]);
            line(&mut s, [5.0, 0.0, 0.0], [5.0, 10.0, 0.0]);
            add(&mut s, EntityKind::Circle(Circle { center: Vec3::new(20.0, 0.0, 0.0), radius: 1.0 }));
            s
        };
        let mut s = setup();
        s.cmdline("RULESURF").unwrap();
        assert!(s.log.iter().any(|l| l.contains("Current wire frame density:  SURFTAB1=6")), "{:?}", s.log);
        assert!(s.prompt_text().ends_with("Select first defining curve:"), "{}", s.prompt_text());
        s.cmdline("0,1").unwrap();
        assert!(s.prompt_text().ends_with("Select second defining curve:"), "{}", s.prompt_text());
        s.cmdline("5,1").unwrap();
        assert!(s.running.is_none());
        let mut s = setup();
        s.cmdline("REVSURF").unwrap();
        assert!(s.prompt_text().ends_with("Select object to revolve:"), "{}", s.prompt_text());
        s.cmdline("5,5").unwrap();
        assert!(s.prompt_text().ends_with("Select object that defines the axis of revolution:"), "{}", s.prompt_text());
        s.cmdline("21,0").unwrap();
        assert!(s.log.last().is_some_and(|l| l.contains("Object not usable as a rotation axis")), "{:?}", s.log);
        s.cmdline("0,1").unwrap();
        assert!(s.prompt_text().ends_with("Specify start angle <0>:"), "{}", s.prompt_text());
        s.cmdline("").unwrap();
        assert!(s.prompt_text().ends_with("Specify included angle (+=ccw, -=cw) <360>:"), "{}", s.prompt_text());
        s.cmdline("180").unwrap();
        assert!(s.running.is_none());
        let m: Vec<PolyMesh> =
            s.doc().unwrap().model.iter().filter_map(|e| if let EntityKind::PolyMesh(m) = &e.kind { Some(m.clone()) } else { None }).collect();
        assert_eq!(m.len(), 1);
        // Picked near (0, 1), the axis runs from (0, 0) up +Y; half a turn takes x = 5 to x = -5.
        assert!(near(m[0].vertices[6 * 7], Vec3::new(-5.0, 0.0, 0.0)), "{:?}", m[0].vertices);
        assert!(!m[0].closed_m);
    }
}
