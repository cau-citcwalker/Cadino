use cadcraft_geom::{Mat4, Mesh3, Vec3};

use super::parse::{self, Model};
use super::*;

/// A box from `a` to `b`: 12 outward triangles, each face with its own corners (as the kernel
/// tessellates one).
pub(crate) fn cube(a: Vec3, b: Vec3) -> Mesh3 {
    let v = |i: u32| Vec3::new(if i & 1 == 0 { a.x } else { b.x }, if i & 2 == 0 { a.y } else { b.y }, if i & 4 == 0 { a.z } else { b.z });
    let corners =
        [[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
    let verts = corners.iter().flatten().map(|&i| v(i)).collect();
    Mesh3 { verts, tris: (0..12).map(|t| [3 * t, 3 * t + 1, 3 * t + 2]).collect(), ..Mesh3::default() }
}

/// A prism over the polygon `outer` (counterclockwise) from z 0 to `h`, faceted as the kernel
/// meshes a cylinder: caps as fans, each side a quad of two triangles.
fn prism(outer: &[(f64, f64)], h: f64) -> Mesh3 {
    let n = outer.len() as u32;
    let mut verts: Vec<Vec3> = outer.iter().map(|&(x, y)| Vec3::new(x, y, 0.0)).collect();
    verts.extend(outer.iter().map(|&(x, y)| Vec3::new(x, y, h)));
    let mut tris = Vec::new();
    for i in 1..n - 1 {
        tris.push([0, i + 1, i]);
        tris.push([n, n + i, n + i + 1]);
    }
    for i in 0..n {
        let j = (i + 1) % n;
        tris.push([i, j, n + j]);
        tris.push([i, n + j, n + i]);
    }
    Mesh3 { verts, tris, ..Mesh3::default() }
}

fn circle(n: usize, r: f64) -> Vec<(f64, f64)> {
    (0..n).map(|i| std::f64::consts::TAU * i as f64 / n as f64).map(|a| (r * a.cos(), r * a.sin())).collect()
}

/// The counts of a parsed body's records of each type.
fn counts(m: &Model) -> Vec<usize> {
    ["body", "lump", "shell", "face", "loop", "plane-surface", "coedge", "edge", "straight-curve", "vertex", "point"]
        .iter()
        .map(|n| m.count(n))
        .collect()
}

/// Every face's plane normal points away from `inside`.
fn outward(m: &Model, inside: [f64; 3]) {
    for (f, sides) in m.check().iter().enumerate() {
        // The face's area vector (Newell), from its coedges: outward for counterclockwise loops.
        let mut a = [0.0; 3];
        for (s, t) in sides {
            a[0] += (s[1] - t[1]) * (s[2] + t[2]);
            a[1] += (s[2] - t[2]) * (s[0] + t[0]);
            a[2] += (s[0] - t[0]) * (s[1] + t[1]);
        }
        let p = sides[0].0;
        let away: f64 = (0..3).map(|k| a[k] * (p[k] - inside[k])).sum();
        assert!(away > 0.0, "face {f} faces inward");
    }
}

#[test]
fn box_is_one_lump_one_shell_six_faces_twelve_edges_eight_vertices() {
    let b = body(&cube(Vec3::ZERO, Vec3::new(600.0, 400.0, 18.0)), &Mat4::IDENTITY).unwrap();
    assert_eq!(b.counts(), [6, 6, 24, 12, 8]);
    assert!(!b.open);
    let sat = b.sat();
    assert!(sat.starts_with("700 0 1 0 \n@6 Cadino @12 ACIS 32.0 NT @24 "), "{sat}");
    assert!(sat.ends_with("End-of-ACIS-data \n"));
    let m = parse::sat(&sat);
    assert_eq!((m.version, m.entities), (700, 1));
    assert_eq!(counts(&m), [1, 1, 1, 6, 6, 6, 24, 12, 12, 8, 8]);
    outward(&m, [300.0, 200.0, 9.0]);
    // Single-sided faces of a solid, every coedge with a partner.
    assert!((0..m.recs.len()).filter(|i| m.recs[*i].name == "face").all(|f| !m.yes(f, 7)));
    assert!((0..m.recs.len()).filter(|i| m.recs[*i].name == "coedge").all(|c| m.ptr(c, 3).is_some()));

    let m = parse::sab(&b.sab());
    assert_eq!((m.version, m.entities), (21800, 2));
    assert_eq!(m.count("asmheader"), 1);
    assert_eq!(counts(&m), [1, 1, 1, 6, 6, 6, 24, 12, 12, 8, 8]);
    outward(&m, [300.0, 200.0, 9.0]);
    // A box's corners each end 3 edges.
    assert!(m.recs.iter().filter(|r| r.name == "vertex").all(|r| r.v[2] == parse::V::Num(3.0)));
}

/// A placed solid is written where it is; a mirroring placement keeps its faces outward.
#[test]
fn placement_moves_the_body_and_mirroring_keeps_it_outside_in() {
    let mesh = cube(Vec3::ZERO, Vec3::new(2.0, 1.0, 1.0));
    let mut m = Mat4::IDENTITY;
    m.m[0][3] = 100.0;
    let sat = body(&mesh, &m).unwrap().sat();
    outward(&parse::sat(&sat), [101.0, 0.5, 0.5]);
    m.m[0][0] = -1.0;
    let sat = body(&mesh, &m).unwrap().sat();
    outward(&parse::sat(&sat), [99.0, 0.5, 0.5]);
}

/// A curved solid is faceted the same way every time: each side a planar quad, the caps polygons.
#[test]
fn faceted_cylinder_is_consistent() {
    let mesh = prism(&circle(24, 50.0), 18.0);
    let b = body(&mesh, &Mat4::IDENTITY).unwrap();
    assert_eq!(b.counts(), [26, 26, 144, 72, 48]);
    let sat = b.sat();
    assert_eq!(sat, body(&mesh, &Mat4::IDENTITY).unwrap().sat());
    let m = parse::sat(&sat);
    outward(&m, [0.0, 0.0, 9.0]);
    // Closed and of genus 0: V - E + F = 2.
    assert_eq!(m.count("vertex") + m.count("face"), m.count("edge") + 2);
    outward(&parse::sab(&b.sab()), [0.0, 0.0, 9.0]);
}

/// A plate with a hole: its top and bottom faces have two loops each.
#[test]
fn face_with_a_hole_has_two_loops() {
    let (o, i) = ([(0.0, 0.0), (3.0, 0.0), (3.0, 3.0), (0.0, 3.0)], [(1.0, 1.0), (2.0, 1.0), (2.0, 2.0), (1.0, 2.0)]);
    let mut verts = Vec::new();
    for z in [0.0, 1.0] {
        verts.extend(o.iter().chain(&i).map(|&(x, y)| Vec3::new(x, y, z)));
    }
    // Corners: outer 0–3, inner 4–7 at z 0; +8 at z 1.
    let mut tris = Vec::new();
    for k in 0..4u32 {
        let n = (k + 1) % 4;
        tris.extend([[8 + k, 8 + n, 12 + n], [8 + k, 12 + n, 12 + k]]);
        tris.extend([[k, 4 + n, n], [k, 4 + k, 4 + n]]);
        tris.extend([[k, n, 8 + n], [k, 8 + n, 8 + k]]);
        tris.extend([[4 + n, 4 + k, 12 + k], [4 + n, 12 + k, 12 + n]]);
    }
    let b = body(&Mesh3 { verts, tris, ..Mesh3::default() }, &Mat4::IDENTITY).unwrap();
    assert_eq!(b.counts(), [10, 12, 48, 24, 16]);
    let faces = parse::sat(&b.sat()).check();
    assert_eq!(faces.iter().filter(|sides| sides.len() == 8).count(), 2, "top and bottom: 4 + 4 coedges");
}

/// A UV sphere repeats its seam and poles: welded, and the triangles that collapse dropped, it is
/// a closed body.
#[test]
fn uv_sphere_welds_into_a_closed_body() {
    let (rings, segs) = (8u32, 16u32);
    let mut verts = Vec::new();
    for i in 0..=rings {
        let lat = -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * f64::from(i) / f64::from(rings);
        for j in 0..=segs {
            let lon = std::f64::consts::TAU * f64::from(j) / f64::from(segs);
            verts.push(Vec3::new(lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()));
        }
    }
    let mut tris = Vec::new();
    for i in 0..rings {
        for j in 0..segs {
            let a = i * (segs + 1) + j;
            let (b, c, d) = (a + 1, a + segs + 1, a + segs + 2);
            tris.extend([[a, b, d], [a, d, c]]);
        }
    }
    let b = body(&Mesh3 { verts, tris, ..Mesh3::default() }, &Mat4::IDENTITY).unwrap();
    assert!(!b.open);
    let m = parse::sat(&b.sat());
    outward(&m, [0.0; 3]);
    assert_eq!(m.count("vertex") + m.count("face"), m.count("edge") + 2);
}

/// An open mesh (a surface) is a sheet: double-sided faces, edges along its border with one coedge.
#[test]
fn open_mesh_is_a_double_sided_sheet() {
    let mut mesh = cube(Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0));
    mesh.tris.truncate(2);
    let b = body(&mesh, &Mat4::IDENTITY).unwrap();
    assert!(b.open);
    let m = parse::sat(&b.sat());
    m.check();
    assert_eq!(counts(&m), [1, 1, 1, 1, 1, 1, 4, 4, 4, 4, 4]);
    let f = m.recs.iter().position(|r| r.name == "face").unwrap();
    assert!(m.yes(f, 7) && !m.yes(f, 8), "double out");
    assert!((0..m.recs.len()).filter(|i| m.recs[*i].name == "coedge").all(|c| m.ptr(c, 3).is_none()));
    parse::sab(&b.sab()).check();
}

/// What ACIS can't take as a body gets none.
#[test]
fn broken_meshes_have_no_body() {
    let unit = cube(Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0));
    let mut twice = unit.clone();
    twice.tris.push(twice.tris[0]);
    let mut bad_index = unit.clone();
    bad_index.tris.push([0, 1, 999]);
    let mut nan = unit.clone();
    nan.verts[0].x = f64::NAN;
    let mut flipped = unit.clone();
    flipped.tris[0].swap(1, 2);
    for (what, m) in
        [("empty", Mesh3::default()), ("doubled triangle", twice), ("bad index", bad_index), ("NaN", nan), ("one triangle flipped", flipped)]
    {
        assert!(body(&m, &Mat4::IDENTITY).is_none(), "{what}");
    }
}

/// Solids from the kernel, when it is built in: a box is the same body as a hand-made one; curved
/// and drilled ones are valid faceted bodies.
#[test]
fn kernel_solids() {
    if !cadcraft_kernel::available() {
        eprintln!("no kernel in this build: skipped");
        return;
    }
    let built = cadcraft_kernel::primitive(&cadcraft_kernel::Primitive::Box { a: Vec3::ZERO, b: Vec3::new(600.0, 400.0, 18.0) }).unwrap();
    let b = body(&built.mesh, &Mat4::IDENTITY).unwrap();
    assert_eq!(b.counts(), [6, 6, 24, 12, 8]);
    outward(&parse::sat(&b.sat()), [300.0, 200.0, 9.0]);
    let cyl = cadcraft_kernel::primitive(&cadcraft_kernel::Primitive::Cylinder { base: Vec3::ZERO, radius: 50.0, height: 18.0 }).unwrap();
    let m = parse::sat(&body(&cyl.mesh, &Mat4::IDENTITY).unwrap().sat());
    outward(&m, [0.0, 0.0, 9.0]);
    assert_eq!(m.count("vertex") + m.count("face"), m.count("edge") + 2);
    use cadcraft_kernel::Primitive as P;
    let curved = [
        P::Sphere { center: Vec3::ZERO, radius: 40.0 },
        P::Cone { base: Vec3::ZERO, base_radius: 30.0, top_radius: 0.0, height: 50.0 },
        P::Torus { center: Vec3::ZERO, radius: 60.0, tube: 10.0 },
    ];
    // For black-box checks with other readers (ezdxf, the ODA converter).
    let dump = |name: String, b: &Body| {
        if let Ok(dir) = std::env::var("CADRHINO_ACIS_OUT") {
            let _ = std::fs::write(format!("{dir}/{name}.sat"), b.sat());
            let _ = std::fs::write(format!("{dir}/{name}.sab"), b.sab());
        }
    };
    for (i, p) in curved.iter().enumerate() {
        let built = cadcraft_kernel::primitive(p).unwrap();
        let b = body(&built.mesh, &Mat4::IDENTITY).unwrap_or_else(|| panic!("shape {i} has a body"));
        dump(format!("kernel_{i}"), &b);
        let m = parse::sab(&b.sab());
        m.check();
        // V - E + F = 2 (1 - genus): the torus has genus 1.
        let euler = (m.count("vertex") + m.count("face")) as i64 - m.count("edge") as i64;
        assert_eq!(euler, if i == 2 { 0 } else { 2 }, "shape {i}");
    }
    // A panel with a drilled hole.
    let hole = cadcraft_kernel::primitive(&P::Cylinder { base: Vec3::new(300.0, 200.0, -1.0), radius: 4.0, height: 20.0 }).unwrap();
    let id = Mat4::IDENTITY;
    let drilled = cadcraft_kernel::boolean(
        cadcraft_kernel::BoolOp::Subtract,
        cadcraft_kernel::Placed { brep: &built.brep, xform: &id },
        cadcraft_kernel::Placed { brep: &hole.brep, xform: &id },
    )
    .unwrap();
    let b = body(&drilled.mesh, &id).unwrap();
    dump("kernel_drilled".into(), &b);
    let m = parse::sat(&b.sat());
    m.check();
    assert!(m.count("face") > 6 && m.count("loop") == m.count("face") + 2, "the hole is an inner loop of the top and the bottom");
}

/// SAT reals parse back exactly, and none is dozens of digits long (an ACIS reader dropped a body
/// with a 49-character one).
#[test]
fn sat_reals_are_short_and_exact() {
    for v in [0.0, -0.0, 1.0, 600.0, 0.1, -5.999039130647431e-31, 1e-300, 1e20, 123456.789, f64::MIN_POSITIVE] {
        let s = Real(v).to_string();
        assert!(s.len() <= 24, "{s}");
        assert_eq!(s.parse::<f64>().unwrap(), v);
    }
}
