use serde::{Deserialize, Serialize};

use crate::{Vec2, Vec3};

/// A 2D affine transform `[a c e; b d f; 0 0 1]` (column vectors, like SVG/PDF).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mat3 {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Mat3 {
    fn default() -> Self {
        Mat3::IDENTITY
    }
}

impl Mat3 {
    pub const IDENTITY: Mat3 = Mat3 { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub fn translate(v: Vec2) -> Mat3 {
        Mat3 { e: v.x, f: v.y, ..Mat3::IDENTITY }
    }
    pub fn scale(sx: f64, sy: f64) -> Mat3 {
        Mat3 { a: sx, d: sy, ..Mat3::IDENTITY }
    }
    pub fn rotate(angle: f64) -> Mat3 {
        let (s, c) = angle.sin_cos();
        Mat3 { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 }
    }
    /// Scale about `base`.
    pub fn scale_about(base: Vec2, s: f64) -> Mat3 {
        Mat3::translate(base).then_before(Mat3::scale(s, s)).then_before(Mat3::translate(-base))
    }
    pub fn rotate_about(base: Vec2, angle: f64) -> Mat3 {
        Mat3::translate(base).then_before(Mat3::rotate(angle)).then_before(Mat3::translate(-base))
    }
    /// Reflection across the line through `p1`, `p2`.
    pub fn mirror(p1: Vec2, p2: Vec2) -> Mat3 {
        let d = (p2 - p1).normalized();
        if d == Vec2::ZERO {
            return Mat3::IDENTITY;
        }
        let (c2, s2) = (d.x * d.x - d.y * d.y, 2.0 * d.x * d.y);
        let r = Mat3 { a: c2, b: s2, c: s2, d: -c2, e: 0.0, f: 0.0 };
        Mat3::translate(p1).then_before(r).then_before(Mat3::translate(-p1))
    }
    /// `self * o`: apply `o` first, then `self`.
    pub fn then_before(self, o: Mat3) -> Mat3 {
        Mat3 {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }
    /// Apply `self` first, then `o`.
    pub fn then(self, o: Mat3) -> Mat3 {
        o.then_before(self)
    }
    pub fn apply(&self, p: Vec2) -> Vec2 {
        Vec2::new(self.a * p.x + self.c * p.y + self.e, self.b * p.x + self.d * p.y + self.f)
    }
    pub fn apply_vec(&self, v: Vec2) -> Vec2 {
        Vec2::new(self.a * v.x + self.c * v.y, self.b * v.x + self.d * v.y)
    }
    pub fn det(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }
    pub fn inverse(&self) -> Option<Mat3> {
        let det = self.det();
        if det.abs() < 1e-300 || !det.is_finite() {
            return None;
        }
        let id = 1.0 / det;
        let a = self.d * id;
        let b = -self.b * id;
        let c = -self.c * id;
        let d = self.a * id;
        let e = -(a * self.e + c * self.f);
        let f = -(b * self.e + d * self.f);
        Some(Mat3 { a, b, c, d, e, f })
    }
    /// Uniform scale factor (geometric mean of the axis scales).
    pub fn scale_factor(&self) -> f64 {
        self.det().abs().sqrt()
    }
    /// Rotation angle of the transformed X axis.
    pub fn rotation(&self) -> f64 {
        crate::norm_angle(self.b.atan2(self.a))
    }
    pub fn is_mirroring(&self) -> bool {
        self.det() < 0.0
    }
    /// Rotation, uniform scale and mirror (and translation) only: no shear or stretch, not
    /// singular.
    pub fn is_similarity(&self) -> bool {
        let (p, q) = (self.a * self.a + self.b * self.b, self.c * self.c + self.d * self.d);
        let tol = 1e-9 * (p + q);
        p > 1e-300 && p.is_finite() && q.is_finite() && (p - q).abs() <= tol && (self.a * self.c + self.b * self.d).abs() <= tol
    }
}

/// A 4x4 transform, row-major, for 3D views and block transforms with elevation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mat4 {
    pub m: [[f64; 4]; 4],
}

impl Default for Mat4 {
    fn default() -> Self {
        Mat4::IDENTITY
    }
}

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4 { m: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]] };
    pub fn mul(&self, o: &Mat4) -> Mat4 {
        let mut r = [[0.0; 4]; 4];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = (0..4).map(|k| self.m[i][k] * o.m[k][j]).sum();
            }
        }
        Mat4 { m: r }
    }
    pub fn apply(&self, p: Vec3) -> Vec3 {
        let m = &self.m;
        let w = m[3][0] * p.x + m[3][1] * p.y + m[3][2] * p.z + m[3][3];
        let w = if w.abs() < 1e-300 { 1.0 } else { w };
        Vec3::new(
            (m[0][0] * p.x + m[0][1] * p.y + m[0][2] * p.z + m[0][3]) / w,
            (m[1][0] * p.x + m[1][1] * p.y + m[1][2] * p.z + m[1][3]) / w,
            (m[2][0] * p.x + m[2][1] * p.y + m[2][2] * p.z + m[2][3]) / w,
        )
    }
    pub fn from_mat3(t: &Mat3) -> Mat4 {
        Mat4 { m: [[t.a, t.c, 0.0, t.e], [t.b, t.d, 0.0, t.f], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]] }
    }
    /// The "arbitrary axis algorithm" frame for an extrusion direction (DXF OCS → WCS).
    pub fn ocs(normal: Vec3) -> Mat4 {
        let n = normal.normalized();
        if n == Vec3::ZERO || (n.x.abs() < 1e-12 && n.y.abs() < 1e-12 && n.z > 0.0) {
            return Mat4::IDENTITY;
        }
        let ax = if n.x.abs() < 1.0 / 64.0 && n.y.abs() < 1.0 / 64.0 { Vec3::new(0.0, 1.0, 0.0).cross(n) } else { Vec3::Z.cross(n) }.normalized();
        let ay = n.cross(ax).normalized();
        Mat4 { m: [[ax.x, ay.x, n.x, 0.0], [ax.y, ay.y, n.y, 0.0], [ax.z, ay.z, n.z, 0.0], [0.0, 0.0, 0.0, 1.0]] }
    }

    /// The frame with axes `x`, `y`, `z` and origin `o` (local → world).
    pub fn from_cols(x: Vec3, y: Vec3, z: Vec3, o: Vec3) -> Mat4 {
        Mat4 { m: [[x.x, y.x, z.x, o.x], [x.y, y.y, z.y, o.y], [x.z, y.z, z.z, o.z], [0.0, 0.0, 0.0, 1.0]] }
    }
    /// A construction plane (CPLANE, UCS): the orthonormal frame at `o` whose x axis runs along
    /// `x` and whose y axis leans toward `y` (made perpendicular to x), z = x × y; local → world.
    /// `None` when a vector is zero or not finite or `x` and `y` are parallel.
    pub fn frame(o: Vec3, x: Vec3, y: Vec3) -> Option<Mat4> {
        if !(o.is_finite() && x.is_finite() && y.is_finite()) {
            return None;
        }
        let (xn, yn) = (x.normalized(), y.normalized());
        let z = xn.cross(yn);
        if z.len() < 1e-9 {
            return None;
        }
        let z = z.normalized();
        Some(Mat4::from_cols(xn, z.cross(xn).normalized(), z, o))
    }
    pub fn translate(v: Vec3) -> Mat4 {
        Mat4::from_cols(Vec3::X, Vec3::Y, Vec3::Z, v)
    }
    pub fn scale(s: Vec3) -> Mat4 {
        Mat4::from_cols(Vec3::X * s.x, Vec3::Y * s.y, Vec3::Z * s.z, Vec3::ZERO)
    }
    /// Rotation by `angle` radians about `axis` through the origin, counter-clockwise looking
    /// down the axis (right-hand rule); the identity for a zero axis.
    pub fn rotation(axis: Vec3, angle: f64) -> Mat4 {
        let k = axis.normalized();
        if k == Vec3::ZERO || !angle.is_finite() {
            return Mat4::IDENTITY;
        }
        let (s, c) = angle.sin_cos();
        // Rodrigues: v cos + (k × v) sin + k (k·v)(1 - cos), applied to the unit axes.
        let r = |v: Vec3| v * c + k.cross(v) * s + k * (k.dot(v) * (1.0 - c));
        Mat4::from_cols(r(Vec3::X), r(Vec3::Y), r(Vec3::Z), Vec3::ZERO)
    }
    /// [`Mat4::rotation`] about the axis through `base`.
    pub fn rotate_about(base: Vec3, axis: Vec3, angle: f64) -> Mat4 {
        Mat4::translate(base).mul(&Mat4::rotation(axis, angle)).mul(&Mat4::translate(-base))
    }
    /// Reflection in the plane through `p` with normal `n`; the identity for a zero normal.
    pub fn mirror(p: Vec3, n: Vec3) -> Mat4 {
        let n = n.normalized();
        let r = |v: Vec3| v - n * (2.0 * n.dot(v));
        Mat4::translate(p).mul(&Mat4::from_cols(r(Vec3::X), r(Vec3::Y), r(Vec3::Z), Vec3::ZERO)).mul(&Mat4::translate(-p))
    }
    /// Column `i` of the linear part (the image of axis `i`); the translation for `i == 3`.
    pub fn col(&self, i: usize) -> Vec3 {
        let i = i.min(3);
        Vec3::new(self.m[0][i], self.m[1][i], self.m[2][i])
    }
    /// The linear part applied to a direction (no translation).
    pub fn apply_vec(&self, v: Vec3) -> Vec3 {
        self.col(0) * v.x + self.col(1) * v.y + self.col(2) * v.z
    }
    /// Determinant of the linear (3x3) part: negative when the transform mirrors.
    pub fn det3(&self) -> f64 {
        self.col(0).cross(self.col(1)).dot(self.col(2))
    }
    /// Every entry finite and the last row `0 0 0 1` (no perspective).
    pub fn is_affine(&self) -> bool {
        self.m.iter().flatten().all(|v| v.is_finite()) && self.m[3] == [0.0, 0.0, 0.0, 1.0]
    }
    /// The inverse of an affine transform; `None` when singular or not affine.
    pub fn inverse(&self) -> Option<Mat4> {
        let det = self.det3();
        if !self.is_affine() || !det.is_finite() || det.abs() < 1e-300 {
            return None;
        }
        // Rows of the inverse linear part are the cross products of the columns over det.
        let (a, b, c) = (self.col(0), self.col(1), self.col(2));
        let (r0, r1, r2) = (b.cross(c) * (1.0 / det), c.cross(a) * (1.0 / det), a.cross(b) * (1.0 / det));
        let t = self.col(3);
        let m = [[r0.x, r0.y, r0.z, -r0.dot(t)], [r1.x, r1.y, r1.z, -r1.dot(t)], [r2.x, r2.y, r2.z, -r2.dot(t)], [0.0, 0.0, 0.0, 1.0]];
        let inv = Mat4 { m };
        inv.m.iter().flatten().all(|v| v.is_finite()).then_some(inv)
    }
    /// The scale factor of a similarity (rotation, uniform scale, optional mirror and
    /// translation); `None` for shear, stretch, a singular or a non-affine transform.
    pub fn similarity_scale(&self) -> Option<f64> {
        if !self.is_affine() {
            return None;
        }
        let (a, b, c) = (self.col(0), self.col(1), self.col(2));
        let (la, lb, lc) = (a.dot(a), b.dot(b), c.dot(c));
        let tol = 1e-9 * (la + lb + lc);
        let ok = la > 1e-300
            && (la - lb).abs() <= tol
            && (la - lc).abs() <= tol
            && a.dot(b).abs() <= tol
            && b.dot(c).abs() <= tol
            && a.dot(c).abs() <= tol;
        ok.then(|| la.sqrt())
    }
    /// What the transform does to the z = 0 plane, seen in the result's XY: rows x and y,
    /// columns x, y and translation. For a view rotation this is the view of the XY plane; for
    /// an object's plane, its plan view.
    pub fn plane_xy(&self) -> Mat3 {
        let m = &self.m;
        Mat3 { a: m[0][0], b: m[1][0], c: m[0][1], d: m[1][1], e: m[0][3], f: m[1][3] }
    }
}
