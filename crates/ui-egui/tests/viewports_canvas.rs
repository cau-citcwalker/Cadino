//! Rhino-style model viewports on the canvas: every tile draws along its own direction (its own
//! display list, no plan grid in 3D), the right button navigates 3D views as in Rhino while a
//! plain right click stays AutoCAD's Enter, and the viewport title's menu acts on its tile.

use std::time::Duration;

use cadcraft_engine::Session;
use cadcraft_engine::geom::{Vec3, ViewDir};
use cadcraft_ui_egui::{CadApp, Services};
use egui::epaint::ClippedShape;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, Shape, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

fn dir(x: f64, y: f64, z: f64) -> ViewDir {
    ViewDir::new(Vec3::new(x, y, z))
}

/// A 20 × 10 rectangle of lines at the origin, in a 1600 × 1000 window (50 ms frames, so two
/// clicks in a row can be a double-click).
fn harness() -> Harness<'static, CadApp> {
    let mut session = Session::new();
    session.execute("line", &json!({ "points": [[0, 0], [20, 0], [20, 10], [0, 10], [0, 0]] })).unwrap();
    let mut app = CadApp::new(session, Services::default());
    app.canvas.zoom_pending = false;
    let mut h = Harness::builder().with_size(vec2(1600.0, 1000.0)).with_step_dt(0.05).build_ui_state(
        |ui, app: &mut CadApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(3);
    h
}

/// Run `id` and frames until no display list is building in the background.
fn run(h: &mut Harness<'_, CadApp>, id: &str) {
    h.state_mut().run(id, json!({})).unwrap();
    settle(h);
}

fn settle(h: &mut Harness<'_, CadApp>) {
    for _ in 0..1000 {
        h.run_steps(1);
        let c = &h.state().canvas;
        if !c.building() && !c.tiles_building() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(2);
}

fn view_dir(h: &Harness<'_, CadApp>) -> ViewDir {
    h.state().session.state().unwrap().view_dir
}

/// The tiles' screen rectangles (`VIEWPORTS.RHINO`'s quadrants), worked out from the current
/// tile's: the canvas draws tile `t` in its fractions of the drawing area, less a point all round.
fn tile_rects(h: &Harness<'_, CadApp>) -> Vec<(Rect, cadcraft_engine::ModelTile)> {
    let tiles = h.state().session.model_tiles();
    let cur = h.state().canvas.rect.unwrap();
    let a = tiles.iter().find(|t| t.active).unwrap();
    let (fw, fh) = ((a.max.x - a.min.x) as f32, (a.max.y - a.min.y) as f32);
    let (w, hh) = ((cur.width() + 2.0) / fw, (cur.height() + 2.0) / fh);
    let (left, bottom) = (cur.left() - 1.0 - w * a.min.x as f32, cur.bottom() + 1.0 + hh * a.min.y as f32);
    tiles
        .into_iter()
        .map(|t| {
            let r = Rect::from_min_max(
                pos2(left + w * t.min.x as f32, bottom - hh * t.max.y as f32),
                pos2(left + w * t.max.x as f32, bottom - hh * t.min.y as f32),
            );
            (r.shrink(1.0), t)
        })
        .collect()
}

fn rect_looking(h: &Harness<'_, CadApp>, d: ViewDir) -> Rect {
    tile_rects(h).into_iter().find(|(_, t)| t.look.dir == d).unwrap().0
}

/// The line work painted clipped to `r`: each line's or path's points.
fn lines_in(shapes: &[ClippedShape], r: Rect) -> Vec<Vec<Pos2>> {
    fn collect(s: &Shape, out: &mut Vec<Vec<Pos2>>) {
        match s {
            Shape::LineSegment { points, .. } => out.push(points.to_vec()),
            Shape::Path(p) => out.push(p.points.clone()),
            Shape::Vec(v) => v.iter().for_each(|s| collect(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for s in shapes.iter().filter(|s| (s.clip_rect.min - r.min).length() < 0.5 && (s.clip_rect.max - r.max).length() < 0.5) {
        collect(&s.shape, &mut out);
    }
    out
}

fn press(h: &mut Harness<'_, CadApp>, at: Pos2, button: PointerButton, pressed: bool, modifiers: Modifiers) {
    h.event(Event::PointerButton { pos: at, button, pressed, modifiers });
    h.run_steps(1);
}

fn move_to(h: &mut Harness<'_, CadApp>, at: Pos2) {
    h.event(Event::PointerMoved(at));
    h.run_steps(1);
}

fn right_drag(h: &mut Harness<'_, CadApp>, from: Pos2, by: egui::Vec2, modifiers: Modifiers) {
    move_to(h, from);
    h.event(Event::ModifiersChanged(modifiers));
    press(h, from, PointerButton::Secondary, true, modifiers);
    for k in 1..=4 {
        move_to(h, from + by * (k as f32 / 4.0));
    }
    press(h, from + by, PointerButton::Secondary, false, modifiers);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(1);
}

fn click(h: &mut Harness<'_, CadApp>, at: Pos2, button: PointerButton) {
    move_to(h, at);
    press(h, at, button, true, Modifiers::NONE);
    press(h, at, button, false, Modifiers::NONE);
    // Well apart from the next click (not a double-click).
    h.run_steps(8);
}

/// The title in the corner of the tile drawn in `r`.
fn title(r: Rect) -> Pos2 {
    r.left_top() + vec2(16.0, 10.0)
}

#[test]
fn four_tiles_draw_along_their_own_directions() {
    let mut h = harness();
    h.state_mut().run("grid", json!({ "on": true })).unwrap();
    run(&mut h, "viewports.rhino");
    let front = dir(0.0, -1.0, 0.0);
    let right = dir(1.0, 0.0, 0.0);
    let persp = dir(-1.0, -1.0, 1.0);
    assert!(view_dir(&h).is_plan(), "Top is current");
    // Each other tile has a list built along its own direction: the rectangle seen edge-on from
    // the front (20 wide) and from the right (10 wide), and at an angle.
    for t in h.state().session.model_tiles().iter().filter(|t| !t.active) {
        let list = h.state().canvas.tile_list(&t.look).unwrap_or_else(|| panic!("no list for {:?}", t.look));
        let b = list.bounds;
        if t.look.dir == front {
            assert!(b.height() < 1e-9 && (b.width() - 20.0).abs() < 1e-6, "{b:?}");
        } else if t.look.dir == right {
            assert!(b.height() < 1e-9 && (b.width() - 10.0).abs() < 1e-6, "{b:?}");
        } else {
            assert_eq!(t.look.dir, persp);
            assert!(b.height() > 1.0 && b.width() > 1.0, "{b:?}");
        }
    }
    // On screen: the front and right tiles draw the rectangle as one flat line and no grid; the
    // plan tile draws its grid; the perspective draws it at an angle.
    let shapes = &h.output().shapes;
    for d in [front, right] {
        let lines = lines_in(shapes, rect_looking(&h, d));
        assert!(!lines.is_empty() && lines.len() < 10, "{d:?}: {} lines", lines.len());
        let ys: Vec<f32> = lines.iter().flatten().map(|p| p.y).collect();
        assert!(ys.iter().all(|y| (y - ys[0]).abs() < 0.5), "{d:?} is edge-on: {ys:?}");
    }
    let iso = lines_in(shapes, rect_looking(&h, persp));
    assert!(iso.iter().flatten().any(|p| (p.y - iso[0][0].y).abs() > 5.0), "the perspective sees the rectangle at an angle");
    let plan = lines_in(shapes, h.state().canvas.rect.unwrap());
    assert!(plan.len() > 20, "the plan tile has its grid: {} lines", plan.len());
}

#[test]
fn right_drag_orbits_a_3d_view_and_a_right_click_stays_enter() {
    let mut h = harness();
    run(&mut h, "view.swiso");
    let c = h.state().canvas.rect.unwrap().center();
    let iso = view_dir(&h);
    // Drawing a line: the drag turns the view and leaves the command running.
    h.state_mut().start("line");
    click(&mut h, c, PointerButton::Primary);
    assert!(h.state().session.prompt_text().contains("next point"), "{}", h.state().session.prompt_text());
    right_drag(&mut h, c, vec2(80.0, 0.0), Modifiers::NONE);
    assert_ne!(view_dir(&h), iso, "right-drag orbits");
    assert!(h.state().session.running.is_some(), "a drag is not Enter");
    click(&mut h, c, PointerButton::Secondary);
    assert!(h.state().session.running.is_none(), "a right click is Enter: {}", h.state().session.prompt_text());
    // Shift pans, Ctrl zooms (up zooms in); neither turns the view.
    let (d0, v0) = (view_dir(&h), h.state().session.state().unwrap().view());
    right_drag(&mut h, c, vec2(60.0, 0.0), Modifiers::SHIFT);
    let v1 = h.state().session.state().unwrap().view();
    assert_eq!(view_dir(&h), d0);
    assert!(v1.center.x < v0.center.x && (v1.height - v0.height).abs() < 1e-9, "{v0:?} → {v1:?}");
    right_drag(&mut h, c, vec2(0.0, -60.0), Modifiers::CTRL);
    let v2 = h.state().session.state().unwrap().view();
    assert_eq!(view_dir(&h), d0);
    assert!(v2.height < v1.height, "{v1:?} → {v2:?}");
    // Plan keeps AutoCAD's right button: a drag doesn't turn it.
    run(&mut h, "plan");
    right_drag(&mut h, c, vec2(80.0, 40.0), Modifiers::NONE);
    assert!(view_dir(&h).is_plan());
}

#[test]
fn right_drag_in_another_3d_tile_makes_it_current_and_turns_it() {
    let mut h = harness();
    run(&mut h, "viewports.rhino");
    let persp = dir(-1.0, -1.0, 1.0);
    let r = rect_looking(&h, persp);
    right_drag(&mut h, r.center(), vec2(80.0, 0.0), Modifiers::NONE);
    settle(&mut h);
    let tiles = h.state().session.model_tiles();
    let cur = tiles.iter().find(|t| t.active).unwrap();
    assert!((cur.min.x - 0.5).abs() < 1e-9 && (cur.min.y - 0.5).abs() < 1e-9, "the perspective tile is current: {cur:?}");
    assert!(!view_dir(&h).is_plan() && view_dir(&h) != persp, "and turned");
    // The plan tile kept its look, and a drag in it is not Rhino's.
    let top = tiles.iter().find(|t| t.look.dir.is_plan()).unwrap();
    assert!(!top.active);
    let r = tile_rects(&h).into_iter().find(|(_, t)| t.look.dir.is_plan()).unwrap().0;
    right_drag(&mut h, r.center(), vec2(80.0, 0.0), Modifiers::NONE);
    assert!(!h.state().session.model_tiles().iter().find(|t| t.look.dir.is_plan()).unwrap().active);
}

#[test]
fn the_title_menu_acts_on_its_tile() {
    let mut h = harness();
    run(&mut h, "viewports.rhino");
    let front = dir(0.0, -1.0, 0.0);
    // A click on the Front title makes it current and opens the menu.
    let at = title(rect_looking(&h, front));
    click(&mut h, at, PointerButton::Primary);
    assert_eq!(view_dir(&h), front);
    h.get_by_label("Shaded").click();
    settle(&mut h);
    let st = h.state().session.state().unwrap();
    assert!(st.shaded && !st.shaded_edges && st.view_dir == front);
    assert!(h.state().session.model_tiles().iter().filter(|t| !t.active).all(|t| !t.look.shaded), "the others stay wireframe");
    // A preset from the menu.
    let at = title(h.state().canvas.rect.unwrap());
    click(&mut h, at, PointerButton::Primary);
    h.get_by_label("Right").click();
    settle(&mut h);
    assert_eq!(view_dir(&h), dir(1.0, 0.0, 0.0));
    // Maximize, then a double-click on the title brings the four views back.
    let at = title(h.state().canvas.rect.unwrap());
    click(&mut h, at, PointerButton::Primary);
    h.get_by_label("Maximize").click();
    settle(&mut h);
    assert!(h.state().session.model_tiles().is_empty());
    assert_eq!(view_dir(&h), dir(1.0, 0.0, 0.0), "the maximized tile keeps its view");
    let at = title(h.state().canvas.rect.unwrap());
    // Well after the menu click, which would otherwise pair with the first click.
    h.run_steps(8);
    move_to(&mut h, at);
    for _ in 0..2 {
        press(&mut h, at, PointerButton::Primary, true, Modifiers::NONE);
        press(&mut h, at, PointerButton::Primary, false, Modifiers::NONE);
    }
    settle(&mut h);
    assert_eq!(h.state().session.model_tiles().len(), 4, "double-click restores");
    assert_eq!(view_dir(&h), dir(1.0, 0.0, 0.0));
}

/// A click in another tile of a 3D layout makes it current and picks there at once, with that
/// tile's own list and view: the lists trade places, so neither tile waits for a build. Tiles all
/// in plan keep AutoCAD's way: the click only makes the tile current.
#[test]
fn a_click_in_another_3d_tile_makes_it_current_and_picks_there() {
    let mut h = harness();
    run(&mut h, "viewports.rhino");
    let front = dir(0.0, -1.0, 0.0);
    let top = h.state().session.state().unwrap().look();
    // The rectangle seen edge-on runs through the middle of the Front tile.
    let at = rect_looking(&h, front).center();
    move_to(&mut h, at);
    press(&mut h, at, PointerButton::Primary, true, Modifiers::NONE);
    press(&mut h, at, PointerButton::Primary, false, Modifiers::NONE);
    assert_eq!(view_dir(&h), front);
    assert_eq!(h.state().session.selection().len(), 1, "the click picked the line under it");
    // On that very frame the canvas has Front's list (flat, edge-on) and Top the canvas's old one.
    let b = h.state().canvas.list.as_ref().unwrap().bounds;
    assert!(b.height() < 1e-9 && (b.width() - 20.0).abs() < 1e-6, "{b:?}");
    assert!(h.state().canvas.tile_list(&top).is_some(), "the tile that was current isn't blank");
    settle(&mut h);
    // Plan tiles: the click only makes the other tile current.
    h.state_mut().session.set_selection(Vec::new());
    run(&mut h, "plan");
    h.state_mut().run("-vports", json!({ "option": "single" })).unwrap();
    h.state_mut().run("-vports", json!({ "option": "2" })).unwrap();
    settle(&mut h);
    let other = tile_rects(&h).into_iter().find(|(_, t)| !t.active).unwrap();
    click(&mut h, other.0.center(), PointerButton::Primary);
    let now = h.state().session.model_tiles().into_iter().find(|t| t.active).unwrap();
    assert_eq!(now.min, other.1.min, "the other tile is current");
    assert!(h.state().session.selection().is_empty() && h.state().session.pending_window.is_none(), "and nothing else happened");
}

/// While -VPORTS Join asks for viewports, clicks on the tiles' titles answer it (no menu).
#[test]
fn titles_answer_the_join_prompts() {
    let mut h = harness();
    run(&mut h, "viewports.rhino");
    let (front, right) = (dir(0.0, -1.0, 0.0), dir(1.0, 0.0, 0.0));
    h.state_mut().session.cmdline("-vports").unwrap();
    h.state_mut().session.cmdline("j").unwrap();
    h.run_steps(2);
    let at = title(rect_looking(&h, front));
    click(&mut h, at, PointerButton::Primary);
    assert!(h.state().session.prompt_text().contains("join"), "{}", h.state().session.prompt_text());
    assert!(h.query_by_label("Four Views").is_none(), "no viewport menu");
    let at = title(rect_looking(&h, right));
    click(&mut h, at, PointerButton::Primary);
    assert!(h.state().session.running.is_none(), "{}", h.state().session.prompt_text());
    let tiles = h.state().session.model_tiles();
    assert_eq!(tiles.len(), 3);
    let joined = tiles.iter().find(|t| t.max.x == 1.0 && t.max.y == 0.5).unwrap();
    assert!(joined.min.x == 0.0 && joined.active && joined.look.dir == front, "Front (dominant) and Right joined: {joined:?}");
}

/// Over a viewport title the pointer is a hand, not the canvas's crosshair.
#[test]
fn the_pointer_is_a_hand_over_a_title() {
    let mut h = harness();
    run(&mut h, "viewports.rhino");
    let r = h.state().canvas.rect.unwrap();
    move_to(&mut h, title(r));
    h.run_steps(1);
    assert_eq!(h.output().platform_output.cursor_icon, egui::CursorIcon::PointingHand);
    move_to(&mut h, r.center());
    h.run_steps(1);
    assert_ne!(h.output().platform_output.cursor_icon, egui::CursorIcon::PointingHand);
}

/// A right-drag during a command that comes back near where it started is navigation: neither
/// Enter nor the command menu (held long enough, a still press would open it).
#[test]
fn a_right_drag_that_comes_back_is_neither_enter_nor_the_command_menu() {
    let mut h = harness();
    run(&mut h, "view.swiso");
    let c = h.state().canvas.rect.unwrap().center();
    h.state_mut().start("line");
    click(&mut h, c, PointerButton::Primary);
    move_to(&mut h, c);
    press(&mut h, c, PointerButton::Secondary, true, Modifiers::NONE);
    for dx in [40.0, 80.0, 40.0, 2.0] {
        move_to(&mut h, c + vec2(dx, 0.0));
    }
    press(&mut h, c + vec2(2.0, 0.0), PointerButton::Secondary, false, Modifiers::NONE);
    h.run_steps(2);
    assert!(h.state().session.running.is_some(), "not Enter");
    assert!(!egui::Popup::is_id_open(&h.ctx, egui::Id::new("cc_command_menu")), "no command menu");
}

/// The title menu opens on a right click too; a shaded tile keeps a list of its own once another
/// is current; Maximize, Restore Viewports and Four Views lay the tiles out.
#[test]
fn the_title_menu_restores_and_lays_out_four_views() {
    let mut h = harness();
    run(&mut h, "viewports.rhino");
    let front = dir(0.0, -1.0, 0.0);
    let at = title(rect_looking(&h, front));
    click(&mut h, at, PointerButton::Secondary);
    assert_eq!(view_dir(&h), front);
    h.get_by_label("Shaded with Edges").click();
    settle(&mut h);
    let shaded = h.state().session.state().unwrap().look();
    assert!(shaded.shaded && shaded.edges && shaded.dir == front);
    // Top current again: Front draws its own shaded list.
    let top = tile_rects(&h).into_iter().find(|(_, t)| t.look.dir.is_plan()).unwrap().0;
    click(&mut h, title(top), PointerButton::Primary);
    settle(&mut h);
    assert!(view_dir(&h).is_plan());
    assert!(h.state().canvas.tile_list(&shaded).is_some(), "the shaded inactive tile has its own list");
    h.get_by_label("Maximize").click();
    settle(&mut h);
    assert!(h.state().session.model_tiles().is_empty());
    let at = title(h.state().canvas.rect.unwrap());
    click(&mut h, at, PointerButton::Primary);
    h.get_by_label("Restore Viewports").click();
    settle(&mut h);
    let tiles = h.state().session.model_tiles();
    assert_eq!(tiles.len(), 4);
    assert!(tiles.iter().any(|t| t.look == shaded), "Front came back shaded");
    let at = title(h.state().canvas.rect.unwrap());
    click(&mut h, at, PointerButton::Primary);
    h.get_by_label("Four Views").click();
    settle(&mut h);
    let tiles = h.state().session.model_tiles();
    assert_eq!(tiles.len(), 4);
    assert!(tiles.iter().all(|t| !t.look.shaded), "Four Views starts over in the current tile's style (Top's wireframe)");
}
