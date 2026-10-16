//! Objects in 3D, the AutoCAD way: a planar object keeps 2D coordinates in its own plane, the
//! object coordinate system (OCS) of its extrusion direction (`Common::extrusion`, DXF 210, by
//! the arbitrary axis algorithm) raised by its elevation (the z of its points, or its own
//! elevation field). With the default extrusion the OCS is the WCS, so 2D drawings are what
//! they always were. Lines, 3D polylines, points, 3D faces, rays, helices and solids are true 3D
//! objects with world points instead.
//!
//! Plan-view picking, snaps, grips and extents see an object's plane from above:
//! [`Entity::prims`], [`Entity::grips`] and the bounds project it to the world XY plane (a
//! circle standing in the XZ plane is a line in plan).

use cadcraft_geom::{Ellipse as GEllipse, Line as GLine, Mat3, Mat4, Segment, Vec2, Vec3};

use super::{Entity, EntityKind, Prim};

/// Tolerance on an elevation that cannot be stored (a region's or wipeout's).
const FLAT: f64 = 1e-9;

/// How an object lives in 3D.
enum Place {
    /// In the plane of its extrusion (OCS) at its elevation.
    Ocs,
    /// In a horizontal plane at its elevation; its extrusion only gives thickness a direction.
    /// ponytail: dimensions, leaders, multilines, multileaders, viewports, images, tables and
    /// tolerances stay horizontal: a tilted extrusion read from a file is kept and written back
    /// (DIMENSION, LEADER, MLINE and TOLERANCE carry DXF 210; MULTILEADER, IMAGE, ACAD_TABLE and
    /// VIEWPORT have none) but they draw, pick and snap level at their elevation, and 3D
    /// transforms that would tilt them are refused. Upgrade: give them `Ocs` once their 2D code
    /// takes OCS points (DXF stores most of theirs in the WCS).
    Level,
    /// In the world XY plane (it stores no elevation).
    /// ponytail: regions and wipeouts have no elevation field, so they stay at z = 0 and 3D
    /// transforms that lift or tilt them are refused; upgrade: an elevation (and extrusion) on
    /// `Region` and `Wipeout`, written as DXF 210 / the wipeout's 3D frame.
    Ground,
    /// World points.
    Free,
}

fn place(k: &EntityKind) -> Place {
    match k {
        EntityKind::Circle(_)
        | EntityKind::Arc(_)
        | EntityKind::Ellipse(_)
        | EntityKind::LwPolyline(_)
        | EntityKind::Spline(_)
        | EntityKind::Text(_)
        | EntityKind::AttDef(_)
        | EntityKind::MText(_)
        | EntityKind::Insert(_)
        | EntityKind::Solid(_)
        | EntityKind::Trace(_)
        | EntityKind::Hatch(_) => Place::Ocs,
        EntityKind::Line(l) if l.a.z == l.b.z => Place::Level,
        EntityKind::Dimension(_)
        | EntityKind::MLine(_)
        | EntityKind::Leader(_)
        | EntityKind::MLeader(_)
        | EntityKind::Viewport(_)
        | EntityKind::Image(_)
        | EntityKind::Table(_)
        | EntityKind::Tolerance(_) => Place::Level,
        EntityKind::Region(_) | EntityKind::Wipeout(_) => Place::Ground,
        EntityKind::Line(_)
        | EntityKind::Point(_)
        | EntityKind::Polyline3d(_)
        | EntityKind::Face3d(_)
        | EntityKind::Ray(_)
        | EntityKind::XLine(_)
        | EntityKind::Solid3d(_)
        | EntityKind::Helix(_)
        | EntityKind::Unknown(_) => Place::Free,
    }
}

impl EntityKind {
    /// The height of a planar object's plane along its extrusion (OCS z); 0 for others.
    pub fn elevation(&self) -> f64 {
        let z = match self {
            EntityKind::Line(l) => l.a.z,
            EntityKind::Circle(c) => c.center.z,
            EntityKind::Arc(a) => a.center.z,
            EntityKind::Ellipse(e) => e.center.z,
            EntityKind::LwPolyline(p) => p.elevation,
            EntityKind::Spline(s) => s.elevation,
            EntityKind::Text(t) => t.insert.z,
            EntityKind::AttDef(a) => a.text.insert.z,
            EntityKind::MText(t) => t.insert.z,
            EntityKind::Insert(i) => i.insert.z,
            EntityKind::Dimension(d) => d.defpt.z,
            EntityKind::MLine(m) => m.vertices.first().map_or(0.0, |v| v.p.z),
            EntityKind::Leader(l) => l.vertices.first().map_or(0.0, |v| v.z),
            EntityKind::MLeader(m) => m.landing.z,
            EntityKind::Hatch(h) => h.elevation,
            EntityKind::Solid(s) | EntityKind::Trace(s) => s.corners[0].z,
            EntityKind::Viewport(v) => v.center.z,
            EntityKind::Image(i) => i.insert.z,
            EntityKind::Table(t) => t.insert.z,
            EntityKind::Tolerance(t) => t.insert.z,
            _ => 0.0,
        };
        if z.is_finite() { z } else { 0.0 }
    }

    /// Put a planar object's plane at elevation `z` (every point's z). Nothing for others.
    pub fn set_elevation(&mut self, z: f64) {
        let set = |p: &mut Vec3| p.z = z;
        match self {
            EntityKind::Line(l) => {
                l.a.z = z;
                l.b.z = z;
            }
            EntityKind::Circle(c) => set(&mut c.center),
            EntityKind::Arc(a) => set(&mut a.center),
            EntityKind::Ellipse(e) => {
                set(&mut e.center);
                e.major.z = 0.0;
            }
            EntityKind::LwPolyline(p) => p.elevation = z,
            EntityKind::Spline(s) => s.elevation = z,
            EntityKind::Text(t) => text_z(t, z),
            EntityKind::AttDef(a) => attrib_z(a, z),
            EntityKind::MText(t) => set(&mut t.insert),
            EntityKind::Insert(i) => {
                set(&mut i.insert);
                i.attribs.iter_mut().for_each(|a| attrib_z(a, z));
            }
            EntityKind::Dimension(d) => {
                for p in [&mut d.defpt, &mut d.text_mid, &mut d.p13, &mut d.p14, &mut d.p15, &mut d.p16] {
                    set(p);
                }
                d.marks.jog_line.iter_mut().for_each(set);
                if let Some(j) = &mut d.marks.jogged {
                    set(&mut j.center);
                    set(&mut j.jog);
                }
                d.marks.breaks.iter_mut().for_each(|b| set(&mut b.at));
            }
            EntityKind::MLine(m) => m.vertices.iter_mut().for_each(|v| set(&mut v.p)),
            EntityKind::Leader(l) => l.vertices.iter_mut().for_each(set),
            EntityKind::MLeader(m) => {
                m.leaders.iter_mut().flatten().for_each(set);
                set(&mut m.landing);
                m.text.iter_mut().for_each(|t| set(&mut t.insert));
                m.block.iter_mut().for_each(|b| set(&mut b.insert));
            }
            EntityKind::Hatch(h) => h.elevation = z,
            EntityKind::Solid(s) | EntityKind::Trace(s) => s.corners.iter_mut().for_each(set),
            EntityKind::Viewport(v) => set(&mut v.center),
            EntityKind::Image(i) => set(&mut i.insert),
            EntityKind::Table(t) => set(&mut t.insert),
            EntityKind::Tolerance(t) => set(&mut t.insert),
            _ => {}
        }
    }
}

fn text_z(t: &mut super::Text, z: f64) {
    t.insert.z = z;
    if let Some(a) = &mut t.align_pt {
        a.z = z;
    }
}

fn attrib_z(a: &mut super::Attrib, z: f64) {
    text_z(&mut a.text, z);
    if let Some(m) = &mut a.multiline {
        m.mtext.insert.z = z;
    }
}

/// Exactly the WCS: the default extrusion (or one too close to it to tell apart).
fn is_wcs(f: &Mat4) -> bool {
    *f == Mat4::IDENTITY
}

impl Entity {
    /// The object's own plane: its OCS → WCS transform raised by its elevation, so plane point
    /// `(x, y, 0)` (its 2D coordinates) is world point `plane.apply(..)`. `None` for true 3D
    /// objects: a line whose ends differ in z, 3D polylines, points, 3D faces, rays, xlines,
    /// helices and solids (and proxies).
    pub fn plane(&self) -> Option<Mat4> {
        let lift = Mat4::translate(Vec3::Z * self.kind.elevation());
        match place(&self.kind) {
            Place::Ocs => Some(Mat4::ocs(self.common.extrusion).mul(&lift)),
            Place::Level => Some(lift),
            Place::Ground => Some(Mat4::IDENTITY),
            Place::Free => None,
        }
    }

    /// The plan view of the object's plane when it is not the XY plane's (a tilted, standing or
    /// upside-down OCS): its 2D coordinates → world XY. `None` when its 2D coordinates are world
    /// XY already.
    pub fn plan_xf(&self) -> Option<Mat3> {
        let tilted = matches!(place(&self.kind), Place::Ocs) && !is_wcs(&Mat4::ocs(self.common.extrusion));
        self.plane().filter(|_| tilted).map(|p| p.plane_xy())
    }

    /// [`EntityKind::prims`] in world XY: an object in a tilted or standing plane is seen from
    /// above (its circles are ellipses, or segments when the plane is vertical).
    pub fn prims(&self) -> Vec<Prim> {
        let prims = self.kind.prims();
        let Some(a) = self.plan_xf() else { return prims };
        let mut out = Vec::with_capacity(prims.len());
        for p in prims {
            project(p, &a, &mut out);
        }
        out
    }

    /// The point of the object's own plane (its 2D coordinates) seen at plan point `to`: the
    /// inverse of [`Entity::plan_xf`], for grips dragged in plan. A plane seen edge-on from above
    /// takes the smallest move from `near` (a point of the plane, such as the dragged grip) that
    /// lands under `to`. `to` itself for objects in the XY plane's orientation.
    pub fn plan_to_local(&self, to: Vec2, near: Vec2) -> Vec2 {
        let Some(a) = self.plan_xf() else { return to };
        let d = to - a.apply(near);
        // The (pseudo-)inverse of the plan view's linear part [a c; b d] applied to `d`.
        let n2 = a.a * a.a + a.b * a.b + a.c * a.c + a.d * a.d;
        let det = a.det();
        let back = if det.abs() > 1e-9 * n2 {
            Vec2::new(a.d * d.x - a.c * d.y, a.a * d.y - a.b * d.x) * (1.0 / det)
        } else if n2 > 1e-300 {
            Vec2::new(a.a * d.x + a.b * d.y, a.c * d.x + a.d * d.y) * (1.0 / n2)
        } else {
            Vec2::ZERO
        };
        let q = near + back;
        if q.is_finite() { q } else { near }
    }

    /// [`EntityKind::grips`] in world XY (see [`Entity::prims`]).
    pub fn grips(&self) -> Vec<Vec2> {
        let g = self.kind.grips();
        match self.plan_xf() {
            Some(a) => g.into_iter().map(|p| a.apply(p)).collect(),
            None => g,
        }
    }

    /// The object in the coordinates of construction plane `cplane` (CPLANE, UCS: plane
    /// coordinates → world), as picks, snaps and grips there see it through [`Entity::prims`]
    /// and [`Entity::grips`]: the plan view generalised to any plane. The object itself for
    /// World (the identity). `None` when the plane can't be inverted or the object can't be
    /// expressed in it ([`Entity::transform3`]: dimensions, leaders, tables... stay level, so a
    /// tilted plane can't hold them).
    pub fn on(&self, cplane: &Mat4) -> Option<Entity> {
        if *cplane == Mat4::IDENTITY {
            return Some(self.clone());
        }
        let mut e = self.clone();
        e.transform3(&cplane.inverse()?).then_some(e)
    }

    /// [`Entity::prims`] seen in construction plane `cplane` ([`Entity::on`]): plan for World,
    /// nothing when the object can't be expressed in the plane.
    pub fn prims_on(&self, cplane: &Mat4) -> Vec<Prim> {
        if *cplane == Mat4::IDENTITY {
            return self.prims();
        }
        self.on(cplane).map(|e| e.prims()).unwrap_or_default()
    }

    /// [`Entity::grips`] seen in construction plane `cplane` (see [`Entity::prims_on`]).
    pub fn grips_on(&self, cplane: &Mat4) -> Vec<Vec2> {
        if *cplane == Mat4::IDENTITY {
            return self.grips();
        }
        self.on(cplane).map(|e| e.grips()).unwrap_or_default()
    }

    /// A 2D transform of the world XY plane (MOVE, ROTATE, SCALE, MIRROR in plan), z scaled like
    /// a solid's (by the square root of the determinant): a uniform SCALE scales heights,
    /// elevations and thicknesses too, as AutoCAD's does, whatever an object's plane; MOVE,
    /// ROTATE and MIRROR keep them. Objects in the XY plane's orientation change in plan as
    /// [`EntityKind::transform`] changes them; one in a tilted plane goes through
    /// [`Entity::transform3`]. False when that can't be represented.
    pub fn transform(&mut self, m: &Mat3) -> bool {
        let mut t = Mat4::from_mat3(m);
        t.m[2][2] = m.det().abs().sqrt();
        if self.plan_xf().is_some() {
            return self.transform3(&t);
        }
        if (t.m[2][2] - 1.0).abs() > 1e-12 && m.is_similarity() && self.transform3(&t) {
            return true;
        }
        self.kind.transform(m);
        // A line's or point's thickness direction turns with it (the default one stays).
        if matches!(self.kind, EntityKind::Line(_) | EntityKind::Point(_)) && self.common.extrusion != Vec3::Z {
            thicken(&mut self.common, &t);
        }
        true
    }

    /// Apply a 3D affine transform. True 3D objects transform their points; a solid composes its
    /// transform (similarities only: uniform scale, rotation, mirror, translation); a planar
    /// object gets the transformed plane (a new extrusion and elevation) and the in-plane
    /// transform of its 2D coordinates. False, leaving the object as it was, when the result
    /// can't be represented: a shear or non-uniform scale of a circle, arc, text or block in its
    /// plane, a plane collapsed to a line, an object kept horizontal (dimensions, leaders,
    /// tables...) turned out of the horizontal, a region or wipeout lifted off z = 0, a helix
    /// whose axis leaves the vertical, a non-affine or non-finite transform, a proxy.
    pub fn transform3(&mut self, m: &Mat4) -> bool {
        if !m.is_affine() {
            return false;
        }
        let mut e = self.clone();
        if !e.transform3_in_place(m) {
            return false;
        }
        *self = e;
        true
    }

    fn transform3_in_place(&mut self, m: &Mat4) -> bool {
        let p = |q: &mut Vec3| *q = m.apply(*q);
        match &mut self.kind {
            EntityKind::Line(l) => {
                p(&mut l.a);
                p(&mut l.b);
                thicken(&mut self.common, m);
                return true;
            }
            EntityKind::Point(pt) => {
                p(&mut pt.p);
                thicken(&mut self.common, m);
                return true;
            }
            EntityKind::Polyline3d(pl) => {
                pl.points.iter_mut().for_each(p);
                return true;
            }
            EntityKind::Face3d(f) => {
                f.corners.iter_mut().for_each(p);
                return true;
            }
            EntityKind::Ray(r) | EntityKind::XLine(r) => {
                let dir = m.apply_vec(r.dir).normalized();
                if dir == Vec3::ZERO {
                    return false;
                }
                p(&mut r.base);
                r.dir = dir;
                return true;
            }
            EntityKind::Solid3d(s) => {
                if m.similarity_scale().is_none() {
                    return false;
                }
                s.xform = m.mul(&s.xform);
                return true;
            }
            EntityKind::Helix(h) => return helix(h, m),
            EntityKind::Unknown(_) => return false,
            _ => {}
        }
        let Some(plane) = self.plane() else { return false };
        let Some((n, z, l)) = replane(&plane, m) else { return false };
        let ocs = matches!(place(&self.kind), Place::Ocs);
        let level = n == Vec3::Z;
        let ok = match place(&self.kind) {
            Place::Ocs => true,
            Place::Level => level,
            Place::Ground => level && z.abs() <= FLAT,
            Place::Free => false,
        };
        if !ok || !(l.is_similarity() || affine_ok(&self.kind)) {
            return false;
        }
        let along = m.apply_vec(plane.col(2)).dot(n);
        self.kind.transform(&l);
        // A block's z scales with its normal, not with the plane (the 2D transform took the plane's).
        let zs = along / l.scale_factor();
        if let EntityKind::Insert(i) = &mut self.kind
            && zs.is_finite()
        {
            i.scale.z *= zs;
        }
        self.kind.set_elevation(z);
        if ocs {
            self.common.extrusion = n;
        }
        if along.is_finite() {
            self.common.thickness *= along;
        }
        true
    }
}

/// A line's or point's thickness direction (its extrusion) carried by `m`, the thickness scaled
/// with it; one turned straight down becomes the default with the thickness negated, as files
/// with extrusion (0,0,-1) are read.
fn thicken(c: &mut super::Common, m: &Mat4) {
    // A file's extrusion need not be a unit vector: the thickness scales by how much `m`
    // stretches the direction, not by its length.
    let was = c.extrusion.len();
    let v = m.apply_vec(c.extrusion);
    let len = v.len();
    if !(len.is_finite() && len > 1e-300 && was.is_finite() && was > 1e-300) {
        return;
    }
    let mut n = v * (1.0 / len);
    let mut t = c.thickness * (len / was);
    if n.x.hypot(n.y) < 1e-9 {
        if n.z < 0.0 {
            t = -t;
        }
        n = Vec3::Z;
    }
    c.extrusion = n;
    if t.is_finite() {
        c.thickness = t;
    }
}

/// Planar objects whose in-plane transform is exact under any affine map (no arcs, radii,
/// widths or text to keep round).
fn affine_ok(k: &EntityKind) -> bool {
    let straight = |vs: &[cadcraft_geom::PolyVertex]| vs.iter().all(|v| v.bulge == 0.0);
    match k {
        EntityKind::LwPolyline(p) => {
            straight(&p.vertices) && p.const_width == 0.0 && p.vertices.iter().all(|v| v.start_width == 0.0 && v.end_width == 0.0)
        }
        EntityKind::Hatch(h) => h.loops.iter().all(|l| straight(&l.vertices)),
        EntityKind::Region(r) => r.loops.iter().all(|l| straight(&l.vertices)),
        EntityKind::Line(_)
        | EntityKind::Spline(_)
        | EntityKind::Solid(_)
        | EntityKind::Trace(_)
        | EntityKind::Wipeout(_)
        | EntityKind::Leader(_) => true,
        _ => false,
    }
}

/// Plane `plane` (local → world) carried by `m`: the new extrusion (horizontal planes get the
/// WCS, an upside-down one as a mirror in the plane, as 2D MIRROR keeps the extrusion), the new
/// elevation, and the in-plane transform of the local coordinates. `None` when the plane
/// collapses to a line.
fn replane(plane: &Mat4, m: &Mat4) -> Option<(Vec3, f64, Mat3)> {
    let q = m.mul(plane);
    let (u, v) = (q.col(0), q.col(1));
    let c = u.cross(v);
    let len = c.len();
    if !(len.is_finite() && len > 1e-12 * u.len() * v.len() && len > 1e-300) {
        return None;
    }
    let mut n = c * (1.0 / len);
    // Keep the side the old extrusion went to.
    if n.dot(m.apply_vec(plane.col(2))) < 0.0 {
        n = -n;
    }
    if n.x.hypot(n.y) < 1e-9 {
        n = Vec3::Z;
    }
    let local = Mat4::ocs(n).inverse()?.mul(&q);
    let z = local.m[2][3];
    z.is_finite().then(|| (n, z, local.plane_xy()))
}

/// A helix keeps a vertical axis: similarities that keep it vertical (turned upside down it
/// grows downwards).
fn helix(h: &mut super::Helix, m: &Mat4) -> bool {
    let Some(s) = m.similarity_scale() else { return false };
    let w = m.apply_vec(Vec3::Z);
    if w.x.hypot(w.y) > 1e-9 * s {
        return false;
    }
    let c = m.apply(h.center);
    let l = m.plane_xy();
    let mut k = EntityKind::Helix(h.clone());
    k.transform(&l);
    let EntityKind::Helix(mut t) = k else { return false };
    t.center = c;
    if w.z < 0.0 {
        t.height = -t.height;
    }
    *h = t;
    true
}

/// `p` (in an object's plane) seen through `a` (its plan view).
fn project(p: Prim, a: &Mat3, out: &mut Vec<Prim>) {
    match p {
        Prim::Seg(Segment::Line(l)) => out.push(Prim::Seg(Segment::Line(GLine::new(a.apply(l.a), a.apply(l.b))))),
        Prim::Seg(Segment::Arc { arc, .. }) => {
            curve(GEllipse { center: arc.center, major: Vec2::new(arc.radius, 0.0), ratio: 1.0, start: arc.start, end: arc.end }, a, out)
        }
        Prim::Circle(c) => curve(GEllipse::full(c.center, Vec2::new(c.radius, 0.0), 1.0), a, out),
        Prim::Ellipse(e) => curve(e, a, out),
        Prim::Spline(mut s) => {
            s.transform(a);
            out.push(Prim::Spline(s));
        }
        Prim::Point(q) => out.push(Prim::Point(a.apply(q))),
        Prim::Infinite { base, dir, ray } => {
            let d = a.apply_vec(dir).normalized();
            out.push(if d == Vec2::ZERO { Prim::Point(a.apply(base)) } else { Prim::Infinite { base: a.apply(base), dir: d, ray } });
        }
        Prim::Fill(pts) => out.push(Prim::Fill(pts.into_iter().map(|q| a.apply(q)).collect())),
    }
}

/// An elliptical arc seen through `a`: an ellipse, or the segment it covers seen edge-on.
fn curve(e: GEllipse, a: &Mat3, out: &mut Vec<Prim>) {
    if let Some(t) = e.transformed(a) {
        out.push(Prim::Ellipse(t));
        return;
    }
    let c = a.apply(e.center);
    let (u, v) = (a.apply_vec(e.major), a.apply_vec(e.minor()));
    let d = if u.len() >= v.len() { u } else { v }.normalized();
    // Along `d` the arc is at (u·d) cos t + (v·d) sin t: extreme at φ and φ + π, or at its ends.
    let (p, q) = (u.dot(d), v.dot(d));
    let phi = q.atan2(p);
    let at = |t: f64| p * t.cos() + q * t.sin();
    let ts = [e.start, e.end, phi, phi + cadcraft_geom::PI];
    let along = ts.iter().filter(|t| e.is_full() || cadcraft_geom::angle_in_sweep(**t, e.start, e.end)).map(|t| at(*t));
    let (lo, hi) = along.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), t| (lo.min(t), hi.max(t)));
    if d == Vec2::ZERO || !(lo.is_finite() && hi.is_finite()) || hi - lo <= 0.0 {
        out.push(Prim::Point(c));
    } else {
        out.push(Prim::Seg(Segment::Line(GLine::new(c + d * lo, c + d * hi))));
    }
}

#[cfg(test)]
mod tests;
