//! Hidden-line removal for VIEWBASE, on the solids' meshes.
//!
//! The lines of a view are the B-rep edges (each mesh's edge polylines) and the silhouettes of
//! curved faces (triangle edges between a front- and a back-facing triangle). A B-rep edge whose
//! faces meet smoothly (a seam, or a tangent edge where a fillet runs into a flat face) is drawn
//! only where it is a silhouette. Each line is sampled along its length: a sample is hidden when
//! a triangle of any solid covers it, strictly inside, and lies in front of it. Runs of samples
//! with one state become the visible and hidden lines; where the state changes the boundary is
//! found by bisection.
//!
//! ponytail: sampled visibility over a uniform grid of projected triangles, with every count, the
//! memory and the work capped per view (logged and reported: past a cap, triangles hide nothing,
//! a big solid shows no silhouettes, lines are drawn visible); exact edge-against-triangle
//! clipping (or a BVH) when drawings need sub-sample accuracy or bigger models. Curved edges come out as the mesh's polylines;
//! fitting arcs and circles back when drawings need true arcs (radius dimensions, CNC export).

use std::collections::{HashMap, HashSet};

use cadcraft_doc::Solid3d;
use cadcraft_geom::{Bounds2, Vec2, Vec3, ViewDir};

/// Most triangles one view takes as occluders (about 100 bytes each).
const MAX_TRIS: usize = 1_000_000;
/// Most triangles of one solid whose edges are matched up for silhouettes and smooth edges (the
/// map takes about 200 bytes a triangle); a bigger solid shows its B-rep edges only.
const MAX_SOLID_TRIS: usize = 250_000;
/// Most triangles one view matches up across all its solids; past it the rest of the solids
/// show their B-rep edges only.
const MAX_MATCHED_TRIS: usize = 1_000_000;
/// Most point-in-triangle tests one view makes (samples times the triangles in their grid cells,
/// bisection included); past it the rest of the lines are drawn visible.
const MAX_TESTS: usize = 100_000_000;
/// Most line pieces one view draws.
const MAX_SEGS: usize = 2_000_000;
/// Most visibility samples one view takes; past it the samples spread out.
const MAX_SAMPLES: usize = 4_000_000;
/// Most (cell, triangle) entries in the occluder grid.
const MAX_CELL_ENTRIES: usize = 16_000_000;
/// Samples along the view's size, when the caps allow.
const SAMPLES_ACROSS: f64 = 400.0;
/// Faces meeting at an edge with facet normals closer than this (cos 40°) meet smoothly:
/// OpenCASCADE meshes curved faces with facets up to about 29° apart.
const SMOOTH_COS: f64 = 0.766;
/// Bisection steps that place a visible/hidden boundary between two samples.
const BISECT: usize = 12;

/// A view's lines in view-plane coordinates (x right, y up), and what was capped.
#[derive(Debug, Default)]
pub(crate) struct Lines {
    pub visible: Vec<Vec<Vec2>>,
    pub hidden: Vec<Vec<Vec2>>,
    pub capped: Vec<String>,
}

/// A projected triangle with the depth (toward the viewer) of each corner.
struct Tri {
    p: [Vec2; 3],
    d: [f64; 3],
    lo: Vec2,
    hi: Vec2,
}

type Key = (i64, i64, i64);

/// `v` as a whole number when it is one within range (quantised coordinates).
fn whole(v: f64) -> Option<i64> {
    (v.is_finite() && v.abs() < 9.0e15).then(|| v.round() as i64)
}

fn whole_usize(v: f64) -> Option<usize> {
    (0.0..1.0e15).contains(&v).then_some(v as usize)
}

/// The projected bounds of the solids' meshes (empty when they have no finite points).
pub(crate) fn extent(solids: &[&Solid3d], view: ViewDir) -> Bounds2 {
    Bounds2::from_points(solids.iter().flat_map(|x| x.mesh.points().map(|p| view.project(x.world(p)))).filter(|p| p.is_finite()))
}

/// A solid's mesh points (vertices and edge points) in the world.
fn world_pts(x: &Solid3d) -> impl Iterator<Item = Vec3> + '_ {
    x.mesh.verts.iter().chain(x.mesh.edges.iter().flatten()).map(move |p| x.world(*p))
}

/// The visible and hidden lines of `solids` seen along `view`.
pub(crate) fn lines(solids: &[&Solid3d], view: ViewDir) -> Lines {
    let mut out = Lines::default();
    let (_, _, toward) = view.axes();
    // World points are made as they are needed (no copies of the meshes); a solid with a
    // non-finite point is left out.
    let (mut lo, mut hi, mut reach) = (Vec3::new(f64::MAX, f64::MAX, f64::MAX), Vec3::new(f64::MIN, f64::MIN, f64::MIN), 0.0_f64);
    let mut kept: Vec<&Solid3d> = Vec::with_capacity(solids.len());
    for x in solids {
        if !world_pts(x).all(|p| p.is_finite()) {
            out.capped.push("a solid with non-finite points was left out".into());
            continue;
        }
        kept.push(x);
        for p in world_pts(x) {
            lo = Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
            hi = Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
            reach = reach.max(p.x.abs()).max(p.y.abs()).max(p.z.abs());
        }
    }
    let size = (hi - lo).len();
    if !size.is_finite() || size <= 0.0 {
        return out;
    }
    // Points closer than `q` are one point (as the kernel joins mesh pieces).
    let q = (size * 1e-9).max(reach * 1e-15);
    let key = |p: Vec3| -> Option<Key> { Some((whole(p.x / q)?, whole(p.y / q)?, whole(p.z / q)?)) };
    let front = |n: Vec3| n.dot(toward) > 1e-9 * n.len();

    let mut segs: Vec<(Vec3, Vec3)> = Vec::new();
    let mut dropped = 0usize;
    let mut tris: Vec<Tri> = Vec::new();
    let mut budget = MAX_MATCHED_TRIS;
    for x in kept {
        let corner = |i: u32| x.mesh.verts.get(i as usize).map(|p| x.world(*p));
        // Facet normals, and the facets on each edge (by position: faces don't share vertices).
        let n = x.mesh.tris.len();
        let matched = n <= MAX_SOLID_TRIS && n <= budget;
        if matched {
            budget -= n;
        } else if n > MAX_SOLID_TRIS {
            out.capped.push(format!("a solid of {n} triangles (past {MAX_SOLID_TRIS}) shows its edges only, no silhouettes"));
        } else {
            out.capped.push(format!("past {MAX_MATCHED_TRIS} triangles matched in this view, a solid of {n} shows its edges only, no silhouettes"));
        }
        let mut normals: Vec<Vec3> = Vec::with_capacity(if matched { x.mesh.tris.len() } else { 0 });
        let mut on_edge: HashMap<(Key, Key), (Vec3, Vec3, Vec<usize>)> = HashMap::new();
        for t in &x.mesh.tris {
            let (Some(a), Some(b), Some(c)) = (corner(t[0]), corner(t[1]), corner(t[2])) else { continue };
            if matched {
                let i = normals.len();
                normals.push((b - a).cross(c - a));
                for (p, r) in [(a, b), (b, c), (c, a)] {
                    let (Some(kp), Some(kr)) = (key(p), key(r)) else { continue };
                    if kp != kr {
                        let k = if kp < kr { (kp, kr) } else { (kr, kp) };
                        on_edge.entry(k).or_insert_with(|| (p, r, Vec::new())).2.push(i);
                    }
                }
            }
            if tris.len() < MAX_TRIS {
                let p = [view.project(a), view.project(b), view.project(c)];
                let area = (p[1] - p[0]).cross(p[2] - p[0]);
                // Edge-on triangles cover nothing.
                if area.abs() > 1e-12 * ((p[1] - p[0]).len2() + (p[2] - p[0]).len2()) {
                    let lo = p[0].min(p[1]).min(p[2]);
                    let hi = p[0].max(p[1]).max(p[2]);
                    tris.push(Tri { p, d: [view.depth(a), view.depth(b), view.depth(c)], lo, hi });
                }
            } else if !out.capped.iter().any(|c| c.starts_with("triangles")) {
                out.capped.push(format!("triangles past {MAX_TRIS} hide nothing"));
            }
        }
        let unit = |i: usize| normals.get(i).map(|n| n.normalized());
        let silhouette = |fs: &[usize]| match fs {
            [i, j] => normals.get(*i).zip(normals.get(*j)).is_some_and(|(a, b)| front(*a) != front(*b)),
            _ => false,
        };
        let smooth = |fs: &[usize]| match fs {
            [i, j] => unit(*i).zip(unit(*j)).is_some_and(|(a, b)| a.dot(b) > SMOOTH_COS),
            _ => false,
        };
        let mut brep: HashSet<(Key, Key)> = HashSet::new();
        let mut keep = |a: Vec3, b: Vec3| {
            if segs.len() < MAX_SEGS {
                segs.push((a, b));
            } else {
                dropped += 1;
            }
        };
        for e in &x.mesh.edges {
            for w in e.windows(2) {
                let (Some(a), Some(b)) = (w.first().map(|p| x.world(*p)), w.get(1).map(|p| x.world(*p))) else { continue };
                let k = key(a).zip(key(b)).map(|(ka, kb)| if ka < kb { (ka, kb) } else { (kb, ka) });
                let faces = k.and_then(|k| on_edge.get(&k)).map(|f| f.2.as_slice()).unwrap_or(&[]);
                if !smooth(faces) || silhouette(faces) {
                    keep(a, b);
                }
                brep.extend(k);
            }
        }
        let mut sil: Vec<(&(Key, Key), Vec3, Vec3)> =
            on_edge.iter().filter(|(k, f)| !brep.contains(k) && silhouette(&f.2)).map(|(k, f)| (k, f.0, f.1)).collect();
        // In a fixed order, so the same model always draws the same.
        sil.sort_unstable_by_key(|x| x.0);
        sil.into_iter().for_each(|(_, a, b)| keep(a, b));
    }
    if dropped > 0 {
        out.capped.push(format!("{dropped} line pieces past {MAX_SEGS} were left out"));
    }

    let ext = extent(solids, view);
    let span = ext.width().max(ext.height());
    if !span.is_finite() || span <= 0.0 {
        return out;
    }
    let grid = Grid::new(&tris, ext, &mut out.capped);
    let dtol = size * 1e-6;
    let tests = std::cell::Cell::new(0usize);
    let hidden_at = |p: Vec3| grid.covers(&tris, view.project(p), view.depth(p) + dtol, &tests);

    // Spread the samples when the lines are too long for the cap.
    let tiny = span * 1e-9;
    let total: f64 = segs.iter().map(|(a, b)| view.project(*a).dist(view.project(*b))).sum();
    let mut step = span / SAMPLES_ACROSS;
    let wanted = total / step + segs.len() as f64;
    if wanted > MAX_SAMPLES as f64 {
        step *= wanted / MAX_SAMPLES as f64;
        out.capped.push(format!("samples spread out to every {step:.3} drawing units (about {wanted:.0} wanted)"));
    }

    let mut pieces: Vec<([Vec2; 2], bool)> = Vec::new();
    for (a, b) in segs {
        let (a2, b2) = (view.project(a), view.project(b));
        let len = a2.dist(b2);
        if len <= tiny {
            continue;
        }
        let n = whole_usize((len / step).ceil()).unwrap_or(1).clamp(1, MAX_SAMPLES);
        let at = |t: f64| a + (b - a) * t;
        let mid = |i: usize| (i as f64 + 0.5) / n as f64;
        let mut runs: Vec<(f64, f64, bool)> = Vec::new();
        let (mut start, mut state) = (0.0, hidden_at(at(mid(0))));
        for i in 1..n {
            let h = hidden_at(at(mid(i)));
            if h != state {
                let (mut l, mut r) = (mid(i - 1), mid(i));
                for _ in 0..BISECT {
                    let m = (l + r) / 2.0;
                    if hidden_at(at(m)) == state {
                        l = m;
                    } else {
                        r = m;
                    }
                }
                let cut = (l + r) / 2.0;
                runs.push((start, cut, state));
                (start, state) = (cut, h);
            }
        }
        runs.push((start, 1.0, state));
        pieces.extend(runs.into_iter().map(|(t0, t1, hidden)| ([a2.lerp(b2, t0), a2.lerp(b2, t1)], hidden)));
    }
    if tests.get() >= MAX_TESTS {
        out.capped.push(format!("hidden-line tests past {MAX_TESTS}: the rest of the lines are drawn visible"));
    }
    let q2 = span * 1e-9;
    let (vis, hid) = overlaps(pieces, q2);
    let qk = |p: Vec2| Some((whole(p.x / q2)?, whole(p.y / q2)?));
    out.visible = join(vis, qk);
    out.hidden = join(hid, qk);
    out
}

/// Pieces on one line merged: overlapping visible pieces become one, and hidden ones lose the
/// parts a visible one covers (a back edge seen behind a front one). The visible pieces, then the
/// hidden ones.
fn overlaps(pieces: Vec<([Vec2; 2], bool)>, q: f64) -> (Vec<[Vec2; 2]>, Vec<[Vec2; 2]>) {
    // Lines by direction (angle in (-90°, 90°]) and offset from the origin, in the order met.
    let mut index: HashMap<(i64, i64), usize> = HashMap::new();
    let mut lines: Vec<(Vec2, f64, Vec<(f64, f64)>, Vec<(f64, f64)>)> = Vec::new();
    for ([a, b], hidden) in pieces {
        let len = a.dist(b);
        if len.is_nan() || len <= q {
            continue;
        }
        let d = (b - a) * (1.0 / len);
        let d = if d.x < 0.0 || (d.x == 0.0 && d.y < 0.0) { d * -1.0 } else { d };
        let off = d.perp().dot(a);
        let Some(k) = whole(d.y.atan2(d.x) * 1e9).zip(whole(off / q)) else { continue };
        let i = *index.entry(k).or_insert_with(|| {
            lines.push((d, off, Vec::new(), Vec::new()));
            lines.len() - 1
        });
        let (t0, t1) = (d.dot(a), d.dot(b));
        if let Some(l) = lines.get_mut(i) {
            let span = (t0.min(t1), t0.max(t1));
            if hidden { l.3.push(span) } else { l.2.push(span) }
        }
    }
    let merge = |mut v: Vec<(f64, f64)>| {
        v.sort_by(|x, y| x.0.total_cmp(&y.0));
        let mut out: Vec<(f64, f64)> = Vec::with_capacity(v.len());
        for (s, e) in v {
            match out.last_mut() {
                Some(l) if s <= l.1 + q => l.1 = l.1.max(e),
                _ => out.push((s, e)),
            }
        }
        out
    };
    let (mut vis, mut hid) = (Vec::new(), Vec::new());
    for (d, off, v, h) in lines {
        let at = |t: f64| d.perp() * off + d * t;
        let v = merge(v);
        for (s, e) in &v {
            vis.push([at(*s), at(*e)]);
        }
        for (mut s, e) in merge(h) {
            // What the visible spans leave of it.
            for &(vs, ve) in &v {
                if ve <= s || vs >= e {
                    continue;
                }
                if vs > s + q {
                    hid.push([at(s), at(vs)]);
                }
                s = s.max(ve);
            }
            if e > s + q {
                hid.push([at(s), at(e)]);
            }
        }
    }
    (vis, hid)
}

/// A uniform grid over the view of the triangles each cell's area meets.
struct Grid {
    lo: Vec2,
    cell: f64,
    n: usize,
    cells: Vec<Vec<u32>>,
}

impl Grid {
    fn new(tris: &[Tri], ext: Bounds2, capped: &mut Vec<String>) -> Grid {
        let n = whole_usize((tris.len() as f64).sqrt().ceil()).unwrap_or(1).clamp(1, 512);
        let cell = (ext.width().max(ext.height()) / n as f64).max(f64::MIN_POSITIVE);
        let mut g = Grid { lo: ext.min, cell, n, cells: vec![Vec::new(); n * n] };
        let mut entries = 0usize;
        for (i, t) in tris.iter().enumerate() {
            let (Some((x0, y0)), Some((x1, y1)), Ok(i)) = (g.at(t.lo), g.at(t.hi), u32::try_from(i)) else { continue };
            let count = (x1 - x0 + 1) * (y1 - y0 + 1);
            if entries + count > MAX_CELL_ENTRIES {
                capped.push(format!("the occluder grid is full ({MAX_CELL_ENTRIES} entries): later triangles hide nothing"));
                break;
            }
            entries += count;
            for y in y0..=y1 {
                for x in x0..=x1 {
                    if let Some(c) = g.cells.get_mut(y * n + x) {
                        c.push(i);
                    }
                }
            }
        }
        g
    }

    /// The cell holding `p` (clamped to the grid).
    fn at(&self, p: Vec2) -> Option<(usize, usize)> {
        let c = |v: f64, o: f64| whole_usize(((v - o) / self.cell).floor().max(0.0)).map(|i| i.min(self.n - 1));
        Some((c(p.x, self.lo.x)?, c(p.y, self.lo.y)?))
    }

    /// Whether a triangle covers `p` strictly inside and is nearer the viewer than `depth`; never
    /// once `tests` (the triangles tried so far) reaches [`MAX_TESTS`].
    fn covers(&self, tris: &[Tri], p: Vec2, depth: f64, tests: &std::cell::Cell<usize>) -> bool {
        let Some((x, y)) = self.at(p) else { return false };
        let Some(cell) = self.cells.get(y * self.n + x) else { return false };
        if tests.get() >= MAX_TESTS {
            return false;
        }
        tests.set(tests.get().saturating_add(cell.len()));
        cell.iter().filter_map(|i| tris.get(*i as usize)).any(|t| {
            if p.x <= t.lo.x || p.x >= t.hi.x || p.y <= t.lo.y || p.y >= t.hi.y {
                return false;
            }
            let [a, b, c] = t.p;
            let area = (b - a).cross(c - a);
            let (u, v) = ((c - b).cross(p - b) / area, (a - c).cross(p - c) / area);
            let w = 1.0 - u - v;
            const IN: f64 = 1e-9;
            u > IN && v > IN && w > IN && u * t.d[0] + v * t.d[1] + w * t.d[2] > depth
        })
    }
}

/// Pieces joined into polylines where exactly two meet end to end.
fn join<K: std::hash::Hash + Eq + Copy>(pieces: Vec<[Vec2; 2]>, key: impl Fn(Vec2) -> Option<K>) -> Vec<Vec<Vec2>> {
    let mut at: HashMap<K, Vec<usize>> = HashMap::new();
    for (i, [a, b]) in pieces.iter().enumerate() {
        for k in [key(*a), key(*b)].into_iter().flatten() {
            at.entry(k).or_default().push(i);
        }
    }
    let mut used = vec![false; pieces.len()];
    let mut out = Vec::new();
    for (i, [a, b]) in pieces.iter().enumerate() {
        if used.get(i).copied().unwrap_or(true) {
            continue;
        }
        if let Some(u) = used.get_mut(i) {
            *u = true;
        }
        let mut line = std::collections::VecDeque::from([*a, *b]);
        for forward in [true, false] {
            loop {
                let end = if forward { line.back() } else { line.front() };
                let Some(k) = end.and_then(|p| key(*p)) else { break };
                let Some(next) = at.get(&k).filter(|v| v.len() == 2).and_then(|v| v.iter().copied().find(|j| !used.get(*j).copied().unwrap_or(true)))
                else {
                    break;
                };
                if let Some(u) = used.get_mut(next) {
                    *u = true;
                }
                let Some([p, r]) = pieces.get(next).copied() else { break };
                let far = if key(p) == Some(k) { r } else { p };
                if forward { line.push_back(far) } else { line.push_front(far) }
            }
        }
        out.push(line.into_iter().collect());
    }
    out
}

#[cfg(test)]
pub(super) mod tests {
    use std::sync::Arc;

    use cadcraft_geom::{Mat4, Mesh3};

    use super::*;

    /// An axis-aligned box mesh, one face's vertices apart from the others' (as the kernel makes
    /// them), with its twelve edges.
    pub(crate) fn cube(lo: Vec3, hi: Vec3) -> Solid3d {
        let c = |i: u32| Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
        // Outward faces as corner indices, counterclockwise from outside.
        let faces = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
        let mut m = Mesh3::default();
        for f in faces {
            let base = m.verts.len() as u32;
            m.verts.extend(f.iter().map(|i| c(*i)));
            m.tris.push([base, base + 1, base + 2]);
            m.tris.push([base, base + 2, base + 3]);
        }
        for (a, b) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
            m.edges.push(vec![c(a), c(b)]);
        }
        Solid3d { mesh: Arc::new(m), xform: Mat4::IDENTITY, ..Default::default() }
    }

    fn total(ls: &[Vec<Vec2>]) -> f64 {
        ls.iter().flat_map(|l| l.windows(2)).map(|w| w[0].dist(w[1])).sum()
    }

    #[test]
    fn a_box_behind_another_shows_hidden_lines() {
        let front_view = ViewDir::new(Vec3::new(0.0, -1.0, 0.0));
        let big = cube(Vec3::ZERO, Vec3::new(10.0, 1.0, 10.0));
        // Smaller, behind the big one: all of it hidden.
        let small = cube(Vec3::new(2.0, 5.0, 2.0), Vec3::new(4.0, 6.0, 4.0));
        let l = lines(&[&big, &small], front_view);
        assert!(l.capped.is_empty(), "{:?}", l.capped);
        // The big box's outline (its back edges coincide with it): 40 units of visible line.
        assert!((total(&l.visible) - 40.0).abs() < 1e-6, "{}", total(&l.visible));
        // The small square, hidden.
        assert!((total(&l.hidden) - 8.0).abs() < 1e-6, "{}", total(&l.hidden));
        // Joined: the outline is one closed polyline.
        assert_eq!(l.visible.len(), 1, "{:?}", l.visible);
    }

    /// Past the per-solid cap no edge map is made: the B-rep edges are drawn, no silhouettes.
    #[test]
    fn a_huge_solid_shows_its_edges_only() {
        let mut big = cube(Vec3::ZERO, Vec3::new(10.0, 10.0, 10.0));
        let mut m = (*big.mesh).clone();
        let first = m.tris[0];
        m.tris.resize(MAX_SOLID_TRIS + 1, first);
        big.mesh = Arc::new(m);
        let l = lines(&[&big], ViewDir::PLAN);
        assert!(l.capped.iter().any(|c| c.contains("edges only")), "{:?}", l.capped);
        assert!((total(&l.visible) - 40.0).abs() < 1e-6, "the top square: {}", total(&l.visible));
    }

    /// The triangles matched up for silhouettes are capped per view, across solids: past the
    /// budget a solid shows its B-rep edges only.
    #[test]
    fn matching_is_capped_per_view() {
        // Solids at the per-solid cap, of zero-area triangles (cheap to match, hide nothing).
        let flat = |x: f64| {
            let mut b = cube(Vec3::new(x, 0.0, 0.0), Vec3::new(x + 1.0, 1.0, 1.0));
            let mut m = (*b.mesh).clone();
            m.tris = vec![[0, 0, 0]; MAX_SOLID_TRIS];
            b.mesh = Arc::new(m);
            b
        };
        let full: Vec<Solid3d> = (0..MAX_MATCHED_TRIS / MAX_SOLID_TRIS).map(|i| flat(2.0 * i as f64)).collect();
        let last = cube(Vec3::new(100.0, 0.0, 0.0), Vec3::new(101.0, 1.0, 1.0));
        let mut all: Vec<&Solid3d> = full.iter().collect();
        assert!(lines(&all, ViewDir::PLAN).capped.is_empty(), "within the budget");
        all.push(&last);
        let l = lines(&all, ViewDir::PLAN);
        assert!(l.capped.iter().any(|c| c.contains("matched in this view")), "{:?}", l.capped);
        // Its edges are still drawn.
        assert!(l.visible.iter().flatten().any(|p| p.x > 99.0), "{:?}", l.visible.len());
    }

    /// Once the test budget is spent nothing hides anything (and nothing more is tested).
    #[test]
    fn hiding_stops_at_the_test_budget() {
        let p = [Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(0.0, 10.0)];
        let tris = [Tri { p, d: [5.0; 3], lo: Vec2::ZERO, hi: Vec2::new(10.0, 10.0) }];
        let grid = Grid::new(&tris, Bounds2 { min: Vec2::ZERO, max: Vec2::new(10.0, 10.0) }, &mut Vec::new());
        let tests = std::cell::Cell::new(0);
        assert!(grid.covers(&tris, Vec2::new(1.0, 1.0), 0.0, &tests));
        assert_eq!(tests.get(), 1);
        tests.set(MAX_TESTS);
        assert!(!grid.covers(&tris, Vec2::new(1.0, 1.0), 0.0, &tests));
        assert_eq!(tests.get(), MAX_TESTS);
    }

    #[test]
    fn partly_hidden_lines_split_where_the_cover_ends() {
        let top = ViewDir::PLAN;
        // A plate at z 5..6 covering x < 5 of a lower box's top edges.
        let plate = cube(Vec3::new(-1.0, -1.0, 5.0), Vec3::new(5.0, 11.0, 6.0));
        let low = cube(Vec3::ZERO, Vec3::new(10.0, 10.0, 1.0));
        let l = lines(&[&plate, &low], top);
        // The low box's edges along x, at y = 0 and y = 10, are hidden from x 0 to 5.
        let hidden = total(&l.hidden);
        assert!((hidden - (5.0 + 5.0 + 10.0)).abs() < 0.01, "{hidden} {:?}", l.hidden);
        let ends: Vec<f64> = l.hidden.iter().flat_map(|p| p.iter().map(|q| q.x)).collect();
        assert!(ends.iter().any(|x| (x - 5.0).abs() < 1e-3), "{ends:?}");
    }
}
