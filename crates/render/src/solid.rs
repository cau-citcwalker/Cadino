//! 3D solids: their edges and silhouettes (wireframe), or their faces shaded and depth-sorted,
//! projected by the view direction of [`crate::Options`]. Shaded model space also records each
//! vertex's depth, which faces they are and the edges to draw over them (see
//! [`crate::DisplayList::faces`]), for a depth-buffered canvas.
//!
//! A solid in a block reference is placed in the world as EXPLODE places it: the references'
//! transform lifted to 3D with z scaled by the square root of its determinant (see
//! [`Solid3d::transform`]), in the reference's own plane. Culling, depth and shading use those
//! points in view space (`Ctx::xf3`).

use cadcraft_color::Rgb;
use cadcraft_doc::Solid3d;
use cadcraft_geom::{Mat3, Mat4, Vec2, Vec3};

use crate::{Builder, Ctx, DPrim, Ink, Kind};

/// Shade steps of a shaded face (runs of equal shade share one primitive): finer than the
/// shade change between neighbouring facets of a curved face, so curved faces don't band.
const LEVELS: f64 = 64.0;

/// A triangle faces the viewer when its unit normal points toward the viewer by more than this;
/// an edge-on triangle (a rounding error either way) faces away.
const FRONT: f64 = 1e-9;

/// Shade of the edges drawn over shaded faces: dark, so they show on the brightest face.
const EDGE_LEVEL: f64 = 0.25;

pub(crate) fn draw(b: &mut Builder, ctx: &Ctx, s: &Solid3d, color: Ink, lw: f32) {
    // Everything happens in view space (`ctx.xf3`: x right, y up, z toward the viewer), where the
    // view direction is +Z. Block references place solids with their plane, elevation, z scale
    // and base point (`Insert::lift`).
    let (right, up, toward) = (Vec3::X, Vec3::Y, Vec3::Z);
    let proj = |w: Vec3| Vec2::new(w.x, w.y);
    let xf = ctx.xf3.mul(&s.xform);
    // A mirroring transform turns the outward winding inward.
    let out = if det3(&xf) < 0.0 { -1.0 } else { 1.0 };
    let world: Vec<Vec3> = s.mesh.verts.iter().map(|p| xf.apply(*p)).collect();
    // The whole mesh counts toward the list's bounds, whatever is drawn of it.
    for p in &world {
        b.list.bounds.add(proj(*p));
    }
    // A triangle's world corners and outward unit normal; `None` for a bad index or no area.
    let face = |t: &[u32; 3]| {
        let w = |i: u32| world.get(i as usize).copied();
        let (p, q, r) = (w(t[0])?, w(t[1])?, w(t[2])?);
        let n = (q - p).cross(r - p) * out;
        let l = n.len();
        (l > 0.0 && l.is_finite()).then(|| ([p, q, r], n * (1.0 / l)))
    };
    if !b.opts.shaded {
        for e in &s.mesh.edges {
            let w: Vec<Vec3> = e.iter().map(|p| xf.apply(*p)).collect();
            let (pts, z): (Vec<Vec2>, Vec<f64>) = w.iter().map(|p| (proj(*p), toward.dot(*p))).unzip();
            b.polyline_at(ctx, color, lw, &pts, Mat3::IDENTITY, &z);
        }
        // Silhouettes: triangle edges between a triangle facing the viewer and one facing away.
        // Only edges inside a face's triangulation (shared vertex indices) are looked at: face
        // boundaries and seams are edges already drawn above.
        let mut sides: Vec<(u32, u32, bool)> = Vec::new();
        for t in &s.mesh.tris {
            let Some((_, n)) = face(t) else { continue };
            let front = n.dot(toward) > FRONT;
            for (i, j) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                sides.push((i.min(j), i.max(j), front));
            }
        }
        sides.sort_unstable();
        for run in sides.chunk_by(|x, y| (x.0, x.1) == (y.0, y.1)) {
            // Sorted, so an edge with both sides starts facing away and ends facing the viewer.
            if let (Some(&(i, j, false)), Some((_, _, true))) = (run.first(), run.last())
                && let (Some(p), Some(q)) = (world.get(i as usize), world.get(j as usize))
            {
                b.polyline_at(ctx, color, lw, &[proj(*p), proj(*q)], Mat3::IDENTITY, &[toward.dot(*p), toward.dot(*q)]);
            }
        }
        return;
    }
    // Light from above and to the left of the viewer, so faces at the same angle to the view
    // (the three faces of a box in an isometric view) shade apart.
    let light = (toward + up * 0.5 - right * 0.3).normalized();
    // ponytail: painter's sort per solid, which is all the CPU raster has: interpenetrating
    // solids can overlap wrongly there, and lines draw over or under faces by draw order. The
    // GPU canvas depth-tests with the depths recorded here instead.
    let mut faces: Vec<(f64, f64, [Vec3; 3])> = Vec::with_capacity(s.mesh.tris.len());
    for t in &s.mesh.tris {
        let Some(([p, q, r], n)) = face(t) else { continue };
        // Back faces are culled; a surface shows both sides of its faces, the back lit as the front.
        let facing = n.dot(toward);
        let n = if facing > FRONT {
            n
        } else if s.mesh.open && facing < -FRONT {
            -n
        } else {
            continue;
        };
        let depth = toward.dot(p) + toward.dot(q) + toward.dot(r);
        let level = ((0.3 + 0.7 * n.dot(light).max(0.0)) * LEVELS).round() / LEVELS;
        faces.push((depth, level, [p, q, r]));
    }
    faces.sort_by(|a, b| a.0.total_cmp(&b.0));
    let base = if color.aci7 { Rgb(205, 205, 205) } else { color.rgb };
    let first = b.list.prims.len();
    let (mut run, mut z): (Vec<Vec2>, Vec<f64>) = (Vec::new(), Vec::new());
    let mut level = f64::NAN;
    for (_, l, tri) in faces {
        if l != level && !run.is_empty() {
            b.tris_at(ctx, shade(base, color, level), &run, Mat3::IDENTITY, &z);
            run.clear();
            z.clear();
        }
        level = l;
        run.extend(tri.map(proj));
        z.extend(tri.map(|w| toward.dot(w)));
    }
    if !run.is_empty() {
        b.tris_at(ctx, shade(base, color, level), &run, Mat3::IDENTITY, &z);
    }
    if b.depth.is_none() {
        return;
    }
    if let (Ok(from), Ok(to)) = (u32::try_from(first), u32::try_from(b.list.prims.len()))
        && from < to
    {
        b.list.faces.push(from..to);
    }
    // Plain Shaded draws no edges; Shaded with Edges ([`crate::Options::edges`]) does.
    if !b.opts.edges {
        return;
    }
    let ink = shade(base, color, EDGE_LEVEL);
    for e in &s.mesh.edges {
        let (pts, z): (Vec<Vec2>, Vec<f64>) = e.iter().map(|p| xf.apply(*p)).map(|w| (proj(w), toward.dot(w))).unzip();
        edge(b, ctx, ink, &pts, &z);
    }
}

/// Add an edge over shaded faces ([`crate::DisplayList::edges`]): points `pts` (view plane) at
/// depths `z`.
fn edge(b: &mut Builder, ctx: &Ctx, color: Ink, pts: &[Vec2], z: &[f64]) {
    let (Ok(start), Ok(len)) = (u32::try_from(b.list.verts.len()), u32::try_from(pts.len())) else { return };
    if len < 2 || z.len() != pts.len() {
        return;
    }
    b.list.verts.extend_from_slice(pts);
    b.list.vert_depth.extend_from_slice(z);
    let prim = DPrim { handle: ctx.top, color: color.rgb, aci7: color.aci7, lw: 0.0, transparency: b.transparency, kind: Kind::Polyline, start, len };
    b.list.edges.push(prim);
}

/// The determinant of a transform's linear (3x3) part.
fn det3(m: &Mat4) -> f64 {
    let m = &m.m;
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

fn shade(base: Rgb, color: Ink, level: f64) -> Ink {
    let f = |c: u8| (f64::from(c) * level).round().clamp(0.0, 255.0) as u8;
    Ink { rgb: Rgb(f(base.0), f(base.1), f(base.2)), aci7: false, aci: color.aci }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cadcraft_doc::{Common, Drawing, EntityKind, Space};
    use cadcraft_geom::{Mesh3, ViewDir};

    use super::*;
    use crate::{Kind, Options, build};

    fn solid(verts: Vec<Vec3>, tris: Vec<[u32; 3]>, edges: Vec<Vec<Vec3>>) -> Solid3d {
        Solid3d { brep: Arc::from(""), mesh: Arc::new(Mesh3 { verts, tris, edges, open: false }), ..Default::default() }
    }

    /// A unit cube's 12 edges and 12 outward triangles, each face with its own corners as the
    /// kernel tessellates it.
    fn cube() -> Solid3d {
        let v = |i: u32| Vec3::new(f64::from(i & 1), f64::from((i >> 1) & 1), f64::from((i >> 2) & 1));
        let corners =
            [[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
        let verts = corners.iter().flatten().map(|&i| v(i)).collect();
        let tris = (0..12).map(|t| [3 * t, 3 * t + 1, 3 * t + 2]).collect();
        let e = |a: u32, b: u32| vec![v(a), v(b)];
        let edges = vec![e(0, 1), e(1, 3), e(3, 2), e(2, 0), e(4, 5), e(5, 7), e(7, 6), e(6, 4), e(0, 4), e(1, 5), e(2, 6), e(3, 7)];
        solid(verts, tris, edges)
    }

    /// A square pyramid on the unit square, apex up: its base (2 triangles) faces down, its 4
    /// sides up.
    fn pyramid() -> Solid3d {
        let v = Vec3::new;
        let verts = vec![v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), v(1.0, 1.0, 0.0), v(0.5, 0.5, 1.0)];
        solid(verts, vec![[0, 2, 1], [1, 2, 3], [0, 1, 4], [1, 3, 4], [3, 2, 4], [2, 0, 4]], vec![])
    }

    /// A unit sphere tessellated in latitude rings, with a seam meridian as its only edge (as
    /// the kernel gives a sphere).
    fn sphere() -> Solid3d {
        let (rings, segs) = (8u32, 16u32);
        let mut verts = Vec::new();
        for i in 0..=rings {
            let lat = -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * f64::from(i) / f64::from(rings);
            for j in 0..=segs {
                let lon = std::f64::consts::TAU * f64::from(j) / f64::from(segs);
                verts.push(Vec3::new(lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()));
            }
        }
        let mut tris = Vec::new();
        for i in 0..rings {
            for j in 0..segs {
                let a = i * (segs + 1) + j;
                let c = a + segs + 1;
                tris.extend([[a, a + 1, c + 1], [a, c + 1, c]]);
            }
        }
        let seam = (0..=rings).filter_map(|i| verts.get((i * (segs + 1)) as usize).copied()).collect();
        solid(verts, tris, vec![seam])
    }

    fn drawing_of(s: Solid3d) -> Drawing {
        let mut d = Drawing::default();
        d.add(&Space::Model, Default::default(), EntityKind::Solid3d(s)).unwrap();
        d
    }

    fn drawing() -> Drawing {
        drawing_of(cube())
    }

    fn view(dir: Vec3, shaded: bool) -> Options {
        Options { view_dir: ViewDir::new(dir), shaded, ..Options::default() }
    }

    /// Shaded with Edges.
    fn with_edges(dir: Vec3) -> Options {
        Options { edges: true, ..view(dir, true) }
    }

    #[test]
    fn plan_wireframe_is_the_footprint() {
        let list = build(&drawing(), &Space::Model, &Options::default());
        assert_eq!(list.prims.iter().filter(|p| p.kind == Kind::Polyline).count(), 12);
        assert!((list.bounds.max.x - 1.0).abs() < 1e-12 && (list.bounds.max.y - 1.0).abs() < 1e-12);
    }

    #[test]
    fn front_view_shows_height() {
        let list = build(&drawing(), &Space::Model, &view(Vec3::new(0.0, -1.0, 0.0), false));
        assert!((list.bounds.max.y - 1.0).abs() < 1e-12 && list.bounds.min.y.abs() < 1e-12);
    }

    #[test]
    fn shaded_iso_draws_three_front_faces() {
        let list = build(&drawing(), &Space::Model, &view(Vec3::new(-1.0, -1.0, 1.0), true));
        // Three faces face the viewer: 6 triangles, 18 vertices, the rest culled.
        assert_eq!(list.tris.len(), 18);
    }

    #[test]
    fn iso_box_faces_differ_in_shade() {
        let list = build(&drawing(), &Space::Model, &view(Vec3::new(-1.0, -1.0, 1.0), true));
        let mut shades = Vec::new();
        for p in list.prims.iter().filter(|p| p.kind == Kind::Tris) {
            if !shades.contains(&p.color) {
                shades.push(p.color);
            }
        }
        assert_eq!(shades.len(), 3, "{shades:?}");
    }

    #[test]
    fn views_from_below_show_the_near_faces() {
        let d = drawing_of(pyramid());
        let verts = |dir: Vec3| build(&d, &Space::Model, &view(dir, true)).tris.len();
        // Bottom: the base only; top: the four sides.
        assert_eq!(verts(Vec3::new(0.0, 0.0, -1.0)), 6);
        assert_eq!(verts(Vec3::new(0.0, 0.0, 1.0)), 12);
        // From below the south-west: the base and the two sides toward the viewer.
        assert_eq!(verts(Vec3::new(-1.0, -1.0, -1.0)), 12);
    }

    fn in_block(s: Solid3d, scale: Vec3) -> Drawing {
        let mut d = Drawing::default();
        let mut blk = cadcraft_doc::Block::new("B");
        blk.entities.push(cadcraft_doc::Entity::new(cadcraft_doc::Handle(1), EntityKind::Solid3d(s)));
        d.blocks.insert("B".into(), Arc::new(blk));
        let ins = cadcraft_doc::Insert {
            block: "B".into(),
            insert: Vec3::ZERO,
            scale,
            rotation: 0.0,
            attribs: vec![],
            cols: 1,
            rows: 1,
            col_spacing: 0.0,
            row_spacing: 0.0,
        };
        d.add(&Space::Model, Common::default(), EntityKind::Insert(ins)).unwrap();
        d
    }

    #[test]
    fn solids_in_block_references_face_and_scale_with_the_reference() {
        // Mirrored in X: the sides still face up, so plan shows them, not the base.
        let d = in_block(pyramid(), Vec3::new(-1.0, 1.0, 1.0));
        assert_eq!(build(&d, &Space::Model, &view(Vec3::Z, true)).tris.len(), 12);
        // Height follows the z scale (DXF 43): scaled 2 in z, twice as tall; 1 in z, as tall.
        let tall = |sz: f64| {
            let d = in_block(cube(), Vec3::new(2.0, 2.0, sz));
            build(&d, &Space::Model, &view(Vec3::new(0.0, -1.0, 0.0), false)).bounds.max.y
        };
        assert!((tall(2.0) - 2.0).abs() < 1e-12 && (tall(1.0) - 1.0).abs() < 1e-12, "{} {}", tall(2.0), tall(1.0));
    }

    #[test]
    fn sphere_silhouette_shows_in_plan() {
        let list = build(&drawing_of(sphere()), &Space::Model, &Options::default());
        // The seam and the 16 segments of the equator.
        assert_eq!(list.prims.iter().filter(|p| p.kind == Kind::Polyline).count(), 17);
        assert!(list.verts.iter().any(|v| v.dist(Vec2::new(-1.0, 0.0)) < 1e-9), "the far side of the equator");
    }

    fn line(d: &mut Drawing, a: Vec2, b: Vec2) {
        d.add(&Space::Model, Default::default(), EntityKind::Line(cadcraft_doc::Line { a: a.to3(0.0), b: b.to3(0.0) })).unwrap();
    }

    /// Depths parallel to the vertices, faces covering triangle primitives, edges in `verts`.
    fn assert_depth_consistent(list: &crate::DisplayList) {
        assert_eq!((list.vert_depth.len(), list.tri_depth.len()), (list.verts.len(), list.tris.len()));
        for r in &list.faces {
            assert!(r.start < r.end && list.prims[r.start as usize..r.end as usize].iter().all(|p| p.kind == Kind::Tris), "{r:?}");
        }
        assert!(list.edges.iter().all(|e| e.kind == Kind::Polyline && list.points(e).len() == e.len as usize && e.len >= 2));
    }

    #[test]
    fn plan_and_wireframe_lists_carry_no_depth() {
        let mut d = drawing();
        line(&mut d, Vec2::new(-5.0, 0.0), Vec2::new(5.0, 0.0));
        for o in [Options::default(), view(Vec3::new(-1.0, -1.0, 1.0), false)] {
            let list = build(&d, &Space::Model, &o);
            assert!(list.vert_depth.is_empty() && list.tri_depth.is_empty() && list.faces.is_empty() && list.edges.is_empty());
            assert_eq!(list.plane_slope, [0.0; 2]);
        }
        // A 2D drawing shaded in plan: the same list, every vertex at depth 0.
        let mut flat = Drawing::default();
        line(&mut flat, Vec2::new(-5.0, 0.0), Vec2::new(5.0, 1.0));
        let wire = build(&flat, &Space::Model, &Options::default());
        let shaded = build(&flat, &Space::Model, &view(Vec3::Z, true));
        assert_eq!((&shaded.prims, &shaded.verts, &shaded.tris), (&wire.prims, &wire.verts, &wire.tris));
        assert!(shaded.vert_depth.len() == shaded.verts.len() && shaded.vert_depth.iter().all(|z| *z == 0.0));
    }

    #[test]
    fn shaded_solid_in_front_of_a_line_is_nearer() {
        // Seen from the south-west above: one line behind the unit cube, one in front of it.
        let mut d = drawing();
        line(&mut d, Vec2::new(2.0, 2.0), Vec2::new(3.0, 3.0));
        line(&mut d, Vec2::new(-3.0, -3.0), Vec2::new(-2.0, -2.0));
        let dir = Vec3::new(-1.0, -1.0, 1.0);
        let list = build(&d, &Space::Model, &with_edges(dir));
        assert_depth_consistent(&list);
        let lines: Vec<&[f64]> =
            list.prims.iter().filter(|p| p.kind == Kind::Polyline).map(|p| &list.vert_depth[p.start as usize..(p.start + p.len) as usize]).collect();
        let (behind, front) = (lines[0], lines[1]);
        let cube = |f: fn(f64, f64) -> f64, init| list.tri_depth.iter().copied().fold(init, f);
        assert!(behind.iter().all(|z| *z < cube(f64::min, f64::INFINITY)), "{behind:?}");
        assert!(front.iter().all(|z| *z > cube(f64::max, f64::NEG_INFINITY)), "{front:?}");
        // A 2D point's depth is its world point's (on z = 0).
        let v = ViewDir::new(dir);
        assert!((behind[0] - v.depth(Vec3::new(2.0, 2.0, 0.0))).abs() < 1e-12);
        // The XY plane's depth slope gives every 2D point's depth.
        let line_start = list.prims.iter().find(|p| p.kind == Kind::Polyline).map(|p| p.start as usize).unwrap_or_default();
        let (g, q) = (list.plane_slope, list.verts[line_start]);
        assert!((g[0] * q.x + g[1] * q.y - behind[0]).abs() < 1e-12, "{g:?}");
        // The cube's faces are marked, and its 12 edges go over them, at their corners' depths.
        assert_eq!(list.faces.len(), 1);
        assert_eq!(list.edges.len(), 12);
        let e = &list.edges[0];
        assert!((list.vert_depth[e.start as usize] - v.depth(Vec3::ZERO)).abs() < 1e-12);
    }

    #[test]
    fn shaded_depths_survive_the_parallel_build() {
        let mut d = Drawing::default();
        for i in 0..600 {
            line(&mut d, Vec2::new(f64::from(i), 0.0), Vec2::new(f64::from(i), 1.0));
            if i % 200 == 7 {
                d.add(&Space::Model, Default::default(), EntityKind::Solid3d(cube())).unwrap();
            }
        }
        let list = build(&d, &Space::Model, &with_edges(Vec3::new(1.0, -1.0, 1.0)));
        assert_depth_consistent(&list);
        assert_eq!((list.faces.len(), list.edges.len()), (3, 36));
        let one = crate::build_entities(&d, d.model.iter().map(|e| e.as_ref()), &with_edges(Vec3::new(1.0, -1.0, 1.0)));
        assert_eq!((&one.vert_depth, &one.tri_depth, &one.faces), (&list.vert_depth, &list.tri_depth, &list.faces));
        assert_eq!(one.plane_slope, list.plane_slope);
    }

    #[test]
    fn open_surfaces_show_both_sides() {
        let tri = |open| {
            let mut s = solid(vec![Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)], vec![[0, 1, 2]], vec![]);
            Arc::make_mut(&mut s.mesh).open = open;
            drawing_of(s)
        };
        let verts = |d: &Drawing, dir| build(d, &Space::Model, &view(dir, true)).tris.len();
        let (closed, open) = (tri(false), tri(true));
        assert_eq!((verts(&closed, Vec3::Z), verts(&closed, -Vec3::Z)), (3, 0), "a solid's back faces are culled");
        assert_eq!((verts(&open, Vec3::Z), verts(&open, -Vec3::Z)), (3, 3));
        // Lit alike from either side.
        let shade = |dir| build(&open, &Space::Model, &view(dir, true)).prims[0].color;
        assert_eq!(shade(Vec3::Z), shade(-Vec3::Z));
    }

    #[test]
    fn plain_shaded_draws_no_edges() {
        let dir = Vec3::new(-1.0, -1.0, 1.0);
        let plain = build(&drawing(), &Space::Model, &view(dir, true));
        let edged = build(&drawing(), &Space::Model, &with_edges(dir));
        assert_depth_consistent(&plain);
        assert!(plain.edges.is_empty());
        assert_eq!(edged.edges.len(), 12);
        // The faces are the same either way; only the edges' vertices are added.
        assert_eq!((&plain.prims, &plain.tris, &plain.faces), (&edged.prims, &edged.tris, &edged.faces));
        assert_eq!(edged.verts.len() - plain.verts.len(), 24);
        // Edges mean nothing without shading: the wireframe is unchanged.
        let wire = |edges| build(&drawing(), &Space::Model, &Options { edges, ..view(dir, false) });
        let (a, b) = (wire(false), wire(true));
        assert!(b.edges.is_empty() && a.prims == b.prims && a.verts == b.verts);
    }

    #[test]
    fn edge_on_view_keeps_tolerance_and_point_size() {
        // A unit circle and a relative-size cross on the XY plane, seen edge-on from the left.
        let mut d = Drawing::default();
        d.header.set_i64("PDMODE", 3);
        d.add(&Space::Model, Default::default(), EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::ZERO, radius: 1.0 })).unwrap();
        d.add(&Space::Model, Default::default(), EntityKind::Point(cadcraft_doc::Point { p: Vec3::ZERO, angle: 0.0 })).unwrap();
        let opts = Options { view_height: 10.0, ..view(Vec3::new(-1.0, 0.0, 0.0), false) };
        let list = build(&d, &Space::Model, &opts);
        // The circle is tessellated finely (it spans its diameter) and the cross stays inside it.
        assert!((list.bounds.width() - 2.0).abs() < 0.01, "{:?}", list.bounds);
    }
}
