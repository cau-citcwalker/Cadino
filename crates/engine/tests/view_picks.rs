//! Picks and typed selection modes in views that pick what is drawn (3D views, plan with a tilted
//! construction plane; `Session::view_at`): a pick at a prompt that takes a point to pick one
//! object (OFFSET, FILLET, CHAMFER, PEDIT, LENGTHEN, BREAK, TRIM, EXTEND) comes as the point of
//! the object under the cursor, and that object is picked there even where plan shows another;
//! W, C, F, WP, CP and BOX select what the view shows in them.

use cadcraft_engine::doc::{EntityKind, Handle, Line, Text};
use cadcraft_engine::geom::{Vec2, Vec3, ViewDir};
use cadcraft_engine::{Input, Session};
use serde_json::json;

fn v3(p: [f64; 3]) -> Vec3 {
    Vec3::new(p[0], p[1], p[2])
}

fn line(s: &mut Session, a: [f64; 3], b: [f64; 3]) -> Handle {
    s.add_entity_world(EntityKind::Line(Line { a: v3(a), b: v3(b) })).unwrap()
}

/// Plan lines `ls` twice: 30 up, then on the ground (drawn last, so a pick in plan at their
/// shadow takes the ground copy). Returns (raised, ground) handles, looking from the south-west.
fn stack(ls: &[([f64; 2], [f64; 2])]) -> (Session, Vec<Handle>, Vec<Handle>) {
    let mut s = Session::new();
    let up: Vec<Handle> = ls.iter().map(|(a, b)| line(&mut s, [a[0], a[1], 30.0], [b[0], b[1], 30.0])).collect();
    let low: Vec<Handle> = ls.iter().map(|(a, b)| line(&mut s, [a[0], a[1], 0.0], [b[0], b[1], 0.0])).collect();
    s.execute("view.swiso", &json!({})).unwrap();
    (s, up, low)
}

fn dir(s: &Session) -> ViewDir {
    s.state().unwrap().view_dir
}

/// A click in the current view on world point `w` of object `h`, as the canvas gives it there.
fn click_on(s: &mut Session, h: Handle, w: [f64; 3]) {
    let d = dir(s);
    s.view_at = Some((d, d.project(v3(w))));
    s.input_view_pick(vec![h]).unwrap();
    s.view_at = None;
}

fn ends(s: &Session, h: Handle) -> (Vec3, Vec3) {
    match &s.doc().unwrap().entity(h).unwrap().kind {
        EntityKind::Line(l) => (l.a, l.b),
        k => panic!("not a line: {k:?}"),
    }
}

fn near(a: Vec3, b: [f64; 3]) -> bool {
    (a - v3(b)).len() < 1e-6
}

/// The lines of the drawing other than `but`, as (a, b).
fn other_lines(s: &Session, but: &[Handle]) -> Vec<(Vec3, Vec3)> {
    s.doc()
        .unwrap()
        .model
        .iter()
        .filter(|e| !but.contains(&e.handle))
        .filter_map(|e| if let EntityKind::Line(l) = &e.kind { Some((l.a, l.b)) } else { None })
        .collect()
}

fn prompt(s: &Session) -> String {
    s.current_prompt().map(|p| p.display()).unwrap_or_default()
}

#[test]
fn offset_takes_the_raised_line_picked_in_a_3d_view() {
    let (mut s, up, low) = stack(&[([0.0, 0.0], [100.0, 0.0])]);
    s.cmdline("offset").unwrap();
    s.cmdline("10").unwrap();
    click_on(&mut s, up[0], [50.0, 0.0, 30.0]);
    assert_eq!(s.selection(), vec![up[0]], "{}", prompt(&s));
    s.cmdline("50,20").unwrap();
    s.cmdline("").unwrap();
    let made = other_lines(&s, &[up[0], low[0]]);
    assert_eq!(made.len(), 1);
    assert!(near(made[0].0, [0.0, 10.0, 30.0]) && near(made[0].1, [100.0, 10.0, 30.0]), "{made:?}");
}

#[test]
fn fillet_and_chamfer_join_the_raised_corner_picked_in_a_3d_view() {
    for cmd in [&["fillet", "r", "0"][..], &["chamfer", "d", "0", "0"]] {
        let (mut s, up, low) = stack(&[([0.0, 0.0], [100.0, 0.0]), ([110.0, 10.0], [110.0, 100.0])]);
        for t in cmd {
            s.cmdline(t).unwrap();
        }
        click_on(&mut s, up[0], [50.0, 0.0, 30.0]);
        click_on(&mut s, up[1], [110.0, 50.0, 30.0]);
        assert!(s.running.is_none(), "{}: {}", cmd[0], prompt(&s));
        // Either way round, still 30 up.
        let between = |h: Handle, p: [f64; 3], q: [f64; 3]| {
            let (a, b) = ends(&s, h);
            (near(a, p) && near(b, q)) || (near(a, q) && near(b, p))
        };
        assert!(between(up[0], [0.0, 0.0, 30.0], [110.0, 0.0, 30.0]), "{}: {:?}", cmd[0], ends(&s, up[0]));
        assert!(between(up[1], [110.0, 0.0, 30.0], [110.0, 100.0, 30.0]), "{}: {:?}", cmd[0], ends(&s, up[1]));
        // The ground corner below is left alone.
        assert_eq!(ends(&s, low[0]), (v3([0.0, 0.0, 0.0]), v3([100.0, 0.0, 0.0])));
        assert_eq!(ends(&s, low[1]), (v3([110.0, 10.0, 0.0]), v3([110.0, 100.0, 0.0])));
    }
}

#[test]
fn pedit_turns_the_raised_line_picked_in_a_3d_view_into_a_polyline() {
    let (mut s, up, low) = stack(&[([0.0, 0.0], [100.0, 0.0])]);
    s.cmdline("pedit").unwrap();
    click_on(&mut s, up[0], [50.0, 0.0, 30.0]);
    s.cmdline("").unwrap();
    s.cmdline("x").unwrap();
    let d = s.doc().unwrap();
    assert!(matches!(&d.entity(low[0]).unwrap().kind, EntityKind::Line(_)));
    let polys: Vec<f64> = d.model.iter().filter_map(|e| if let EntityKind::LwPolyline(p) = &e.kind { Some(p.elevation) } else { None }).collect();
    assert_eq!(polys, vec![30.0]);
}

#[test]
fn lengthen_changes_the_raised_line_at_the_end_picked_in_a_3d_view() {
    let (mut s, up, low) = stack(&[([0.0, 0.0], [100.0, 0.0])]);
    for t in ["lengthen", "de", "10"] {
        s.cmdline(t).unwrap();
    }
    click_on(&mut s, up[0], [90.0, 0.0, 30.0]);
    s.cmdline("").unwrap();
    let (a, b) = ends(&s, up[0]);
    assert!(near(a, [0.0, 0.0, 30.0]) && near(b, [110.0, 0.0, 30.0]), "{a:?} {b:?}");
    assert_eq!(ends(&s, low[0]).1, v3([100.0, 0.0, 0.0]));
}

#[test]
fn break_starts_where_the_raised_line_is_picked_in_a_3d_view() {
    let (mut s, up, low) = stack(&[([0.0, 0.0], [100.0, 0.0])]);
    s.cmdline("break").unwrap();
    click_on(&mut s, up[0], [30.0, 0.0, 30.0]);
    s.cmdline("60,0").unwrap();
    assert!(s.running.is_none(), "{}", prompt(&s));
    // The pieces stay 30 up.
    let mut rest = other_lines(&s, &[low[0]]);
    rest.sort_by(|p, q| p.0.x.total_cmp(&q.0.x));
    assert!(rest.len() == 2 && near(rest[0].0, [0.0, 0.0, 30.0]) && near(rest[0].1, [30.0, 0.0, 30.0]), "{rest:?}");
    assert!(near(rest[1].0, [60.0, 0.0, 30.0]) && near(rest[1].1, [100.0, 0.0, 30.0]), "{rest:?}");
    assert!(s.doc().unwrap().entity(up[0]).is_none());
    assert_eq!(ends(&s, low[0]), (v3([0.0, 0.0, 0.0]), v3([100.0, 0.0, 0.0])));
}

#[test]
fn trim_and_extend_change_the_raised_line_picked_in_a_3d_view() {
    // TRIM at a cutting edge across x = 50.
    let (mut s, up, low) = stack(&[([0.0, 0.0], [100.0, 0.0]), ([50.0, -10.0], [50.0, 10.0])]);
    s.cmdline("trim").unwrap();
    click_on(&mut s, up[0], [80.0, 0.0, 30.0]);
    s.cmdline("").unwrap();
    let kept = other_lines(&s, &[low[0], low[1], up[1]]);
    assert!(kept.len() == 1 && near(kept[0].0, [0.0, 0.0, 30.0]) && near(kept[0].1, [50.0, 0.0, 30.0]), "{kept:?}");
    assert_eq!(ends(&s, low[0]).1, v3([100.0, 0.0, 0.0]));
    // EXTEND to a boundary across x = 150.
    let (mut s, up, low) = stack(&[([0.0, 0.0], [100.0, 0.0]), ([150.0, -10.0], [150.0, 10.0])]);
    s.cmdline("extend").unwrap();
    click_on(&mut s, up[0], [90.0, 0.0, 30.0]);
    s.cmdline("").unwrap();
    let (a, b) = ends(&s, up[0]);
    assert!(near(a, [0.0, 0.0, 30.0]) && near(b, [150.0, 0.0, 30.0]), "{a:?} {b:?}");
    assert_eq!(ends(&s, low[0]).1, v3([100.0, 0.0, 0.0]));
}

#[test]
fn join_takes_the_raised_lines_picked_in_a_3d_view() {
    let (mut s, up, low) = stack(&[([0.0, 0.0], [50.0, 0.0]), ([50.0, 0.0], [100.0, 0.0])]);
    s.cmdline("join").unwrap();
    click_on(&mut s, up[0], [25.0, 0.0, 30.0]);
    click_on(&mut s, up[1], [75.0, 0.0, 30.0]);
    s.cmdline("").unwrap();
    let d = s.doc().unwrap();
    assert!(d.entity(low[0]).is_some() && d.entity(low[1]).is_some());
    // One polyline through both, still 30 up.
    assert!(other_lines(&s, &low).is_empty());
    let polys: Vec<(f64, Vec<Vec2>)> = d
        .model
        .iter()
        .filter_map(|e| if let EntityKind::LwPolyline(p) = &e.kind { Some((p.elevation, p.vertices.iter().map(|v| v.p).collect())) } else { None })
        .collect();
    assert_eq!(polys, vec![(30.0, vec![Vec2::new(0.0, 0.0), Vec2::new(50.0, 0.0), Vec2::new(100.0, 0.0)])]);
}

/// In plan (World) a pick is a click on the plan: a point picks the topmost object there, and a
/// view pick without a view (what the canvas sends in plan never is one) stays a pick.
#[test]
fn plan_picks_are_unchanged() {
    let (mut s, up, low) = stack(&[([0.0, 0.0], [100.0, 0.0])]);
    s.execute("plan", &json!({})).unwrap();
    s.cmdline("offset").unwrap();
    s.cmdline("10").unwrap();
    s.input_view_pick(vec![up[0]]).unwrap();
    assert!(s.selection().is_empty() && prompt(&s).starts_with("Select object to offset"), "{}", prompt(&s));
    s.cmdline("50,0").unwrap();
    assert_eq!(s.selection(), vec![low[0]]);
}

/// A level UCS (moved and turned) in a 3D view: OFFSET runs in World with the picked point taken
/// there, and still takes the raised line.
#[test]
fn a_level_ucs_takes_the_raised_line_picked_in_a_3d_view() {
    let (mut s, up, low) = stack(&[([0.0, 0.0], [100.0, 0.0])]);
    for t in ["ucs 100,50", "", "ucs z 90"] {
        s.cmdline(t).unwrap();
    }
    s.cmdline("offset").unwrap();
    s.cmdline("10").unwrap();
    click_on(&mut s, up[0], [50.0, 0.0, 30.0]);
    assert_eq!(s.selection(), vec![up[0]], "{}", prompt(&s));
    let side = s.cplane().inverse().unwrap().apply(v3([50.0, 20.0, 0.0])).xy();
    s.cmdline(&format!("{},{}", side.x, side.y)).unwrap();
    s.cmdline("").unwrap();
    let made = other_lines(&s, &[up[0], low[0]]);
    assert!(made.len() == 1 && near(made[0].0, [0.0, 10.0, 30.0]) && near(made[0].1, [100.0, 10.0, 30.0]), "{made:?}");
}

/// In plan with a tilted plane (Front) the plan-only commands are refused as before, and a
/// point-only object prompt that works there (TEXTEDIT's) takes the object picked where plan
/// shows it, though in the plane's coordinates another lies over it.
#[test]
fn a_tilted_plane_in_plan_takes_the_object_picked_where_it_is_drawn() {
    let mut s = Session::new();
    s.execute("cplane", &json!({ "preset": "front" })).unwrap();
    assert!(s.cmdline("offset").is_err() || s.running.is_none());
    let text = |value: &str, z: f64| {
        EntityKind::Text(Text {
            insert: Vec3::new(0.0, 0.0, z),
            align_pt: None,
            height: 2.0,
            value: value.into(),
            rotation: 0.0,
            width_factor: 1.0,
            oblique: 0.0,
            style: "Standard".into(),
            halign: Default::default(),
            valign: Default::default(),
        })
    };
    // In the Front plane 20 apart along its normal (-Y): at world y = -20 and y = 0.
    s.add_entity(text("far", 20.0)).unwrap();
    s.add_entity(text("near", 0.0)).unwrap();
    let far = s.doc().unwrap().model.iter().find(|e| matches!(&e.kind, EntityKind::Text(t) if t.value == "far")).unwrap().handle;
    s.cmdline("textedit").unwrap();
    let plan = ViewDir::PLAN;
    s.view_at = Some((plan, Vec2::new(1.0, -20.0)));
    s.input_view_pick(vec![far]).unwrap();
    s.view_at = None;
    assert!(prompt(&s).contains("<far>"), "{}", prompt(&s));
}

/// Corners in view-plane coordinates around world points `ws`, `pad` out.
fn around(d: ViewDir, ws: &[[f64; 3]], pad: f64) -> (Vec2, Vec2) {
    let ps: Vec<Vec2> = ws.iter().map(|w| d.project(v3(*w))).collect();
    let lo = ps.iter().fold(Vec2::new(f64::MAX, f64::MAX), |m, p| Vec2::new(m.x.min(p.x), m.y.min(p.y)));
    let hi = ps.iter().fold(Vec2::new(f64::MIN, f64::MIN), |m, p| Vec2::new(m.x.max(p.x), m.y.max(p.y)));
    (lo - Vec2::new(pad, pad), hi + Vec2::new(pad, pad))
}

/// Type `mode` at ERASE's "Select objects", click view points `vs` (the canvas gives each its
/// point on the construction plane, here the XY plane's), Enter: what is left.
fn erase_by(s: &mut Session, mode: &str, vs: &[Vec2], seen: bool) -> Vec<Handle> {
    let d = dir(s);
    let all = s.doc().unwrap().model.len();
    s.cmdline("erase").unwrap();
    s.cmdline(mode).unwrap();
    for v in vs {
        s.view_at = seen.then_some((d, *v));
        s.input(Input::Point(d.unproject_xy(*v))).unwrap();
        s.view_at = None;
    }
    if !matches!(mode, "w" | "c" | "box") {
        s.input(Input::Enter).unwrap();
    }
    s.input(Input::Enter).unwrap();
    let left: Vec<Handle> = s.doc().unwrap().model.iter().map(|e| e.handle).collect();
    if left.len() < all {
        s.cmdline("u").unwrap();
    }
    left
}

#[test]
fn typed_modes_in_a_3d_view_select_what_is_drawn() {
    let mut s = Session::new();
    let low = line(&mut s, [0.0, 0.0, 0.0], [10.0, 0.0, 0.0]);
    let high = line(&mut s, [0.0, 0.0, 100.0], [10.0, 0.0, 100.0]);
    s.execute("view.swiso", &json!({})).unwrap();
    let d = dir(&s);
    let (lo, hi) = around(d, &[[0.0, 0.0, 100.0], [10.0, 0.0, 100.0]], 1.0);
    let mid = |a: [f64; 3]| d.project(v3(a));
    // A window and a crossing round what is drawn up there, either way round for W and C.
    assert_eq!(erase_by(&mut s, "w", &[lo, hi], true), vec![low]);
    assert_eq!(erase_by(&mut s, "w", &[hi, lo], true), vec![low]);
    assert_eq!(erase_by(&mut s, "c", &[hi, mid([5.0, 0.0, 100.0])], true), vec![low]);
    // BOX: to the right a window (the line is only partly in it), to the left a crossing.
    let half = (Vec2::new(lo.x, lo.y), mid([5.0, 0.0, 100.0]) + Vec2::new(0.0, 1.0));
    assert_eq!(erase_by(&mut s, "box", &[half.0, half.1], true), vec![low, high]);
    assert_eq!(erase_by(&mut s, "box", &[half.1, half.0], true), vec![low]);
    // A fence across it and polygons round or across it.
    let across = [mid([5.0, 0.0, 100.0]) + Vec2::new(0.0, 3.0), mid([5.0, 0.0, 100.0]) - Vec2::new(0.0, 3.0)];
    assert_eq!(erase_by(&mut s, "f", &across, true), vec![low]);
    let round = [lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)];
    assert_eq!(erase_by(&mut s, "wp", &round, true), vec![low]);
    let part = [lo, half.1, Vec2::new(lo.x, hi.y)];
    assert_eq!(erase_by(&mut s, "cp", &part, true), vec![low]);
    assert_eq!(erase_by(&mut s, "wp", &part, true), vec![low, high]);
    // Points not clicked in the view (typed) select in the construction plane, as before: the
    // plane under that window holds neither line.
    assert_eq!(erase_by(&mut s, "w", &[lo, hi], false), vec![low, high]);
}

#[test]
fn typed_modes_in_plan_with_a_tilted_plane_select_what_plan_shows() {
    let mut s = Session::new();
    let north = line(&mut s, [0.0, 50.0, 0.0], [10.0, 50.0, 0.0]);
    let south = line(&mut s, [0.0, -50.0, 0.0], [10.0, -50.0, 0.0]);
    s.execute("cplane", &json!({ "preset": "front" })).unwrap();
    let (a, b) = (Vec2::new(-1.0, 40.0), Vec2::new(11.0, 60.0));
    assert_eq!(erase_by(&mut s, "w", &[a, b], true), vec![south]);
    assert_eq!(erase_by(&mut s, "c", &[Vec2::new(5.0, -40.0), Vec2::new(20.0, -60.0)], true), vec![north]);
}

/// What FILLET makes of raised objects stays at their height: two lines with a radius (the new
/// arc too), and a line with an arc.
#[test]
fn fillet_keeps_raised_objects_at_their_height() {
    let mut s = Session::new();
    let a = line(&mut s, [0.0, 0.0, 30.0], [100.0, 0.0, 30.0]);
    let b = line(&mut s, [110.0, 10.0, 30.0], [110.0, 100.0, 30.0]);
    let r = s.execute("fillet", &json!({ "h1": a.hex(), "p1": [50, 0], "h2": b.hex(), "p2": [110, 50], "radius": 5 })).unwrap();
    let made = Handle::parse_hex(r["arc"].as_str().unwrap()).unwrap();
    let z = |s: &Session, h: Handle| s.doc().unwrap().entity(h).unwrap().kind.elevation();
    assert_eq!((z(&s, a), z(&s, b), z(&s, made)), (30.0, 30.0, 30.0));
    let l = line(&mut s, [0.0, 50.0, 30.0], [5.0, 50.0, 30.0]);
    let arc = cadcraft_engine::doc::Arc {
        center: Vec3::new(20.0, 50.0, 30.0),
        radius: 10.0,
        start: std::f64::consts::FRAC_PI_2,
        end: 1.5 * std::f64::consts::PI,
    };
    let c = s.add_entity_world(EntityKind::Arc(arc)).unwrap();
    s.execute("fillet", &json!({ "h1": l.hex(), "p1": [2, 50], "h2": c.hex(), "p2": [20, 60], "radius": 0 })).unwrap();
    assert_eq!((z(&s, l), z(&s, c)), (30.0, 30.0));
    assert!(near(ends(&s, l).1, [10.0, 50.0, 30.0]) || near(ends(&s, l).0, [10.0, 50.0, 30.0]), "{:?}", ends(&s, l));
}
