//! ACIS solid model data, the geometry AutoCAD keeps in a 3DSOLID: a manifold body (body, lump,
//! shell, faces, loops, coedges, edges, vertices) written as SAT text or SAB binary. Written from
//! Spatial's published SAT/SAB save file format and the DXF Reference.
//!
//! The body is polyhedral: built from a solid's display mesh, with each set of connected coplanar
//! triangles one planar face (`plane-surface`) bounded by straight edges (`straight-curve`). That
//! is exact for flat-faced solids (panels, boxes) and faceted for curved ones; valid ACIS either way.
//! ponytail: exact cylinders, cones, spheres and tori need the kernel's face of each mesh triangle
//! (`cadcraft_kernel::Built` has none yet); add `cylinder-surface`/`ellipse-curve` records then.
//!
//! Versions: SAT 700 (`ACIS 32.0 NT`) for DXF and DWG 2000–2010, SAB 21800 with an `asmheader`
//! record for 2013 and later. The SAB record layout of 21800 adds an integer to vertices (the count
//! of edges at the vertex) and to coedges (0).

use std::collections::HashMap;

use cadcraft_geom::{Mat4, Mesh3, Vec3};

#[cfg(test)]
pub(crate) mod parse;

/// Most mesh triangles made into an ACIS body; a larger mesh gets none.
const MAX_TRIS: usize = 200_000;
/// How far (drawing units) a triangle's corner may be from a face's plane and still join it. Under
/// ACIS's absolute resolution (1e-6), so every vertex lies on its face's surface.
const PLANE_TOL: f64 = 1e-7;
/// Recorded in the files; ACIS readers don't depend on it. ponytail: a fixed date keeps the output
/// reproducible.
const DATE: &str = "Mon Jan 15 00:00:00 2024";
const PRODUCT: &str = "Cadino";
/// The SAB end marker of ASM (21800) data: "End", "of", "ASM" as sub-type names, "data" a type.
const SAB_END: &[u8] = b"\x0e\x03End\x0e\x02of\x0e\x03ASM\x0d\x04data";

/// A body made from a mesh (see [`body`]).
#[derive(Debug)]
pub(crate) struct Body {
    pts: Vec<Vec3>,
    /// Start and end vertex, first coedge.
    edges: Vec<[usize; 3]>,
    coedges: Vec<Coedge>,
    /// First coedge, face.
    loops: Vec<[usize; 2]>,
    faces: Vec<Face>,
    /// An open mesh (a surface, not a solid): double-sided faces.
    open: bool,
}

#[derive(Debug)]
struct Coedge {
    next: usize,
    prev: usize,
    partner: Option<usize>,
    edge: usize,
    /// Runs from the edge's end to its start.
    reversed: bool,
    lp: usize,
}

#[derive(Debug)]
struct Face {
    loops: Vec<usize>,
    origin: Vec3,
    /// Outward unit normal.
    normal: Vec3,
    /// Unit direction in the plane.
    u: Vec3,
}

/// The ACIS body of `mesh` placed by `xform` (model → world): `None` when the mesh is empty, too
/// large, or not a consistently oriented manifold (an edge shared by more than two triangles, or by
/// two running the same way), which ACIS can't take as a body.
pub(crate) fn body(mesh: &Mesh3, xform: &Mat4) -> Option<Body> {
    if mesh.tris.is_empty() || mesh.tris.len() > MAX_TRIS {
        return None;
    }
    // Weld coincident corners: a mesh may repeat a corner per face.
    let world: Vec<Vec3> = mesh.verts.iter().map(|p| xform.apply(*p)).collect();
    if world.iter().any(|p| !p.is_finite()) {
        return None;
    }
    let size = world.iter().fold(1.0_f64, |m, p| m.max(p.x.abs()).max(p.y.abs()).max(p.z.abs()));
    // ponytail: rounding to a grid keeps apart two corners a hair apart that straddle a grid line;
    // meshes repeat corners bit for bit, so it doesn't happen in practice.
    let step = size * 1e-12;
    let mut ids: HashMap<[i64; 3], usize> = HashMap::new();
    let mut pts: Vec<Vec3> = Vec::new();
    let weld: Vec<usize> = world
        .iter()
        .map(|p| {
            let key = [p.x, p.y, p.z].map(|c| (c / step).round() as i64);
            *ids.entry(key).or_insert_with(|| {
                pts.push(*p);
                pts.len() - 1
            })
        })
        .collect();
    // A mirroring transform turns the triangles inside out: turn them back.
    let flip = xform.det3() < 0.0;
    let mut tris: Vec<[usize; 3]> = Vec::with_capacity(mesh.tris.len());
    for t in &mesh.tris {
        let c = t.map(|i| weld.get(i as usize).copied());
        let [Some(a), Some(b), Some(c)] = c else { return None };
        if a == b || b == c || c == a {
            continue;
        }
        tris.push(if flip { [a, c, b] } else { [a, b, c] });
    }
    let corner = |t: &[usize; 3], k: usize| pts.get(t[k % 3]).copied().unwrap_or_default();
    let normal = |t: &[usize; 3]| (corner(t, 1) - corner(t, 0)).cross(corner(t, 2) - corner(t, 0));
    // Half-edge `3 * t + k` runs from corner k to corner k + 1 of triangle t.
    let from = |h: usize| tris.get(h / 3).map_or(0, |t| t[h % 3]);
    let to = |h: usize| tris.get(h / 3).map_or(0, |t| t[(h + 1) % 3]);
    let mut half: HashMap<(usize, usize), usize> = HashMap::with_capacity(tris.len() * 3);
    for h in 0..tris.len() * 3 {
        if half.insert((from(h), to(h)), h).is_some() {
            return None;
        }
    }
    let twin = |h: usize| half.get(&(to(h), from(h))).copied();
    let open = (0..tris.len() * 3).any(|h| twin(h).is_none());

    // Faces: connected coplanar triangles, seeded largest first so a face's plane is its biggest
    // triangle's.
    let mut order: Vec<usize> = (0..tris.len()).collect();
    order.sort_by(|a, b| {
        let area = |t: &usize| tris.get(*t).map_or(0.0, |t| normal(t).len());
        area(b).total_cmp(&area(a))
    });
    let mut region = vec![usize::MAX; tris.len()];
    let mut planes: Vec<(Vec3, f64)> = Vec::new();
    for &seed in &order {
        let Some(st) = tris.get(seed) else { continue };
        if region.get(seed) != Some(&usize::MAX) {
            continue;
        }
        let n = normal(st).normalized();
        if n == Vec3::ZERO {
            // A sliver: no plane of its own.
            return None;
        }
        let r = planes.len();
        let d = n.dot(corner(st, 0));
        planes.push((n, d));
        region[seed] = r;
        let mut stack = vec![seed];
        while let Some(t) = stack.pop() {
            for k in 0..3 {
                let Some(o) = twin(3 * t + k).map(|h| h / 3) else { continue };
                let Some(ot) = tris.get(o) else { continue };
                let flat = (0..3).all(|j| (n.dot(corner(ot, j)) - d).abs() <= PLANE_TOL) && normal(ot).dot(n) > 0.0;
                if region.get(o) == Some(&usize::MAX) && flat {
                    region[o] = r;
                    stack.push(o);
                }
            }
        }
    }

    // A face's boundary: its half-edges whose twin is on another face (or missing), in loops.
    let mut outgoing: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    let boundary = |h: usize| twin(h).is_none_or(|o| region.get(o / 3) != region.get(h / 3));
    for h in (0..tris.len() * 3).filter(|h| boundary(*h)) {
        let r = region.get(h / 3).copied().unwrap_or(usize::MAX);
        outgoing.entry((r, from(h))).or_default().push(h);
    }
    let mut b = Body { pts: Vec::new(), edges: Vec::new(), coedges: Vec::new(), loops: Vec::new(), faces: Vec::new(), open };
    let mut coedge_of: HashMap<usize, usize> = HashMap::new();
    let mut vertex_of: HashMap<usize, usize> = HashMap::new();
    let mut edge_of: HashMap<(usize, usize), usize> = HashMap::new();
    let mut keys: Vec<(usize, usize)> = outgoing.keys().copied().collect();
    keys.sort_unstable();
    let mut faces: Vec<Vec<usize>> = vec![Vec::new(); planes.len()];
    for key in keys {
        while let Some(start) = outgoing.get_mut(&key).and_then(Vec::pop) {
            let (r, v0) = key;
            let first = b.coedges.len();
            let lp = b.loops.len();
            let mut h = start;
            loop {
                let (u, v) = (from(h), to(h));
                let e = *edge_of.entry((u.min(v), u.max(v))).or_insert_with(|| {
                    let mut vertex = |p: usize| {
                        *vertex_of.entry(p).or_insert_with(|| {
                            b.pts.push(pts.get(p).copied().unwrap_or_default());
                            b.pts.len() - 1
                        })
                    };
                    let (s, t) = (vertex(u.min(v)), vertex(u.max(v)));
                    b.edges.push([s, t, b.coedges.len()]);
                    b.edges.len() - 1
                });
                let c = b.coedges.len();
                coedge_of.insert(h, c);
                b.coedges.push(Coedge { next: c + 1, prev: c.wrapping_sub(1), partner: None, edge: e, reversed: u > v, lp });
                if v == v0 {
                    break;
                }
                // An unclosed boundary (a broken mesh) ends the body.
                h = outgoing.get_mut(&(r, v)).and_then(Vec::pop)?;
                if b.coedges.len() - first > tris.len() * 3 {
                    return None;
                }
            }
            let last = b.coedges.len() - 1;
            if let Some(c) = b.coedges.get_mut(first) {
                c.prev = last;
            }
            if let Some(c) = b.coedges.get_mut(last) {
                c.next = first;
            }
            b.loops.push([first, r]);
            faces.get_mut(r)?.push(lp);
        }
    }
    for (h, &c) in &coedge_of {
        let partner = twin(*h).and_then(|o| coedge_of.get(&o).copied());
        if let Some(co) = b.coedges.get_mut(c) {
            co.partner = partner;
        }
    }
    // Faces in the order of their first loops; a face's loops point at its index.
    let mut index = vec![usize::MAX; planes.len()];
    for (r, loops) in faces.into_iter().enumerate().filter(|(_, l)| !l.is_empty()) {
        let (normal, _) = planes.get(r).copied()?;
        let first = loops.first().and_then(|l| b.loops.get(*l)).and_then(|l| b.coedges.get(l[0]))?;
        let edge = b.edges.get(first.edge)?;
        let (s, t) = (b.pts.get(edge[0]).copied()?, b.pts.get(edge[1]).copied()?);
        let (origin, along) = if first.reversed { (t, s - t) } else { (s, t - s) };
        let u = (along - normal * normal.dot(along)).normalized();
        if u == Vec3::ZERO {
            return None;
        }
        index[r] = b.faces.len();
        b.faces.push(Face { loops, origin, normal, u });
    }
    for l in &mut b.loops {
        l[1] = index.get(l[1]).copied().filter(|i| *i != usize::MAX)?;
    }
    Some(b)
}

/// A real as SAT text: shortest round trip, with an exponent when very small or large (as C's `%g`
/// writes it) so no number is dozens of digits long.
struct Real(f64);

impl std::fmt::Display for Real {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let a = self.0.abs();
        if a == 0.0 || (1e-5..1e15).contains(&a) { write!(f, "{}", self.0) } else { write!(f, "{:e}", self.0) }
    }
}

/// A value in a record, written as SAT text or SAB binary.
enum T {
    /// A record, by index.
    P(usize),
    Null,
    I(i64),
    F(f64),
    /// A position, then a direction.
    Pos(Vec3),
    Dir(Vec3),
    /// A logical: the value, its SAT words for true and false.
    B(bool, &'static str, &'static str),
    S(&'static str),
}

const FORWARD: T = T::B(false, "reversed", "forward");
const INFINITE: T = T::B(false, "F", "I");

impl Body {
    /// Counts: (faces, loops, coedges, edges, vertices).
    #[cfg(test)]
    fn counts(&self) -> [usize; 5] {
        [self.faces.len(), self.loops.len(), self.coedges.len(), self.edges.len(), self.pts.len()]
    }

    /// The records in file order; `asm` (21800) puts an `asmheader` first and adds the 21800
    /// integers to vertices and coedges.
    fn records_for(&self, asm: bool) -> Vec<(&'static str, Vec<T>)> {
        use T::{I, Null, P};
        let (nf, nl, nc, ne, nv) = (self.faces.len(), self.loops.len(), self.coedges.len(), self.edges.len(), self.pts.len());
        let body = usize::from(asm);
        let (lump, shell, face) = (body + 1, body + 2, body + 3);
        let lp = face + nf;
        let surface = lp + nl;
        let coedge = surface + nf;
        let edge = coedge + nc;
        let curve = edge + ne;
        let vertex = curve + ne;
        let point = vertex + nv;
        let ptr = |base: usize, i: Option<usize>| i.map_or(Null, |i| P(base + i));
        let mut out: Vec<(&'static str, Vec<T>)> = Vec::with_capacity(point + nv);
        if asm {
            out.push(("asmheader", vec![T::S("208.0.4.7009")]));
        }
        // Each entity's own data after its attribute pointer and id; topology and geometry start
        // with a pattern pointer.
        out.push(("body", vec![Null, P(lump), Null, Null]));
        out.push(("lump", vec![Null, Null, P(shell), P(body)]));
        out.push(("shell", vec![Null, Null, Null, ptr(face, (nf > 0).then_some(0)), Null, P(lump)]));
        for (i, f) in self.faces.iter().enumerate() {
            let next = ptr(face, (i + 1 < nf).then_some(i + 1));
            let mut v = vec![Null, next, ptr(lp, f.loops.first().copied()), P(shell), Null, P(surface + i), FORWARD];
            v.push(T::B(self.open, "double", "single"));
            if self.open {
                // Both sides outside: a sheet, not the skin of a solid.
                v.push(T::B(false, "in", "out"));
            }
            out.push(("face", v));
        }
        for (i, l) in self.loops.iter().enumerate() {
            let face_loops = self.faces.get(l[1]).map(|f| f.loops.as_slice()).unwrap_or_default();
            let at = face_loops.iter().position(|x| *x == i);
            let next = at.and_then(|k| face_loops.get(k + 1)).copied();
            out.push(("loop", vec![Null, ptr(lp, next), P(coedge + l[0]), P(face + l[1])]));
        }
        for f in &self.faces {
            let v = vec![
                Null,
                T::Pos(f.origin),
                T::Dir(f.normal),
                T::Dir(f.u),
                T::B(false, "reverse_v", "forward_v"),
                INFINITE,
                INFINITE,
                INFINITE,
                INFINITE,
            ];
            out.push(("plane-surface", v));
        }
        for c in &self.coedges {
            let mut v = vec![Null, P(coedge + c.next), P(coedge + c.prev), ptr(coedge, c.partner), P(edge + c.edge)];
            v.push(T::B(c.reversed, "reversed", "forward"));
            v.push(P(lp + c.lp));
            if asm {
                v.push(I(0));
            }
            v.push(Null);
            out.push(("coedge", v));
        }
        let mut at_vertex = vec![0i64; nv];
        let mut vertex_edge = vec![0usize; nv];
        for (i, e) in self.edges.iter().enumerate().rev() {
            for &p in &e[..2] {
                if let (Some(n), Some(first)) = (at_vertex.get_mut(p), vertex_edge.get_mut(p)) {
                    *n += 1;
                    *first = i;
                }
            }
        }
        for (i, e) in self.edges.iter().enumerate() {
            let len = self.pts.get(e[1]).copied().unwrap_or_default() - self.pts.get(e[0]).copied().unwrap_or_default();
            out.push((
                "edge",
                vec![Null, P(vertex + e[0]), T::F(0.0), P(vertex + e[1]), T::F(len.len()), P(coedge + e[2]), P(curve + i), FORWARD, T::S("unknown")],
            ));
        }
        for e in &self.edges {
            let (s, t) = (self.pts.get(e[0]).copied().unwrap_or_default(), self.pts.get(e[1]).copied().unwrap_or_default());
            out.push(("straight-curve", vec![Null, T::Pos(s), T::Dir((t - s).normalized()), INFINITE, INFINITE]));
        }
        for (i, (n, first)) in at_vertex.iter().zip(&vertex_edge).enumerate() {
            let mut v = vec![Null, P(edge + first)];
            if asm {
                v.push(I(*n));
            }
            v.push(P(point + i));
            out.push(("vertex", v));
        }
        for p in &self.pts {
            out.push(("point", vec![Null, T::Pos(*p)]));
        }
        out
    }

    /// SAT text, version 700 (AutoCAD 2000–2010), ending with `End-of-ACIS-data`.
    pub(crate) fn sat(&self) -> String {
        use std::fmt::Write as _;
        let mut s = format!("700 0 1 0 \n@{} {PRODUCT} @12 ACIS 32.0 NT @{} {DATE} \n1 9.9999999999999995e-007 1e-010 \n", PRODUCT.len(), DATE.len());
        for (name, values) in self.records_for(false) {
            s.push_str(name);
            s.push_str(" $-1 -1");
            for v in values {
                let _ = match v {
                    T::P(i) => write!(s, " ${i}"),
                    T::Null => write!(s, " $-1"),
                    T::I(i) => write!(s, " {i}"),
                    T::F(f) => write!(s, " {}", Real(f)),
                    T::Pos(p) | T::Dir(p) => write!(s, " {} {} {}", Real(p.x), Real(p.y), Real(p.z)),
                    T::B(b, t, f) => write!(s, " {}", if b { t } else { f }),
                    T::S(t) => write!(s, " @{} {t}", t.len()),
                };
            }
            s.push_str(" #\n");
        }
        s.push_str("End-of-ACIS-data \n");
        s
    }

    /// SAB binary, version 21800 (AutoCAD 2013 and later), ending with `End-of-ASM-data`.
    pub(crate) fn sab(&self) -> Vec<u8> {
        let mut b: Vec<u8> = b"ACIS BinaryFile".to_vec();
        // Version, records (0: not counted), entities (the asmheader and the body), flags.
        for n in [21800i32, 0, 2, 12] {
            b.extend(n.to_le_bytes());
        }
        let str_tag = |b: &mut Vec<u8>, tag: u8, s: &str| {
            b.push(tag);
            b.push(u8::try_from(s.len()).unwrap_or(u8::MAX));
            b.extend(s.bytes().take(255));
        };
        for s in [PRODUCT, "ACIS 218.00 NT", DATE] {
            str_tag(&mut b, 0x07, s);
        }
        // Millimetres per unit, absolute and normal resolution (the SAT header's 9.9999999999999995e-007).
        for f in [1.0f64, 1e-6, 1e-10] {
            b.push(0x06);
            b.extend(f.to_le_bytes());
        }
        let vec3 = |b: &mut Vec<u8>, tag: u8, p: Vec3| {
            b.push(tag);
            for c in [p.x, p.y, p.z] {
                b.extend(c.to_le_bytes());
            }
        };
        for (name, values) in self.records_for(true) {
            let mut parts = name.split('-').peekable();
            while let Some(part) = parts.next() {
                str_tag(&mut b, if parts.peek().is_some() { 0x0E } else { 0x0D }, part);
            }
            for v in std::iter::once(T::Null).chain(std::iter::once(T::I(-1))).chain(values) {
                match v {
                    T::P(i) => {
                        b.push(0x0C);
                        b.extend(i32::try_from(i).unwrap_or(-1).to_le_bytes());
                    }
                    T::Null => {
                        b.push(0x0C);
                        b.extend((-1i32).to_le_bytes());
                    }
                    T::I(i) => {
                        b.push(0x04);
                        b.extend(i32::try_from(i).unwrap_or(0).to_le_bytes());
                    }
                    T::F(f) => {
                        b.push(0x06);
                        b.extend(f.to_le_bytes());
                    }
                    T::Pos(p) => vec3(&mut b, 0x13, p),
                    T::Dir(p) => vec3(&mut b, 0x14, p),
                    // SAB writes every logical as a tag: 0x0A for the first word (true), 0x0B for
                    // the second.
                    T::B(t, _, _) => b.push(if t { 0x0A } else { 0x0B }),
                    T::S(s) => str_tag(&mut b, 0x07, s),
                }
            }
            b.push(0x11);
        }
        b.extend_from_slice(SAB_END);
        b
    }
}

#[cfg(test)]
pub(crate) mod tests;
