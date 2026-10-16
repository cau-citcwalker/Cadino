use cadcraft_geom::{PolyVertex, Spline as GSpline};

use super::*;
use crate::*;

fn near(a: Vec3, b: Vec3) -> bool {
    (a - b).len() < 1e-9
}

fn ent(kind: EntityKind) -> Entity {
    Entity::new(Handle(1), kind)
}

fn circle(center: Vec3, radius: f64) -> Entity {
    ent(EntityKind::Circle(Circle { center, radius }))
}

fn text() -> Text {
    Text {
        insert: Vec3::new(1.0, 2.0, 3.0),
        align_pt: None,
        height: 1.0,
        value: "A".into(),
        rotation: 0.2,
        width_factor: 1.0,
        oblique: 0.0,
        style: "Standard".into(),
        halign: HAlign::Left,
        valign: VAlign::Baseline,
    }
}

/// One object of every planar kind, at elevation 3 with the default extrusion.
fn planar() -> Vec<Entity> {
    let v = |x, y| PolyVertex::new(Vec2::new(x, y));
    let mut sp = GSpline::from_fit_points(&[Vec2::ZERO, Vec2::new(1.0, 2.0), Vec2::new(3.0, 1.0)]);
    sp.elevation = 3.0;
    vec![
        circle(Vec3::new(1.0, 2.0, 3.0), 2.0),
        ent(EntityKind::Arc(Arc { center: Vec3::new(1.0, 2.0, 3.0), radius: 2.0, start: 0.3, end: 2.0 })),
        ent(EntityKind::Ellipse(Ellipse { center: Vec3::new(1.0, 2.0, 3.0), major: Vec3::new(2.0, 1.0, 0.0), ratio: 0.5, start: 0.0, end: 3.0 })),
        ent(EntityKind::LwPolyline(LwPolyline {
            vertices: vec![v(0.0, 0.0), PolyVertex::with_bulge(Vec2::new(4.0, 0.0), 0.5), v(4.0, 3.0)],
            closed: false,
            const_width: 0.0,
            elevation: 3.0,
            plinegen: false,
        })),
        ent(EntityKind::Spline(sp)),
        ent(EntityKind::Text(text())),
        ent(EntityKind::Solid(Solid {
            corners: [Vec3::new(0.0, 0.0, 3.0), Vec3::new(1.0, 0.0, 3.0), Vec3::new(0.0, 1.0, 3.0), Vec3::new(1.0, 1.0, 3.0)],
        })),
    ]
}

/// The world points of an object's grips (a circle's centre: its quadrant grips follow the
/// OCS axes, not the circle).
fn world_grips(e: &Entity) -> Vec<Vec3> {
    let p = e.plane().unwrap_or(Mat4::IDENTITY);
    let n = if matches!(e.kind, EntityKind::Circle(_)) { 1 } else { usize::MAX };
    e.kind.grips().into_iter().take(n).map(|g| p.apply(g.to3(0.0))).collect()
}

#[test]
fn plane_is_the_ocs_raised_by_the_elevation() {
    let mut c = circle(Vec3::new(1.0, 2.0, 3.0), 1.0);
    assert!(near(c.plane().unwrap().apply(Vec3::new(1.0, 2.0, 0.0)), Vec3::new(1.0, 2.0, 3.0)));
    assert!(c.plan_xf().is_none());
    // Standing in the XZ plane, facing -Y: OCS x is world X, OCS y world Z, the elevation -Y.
    c.common.extrusion = Vec3::new(0.0, -1.0, 0.0);
    assert!(near(c.plane().unwrap().apply(Vec3::new(1.0, 2.0, 0.0)), Vec3::new(1.0, -3.0, 2.0)));
    // True 3D objects have no plane; a level line has its height.
    let line = |z| ent(EntityKind::Line(Line { a: Vec3::new(0.0, 0.0, 1.0), b: Vec3::new(1.0, 0.0, z) }));
    assert!(line(2.0).plane().is_none());
    assert!(near(line(1.0).plane().unwrap().apply(Vec3::ZERO), Vec3::Z));
    for k in [EntityKind::Point(Point { p: Vec3::ZERO, angle: 0.0 }), EntityKind::Polyline3d(Polyline3d { points: vec![], closed: false })] {
        assert!(ent(k).plane().is_none());
    }
    // A hostile extrusion or elevation falls back to the WCS / zero.
    c.common.extrusion = Vec3::new(f64::NAN, 0.0, 1.0);
    let EntityKind::Circle(cc) = &mut c.kind else { panic!() };
    cc.center.z = f64::INFINITY;
    assert_eq!(c.plane(), Some(Mat4::IDENTITY));
}

#[test]
fn rotating_a_circle_about_x_stands_it_up() {
    let mut c = circle(Vec3::new(1.0, 2.0, 3.0), 2.0);
    let r = Mat4::rotation(Vec3::X, std::f64::consts::FRAC_PI_2);
    assert!(c.transform3(&r));
    assert!(near(c.common.extrusion, Vec3::new(0.0, -1.0, 0.0)), "{:?}", c.common.extrusion);
    let EntityKind::Circle(cc) = &c.kind else { panic!() };
    assert!(near(cc.center, Vec3::new(1.0, 2.0, 3.0)) && cc.radius == 2.0, "{cc:?}");
    assert!(near(c.plane().unwrap().apply(Vec3::new(1.0, 2.0, 0.0)), r.apply(Vec3::new(1.0, 2.0, 3.0))));
    // Seen from above it is a segment along X at y = -3.
    let prims = c.prims();
    let [Prim::Seg(Segment::Line(l))] = prims.as_slice() else { panic!("{prims:?}") };
    assert!(
        (l.a.dist(Vec2::new(-1.0, -3.0)) < 1e-6 && l.b.dist(Vec2::new(3.0, -3.0)) < 1e-6)
            || (l.b.dist(Vec2::new(-1.0, -3.0)) < 1e-6 && l.a.dist(Vec2::new(3.0, -3.0)) < 1e-6),
        "{l:?}"
    );
    let b = entity_bounds(&Drawing::default(), &c, 0);
    assert!((b.min.x + 1.0).abs() < 1e-9 && (b.max.x - 3.0).abs() < 1e-9 && (b.min.y + 3.0).abs() < 1e-9 && (b.max.y + 3.0).abs() < 1e-9, "{b:?}");
    assert!(c.grips().iter().all(|g| (g.y + 3.0).abs() < 1e-9));
    // Tilted 45° it is an ellipse.
    let mut t = circle(Vec3::ZERO, 1.0);
    assert!(t.transform3(&Mat4::rotation(Vec3::X, std::f64::consts::FRAC_PI_4)));
    let prims = t.prims();
    let [Prim::Ellipse(e)] = prims.as_slice() else { panic!("{prims:?}") };
    assert!((e.ratio - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-9 && (e.major.len() - 1.0).abs() < 1e-9);
}

#[test]
fn every_planar_kind_moves_with_its_plane() {
    let m = Mat4::translate(Vec3::new(5.0, -1.0, 2.0)).mul(&Mat4::rotation(Vec3::new(1.0, 2.0, 0.5), 1.1));
    for e in planar() {
        let before: Vec<Vec3> = world_grips(&e).into_iter().map(|p| m.apply(p)).collect();
        let mut t = e.clone();
        assert!(t.transform3(&m), "{}", e.kind.type_name());
        assert!(!near(t.common.extrusion, Vec3::Z), "{}", e.kind.type_name());
        let after = world_grips(&t);
        assert_eq!(before.len(), after.len());
        for (a, b) in before.iter().zip(&after) {
            assert!((*a - *b).len() < 1e-6, "{}: {a:?} {b:?}", e.kind.type_name());
        }
        // And back again.
        assert!(t.transform3(&m.inverse().unwrap()));
        for (a, b) in world_grips(&e).iter().zip(world_grips(&t).iter()) {
            assert!((*a - *b).len() < 1e-6, "{}", e.kind.type_name());
        }
    }
}

#[test]
fn turning_over_keeps_the_wcs_with_a_mirror_in_the_plane() {
    let mut a = ent(EntityKind::Arc(Arc { center: Vec3::new(1.0, 2.0, 3.0), radius: 1.0, start: 0.0, end: 1.0 }));
    let start = Vec3::new(2.0, 2.0, 3.0);
    let r = Mat4::rotation(Vec3::X, std::f64::consts::PI);
    assert!(a.transform3(&r));
    assert_eq!(a.common.extrusion, Vec3::Z);
    let EntityKind::Arc(arc) = &a.kind else { panic!() };
    assert!(near(arc.center, Vec3::new(1.0, -2.0, -3.0)));
    // The start point is now the end of the (still counter-clockwise) arc.
    assert!(near(Vec2::polar(arc.center.xy(), 1.0, arc.end).to3(-3.0), r.apply(start)), "{arc:?}");
    // A plan MIRROR keeps the extrusion too.
    let mut c = circle(Vec3::new(1.0, 0.0, 0.0), 1.0);
    assert!(c.transform3(&Mat4::mirror(Vec3::ZERO, Vec3::X)));
    assert_eq!(c.common.extrusion, Vec3::Z);
}

#[test]
fn what_cannot_be_represented_is_refused_and_left_alone() {
    // A circle stretched off its plane.
    let mut c = circle(Vec3::new(1.0, 2.0, 3.0), 2.0);
    c.common.extrusion = Vec3::new(0.0, -1.0, 0.0);
    let before = c.clone();
    assert!(!c.transform3(&Mat4::scale(Vec3::new(1.0, 1.0, 2.0))));
    assert_eq!(c, before);
    // Stretched along its own normal it only moves.
    let mut flat = circle(Vec3::new(1.0, 2.0, 3.0), 2.0);
    assert!(flat.transform3(&Mat4::scale(Vec3::new(1.0, 1.0, 2.0))));
    assert_eq!(flat.kind, EntityKind::Circle(Circle { center: Vec3::new(1.0, 2.0, 6.0), radius: 2.0 }));
    // A plane squashed to a line, a non-affine or non-finite transform.
    assert!(!flat.transform3(&Mat4::scale(Vec3::new(1.0, 0.0, 1.0))));
    let mut nan = Mat4::IDENTITY;
    nan.m[0][0] = f64::NAN;
    assert!(!flat.transform3(&nan));
    // A straight polyline can be sheared; a solid can't.
    let shear = Mat4::from_cols(Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Z, Vec3::ZERO);
    let mut pl = ent(EntityKind::LwPolyline(LwPolyline {
        vertices: vec![PolyVertex::new(Vec2::ZERO), PolyVertex::new(Vec2::new(0.0, 1.0))],
        closed: false,
        const_width: 0.0,
        elevation: 0.0,
        plinegen: false,
    }));
    assert!(pl.transform3(&shear));
    assert_eq!(pl.kind.grips().last().copied(), Some(Vec2::new(1.0, 1.0)));
    let mut s = ent(EntityKind::Solid3d(Solid3d::default()));
    assert!(!s.transform3(&shear));
    let r = Mat4::rotation(Vec3::Y, 0.4).mul(&Mat4::scale(Vec3::new(2.0, 2.0, 2.0)));
    assert!(s.transform3(&r));
    let EntityKind::Solid3d(sd) = &s.kind else { panic!() };
    assert_eq!(sd.xform, r);
    // Horizontal-only objects can move and turn in plan, not stand up.
    let mut dim = ent(EntityKind::Leader(Leader { vertices: vec![Vec3::ZERO, Vec3::X], arrow: true, spline: false, style: "Standard".into() }));
    assert!(dim.transform3(&Mat4::translate(Vec3::new(0.0, 0.0, 4.0))));
    assert_eq!(dim.kind.elevation(), 4.0);
    assert!(!dim.transform3(&Mat4::rotation(Vec3::X, 1.0)));
    let mut region = ent(EntityKind::Region(Region { loops: vec![] }));
    assert!(region.transform3(&Mat4::rotation(Vec3::Z, 1.0)));
    assert!(!region.transform3(&Mat4::translate(Vec3::Z)));
    assert!(!ent(EntityKind::Unknown(Unknown { dxf_type: "X".into(), tags: vec![] })).transform3(&Mat4::IDENTITY));
}

#[test]
fn true_3d_objects_transform_their_points() {
    let r = Mat4::rotation(Vec3::X, std::f64::consts::FRAC_PI_2);
    let mut l = ent(EntityKind::Line(Line { a: Vec3::new(1.0, 1.0, 0.0), b: Vec3::new(2.0, 1.0, 0.0) }));
    assert!(l.transform3(&r));
    assert_eq!(l.kind.elevation(), 1.0);
    let EntityKind::Line(line) = &l.kind else { panic!() };
    assert!(near(line.a, Vec3::new(1.0, 0.0, 1.0)) && l.plane().is_some());
    let mut ray = ent(EntityKind::Ray(RayLine { base: Vec3::ZERO, dir: Vec3::Y }));
    assert!(ray.transform3(&r));
    let EntityKind::Ray(rl) = &ray.kind else { panic!() };
    assert!(near(rl.dir, Vec3::Z));
    let helix = |h: f64| {
        ent(EntityKind::Helix(Helix {
            center: Vec3::new(1.0, 0.0, 0.0),
            base_radius: 1.0,
            top_radius: 1.0,
            height: h,
            turns: 2.0,
            start_angle: 0.0,
            ccw: true,
            constrain: Default::default(),
        }))
    };
    let mut h = helix(4.0);
    assert!(!h.transform3(&r), "the axis must stay vertical");
    assert!(h.transform3(&Mat4::mirror(Vec3::new(0.0, 0.0, 1.0), Vec3::Z)));
    let EntityKind::Helix(hx) = &h.kind else { panic!() };
    assert!(near(hx.center, Vec3::new(1.0, 0.0, 2.0)) && hx.height == -4.0 && hx.ccw);
}

#[test]
fn plan_transform_of_a_standing_object_moves_it_in_3d() {
    let mut c = circle(Vec3::ZERO, 1.0);
    assert!(c.transform3(&Mat4::rotation(Vec3::X, std::f64::consts::FRAC_PI_2)));
    assert!(c.transform(&Mat3::rotate(std::f64::consts::FRAC_PI_2).then(Mat3::translate(Vec2::new(5.0, 0.0)))));
    // It now faces +X, centred at (5, 0, 0).
    assert!(near(c.common.extrusion, Vec3::X), "{:?}", c.common.extrusion);
    assert!(near(c.plane().unwrap().apply(Vec3::ZERO), Vec3::new(5.0, 0.0, 0.0)));
    // Objects in the XY plane change in plan exactly as before; a uniform scale scales their
    // height too (SCALE is a 3D scale, as AutoCAD's is), a move or turn keeps it.
    let mut flat = circle(Vec3::new(1.0, 1.0, 2.0), 1.0);
    let mut kind = flat.kind.clone();
    let m = Mat3::rotate(0.3).then(Mat3::scale(2.0, 2.0));
    kind.transform(&m);
    kind.set_elevation(4.0);
    assert!(flat.transform(&m));
    assert_eq!(flat.kind, kind);
    let mut kind = flat.kind.clone();
    let m = Mat3::rotate(0.3).then(Mat3::translate(Vec2::new(3.0, -1.0)));
    kind.transform(&m);
    assert!(flat.transform(&m));
    assert_eq!(flat.kind, kind);
}

/// One plan SCALE scales every object's heights alike: standing panels, a raised shelf, a 3D
/// line and a thickness along a tilted direction (which keeps its angle).
#[test]
fn plan_scale_scales_heights_of_every_kind_alike() {
    let m = Mat3::scale_about(Vec2::ZERO, 0.5);
    // A side panel standing in the YZ plane, 700 tall.
    let mut side = ent(EntityKind::LwPolyline(LwPolyline {
        vertices: [(0.0, 0.0), (500.0, 0.0), (500.0, 700.0), (0.0, 700.0)].into_iter().map(|(x, y)| PolyVertex::new(Vec2::new(x, y))).collect(),
        closed: true,
        const_width: 0.0,
        elevation: 0.0,
        plinegen: false,
    }));
    side.common.extrusion = Vec3::X;
    assert!(side.transform(&m));
    let top = side.kind.grips().iter().map(|p| side.plane().unwrap().apply(p.to3(0.0)).z).fold(0.0, f64::max);
    assert!((top - 350.0).abs() < 1e-9, "{top}");
    // A shelf at elevation 400 comes down to 200, still level with the panels' middle.
    let mut shelf = circle(Vec3::new(0.0, 0.0, 400.0), 10.0);
    shelf.common.thickness = 18.0;
    assert!(shelf.transform(&m));
    assert_eq!(shelf.kind.elevation(), 200.0);
    assert_eq!((shelf.common.thickness, shelf.common.extrusion), (9.0, Vec3::Z));
    // A 3D line.
    let mut l = ent(EntityKind::Line(Line { a: Vec3::new(0.0, 0.0, 100.0), b: Vec3::new(10.0, 0.0, 300.0) }));
    assert!(l.transform(&m));
    let EntityKind::Line(k) = &l.kind else { panic!() };
    assert!(near(k.a, Vec3::new(0.0, 0.0, 50.0)) && near(k.b, Vec3::new(5.0, 0.0, 150.0)), "{k:?}");
    // A thickness along (0.6, 0, 0.8) scaled by 2 keeps its direction and doubles.
    let mut t = ent(EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::X }));
    (t.common.extrusion, t.common.thickness) = (Vec3::new(0.6, 0.0, 0.8), 10.0);
    assert!(t.transform(&Mat3::scale(2.0, 2.0)));
    assert!(near(t.common.extrusion, Vec3::new(0.6, 0.0, 0.8)) && (t.common.thickness - 20.0).abs() < 1e-9, "{:?}", t.common);
}

/// A file's extrusion of other than unit length is a direction: moving the object keeps its
/// thickness.
#[test]
fn a_non_unit_extrusion_keeps_its_thickness() {
    let mut l = ent(EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::X }));
    (l.common.extrusion, l.common.thickness) = (Vec3::new(0.0, -2.0, 0.0), 10.0);
    assert!(l.transform(&Mat3::translate(Vec2::new(5.0, 0.0))));
    assert!(near(l.common.extrusion, -Vec3::Y) && (l.common.thickness - 10.0).abs() < 1e-12, "{:?}", l.common);
    let mut p = ent(EntityKind::Point(Point { p: Vec3::ZERO, angle: 0.0 }));
    (p.common.extrusion, p.common.thickness) = (Vec3::new(0.0, 0.0, 3.0), 4.0);
    assert!(p.transform3(&Mat4::translate(Vec3::X)));
    assert_eq!((p.common.extrusion, p.common.thickness), (Vec3::Z, 4.0));
}

#[test]
fn a_block_stretched_along_its_normal_scales_its_z() {
    let ins = Insert {
        block: "B".into(),
        insert: Vec3::new(1.0, 1.0, 0.0),
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: vec![],
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    };
    let mut e = ent(EntityKind::Insert(ins));
    assert!(e.transform3(&Mat4::scale(Vec3::new(1.0, 1.0, 2.0))));
    let EntityKind::Insert(i) = &e.kind else { panic!() };
    assert_eq!(i.scale, Vec3::new(1.0, 1.0, 2.0));
    assert!(e.transform3(&Mat4::scale(Vec3::new(3.0, 3.0, 3.0))));
    let EntityKind::Insert(i) = &e.kind else { panic!() };
    assert!((i.scale.x - 3.0).abs() < 1e-12 && (i.scale.z - 6.0).abs() < 1e-12, "{:?}", i.scale);
}

fn near2(a: Vec2, b: Vec2) -> bool {
    (a - b).len() < 1e-9
}

/// A level object moved (and turned about Z by `turn`) goes up with its plane; turned about X it
/// is refused and left alone.
fn moves_level(e: Entity, turn: f64) {
    let name = e.kind.type_name();
    let m = Mat4::translate(Vec3::new(1.0, 2.0, 3.0)).mul(&Mat4::rotation(Vec3::Z, turn));
    let mut t = e.clone();
    assert!(t.transform3(&m), "{name}");
    assert!((t.kind.elevation() - e.kind.elevation() - 3.0).abs() < 1e-9, "{name}: {}", t.kind.elevation());
    assert_eq!(t.common.extrusion, Vec3::Z, "{name}");
    let (g0, g1) = (e.grips(), t.grips());
    assert_eq!(g0.len(), g1.len(), "{name}");
    for (a, b) in g0.iter().zip(&g1) {
        assert!(near2(m.plane_xy().apply(*a), *b), "{name}: {a:?} {b:?}");
    }
    let before = t.clone();
    assert!(!t.transform3(&Mat4::rotation(Vec3::X, 1.0)), "{name}");
    assert_eq!(t, before, "{name}");
}

#[test]
fn level_kinds_move_up_and_refuse_to_tilt() {
    let dim = Dimension {
        kind: DimKind::Aligned,
        defpt: Vec3::new(0.0, 2.0, 1.0),
        text_mid: Vec3::new(1.0, 2.5, 1.0),
        p13: Vec3::new(0.0, 0.0, 1.0),
        p14: Vec3::new(2.0, 0.0, 1.0),
        p15: Vec3::new(0.0, 0.0, 1.0),
        p16: Vec3::new(0.0, 0.0, 1.0),
        text: String::new(),
        style: "Standard".into(),
        measurement: 0.0,
        text_rotation: 0.0,
        user_text_pos: false,
        block: None,
        overrides: Default::default(),
        assoc: Vec::new(),
        marks: Default::default(),
    };
    let mline = MLine {
        style: "Standard".into(),
        def: Default::default(),
        scale: 1.0,
        justification: Default::default(),
        closed: false,
        no_start_caps: false,
        no_end_caps: false,
        vertices: vec![MLineVertex::new(Vec3::new(0.0, 0.0, 1.0)), MLineVertex::new(Vec3::new(4.0, 1.0, 1.0))],
    };
    let mleader = MLeader {
        leaders: vec![vec![Vec3::new(0.0, 0.0, 1.0)]],
        landing: Vec3::new(2.0, 1.0, 1.0),
        dogleg: 0.5,
        text: None,
        style: "Standard".into(),
        arrow_size: 1.0,
        spline: false,
        block: None,
    };
    let tol = Tolerance { insert: Vec3::new(1.0, 1.0, 1.0), dir: Vec3::X, style: "Standard".into(), text: "%%v0.1".into() };
    let table = Table { insert: Vec3::new(1.0, 1.0, 1.0), ..Table::default() };
    let image = Image { insert: Vec3::new(1.0, 1.0, 1.0), ..Image::default() };
    let vp = Viewport { center: Vec3::new(1.0, 1.0, 1.0), ..Viewport::default() };
    for k in [
        EntityKind::Dimension(dim),
        EntityKind::MLine(mline),
        EntityKind::MLeader(mleader),
        EntityKind::Tolerance(tol),
        EntityKind::Table(table),
        EntityKind::Image(image),
    ] {
        moves_level(ent(k), 0.5);
    }
    // A viewport doesn't turn.
    moves_level(ent(EntityKind::Viewport(vp)), 0.0);
    // A wipeout turns in the XY plane but stays on it.
    let mut w = ent(EntityKind::Wipeout(Wipeout { boundary: vec![Vec2::ZERO, Vec2::X, Vec2::Y] }));
    assert!(w.transform3(&Mat4::rotation(Vec3::Z, 1.0)));
    let before = w.clone();
    assert!(!w.transform3(&Mat4::translate(Vec3::Z)));
    assert!(!w.transform3(&Mat4::rotation(Vec3::X, 0.3)));
    assert_eq!(w, before);
}

#[test]
fn more_true_3d_kinds_and_planar_kinds_follow_the_transform() {
    let m = Mat4::translate(Vec3::new(1.0, -2.0, 3.0)).mul(&Mat4::rotation(Vec3::new(1.0, 1.0, 0.0), 0.7));
    let pts = [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 2.0, 1.0), Vec3::new(3.0, 1.0, -1.0), Vec3::new(2.0, 2.0, 2.0)];
    let points = |k: &EntityKind| match k {
        EntityKind::Point(p) => vec![p.p],
        EntityKind::Polyline3d(p) => p.points.clone(),
        EntityKind::Face3d(f) => f.corners.to_vec(),
        _ => vec![],
    };
    for k in [
        EntityKind::Point(Point { p: pts[0], angle: 0.0 }),
        EntityKind::Polyline3d(Polyline3d { points: pts.to_vec(), closed: true }),
        EntityKind::Face3d(Face3d { corners: pts, hidden_edges: 0 }),
    ] {
        let mut e = ent(k.clone());
        assert!(e.transform3(&m), "{}", k.type_name());
        let (a, b) = (points(&k), points(&e.kind));
        assert!(!a.is_empty() && a.len() == b.len(), "{}", k.type_name());
        for (p, q) in a.iter().zip(&b) {
            assert!(near(m.apply(*p), *q), "{}: {p:?} {q:?}", k.type_name());
        }
    }
    let mut x = ent(EntityKind::XLine(RayLine { base: pts[1], dir: Vec3::X }));
    assert!(x.transform3(&m));
    let EntityKind::XLine(r) = &x.kind else { panic!() };
    assert!(near(r.base, m.apply(pts[1])) && near(r.dir, m.apply_vec(Vec3::X).normalized()), "{r:?}");
    // An attribute definition and a trace keep their shape in their new plane.
    let def = Attrib {
        tag: "T".into(),
        text: text(),
        invisible: false,
        constant: false,
        prompt: String::new(),
        props: Default::default(),
        multiline: None,
        modes: Default::default(),
    };
    let trace = Solid { corners: [Vec3::new(0.0, 0.0, 3.0), Vec3::new(1.0, 0.0, 3.0), Vec3::new(0.0, 1.0, 3.0), Vec3::new(1.0, 1.0, 3.0)] };
    for e in [ent(EntityKind::AttDef(def)), ent(EntityKind::Trace(trace))] {
        let before: Vec<Vec3> = world_grips(&e).into_iter().map(|p| m.apply(p)).collect();
        let mut t = e.clone();
        assert!(t.transform3(&m), "{}", e.kind.type_name());
        let after = world_grips(&t);
        assert_eq!(before.len(), after.len());
        for (a, b) in before.iter().zip(&after) {
            assert!((*a - *b).len() < 1e-9, "{}: {a:?} {b:?}", e.kind.type_name());
        }
    }
}

#[test]
fn a_line_or_point_turns_its_thickness_with_it() {
    let mut l = ent(EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::X }));
    l.common.thickness = 2.0;
    assert!(l.transform3(&Mat4::rotation(Vec3::X, std::f64::consts::FRAC_PI_2)));
    assert!(near(l.common.extrusion, Vec3::new(0.0, -1.0, 0.0)) && l.common.thickness == 2.0, "{:?}", l.common);
    // Turned straight down: the default extrusion with the thickness negated (as files read).
    let mut p = ent(EntityKind::Point(Point { p: Vec3::ZERO, angle: 0.0 }));
    p.common.thickness = 1.5;
    assert!(p.transform3(&Mat4::mirror(Vec3::ZERO, Vec3::Z).mul(&Mat4::scale(Vec3::new(2.0, 2.0, 2.0)))));
    assert_eq!((p.common.extrusion, p.common.thickness), (Vec3::Z, -3.0));
}

#[test]
fn a_plan_transform_turns_a_line_or_points_thickness_too() {
    // ROTATE 90° in plan: thickness along +X turns to +Y; the default direction stays.
    let rot = Mat3::rotate(std::f64::consts::FRAC_PI_2);
    for kind in [EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::X }), EntityKind::Point(Point { p: Vec3::ZERO, angle: 0.0 })] {
        let mut e = ent(kind.clone());
        e.common.extrusion = Vec3::X;
        e.common.thickness = 2.0;
        assert!(e.transform(&rot));
        assert!(near(e.common.extrusion, Vec3::Y) && e.common.thickness == 2.0, "{:?}", e.common);
        let mut z = ent(kind);
        z.common.thickness = 2.0;
        assert!(z.transform(&rot));
        assert_eq!((z.common.extrusion, z.common.thickness), (Vec3::Z, 2.0));
    }
}

#[test]
fn plan_points_go_back_into_the_plane() {
    // Tilted 45° about X: the plan view is invertible, so the point lands exactly under `to`.
    let mut c = circle(Vec3::new(1.0, 2.0, 0.5), 1.0);
    assert!(c.transform3(&Mat4::rotation(Vec3::X, std::f64::consts::FRAC_PI_4)));
    let a = c.plan_xf().unwrap();
    let to = Vec2::new(3.0, -1.0);
    assert!(near2(a.apply(c.plan_to_local(to, Vec2::new(1.0, 2.0))), to));
    // Standing (seen edge-on): the smallest move in the plane, along what plan view shows.
    let mut s = circle(Vec3::new(1.0, 2.0, 3.0), 1.0);
    s.common.extrusion = Vec3::new(0.0, -1.0, 0.0);
    assert!(near2(s.plan_to_local(Vec2::new(4.0, 7.0), Vec2::new(1.0, 2.0)), Vec2::new(4.0, 2.0)));
    // In the XY plane: the point itself; hostile input stays finite.
    assert_eq!(circle(Vec3::ZERO, 1.0).plan_to_local(Vec2::new(5.0, 6.0), Vec2::ZERO), Vec2::new(5.0, 6.0));
    assert!(s.plan_to_local(Vec2::new(f64::NAN, 1e308), Vec2::ZERO).is_finite());
}

#[test]
fn a_block_reference_in_3d_scales_z_about_the_base_point() {
    let ins = Insert {
        block: "B".into(),
        insert: Vec3::new(5.0, 0.0, 7.0),
        scale: Vec3::new(2.0, 2.0, 3.0),
        rotation: 0.0,
        attribs: vec![],
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    };
    // The block transform: base point (1,1,1) to the insertion point's x and y, z by 3 about 1.
    let t = ins.transform3(Vec3::new(1.0, 1.0, 1.0));
    assert!(near(t.apply(Vec3::new(1.0, 2.0, 2.0)), Vec3::new(5.0, 2.0, 3.0)));
    // The reference's plane adds the insertion point's z.
    let e = ent(EntityKind::Insert(ins.clone()));
    assert!(near(e.plane().unwrap().mul(&t).apply(Vec3::new(1.0, 1.0, 1.0)), Vec3::new(5.0, 0.0, 7.0)));
    // A hostile z scale or base point counts as 1 at height 0.
    let bad = Insert { scale: Vec3::new(1.0, 1.0, f64::NAN), ..ins };
    assert!(bad.transform3(Vec3::new(0.0, 0.0, f64::INFINITY)).is_affine());
}

/// The Front construction plane: x = world X, y = world Z (normal -Y).
fn front() -> Mat4 {
    Mat4::from_cols(Vec3::X, Vec3::Z, -Vec3::Y, Vec3::ZERO)
}

#[test]
fn objects_seen_in_a_construction_plane() {
    // A circle standing in the XZ plane (as drawn in Front) is a circle in Front's coordinates,
    // a segment in plan.
    let mut c = circle(Vec3::new(1.0, 2.0, 0.0), 2.0);
    assert!(c.transform3(&front()));
    assert!(near(c.common.extrusion, -Vec3::Y));
    let on = c.on(&front()).unwrap();
    assert_eq!(on.common.extrusion, Vec3::Z);
    let EntityKind::Circle(k) = &on.kind else { panic!("{on:?}") };
    assert!(near(k.center, Vec3::new(1.0, 2.0, 0.0)) && (k.radius - 2.0).abs() < 1e-12);
    assert!(matches!(c.prims_on(&front()).as_slice(), [Prim::Circle(_)]));
    assert!(matches!(c.prims().as_slice(), [Prim::Seg(_)]));
    let g = c.grips_on(&front());
    assert!(g.first().is_some_and(|p| (*p - Vec2::new(1.0, 2.0)).len() < 1e-9), "{g:?}");
    // World: plan exactly as before.
    assert_eq!(c.on(&Mat4::IDENTITY).as_ref(), Some(&c));
    assert_eq!(c.prims_on(&Mat4::IDENTITY), c.prims());
    assert_eq!(c.grips_on(&Mat4::IDENTITY), c.grips());
    // A 3D line: its points in the plane's coordinates (z along the normal).
    let l = ent(EntityKind::Line(Line { a: Vec3::new(0.0, -4.0, 0.0), b: Vec3::new(3.0, -4.0, 5.0) }));
    let EntityKind::Line(ll) = l.on(&front()).unwrap().kind else { panic!() };
    assert!(near(ll.a, Vec3::new(0.0, 0.0, 4.0)) && near(ll.b, Vec3::new(3.0, 5.0, 4.0)));
    // What stays level can't stand in Front; a singular plane holds nothing.
    let lead = ent(EntityKind::Leader(Leader { vertices: vec![Vec3::ZERO, Vec3::X], arrow: true, spline: false, style: "Standard".into() }));
    assert!(lead.on(&front()).is_none() && lead.prims_on(&front()).is_empty() && lead.grips_on(&front()).is_empty());
    assert!(c.on(&Mat4::scale(Vec3::new(1.0, 0.0, 1.0))).is_none());
}
