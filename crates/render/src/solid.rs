//! 3D solids: their edges (wireframe), or their faces shaded and depth-sorted, projected by
//! the view direction of [`crate::Options`].
//!
//! A point goes onto the view plane as `ctx.xf` (block transforms and the view's projection of
//! the XY plane) of its x and y, plus its z along the view's projected Z axis.

use cadcraft_color::Rgb;
use cadcraft_doc::Solid3d;
use cadcraft_geom::{Mat3, Vec2, Vec3};

use crate::{Builder, Ctx, Ink};

/// Shade steps of a shaded face (runs of equal shade share one primitive).
const LEVELS: f64 = 24.0;

pub(crate) fn draw(b: &mut Builder, ctx: &Ctx, s: &Solid3d, color: Ink, lw: f32) {
    let (vx, vy, toward) = b.opts.view_dir.axes();
    let zdir = Vec2::new(vx.z, vy.z);
    let proj = |w: Vec3| ctx.xf.apply(w.xy()) + zdir * w.z;
    if !b.opts.shaded {
        for e in &s.mesh.edges {
            let pts: Vec<Vec2> = e.iter().map(|p| proj(s.world(*p))).collect();
            b.polyline_at(ctx, color, lw, &pts, Mat3::IDENTITY);
        }
        return;
    }
    let x = &s.xform.m;
    let mirrored = (x[0][0] * x[1][1] - x[1][0] * x[0][1] < 0.0) != ctx.xf.is_mirroring();
    // ponytail: painter's sort per solid; interpenetrating solids can overlap wrongly. Upgrade
    // path: a depth buffer in the GPU canvas.
    let mut faces: Vec<(f64, f64, [Vec2; 3])> = Vec::with_capacity(s.mesh.tris.len());
    for t in &s.mesh.tris {
        let Some([p, q, r]) = s.mesh.tri(t) else { continue };
        let (p, q, r) = (s.world(p), s.world(q), s.world(r));
        let n = (q - p).cross(r - p).normalized();
        let facing = if mirrored { -n.dot(toward) } else { n.dot(toward) };
        // Back faces (and NaN normals of degenerate triangles) are culled.
        if facing.is_nan() || facing <= 0.0 {
            continue;
        }
        let depth = toward.dot(p) + toward.dot(q) + toward.dot(r);
        let level = ((0.3 + 0.7 * facing) * LEVELS).round() / LEVELS;
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

fn shade(base: Rgb, color: Ink, level: f64) -> Ink {
    let f = |c: u8| (f64::from(c) * level).round().clamp(0.0, 255.0) as u8;
    Ink { rgb: Rgb(f(base.0), f(base.1), f(base.2)), aci7: false, aci: color.aci }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cadcraft_doc::{Drawing, EntityKind, Space};
    use cadcraft_geom::{Mesh3, ViewDir};

    use super::*;
    use crate::{Kind, Options, build};

    /// A unit cube's 12 edges and 12 outward triangles.
    fn cube() -> Solid3d {
        let v = |i: u32| Vec3::new(f64::from(i & 1), f64::from((i >> 1) & 1), f64::from((i >> 2) & 1));
        let verts = (0..8).map(v).collect();
        let tris =
            vec![[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
        let e = |a: u32, b: u32| vec![v(a), v(b)];
        let edges = vec![e(0, 1), e(1, 3), e(3, 2), e(2, 0), e(4, 5), e(5, 7), e(7, 6), e(6, 4), e(0, 4), e(1, 5), e(2, 6), e(3, 7)];
        Solid3d { brep: Arc::from(""), mesh: Arc::new(Mesh3 { verts, tris, edges }), xform: Default::default() }
    }

    fn drawing() -> Drawing {
        let mut d = Drawing::default();
        d.add(&Space::Model, Default::default(), EntityKind::Solid3d(cube())).unwrap();
        d
    }

    #[test]
    fn plan_wireframe_is_the_footprint() {
        let list = build(&drawing(), &Space::Model, &Options::default());
        assert_eq!(list.prims.iter().filter(|p| p.kind == Kind::Polyline).count(), 12);
        assert!((list.bounds.max.x - 1.0).abs() < 1e-12 && (list.bounds.max.y - 1.0).abs() < 1e-12);
    }

    #[test]
    fn front_view_shows_height() {
        let opts = Options { view_dir: ViewDir::new(Vec3::new(0.0, -1.0, 0.0)), ..Options::default() };
        let list = build(&drawing(), &Space::Model, &opts);
        assert!((list.bounds.max.y - 1.0).abs() < 1e-12 && list.bounds.min.y.abs() < 1e-12);
    }

    #[test]
    fn shaded_iso_draws_three_front_faces() {
        let opts = Options { view_dir: ViewDir::new(Vec3::new(-1.0, -1.0, 1.0)), shaded: true, ..Options::default() };
        let list = build(&drawing(), &Space::Model, &opts);
        // Three faces face the viewer: 6 triangles, 18 vertices, the rest culled.
        assert_eq!(list.tris.len(), 18);
    }
}
