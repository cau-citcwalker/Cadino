//! Objects drawn where they are in 3D. A planar object draws in its own plane
//! ([`Entity::plane`]: its extrusion's OCS at its elevation): its 2D drawing goes through the
//! view projection of that plane, a 2D affine transform ([`with_plane`]), so circles, text,
//! hatches and dimensions keep their 2D drawing code. True 3D objects (lines whose ends differ
//! in height, 3D polylines, 3D faces, helices, rays) project each 3D point ([`free`]). Seen
//! from straight above both draw exactly as 2D always has.

use cadcraft_doc::{Entity, EntityKind, Linetype};
use cadcraft_geom::{Mat3, Mat4, TAU, Vec2, Vec3};

use crate::{Builder, Ctx, Ink, linetype};

/// A 2D transform in 3D, with z scaled by the square root of its determinant (a viewport's
/// model → paper transform; block references take their own z scale, `Insert::lift`).
pub(crate) fn lift(m: &Mat3) -> Mat4 {
    let mut t = Mat4::from_mat3(m);
    t.m[2][2] = m.det().abs().sqrt();
    t
}

/// The depth of the local XY plane under `m` (local → view x, y and depth) at view-plane point
/// `q`, as `g[0] q.x + g[1] q.y + g[2]`. Where the view sees the plane edge-on, the
/// least-squares point of the plane (as [`cadcraft_geom::ViewDir::unproject_xy`]).
pub(crate) fn depth_gradient(m: &Mat4) -> [f64; 3] {
    let a = m.plane_xy();
    let (g0, g1, d0) = (m.m[2][0], m.m[2][1], m.m[2][3]);
    let n2 = a.a * a.a + a.b * a.b + a.c * a.c + a.d * a.d;
    // Local point = R (q - t): R the inverse of the plane's linear part, or its pseudo-inverse.
    let r = match a.inverse().filter(|_| a.det().abs() > 1e-6 * n2 / 2.0) {
        Some(i) => [i.a, i.c, i.b, i.d],
        None if n2 > 1e-24 => [a.a / n2, a.b / n2, a.c / n2, a.d / n2],
        None => return [0.0, 0.0, if d0.is_finite() { d0 } else { 0.0 }],
    };
    let (gx, gy) = (g0 * r[0] + g1 * r[2], g0 * r[1] + g1 * r[3]);
    let g = [gx, gy, d0 - gx * a.e - gy * a.f];
    if g.iter().all(|v| v.is_finite()) { g } else { [0.0; 3] }
}

/// The plane `e` draws in: its own, or the level of a 3D object that lies flat (a point's
/// figure, a level ray or 3D polyline), which then draws as in 2D. `None` for objects drawn by
/// their 3D points.
pub(crate) fn draw_plane(e: &Entity) -> Option<Mat4> {
    let level = |z: f64, flat: bool| (z.is_finite() && flat).then(|| Mat4::translate(Vec3::Z * z));
    match &e.kind {
        EntityKind::Point(p) => level(p.p.z, true),
        EntityKind::Ray(r) | EntityKind::XLine(r) => level(r.base.z, r.dir.z == 0.0),
        EntityKind::Polyline3d(p) => {
            let z = p.points.first()?.z;
            level(z, p.points.iter().all(|q| q.z == z))
        }
        EntityKind::Face3d(f) => level(f.corners[0].z, f.corners.iter().all(|q| q.z == f.corners[0].z)),
        _ => e.plane(),
    }
}

/// `ctx` for an object in plane `p` (local 2D → the block's or world 3D).
pub(crate) fn with_plane<'a>(ctx: &Ctx<'a>, p: &Mat4) -> Ctx<'a> {
    let xf3 = ctx.xf3.mul(p);
    // Seen from above, a level plane draws as the XY plane does: keep that exact transform.
    let xf = if ctx.flat() && p.plane_xy() == Mat3::IDENTITY { ctx.xf } else { xf3.plane_xy() };
    Ctx { xf, xf3, grad: depth_gradient(&xf3), ..ctx.clone() }
}

/// Draw a true 3D object in a view that shows heights.
pub(crate) fn free(b: &mut Builder, ctx: &Ctx, e: &Entity, rgb: Ink, lw: f32, lt: Option<&Linetype>, ltscale: f64, tol: f64) {
    match &e.kind {
        EntityKind::Line(l) => stroke3(b, ctx, rgb, lw, lt, ltscale, &[l.a, l.b]),
        EntityKind::Polyline3d(p) => {
            let mut pts = p.points.clone();
            if p.closed
                && let Some(f) = pts.first().copied()
            {
                pts.push(f);
            }
            stroke3(b, ctx, rgb, lw, lt, ltscale, &pts);
        }
        EntityKind::Face3d(f) if f.hidden_edges & 15 == 0 => {
            let c = f.corners;
            stroke3(b, ctx, rgb, lw, lt, ltscale, &[c[0], c[1], c[2], c[3], c[0]]);
        }
        EntityKind::Face3d(f) => {
            // Invisible edges (DXF 70 bits 1, 2, 4, 8) are not drawn.
            let c = f.corners;
            for (i, (p, q)) in [(c[0], c[1]), (c[1], c[2]), (c[2], c[3]), (c[3], c[0])].into_iter().enumerate() {
                if f.hidden_edges & (1 << i) == 0 {
                    stroke3(b, ctx, rgb, lw, lt, ltscale, &[p, q]);
                }
            }
        }
        EntityKind::Helix(h) => {
            let h = h.clone().sanitized();
            let n = cadcraft_geom::arc_segments(h.base_radius.max(h.top_radius), TAU * h.turns, tol).max(8);
            let pts: Vec<Vec3> = (0..=n).map(|i| h.point_at(i as f64 / n as f64)).collect();
            stroke3(b, ctx, rgb, lw, lt, ltscale, &pts);
        }
        EntityKind::Ray(r) | EntityKind::XLine(r) if b.lens().is_some() => {
            if let Some(c) = b.lens() {
                b.persp_infinite(ctx, &c, rgb, ctx.xf3.apply(r.base), ctx.xf3.apply_vec(r.dir), matches!(e.kind, EntityKind::Ray(_)));
            }
        }
        EntityKind::Ray(r) | EntityKind::XLine(r) => {
            let q = ctx.xf3.apply(r.base);
            let d = ctx.xf3.apply_vec(r.dir).xy().normalized();
            if d == Vec2::ZERO {
                // Seen end-on.
                b.point_at(ctx, rgb, q.xy(), Some(q.z));
            } else {
                b.infinite_at(ctx, rgb, q.xy(), d, matches!(e.kind, EntityKind::Ray(_)), Some(q.z));
            }
        }
        EntityKind::Solid3d(s) => crate::solid::draw(b, ctx, s, rgb, lw),
        EntityKind::PolyMesh(m) => crate::solid::draw_mesh(b, ctx, &m.mesh3(), &Mat4::IDENTITY, rgb, lw, false),
        _ => {}
    }
}

/// A 3D polyline (local points) with linetype `lt`, whose pattern runs along the 3D length.
/// ponytail: only the dashes and dots: text and shapes embedded in the linetype aren't drawn
/// along a 3D line (they need a plane to stand in); upgrade: place them in the plane of the
/// line and the view direction, as `linetype_text::texts` does in 2D.
fn stroke3(b: &mut Builder, ctx: &Ctx, rgb: Ink, lw: f32, lt: Option<&Linetype>, ltscale: f64, pts: &[Vec3]) {
    let draw = |b: &mut Builder, run: &[Vec3]| {
        let (q, z): (Vec<Vec2>, Vec<f64>) = run.iter().map(|p| ctx.xf3.apply(*p)).map(|v| (v.xy(), v.z)).unzip();
        match (q.as_slice(), z.first()) {
            ([p], z) => b.point_at(ctx, rgb, *p, z.copied()),
            _ => b.polyline_at(ctx, rgb, lw, &q, Mat3::IDENTITY, &z),
        }
    };
    let Some(lt) = lt else { return draw(b, pts) };
    // Lay the polyline out along x by its 3D lengths, dash that, and take the dashes back.
    let mut along = Vec::with_capacity(pts.len());
    let mut s = 0.0;
    for (i, p) in pts.iter().enumerate() {
        if let Some(prev) = i.checked_sub(1).and_then(|j| pts.get(j)) {
            s += (*p - *prev).len();
        }
        along.push(s);
    }
    let flat: Vec<Vec2> = along.iter().map(|x| Vec2::new(*x, 0.0)).collect();
    let min = b.opts.min_dash / ctx.plan.scale_factor().max(1e-12);
    for dash in linetype::apply(&flat, lt, ltscale, min) {
        let run: Vec<Vec3> = dash.iter().map(|q| at(pts, &along, q.x)).collect();
        draw(b, &run);
    }
}

/// The point `x` along polyline `pts` whose vertices are `along` from its start.
fn at(pts: &[Vec3], along: &[f64], x: f64) -> Vec3 {
    let i = along.partition_point(|v| *v < x);
    let prev = i.checked_sub(1).and_then(|j| pts.get(j).zip(along.get(j)));
    match (prev, pts.get(i).zip(along.get(i))) {
        (Some((a, sa)), Some((b, sb))) if sb > sa => *a + (*b - *a) * ((x - sa) / (sb - sa)),
        (_, Some((b, _))) => *b,
        (Some((a, _)), None) => *a,
        (None, None) => Vec3::ZERO,
    }
}
