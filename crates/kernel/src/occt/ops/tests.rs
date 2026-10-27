//! The operations through the crate's API (checks, then the backend, in-process).

use std::f64::consts::PI;

use cadcraft_geom::{Mat4, Mesh3, Vec3};

use crate::{Bevel, Built, Curve, EdgeCurve, Justify, KernelError, Placed, Primitive, Probe, Region, Seg, SurfaceKind, Topology};

fn v(x: f64, y: f64, z: f64) -> Vec3 {
    Vec3::new(x, y, z)
}

fn p(b: &Built) -> Placed<'_> {
    Placed { brep: &b.brep, xform: &Mat4::IDENTITY }
}

fn cube(a: Vec3, b: Vec3) -> Built {
    crate::primitive(&Primitive::Box { a, b }).unwrap()
}

/// 10 × 20 × 30 from the origin.
fn block() -> Built {
    cube(Vec3::ZERO, v(10.0, 20.0, 30.0))
}

fn bounds(pts: &[Vec3]) -> (Vec3, Vec3) {
    pts.iter().fold((v(f64::MAX, f64::MAX, f64::MAX), v(f64::MIN, f64::MIN, f64::MIN)), |(lo, hi), q| {
        (v(lo.x.min(q.x), lo.y.min(q.y), lo.z.min(q.z)), v(hi.x.max(q.x), hi.y.max(q.y), hi.z.max(q.z)))
    })
}

fn near(a: Vec3, b: Vec3, tol: f64) -> bool {
    (a - b).len() < tol
}

fn volume(m: &Mesh3) -> f64 {
    m.tris.iter().filter_map(|t| m.tri(t)).map(|[a, b, c]| a.dot(b.cross(c)) / 6.0).sum()
}

/// The face whose outward normal is `n`.
fn facing(t: &Topology, n: Vec3) -> u32 {
    t.faces.iter().position(|f| near(f.normal, n, 1e-9)).unwrap() as u32
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64, z: f64) -> Vec<Seg> {
    let c = [v(x0, y0, z), v(x1, y0, z), v(x1, y1, z), v(x0, y1, z)];
    (0..4).map(|i| Seg::Line { a: c[i], b: c[(i + 1) % 4] }).collect()
}

fn line(a: Vec3, b: Vec3) -> Curve {
    Curve::Segs(vec![Seg::Line { a, b }])
}

fn circle(center: Vec3, radius: f64) -> Curve {
    Curve::Segs(vec![Seg::Circle { center, radius, normal: Vec3::Z }])
}

/// A 40 × 40 × 10 plate with a hole of radius 3 through it at (20, 20).
fn holed() -> Built {
    let plate = cube(Vec3::ZERO, v(40.0, 40.0, 10.0));
    let pin = crate::primitive(&Primitive::Cylinder { base: v(20.0, 20.0, -1.0), radius: 3.0, height: 12.0 }).unwrap();
    crate::boolean(crate::BoolOp::Subtract, p(&plate), p(&pin)).unwrap()
}

fn hole_face(t: &Topology) -> u32 {
    t.faces.iter().position(|f| f.kind == SurfaceKind::Cylinder).unwrap() as u32
}

fn ends(c: &EdgeCurve) -> Vec<Vec3> {
    match c {
        EdgeCurve::Line { a, b } => vec![*a, *b],
        EdgeCurve::Arc { a, mid, b } => vec![*a, *mid, *b],
        EdgeCurve::Circle { center, .. } | EdgeCurve::Ellipse { center, .. } => vec![*center],
        EdgeCurve::Spline { points, .. } => points.clone(),
    }
}

#[test]
fn topology_names_faces_and_edges() {
    let b = block();
    let t = crate::topology(p(&b)).unwrap();
    assert_eq!((t.faces.len(), t.edges.len()), (6, 12));
    assert!(t.faces.iter().all(|f| f.kind == SurfaceKind::Plane && f.edges.len() == 4));
    assert!((t.faces.iter().map(|f| f.area).sum::<f64>() - 2.0 * (200.0 + 300.0 + 600.0)).abs() < 1e-6);
    let top = &t.faces[facing(&t, Vec3::Z) as usize];
    assert!(near(top.centroid, v(5.0, 10.0, 30.0), 1e-9) && near(top.axis.1, Vec3::Z, 1e-12));
    assert!(near(top.bbox.0, v(0.0, 0.0, 30.0), 1e-6) && near(top.bbox.1, v(10.0, 20.0, 30.0), 1e-6), "{:?}", top.bbox);
    assert!((t.edges.iter().map(|e| e.length).sum::<f64>() - 240.0).abs() < 1e-9);
    assert!(t.edges.iter().all(|e| matches!(e.curve, EdgeCurve::Line { .. }) && e.faces.len() == 2 && !e.seam));
    // Each face's edges name it back.
    for (i, f) in t.faces.iter().enumerate() {
        assert!(f.edges.iter().all(|e| t.edges[*e as usize].faces.contains(&(i as u32))));
    }
    // The same text names the same faces; a placed copy's are where the transform takes them.
    assert_eq!(crate::topology(p(&b)).unwrap(), t);
    let x = crate::tests::xform(v(1.0, -2.0, 3.0), 1.1, 2.5, true, v(7.0, -4.0, 2.0));
    let moved = crate::topology(Placed { brep: &b.brep, xform: &x }).unwrap();
    for (f, g) in t.faces.iter().zip(&moved.faces) {
        assert!(near(x.apply(f.centroid), g.centroid, 1e-6), "{f:?} {g:?}");
        assert!(near(x.apply_vec(f.normal) * (1.0 / 2.5), g.normal, 1e-9), "mirrored, still outward: {f:?} {g:?}");
    }
    // A cylinder: its side, two caps; two circles and a seam.
    let c = crate::primitive(&Primitive::Cylinder { base: Vec3::ZERO, radius: 3.0, height: 5.0 }).unwrap();
    let t = crate::topology(p(&c)).unwrap();
    let side = &t.faces[hole_face(&t) as usize];
    assert!((side.radius - 3.0).abs() < 1e-12 && near(side.axis.1, Vec3::Z, 1e-12) && (side.area - 30.0 * PI).abs() < 1e-6);
    let circles = t.edges.iter().filter(|e| matches!(e.curve, EdgeCurve::Circle { radius, .. } if (radius - 3.0).abs() < 1e-12)).count();
    assert_eq!(circles, 2, "{:?}", t.edges);
    assert_eq!(t.edges.iter().filter(|e| e.seam).count(), 1);
    // A sphere's poles are degenerate edges.
    let s = crate::primitive(&Primitive::Sphere { center: Vec3::ZERO, radius: 2.0 }).unwrap();
    assert!(crate::topology(p(&s)).unwrap().edges.iter().any(|e| e.degenerate));
    // Shear can't be placed.
    let mut shear = Mat4::IDENTITY;
    shear.m[0][1] = 0.5;
    assert!(matches!(crate::topology(Placed { brep: &b.brep, xform: &shear }), Err(KernelError::Invalid(_))));
}

#[test]
fn pick_finds_faces_and_edges_front_first() {
    let b = block();
    let t = crate::topology(p(&b)).unwrap();
    let (top, bottom) = (facing(&t, Vec3::Z), facing(&t, -Vec3::Z));
    let got = crate::pick(p(&b), Probe::Ray { origin: v(5.0, 10.0, 100.0), dir: v(0.0, 0.0, -3.0) }, 0.1).unwrap();
    assert_eq!(got.faces.iter().map(|h| h.index).collect::<Vec<_>>(), vec![top, bottom]);
    assert!((got.faces[0].along - 70.0).abs() < 1e-9 && near(got.faces[0].point, v(5.0, 10.0, 30.0), 1e-9));
    assert!(got.edges.is_empty(), "{:?}", got.edges);
    // Beside an edge of the top: that edge first, then the one below it.
    let got = crate::pick(p(&b), Probe::Ray { origin: v(0.05, 10.0, 100.0), dir: v(0.0, 0.0, -1.0) }, 0.1).unwrap();
    let first = &got.edges[0];
    assert!((first.distance - 0.05).abs() < 1e-9 && (first.point.z - 30.0).abs() < 1e-9, "{:?}", got.edges);
    assert!(t.edges[first.index as usize].faces.contains(&top));
    assert_eq!(got.edges.len(), 2);
    // Around a point by the +X face, nearer it than any edge.
    let got = crate::pick(p(&b), Probe::Point(v(10.5, 10.0, 15.0)), 1.0).unwrap();
    assert_eq!(got.faces.iter().map(|h| h.index).collect::<Vec<_>>(), vec![facing(&t, v(1.0, 0.0, 0.0))]);
    assert!(got.edges.is_empty());
    assert!(matches!(crate::pick(p(&b), Probe::Ray { origin: Vec3::ZERO, dir: Vec3::ZERO }, 0.1), Err(KernelError::Invalid(_))));
}

#[test]
fn fillet_and_chamfer_only_the_chosen_edges() {
    let b = block();
    let t = crate::topology(p(&b)).unwrap();
    // The four upright edges (along Z).
    let upright: Vec<u32> = (0..12).filter(|i| matches!(t.edges[*i as usize].curve, EdgeCurve::Line { a, b } if (a.z - b.z).abs() > 1.0)).collect();
    assert_eq!(upright.len(), 4);
    let r = crate::fillet_edges(p(&b), &upright.iter().map(|e| (*e, 2.0)).collect::<Vec<_>>()).unwrap();
    let want = 6000.0 - 4.0 * (4.0 - PI) * 30.0;
    // (The mesh's chords cut the fillets' curve a little short.)
    assert!((volume(&r.mesh) - want).abs() < 3.0, "{} vs {want}", volume(&r.mesh));
    assert_eq!(crate::topology(p(&r)).unwrap().faces.len(), 10);
    // Too large for the 10 wide block: refused, not a broken solid.
    assert!(crate::fillet_edges(p(&b), &upright.iter().map(|e| (*e, 6.0)).collect::<Vec<_>>()).is_err());
    assert!(matches!(crate::fillet_edges(p(&b), &[(0, 0.0)]), Err(KernelError::Invalid(_))));
    assert!(crate::fillet_edges(p(&b), &[(99, 1.0)]).is_err(), "no edge 99");
    // One top edge bevelled 1 along the top and 3 down the side.
    let top = facing(&t, Vec3::Z);
    let e = t.faces[top as usize].edges[0];
    let c = crate::chamfer_edges(p(&b), &[Bevel { edge: e, base: 1.0, other: 3.0, face: Some(top) }]).unwrap();
    let ct = crate::topology(p(&c)).unwrap();
    assert_eq!(ct.faces.len(), 7);
    assert!((volume(&c.mesh) - (6000.0 - 0.5 * 1.0 * 3.0 * t.edges[e as usize].length)).abs() < 1e-6, "{}", volume(&c.mesh));
    // The top lost a strip 1 wide.
    let new_top = &ct.faces[facing(&ct, Vec3::Z) as usize];
    assert!((new_top.area - (200.0 - t.edges[e as usize].length)).abs() < 1e-6, "{}", new_top.area);
    let bottom = facing(&t, -Vec3::Z);
    assert!(
        matches!(crate::chamfer_edges(p(&b), &[Bevel { edge: e, base: 1.0, other: 3.0, face: Some(bottom) }]), Err(KernelError::Failed(m)) if m.contains("not next to"))
    );
}

#[test]
fn faces_extrude_with_a_taper_and_cut_inward() {
    let b = block();
    let top = facing(&crate::topology(p(&b)).unwrap(), Vec3::Z);
    let up = crate::extrude_faces(p(&b), &[top], 5.0, 0.0).unwrap();
    let (lo, hi) = bounds(&up.mesh.verts);
    assert!(near(lo, Vec3::ZERO, 1e-9) && near(hi, v(10.0, 20.0, 35.0), 1e-9));
    assert_eq!(crate::topology(p(&up)).unwrap().faces.len(), 6, "merged into a taller block");
    // Tapered in: the new top is smaller.
    let tapered = crate::extrude_faces(p(&b), &[top], 5.0, 0.1).unwrap();
    let (tlo, thi) = bounds(&tapered.mesh.verts.iter().copied().filter(|q| (q.z - 35.0).abs() < 1e-9).collect::<Vec<_>>());
    let inset = 5.0 * 0.1f64.tan();
    assert!(near(tlo, v(inset, inset, 35.0), 1e-6) && near(thi, v(10.0 - inset, 20.0 - inset, 35.0), 1e-6), "{tlo:?} {thi:?}");
    let down = crate::extrude_faces(p(&b), &[top], -5.0, 0.0).unwrap();
    assert!((bounds(&down.mesh.verts).1.z - 25.0).abs() < 1e-9);
    let h = holed();
    let ht = crate::topology(p(&h)).unwrap();
    assert!(matches!(crate::extrude_faces(p(&h), &[hole_face(&ht)], 1.0, 0.0), Err(KernelError::Failed(m)) if m.contains("flat")));
    assert!(matches!(crate::extrude_faces(p(&b), &[top], 5.0, 1.6), Err(KernelError::Invalid(_))));
}

#[test]
fn faces_offset_and_move_with_their_neighbours() {
    let b = block();
    let t = crate::topology(p(&b)).unwrap();
    let top = facing(&t, Vec3::Z);
    let r = crate::offset_faces(p(&b), &[top], 2.0).unwrap();
    assert!(near(bounds(&r.mesh.verts).1, v(10.0, 20.0, 32.0), 1e-6));
    let r = crate::move_faces(p(&b), &[top], v(3.0, 0.0, 4.0)).unwrap();
    assert!(near(bounds(&r.mesh.verts).1, v(10.0, 20.0, 34.0), 1e-6), "only the move along its normal counts");
    let r = crate::move_faces(p(&b), &[top], v(3.0, 1.0, 0.0)).unwrap();
    assert!((volume(&r.mesh) - 6000.0).abs() < 1e-6, "within its plane it stays put");
    // A hole: offset grows the solid (the hole shrinks); moved, the hole goes with it.
    let h = holed();
    let ht = crate::topology(p(&h)).unwrap();
    let hole = hole_face(&ht);
    let base = volume(&h.mesh);
    let r = crate::offset_faces(p(&h), &[hole], 1.0).unwrap();
    let grown = volume(&r.mesh) - base;
    assert!((grown - (9.0 - 4.0) * PI * 10.0).abs() < 1.0, "{grown}");
    let r = crate::move_faces(p(&h), &[hole], v(5.0, 0.0, 0.0)).unwrap();
    let rt = crate::topology(p(&r)).unwrap();
    let moved = &rt.faces[hole_face(&rt) as usize];
    assert!((moved.axis.0.x - 25.0).abs() < 1e-6 && (moved.axis.0.y - 20.0).abs() < 1e-6, "{moved:?}");
    assert!((volume(&r.mesh) - base).abs() < 0.5);
    let top = facing(&ht, Vec3::Z);
    assert!(matches!(crate::move_faces(p(&h), &[hole, top], v(1.0, 0.0, 0.0)), Err(KernelError::Failed(m)) if m.contains("separately")));
}

#[test]
fn faces_turn_and_taper() {
    let b = block();
    let t = crate::topology(p(&b)).unwrap();
    let (top, east) = (facing(&t, Vec3::Z), facing(&t, v(1.0, 0.0, 0.0)));
    let a = 10f64.to_radians();
    // The top turned 10° about the X axis line through its middle.
    let r = crate::rotate_faces(p(&b), &[top], v(0.0, 10.0, 30.0), v(1.0, 0.0, 0.0), a).unwrap();
    let rt = crate::topology(p(&r)).unwrap();
    assert!(rt.faces.iter().any(|f| near(f.normal, v(0.0, -a.sin(), a.cos()), 1e-9)), "{:?}", rt.faces);
    assert!((bounds(&r.mesh.verts).1.z - (30.0 + 10.0 * a.tan())).abs() < 1e-6);
    assert!((volume(&r.mesh) - 6000.0).abs() < 1e-6, "about its middle: as much in as out");
    // The other way, about a line off the face: the same tilt, raised.
    let r = crate::rotate_faces(p(&b), &[top], v(0.0, 0.0, 0.0), v(-1.0, 0.0, 0.0), -a).unwrap();
    let rt = crate::topology(p(&r)).unwrap();
    assert!(rt.faces.iter().any(|f| near(f.normal, v(0.0, -a.sin(), a.cos()), 1e-9)), "{:?}", rt.faces);
    // The +X side tapered in 10° going up from the base.
    let r = crate::taper_faces(p(&b), &[east], Vec3::ZERO, Vec3::Z, a).unwrap();
    let top_x = r.mesh.verts.iter().filter(|q| (q.z - 30.0).abs() < 1e-9).map(|q| q.x).fold(f64::MIN, f64::max);
    assert!((top_x - (10.0 - 30.0 * a.tan())).abs() < 1e-6, "{top_x}");
    assert!(matches!(crate::taper_faces(p(&b), &[east], Vec3::ZERO, Vec3::Z, 1.6), Err(KernelError::Invalid(_))));
    // A hole turned about an upright line beside it swings round with it.
    let h = holed();
    let hole = hole_face(&crate::topology(p(&h)).unwrap());
    let r = crate::rotate_faces(p(&h), &[hole], v(10.0, 20.0, 0.0), Vec3::Z, PI / 2.0).unwrap();
    let rt = crate::topology(p(&r)).unwrap();
    let c = rt.faces[hole_face(&rt) as usize].axis.0;
    assert!((c.x - 10.0).abs() < 1e-6 && (c.y - 30.0).abs() < 1e-6, "{c:?}");
}

#[test]
fn faces_delete_and_copy() {
    let h = holed();
    let ht = crate::topology(p(&h)).unwrap();
    let r = crate::delete_faces(p(&h), &[hole_face(&ht)]).unwrap();
    assert!((volume(&r.mesh) - 16000.0).abs() < 1e-6, "the hole filled");
    let f = crate::fillet_edges(p(&block()), &[(0, 2.0)]).unwrap();
    let ft = crate::topology(p(&f)).unwrap();
    let round = hole_face(&ft);
    let back = crate::delete_faces(p(&f), &[round]).unwrap();
    assert!((volume(&back.mesh) - 6000.0).abs() < 1e-6, "the fillet gone: {}", volume(&back.mesh));
    let b = block();
    assert!(crate::delete_faces(p(&b), &[0, 1, 2, 3, 4, 5]).is_err());
    let copies = crate::copy_faces(p(&b), &[facing(&crate::topology(p(&b)).unwrap(), Vec3::Z), 0]).unwrap();
    assert_eq!(copies.len(), 2);
    assert!(copies.iter().all(|c| c.mesh.open && c.mesh.tris.len() == 2));
}

#[test]
fn bodies_imprint_separate_shell_clean_and_check() {
    let b = block();
    let t = crate::topology(p(&b)).unwrap();
    let top = facing(&t, Vec3::Z);
    let marked = crate::imprint(p(&b), &[], &[circle(v(5.0, 10.0, 30.0), 2.0)]).unwrap();
    assert_eq!(crate::topology(p(&marked)).unwrap().faces.len(), 7, "the top split into a ring and a disc");
    assert!((volume(&marked.mesh) - 6000.0).abs() < 1e-6);
    // A solid crossing it imprints where it cuts the faces.
    let post = cube(v(4.0, 4.0, 20.0), v(6.0, 6.0, 40.0));
    let marked2 = crate::imprint(p(&b), &[p(&post)], &[]).unwrap();
    assert_eq!(crate::topology(p(&marked2)).unwrap().faces.len(), 7);
    assert_eq!(crate::topology(p(&crate::clean(p(&marked)).unwrap())).unwrap().faces.len(), 6);
    // Two lumps apart.
    let far = cube(v(50.0, 0.0, 0.0), v(51.0, 1.0, 1.0));
    let both = crate::boolean(crate::BoolOp::Union, p(&b), p(&far)).unwrap();
    let parts = crate::separate(p(&both)).unwrap();
    assert_eq!(parts.len(), 2);
    assert!(parts.iter().all(|q| !q.mesh.open));
    // Shelled with the top open; closed all round.
    let cup = crate::shell_faces(p(&b), 1.0, &[top]).unwrap();
    assert!((volume(&cup.mesh) - (6000.0 - 8.0 * 18.0 * 29.0)).abs() < 1e-6, "{}", volume(&cup.mesh));
    let boxed = crate::shell_faces(p(&b), 1.0, &[]).unwrap();
    assert!((volume(&boxed.mesh) - (6000.0 - 8.0 * 18.0 * 28.0)).abs() < 1e-6, "{}", volume(&boxed.mesh));
    let c = crate::check(p(&b)).unwrap();
    assert!(c.valid && c.problems.is_empty(), "{c:?}");
}

#[test]
fn press_pull_regions_out_and_in() {
    let b = block();
    let top = facing(&crate::topology(p(&b)).unwrap(), Vec3::Z);
    let boss = Region::Profile { outer: Curve::Segs(rect(2.0, 2.0, 6.0, 8.0, 30.0)), holes: Vec::new() };
    let r = crate::press_pull(p(&b), &boss, 5.0).unwrap();
    assert!((volume(&r.mesh) - (6000.0 + 24.0 * 5.0)).abs() < 1e-6);
    assert!((bounds(&r.mesh.verts).1.z - 35.0).abs() < 1e-9);
    let r = crate::press_pull(p(&b), &boss, -5.0).unwrap();
    assert!((volume(&r.mesh) - (6000.0 - 24.0 * 5.0)).abs() < 1e-6, "a pocket");
    // A ring (a region with a hole) pulled up: a tube.
    let ring = Region::Profile { outer: circle(v(5.0, 10.0, 30.0), 4.0), holes: vec![circle(v(5.0, 10.0, 30.0), 2.0)] };
    let r = crate::press_pull(p(&b), &ring, 3.0).unwrap();
    assert!((volume(&r.mesh) - (6000.0 + 12.0 * PI * 3.0)).abs() < 0.5, "{}", volume(&r.mesh));
    // On the underside (its normal points down): pulled down.
    let under = Region::Profile { outer: Curve::Segs(rect(2.0, 2.0, 6.0, 8.0, 0.0)), holes: Vec::new() };
    assert!((bounds(&crate::press_pull(p(&b), &under, 2.0).unwrap().mesh.verts).0.z + 2.0).abs() < 1e-9);
    let off = Region::Profile { outer: Curve::Segs(rect(2.0, 2.0, 6.0, 8.0, 50.0)), holes: Vec::new() };
    assert!(matches!(crate::press_pull(p(&b), &off, 2.0), Err(KernelError::Failed(m)) if m.contains("not on a face")));
    let r = crate::press_pull(p(&b), &Region::Face(top), 4.0).unwrap();
    assert!((bounds(&r.mesh.verts).1.z - 34.0).abs() < 1e-9);
}

#[test]
fn surfaces_thicken_into_solids() {
    let sheet = crate::plane_surface(&rect(0.0, 0.0, 10.0, 10.0, 0.0)).unwrap();
    let slab = crate::thicken(p(&sheet), 2.0, false).unwrap();
    assert!(!slab.mesh.open && (volume(&slab.mesh).abs() - 200.0).abs() < 1e-6, "{}", volume(&slab.mesh));
    let (lo, hi) = bounds(&slab.mesh.verts);
    assert!((hi.z - lo.z - 2.0).abs() < 1e-9 && (lo.z.abs() < 1e-9 || hi.z.abs() < 1e-9), "{lo:?} {hi:?}");
    let mid = crate::thicken(p(&sheet), 2.0, true).unwrap();
    let (lo, hi) = bounds(&mid.mesh.verts);
    assert!((lo.z + 1.0).abs() < 1e-9 && (hi.z - 1.0).abs() < 1e-9, "{lo:?} {hi:?}");
    assert!(crate::thicken(p(&block()), 1.0, false).is_err(), "a solid isn't a surface");
}

#[test]
fn sections_and_flatshots() {
    let h = holed();
    let s = crate::section(p(&h), v(0.0, 0.0, 5.0), Vec3::Z).unwrap();
    assert_eq!(s.regions.len(), 1);
    let r = &s.regions[0];
    assert_eq!((r.outer.curves.len(), r.holes.len()), (4, 1));
    assert!(r.outer.closed && r.face.mesh.open);
    assert!(matches!(r.holes[0].curves.as_slice(), [EdgeCurve::Circle { radius, .. }] if (radius - 3.0).abs() < 1e-9), "{:?}", r.holes);
    assert_eq!(s.wires.len(), 2);
    assert!(s.wires.iter().all(|w| w.closed));
    assert!(crate::section(p(&h), v(0.0, 0.0, 50.0), Vec3::Z).unwrap().regions.is_empty());
    // Seen from the front (looking along +Y), the plate's hole shows as hidden lines.
    let f = crate::flatshot(&[p(&h)], v(0.0, -1.0, 0.0), v(1.0, 0.0, 0.0)).unwrap();
    let (lo, hi) = bounds(&f.visible.iter().flat_map(ends).collect::<Vec<_>>());
    assert!(near(lo, v(0.0, 0.0, 0.0), 1e-6) && near(hi, v(40.0, 10.0, 0.0), 1e-6), "{lo:?} {hi:?}");
    assert!(f.hidden.iter().flat_map(ends).any(|q| (q.x - 17.0).abs() < 1e-6), "{:?}", f.hidden);
    assert!(matches!(crate::flatshot(&[p(&h)], Vec3::Z, Vec3::Z), Err(KernelError::Invalid(_))));
}

#[test]
fn interference_finds_overlaps_not_touches() {
    let a = cube(Vec3::ZERO, v(10.0, 10.0, 10.0));
    let b = cube(v(8.0, 0.0, 0.0), v(20.0, 10.0, 10.0));
    let c = cube(v(20.0, 0.0, 0.0), v(30.0, 10.0, 10.0));
    let far = cube(v(100.0, 0.0, 0.0), v(110.0, 10.0, 10.0));
    let got = crate::interference(&[p(&a), p(&b), p(&c), p(&far)], &[]).unwrap();
    assert_eq!(got.len(), 1, "b and c only touch");
    assert!((got[0].a, got[0].b) == (0, 1) && (got[0].volume - 200.0).abs() < 1e-6);
    assert!((volume(&got[0].solid.mesh) - 200.0).abs() < 1e-6);
    let got = crate::interference(&[p(&b)], &[p(&far), p(&a)]).unwrap();
    assert_eq!(got.iter().map(|g| (g.a, g.b)).collect::<Vec<_>>(), vec![(0, 1)]);
}

#[test]
fn polysolids_stand_beside_their_path() {
    let l = [Seg::Line { a: Vec3::ZERO, b: v(20.0, 0.0, 0.0) }, Seg::Line { a: v(20.0, 0.0, 0.0), b: v(20.0, 10.0, 0.0) }];
    let w = crate::polysolid(&l, 2.0, 5.0, Justify::Left, Vec3::Z).unwrap();
    assert!(!w.mesh.open);
    let (lo, hi) = bounds(&w.mesh.verts);
    // Left of the path going +X then +Y: inside the corner.
    assert!(near(lo, Vec3::ZERO, 1e-6) && near(hi, v(20.0, 10.0, 5.0), 1e-6), "{lo:?} {hi:?}");
    assert!((volume(&w.mesh) - (20.0 * 2.0 + 8.0 * 2.0) * 5.0).abs() < 1e-6, "{}", volume(&w.mesh));
    let c = crate::polysolid(&l, 2.0, 5.0, Justify::Center, Vec3::Z).unwrap();
    let (lo, hi) = bounds(&c.mesh.verts);
    assert!(near(lo, v(0.0, -1.0, 0.0), 1e-6) && near(hi, v(21.0, 10.0, 5.0), 1e-6), "{lo:?} {hi:?}");
    let r = crate::polysolid(&l, 2.0, 5.0, Justify::Right, Vec3::Z).unwrap();
    assert!(near(bounds(&r.mesh.verts).1, v(22.0, 10.0, 5.0), 1e-6));
    // A clean wall: one face a side along each straight run, two ends, top and bottom.
    assert_eq!(crate::topology(p(&w)).unwrap().faces.len(), 8);
    // An arc: a curved wall.
    let arc = [Seg::Arc { a: v(10.0, 0.0, 0.0), mid: v(0.0, 10.0, 0.0), b: v(-10.0, 0.0, 0.0) }];
    let a = crate::polysolid(&arc, 1.0, 3.0, Justify::Center, Vec3::Z).unwrap();
    assert!((volume(&a.mesh) - PI * 10.0 * 1.0 * 3.0).abs() < 0.05, "{}", volume(&a.mesh));
    assert!(matches!(crate::polysolid(&l, 0.0, 5.0, Justify::Left, Vec3::Z), Err(KernelError::Invalid(_))));
    // Round a closed square: a frame mitred at every corner, the last too.
    let sq = rect(0.0, 0.0, 10.0, 10.0, 0.0);
    let f = crate::polysolid(&sq, 1.0, 2.0, Justify::Center, Vec3::Z).unwrap();
    assert!(!f.mesh.open && (volume(&f.mesh) - (121.0 - 81.0) * 2.0).abs() < 1e-6, "{}", volume(&f.mesh));
    // Closed loops with rounded corners and a round end: the whole wall, no gap (the trims at the
    // sharp corners once reached across to the far side, a wall's height wide).
    let h = std::f64::consts::FRAC_1_SQRT_2 * 100.0;
    let rounded = [
        Seg::Line { a: v(0.0, 0.0, 0.0), b: v(200.0, 0.0, 0.0) },
        Seg::Arc { a: v(200.0, 0.0, 0.0), mid: v(200.0 + h, 100.0 - h, 0.0), b: v(300.0, 100.0, 0.0) },
        Seg::Line { a: v(300.0, 100.0, 0.0), b: v(300.0, 200.0, 0.0) },
        Seg::Line { a: v(300.0, 200.0, 0.0), b: v(100.0, 200.0, 0.0) },
        Seg::Arc { a: v(100.0, 200.0, 0.0), mid: v(100.0 - h, 100.0 + h, 0.0), b: v(0.0, 100.0, 0.0) },
        Seg::Line { a: v(0.0, 100.0, 0.0), b: v(0.0, 0.0, 0.0) },
    ];
    let w = crate::polysolid(&rounded, 20.0, 300.0, Justify::Center, Vec3::Z).unwrap();
    let want = (600.0 + 100.0 * PI) * 20.0 * 300.0;
    assert!((volume(&w.mesh) - want).abs() < 0.002 * want, "{} vs {want}", volume(&w.mesh));
    let planter = [
        Seg::Line { a: v(1200.0, 300.0, 0.0), b: v(1500.0, 300.0, 0.0) },
        Seg::Arc { a: v(1500.0, 300.0, 0.0), mid: v(1600.0, 400.0, 0.0), b: v(1500.0, 500.0, 0.0) },
        Seg::Line { a: v(1500.0, 500.0, 0.0), b: v(1200.0, 500.0, 0.0) },
        Seg::Line { a: v(1200.0, 500.0, 0.0), b: v(1200.0, 300.0, 0.0) },
    ];
    let w = crate::polysolid(&planter, 20.0, 300.0, Justify::Center, Vec3::Z).unwrap();
    let want = (800.0 + 100.0 * PI) * 20.0 * 300.0;
    assert!((volume(&w.mesh) - want).abs() < 0.002 * want, "{} vs {want}", volume(&w.mesh));
    // Climbing: still square to its path.
    let ramp = [Seg::Line { a: Vec3::ZERO, b: v(10.0, 0.0, 10.0) }];
    let r = crate::polysolid(&ramp, 1.0, 1.0, Justify::Center, Vec3::Z).unwrap();
    assert!((volume(&r.mesh) - 200f64.sqrt()).abs() < 1e-6, "{}", volume(&r.mesh));
}

#[test]
fn surfaces_and_meshes_convert_to_solids() {
    let b = block();
    let faces = crate::copy_faces(p(&b), &[0, 1, 2, 3, 4, 5]).unwrap();
    let placed: Vec<Placed> = faces.iter().map(p).collect();
    let s = crate::sew(&placed, true).unwrap();
    assert!(!s.mesh.open && (volume(&s.mesh) - 6000.0).abs() < 1e-6);
    assert!(crate::sew(&placed[..5], false).unwrap().mesh.open);
    assert!(matches!(crate::sew(&placed[..5], true), Err(KernelError::Failed(m)) if m.contains("watertight")));
    // A tetrahedron's mesh, either way round, comes out facing outward.
    let verts = [Vec3::ZERO, v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), v(0.0, 0.0, 1.0)];
    let tris = [[0, 2, 1], [0, 1, 3], [1, 2, 3], [0, 3, 2]];
    let t = crate::mesh_shape(&verts, &tris, true, false).unwrap();
    assert!(!t.mesh.open && (volume(&t.mesh) - 1.0 / 6.0).abs() < 1e-9);
    let flipped: Vec<[u32; 3]> = tris.iter().map(|[a, b, c]| [*a, *c, *b]).collect();
    assert!((volume(&crate::mesh_shape(&verts, &flipped, true, false).unwrap().mesh) - 1.0 / 6.0).abs() < 1e-9);
    assert!(crate::mesh_shape(&verts, &tris[..3], true, false).is_err());
    assert!(matches!(crate::mesh_shape(&verts, &[[0, 1, 9]], true, false), Err(KernelError::Invalid(_))));
    // A box's twelve triangles merged back into six faces.
    let m = crate::mesh_shape(&b.mesh.verts, &b.mesh.tris, true, true).unwrap();
    assert_eq!(crate::topology(p(&m)).unwrap().faces.len(), 6);
    assert!((volume(&m.mesh) - 6000.0).abs() < 1e-6);
    // It models on like any solid (its faces carry no curves on their planes): cut, filleted.
    let cut = crate::boolean(crate::BoolOp::Subtract, p(&m), p(&cube(v(-1.0, -1.0, 25.0), v(11.0, 21.0, 31.0)))).unwrap();
    assert!((volume(&cut.mesh) - 5000.0).abs() < 1e-6, "{}", volume(&cut.mesh));
    let top = facing(&crate::topology(p(&m)).unwrap(), Vec3::Z);
    let edge = crate::topology(p(&m)).unwrap().faces[top as usize].edges[0];
    assert!(crate::fillet_edges(p(&m), &[(edge, 1.0)]).is_ok_and(|r| volume(&r.mesh) < 6000.0));
    assert!(crate::check(p(&m)).unwrap().valid);
    let raw = crate::mesh_shape(&b.mesh.verts, &b.mesh.tris, true, false).unwrap();
    let c = crate::check(p(&raw)).unwrap();
    assert!(c.valid, "{c:?}");
    let cut = crate::boolean(crate::BoolOp::Subtract, p(&raw), p(&cube(v(-1.0, -1.0, 25.0), v(11.0, 21.0, 31.0)))).unwrap();
    assert!((volume(&cut.mesh) - 5000.0).abs() < 1e-6, "{}", volume(&cut.mesh));
    let surf = crate::to_surface(p(&b)).unwrap();
    assert_eq!(crate::topology(p(&surf)).unwrap().faces.len(), 6);
    assert!(crate::separate(p(&surf)).is_err(), "no longer a solid");
}

#[test]
fn slices_by_a_surface() {
    let b = block();
    let cutter = crate::plane_surface(&rect(-5.0, -5.0, 15.0, 25.0, 12.0)).unwrap();
    let parts = crate::split(p(&b), &[p(&cutter)]).unwrap();
    assert_eq!(parts.len(), 2);
    let mut vols: Vec<f64> = parts.iter().map(|q| volume(&q.mesh)).collect();
    vols.sort_by(f64::total_cmp);
    assert!((vols[0] - 2400.0).abs() < 1e-6 && (vols[1] - 3600.0).abs() < 1e-6, "{vols:?}");
}

#[test]
fn surfaces_from_curves() {
    // RULESURF between two lines: a flat quad; between two circles: a cone's side.
    let r = crate::ruled_surface(&line(Vec3::ZERO, v(10.0, 0.0, 0.0)), &line(v(0.0, 5.0, 3.0), v(10.0, 5.0, 3.0))).unwrap();
    assert!(r.mesh.open && r.mesh.tris.len() == 2);
    assert!(crate::ruled_surface(&circle(Vec3::ZERO, 5.0), &circle(v(0.0, 0.0, 4.0), 2.0)).unwrap().mesh.open);
    assert!(crate::ruled_surface(&circle(Vec3::ZERO, 5.0), &line(Vec3::ZERO, v(1.0, 0.0, 0.0))).is_err());
    // TABSURF: an arc swept up.
    let arc = Curve::Segs(vec![Seg::Arc { a: v(5.0, 0.0, 0.0), mid: v(0.0, 5.0, 0.0), b: v(-5.0, 0.0, 0.0) }]);
    let t = crate::tabulated_surface(&arc, v(0.0, 0.0, 7.0)).unwrap();
    assert!(near(bounds(&t.mesh.verts).1, v(5.0, 5.0, 7.0), 1e-3), "{:?}", bounds(&t.mesh.verts));
    // REVSURF: a line beside the Z axis turned a quarter from a quarter: x ≤ 0, y ≥ 0.
    let upright = line(v(3.0, 0.0, 0.0), v(3.0, 0.0, 4.0));
    let q = crate::revolved_surface(&upright, Vec3::ZERO, Vec3::Z, PI / 2.0, PI / 2.0).unwrap();
    let (lo, hi) = bounds(&q.mesh.verts);
    assert!(hi.x < 1e-9 && lo.y > -1e-9 && (lo.x + 3.0).abs() < 1e-6 && (hi.y - 3.0).abs() < 1e-6, "{lo:?} {hi:?}");
    let full = crate::revolved_surface(&upright, Vec3::ZERO, Vec3::Z, 0.0, 2.0 * PI).unwrap();
    assert!(near(bounds(&full.mesh.verts).0, v(-3.0, -3.0, 0.0), 1e-3));
    // EDGESURF: four lines given in any order and direction, a saddle between them.
    let (a, b, c, d) = (Vec3::ZERO, v(10.0, 0.0, 0.0), v(10.0, 10.0, 5.0), v(0.0, 10.0, 0.0));
    let e = crate::edge_surface(&[line(a, b), line(c, d), line(c, b), line(a, d)]).unwrap();
    assert!(e.mesh.open);
    let (lo, hi) = bounds(&e.mesh.verts);
    assert!(near(lo, Vec3::ZERO, 1e-6) && near(hi, v(10.0, 10.0, 5.0), 1e-6), "{lo:?} {hi:?}");
    assert!(crate::edge_surface(&[line(a, b), line(c, d), line(c, b), line(a, v(0.0, 9.0, 0.0))]).is_err(), "not a loop");
    // PLANESURF with a hole.
    let s = crate::planar_surface(&Curve::Segs(rect(0.0, 0.0, 10.0, 10.0, 0.0)), &[circle(v(5.0, 5.0, 0.0), 2.0)]).unwrap();
    let area = crate::topology(p(&s)).unwrap().faces[0].area;
    assert!((area - (100.0 - 4.0 * PI)).abs() < 1e-6, "{area}");
    let lifted = circle(v(5.0, 5.0, 1.0), 2.0);
    assert!(matches!(crate::planar_surface(&Curve::Segs(rect(0.0, 0.0, 10.0, 10.0, 0.0)), &[lifted]), Err(KernelError::Invalid(_))));
}

/// A square section `2 half` wide, square to X at `x`.
fn square_at(half: f64, x: f64) -> Curve {
    let c = [v(x, -half, -half), v(x, half, -half), v(x, half, half), v(x, -half, half)];
    Curve::Segs((0..4).map(|i| Seg::Line { a: c[i], b: c[(i + 1) % 4] }).collect())
}

#[test]
fn lofts_follow_a_path_or_guides() {
    // Along a straight path: a frustum of square sections 4 and 2 wide, 10 long.
    let l = crate::loft_along(&[square_at(2.0, 0.0), square_at(1.0, 10.0)], &line(Vec3::ZERO, v(10.0, 0.0, 0.0))).unwrap();
    assert!(!l.mesh.open);
    let want = 10.0 / 3.0 * (16.0 + 4.0 + 8.0);
    assert!((volume(&l.mesh) - want).abs() < 0.05, "{} vs {want}", volume(&l.mesh));
    // Along a guide through the sections' corners.
    let g = crate::loft_guided(&[square_at(2.0, 0.0), square_at(2.0, 10.0)], &[line(v(0.0, -2.0, -2.0), v(10.0, -2.0, -2.0))]).unwrap();
    assert!(!g.mesh.open && (volume(&g.mesh) - 160.0).abs() < 0.05, "{}", volume(&g.mesh));
    // Two guides through opposite corners of a square growing from 4 to 6 wide: a frustum.
    let guides = [line(v(0.0, -2.0, -2.0), v(10.0, -3.0, -3.0)), line(v(0.0, 2.0, 2.0), v(10.0, 3.0, 3.0))];
    let g2 = crate::loft_guided(&[square_at(2.0, 0.0), square_at(3.0, 10.0)], &guides).unwrap();
    let want = 10.0 / 3.0 * (16.0 + 36.0 + 24.0);
    assert!(!g2.mesh.open && (volume(&g2.mesh) - want).abs() < 0.5, "{} vs {want}", volume(&g2.mesh));
    assert!(matches!(crate::loft_guided(&[square_at(2.0, 0.0)], &[]), Err(KernelError::Invalid(_))));
    // A guide that misses a section is refused.
    let off = line(v(0.0, -2.0, -2.0), v(10.0, -1.0, -1.0));
    assert!(matches!(crate::loft_guided(&[square_at(2.0, 0.0), square_at(3.0, 10.0)], &[off]), Err(KernelError::Failed(m)) if m.contains("touch")));
}

/// LOFT Guides shapes only the faces beside the guide, as AutoCAD's: two 100 squares 100 apart
/// with a guide bowing out 30 from one corner; halfway up, the far sides haven't moved.
#[test]
fn a_guide_bends_only_its_side_of_the_loft() {
    let sq = |z: f64| Curve::Segs(rect(0.0, 0.0, 100.0, 100.0, z));
    let guide = Curve::Segs(vec![Seg::Line { a: Vec3::ZERO, b: v(-30.0, 0.0, 50.0) }, Seg::Line { a: v(-30.0, 0.0, 50.0), b: v(0.0, 0.0, 100.0) }]);
    let l = crate::loft_guided(&[sq(0.0), sq(100.0)], &[guide]).unwrap();
    assert!(!l.mesh.open && volume(&l.mesh) > 1e6, "{}", volume(&l.mesh));
    let cut = crate::section(p(&l), v(0.0, 0.0, 50.0), Vec3::Z).unwrap();
    assert_eq!(cut.regions.len(), 1);
    let pts: Vec<Vec3> = cut.regions[0].outer.curves.iter().flat_map(ends).collect();
    let (lo, hi) = bounds(&pts);
    assert!(near(lo, v(-30.0, 0.0, 50.0), 1e-3) && near(hi, v(100.0, 100.0, 50.0), 1e-3), "{lo:?} {hi:?}");
    // The far side runs straight from (100, 0) to (100, 100).
    let far = cut.regions[0].outer.curves.iter().any(|c| {
        let e = ends(c);
        e.iter().all(|q| (q.x - 100.0).abs() < 1e-6) && e.iter().any(|q| q.y.abs() < 1e-6) && e.iter().any(|q| (q.y - 100.0).abs() < 1e-6)
    });
    assert!(far, "{:?}", cut.regions[0].outer.curves);
}

/// An imprint stays through edits elsewhere on the solid (IMPRINT then PRESSPULL each cut-out).
#[test]
fn imprints_survive_face_edits() {
    let plate = cube(Vec3::ZERO, v(60.0, 40.0, 2.0));
    let marked = crate::imprint(p(&plate), &[], &[circle(v(15.0, 20.0, 2.0), 5.0), circle(v(45.0, 20.0, 2.0), 5.0)]).unwrap();
    let disc = |t: &Topology, x: f64| t.faces.iter().position(|f| near(f.centroid, v(x, 20.0, 2.0), 1e-6)).unwrap() as u32;
    let has_mark = |b: &Built, x: f64| {
        crate::topology(p(b))
            .unwrap()
            .edges
            .iter()
            .any(|e| matches!(e.curve, EdgeCurve::Circle { center, .. } if near(center, v(x, 20.0, 2.0), 1e-6)))
    };
    let t = crate::topology(p(&marked)).unwrap();
    // The first disc pressed through: a hole; the second circle is still there.
    let holed = crate::extrude_faces(p(&marked), &[disc(&t, 15.0)], -2.0, 0.0).unwrap();
    assert!((volume(&holed.mesh) - (4800.0 - 25.0 * PI * 2.0)).abs() < 1.0, "{}", volume(&holed.mesh));
    assert!(has_mark(&holed, 45.0), "the other imprint is kept");
    // Then the second, found again by its own face.
    let t2 = crate::topology(p(&holed)).unwrap();
    let both = crate::press_pull(p(&holed), &Region::Face(disc(&t2, 45.0)), -2.0).unwrap();
    assert!((volume(&both.mesh) - (4800.0 - 50.0 * PI * 2.0)).abs() < 2.0, "{}", volume(&both.mesh));
    // Pulled up as a boss, the other mark stays too; extruding a whole face keeps marks on others.
    let boss = crate::press_pull(p(&marked), &Region::Face(disc(&t, 15.0)), 3.0).unwrap();
    assert!(has_mark(&boss, 45.0));
    let bottom = facing(&t, -Vec3::Z);
    assert!(has_mark(&crate::extrude_faces(p(&marked), &[bottom], 1.0, 0.0).unwrap(), 45.0));
}

/// A surface is pulled into a solid (PRESSPULL on a planar surface), and CHECK calls it a surface.
#[test]
fn surfaces_pull_into_solids() {
    let sheet = crate::plane_surface(&rect(0.0, 0.0, 10.0, 10.0, 0.0)).unwrap();
    let up = crate::extrude_faces(p(&sheet), &[0], 2.0, 0.0).unwrap();
    assert!(!up.mesh.open && (volume(&up.mesh) - 200.0).abs() < 1e-6, "{}", volume(&up.mesh));
    let (lo, hi) = bounds(&crate::press_pull(p(&sheet), &Region::Face(0), -3.0).unwrap().mesh.verts);
    assert!((hi.z - lo.z - 3.0).abs() < 1e-9 && (lo.z.abs() < 1e-9 || hi.z.abs() < 1e-9), "{lo:?} {hi:?}");
    let c = crate::check(p(&sheet)).unwrap();
    assert!(!c.valid && c.problems.iter().any(|m| m.contains("surface")), "{c:?}");
    // A face of a bigger surface alone would lose the rest: refused.
    let shell = crate::to_surface(p(&block())).unwrap();
    assert!(matches!(crate::extrude_faces(p(&shell), &[0], 1.0, 0.0), Err(KernelError::Failed(m)) if m.contains("whole")));
}

/// SHELL outward (negative) with no face removed: a skin round the solid's own volume, hollow.
#[test]
fn a_closed_shell_outward_wraps_the_solid() {
    let r = crate::shell_faces(p(&block()), -1.0, &[]).unwrap();
    assert!((volume(&r.mesh) - (12.0 * 22.0 * 32.0 - 6000.0)).abs() < 1e-6, "{}", volume(&r.mesh));
}

/// A surface stays a surface however closed (CONVTOSURFACE of a solid, read back from a file).
#[test]
fn closed_surfaces_are_still_surfaces() {
    let surf = crate::to_surface(p(&block())).unwrap();
    assert!(surf.mesh.open);
    assert!(crate::tessellate(&surf.brep).unwrap().open, "tessellated again, as when a drawing is opened");
    assert!(!crate::tessellate(&block().brep).unwrap().open);
}

/// SLICE of a surface by a surface: the pieces either side, not one per face.
#[test]
fn a_sliced_surface_comes_apart_in_two() {
    let shell = crate::to_surface(p(&block())).unwrap();
    let cutter = crate::plane_surface(&rect(-5.0, -5.0, 15.0, 25.0, 12.0)).unwrap();
    let parts = crate::split(p(&shell), &[p(&cutter)]).unwrap();
    assert_eq!(parts.len(), 2);
    let mut faces: Vec<usize> = parts.iter().map(|q| crate::topology(p(q)).unwrap().faces.len()).collect();
    faces.sort_unstable();
    assert_eq!(faces, vec![5, 5]);
}

/// Batched picks and sections: one worker call for many parts.
#[test]
fn picks_and_sections_come_in_batches() {
    let (a, b) = (block(), cube(v(20.0, 0.0, 0.0), v(30.0, 20.0, 30.0)));
    let got = crate::picks(&[p(&a), p(&b)], Probe::Ray { origin: v(25.0, 10.0, 100.0), dir: v(0.0, 0.0, -1.0) }, 0.1).unwrap();
    assert_eq!(got.len(), 2);
    assert!(got[0].as_ref().unwrap().faces.is_empty() && got[1].as_ref().unwrap().faces.len() == 2);
    let cuts = crate::sections(&[p(&a), p(&b)], v(0.0, 0.0, 10.0), Vec3::Z, true).unwrap();
    assert_eq!(cuts.len(), 2);
    for c in &cuts {
        let (cut, back) = c.as_ref().unwrap();
        assert_eq!(cut.regions.len(), 1);
        let back = back.as_ref().unwrap();
        assert!((bounds(&back.mesh.verts).1.z - 10.0).abs() < 1e-9, "the piece behind the plane (below it)");
    }
    assert!(crate::sections(&[], Vec3::ZERO, Vec3::Z, false).unwrap().is_empty());
}

/// A big mesh converts well inside the worker's minute (it was sewn triangle by triangle).
#[test]
fn a_big_mesh_converts_quickly() {
    // A sphere of 49,600 triangles (MAX_MESH_TRIS is 50,000).
    let (rings, around) = (125usize, 200usize);
    let mut verts = vec![v(0.0, 0.0, 50.0), v(0.0, 0.0, -50.0)];
    for i in 1..rings {
        let a = PI * i as f64 / rings as f64;
        for j in 0..around {
            let b = 2.0 * PI * j as f64 / around as f64;
            verts.push(v(50.0 * a.sin() * b.cos(), 50.0 * a.sin() * b.sin(), 50.0 * a.cos()));
        }
    }
    let at = |i: usize, j: usize| (2 + (i - 1) * around + j % around) as u32;
    let mut tris = Vec::new();
    for j in 0..around {
        tris.push([0, at(1, j), at(1, j + 1)]);
        tris.push([1, at(rings - 1, j + 1), at(rings - 1, j)]);
    }
    for i in 1..rings - 1 {
        for j in 0..around {
            tris.push([at(i, j), at(i + 1, j), at(i + 1, j + 1)]);
            tris.push([at(i, j), at(i + 1, j + 1), at(i, j + 1)]);
        }
    }
    let t = std::time::Instant::now();
    let s = crate::mesh_shape(&verts, &tris, true, true).unwrap();
    let took = t.elapsed().as_secs_f64();
    assert!(!s.mesh.open && (volume(&s.mesh) - 4.0 / 3.0 * PI * 125_000.0).abs() < 2000.0, "{}", volume(&s.mesh));
    assert!(took < 30.0, "{took} s for {} triangles", tris.len());
}

/// A boolean whose merged result can't be finished (merging spoiled it, or the checker refuses
/// it) gives the boolean's own result, as booleans did before faces were merged: here the touching
/// boxes' ten faces, those they share a plane on left split. An empty result stays an error.
#[test]
fn a_boolean_merging_cant_finish_falls_back_to_its_own_result() {
    let (a, b) = (block(), cube(v(10.0, 0.0, 0.0), v(20.0, 20.0, 30.0)));
    let (sa, sb) = (super::placed(p(&a)), super::placed(p(&b)));
    let faces = |r: &Built| crate::topology(p(r)).unwrap().faces.len();
    let run = |merge| super::text(super::x::boolean(&sa, &sb, 0, merge));
    assert_eq!(faces(&run(true).unwrap()), 6);
    assert_eq!(faces(&run(false).unwrap()), 10);
    let spoiled = super::merged_else_raw(|merge| if merge { Err(KernelError::Failed("made a shape that isn't valid".into())) } else { run(false) });
    assert_eq!(faces(&spoiled.unwrap()), 10);
    let far = super::placed(p(&cube(v(50.0, 0.0, 0.0), v(60.0, 1.0, 1.0))));
    assert!(super::merged_else_raw(|merge| super::text(super::x::boolean(&sa, &far, 2, merge))).is_err());
}
