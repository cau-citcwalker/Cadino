//! 3D solids: their edges and silhouettes (wireframe), or their faces shaded and depth-sorted,
//! projected by the view direction of [`crate::Options`].
//!
//! A solid in a block reference is placed in the world as EXPLODE places it: the references'
//! transform (`ctx.plan`) lifted to 3D with z scaled by the square root of its determinant (see
//! [`Solid3d::transform`]). Culling, depth and shading use those world points, which the view
//! then projects.

use cadcraft_color::Rgb;
use cadcraft_doc::Solid3d;
use cadcraft_geom::{Mat3, Mat4, Vec2, Vec3};

use crate::{Builder, Ctx, Ink};

/// Shade steps of a shaded face (runs of equal shade share one primitive): finer than the
/// shade change between neighbouring facets of a curved face, so curved faces don't band.
const LEVELS: f64 = 64.0;

/// A triangle faces the viewer when its unit normal points toward the viewer by more than this;
/// an edge-on triangle (a rounding error either way) faces away.
const FRONT: f64 = 1e-9;

pub(crate) fn draw(b: &mut Builder, ctx: &Ctx, s: &Solid3d, color: Ink, lw: f32) {
    let (right, up, toward) = b.opts.view_dir.axes();
    let proj = |w: Vec3| Vec2::new(right.dot(w), up.dot(w));
    // ponytail: the 2D block transform carries no insert z, base z or z scale (EXPLODE drops
    // them too); carry a Mat4 in Ctx if elevated references to solids matter.
    let mut lift = Mat4::from_mat3(&ctx.plan);
    lift.m[2][2] = ctx.plan.det().abs().sqrt();
    let xf = lift.mul(&s.xform);
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
            let pts: Vec<Vec2> = e.iter().map(|p| proj(xf.apply(*p))).collect();
            b.polyline_at(ctx, color, lw, &pts, Mat3::IDENTITY);
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
                b.polyline_at(ctx, color, lw, &[proj(*p), proj(*q)], Mat3::IDENTITY);
            }
        }
        return;
    }
    // Light from above and to the left of the viewer, so faces at the same angle to the view
    // (the three faces of a box in an isometric view) shade apart.
    let light = (toward + up * 0.5 - right * 0.3).normalized();
    // ponytail: painter's sort per solid, no depth buffer: interpenetrating solids can overlap
    // wrongly, and lines and points draw over or under faces by draw order alone (the GPU
    // canvas puts every line over every fill). Upgrade path: a depth per vertex in the display
    // list and a depth buffer in the GPU canvas.
    let mut faces: Vec<(f64, f64, [Vec2; 3])> = Vec::with_capacity(s.mesh.tris.len());
    for t in &s.mesh.tris {
        let Some(([p, q, r], n)) = face(t) else { continue };
        // Back faces are culled.
        if n.dot(toward) <= FRONT {
            continue;
        }
        let depth = toward.dot(p) + toward.dot(q) + toward.dot(r);
        let level = ((0.3 + 0.7 * n.dot(light).max(0.0)) * LEVELS).round() / LEVELS;
        faces.push((depth, level, [proj(p), proj(q), proj(r)]));
    }
    faces.sort_by(|a, b| a.0.total_cmp(&b.0));
    let base = if color.aci7 { Rgb(205, 205, 205) } else { color.rgb };
    let mut run: Vec<Vec2> = Vec::new();
    let mut level = f64::NAN;
    for (_, l, tri) in faces {
        if l != level && !run.is_empty() {
            b.tris_at(ctx, shade(base, color, level), &run, Mat3::IDENTITY);
            run.clear();
        }
        level = l;
        run.extend(tri);
    }
    if !run.is_empty() {
        b.tris_at(ctx, shade(base, color, level), &run, Mat3::IDENTITY);
    }
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
        Solid3d { brep: Arc::from(""), mesh: Arc::new(Mesh3 { verts, tris, edges }), xform: Default::default() }
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
    fn solids_in_block_references_face_and_scale_as_exploded() {
        // Mirrored in X: the sides still face up, so plan shows them, not the base.
        let d = in_block(pyramid(), Vec3::new(-1.0, 1.0, 1.0));
        assert_eq!(build(&d, &Space::Model, &view(Vec3::Z, true)).tris.len(), 12);
        // Scaled 2: twice as tall too.
        let d = in_block(cube(), Vec3::new(2.0, 2.0, 1.0));
        let list = build(&d, &Space::Model, &view(Vec3::new(0.0, -1.0, 0.0), false));
        assert!((list.bounds.max.y - 2.0).abs() < 1e-12, "{:?}", list.bounds);
    }

    #[test]
    fn sphere_silhouette_shows_in_plan() {
        let list = build(&drawing_of(sphere()), &Space::Model, &Options::default());
        // The seam and the 16 segments of the equator.
        assert_eq!(list.prims.iter().filter(|p| p.kind == Kind::Polyline).count(), 17);
        assert!(list.verts.iter().any(|v| v.dist(Vec2::new(-1.0, 0.0)) < 1e-9), "the far side of the equator");
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
