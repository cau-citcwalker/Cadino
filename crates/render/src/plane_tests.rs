//! Objects drawn where they are in 3D: their own planes, elevations and 3D points.

use cadcraft_color::Rgb;
use cadcraft_doc::*;
use cadcraft_geom::{PolyVertex, Vec2, Vec3, ViewDir};

use crate::{DisplayList, Kind, Options, build};

/// FNV-1a over a display list's primitives, vertices and triangles, bit for bit.
pub(crate) fn digest(l: &DisplayList) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |v: u64| {
        for b in v.to_le_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    };
    for p in &l.prims {
        let kind = match p.kind {
            Kind::Polyline => 1,
            Kind::Tris => 2,
            Kind::Mask => 3,
            Kind::Point => 4,
            Kind::Infinite { ray } => 5 + u64::from(ray),
        };
        let Rgb(r, g, b) = p.color;
        for v in [p.handle.0, kind, u64::from(p.start), u64::from(p.len), u64::from(r), u64::from(g), u64::from(b)] {
            eat(v);
        }
        eat(u64::from(p.lw.to_bits()));
    }
    for v in l.verts.iter().chain(&l.tris) {
        eat(v.x.to_bits());
        eat(v.y.to_bits());
    }
    h
}

fn text(insert: Vec3, value: &str) -> Text {
    Text {
        insert,
        align_pt: None,
        height: 0.25,
        value: value.into(),
        rotation: 0.3,
        width_factor: 1.0,
        oblique: 0.0,
        style: "Standard".into(),
        halign: HAlign::Left,
        valign: VAlign::Baseline,
    }
}

/// Every kind of 2D object at z = 0 with the default extrusion, a block reference with
/// attributes, and (`with_3d`) 3D objects, whose plan view is their footprint.
pub(crate) fn flat_drawing(with_3d: bool) -> Drawing {
    let mut d = Drawing::new_imperial();
    d.linetypes.extend(cadcraft_doc::library::standard_linetypes());
    d.header.set_i64("PDMODE", 3);
    let c = Common::default;
    let dashed = || Common { linetype: "DASHED".into(), ..Common::default() };
    let v3 = Vec3::new;
    let add = |d: &mut Drawing, common: Common, k: EntityKind| {
        d.add(&Space::Model, common, k).unwrap();
    };
    add(&mut d, c(), EntityKind::Line(Line { a: v3(0.0, 0.0, 0.0), b: v3(10.0, 5.0, 0.0) }));
    add(&mut d, dashed(), EntityKind::Line(Line { a: v3(0.0, 1.0, 0.0), b: v3(10.0, 6.0, 0.0) }));
    add(&mut d, c(), EntityKind::Circle(Circle { center: v3(3.0, 4.0, 0.0), radius: 2.5 }));
    add(&mut d, dashed(), EntityKind::Arc(Arc { center: v3(1.0, 1.0, 0.0), radius: 1.5, start: 0.5, end: 2.0 }));
    add(&mut d, c(), EntityKind::Ellipse(Ellipse { center: v3(8.0, 8.0, 0.0), major: v3(3.0, 1.0, 0.0), ratio: 0.5, start: 0.3, end: 4.0 }));
    add(
        &mut d,
        c(),
        EntityKind::LwPolyline(LwPolyline {
            vertices: vec![PolyVertex::new(Vec2::ZERO), PolyVertex::with_bulge(Vec2::new(4.0, 0.0), 0.5), PolyVertex::new(Vec2::new(4.0, 3.0))],
            closed: true,
            const_width: 0.0,
            elevation: 0.0,
            plinegen: false,
        }),
    );
    add(
        &mut d,
        c(),
        EntityKind::LwPolyline(LwPolyline {
            vertices: vec![
                PolyVertex::new(Vec2::new(0.0, -3.0)),
                PolyVertex::with_bulge(Vec2::new(4.0, -3.0), 0.3),
                PolyVertex::new(Vec2::new(6.0, -1.0)),
            ],
            closed: false,
            const_width: 0.2,
            elevation: 0.0,
            plinegen: false,
        }),
    );
    add(
        &mut d,
        c(),
        EntityKind::Spline(cadcraft_geom::Spline::from_fit_points(&[Vec2::ZERO, Vec2::new(1.0, 2.0), Vec2::new(3.0, 1.0), Vec2::new(4.0, 3.0)])),
    );
    add(&mut d, c(), EntityKind::Text(text(v3(1.0, 1.0, 0.0), "Hello")));
    add(
        &mut d,
        c(),
        EntityKind::MText(MText {
            insert: v3(5.0, 5.0, 0.0),
            height: 0.2,
            width: 3.0,
            attach: 1,
            rotation: 0.2,
            style: "Standard".into(),
            contents: "line one\\Pline two".into(),
            line_spacing: 1.0,
            line_spacing_exact: false,
            columns: None,
        }),
    );
    add(
        &mut d,
        c(),
        EntityKind::Dimension(Dimension {
            kind: DimKind::Linear { rotation: 0.0 },
            defpt: v3(0.0, -1.0, 0.0),
            text_mid: Vec3::ZERO,
            p13: Vec3::ZERO,
            p14: v3(10.0, 0.0, 0.0),
            p15: Vec3::ZERO,
            p16: Vec3::ZERO,
            text: String::new(),
            style: "Standard".into(),
            measurement: 10.0,
            text_rotation: 0.0,
            user_text_pos: false,
            block: None,
            overrides: Default::default(),
            assoc: Vec::new(),
            marks: Default::default(),
        }),
    );
    let sq = |x: f64| {
        vec![
            PolyVertex::new(Vec2::new(x, 10.0)),
            PolyVertex::new(Vec2::new(x + 4.0, 10.0)),
            PolyVertex::with_bulge(Vec2::new(x + 4.0, 14.0), 0.4),
            PolyVertex::new(Vec2::new(x, 14.0)),
        ]
    };
    for (pattern, solid, x) in [("ANSI31", false, 0.0), ("SOLID", true, 6.0)] {
        add(
            &mut d,
            c(),
            EntityKind::Hatch(Hatch {
                pattern: pattern.into(),
                solid,
                loops: vec![HatchLoop { vertices: sq(x), outer: true, sources: Vec::new() }],
                scale: 1.0,
                angle: 0.0,
                associative: false,
                style: 0,
                elevation: 0.0,
                gradient: None,
                origin: Vec2::ZERO,
                background: None,
                pattern_lines: Vec::new(),
            }),
        );
    }
    add(&mut d, c(), EntityKind::Solid(Solid { corners: [v3(12.0, 0.0, 0.0), v3(14.0, 0.0, 0.0), v3(12.0, 2.0, 0.0), v3(14.0, 3.0, 0.0)] }));
    add(&mut d, c(), EntityKind::Point(Point { p: v3(15.0, 1.0, 0.0), angle: 0.0 }));
    add(&mut d, c(), EntityKind::Ray(RayLine { base: v3(0.0, 20.0, 0.0), dir: v3(1.0, 0.2, 0.0) }));
    add(&mut d, c(), EntityKind::XLine(RayLine { base: v3(0.0, 21.0, 0.0), dir: v3(0.3, 1.0, 0.0) }));
    add(
        &mut d,
        c(),
        EntityKind::Leader(Leader {
            vertices: vec![v3(16.0, 0.0, 0.0), v3(18.0, 2.0, 0.0), v3(20.0, 2.0, 0.0)],
            arrow: true,
            spline: false,
            style: "Standard".into(),
        }),
    );
    let mut b = Block::new("Bolt");
    b.entities.push(Entity::new(Handle(0x50), EntityKind::Circle(Circle { center: Vec3::ZERO, radius: 0.25 })));
    b.entities.push(Entity::new(Handle(0x51), EntityKind::Line(Line { a: v3(-0.5, 0.0, 0.0), b: v3(0.5, 0.0, 0.0) })));
    d.blocks.insert("Bolt".into(), std::sync::Arc::new(b));
    let attrib = Attrib {
        tag: "N".into(),
        text: text(v3(2.5, 2.0, 0.0), "A1"),
        invisible: false,
        constant: false,
        prompt: String::new(),
        props: Default::default(),
        multiline: None,
        modes: Default::default(),
    };
    add(
        &mut d,
        c(),
        EntityKind::Insert(Insert {
            block: "Bolt".into(),
            insert: v3(2.0, 2.0, 0.0),
            scale: v3(1.5, 1.5, 1.0),
            rotation: 0.4,
            attribs: vec![attrib],
            cols: 2,
            rows: 1,
            col_spacing: 3.0,
            row_spacing: 0.0,
        }),
    );
    if with_3d {
        add(&mut d, c(), EntityKind::Line(Line { a: v3(0.0, 0.0, 1.0), b: v3(5.0, 5.0, 3.0) }));
        add(&mut d, c(), EntityKind::Polyline3d(Polyline3d { points: vec![v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 2.0), v3(1.0, 1.0, 4.0)], closed: true }));
        add(
            &mut d,
            c(),
            EntityKind::Face3d(Face3d { corners: [v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 1.0), v3(1.0, 1.0, 1.0), v3(0.0, 1.0, 0.0)], hidden_edges: 0 }),
        );
        add(&mut d, c(), EntityKind::Point(Point { p: v3(3.0, 3.0, 5.0), angle: 0.0 }));
        add(&mut d, c(), EntityKind::Circle(Circle { center: v3(3.0, 4.0, 7.0), radius: 1.0 }));
        add(&mut d, c(), EntityKind::Ray(RayLine { base: v3(0.0, 30.0, 0.0), dir: v3(1.0, 0.0, 1.0) }));
        add(
            &mut d,
            c(),
            EntityKind::Helix(Helix {
                center: v3(20.0, 20.0, 0.0),
                base_radius: 2.0,
                top_radius: 1.0,
                height: 5.0,
                turns: 3.0,
                start_angle: 0.0,
                ccw: true,
                constrain: Default::default(),
            }),
        );
    }
    d
}

fn opts(view_dir: ViewDir) -> Options {
    Options { tolerance: 0.01, view_dir, ..Options::default() }
}

#[test]
fn plan_and_flat_lists_are_unchanged() {
    // Every object of a flat drawing, plus 3D ones in plan.
    let iso = ViewDir::new(Vec3::new(-1.0, -1.0, 1.0));
    let front = ViewDir::new(Vec3::new(0.0, -1.0, 0.0));
    let got = [
        digest(&build(&flat_drawing(true), &Space::Model, &opts(ViewDir::PLAN))),
        digest(&build(&flat_drawing(false), &Space::Model, &opts(iso))),
        digest(&build(&flat_drawing(false), &Space::Model, &opts(front))),
    ];
    // Built by the renderer before objects were drawn in their 3D planes (a 3D view of a drawing
    // that is all at z = 0 is unchanged too).
    assert_eq!(got, [1080351100466985796, 16819804423907798368, 11451345774524608355]);
}

fn one(kind: EntityKind, common: Common) -> Drawing {
    let mut d = Drawing::new_imperial();
    d.add(&Space::Model, common, kind).unwrap();
    d
}

fn circle(center: Vec3) -> EntityKind {
    EntityKind::Circle(Circle { center, radius: 1.0 })
}

fn iso() -> ViewDir {
    ViewDir::new(Vec3::new(-1.0, -1.0, 1.0))
}

fn front() -> ViewDir {
    ViewDir::new(Vec3::new(0.0, -1.0, 0.0))
}

fn assert_same(a: &[Vec2], b: &[Vec2], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}");
    for (p, q) in a.iter().zip(b) {
        assert!(p.dist(*q) < 1e-9, "{what}: {p:?} {q:?}");
    }
}

#[test]
fn a_raised_circle_draws_at_its_height() {
    let flat = build(&one(circle(Vec3::ZERO), Common::default()), &Space::Model, &opts(iso()));
    let raised = build(&one(circle(Vec3::new(0.0, 0.0, 5.0)), Common::default()), &Space::Model, &opts(iso()));
    let up = iso().project(Vec3::new(0.0, 0.0, 5.0));
    let moved: Vec<Vec2> = flat.verts.iter().map(|v| *v + up).collect();
    assert_same(&raised.verts, &moved, "iso");
    // In plan it is where it always was.
    let plan = build(&one(circle(Vec3::new(0.0, 0.0, 5.0)), Common::default()), &Space::Model, &opts(ViewDir::PLAN));
    assert_same(&plan.verts, &build(&one(circle(Vec3::ZERO), Common::default()), &Space::Model, &opts(ViewDir::PLAN)).verts, "plan");
}

#[test]
fn a_standing_circle_is_round_in_front_and_a_line_in_plan() {
    // Facing -Y at elevation 2: in the plane y = -2, centre (3, -2, 4).
    let standing = Common { extrusion: Vec3::new(0.0, -1.0, 0.0), ..Common::default() };
    let d = one(circle(Vec3::new(3.0, 4.0, 2.0)), standing);
    let f = build(&d, &Space::Model, &opts(front()));
    assert!(f.verts.len() > 8);
    assert!(f.verts.iter().all(|v| (v.dist(Vec2::new(3.0, 4.0)) - 1.0).abs() < 0.02), "{:?}", f.bounds);
    let p = build(&d, &Space::Model, &opts(ViewDir::PLAN));
    assert!(p.bounds.height() < 1e-9 && (p.bounds.min.y + 2.0).abs() < 1e-9 && (p.bounds.width() - 2.0).abs() < 0.02, "{:?}", p.bounds);
}

#[test]
fn text_stood_up_reads_in_front_as_it_did_in_plan() {
    let t = EntityKind::Text(text(Vec3::new(1.0, 2.0, 0.0), "Side"));
    let plan = build(&one(t.clone(), Common::default()), &Space::Model, &opts(ViewDir::PLAN));
    let mut e = Entity::new(Handle(1), t);
    assert!(e.transform3(&cadcraft_geom::Mat4::rotation(Vec3::X, std::f64::consts::FRAC_PI_2)));
    let stood = build(&one(e.kind, e.common), &Space::Model, &opts(front()));
    assert!(!plan.verts.is_empty());
    assert_same(&stood.verts, &plan.verts, "verts");
    assert_same(&stood.tris, &plan.tris, "tris");
}

#[test]
fn a_block_reference_draws_in_its_plane() {
    let mut d = Drawing::new_imperial();
    let mut b = Block::new("B");
    b.entities.push(Entity::new(Handle(0x50), circle(Vec3::ZERO)));
    d.blocks.insert("B".into(), std::sync::Arc::new(b));
    let ins = Insert {
        block: "B".into(),
        insert: Vec3::new(5.0, 0.0, 1.0),
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: vec![],
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    };
    d.add(&Space::Model, Common { extrusion: Vec3::new(0.0, -1.0, 0.0), ..Common::default() }, EntityKind::Insert(ins)).unwrap();
    // OCS (5, 0) is world x 5, z 0: round about (5, 0) in front.
    let f = build(&d, &Space::Model, &opts(front()));
    assert!(f.verts.len() > 8 && f.verts.iter().all(|v| (v.dist(Vec2::new(5.0, 0.0)) - 1.0).abs() < 0.02), "{:?}", f.bounds);
}

#[test]
fn block_contents_take_the_z_scale_about_the_base_point() {
    // A post from z 1 to 2 in a block based at z 1, referenced with z scale `sz` at height 4.
    let drawn = |sz: f64| {
        let mut d = Drawing::new_imperial();
        let mut b = Block::new("B");
        b.base = Vec3::new(0.0, 0.0, 1.0);
        b.entities.push(Entity::new(Handle(0x50), EntityKind::Line(Line { a: Vec3::new(0.0, 0.0, 1.0), b: Vec3::new(0.0, 0.0, 2.0) })));
        d.blocks.insert("B".into(), std::sync::Arc::new(b));
        let ins = Insert {
            block: "B".into(),
            insert: Vec3::new(3.0, 0.0, 4.0),
            scale: Vec3::new(1.0, 1.0, sz),
            rotation: 0.0,
            attribs: vec![],
            cols: 1,
            rows: 1,
            col_spacing: 0.0,
            row_spacing: 0.0,
        };
        d.add(&Space::Model, Common::default(), EntityKind::Insert(ins)).unwrap();
        build(&d, &Space::Model, &opts(front())).verts
    };
    // Seen from the front (x across, z up): from 4 up by sz.
    assert_same(&drawn(3.0), &[Vec2::new(3.0, 4.0), Vec2::new(3.0, 7.0)], "z scale 3");
    // Mirrored in z (MIRROR3D across a level plane, or a file's (0,0,-1) reference): downwards.
    assert_same(&drawn(-1.0), &[Vec2::new(3.0, 4.0), Vec2::new(3.0, 3.0)], "z scale -1");
    // A hostile z scale draws as 1.
    assert_same(&drawn(f64::NAN), &[Vec2::new(3.0, 4.0), Vec2::new(3.0, 5.0)], "z scale NaN");
}

#[test]
fn flat_block_contents_draw_at_the_lifted_height() {
    // A circle at block z 0 in a block based at z 1, referenced at height 4 with z scale `sz`:
    // it lies at 4 + sz (0 - 1), where a top-level circle there draws, in 3D views.
    let drawn = |sz: f64, view: ViewDir| {
        let mut d = Drawing::new_imperial();
        let mut b = Block::new("B");
        b.base = Vec3::new(0.0, 0.0, 1.0);
        b.entities.push(Entity::new(Handle(0x50), circle(Vec3::ZERO)));
        d.blocks.insert("B".into(), std::sync::Arc::new(b));
        let ins = Insert {
            block: "B".into(),
            insert: Vec3::new(3.0, 0.0, 4.0),
            scale: Vec3::new(1.0, 1.0, sz),
            rotation: 0.0,
            attribs: vec![],
            cols: 1,
            rows: 1,
            col_spacing: 0.0,
            row_spacing: 0.0,
        };
        d.add(&Space::Model, Common::default(), EntityKind::Insert(ins)).unwrap();
        build(&d, &Space::Model, &opts(view)).verts
    };
    let at = |z: f64, view: ViewDir| build(&one(circle(Vec3::new(3.0, 0.0, z)), Common::default()), &Space::Model, &opts(view)).verts;
    for view in [iso(), front()] {
        assert_same(&drawn(3.0, view), &at(1.0, view), "z scale 3");
        assert_same(&drawn(1.0, view), &at(3.0, view), "raised");
    }
    // In plan it is where it always was.
    assert_same(&drawn(3.0, ViewDir::PLAN), &at(0.0, ViewDir::PLAN), "plan");
}

#[test]
fn true_3d_objects_project_their_points() {
    let line = EntityKind::Line(Line { a: Vec3::new(0.0, 0.0, 1.0), b: Vec3::new(4.0, 3.0, 5.0) });
    let f = build(&one(line.clone(), Common::default()), &Space::Model, &opts(front()));
    assert_same(&f.verts, &[Vec2::new(0.0, 1.0), Vec2::new(4.0, 5.0)], "front line");
    // Dashed, the dashes stay on it.
    let dashed = one(line, Common { linetype: "DASHED".into(), ..Common::default() });
    let mut dd = dashed.clone();
    dd.linetypes.extend(cadcraft_doc::library::standard_linetypes());
    let l = build(&dd, &Space::Model, &opts(front()));
    assert!(l.prims.len() > 2);
    assert!(l.verts.iter().all(|v| (v.y - 1.0 - v.x).abs() < 1e-9), "{:?}", l.verts);
    let poly = EntityKind::Polyline3d(Polyline3d { points: vec![Vec3::ZERO, Vec3::new(1.0, 0.0, 2.0), Vec3::new(1.0, 1.0, 4.0)], closed: false });
    assert_same(
        &build(&one(poly, Common::default()), &Space::Model, &opts(front())).verts,
        &[Vec2::ZERO, Vec2::new(1.0, 2.0), Vec2::new(1.0, 4.0)],
        "3D polyline",
    );
    let helix = EntityKind::Helix(Helix {
        center: Vec3::new(0.0, 0.0, 1.0),
        base_radius: 2.0,
        top_radius: 2.0,
        height: 5.0,
        turns: 3.0,
        start_angle: 0.0,
        ccw: true,
        constrain: Default::default(),
    });
    let h = build(&one(helix, Common::default()), &Space::Model, &opts(front()));
    assert!((h.bounds.min.y - 1.0).abs() < 1e-9 && (h.bounds.max.y - 6.0).abs() < 1e-9 && (h.bounds.width() - 4.0).abs() < 0.05, "{:?}", h.bounds);
    // A vertical ray seen from the front points up.
    let ray = EntityKind::Ray(RayLine { base: Vec3::new(1.0, 0.0, 0.0), dir: Vec3::new(0.0, 0.0, 1.0) });
    let r = build(&one(ray, Common::default()), &Space::Model, &opts(front()));
    assert_eq!(r.prims[0].kind, Kind::Infinite { ray: true });
    assert_same(&r.verts, &[Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)], "ray");
    // Seen end-on from above it is a point.
    let r = build(&one(EntityKind::Ray(RayLine { base: Vec3::ZERO, dir: Vec3::Z }), Common::default()), &Space::Model, &opts(iso()));
    assert_eq!(r.prims.len(), 1);
}

#[test]
fn shaded_depths_follow_the_plane() {
    let d = one(circle(Vec3::new(0.0, 0.0, 5.0)), Common::default());
    let list = build(&d, &Space::Model, &Options { shaded: true, ..opts(iso()) });
    assert_eq!(list.vert_depth.len(), list.verts.len());
    let want = iso().depth(Vec3::new(1.0, 0.0, 5.0));
    // The first vertex is the circle's point at angle 0.
    assert!((list.vert_depth[0] - want).abs() < 1e-9, "{} {want}", list.vert_depth[0]);
    let line = one(EntityKind::Line(Line { a: Vec3::new(0.0, 0.0, 1.0), b: Vec3::new(4.0, 3.0, 5.0) }), Common::default());
    let l = build(&line, &Space::Model, &Options { shaded: true, ..opts(iso()) });
    assert!((l.vert_depth[1] - iso().depth(Vec3::new(4.0, 3.0, 5.0))).abs() < 1e-9);
}

#[test]
fn hostile_planes_do_not_panic() {
    for n in [Vec3::new(f64::NAN, 0.0, 1.0), Vec3::ZERO, Vec3::new(1e308, -1e308, 1e308), Vec3::new(0.0, 0.0, -1.0)] {
        for z in [0.0, f64::INFINITY, 1e300] {
            let d = one(circle(Vec3::new(1.0, 1.0, z)), Common { extrusion: n, ..Common::default() });
            for v in [ViewDir::PLAN, iso(), front()] {
                let _ = build(&d, &Space::Model, &Options { shaded: true, ..opts(v) });
            }
        }
    }
}
