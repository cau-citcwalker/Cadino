//! 3D solids as data, and orthographic view directions.
//!
//! A [`Mesh3`] is what the document keeps of a solid for drawing: triangles for shading and
//! edge polylines for wireframe. A [`ViewDir`] maps world points to the view plane; for points
//! on the WCS XY plane that map is the 2D affine [`ViewDir::plane_xf`], which is how the 2D
//! pipeline draws rotated views.

use serde::{Deserialize, Serialize};

use crate::{Bounds2, Mat3, Mat4, Vec2, Vec3};

/// A tessellated solid: triangles (indices into `verts`) and the B-rep edges as polylines.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Mesh3 {
    pub verts: Vec<Vec3>,
    pub tris: Vec<[u32; 3]>,
    pub edges: Vec<Vec<Vec3>>,
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

/// An orthographic view direction: the unit vector from the target toward the viewer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewDir {
    pub dir: Vec3,
}

impl Default for ViewDir {
    fn default() -> Self {
        ViewDir::PLAN
    }
}

impl ViewDir {
    /// Looking down the Z axis (plan view of the WCS).
    pub const PLAN: ViewDir = ViewDir { dir: Vec3::Z };

    /// A direction from its angle in the XY plane (from +X) and its angle above the XY plane,
    /// as VPOINT's Rotate option takes them. Radians.
    pub fn from_angles(azimuth: f64, elevation: f64) -> ViewDir {
        let (ca, sa, ce, se) = (azimuth.cos(), azimuth.sin(), elevation.cos(), elevation.sin());
        ViewDir::new(Vec3::new(ce * ca, ce * sa, se))
    }

    /// The direction toward the viewer `d`, normalised; a zero or non-finite `d` gives plan.
    pub fn new(d: Vec3) -> ViewDir {
        let n = d.normalized();
        if n == Vec3::ZERO || !n.is_finite() { ViewDir::PLAN } else { ViewDir { dir: n } }
    }

    /// Angle in the XY plane from +X, and angle above the XY plane (radians). Looking straight
    /// down or up the azimuth is the one plan view continues into: from the south (-90°), where
    /// +X stays to the right.
    pub fn angles(&self) -> (f64, f64) {
        let d = self.dir;
        let az = if d.x.abs() < 1e-12 && d.y.abs() < 1e-12 { -std::f64::consts::FRAC_PI_2 } else { d.y.atan2(d.x) };
        (az, d.z.clamp(-1.0, 1.0).asin())
    }

    pub fn is_plan(&self) -> bool {
        self.dir.x.abs() < 1e-12 && self.dir.y.abs() < 1e-12 && self.dir.z > 0.0
    }

    /// The view's right and up axes and the direction toward the viewer, in world coordinates.
    /// Up is the world Z axis projected on the view plane; looking straight down or up, right
    /// is +X, so plan view is the identity.
    pub fn axes(&self) -> (Vec3, Vec3, Vec3) {
        let d = self.dir;
        let x = Vec3::Z.cross(d).normalized();
        let x = if x == Vec3::ZERO { Vec3::new(1.0, 0.0, 0.0) } else { x };
        (x, d.cross(x).normalized(), d)
    }

    /// The view-plane position of a world point.
    pub fn project(&self, p: Vec3) -> Vec2 {
        let (x, y, _) = self.axes();
        Vec2::new(x.dot(p), y.dot(p))
    }

    /// How far toward the viewer a world point is (larger = nearer).
    pub fn depth(&self, p: Vec3) -> f64 {
        self.dir.dot(p)
    }

    /// The view projection of the WCS XY plane (z = 0) as a 2D affine transform.
    pub fn plane_xf(&self) -> Mat3 {
        let (x, y, _) = self.axes();
        Mat3 { a: x.x, b: y.x, c: x.y, d: y.y, e: 0.0, f: 0.0 }
    }

    /// The world XY point (z = 0) seen at view-plane point `v`. Where the view sees the XY plane
    /// edge-on (or nearly), the least-squares one: the point on the plane's projected line.
    pub fn unproject_xy(&self, v: Vec2) -> Vec2 {
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
    fn bad_direction_falls_back_to_plan() {
        assert!(ViewDir::new(Vec3::ZERO).is_plan());
        assert!(ViewDir::new(Vec3::new(f64::NAN, 0.0, 1.0)).is_plan());
    }

    #[test]
    fn hostile_triangle_index_is_none() {
        let m = Mesh3 { verts: vec![Vec3::ZERO], tris: vec![[0, 0, 9]], edges: vec![] };
        assert!(m.tri(&m.tris[0]).is_none());
    }
}
