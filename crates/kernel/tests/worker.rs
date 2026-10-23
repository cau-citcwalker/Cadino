//! The worker process: this test binary is its own worker (re-run with the worker variable and a
//! filter that runs only `worker_entry`), so a kernel crash ends the child, not the tests.
#![cfg(feature = "occt")]

use cadcraft_geom::Vec3;
use cadcraft_kernel::{KernelError, Primitive, WORKER_ENV};

/// The worker's side; does nothing in a normal test run.
#[test]
fn worker_entry() {
    if std::env::var_os(WORKER_ENV).is_some() {
        cadcraft_kernel::worker_main();
    }
}

fn use_worker() {
    let args = ["worker_entry", "--exact", "--test-threads=1", "--nocapture"];
    cadcraft_kernel::set_worker(std::env::current_exe().unwrap(), args.iter().map(|a| a.to_string()).collect());
}

#[test]
fn primitive_round_trips_through_the_worker() {
    use_worker();
    let b = cadcraft_kernel::primitive(&Primitive::Box { a: Vec3::ZERO, b: Vec3::new(10.0, 20.0, 30.0) }).unwrap();
    assert_eq!(b.mesh.tris.len(), 12);
    assert!(b.brep.contains("CASCADE Topology"));
    assert_eq!(cadcraft_kernel::tessellate(&b.brep).unwrap().tris.len(), 12);
    // The worker's errors come back as they are.
    let placed = cadcraft_kernel::Placed { brep: &b.brep, xform: &cadcraft_geom::Mat4::IDENTITY };
    assert!(matches!(cadcraft_kernel::fillet(placed, 100.0), Err(KernelError::Invalid(m)) if m.contains("too large")));
}

#[test]
fn hostile_brep_crashes_the_worker_not_the_app() {
    use_worker();
    // A header OpenCASCADE accepts, then a triangulation with -1 nodes: it throws, and a throw
    // through the bindings aborts the process.
    let brep = "CASCADE Topology V1, (c) Matra-Datavision\nLocations 0\nCurve2ds 0\nCurves 0\nPolygon3D 0\n\
                PolygonOnTriangulations 0\nSurfaces 0\nTriangulations 1\n-1 1 0 0\n";
    match cadcraft_kernel::tessellate(brep) {
        Err(KernelError::Failed(m)) => assert!(m.contains("stopped (crashed) while reading a solid"), "{m}"),
        r => panic!("expected a crash reported as an error, got {r:?}"),
    }
}

#[test]
fn modelling_operations_round_trip_through_the_worker() {
    use cadcraft_kernel::{Curve, Seg};
    use_worker();
    let v = Vec3::new;
    let circle = |r: f64, z: f64| Curve::Segs(vec![Seg::Circle { center: v(0.0, 0.0, z), radius: r, normal: Vec3::Z }]);
    let loft = cadcraft_kernel::loft(&[circle(2.0, 0.0), circle(1.0, 3.0)], false).unwrap();
    assert!(!loft.mesh.open);
    let path = Curve::Segs(vec![Seg::Line { a: v(0.0, 0.0, 0.0), b: v(5.0, 0.0, 0.0) }, Seg::Line { a: v(5.0, 0.0, 0.0), b: v(5.0, 5.0, 0.0) }]);
    let pipe = cadcraft_kernel::sweep(&circle(0.5, 0.0), &path).unwrap();
    assert!(!pipe.mesh.open);
    let placed = cadcraft_kernel::Placed { brep: &pipe.brep, xform: &cadcraft_geom::Mat4::IDENTITY };
    let [a, b] = cadcraft_kernel::slice(placed, v(2.0, 0.0, 0.0), v(1.0, 0.0, 0.0)).unwrap();
    assert!(a.is_some() && b.is_some());
    let pyramid = Primitive::Pyramid { base: Vec3::ZERO, sides: 5, radius: 3.0, top_radius: 0.0, height: 4.0 };
    assert_eq!(cadcraft_kernel::primitive(&pyramid).unwrap().mesh.tris.len(), 3 + 5);
    // Drawings' outlines and exact boxes, a bad B-rep among them coming back as none.
    let cyl = cadcraft_kernel::primitive(&Primitive::Cylinder { base: Vec3::ZERO, radius: 2.0, height: 5.0 }).unwrap();
    let o = cadcraft_kernel::outlines(&[&cyl.brep, "nope"]).unwrap();
    assert!(o.len() == 2 && o[1].is_none() && o[0].as_ref().is_some_and(|o| o.smooth.len() == o.mesh.edges.len() && o.smooth.contains(&true)));
    let id = cadcraft_geom::Mat4::IDENTITY;
    let b = cadcraft_kernel::boxes(&[cadcraft_kernel::Placed { brep: &cyl.brep, xform: &id }, cadcraft_kernel::Placed { brep: "nope", xform: &id }])
        .unwrap();
    let (lo, hi) = b[0].unwrap();
    assert!((lo - v(-2.0, -2.0, 0.0)).len() < 1e-6 && (hi - v(2.0, 2.0, 5.0)).len() < 1e-6 && b[1].is_none(), "{b:?}");
}

/// Mirror (y → -y) when asked, scale, turn by `angle` about the unit `k` (Rodrigues), then move.
fn xform(k: Vec3, angle: f64, scale: f64, mirror: bool, offset: Vec3) -> cadcraft_geom::Mat4 {
    let (s, c) = angle.sin_cos();
    let t = 1.0 - c;
    let r = [
        [t * k.x * k.x + c, t * k.x * k.y - s * k.z, t * k.x * k.z + s * k.y],
        [t * k.x * k.y + s * k.z, t * k.y * k.y + c, t * k.y * k.z - s * k.x],
        [t * k.x * k.z - s * k.y, t * k.y * k.z + s * k.x, t * k.z * k.z + c],
    ];
    let my = if mirror { -1.0 } else { 1.0 };
    let o = [offset.x, offset.y, offset.z];
    let mut m = cadcraft_geom::Mat4::IDENTITY;
    for i in 0..3 {
        m.m[i] = [r[i][0] * scale, r[i][1] * scale * my, r[i][2] * scale, o[i]];
    }
    m
}

fn bounds(pts: &[Vec3]) -> (Vec3, Vec3) {
    let v = Vec3::new;
    pts.iter().fold((v(f64::MAX, f64::MAX, f64::MAX), v(f64::MIN, f64::MIN, f64::MIN)), |(lo, hi), p| {
        (v(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), v(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
    })
}

fn temp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("cadrhino-worker-test-{}-{name}", std::process::id()))
}

#[test]
fn solids_turned_about_any_axis_are_placed_through_the_worker() {
    use cadcraft_kernel::Placed;
    use_worker();
    let v = Vec3::new;
    let b = cadcraft_kernel::primitive(&Primitive::Box { a: v(1.0, 2.0, 3.0), b: v(11.0, 22.0, 33.0) }).unwrap();
    let h = 1.0 / 14.0_f64.sqrt();
    for (i, x) in [
        xform(v(1.0, 0.0, 0.0), std::f64::consts::FRAC_PI_2, 1.0, false, v(5.0, 0.0, 0.0)),
        xform(v(h, -2.0 * h, 3.0 * h), 1.1, 2.0, false, v(-3.0, 4.0, 1.0)),
        xform(v(h, -2.0 * h, 3.0 * h), 2.5, 0.5, true, Vec3::ZERO),
    ]
    .iter()
    .enumerate()
    {
        // Out through the kernel's placement and back: the box where Rust puts it.
        let path = temp(&format!("placed-{i}.stp"));
        cadcraft_kernel::export_step(&[Placed { brep: &b.brep, xform: x }], &path).unwrap();
        let back = cadcraft_kernel::import_step(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let want: Vec<Vec3> = b.mesh.verts.iter().map(|p| x.apply(*p)).collect();
        let ((lo, hi), (wlo, whi)) = (bounds(&back.mesh.verts), bounds(&want));
        assert!((lo - wlo).len() < 1e-5 && (hi - whi).len() < 1e-5, "{i}: {lo:?} {hi:?} vs {wlo:?} {whi:?}");
        assert!(want.iter().all(|p| back.mesh.verts.iter().any(|q| (*q - *p).len() < 1e-5)), "{i}: a corner is missing");
    }
}

#[test]
fn iges_round_trips_a_box_a_line_and_an_arc_through_the_worker() {
    use cadcraft_kernel::{Curve, Placed, Seg};
    use_worker();
    let v = Vec3::new;
    let b = cadcraft_kernel::primitive(&Primitive::Box { a: Vec3::ZERO, b: v(600.0, 18.0, 720.0) }).unwrap();
    let line = Curve::Segs(vec![Seg::Line { a: v(0.0, -50.0, 0.0), b: v(600.0, -50.0, 0.0) }]);
    let arc = Curve::Segs(vec![Seg::Arc { a: v(0.0, -100.0, 0.0), mid: v(300.0, -200.0, 0.0), b: v(600.0, -100.0, 0.0) }]);
    let path = temp("round.igs");
    let x = cadcraft_geom::Mat4::IDENTITY;
    assert_eq!(cadcraft_kernel::export_iges(&[Placed { brep: &b.brep, xform: &x }], &[line, arc], &path).unwrap(), 2);
    let got = cadcraft_kernel::import_iges(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(got.shapes.len(), 1);
    let (lo, hi) = bounds(&got.shapes[0].mesh.verts);
    assert!(lo.len() < 1e-6 && (hi - v(600.0, 18.0, 720.0)).len() < 1e-6, "{lo:?} {hi:?}");
    assert!(!got.shapes[0].mesh.open);
    assert_eq!(got.curves.len(), 2, "{:?}", got.curves);
    assert!(got.curves.iter().any(|c| c.len() == 2), "the line");
    assert!(got.curves.iter().any(|c| c.len() > 5 && c.iter().all(|p| p.y < -99.0)), "the arc");
}

#[test]
fn hostile_iges_fails_the_worker_not_the_app() {
    use_worker();
    let junk = temp("junk.igs");
    // A start and global section, then directory entries pointing nowhere.
    let mut text = String::from("                                                                        S      1\n");
    text.push_str("1H,,1H;,,,,,,,,,,,,,,,,,,,,,,,;                                           G      1\n");
    text.push_str("     126      99999       0       0       0       0       0       000000000D      1\n");
    text.push_str("     126       0       0  -99999       0                               0D      2\n");
    text.push_str("126,-5,-7,1e999,nan;                                                  1P      1\n");
    text.push_str("S      1G      1D      2P      1                                        T      1\n");
    std::fs::write(&junk, text).unwrap();
    let r = cadcraft_kernel::import_iges(&junk);
    let _ = std::fs::remove_file(&junk);
    assert!(r.is_err(), "{r:?}");
    assert!(cadcraft_kernel::import_iges(&temp("missing.igs")).is_err());
}

/// Several B-reps are measured by several workers at once, in order; measured again they come
/// from the earlier measurement, moved where they are now.
#[test]
fn boxes_are_measured_in_parallel_workers_once() {
    use cadcraft_geom::Mat4;
    use cadcraft_kernel::Placed;
    use_worker();
    let v = Vec3::new;
    let solids: Vec<_> =
        (1..=5).map(|i| cadcraft_kernel::primitive(&Primitive::Box { a: Vec3::ZERO, b: v(f64::from(i) * 7.0, 3.0, 2.0) }).unwrap()).collect();
    let at = |x: f64| Mat4::translate(v(x, 0.0, 0.0));
    let (here, there) = (at(0.0), at(100.0));
    let sizes = |x: &Mat4| -> Vec<Vec3> {
        let placed: Vec<Placed> = solids.iter().map(|s| Placed { brep: &s.brep, xform: x }).collect();
        cadcraft_kernel::boxes(&placed).unwrap().into_iter().map(|b| b.unwrap()).map(|(lo, hi)| v(lo.x - x.m[0][3], hi.x - lo.x, hi.y)).collect()
    };
    let want: Vec<Vec3> = (1..=5).map(|i| v(0.0, f64::from(i) * 7.0, 3.0)).collect();
    let near = |a: &[Vec3]| a.iter().zip(&want).all(|(a, b)| (*a - *b).len() < 1e-6);
    let first = sizes(&here);
    assert!(near(&first), "{first:?}");
    let measured = cadcraft_kernel::boxes_measured();
    let again = sizes(&there);
    assert!(near(&again), "{again:?}");
    assert_eq!(cadcraft_kernel::boxes_measured(), measured, "measured again");
}

/// The face and edge operations through the worker: indices named in one call, used in the next.
#[test]
fn topology_and_face_edits_round_trip_through_the_worker() {
    use cadcraft_kernel::{EdgeCurve, Placed, Probe};
    use_worker();
    let v = Vec3::new;
    let b = cadcraft_kernel::primitive(&Primitive::Box { a: Vec3::ZERO, b: v(10.0, 20.0, 30.0) }).unwrap();
    let id = cadcraft_geom::Mat4::IDENTITY;
    let s = Placed { brep: &b.brep, xform: &id };
    let t = cadcraft_kernel::topology(s).unwrap();
    assert_eq!((t.faces.len(), t.edges.len()), (6, 12));
    let hit = cadcraft_kernel::pick(s, Probe::Ray { origin: v(5.0, 10.0, 100.0), dir: v(0.0, 0.0, -1.0) }, 0.1).unwrap();
    let top = hit.faces[0].index;
    assert!((t.faces[top as usize].normal - Vec3::Z).len() < 1e-9);
    let taller = cadcraft_kernel::extrude_faces(s, &[top], 5.0, 0.0).unwrap();
    assert!(taller.mesh.verts.iter().any(|p| (p.z - 35.0).abs() < 1e-9));
    let e = t.faces[top as usize].edges[0];
    assert!(cadcraft_kernel::fillet_edges(s, &[(e, 1.0)]).is_ok());
    let cut = cadcraft_kernel::section(s, v(0.0, 0.0, 10.0), Vec3::Z).unwrap();
    assert!(cut.regions.len() == 1 && cut.regions[0].outer.curves.iter().all(|c| matches!(c, EdgeCurve::Line { .. })));
    // OpenCASCADE's refusal comes back as an error.
    assert!(matches!(cadcraft_kernel::fillet_edges(s, &[(e, 50.0)]), Err(KernelError::Failed(_))));
}
