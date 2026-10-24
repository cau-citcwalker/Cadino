//! Perspective views ([`cadcraft_geom::Persp`]). The builder works in view coordinates (x and y
//! in the parallel view plane, z the depth toward the viewer) as for a parallel view; in
//! perspective each primitive's points then go through the eye onto the target plane, after
//! lines and triangles are clipped at the near plane, so nothing behind the eye draws. The list
//! holds target-plane points with screen-linear depths ([`cadcraft_geom::Persp::map`]), so it
//! draws, picks and depth-tests as a parallel view's does.

use cadcraft_geom::{Mat3, Persp, Vec2, Vec3, ViewDir};

use crate::{Builder, Ctx, DPrim, Ink, Kind};

impl Builder<'_> {
    /// The points of a primitive in view coordinates: `pts` after `xf` at depths `z` (3D points),
    /// else on the context's plane: exactly through `ctx.xf3` when `xf` is the context's own
    /// transform, else by the plane's depth gradient (which loses a plane seen edge-on in the
    /// parallel view).
    pub(crate) fn view_points(&self, ctx: &Ctx, pts: &[Vec2], xf: Mat3, z: &[f64]) -> Vec<Vec3> {
        let exact = z.is_empty() && xf == ctx.xf;
        let g = ctx.grad;
        pts.iter()
            .enumerate()
            .map(|(i, p)| match z.get(i) {
                Some(z) => xf.apply(*p).to3(*z),
                None if exact => ctx.xf3.apply(p.to3(0.0)),
                None => {
                    let q = xf.apply(*p);
                    q.to3(g[0] * q.x + g[1] * q.y + g[2])
                }
            })
            .collect()
    }

    /// A polyline through view-coordinates points `v` seen through `c`: its pieces in front of
    /// the near plane.
    pub(crate) fn persp_polyline(&mut self, ctx: &Ctx, c: &Persp, color: Ink, lw: f32, v: &[Vec3]) {
        for run in clip_line(c, v) {
            self.push_line(ctx, color, lw, &run, true);
        }
    }

    /// A polyline through target-plane points with their depths; `bounds`: they count toward the
    /// list's bounds (a line without end's don't).
    fn push_line(&mut self, ctx: &Ctx, color: Ink, lw: f32, run: &[(Vec2, f64)], bounds: bool) {
        let (Ok(start), Ok(len)) = (u32::try_from(self.list.verts.len()), u32::try_from(run.len())) else { return };
        if len < 2 {
            return;
        }
        self.note_pen(color);
        let (color, lw) = self.styled(color, lw);
        for (q, d) in run {
            if bounds {
                self.list.bounds.add(*q);
            }
            self.list.verts.push(*q);
            if self.depth.is_some() {
                self.list.vert_depth.push(*d);
            }
        }
        self.list.prims.push(DPrim {
            handle: ctx.top,
            color: color.rgb,
            aci7: color.aci7,
            lw,
            transparency: self.transparency,
            kind: Kind::Polyline,
            start,
            len,
        });
    }

    /// Triangles (view-coordinates corners `v`, three a triangle) seen through `c`, clipped at
    /// the near plane.
    pub(crate) fn persp_tris(&mut self, ctx: &Ctx, c: &Persp, color: Ink, v: &[Vec3]) {
        let mut out = Vec::with_capacity(v.len());
        for t in v.chunks_exact(3) {
            if let [a, b, d] = t {
                clip_tri(c, [*a, *b, *d], &mut out);
            }
        }
        let (Ok(start), Ok(len)) = (u32::try_from(self.list.tris.len()), u32::try_from(out.len())) else { return };
        if len < 3 {
            return;
        }
        self.note_pen(color);
        let (color, _) = self.styled(color, 0.0);
        for (q, d) in &out {
            self.list.bounds.add(*q);
            self.list.tris.push(*q);
            if self.depth.is_some() {
                self.list.tri_depth.push(*d);
            }
        }
        self.list.prims.push(DPrim {
            handle: ctx.top,
            color: color.rgb,
            aci7: color.aci7,
            lw: 0.0,
            transparency: self.transparency,
            kind: Kind::Tris,
            start,
            len,
        });
    }

    /// A point marker at view-coordinates point `v`, unless it is behind the near plane.
    pub(crate) fn persp_point(&mut self, ctx: &Ctx, c: &Persp, color: Ink, v: Vec3) {
        let Some((q, d)) = c.map(v) else { return };
        let Ok(start) = u32::try_from(self.list.verts.len()) else { return };
        let (color, _) = self.styled(color, 0.0);
        self.list.bounds.add(q);
        self.list.verts.push(q);
        if self.depth.is_some() {
            self.list.vert_depth.push(d);
        }
        self.list.prims.push(DPrim {
            handle: ctx.top,
            color: color.rgb,
            aci7: color.aci7,
            lw: 0.0,
            transparency: self.transparency,
            kind: Kind::Point,
            start,
            len: 1,
        });
    }

    /// An infinite line (a ray: from `base` on) through view-coordinates point `base` along
    /// `dir`, seen through `c`. Square to the line of sight it is a line without end at its depth;
    /// else what is in front of the eye runs from the near plane (or the ray's base) to the
    /// vanishing point of its direction away from the eye.
    pub(crate) fn persp_infinite(&mut self, ctx: &Ctx, c: &Persp, color: Ink, base: Vec3, dir: Vec3, ray: bool) {
        let n = dir.len();
        if !(n > 0.0 && n.is_finite() && base.is_finite()) {
            return;
        }
        let d = dir * (1.0 / n);
        if d.z.abs() < 1e-9 {
            let (Some((q, depth)), Ok(start)) = (c.map(base), u32::try_from(self.list.verts.len())) else { return };
            let d2 = d.xy().normalized();
            if d2 == Vec2::ZERO {
                return;
            }
            let (color, _) = self.styled(color, 0.0);
            self.list.verts.extend([q, d2]);
            if self.depth.is_some() {
                self.list.vert_depth.extend([depth, depth]);
            }
            self.list.prims.push(DPrim {
                handle: ctx.top,
                color: color.rgb,
                aci7: color.aci7,
                lw: 0.0,
                transparency: self.transparency,
                kind: Kind::Infinite { ray },
                start,
                len: 2,
            });
            return;
        }
        let lim = clip_depth(c);
        // The parameter where the line crosses the near plane, and that point exactly on it.
        let t_near = (lim - base.z) / d.z;
        let on_near = |t: f64| {
            let p = base + d * t;
            c.map(Vec3::new(p.x, p.y, lim))
        };
        // Where lines along `e` (going away from the eye: e.z < 0) meet on the target plane.
        let vanish = |e: Vec3| Some((c.eye.xy() + e.xy() * (c.focal / -e.z), c.eye.z - 2.0 * c.focal));
        let (from, to) = if d.z < 0.0 {
            // Going away as t grows: seen from the near plane (or the ray's base, if in front).
            let from = if ray && t_near <= 0.0 { c.map(base) } else { on_near(t_near) };
            (from, vanish(d))
        } else if ray {
            // Coming toward the eye: a ray is seen from its base to the near plane.
            if t_near < 0.0 {
                return;
            }
            (c.map(base), on_near(t_near))
        } else {
            (vanish(-d), on_near(t_near))
        };
        if let (Some(a), Some(b)) = (from, to)
            && a.0.is_finite()
            && b.0.is_finite()
        {
            self.push_line(ctx, color, 0.0, &[a, b], false);
        }
    }
}

/// The depth lines and triangles are cut at: a hair in front of the near plane, so the points
/// on the cut map whatever the rounding.
pub(crate) fn clip_depth(c: &Persp) -> f64 {
    c.eye.z - c.near() * (1.0 + 1e-3)
}

/// The pieces of polyline `v` (view coordinates) in front of `c`'s near plane, seen through the
/// eye: target-plane points with their depths.
pub(crate) fn clip_line(c: &Persp, v: &[Vec3]) -> Vec<Vec<(Vec2, f64)>> {
    let lim = clip_depth(c);
    let inside = |p: &Vec3| p.z <= lim;
    let mut runs = Vec::new();
    let mut cur: Vec<(Vec2, f64)> = Vec::with_capacity(v.len());
    let mut prev: Option<Vec3> = None;
    for p in v {
        if let Some(a) = prev
            && inside(&a) != inside(p)
        {
            let t = (lim - a.z) / (p.z - a.z);
            let x = a + (*p - a) * t;
            if let Some(m) = c.map(Vec3::new(x.x, x.y, lim)) {
                cur.push(m);
            }
            if !inside(p) && !cur.is_empty() {
                let run = std::mem::take(&mut cur);
                if run.len() >= 2 {
                    runs.push(run);
                }
            }
        }
        if inside(p)
            && let Some(m) = c.map(*p)
        {
            cur.push(m);
        }
        prev = Some(*p);
    }
    if cur.len() >= 2 {
        runs.push(cur);
    }
    runs
}

/// Triangle `t` (view coordinates) clipped at `c`'s near plane and seen through the eye, as
/// triangles (target-plane corners with their depths) added to `out`.
pub(crate) fn clip_tri(c: &Persp, t: [Vec3; 3], out: &mut Vec<(Vec2, f64)>) {
    let lim = clip_depth(c);
    let inside = |p: &Vec3| p.z <= lim;
    let mut poly: Vec<Vec3> = Vec::with_capacity(4);
    for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
        if inside(&a) {
            poly.push(a);
        }
        if inside(&a) != inside(&b) {
            let x = a + (b - a) * ((lim - a.z) / (b.z - a.z));
            poly.push(Vec3::new(x.x, x.y, lim));
        }
    }
    let Some(m) = poly.iter().map(|p| c.map(*p)).collect::<Option<Vec<_>>>() else { return };
    if let [p0, rest @ ..] = m.as_slice() {
        for w in rest.windows(2) {
            if let [p1, p2] = w {
                out.extend([*p0, *p1, *p2]);
            }
        }
    }
}

/// How the depth (as [`Persp::map`] gives it) of the plane through `o` with normal `n` (view
/// coordinates) varies across the target plane: `g` with depth `g[0] q.x + g[1] q.y + g[2]` at
/// target-plane point `q`. Zero where the plane passes through the eye or is seen edge-on.
pub(crate) fn plane_gradient(c: &Persp, o: Vec3, n: Vec3) -> [f64; 3] {
    // Where the lines of sight through three points of the target plane meet the plane.
    let hit = |q: Vec2| {
        let dir = (q - c.eye.xy()).to3(-c.focal);
        let nd = n.dot(dir);
        let t = n.dot(o - c.eye) / nd;
        (nd.abs() > 1e-12 * n.len() * dir.len() && t > 0.0).then(|| c.map(c.eye + dir * t)).flatten()
    };
    // Three hits not in a line, from a grid of points around the centre (a plane along the line
    // of sight, the floor of a level view, isn't hit at the centre; one beside the eye only on
    // its side).
    let e = c.eye.xy();
    const STEPS: [f64; 7] = [0.0, 0.5, -0.5, 1.0, -1.0, 3.0, -3.0];
    let hits: Vec<(Vec2, f64)> =
        STEPS.iter().flat_map(|y| STEPS.iter().map(move |x| Vec2::new(*x, *y))).filter_map(|k| hit(e + k * c.focal)).collect();
    let mut three = None;
    'find: for (i, h0) in hits.iter().enumerate() {
        for (j, h1) in hits.iter().enumerate().skip(i + 1) {
            for h2 in hits.iter().skip(j + 1) {
                let (a, b) = (h1.0 - h0.0, h2.0 - h0.0);
                if (a.x * b.y - b.x * a.y).abs() > 1e-6 * c.focal * c.focal {
                    three = Some((*h0, *h1, *h2));
                    break 'find;
                }
            }
        }
    }
    let Some(((q0, d0), (q1, d1), (q2, d2))) = three else { return [0.0; 3] };
    let (a, b) = (q1 - q0, q2 - q0);
    let det = a.x * b.y - b.x * a.y;
    let (gx, gy) = (((d1 - d0) * b.y - (d2 - d0) * a.y) / det, (a.x * (d2 - d0) - b.x * (d1 - d0)) / det);
    let g = [gx, gy, d0 - gx * q0.x - gy * q0.y];
    if g.iter().all(|v| v.is_finite()) { g } else { [0.0; 3] }
}

/// The depth slope of the WCS XY plane in perspective view `v` ([`crate::DisplayList::plane_slope`]).
pub(crate) fn xy_slope(v: &ViewDir, c: &Persp) -> [f64; 2] {
    let g = plane_gradient(c, v.view_of(Vec3::ZERO), v.view_of(Vec3::Z));
    [g[0], g[1]]
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_geom::Camera;

    fn persp() -> (ViewDir, Persp) {
        let cam = Camera::new(50.0, 0.0);
        let v = ViewDir::named("front").unwrap_or_default().with_camera(Some(cam), Vec2::new(0.0, 4.0), cam.height_at(10.0));
        (v, v.persp.unwrap_or(Persp { eye: Vec3::Z, focal: 1.0 }))
    }

    #[test]
    fn lines_through_the_eye_plane_are_cut_at_the_near_plane() {
        let (_, c) = persp();
        // From in front of the eye to behind it, and back.
        let runs = clip_line(&c, &[Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 20.0), Vec3::new(1.0, 0.0, 0.0)]);
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|r| r.len() == 2 && r.iter().all(|(q, d)| q.is_finite() && d.is_finite())));
        // Wholly behind: nothing.
        assert!(clip_line(&c, &[Vec3::new(0.0, 0.0, 15.0), Vec3::new(1.0, 0.0, 30.0)]).is_empty());
        // A NaN point draws nothing and doesn't panic.
        assert!(clip_line(&c, &[Vec3::new(f64::NAN, 0.0, 0.0), Vec3::new(f64::NAN, 1.0, f64::NAN)]).is_empty());
    }

    #[test]
    fn triangles_crossing_the_near_plane_keep_their_front() {
        let (_, c) = persp();
        let mut out = Vec::new();
        // One corner behind the eye: a quadrilateral, two triangles.
        clip_tri(&c, [Vec3::new(-1.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 20.0)], &mut out);
        assert_eq!(out.len(), 6);
        out.clear();
        clip_tri(&c, [Vec3::new(-1.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)], &mut out);
        assert_eq!(out.len(), 3);
        out.clear();
        clip_tri(&c, [Vec3::new(-1.0, 0.0, 30.0), Vec3::new(1.0, 0.0, 30.0), Vec3::new(0.0, 1.0, 30.0)], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn a_perspective_list_draws_nearer_larger_and_nothing_behind_the_eye() {
        use cadcraft_doc::{Drawing, EntityKind, Line, Space};
        let mut d = Drawing::default();
        let line = |d: &mut Drawing, a: Vec3, b: Vec3| d.add(&Space::Model, Default::default(), EntityKind::Line(Line { a, b })).unwrap();
        // Two posts 10 high, one 50 nearer the viewer (front view: nearer is -Y), and a line
        // from in front of the eye to behind it.
        let near = line(&mut d, Vec3::new(0.0, -50.0, 0.0), Vec3::new(0.0, -50.0, 10.0));
        let far = line(&mut d, Vec3::new(5.0, 0.0, 0.0), Vec3::new(5.0, 0.0, 10.0));
        let through = line(&mut d, Vec3::new(-5.0, 0.0, 5.0), Vec3::new(-5.0, -500.0, 5.0));
        let cam = Camera::new(50.0, 0.0);
        let dir = ViewDir::named("front").unwrap().with_camera(Some(cam), Vec2::new(0.0, 5.0), cam.height_at(100.0));
        let list = crate::build(&d, &Space::Model, &crate::Options { view_dir: dir, shaded: true, ..Default::default() });
        let len = |h| {
            let p = list.prims.iter().find(|p| p.handle == h).unwrap();
            let pts = list.points(p);
            pts[0].dist(pts[pts.len() - 1])
        };
        assert!((len(far) - 10.0).abs() < 1e-9, "the target plane keeps sizes");
        assert!((len(near) - 20.0).abs() < 1e-9, "half as far from the eye: twice as long");
        // The line through the eye plane is cut at the near plane: drawn, finite.
        assert!(list.prims.iter().any(|p| p.handle == through));
        assert!(list.verts.iter().all(|q| q.is_finite()) && list.bounds.max.is_finite());
        // Depths: one per vertex, nearer larger.
        assert_eq!(list.vert_depth.len(), list.verts.len());
        let depth = |h| list.prims.iter().find(|p| p.handle == h).map(|p| list.vert_depth[p.start as usize]).unwrap();
        assert!(depth(near) > depth(far));
    }

    #[test]
    fn the_xy_planes_depth_is_affine_on_screen() {
        let (v, c) = persp();
        let g = xy_slope(&v, &c);
        // Two floor points: their depth difference follows the slope.
        let (pa, pb) = (Vec3::new(1.0, 3.0, 0.0), Vec3::new(-2.0, 7.0, 0.0));
        let (Some((qa, da)), Some((qb, db))) = (c.map(v.view_of(pa)), c.map(v.view_of(pb))) else { panic!("in front") };
        assert!(((db - da) - (g[0] * (qb.x - qa.x) + g[1] * (qb.y - qa.y))).abs() < 1e-9);
    }
}
