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
}
