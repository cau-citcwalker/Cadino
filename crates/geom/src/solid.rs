//! 3D solids as data, view directions and perspective cameras.
//!
//! A [`Mesh3`] is what the document keeps of a solid for drawing: triangles for shading and
//! edge polylines for wireframe. A [`ViewDir`] maps world points to the view plane; for points
//! on the WCS XY plane its parallel projection is the 2D affine [`ViewDir::plane_xf`], which is
//! how the 2D pipeline draws rotated views. A view with a [`Persp`] camera projects through the
//! eye onto its target plane instead (DVIEW Distance, Rhino's Perspective view): view-plane
//! coordinates are then the target plane's, where sizes are true.

use serde::{Deserialize, Serialize};

use crate::{Bounds2, Mat3, Mat4, Vec2, Vec3};

/// A tessellated solid: triangles (indices into `verts`) and the B-rep edges as polylines.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Mesh3 {
    pub verts: Vec<Vec3>,
    pub tris: Vec<[u32; 3]>,
    pub edges: Vec<Vec<Vec3>>,
    /// Not a closed solid (a surface or an open shell): both sides of its faces show, so none is
    /// culled as a back face.
    #[serde(default)]
    pub open: bool,
}

impl Mesh3 {
    /// The triangle's corners; `None` when an index is out of range (hostile files).
    pub fn tri(&self, t: &[u32; 3]) -> Option<[Vec3; 3]> {
        let p = |i: u32| self.verts.get(i as usize).copied();
        Some([p(t[0])?, p(t[1])?, p(t[2])?])
    }

    /// Every point of the mesh and its edges.
    pub fn points(&self) -> impl Iterator<Item = Vec3> + '_ {
        self.verts.iter().copied().chain(self.edges.iter().flatten().copied())
    }

    /// Plan-view bounds after `xf`.
    pub fn bounds_xy(&self, xf: &Mat4) -> Bounds2 {
        Bounds2::from_points(self.points().map(|p| xf.apply(p).xy()))
    }
}

/// A view direction (the unit vector from the target toward the viewer), its twist and clipping
/// planes and, for a perspective view, its camera.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewDir {
    pub dir: Vec3,
    /// The perspective camera; `None`: parallel projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persp: Option<Persp>,
    /// DVIEW TWist (VIEWTWIST, DXF VPORT 51): the picture turned counter-clockwise about the line
    /// of sight, in radians ([`Self::axes`]); 0 keeps world Z up the screen.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub twist: f64,
    /// DVIEW CLip: the front and back clipping planes.
    #[serde(default, skip_serializing_if = "Clip::is_off")]
    pub clip: Clip,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// DVIEW CLip's clipping planes, square to the line of sight, as depths along the view direction
/// ([`ViewDir::depth`]): nothing nearer than `front` or farther than `back` shows. `None`: that
/// plane is off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    #[serde(default)]
    pub front: Option<f64>,
    #[serde(default)]
    pub back: Option<f64>,
}

impl Clip {
    pub const OFF: Clip = Clip { front: None, back: None };

    /// The planes at depths `front` and `back`; one that isn't a finite depth is off.
    pub fn new(front: Option<f64>, back: Option<f64>) -> Clip {
        Clip { front: front.filter(|f| f.is_finite()), back: back.filter(|b| b.is_finite()) }
    }
    pub fn is_off(&self) -> bool {
        self.front.is_none() && self.back.is_none()
    }
    /// The depths kept: from the back plane to the front one (infinite where a plane is off).
    pub fn range(&self) -> (f64, f64) {
        (self.back.unwrap_or(f64::NEG_INFINITY), self.front.unwrap_or(f64::INFINITY))
    }
    /// Whether a point at depth `d` is between the planes (with any on, NaN is not).
    pub fn keeps(&self, d: f64) -> bool {
        let (lo, hi) = self.range();
        self.is_off() || (lo..=hi).contains(&d)
    }
}

/// A perspective view's eye in view coordinates ([`ViewDir::view_of`]: x right and y up in the
/// view plane, z the depth toward the viewer). A point projects through the eye onto the target
/// plane, `focal` in front of the eye, where its view-plane coordinates are measured.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Persp {
    pub eye: Vec3,
    /// From the eye to the target plane (> 0).
    pub focal: f64,
}

/// The height of a 35 mm camera's frame in millimetres: a lens length `L` sees a view `h` high
/// from `h * L / FILM` away (50 mm: about 27° high).
pub const FILM: f64 = 24.0;

/// Points nearer the eye than this fraction of the focal distance are clipped away.
pub const NEAR: f64 = 1e-2;

/// What a perspective view keeps besides its direction and 2D view (centre and height): the lens
/// length and the depth of its target plane along the direction. With the view's centre and
/// height it places the eye ([`ViewDir::with_camera`]), so zooming dollies the eye toward the
/// target plane and panning moves eye and target together.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    /// Lens length in millimetres on a 35 mm frame ([`FILM`]).
    pub lens: f64,
    /// The depth ([`ViewDir::depth`]) of the target plane.
    pub target: f64,
}

impl Camera {
    /// AutoCAD's and Rhino's default lens length.
    pub const LENS: f64 = 50.0;
    /// Lens lengths from 1 mm (very wide) to 100 m (nearly parallel).
    pub const LENS_MIN: f64 = 1.0;
    pub const LENS_MAX: f64 = 100_000.0;

    /// A camera with lens `lens` (clamped to [`Self::LENS_MIN`]..=[`Self::LENS_MAX`], 50 when not
    /// a number) whose target plane is at depth `target` (0 when not finite).
    pub fn new(lens: f64, target: f64) -> Camera {
        let lens = if lens.is_finite() { lens.clamp(Self::LENS_MIN, Self::LENS_MAX) } else { Self::LENS };
        Camera { lens, target: if target.is_finite() { target } else { 0.0 } }
    }
    /// The distance from the eye to the target plane for a view `height` high.
    pub fn focal(&self, height: f64) -> f64 {
        height * self.lens / FILM
    }
    /// The view height that puts the eye `distance` from the target plane.
    pub fn height_at(&self, distance: f64) -> f64 {
        distance * FILM / self.lens.max(1e-9)
    }
    /// The vertical field of view (radians).
    pub fn field_of_view(&self) -> f64 {
        2.0 * (FILM / 2.0 / self.lens.max(1e-9)).atan()
    }
}

impl Persp {
    /// How far in front of the eye the near clipping plane is.
    pub fn near(&self) -> f64 {
        self.focal * NEAR
    }
    /// View-coordinates point `v` (x, y, depth) seen through the eye: its target-plane point and
    /// a depth for depth buffers, `eye.z - 2 focal + focal² / w` (`w`: how far in front of the eye
    /// it is). That depth is larger nearer, keeps the target plane's depth (and its slope there),
    /// and varies linearly across the screen over any plane, so depth buffers interpolate it.
    /// `None` nearer than [`Self::near`] or behind the eye.
    pub fn map(&self, v: Vec3) -> Option<(Vec2, f64)> {
        let w = self.eye.z - v.z;
        // NaN fails too.
        if w.partial_cmp(&self.near()).is_none_or(|o| o.is_lt()) {
            return None;
        }
        let k = self.focal / w;
        let q = Vec2::new(self.eye.x + (v.x - self.eye.x) * k, self.eye.y + (v.y - self.eye.y) * k);
        let depth = self.eye.z - 2.0 * self.focal + self.focal * k;
        (q.is_finite() && depth.is_finite()).then_some((q, depth))
    }
    /// The view-coordinates point that [`Self::map`] takes to target-plane point `q` at depth
    /// `depth`.
    pub fn unmap(&self, q: Vec2, depth: f64) -> Vec3 {
        let k = (depth - self.eye.z + 2.0 * self.focal) / self.focal;
        let k = if k.abs() > 1e-300 && k.is_finite() { k } else { 1.0 };
        Vec3::new(self.eye.x + (q.x - self.eye.x) / k, self.eye.y + (q.y - self.eye.y) / k, self.eye.z - self.focal / k)
    }
}

impl Default for ViewDir {
    fn default() -> Self {
        ViewDir::PLAN
    }
}

impl ViewDir {
    /// Looking down the Z axis (plan view of the WCS).
    pub const PLAN: ViewDir = ViewDir { dir: Vec3::Z, persp: None, twist: 0.0, clip: Clip::OFF };

    /// A direction from its angle in the XY plane (from +X) and its angle above the XY plane,
    /// as VPOINT's Rotate option takes them. Radians.
    pub fn from_angles(azimuth: f64, elevation: f64) -> ViewDir {
        let (ca, sa, ce, se) = (azimuth.cos(), azimuth.sin(), elevation.cos(), elevation.sin());
        ViewDir::new(Vec3::new(ce * ca, ce * sa, se))
    }

    /// The direction toward the viewer `d`, normalised; a zero or non-finite `d` gives plan.
    pub fn new(d: Vec3) -> ViewDir {
        let n = d.normalized();
        if n == Vec3::ZERO || !n.is_finite() { ViewDir::PLAN } else { ViewDir { dir: n, ..ViewDir::PLAN } }
    }

    /// The same direction in parallel projection.
    pub fn parallel(self) -> ViewDir {
        ViewDir { persp: None, ..self }
    }

    /// The same direction seen through `camera` (parallel for `None`) by a view centred on
    /// `center` (in the view plane) and `height` high: the eye is on the centre's line of sight,
    /// [`Camera::focal`] in front of the target plane.
    pub fn with_camera(self, camera: Option<Camera>, center: Vec2, height: f64) -> ViewDir {
        let persp = camera.and_then(|c| {
            let focal = c.focal(height);
            let eye = center.to3(c.target + focal);
            (focal > 0.0 && focal.is_finite() && eye.is_finite()).then_some(Persp { eye, focal })
        });
        ViewDir { persp, ..self }
    }

    /// The camera of a perspective view `height` high ([`Self::with_camera`] undone); `None` in
    /// parallel projection.
    pub fn camera(&self, height: f64) -> Option<Camera> {
        self.persp.map(|c| Camera::new(c.focal * FILM / height, c.eye.z - c.focal))
    }

    /// World point `p` in view coordinates: x right and y up in the view plane, z its depth toward
    /// the viewer (the parallel projection with its depth).
    pub fn view_of(&self, p: Vec3) -> Vec3 {
        let (x, y, d) = self.axes();
        Vec3::new(x.dot(p), y.dot(p), d.dot(p))
    }

    /// The world point at view coordinates `v` ([`Self::view_of`] inverted).
    pub fn world_of(&self, v: Vec3) -> Vec3 {
        let (x, y, d) = self.axes();
        x * v.x + y * v.y + d * v.z
    }

    /// The eye of a perspective view, in the world.
    pub fn eye(&self) -> Option<Vec3> {
        self.persp.map(|c| self.world_of(c.eye))
    }

    /// The unit direction from world point `p` toward the viewer: the view direction, or in
    /// perspective toward the eye.
    pub fn toward(&self, p: Vec3) -> Vec3 {
        let t = self.eye().map(|e| (e - p).normalized()).unwrap_or(self.dir);
        if t == Vec3::ZERO || !t.is_finite() { self.dir } else { t }
    }

    /// The line of sight through view-plane point `v`: a world point on it (the eye, in
    /// perspective) and its unit direction away from the viewer.
    pub fn ray(&self, v: Vec2) -> (Vec3, Vec3) {
        match self.persp {
            Some(c) => {
                let eye = self.world_of(c.eye);
                let d = (self.world_of(v.to3(c.eye.z - c.focal)) - eye).normalized();
                (eye, if d == Vec3::ZERO || !d.is_finite() { -self.dir } else { d })
            }
            None => (self.world_of(v.to3(0.0)), -self.dir),
        }
    }

    /// Where the line of sight through view-plane point `v` meets the plane through `o` with
    /// normal `n`: `None` where it runs along the plane, or in perspective meets it behind the
    /// eye.
    pub fn ray_hit(&self, v: Vec2, o: Vec3, n: Vec3) -> Option<Vec3> {
        let (p0, d) = self.ray(v);
        let nd = n.dot(d);
        if nd.abs().partial_cmp(&(1e-9 * n.len())).is_none_or(|o| o.is_lt()) {
            return None;
        }
        let t = n.dot(o - p0) / nd;
        if self.persp.is_some() && t <= 0.0 {
            return None;
        }
        let p = p0 + d * t;
        p.is_finite().then_some(p)
    }

    /// Whether the view sees world point `p`: between its clipping planes and, in perspective, in
    /// front of the near plane.
    pub fn sees(&self, p: Vec3) -> bool {
        self.persp.is_none_or(|c| c.eye.z - self.dir.dot(p) >= c.near()) && self.clip.keeps(self.depth(p))
    }

    /// The depths ([`Self::depth`]) the view sees: between its clipping planes, and in perspective
    /// in front of the near plane (infinite where nothing limits them).
    pub fn depth_range(&self) -> (f64, f64) {
        let (lo, hi) = self.clip.range();
        (lo, self.persp.map_or(hi, |c| hi.min(c.eye.z - c.near())))
    }

    /// World segment `a`–`b` cut to what the view sees ([`Self::sees`]); `None` when it sees none
    /// of it.
    pub fn clip(&self, a: Vec3, b: Vec3) -> Option<(Vec3, Vec3)> {
        let (lo, hi) = self.depth_range();
        if lo == f64::NEG_INFINITY && hi == f64::INFINITY {
            return Some((a, b));
        }
        // A front plane behind the back one (or behind the eye's near plane) leaves nothing.
        if lo.partial_cmp(&hi).is_none_or(std::cmp::Ordering::is_gt) {
            return None;
        }
        let (da, db) = (self.depth(a), self.depth(b));
        if !(da.is_finite() && db.is_finite()) {
            return None;
        }
        // The part at depths `da + (db - da) t` in [lo, hi], t in [0, 1].
        let dd = db - da;
        let (t0, t1) = if dd == 0.0 {
            if !(lo..=hi).contains(&da) {
                return None;
            }
            (0.0, 1.0)
        } else {
            let (p, q) = ((lo - da) / dd, (hi - da) / dd);
            (p.min(q).max(0.0), p.max(q).min(1.0))
        };
        if t0 > t1 {
            return None;
        }
        let at = |t: f64| {
            if t <= 0.0 {
                a
            } else if t >= 1.0 {
                b
            } else {
                a + (b - a) * t
            }
        };
        Some((at(t0), at(t1)))
    }

    /// Angle in the XY plane from +X, and angle above the XY plane (radians). Looking straight
    /// down or up the azimuth is the one plan view continues into: from the south (-90°), where
    /// +X stays to the right.
    pub fn angles(&self) -> (f64, f64) {
        let d = self.dir;
        let az = if d.x.abs() < 1e-12 && d.y.abs() < 1e-12 { -std::f64::consts::FRAC_PI_2 } else { d.y.atan2(d.x) };
        (az, d.z.clamp(-1.0, 1.0).asin())
    }

    /// A standard view by name, any case: `plan`/`top`, `front` (from -Y), `back`, `left` (from
    /// -X), `right`, `iso` (south-west isometric), `iso_se`, `iso_ne`, `iso_nw`.
    pub fn named(name: &str) -> Option<ViewDir> {
        let d = match name.to_ascii_lowercase().as_str() {
            "plan" | "top" => return Some(ViewDir::PLAN),
            "front" => Vec3::new(0.0, -1.0, 0.0),
            "back" => Vec3::new(0.0, 1.0, 0.0),
            "left" => Vec3::new(-1.0, 0.0, 0.0),
            "right" => Vec3::new(1.0, 0.0, 0.0),
            "iso" => Vec3::new(-1.0, -1.0, 1.0),
            "iso_se" => Vec3::new(1.0, -1.0, 1.0),
            "iso_ne" => Vec3::new(1.0, 1.0, 1.0),
            "iso_nw" => Vec3::new(-1.0, 1.0, 1.0),
            _ => return None,
        };
        Some(ViewDir::new(d))
    }

    /// Looking straight down world Z in parallel projection, upright and unclipped: AutoCAD's 2D
    /// drawing area. A perspective, twisted or clipped view is a 3D view (picks take what it
    /// draws).
    pub fn is_plan(&self) -> bool {
        self.persp.is_none() && self.twist == 0.0 && self.clip.is_off() && self.dir.x.abs() < 1e-12 && self.dir.y.abs() < 1e-12 && self.dir.z > 0.0
    }

    /// The view's right and up axes and the direction toward the viewer, in world coordinates.
    /// Upright, up is the world Z axis projected on the view plane and, looking straight down or
    /// up, right is +X, so plan view is the identity. [`Self::twist`] turns both clockwise in the
    /// view plane, so the picture turns counter-clockwise on the screen.
    pub fn axes(&self) -> (Vec3, Vec3, Vec3) {
        let d = self.dir;
        let x = Vec3::Z.cross(d).normalized();
        let x = if x == Vec3::ZERO { Vec3::new(1.0, 0.0, 0.0) } else { x };
        let y = d.cross(x).normalized();
        if self.twist == 0.0 || !self.twist.is_finite() {
            return (x, y, d);
        }
        let (s, c) = self.twist.sin_cos();
        (x * c - y * s, x * s + y * c, d)
    }

    /// The view-plane position of a world point; in perspective, where the line from it to the eye
    /// meets the target plane. A point the view doesn't see ([`Self::sees`]) is put as if on the
    /// near plane: on the side it is on, not mirrored, so check `sees` (or [`Self::clip`]
    /// segments) before hit-testing what this gives.
    pub fn project(&self, p: Vec3) -> Vec2 {
        let v = self.view_of(p);
        match self.persp {
            None => v.xy(),
            Some(c) => {
                let k = c.focal / (c.eye.z - v.z).max(c.near());
                Vec2::new(c.eye.x + (v.x - c.eye.x) * k, c.eye.y + (v.y - c.eye.y) * k)
            }
        }
    }

    /// How far toward the viewer a world point is along the view direction (larger = nearer).
    pub fn depth(&self, p: Vec3) -> f64 {
        self.dir.dot(p)
    }

    /// The parallel projection of the WCS XY plane (z = 0) as a 2D affine transform (perspective
    /// isn't affine: [`Self::project`] takes it into account).
    pub fn plane_xf(&self) -> Mat3 {
        let (x, y, _) = self.axes();
        Mat3 { a: x.x, b: y.x, c: x.y, d: y.y, e: 0.0, f: 0.0 }
    }

    /// The world XY point (z = 0) seen at view-plane point `v`. Where the view sees the XY plane
    /// edge-on (or nearly), or in perspective above the horizon, the parallel view's
    /// least-squares one: the point on the plane's projected line.
    pub fn unproject_xy(&self, v: Vec2) -> Vec2 {
        if self.persp.is_some()
            && let Some(p) = self.ray_hit(v, Vec3::ZERO, Vec3::Z)
        {
            return p.xy();
        }
        let m = self.plane_xf();
        if m.det().abs() > 1e-6
            && let Some(i) = m.inverse()
        {
            return i.apply(v);
        }
        // Rank one: the pseudo-inverse is the transpose over the squared Frobenius norm.
        let n2 = m.a * m.a + m.b * m.b + m.c * m.c + m.d * m.d;
        if n2 < 1e-24 {
            return v;
        }
        Vec2::new((m.a * v.x + m.b * v.y) / n2, (m.c * v.x + m.d * v.y) / n2)
    }

    /// The world → view rotation as a 4x4 transform (rows: right, up, toward the viewer).
    pub fn rotation(&self) -> Mat4 {
        let (x, y, d) = self.axes();
        Mat4 { m: [[x.x, x.y, x.z, 0.0], [y.x, y.y, y.z, 0.0], [d.x, d.y, d.z, 0.0], [0.0, 0.0, 0.0, 1.0]] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: Vec2, b: Vec2) -> bool {
        a.dist(b) < 1e-12
    }

    #[test]
    fn a_perspectives_camera_comes_back_from_its_projection() {
        let cam = Camera::new(35.0, -120.0);
        let v = ViewDir::named("iso").unwrap().with_camera(Some(cam), Vec2::new(3.0, -4.0), 250.0);
        let back = v.camera(250.0).unwrap();
        assert!((back.lens - 35.0).abs() < 1e-9 && (back.target + 120.0).abs() < 1e-9);
        assert_eq!(v.parallel().camera(250.0), None);
    }

    #[test]
    fn segments_are_cut_at_the_near_plane() {
        let v = ViewDir::named("front").unwrap().with_camera(Some(Camera::new(50.0, 0.0)), Vec2::ZERO, 24.0);
        // The eye is 50 in front of the target plane (y = -50 in the world); the near plane 0.5
        // in front of it.
        let (a, b) = (Vec3::new(0.0, 10.0, 0.0), Vec3::new(0.0, -100.0, 0.0));
        let (p, q) = v.clip(a, b).unwrap();
        assert_eq!(p, a);
        assert!((q.y + 49.5).abs() < 1e-9 && v.sees(p));
        assert_eq!(v.clip(b, b + Vec3::new(1.0, -1.0, 0.0)), None);
        let flat = ViewDir::named("front").unwrap();
        assert_eq!(flat.clip(a, b), Some((a, b)));
    }

    #[test]
    fn plan_is_identity() {
        let v = ViewDir::PLAN;
        assert!(v.is_plan());
        assert_eq!(v.plane_xf(), Mat3::IDENTITY);
        assert!(near(v.project(Vec3::new(3.0, 4.0, 5.0)), Vec2::new(3.0, 4.0)));
    }

    #[test]
    fn front_view_shows_x_right_z_up() {
        let v = ViewDir::new(Vec3::new(0.0, -1.0, 0.0));
        assert!(near(v.project(Vec3::new(2.0, 7.0, 3.0)), Vec2::new(2.0, 3.0)));
        assert!(v.depth(Vec3::new(0.0, -5.0, 0.0)) > v.depth(Vec3::ZERO));
    }

    #[test]
    fn plane_xf_matches_projection_of_xy_points() {
        let v = ViewDir::from_angles(-135f64.to_radians(), 35.264f64.to_radians());
        let p = Vec2::new(1.5, -2.0);
        assert!(near(v.plane_xf().apply(p), v.project(p.to3(0.0))));
        let (az, el) = v.angles();
        assert!((az - -135f64.to_radians()).abs() < 1e-12 && (el - 35.264f64.to_radians()).abs() < 1e-12);
    }

    #[test]
    fn plan_continues_into_the_view_from_the_south() {
        let (az, el) = ViewDir::PLAN.angles();
        let tilted = ViewDir::from_angles(az, el - 0.1);
        // Tilting plan keeps +X to the right.
        assert!(tilted.project(Vec3::new(1.0, 0.0, 0.0)).x > 0.99);
    }

    #[test]
    fn unproject_inverts_project_and_handles_edge_on_views() {
        let iso = ViewDir::new(Vec3::new(-1.0, -1.0, 1.0));
        let p = Vec2::new(3.0, -4.0);
        assert!(near(iso.unproject_xy(iso.project(p.to3(0.0))), p));
        // Front: view x is world x, world y is lost (0).
        let front = ViewDir::new(Vec3::new(0.0, -1.0, 0.0));
        assert!(near(front.unproject_xy(Vec2::new(5.0, 2.0)), Vec2::new(5.0, 0.0)));
        // Left: view x runs along -Y.
        let left = ViewDir::new(Vec3::new(-1.0, 0.0, 0.0));
        assert!(near(left.unproject_xy(Vec2::new(5.0, 2.0)), Vec2::new(0.0, -5.0)));
    }

    #[test]
    fn named_views() {
        assert!(ViewDir::named("TOP").is_some_and(|v| v.is_plan()) && ViewDir::named("plan") == ViewDir::named("top"));
        assert_eq!(ViewDir::named("front"), Some(ViewDir::new(Vec3::new(0.0, -1.0, 0.0))));
        assert_eq!(ViewDir::named("Iso"), Some(ViewDir::new(Vec3::new(-1.0, -1.0, 1.0))));
        assert_eq!(ViewDir::named("iso_ne"), Some(ViewDir::new(Vec3::new(1.0, 1.0, 1.0))));
        // Seen from the right, +Y runs to the right.
        assert!(ViewDir::named("right").is_some_and(|v| v.project(Vec3::new(0.0, 1.0, 0.0)).x > 0.99));
        assert!(ViewDir::named("current").is_none() && ViewDir::named("").is_none());
    }

    #[test]
    fn bad_direction_falls_back_to_plan() {
        assert!(ViewDir::new(Vec3::ZERO).is_plan());
        assert!(ViewDir::new(Vec3::new(f64::NAN, 0.0, 1.0)).is_plan());
    }

    #[test]
    fn hostile_triangle_index_is_none() {
        let m = Mesh3 { verts: vec![Vec3::ZERO], tris: vec![[0, 0, 9]], edges: vec![], open: false };
        assert!(m.tri(&m.tris[0]).is_none());
    }

    /// A south-west perspective looking at the origin from 100 away, 40 high.
    fn persp() -> ViewDir {
        let v = ViewDir::named("iso").unwrap();
        let cam = Camera::new(50.0, 0.0);
        v.with_camera(Some(cam), Vec2::ZERO, cam.height_at(100.0))
    }

    #[test]
    fn a_perspective_camera_sits_on_the_line_of_sight() {
        let v = persp();
        let eye = v.eye().unwrap();
        assert!((eye.len() - 100.0).abs() < 1e-9 && (eye.normalized() - v.dir).len() < 1e-12);
        // The target plane keeps sizes; nearer is larger, farther smaller.
        let (_, up, _) = v.axes();
        assert!((v.project(up * 10.0).y - 10.0).abs() < 1e-9);
        assert!(v.project(up * 10.0 + v.dir * 50.0).y > 19.9);
        assert!(v.project(up * 10.0 - v.dir * 100.0).y < 5.01);
        // 50 mm sees about 27° high: the view is FILM/lens of the distance high.
        let cam = Camera::new(50.0, 0.0);
        assert!((cam.field_of_view().to_degrees() - 26.99).abs() < 0.01 && (cam.focal(cam.height_at(7.0)) - 7.0).abs() < 1e-12);
        // Parallel again drops the camera; a hostile lens is clamped.
        assert!(v.parallel().persp.is_none() && Camera::new(f64::NAN, f64::INFINITY) == Camera::new(50.0, 0.0));
        assert_eq!(Camera::new(0.0, 1.0).lens, Camera::LENS_MIN);
    }

    #[test]
    fn perspective_projection_round_trips_through_its_depth() {
        let v = persp();
        let c = v.persp.unwrap();
        for p in [Vec3::new(3.0, -4.0, 5.0), Vec3::new(-20.0, 7.0, -11.0), Vec3::ZERO] {
            let (q, depth) = c.map(v.view_of(p)).unwrap();
            assert!(q.dist(v.project(p)) < 1e-9);
            assert!((v.world_of(c.unmap(q, depth)) - p).len() < 1e-9, "{p:?}");
        }
        // Behind the eye (and too near it) nothing is seen.
        let behind = v.dir * 150.0;
        assert!(c.map(v.view_of(behind)).is_none() && !v.sees(behind) && v.sees(Vec3::ZERO));
        // Depths order points along a line of sight, nearer larger.
        let (o, d) = v.ray(Vec2::new(2.0, 1.0));
        let z = |t: f64| c.map(v.view_of(o + d * t)).unwrap().1;
        assert!(z(10.0) > z(50.0) && z(50.0) > z(500.0));
    }

    #[test]
    fn perspective_rays_pick_where_the_view_shows_a_point() {
        let v = persp();
        for p in [Vec3::new(3.0, -4.0, 0.0), Vec3::new(12.0, 30.0, 0.0)] {
            // The XY point under its projection is itself.
            assert!(v.unproject_xy(v.project(p)).dist(p.xy()) < 1e-9);
            // The line of sight through its projection passes through it.
            let (o, d) = v.ray(v.project(p));
            let t = (p - o).dot(d);
            assert!((o + d * t - p).len() < 1e-9);
            assert!(v.ray_hit(v.project(p), Vec3::ZERO, Vec3::Z).is_some_and(|h| (h - p).len() < 1e-9));
        }
        // A plane behind the eye isn't hit; toward points at the eye.
        assert!(v.ray_hit(Vec2::ZERO, v.dir * 200.0, v.dir).is_none());
        assert!((v.toward(Vec3::ZERO) - v.dir).len() < 1e-12);
        // Parallel rays run along the view direction.
        let par = v.parallel();
        assert!((par.ray(Vec2::new(1.0, 2.0)).1 + par.dir).len() < 1e-12);
    }

    /// A twist turns the picture counter-clockwise about the line of sight: twisted 90°, plan
    /// shows +X up the screen. A twisted plan is a 3D view; the camera and rays follow the twist.
    #[test]
    fn a_twist_turns_the_picture_counter_clockwise() {
        let v = ViewDir { twist: std::f64::consts::FRAC_PI_2, ..ViewDir::PLAN };
        assert!(near(v.project(Vec3::new(1.0, 0.0, 0.0)), Vec2::new(0.0, 1.0)));
        assert!(near(v.project(Vec3::new(0.0, 1.0, 0.0)), Vec2::new(-1.0, 0.0)));
        assert!(!v.is_plan() && ViewDir::PLAN.is_plan());
        let p = Vec2::new(3.0, -4.0);
        assert!(near(v.unproject_xy(v.project(p.to3(0.0))), p));
        // Twisted 30° from the south-west, world Z leans 30° to the left of up.
        let iso = ViewDir { twist: 30f64.to_radians(), ..ViewDir::named("iso").unwrap() };
        let z = iso.project(Vec3::Z);
        assert!((z.y.atan2(z.x).to_degrees() - 120.0).abs() < 1e-9, "{z:?}");
        let cam = Camera::new(50.0, 0.0);
        let persp = iso.with_camera(Some(cam), Vec2::new(2.0, 1.0), 40.0);
        assert!((persp.twist - iso.twist).abs() < 1e-15 && persp.camera(40.0).is_some());
        let q = Vec3::new(5.0, -2.0, 3.0);
        let (o, d) = persp.ray(persp.project(q));
        assert!((o + d * (q - o).dot(d) - q).len() < 1e-9);
        // A hostile twist keeps the view upright rather than producing NaN axes.
        let bad = ViewDir { twist: f64::NAN, ..ViewDir::PLAN };
        assert!(near(bad.project(Vec3::new(1.0, 2.0, 0.0)), Vec2::new(1.0, 2.0)));
    }

    /// Clipping planes hide what is nearer than the front one and farther than the back one:
    /// points and segments (snaps, grips, picks) see only what lies between.
    #[test]
    fn clipping_planes_cut_what_the_view_sees() {
        let front = ViewDir::named("front").unwrap();
        // Front view: the depth toward the viewer is -y.
        let v = ViewDir { clip: Clip::new(Some(5.0), Some(-10.0)), ..front };
        assert!(v.sees(Vec3::ZERO) && !v.sees(Vec3::new(0.0, -6.0, 0.0)) && !v.sees(Vec3::new(0.0, 11.0, 0.0)));
        assert!(!v.is_plan() && !v.sees(Vec3::new(f64::NAN, 0.0, 0.0)));
        let (a, b) = v.clip(Vec3::new(0.0, -20.0, 0.0), Vec3::new(0.0, 20.0, 0.0)).unwrap();
        assert!((a.y + 5.0).abs() < 1e-12 && (b.y - 10.0).abs() < 1e-12);
        // Wholly in front of the front plane, or square to the line of sight behind the back one.
        assert_eq!(v.clip(Vec3::new(0.0, -7.0, 0.0), Vec3::new(1.0, -9.0, 3.0)), None);
        assert_eq!(v.clip(Vec3::new(0.0, 12.0, 0.0), Vec3::new(5.0, 12.0, 0.0)), None);
        let inside = (Vec3::new(0.0, 1.0, 0.0), Vec3::new(4.0, -1.0, 2.0));
        assert_eq!(v.clip(inside.0, inside.1), Some(inside));
        // In perspective the near plane still cuts what the front plane would keep.
        let cam = Camera::new(50.0, 0.0);
        let p = ViewDir { clip: Clip::new(Some(1e9), None), ..front }.with_camera(Some(cam), Vec2::ZERO, cam.height_at(100.0));
        assert!(p.depth_range().1 < 100.0 && !p.sees(Vec3::new(0.0, -150.0, 0.0)));
        // Planes that aren't finite depths are off.
        assert!(Clip::new(Some(f64::NAN), Some(f64::INFINITY)).is_off());
        // A front plane behind the back one sees nothing, so nothing is cut out for snaps.
        let inverted = ViewDir { clip: Clip::new(Some(-100.0), Some(50.0)), ..front };
        assert_eq!(inverted.clip(Vec3::new(0.0, -200.0, 0.0), Vec3::new(0.0, 200.0, 0.0)), None);
        assert!(!inverted.sees(Vec3::ZERO));
        // So does a back plane behind the eye in perspective.
        let behind = ViewDir { clip: Clip::new(None, Some(1e9)), ..p };
        assert_eq!(behind.clip(Vec3::new(0.0, -200.0, 0.0), Vec3::new(0.0, 200.0, 0.0)), None);
    }
}
