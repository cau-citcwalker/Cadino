//! Polygon and polyface meshes: a POLYLINE with mesh flags (DXF Reference, POLYLINE group 70
//! bits 16 and 64). EDGESURF, RULESURF, REVSURF and TABSURF make polygon meshes (M rows of N
//! vertices); polyface meshes (a vertex list and faces naming three or four of them) are read
//! from files. Vertices are world points.

use cadcraft_geom::{Bounds2, Mesh3, Vec3};
use serde::{Deserialize, Serialize};

/// Most vertices a mesh made here has (M × N), so a large SURFTAB can't exhaust memory.
pub const MAX_MESH_VERTICES: usize = 250_000;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolyMesh {
    /// A polygon mesh's M rows of N vertices, row by row; a polyface mesh's vertex list.
    pub vertices: Vec<Vec3>,
    /// A polygon mesh's vertex counts in M (DXF 71) and N (72).
    pub m: u32,
    pub n: u32,
    /// Closed in M (DXF 70 bit 1: the last row joins the first) and in N (bit 32).
    pub closed_m: bool,
    pub closed_n: bool,
    /// A polyface mesh's faces (empty for a polygon mesh): 1-based vertex indices, 0 where a
    /// triangle has no fourth; a negative index hides the edge that starts at that vertex.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub faces: Vec<[i32; 4]>,
}

impl PolyMesh {
    /// An M × N polygon mesh, row by row; `None` when the counts don't match the vertices or
    /// leave no face.
    pub fn grid(m: usize, n: usize, vertices: Vec<Vec3>, closed_m: bool, closed_n: bool) -> Option<PolyMesh> {
        let ok = m >= 2 && n >= 2 && m.checked_mul(n) == Some(vertices.len()) && vertices.len() <= MAX_MESH_VERTICES;
        ok.then(|| PolyMesh { vertices, m: m as u32, n: n as u32, closed_m, closed_n, faces: Vec::new() })
    }

    pub fn is_polyface(&self) -> bool {
        !self.faces.is_empty()
    }

    /// Vertex `(i, j)` of a polygon mesh (row `i` in M, column `j` in N), wrapping round a closed
    /// direction.
    fn at(&self, i: usize, j: usize) -> Option<usize> {
        let (m, n) = (self.m as usize, self.n as usize);
        let k = (i % m.max(1)).checked_mul(n)?.checked_add(j % n.max(1))?;
        (k < self.vertices.len()).then_some(k)
    }

    /// Rows (M) and columns (N) of faces of a polygon mesh.
    fn spans(&self) -> (usize, usize) {
        let (m, n) = (self.m as usize, self.n as usize);
        if m < 2 || n < 2 || m.saturating_mul(n) > self.vertices.len() {
            return (0, 0);
        }
        (if self.closed_m { m } else { m - 1 }, if self.closed_n { n } else { n - 1 })
    }

    /// The faces as vertex indices (a triangle repeats its third) with their hidden-edge bits
    /// (1 the edge from the first corner ... 8 from the fourth), as 3DFACE keeps them.
    pub fn faces4(&self) -> Vec<([usize; 4], u8)> {
        if self.is_polyface() {
            let mut out = Vec::new();
            for f in &self.faces {
                let mut idx: Vec<usize> = Vec::with_capacity(4);
                let mut hidden = 0u8;
                for &v in f.iter().filter(|v| **v != 0) {
                    let Some(i) = usize::try_from(v.unsigned_abs()).ok().and_then(|i| i.checked_sub(1)).filter(|i| *i < self.vertices.len()) else {
                        idx.clear();
                        break;
                    };
                    if v < 0 {
                        hidden |= 1 << idx.len();
                    }
                    idx.push(i);
                }
                match idx.as_slice() {
                    // A triangle's closing edge is the fourth (the third is degenerate).
                    [a, b, c] => out.push(([*a, *b, *c, *c], hidden | if hidden & 4 != 0 { 8 } else { 0 })),
                    [a, b, c, d] => out.push(([*a, *b, *c, *d], hidden)),
                    _ => {}
                }
            }
            return out;
        }
        let (rows, cols) = self.spans();
        let mut out = Vec::with_capacity(rows.saturating_mul(cols));
        for i in 0..rows {
            for j in 0..cols {
                if let (Some(a), Some(b), Some(c), Some(d)) = (self.at(i, j), self.at(i, j + 1), self.at(i + 1, j + 1), self.at(i + 1, j)) {
                    out.push(([a, b, c, d], 0));
                }
            }
        }
        out
    }

    /// The lines drawn: a polygon mesh's M and N lines through its vertices, a polyface mesh's
    /// visible face edges (each once).
    pub fn lines(&self) -> Vec<Vec<Vec3>> {
        let v = |k: Option<usize>| k.and_then(|k| self.vertices.get(k)).copied();
        if self.is_polyface() {
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for (f, hidden) in self.faces4() {
                for e in 0..4 {
                    let (a, b) = (f[e], f[(e + 1) % 4]);
                    if hidden & (1 << e) == 0 && a != b && seen.insert((a.min(b), a.max(b))) {
                        out.extend(v(Some(a)).zip(v(Some(b))).map(|(p, q)| vec![p, q]));
                    }
                }
            }
            return out;
        }
        let (rows, cols) = self.spans();
        if rows == 0 {
            return Vec::new();
        }
        let (m, n) = (self.m as usize, self.n as usize);
        let row = |i: usize| (0..=cols).filter_map(|j| v(self.at(i, j))).collect::<Vec<_>>();
        let col = |j: usize| (0..=rows).filter_map(|i| v(self.at(i, j))).collect::<Vec<_>>();
        (0..m).map(row).chain((0..n).map(col)).filter(|l| l.len() >= 2).collect()
    }

    /// The faces as triangles and the lines as edges, for drawing ([`Mesh3::open`]: both sides
    /// show).
    pub fn mesh3(&self) -> Mesh3 {
        let mut tris = Vec::new();
        for ([a, b, c, d], _) in self.faces4() {
            let t = |p: usize, q: usize, r: usize| [p, q, r].map(|i| i as u32);
            if a != b && b != c && a != c {
                tris.push(t(a, b, c));
            }
            if c != d && d != a && a != c {
                tris.push(t(a, c, d));
            }
        }
        Mesh3 { verts: self.vertices.clone(), tris, edges: self.lines(), open: true }
    }

    pub fn bounds(&self) -> Bounds2 {
        Bounds2::from_points(self.vertices.iter().map(|p| p.xy()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn grid3x4(closed_n: bool) -> PolyMesh {
        let v = (0..3).flat_map(|i| (0..4).map(move |j| Vec3::new(j as f64, i as f64, 0.0))).collect();
        PolyMesh::grid(3, 4, v, false, closed_n).unwrap()
    }

    /// An open 3 × 4 mesh has 2 × 3 faces and 3 + 4 lines; closed in N, a fourth face per row
    /// and lines that come back round. Counts that don't match the vertices are refused.
    #[test]
    fn faces_and_lines_of_a_polygon_mesh() {
        let m = grid3x4(false);
        assert_eq!(m.faces4().len(), 6);
        assert_eq!(m.lines().len(), 7);
        assert_eq!(m.mesh3().tris.len(), 12);
        let c = grid3x4(true);
        assert_eq!(c.faces4().len(), 8);
        assert_eq!(c.lines()[0].len(), 5, "a row closed in N ends where it starts");
        assert!(PolyMesh::grid(3, 4, vec![Vec3::ZERO; 11], false, false).is_none());
        assert!(PolyMesh::grid(1, 4, vec![Vec3::ZERO; 4], false, false).is_none());
    }

    /// Polyface faces: triangles, hidden edges and out-of-range indices (hostile files) skipped.
    #[test]
    fn polyface_faces_and_hidden_edges() {
        let v = vec![Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y];
        let m = PolyMesh { vertices: v, faces: vec![[1, 2, -3, 0], [1, 3, 4, 0], [1, 2, 9, 0], [i32::MIN, 1, 2, 3]], ..Default::default() };
        let f = m.faces4();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0], ([0, 1, 2, 2], 4 | 8));
        // The first face draws 1-2 and 2-3 (its edge from 3 back to 1 is hidden), the second
        // 1-3, 3-4 and 4-1.
        assert_eq!(m.lines().len(), 2 + 3);
        assert_eq!(m.mesh3().tris.len(), 2);
    }
}
