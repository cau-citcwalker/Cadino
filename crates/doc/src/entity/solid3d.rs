//! 3D solids (3DSOLID): a B-rep the kernel made, kept here as data.
//!
//! The document never calls the kernel. It stores the B-rep text, the tessellation drawn on
//! screen, and a transform; MOVE, ROTATE, SCALE, MIRROR and COPY change only the transform.

use std::sync::Arc;

use cadcraft_geom::{Bounds2, Mat3, Mat4, Mesh3, Segment, Vec3};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Solid3d {
    /// The shape in OpenCASCADE's BRep text format, in model coordinates (empty: mesh only,
    /// such as a solid read where the kernel isn't available).
    pub brep: Arc<str>,
    /// Tessellation and edges in model coordinates.
    pub mesh: Arc<Mesh3>,
    /// Model → world.
    #[serde(default)]
    pub xform: Mat4,
}

impl Solid3d {
    pub fn transform(&mut self, m: &Mat3) {
        let mut t = Mat4::from_mat3(m);
        // Solids scale uniformly: z by the same factor as x and y.
        t.m[2][2] = m.det().abs().sqrt();
        self.xform = t.mul(&self.xform);
    }

    pub fn world(&self, p: Vec3) -> Vec3 {
        self.xform.apply(p)
    }

    pub fn bounds(&self) -> Bounds2 {
        self.mesh.bounds_xy(&self.xform)
    }

    /// The edges in plan view, as segments.
    pub fn segments(&self) -> Vec<Segment> {
        let mut out = Vec::new();
        for e in &self.mesh.edges {
            for w in e.windows(2) {
                if let [a, b] = w {
                    out.push(Segment::Line(cadcraft_geom::Line::new(self.world(*a).xy(), self.world(*b).xy())));
                }
            }
        }
        out
    }

    /// Edge end points in world coordinates (grips, snaps, extents).
    pub fn corners(&self) -> Vec<Vec3> {
        self.mesh.edges.iter().filter_map(|e| Some([*e.first()?, *e.last()?])).flatten().map(|p| self.world(p)).collect()
    }
}
