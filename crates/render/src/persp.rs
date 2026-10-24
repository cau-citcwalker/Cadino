//! Perspective views ([`cadcraft_geom::Persp`]) and clipping planes ([`cadcraft_geom::Clip`]).
//! The builder works in view coordinates (x and y in the parallel view plane, z the depth toward
//! the viewer) as for a parallel view; in perspective, or with clipping planes, each primitive's
//! points then go through a [`Lens`]: lines and triangles are clipped at the near plane and the
//! front and back clipping planes, so nothing behind the eye or outside the planes draws, and in
//! perspective what is left is seen through the eye onto the target plane. The list holds
//! target-plane points with screen-linear depths ([`cadcraft_geom::Persp::map`]), so it draws,
//! picks and depth-tests as a parallel view's does.

use cadcraft_geom::{Mat3, Persp, Vec2, Vec3, ViewDir};

use crate::{Builder, Ctx, DPrim, Ink, Kind};

/// How a view sees view-coordinates points when it isn't a plain parallel view: through a
/// perspective camera (`persp`) and/or between clipping planes. Only depths (view z) from `lo`
/// to `hi` are seen: the back and front planes, the near plane in perspective.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Lens {
    pub(crate) persp: Option<Persp>,
    pub(crate) lo: f64,
    pub(crate) hi: f64,
}

impl Lens {
    /// The lens of view `v`; `None` for a parallel view without clipping planes.
    pub(crate) fn of(v: &ViewDir) -> Option<Lens> {
        if v.persp.is_none() && v.clip.is_off() {
            return None;
        }
        let (lo, hi) = v.clip.range();
        Some(Lens { persp: v.persp, lo, hi: v.persp.map_or(hi, |c| hi.min(clip_depth(&c))) })
    }
    /// Where the list keeps view-coordinates point `v` and its depth: through the eye in
    /// perspective; `None` outside the depths seen.
    pub(crate) fn map(&self, v: Vec3) -> Option<(Vec2, f64)> {
        if !(self.lo..=self.hi).contains(&v.z) {
            return None;
        }
        match &self.persp {
            Some(c) => c.map(v),
            None => v.is_finite().then(|| (v.xy(), v.z)),
        }
    }
    /// The part of segment `a`–`b` (view coordinates) at the depths seen, ends cut exactly onto
    /// the plane they are cut at, and whether each end was cut. `None` when none of it is seen.
    fn cut(&self, a: Vec3, b: Vec3) -> Option<(Vec3, Vec3, bool, bool)> {
        if !(a.z.is_finite() && b.z.is_finite()) {
            return None;
        }
        let d = b.z - a.z;
        let (mut t0, mut t1, mut z0, mut z1) = (0.0, 1.0, None, None);
        if d == 0.0 {
            if !(self.lo..=self.hi).contains(&a.z) {
                return None;
            }
        } else {
            let (tl, th) = ((self.lo - a.z) / d, (self.hi - a.z) / d);
            let (enter, exit, ze, zx) = if d > 0.0 { (tl, th, self.lo, self.hi) } else { (th, tl, self.hi, self.lo) };
            if enter > 0.0 {
                (t0, z0) = (enter, Some(ze));
            }
            if exit < 1.0 {
                (t1, z1) = (exit, Some(zx));
            }
        }
        if t0 > t1 {
            return None;
        }
        let at = |t: f64, z: Option<f64>, end: Vec3| match z {
            Some(z) => {
                let p = a + (b - a) * t;
                Vec3::new(p.x, p.y, z)
            }
            None => end,
        };
        Some((at(t0, z0, a), at(t1, z1, b), z0.is_some(), z1.is_some()))
    }
}

impl Builder<'_> {
    /// The view's [`Lens`]: primitives go through it when it has one.
    pub(crate) fn lens(&self) -> Option<Lens> {
        Lens::of(&self.opts.view_dir)
    }

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

    /// A polyline through view-coordinates points `v` seen through `c`: its pieces it sees.
    pub(crate) fn persp_polyline(&mut self, ctx: &Ctx, c: &Lens, color: Ink, lw: f32, v: &[Vec3]) {
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
    /// the planes it sees between.
    pub(crate) fn persp_tris(&mut self, ctx: &Ctx, c: &Lens, color: Ink, v: &[Vec3]) {
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

    /// A point marker at view-coordinates point `v`, unless `c` doesn't see it.
    pub(crate) fn persp_point(&mut self, ctx: &Ctx, c: &Lens, color: Ink, v: Vec3) {
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

    /// A line without end at target-plane point `q` (depth `depth`) along unit `d2`, a ray from
    /// it when `ray`.
    fn push_infinite(&mut self, ctx: &Ctx, color: Ink, (q, depth): (Vec2, f64), d2: Vec2, ray: bool) {
        let Ok(start) = u32::try_from(self.list.verts.len()) else { return };
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
    }

    /// An infinite line (a ray: from `base` on) through view-coordinates point `base` along
    /// `dir`, seen through `c`. Square to the line of sight it is a line without end at its depth
    /// (if `c` sees that depth); else what `c` sees of it runs between the planes it crosses (the
    /// near and front planes, the back plane, the ray's base) and, where nothing ends it, in
    /// perspective to the vanishing point of its direction away from the eye, in a clipped
    /// parallel view on without end.
    pub(crate) fn persp_infinite(&mut self, ctx: &Ctx, c: &Lens, color: Ink, base: Vec3, dir: Vec3, ray: bool) {
        let n = dir.len();
        if !(n > 0.0 && n.is_finite() && base.is_finite()) {
            return;
        }
        let d = dir * (1.0 / n);
        if d.z.abs() < 1e-9 {
            let d2 = d.xy().normalized();
            if let Some(m) = c.map(base)
                && d2 != Vec2::ZERO
            {
                self.push_infinite(ctx, color, m, d2, ray);
            }
            return;
        }
        // The parameters where the line crosses the planes, and the part between them seen.
        let (tl, th) = ((c.lo - base.z) / d.z, (c.hi - base.z) / d.z);
        let (t0, t1) = (tl.min(th), tl.max(th));
        let t0 = if ray { t0.max(0.0) } else { t0 };
        if t0 > t1 {
            return;
        }
        // A finite end exactly on the plane it is cut at; an endless one (going away from the
        // eye: e.z < 0) at its direction's vanishing point in perspective.
        let end = |t: f64| -> Option<(Vec2, f64)> {
            if t.is_finite() {
                let p = base + d * t;
                let z = if t == tl {
                    c.lo
                } else if t == th {
                    c.hi
                } else {
                    p.z
                };
                return c.map(Vec3::new(p.x, p.y, z));
            }
            let e = d * t.signum();
            c.persp.filter(|_| e.z < 0.0).map(|p| (p.eye.xy() + e.xy() * (p.focal / -e.z), p.eye.z - 2.0 * p.focal))
        };
        if c.persp.is_none() && !(t0.is_finite() && t1.is_finite()) {
            // A clipped parallel view: a ray from the plane it crosses, away from it.
            let (from, sign) = if t0.is_finite() { (t0, 1.0) } else { (t1, -1.0) };
            let d2 = (d.xy() * sign).normalized();
            match end(from) {
                Some(m) if d2 != Vec2::ZERO => self.push_infinite(ctx, color, m, d2, true),
                Some(m) => self.push_line(ctx, color, 0.0, &[m, m], false),
                None => {}
            }
            return;
        }
        if let (Some(a), Some(b)) = (end(t0), end(t1))
            && a.0.is_finite()
            && b.0.is_finite()
        {
            self.push_line(ctx, color, 0.0, &[a, b], false);
        }
    }
}

/// View `v`'s clipping planes as depths the way lists and pictures keep them ([`Persp::map`]'s
/// in perspective, view z in parallel): the depths kept, from the back plane's to the front's
/// (infinite where a plane is off).
pub(crate) fn kept_depths(v: &ViewDir) -> (f64, f64) {
    let (lo, hi) = v.clip.range();
    let Some(c) = v.persp else { return (lo, hi) };
    // A plane at or behind the near plane maps nowhere: a back plane there keeps nothing in
    // front of the eye, a front plane there limits nothing.
    let depth = |z: f64| if z.is_finite() { c.map(Vec3::new(c.eye.x, c.eye.y, z)).map_or(f64::INFINITY, |m| m.1) } else { z };
    (depth(lo), depth(hi))
}

/// The depth lines and triangles are cut at: a hair in front of the near plane, so the points
/// on the cut map whatever the rounding.
pub(crate) fn clip_depth(c: &Persp) -> f64 {
    c.eye.z - c.near() * (1.0 + 1e-3)
}

/// The pieces of polyline `v` (view coordinates) that `c` sees, as the list keeps them:
/// target-plane points with their depths.
pub(crate) fn clip_line(c: &Lens, v: &[Vec3]) -> Vec<Vec<(Vec2, f64)>> {
    let mut runs = Vec::new();
    let mut cur: Vec<(Vec2, f64)> = Vec::with_capacity(v.len());
    let end_run = |cur: &mut Vec<(Vec2, f64)>, runs: &mut Vec<Vec<(Vec2, f64)>>| {
        let run = std::mem::take(cur);
        if run.len() >= 2 {
            runs.push(run);
        }
    };
    for w in v.windows(2) {
        let [a, b] = w else { continue };
        let Some((p, q, entered, left)) = c.cut(*a, *b) else {
            end_run(&mut cur, &mut runs);
            continue;
        };
        if entered {
            end_run(&mut cur, &mut runs);
        }
        if cur.is_empty() {
            cur.extend(c.map(p));
        }
        cur.extend(c.map(q));
        if left {
            end_run(&mut cur, &mut runs);
        }
    }
    end_run(&mut cur, &mut runs);
    runs
}

/// Polygon `poly` (view coordinates) cut to the depths on one side of `bound`: at most `bound`
/// (`below`), else at least.
fn cut_poly(poly: Vec<Vec3>, bound: f64, below: bool) -> Vec<Vec3> {
    if bound.is_infinite() {
        return poly;
    }
    let inside = |p: &Vec3| if below { p.z <= bound } else { p.z >= bound };
    let mut out = Vec::with_capacity(poly.len() + 1);
    for (i, a) in poly.iter().enumerate() {
        let Some(b) = poly.get((i + 1) % poly.len()) else { continue };
        if inside(a) {
            out.push(*a);
        }
        if inside(a) != inside(b) {
            let x = *a + (*b - *a) * ((bound - a.z) / (b.z - a.z));
            out.push(Vec3::new(x.x, x.y, bound));
        }
    }
    out
}

/// Triangle `t` (view coordinates) clipped to what `c` sees and mapped as the list keeps it, as
/// triangles (target-plane corners with their depths) added to `out`.
pub(crate) fn clip_tri(c: &Lens, t: [Vec3; 3], out: &mut Vec<(Vec2, f64)>) {
    let poly = cut_poly(cut_poly(t.to_vec(), c.hi, true), c.lo, false);
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

    fn lens() -> Lens {
        Lens::of(&persp().0).unwrap()
    }

    #[test]
    fn lines_through_the_eye_plane_are_cut_at_the_near_plane() {
        let c = lens();
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
        let c = lens();
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

    /// DVIEW CLip: a parallel view with clipping planes draws only what is between them (lines
    /// crossing a plane are cut there, lines without end become segments or rays), and a
    /// perspective view's back plane hides what is farther.
    #[test]
    fn clipping_planes_cut_parallel_and_perspective_lists() {
        use cadcraft_doc::{Drawing, EntityKind, Line, RayLine, Space};
        use cadcraft_geom::Clip;
        let mut d = Drawing::default();
        let mut add = |k| d.add(&Space::Model, Default::default(), k).unwrap();
        let line = |a: Vec3, b: Vec3| EntityKind::Line(Line { a, b });
        // Front view: the depth toward the viewer is -y. Posts at depths 20, 0 and -20, a line
        // running from depth 30 to -30, and an xline along the line of sight and one across it.
        let near = add(line(Vec3::new(0.0, -20.0, 0.0), Vec3::new(0.0, -20.0, 10.0)));
        let mid = add(line(Vec3::new(5.0, 0.0, 0.0), Vec3::new(5.0, 0.0, 10.0)));
        let far = add(line(Vec3::new(9.0, 20.0, 0.0), Vec3::new(9.0, 20.0, 10.0)));
        let through = add(line(Vec3::new(-5.0, -30.0, 5.0), Vec3::new(-5.0, 30.0, 5.0)));
        let along = add(EntityKind::XLine(RayLine { base: Vec3::new(2.0, 0.0, 2.0), dir: Vec3::new(0.0, 1.0, 1.0) }));
        let across = add(EntityKind::XLine(RayLine { base: Vec3::new(0.0, 25.0, 3.0), dir: Vec3::new(1.0, 0.0, 0.0) }));
        let front = ViewDir::named("front").unwrap();
        let build = |dir: ViewDir| crate::build(&d, &Space::Model, &crate::Options { view_dir: dir, ..Default::default() });
        let has = |list: &crate::DisplayList, h| list.prims.iter().any(|p| p.handle == h);
        let list = build(ViewDir { clip: Clip::new(Some(10.0), Some(-10.0)), ..front });
        assert!(!has(&list, near) && has(&list, mid) && !has(&list, far) && !has(&list, across));
        // The xline rising 45° away from the viewer is cut at the planes, 20 apart in depth: 20
        // high on screen.
        let cut = list.prims.iter().find(|p| p.handle == along).unwrap();
        assert_eq!(cut.kind, Kind::Polyline, "an xline crossing both planes is a segment");
        let pts = list.points(cut);
        assert!((pts[0].y - pts[1].y).abs() > 19.9 && (pts[0].y - pts[1].y).abs() < 20.1, "{pts:?}");
        assert!(has(&list, through));
        // Only a front plane: the xline along the line of sight runs on from it without end.
        let list = build(ViewDir { clip: Clip::new(Some(10.0), None), ..front });
        let ray = list.prims.iter().find(|p| p.handle == along).unwrap();
        assert_eq!(ray.kind, Kind::Infinite { ray: true });
        assert!(has(&list, far) && has(&list, across) && !has(&list, near));
        // Unclipped, everything draws as before.
        let list = build(front);
        assert!([near, mid, far, through, along, across].iter().all(|h| has(&list, *h)));
        // Perspective from 100 in front: the back plane hides the far post.
        let cam = Camera::new(50.0, 0.0);
        let p = ViewDir { clip: Clip::new(None, Some(-10.0)), ..front }.with_camera(Some(cam), Vec2::new(0.0, 5.0), cam.height_at(100.0));
        let list = build(p);
        assert!(has(&list, near) && has(&list, mid) && !has(&list, far) && !has(&list, across) && has(&list, along));
        assert!(list.verts.iter().all(|q| q.is_finite()));
    }

    /// A closed solid cut open by a clipping plane shows its inside: its back faces draw there.
    #[test]
    fn a_clipped_solid_shows_its_inside() {
        use cadcraft_doc::{Drawing, EntityKind, Solid3d, Space};
        use cadcraft_geom::{Clip, Mesh3};
        let v = |i: u32| Vec3::new(f64::from(i & 1), f64::from((i >> 1) & 1), f64::from((i >> 2) & 1)) * 10.0;
        let corners =
            [[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
        let verts = corners.iter().flatten().map(|&i| v(i)).collect();
        let mesh = Mesh3 { verts, tris: (0..12).map(|t| [3 * t, 3 * t + 1, 3 * t + 2]).collect(), edges: vec![], open: false };
        let mut d = Drawing::default();
        d.add(&Space::Model, Default::default(), EntityKind::Solid3d(Solid3d { mesh: std::sync::Arc::new(mesh), ..Default::default() })).unwrap();
        let front = ViewDir::named("front").unwrap();
        let tris = |dir: ViewDir| crate::build(&d, &Space::Model, &crate::Options { view_dir: dir, shaded: true, ..Default::default() }).tris.len();
        // Its front face, two triangles; cut through the middle, the front face is gone and the
        // back face shows from inside instead of nothing.
        assert_eq!(tris(front), 6);
        assert_eq!(tris(ViewDir { clip: Clip::new(Some(-5.0), None), ..front }), 6);
        assert_eq!(tris(ViewDir { clip: Clip::new(Some(-11.0), None), ..front }), 0, "all of it in front of the plane");
    }
}
