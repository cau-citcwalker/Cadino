//! The canvas in a perspective view: screen ↔ CPlane through the eye, picking what is drawn,
//! grips dragged in their object's plane with their height, and typed selection modes' rubber
//! bands.

use cadcraft_doc::{Common, EntityKind, Line, Space};
use cadcraft_geom::{Camera, Mat4, Vec2, Vec3, ViewDir};
use egui::{Pos2, Rect, pos2, vec2};
use serde_json::json;

use super::{Xf, view_pick_at};
use crate::CadApp;

/// Frames a tenth of a second apart in a 1600 × 1000 window.
#[derive(Default)]
struct Frames {
    ctx: egui::Context,
    time: f64,
}

impl Frames {
    fn run(&mut self, app: &mut CadApp, events: Vec<egui::Event>) {
        self.time += 0.1;
        app.logic(&self.ctx);
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1000.0))),
            events,
            time: Some(self.time),
            ..Default::default()
        };
        let mut out = self.ctx.run_ui(raw, |ui| app.ui(ui));
        out.textures_delta.clear();
    }
    fn click(&mut self, app: &mut CadApp, at: Pos2) {
        self.time += 1.0;
        self.run(app, vec![egui::Event::PointerMoved(at)]);
        let b =
            |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::default() };
        self.run(app, vec![b(true)]);
        self.run(app, vec![b(false)]);
        self.run(app, Vec::new());
    }
}

fn persp_dir(center: Vec2, distance: f64) -> ViewDir {
    let cam = Camera::new(50.0, 0.0);
    ViewDir::named("iso").unwrap().with_camera(Some(cam), center, cam.height_at(distance))
}

#[test]
fn points_on_the_cplane_round_trip_through_the_eye() {
    let rect = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
    let dir = persp_dir(Vec2::new(10.0, 5.0), 400.0);
    let front = Mat4::from_cols(Vec3::X, Vec3::Z, -Vec3::Y, Vec3::new(5.0, 0.0, 0.0));
    for cplane in [Mat4::IDENTITY, front] {
        let xf = Xf::new(rect, Vec2::new(10.0, 5.0), 600.0 / Camera::new(50.0, 0.0).height_at(400.0)).looking(dir).on(cplane);
        assert!(!xf.is_plan() && !xf.flat());
        for p in [Vec2::new(0.0, 0.0), Vec2::new(30.0, -20.0), Vec2::new(-12.5, 7.0)] {
            let back = xf.to_world(xf.to_screen(p));
            assert!(back.dist(p) < 1e-3, "{p:?} came back at {back:?}");
        }
        // A segment reaching behind the eye shows only its part in front.
        let eye = cplane.inverse().unwrap().apply(dir.eye().unwrap());
        let s = xf.segment_to_screen(Vec2::ZERO, eye.xy() + (eye.xy() - Vec2::ZERO) * 3.0);
        assert!(s.is_none_or(|[a, b]| a.x.is_finite() && b.y.is_finite()));
    }
}

#[test]
fn a_perspective_view_picks_what_it_draws() {
    let mut f = Frames::default();
    let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
    let add = |app: &mut CadApp, a: Vec3, b: Vec3| {
        app.session.doc_mut().unwrap().add(&Space::Model, Common::default(), EntityKind::Line(Line { a, b })).unwrap()
    };
    let ground = add(&mut app, Vec3::new(0.0, 0.0, 0.0), Vec3::new(100.0, 0.0, 0.0));
    let raised = add(&mut app, Vec3::new(0.0, 60.0, 80.0), Vec3::new(100.0, 60.0, 80.0));
    f.run(&mut app, Vec::new());
    app.session.execute("view.swiso", &json!({})).unwrap();
    app.session.execute("setvar", &json!({ "name": "PERSPECTIVE", "value": 1 })).unwrap();
    app.canvas.list = None;
    f.run(&mut app, Vec::new());
    let xf = app.canvas.xf.unwrap();
    assert!(xf.dir().persp.is_some(), "the canvas shows the perspective");
    for (h, mid) in [(raised, Vec3::new(50.0, 60.0, 80.0)), (ground, Vec3::new(50.0, 0.0, 0.0))] {
        let at = xf.world_to_screen(mid);
        assert_eq!(view_pick_at(&app, &xf, Some(at)), Some(h));
    }
    // A click there selects it.
    let at = xf.world_to_screen(Vec3::new(30.0, 60.0, 80.0));
    f.click(&mut app, at);
    assert_eq!(app.session.selection(), vec![raised]);
    // The point under the cursor at a prompt is on the CPlane where the ray from the eye meets it.
    let w = xf.to_world(xf.world_to_screen(Vec3::new(70.0, 40.0, 0.0)));
    assert!(w.dist(Vec2::new(70.0, 40.0)) < 1e-3, "{w:?}");
}

/// A circle standing square to the CPlane: its top quadrant grip dragged up stretches it in its
/// own plane, the drag's height going with the point.
#[test]
fn a_grip_drag_sends_its_height_in_the_objects_plane() {
    let mut f = Frames::default();
    let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
    // Grips in a 3D view: the gumball off (see `gumball::replaces_grips`).
    app.ui.gumball = false;
    let common = Common { extrusion: Vec3::new(1.0, 0.0, 0.0), ..Default::default() };
    let c = cadcraft_doc::Circle { center: Vec3::ZERO, radius: 20.0 };
    let h = app.session.doc_mut().unwrap().add(&Space::Model, common, EntityKind::Circle(c)).unwrap();
    // No object snaps: the circle's centre is under the cursor in plan.
    app.session.settings.osmode = 0;
    f.run(&mut app, Vec::new());
    app.session.execute("view.swiso", &json!({})).unwrap();
    // Room above the circle to drag its top to.
    let st = app.session.state_mut().unwrap();
    let v = st.view();
    st.set_view_quiet(cadcraft_engine::View { center: v.center, height: 200.0 });
    app.session.set_selection(vec![h]);
    // The list for the new direction at once (not in the background).
    app.canvas.list = None;
    f.run(&mut app, Vec::new());
    let xf = app.canvas.xf.unwrap();
    assert!(!xf.is_plan());
    let e = app.session.pick_doc().unwrap().entity(h).unwrap().clone();
    let top = super::snap3d::grips3(&e).iter().position(|p| (*p - Vec3::new(0.0, 0.0, 20.0)).len() < 1e-6).expect("a grip at the top");
    let spots = super::grip_spots(&e, &xf);
    f.click(&mut app, spots[top]);
    assert!(app.session.running.as_ref().is_some_and(|r| r.id == "grip"), "the grip edit started");
    let to = xf.world_to_screen(Vec3::new(0.0, 0.0, 40.0));
    f.run(&mut app, vec![egui::Event::PointerMoved(to)]);
    assert!(app.canvas.grip_z.is_some_and(|z| (z - 40.0).abs() < 1e-3), "{:?}", app.canvas.grip_z);
    f.click(&mut app, to);
    let EntityKind::Circle(c) = &app.session.doc().unwrap().entity(h).unwrap().kind else { panic!("a circle") };
    assert!((c.radius - 40.0).abs() < 1e-3, "stretched up in its plane: {}", c.radius);
}

#[test]
fn typed_selection_modes_draw_their_rubber_bands() {
    use cadcraft_engine::select::{SelectKind, SelectMode};
    let ctx = egui::Context::default();
    let s = cadcraft_engine::Settings::default();
    let xf = Xf::new(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0)), Vec2::ZERO, 1.0);
    for (kind, points) in
        [(SelectKind::Window, 1), (SelectKind::Crossing, 1), (SelectKind::WPolygon, 2), (SelectKind::CPolygon, 3), (SelectKind::Fence, 2)]
    {
        let m = SelectMode { kind, pts: (0..points).map(|i| Vec2::new(f64::from(i) * 30.0, f64::from(i * i) * 10.0)).collect(), seen: Vec::new() };
        let mut out = ctx.run_ui(Default::default(), |ui| super::draw_select_mode(ui.painter(), &xf, &m, pos2(300.0, 250.0), &s));
        out.textures_delta.clear();
        let shapes = out.shapes.iter().filter(|s| !matches!(s.shape, egui::Shape::Noop)).count();
        assert!(shapes > 0, "{kind:?} drew nothing");
    }
    // No points yet: nothing.
    let m = SelectMode { kind: SelectKind::Window, pts: Vec::new(), seen: Vec::new() };
    let mut out = ctx.run_ui(Default::default(), |ui| super::draw_select_mode(ui.painter(), &xf, &m, pos2(300.0, 250.0), &s));
    out.textures_delta.clear();
    assert!(out.shapes.iter().all(|s| matches!(s.shape, egui::Shape::Noop)) || out.shapes.is_empty());
}

#[test]
fn what_is_behind_the_eye_neither_snaps_nor_shows_grips() {
    use cadcraft_doc::{Drawing, Point};
    use cadcraft_engine::snap::mode;
    let rect = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
    let dir = persp_dir(Vec2::ZERO, 400.0);
    let xf = Xf::new(rect, Vec2::ZERO, 600.0 / Camera::new(50.0, 0.0).height_at(400.0)).looking(dir);
    // Just behind the eye on its axis: `project` puts it at the centre of the screen.
    let behind = dir.eye().unwrap() + dir.dir;
    assert!(xf.world_to_screen(behind).distance(rect.center()) < 1.0);
    let mut d = Drawing::new_metric();
    d.add(&Space::Model, Common::default(), EntityKind::Point(Point { p: behind, angle: 0.0 })).unwrap();
    let l = d.add(&Space::Model, Common::default(), EntityKind::Line(Line { a: behind, b: behind + dir.dir * 50.0 })).unwrap();
    let all = mode::END | mode::MID | mode::NOD | mode::NEA | mode::INT;
    assert!(super::snap3d::candidates_at(&d, &Space::Model, &xf, rect.center(), 10.0, all, None).is_empty());
    // A line running from in front of the eye to behind it is hit where it is drawn.
    let far = dir.world_of(Vec3::new(0.0, -50.0, -200.0));
    let mut d2 = Drawing::new_metric();
    d2.add(&Space::Model, Common::default(), EntityKind::Line(Line { a: far, b: behind })).unwrap();
    assert!(super::snap3d::candidates_at(&d2, &Space::Model, &xf, xf.world_to_screen(far), 10.0, mode::END, None).len() == 1);
    // Its grips are nowhere.
    let e = d.entity(l).unwrap().clone();
    assert!(super::grip_spots(&e, &xf).iter().all(|p| p.x.is_nan()));
}
