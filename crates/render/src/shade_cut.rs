//! RENDER's solids in an XCLIPped block reference: their triangles and edges cut by the clip
//! boundary's prism (its sides along the block's Z axis) and its front and back clipping planes
//! (Clipdepth), so what the clip hides doesn't render. The boundary is cut into triangles; each
//! triangle's prism is convex, so a face is cut by its planes (Sutherland–Hodgman) and an edge
//! by its plane crossings.

use cadcraft_color::Rgb;
use cadcraft_doc::xclip::XClip;
use cadcraft_geom::{Bounds2, Mat4, Vec3};

use crate::DisplayList;
use crate::shade::ShadeTri;

/// Points with `n · p + d >= 0` (block coordinates) are kept.
#[derive(Clone, Copy)]
struct Plane {
    n: Vec3,
    d: f64,
}

impl Plane {
    fn at(&self, p: Vec3) -> f64 {
        self.n.dot(p) + self.d
    }
}

/// One convex piece of the clip's prism: its planes and its boundary triangle's box.
struct Cell {
    planes: Vec<Plane>,
    bounds: Bounds2,
}

/// Most face-and-cell tests one cut makes; past it the remaining faces render whole.
// ponytail: every face is tested against every cell's box; bucket the cells if hostile
// many-sided boundaries over big meshes matter.
const MAX_TESTS: usize = 50_000_000;

/// The cells of `x`'s prism.
fn cells(x: &XClip) -> Vec<Cell> {
    let poly = x.polygon();
    let depth: Vec<Plane> =
        [x.front.map(|f| Plane { n: -Vec3::Z, d: f }), x.back.map(|b| Plane { n: Vec3::Z, d: -b })].into_iter().flatten().collect();
    let tris = crate::fill::triangulate_evenodd(std::slice::from_ref(&poly));
    let mut out = Vec::new();
    for t in tris.as_chunks::<3>().0 {
        let ccw = (t[1] - t[0]).cross(t[2] - t[0]);
        if !(ccw.is_finite() && ccw.abs() > 1e-300) {
            continue;
        }
        let mut planes = depth.clone();
        for (p, q) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            // Inward: the left of each edge of a counter-clockwise triangle.
            let e = (q - p) * ccw.signum();
            let n = Vec3::new(-e.y, e.x, 0.0);
            planes.push(Plane { n, d: -n.dot(p.to3(0.0)) });
        }
        out.push(Cell { planes, bounds: Bounds2::from_points(t.iter().copied()) });
    }
    out
}

/// A corner being cut: block and view coordinates and the view normal.
#[derive(Clone, Copy)]
struct V {
    b: Vec3,
    p: Vec3,
    n: Vec3,
}

fn lerp(a: V, c: V, t: f64) -> V {
    V { b: a.b + (c.b - a.b) * t, p: a.p + (c.p - a.p) * t, n: a.n + (c.n - a.n) * t }
}

/// Polygon `poly` cut by `planes`.
fn clip(mut poly: Vec<V>, planes: &[Plane]) -> Vec<V> {
    for pl in planes {
        let n = poly.len();
        let mut next = Vec::with_capacity(n + 2);
        for i in 0..n {
            let (Some(a), Some(c)) = (poly.get(i).copied(), poly.get((i + 1) % n).copied()) else { continue };
            let (da, dc) = (pl.at(a.b), pl.at(c.b));
            if da >= 0.0 {
                next.push(a);
            }
            if (da >= 0.0) != (dc >= 0.0) {
                next.push(lerp(a, c, da / (da - dc)));
            }
        }
        poly = next;
        if poly.len() < 3 {
            return Vec::new();
        }
    }
    poly
}

/// Cut what `list` renders (its faces and edges, in view coordinates) by clip `x` of the block
/// that `xf3` places (block → view coordinates).
pub(crate) fn cut(list: &mut DisplayList, xf3: &Mat4, x: &XClip) {
    if list.shade.is_empty() && list.shade_edges.is_empty() {
        return;
    }
    let Some(to_block) = xf3.inverse() else { return };
    let cells = cells(x);
    let mut tests = 0usize;
    let mut faces = Vec::with_capacity(list.shade.len());
    for t in &list.shade {
        let v = [0, 1, 2].map(|k| V { b: to_block.apply(t.p[k]), p: t.p[k], n: t.n[k] });
        tests = tests.saturating_add(cells.len());
        if tests > MAX_TESTS {
            faces.push(*t);
            continue;
        }
        let tb = Bounds2::from_points(v.iter().map(|c| c.b.xy()));
        for cell in cells.iter().filter(|c| c.bounds.intersects(&tb)) {
            let poly = clip(v.to_vec(), &cell.planes);
            if let [first, rest @ ..] = poly.as_slice() {
                for w in rest.windows(2) {
                    if let [a, c] = w {
                        let n = |c: &V| {
                            let m = c.n.normalized();
                            if m == Vec3::ZERO || !m.is_finite() { t.n[0] } else { m }
                        };
                        faces.push(ShadeTri { p: [first.p, a.p, c.p], n: [n(first), n(a), n(c)], ..*t });
                    }
                }
            }
        }
    }
    list.shade = faces;
    let edges = std::mem::take(&mut list.shade_edges);
    for (pts, color) in edges {
        cut_edge(&pts, color, &to_block, &cells, &mut list.shade_edges);
    }
}

/// The pieces of edge `pts` (view coordinates) inside the prism's `cells`, added to `out`.
fn cut_edge(pts: &[Vec3], color: Rgb, to_block: &Mat4, cells: &[Cell], out: &mut Vec<(Vec<Vec3>, Rgb)>) {
    let mut run: Vec<Vec3> = Vec::new();
    for w in pts.windows(2) {
        let [a, c] = w else { continue };
        let (ab, cb) = (to_block.apply(*a), to_block.apply(*c));
        let sb = Bounds2::from_points([ab.xy(), cb.xy()]);
        // The parameter ranges inside each cell (they touch at most where cells meet).
        let mut spans: Vec<(f64, f64)> = cells
            .iter()
            .filter(|cell| cell.bounds.intersects(&sb))
            .filter_map(|cell| {
                let (mut lo, mut hi) = (0.0f64, 1.0f64);
                for pl in &cell.planes {
                    let (da, dc) = (pl.at(ab), pl.at(cb));
                    if da < 0.0 && dc < 0.0 {
                        return None;
                    }
                    let t = da / (da - dc);
                    if da < 0.0 {
                        lo = lo.max(t);
                    } else if dc < 0.0 {
                        hi = hi.min(t);
                    }
                }
                (lo < hi).then_some((lo, hi))
            })
            .collect();
        spans.sort_by(|p, q| p.0.total_cmp(&q.0));
        // An edge along a cell's side lies in both cells.
        let mut merged: Vec<(f64, f64)> = Vec::with_capacity(spans.len());
        for (lo, hi) in spans {
            match merged.last_mut() {
                Some(m) if lo <= m.1 + 1e-12 => m.1 = m.1.max(hi),
                _ => merged.push((lo, hi)),
            }
        }
        let at = |t: f64| *a + (*c - *a) * t;
        for (lo, hi) in merged {
            let joins = run.last().is_some_and(|l| (*l - at(lo)).len() <= 1e-9 * (1.0 + l.len()));
            if !joins {
                if run.len() >= 2 {
                    out.push((std::mem::take(&mut run), color));
                }
                run = vec![at(lo)];
            }
            run.push(at(hi));
        }
        if run.last().is_some_and(|l| (*l - *c).len() > 1e-9 * (1.0 + l.len())) && run.len() >= 2 {
            out.push((std::mem::take(&mut run), color));
        }
    }
    if run.len() >= 2 {
        out.push((run, color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_doc::Handle;
    use cadcraft_geom::Vec2;

    fn tri(a: Vec3, b: Vec3, c: Vec3) -> ShadeTri {
        ShadeTri { p: [a, b, c], n: [Vec3::Z; 3], color: Rgb(1, 2, 3), handle: Handle(1) }
    }

    fn area(ts: &[ShadeTri]) -> f64 {
        ts.iter().map(|t| (t.p[1] - t.p[0]).cross(t.p[2] - t.p[0]).len() / 2.0).sum()
    }

    #[test]
    fn faces_and_edges_are_cut_at_the_prism_and_its_depth() {
        // A 10 x 10 floor and a 10 x 10 wall standing on its x = 10 side; the clip keeps x and y
        // from 2 to 6 and heights from -1 to 3.
        let mut list = DisplayList::default();
        let (o, x, y, z) = (Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), Vec3::new(0.0, 10.0, 0.0), Vec3::new(0.0, 0.0, 10.0));
        list.shade = vec![tri(o, x, x + y), tri(o, x + y, y), tri(x, x + y, x + y + z), tri(x, x + y + z, x + z)];
        list.shade_edges = vec![(vec![o, x, x + y], Rgb(0, 0, 0)), (vec![Vec3::new(4.0, 4.0, -5.0), Vec3::new(4.0, 4.0, 5.0)], Rgb(0, 0, 0))];
        let clip = XClip { boundary: vec![Vec2::new(2.0, 2.0), Vec2::new(6.0, 6.0)], on: true, front: Some(3.0), back: Some(-1.0) };
        // The block is placed 100 to the right in view coordinates.
        let xf3 = Mat4::translate(Vec3::new(100.0, 0.0, 0.0));
        let moved = |l: &mut DisplayList| {
            for t in &mut l.shade {
                t.p = t.p.map(|p| xf3.apply(p));
            }
            for (pts, _) in &mut l.shade_edges {
                *pts = pts.iter().map(|p| xf3.apply(*p)).collect();
            }
        };
        moved(&mut list);
        cut(&mut list, &xf3, &clip);
        // The floor keeps its 4 x 4 square; the wall at x = 10 lies outside the boundary.
        assert!((area(&list.shade) - 16.0).abs() < 1e-9, "{}", area(&list.shade));
        assert!(list.shade.iter().all(|t| t.p.iter().all(|p| (102.0 - 1e-9..=106.0 + 1e-9).contains(&p.x) && p.z.abs() < 1e-12)));
        assert!(list.shade.iter().all(|t| t.n == [Vec3::Z; 3] && t.color == Rgb(1, 2, 3)));
        // The floor's edges are outside; the vertical edge keeps heights -1 to 3.
        assert_eq!(list.shade_edges.len(), 1, "{:?}", list.shade_edges);
        let e = &list.shade_edges[0].0;
        assert!(e.len() == 2 && (e[0].z + 1.0).abs() < 1e-9 && (e[1].z - 3.0).abs() < 1e-9, "{e:?}");
    }

    #[test]
    fn a_polygon_boundary_keeps_only_its_inside() {
        // An L-shaped boundary over a 10 x 10 floor: 6 x 6 less its 3 x 3 upper-right corner.
        let mut list = DisplayList::default();
        let (o, x, y) = (Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), Vec3::new(0.0, 10.0, 0.0));
        list.shade = vec![tri(o, x, x + y), tri(o, x + y, y)];
        let l = [(0.0, 0.0), (6.0, 0.0), (6.0, 3.0), (3.0, 3.0), (3.0, 6.0), (0.0, 6.0)].map(|(a, b)| Vec2::new(a, b));
        cut(&mut list, &Mat4::IDENTITY, &XClip { boundary: l.to_vec(), on: true, front: None, back: None });
        assert!((area(&list.shade) - 27.0).abs() < 1e-9, "{}", area(&list.shade));
        // A singular placement leaves everything alone; a boundary with no area keeps nothing.
        let mut all = DisplayList { shade: vec![tri(o, x, y)], ..Default::default() };
        cut(&mut all, &Mat4::scale(Vec3::ZERO), &XClip { boundary: l.to_vec(), on: true, front: None, back: None });
        assert_eq!(all.shade.len(), 1);
        let flat = XClip { boundary: vec![Vec2::ZERO, Vec2::new(5.0, 0.0), Vec2::new(9.0, 0.0)], on: true, front: None, back: None };
        cut(&mut all, &Mat4::IDENTITY, &flat);
        assert!(all.shade.is_empty());
    }
}
