//! The navigation overlays on the drawing area: the ViewCube (Top face, Home, compass ring and
//! the WCS menu) and the UCS icon.
//!
//! Every overlay registers a widget that senses clicks *and* drags, so egui hands the pointer to
//! the overlay instead of the canvas underneath (a click on the ViewCube must never also pick or
//! start a selection window in the drawing). What the overlays do goes through commands:
//! `plan` (Top face), `view.swiso` (Home), `ui.toggle.viewcube`, `ui.toggle.ucsicon`; dragging
//! the compass ring orbits the model view.
//!
//! There is no user coordinate system yet, so the WCS menu lists the only one there is.

use cadcraft_geom::{Vec3, ViewDir};
use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, pos2, vec2};
use serde_json::json;

use crate::CadApp;
use crate::canvas::Xf;
use crate::theme::Tokens;

/// Where the ViewCube's parts are on the canvas.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub center: Pos2,
    /// Radius of the compass ring's centre line.
    pub ring: f32,
    pub face: Rect,
    pub home: Rect,
    /// The "WCS" coordinate-system menu under the cube.
    pub pill: Rect,
    /// Everything that belongs to the cube (the area kept away from the canvas).
    pub bounds: Rect,
}

impl Layout {
    pub fn new(canvas: Rect) -> Self {
        let center = pos2(canvas.right() - 92.0, canvas.top() + 82.0);
        let ring = 58.0;
        Layout {
            center,
            ring,
            face: Rect::from_center_size(center, vec2(44.0, 44.0)),
            home: Rect::from_center_size(center + vec2(-ring + 2.0, -ring + 2.0), vec2(20.0, 20.0)),
            pill: Rect::from_center_size(center + vec2(0.0, ring + 26.0), vec2(56.0, 16.0)),
            bounds: Rect::from_center_size(center, vec2(2.0 * (ring + 10.0), 2.0 * (ring + 10.0))),
        }
    }

    /// The part of the cube at `p` (not the pill, which is its own widget).
    pub fn part_at(&self, p: Pos2) -> Option<Part> {
        if self.home.contains(p) {
            return Some(Part::Home);
        }
        if self.face.contains(p) {
            return Some(Part::Face);
        }
        let d = p.distance(self.center);
        ((self.ring - 10.0..=self.ring + 10.0).contains(&d)).then_some(Part::Ring)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Face,
    Home,
    Ring,
}

impl Part {
    fn tip(self) -> &'static str {
        match self {
            Part::Face => "Face: look straight at the face shown (zoom extents)",
            Part::Home => "Home: the south-west isometric view",
            Part::Ring => "Compass: drag to orbit the view",
        }
    }
}

/// The preset views as the viewport label names them, with their directions toward the viewer
/// (VPOINT's presets).
const VIEWS: [(&str, [f64; 3]); 10] = [
    ("Top", [0.0, 0.0, 1.0]),
    ("Bottom", [0.0, 0.0, -1.0]),
    ("Front", [0.0, -1.0, 0.0]),
    ("Back", [0.0, 1.0, 0.0]),
    ("Left", [-1.0, 0.0, 0.0]),
    ("Right", [1.0, 0.0, 0.0]),
    ("SW Isometric", [-1.0, -1.0, 1.0]),
    ("SE Isometric", [1.0, -1.0, 1.0]),
    ("NE Isometric", [1.0, 1.0, 1.0]),
    ("NW Isometric", [-1.0, 1.0, 1.0]),
];

/// The name of view `v` (translated): a preset view's, else "Custom View".
pub(crate) fn view_name(v: ViewDir) -> &'static str {
    let name = VIEWS.iter().find(|(_, [x, y, z])| (ViewDir::new(Vec3::new(*x, *y, *z)).dir - v.dir).len() < 1e-6).map_or("Custom View", |(n, _)| n);
    crate::i18n::t(name)
}

/// The label of the cube face view `v` looks at most directly (the top or bottom first).
fn face_name(v: ViewDir) -> &'static str {
    face(v).0
}

/// The face view `v` looks at most directly: its label and the command that looks straight at
/// it (the top's is PLAN).
fn face(v: ViewDir) -> (&'static str, &'static str) {
    let d = v.dir;
    if d.z.abs() >= d.x.abs().max(d.y.abs()) {
        if d.z >= 0.0 { (crate::tl!("TOP"), "plan") } else { (crate::tl!("BOTTOM"), "view.bottom") }
    } else if d.y.abs() >= d.x.abs() {
        if d.y < 0.0 { (crate::tl!("FRONT"), "view.front") } else { (crate::tl!("BACK"), "view.back") }
    } else if d.x < 0.0 {
        (crate::tl!("LEFT"), "view.left")
    } else {
        (crate::tl!("RIGHT"), "view.right")
    }
}

/// Paint the ViewCube's ring, compass letters, Top face and Home button, and handle the pointer.
/// The canvas paints the WCS pill itself; this returns the pointer's part for its highlight.
pub fn show(app: &mut CadApp, ui: &mut egui::Ui, l: &Layout) {
    let resp = ui.interact(l.bounds, ui.id().with("viewcube"), Sense::click_and_drag());
    let part = resp.hover_pos().and_then(|p| l.part_at(p));
    let p = ui.painter_at(l.bounds.union(l.pill));
    let ring_col = if part == Some(Part::Ring) { Color32::from_rgb(0x5c, 0x67, 0x7c) } else { Color32::from_rgb(0x48, 0x50, 0x5c) };
    p.circle_stroke(l.center, l.ring, Stroke::new(9.0, ring_col));
    p.circle_stroke(l.center, l.ring + 4.5, Stroke::new(1.0, Color32::from_rgb(0x5c, 0x65, 0x72)));
    let f = egui::FontId::proportional(17.0);
    let lc = Color32::from_rgb(0xc8, 0xcc, 0xd2);
    let c = l.center;
    let ring = l.ring;
    p.text(c + vec2(0.0, -ring - 1.0), egui::Align2::CENTER_CENTER, crate::tl!("N"), f.clone(), lc);
    p.text(c + vec2(0.0, ring + 1.0), egui::Align2::CENTER_CENTER, crate::tl!("S"), f.clone(), lc);
    p.text(c + vec2(ring + 1.0, 0.0), egui::Align2::CENTER_CENTER, crate::tl!("E"), f.clone(), lc);
    p.text(c + vec2(-ring - 1.0, 0.0), egui::Align2::CENTER_CENTER, crate::tl!("W"), f, lc);
    let face_col = if part == Some(Part::Face) { Color32::from_rgb(0xb8, 0xbc, 0xc2) } else { Color32::from_rgb(0x9a, 0x9e, 0xa4) };
    p.rect_filled(l.face, 2.0, face_col);
    p.rect_stroke(l.face, 2.0, Stroke::new(1.0, Color32::from_rgb(0x6c, 0x70, 0x76)), egui::StrokeKind::Inside);
    // The face the view looks at, its name shrunk to fit.
    let dir = match app.session.state() {
        Ok(st) if st.space == cadcraft_doc::Space::Model => st.view_dir,
        _ => ViewDir::PLAN,
    };
    let ink = Color32::from_rgb(0x50, 0x54, 0x5a);
    let mut g = p.layout_no_wrap(face_name(dir).to_owned(), egui::FontId::proportional(13.0), ink);
    if g.size().x > l.face.width() - 4.0 {
        g = p.layout_no_wrap(face_name(dir).to_owned(), egui::FontId::proportional(13.0 * (l.face.width() - 4.0) / g.size().x), ink);
    }
    p.galley(c - g.size() / 2.0, g, ink);
    paint_home(&p, l.home, part == Some(Part::Home));

    let resp = match part {
        Some(part) => resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(part.tip()),
        None => resp,
    };
    let pressed_on = resp.interact_pointer_pos().and_then(|o| l.part_at(o));
    // A drag belongs to the part it started on, wherever the pointer goes.
    let dragged_from = ui.input(|i| i.pointer.press_origin()).and_then(|o| l.part_at(o));
    if resp.clicked() {
        match pressed_on {
            // The face shown: look straight at it (the top: plan view of the WCS); Home: the
            // south-west isometric.
            Some(Part::Face) => {
                let _ = app.run(face(dir).1, json!({}));
            }
            Some(Part::Home) => {
                let _ = app.run("view.swiso", json!({}));
            }
            Some(Part::Ring) | None => {}
        }
    }
    // Dragging the ring orbits the view (the Model tab's: MSPACE viewports show plan).
    if resp.dragged() && dragged_from == Some(Part::Ring) && app.session.layout_space() == cadcraft_doc::Space::Model {
        let d = resp.drag_delta();
        let k = crate::canvas::ORBIT_DEG_PER_PX;
        let _ = cadcraft_engine::orbit(&mut app.session, -f64::from(d.x) * k, f64::from(d.y) * k);
    }
    let mut action = None;
    resp.context_menu(|ui| {
        if ui.button("Home").clicked() {
            action = Some(("view.swiso", json!({})));
            ui.close();
        }
        if ui.button("Hide ViewCube").clicked() {
            action = Some(("ui.toggle.viewcube", json!({"on": false})));
            ui.close();
        }
    });
    if let Some((id, params)) = action {
        let _ = app.run(id, params);
    }
    // The WCS pill (painted by the canvas) opens the coordinate-system menu.
    let pill = ui.interact(l.pill, ui.id().with("viewcube_wcs"), Sense::click_and_drag()).on_hover_cursor(CursorIcon::PointingHand);
    let pill = pill.on_hover_text("Coordinate system");
    ucs_menu(app, egui::Popup::menu(&pill));
}

/// A small house, drawn in code.
fn paint_home(p: &egui::Painter, r: Rect, hot: bool) {
    let t = Tokens::get();
    if hot {
        p.rect_filled(r, 3.0, t.control_hover);
    }
    let st = Stroke::new(1.3, if hot { t.text } else { t.text_dim });
    let c = r.center();
    let roof = [c + vec2(-6.5, 0.5), c + vec2(0.0, -6.0), c + vec2(6.5, 0.5)];
    p.line(roof.to_vec(), st);
    p.line(vec![c + vec2(-4.5, -1.0), c + vec2(-4.5, 6.0), c + vec2(4.5, 6.0), c + vec2(4.5, -1.0)], st);
    p.line_segment([c + vec2(-1.2, 6.0), c + vec2(-1.2, 2.5)], st);
    p.line_segment([c + vec2(1.2, 6.0), c + vec2(1.2, 2.5)], st);
    p.line_segment([c + vec2(-1.2, 2.5), c + vec2(1.2, 2.5)], st);
}

/// The UCS icon's arms as the canvas draws them in view `xf` ([`crate::canvas::ucs_icon_arms`]):
/// a box round each arm and its label.
pub fn ucs_arms(canvas: Rect, xf: &Xf) -> Vec<Rect> {
    let (o, arms) = crate::canvas::ucs_icon_arms(canvas, xf);
    arms.into_iter().map(|(_, tip)| Rect::from_two_pos(o, o + (tip - o) * 1.3).expand(8.0)).collect()
}

/// Make the UCS icon clickable where the canvas draws it in view `xf`: hovering highlights it, a
/// click (either button) opens the coordinate-system menu.
pub fn ucs_icon(app: &mut CadApp, ui: &mut egui::Ui, canvas: Rect, xf: &Xf) {
    let t = Tokens::get();
    let mut resp: Option<egui::Response> = None;
    for (i, r) in ucs_arms(canvas, xf).into_iter().enumerate() {
        let arm = ui.interact(r, ui.id().with(("ucs_arm", i)), Sense::click_and_drag());
        resp = Some(match resp {
            Some(r) => r | arm,
            None => arm,
        });
    }
    let Some(resp) = resp else { return };
    let text = if xf.cplane() == cadcraft_geom::Mat4::IDENTITY {
        crate::tl!("UCS icon: World Coordinate System")
    } else {
        crate::tl!("UCS icon: a user coordinate system (construction plane)")
    };
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(text);
    if resp.hovered() {
        let (o, arms) = crate::canvas::ucs_icon_arms(canvas, xf);
        let st = Stroke::new(2.0, t.hover);
        let p = ui.painter_at(canvas);
        for (_, tip) in arms {
            p.line_segment([o, tip], st);
        }
    }
    // Opened with `Bool(true)`: a pointer-anchored popup records its position only then.
    let open = (resp.clicked() || resp.secondary_clicked()).then_some(egui::SetOpenCommand::Bool(true));
    ucs_menu(app, egui::Popup::menu(&resp).open_memory(open).at_pointer_fixed());
}

/// The coordinate-system menu: the current view's construction plane (World, or one of the
/// preset planes, set with CPLANE) and the icon switch.
fn ucs_menu(app: &mut CadApp, popup: egui::Popup<'_>) {
    let mut hide = false;
    let mut preset = None;
    let world = app.session.cplane() == cadcraft_geom::Mat4::IDENTITY;
    popup.show(|ui| {
        if ui.radio(world, crate::tl!("World (WCS)")).on_hover_text(crate::tl!("Draw on the world XY plane")).clicked() {
            preset = Some("world");
        }
        let presets = [
            (crate::tl!("CPlane Front"), "front"),
            (crate::tl!("CPlane Right"), "right"),
            (crate::tl!("CPlane Back"), "back"),
            (crate::tl!("CPlane Left"), "left"),
            (crate::tl!("CPlane Bottom"), "bottom"),
        ];
        for (label, p) in presets {
            if ui.button(label).clicked() {
                preset = Some(p);
            }
        }
        ui.separator();
        if ui.button(crate::tl!("Hide UCS Icon")).clicked() {
            hide = true;
        }
        if hide || preset.is_some() {
            ui.close();
        }
    });
    if let Some(p) = preset {
        // A construction plane is an undo step: a running command ends first.
        app.run_after_cancel("cplane", json!({ "preset": p }));
    }
    if hide {
        let _ = app.run("ui.toggle.ucsicon", json!({"on": false}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;

    /// The app with the sample drawing, laid out once in a 1400×900 window.
    fn app_and_ctx() -> (CadApp, egui::Context) {
        let mut app = CadApp::new(Session::empty(), crate::Services::default());
        app.ui.in_window_menu = true;
        app.start("ui.sample");
        let ctx = egui::Context::default();
        frame(&mut app, &ctx, Vec::new());
        frame(&mut app, &ctx, Vec::new());
        (app, ctx)
    }

    fn frame(app: &mut CadApp, ctx: &egui::Context, events: Vec<egui::Event>) {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1400.0, 900.0))), events, ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            app.logic(ui.ctx());
            app.ui(ui);
        });
        out.textures_delta.clear();
    }

    /// A real pointer click (move, press, release in separate frames, like the control channel).
    pub(crate) fn click(app: &mut CadApp, ctx: &egui::Context, at: Pos2, button: egui::PointerButton) {
        let m = egui::Modifiers::default();
        frame(app, ctx, vec![egui::Event::PointerMoved(at)]);
        frame(app, ctx, vec![egui::Event::PointerButton { pos: at, button, pressed: true, modifiers: m }]);
        frame(app, ctx, vec![egui::Event::PointerButton { pos: at, button, pressed: false, modifiers: m }]);
        frame(app, ctx, Vec::new());
    }

    /// A menu popup was shown in the last frame.
    fn menu_shown(ctx: &egui::Context) -> bool {
        ctx.memory(|m| m.areas().visible_layer_ids().iter().any(|l| l.order == egui::Order::Foreground))
    }

    fn canvas(app: &CadApp) -> Rect {
        app.canvas.rect.unwrap_or(Rect::NOTHING)
    }

    fn view(app: &CadApp) -> (f64, f64, f64) {
        let v = app.session.state().map(|s| s.view()).unwrap_or_default();
        (v.center.x, v.center.y, v.height)
    }

    #[test]
    fn parts_are_found_where_they_are_drawn() {
        let l = Layout::new(Rect::from_min_size(pos2(200.0, 80.0), vec2(1000.0, 800.0)));
        assert_eq!(l.part_at(l.center), Some(Part::Face));
        assert_eq!(l.part_at(l.home.center()), Some(Part::Home));
        assert_eq!(l.part_at(l.center + vec2(0.0, -l.ring)), Some(Part::Ring), "N");
        assert_eq!(l.part_at(l.center + vec2(l.ring + 3.0, 0.0)), Some(Part::Ring), "E");
        assert_eq!(l.part_at(l.center + vec2(30.0, 30.0)), None, "between the face and the ring");
        assert!(l.bounds.contains_rect(l.home) && l.bounds.contains_rect(l.face));
    }

    #[test]
    fn viewcube_clicks_reach_the_cube_not_the_drawing() {
        let (mut app, ctx) = app_and_ctx();
        let l = Layout::new(canvas(&app));
        let fitted = view(&app);
        // Zoomed in, the Top face fits the drawing again (plan view, zoom extents) and nothing is
        // picked.
        let _ = app.run("zoom.in", json!({}));
        assert_ne!(view(&app), fitted);
        click(&mut app, &ctx, l.center, egui::PointerButton::Primary);
        assert_eq!(view(&app), fitted, "Top face zooms to extents");
        assert!(app.session.pending_window.is_none(), "the click didn't start a selection window");
        assert!(app.session.selection().is_empty());
        // Home turns to the south-west isometric view.
        click(&mut app, &ctx, l.home.center(), egui::PointerButton::Primary);
        assert!(!app.session.state().unwrap().view_dir.is_plan(), "Home is an isometric view");
        click(&mut app, &ctx, l.center, egui::PointerButton::Primary);
        assert!(app.session.state().unwrap().view_dir.is_plan());
        // A click on the ring doesn't touch the drawing.
        click(&mut app, &ctx, l.center + vec2(0.0, -l.ring), egui::PointerButton::Primary);
        assert!(app.session.pending_window.is_none());
        assert_eq!(view(&app), fitted);
        // A click in the drawing still reaches the drawing.
        let empty = canvas(&app).center() + vec2(0.0, -300.0);
        click(&mut app, &ctx, empty, egui::PointerButton::Primary);
        assert!(app.session.pending_window.is_some(), "an empty-space click starts a selection window");
    }

    /// A drag orbits when it started on the compass ring, wherever the pointer goes from there,
    /// and never when it started elsewhere and crossed the ring (#66).
    #[test]
    fn ring_drags_orbit_from_where_they_started() {
        let (mut app, ctx) = app_and_ctx();
        let l = Layout::new(canvas(&app));
        let m = egui::Modifiers::default();
        let drag = |app: &mut CadApp, from: Pos2, to: Pos2| {
            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: m };
            frame(app, &ctx, vec![egui::Event::PointerMoved(from)]);
            frame(app, &ctx, vec![button(from, true)]);
            frame(app, &ctx, vec![egui::Event::PointerMoved(to)]);
            frame(app, &ctx, vec![button(to, false)]);
        };
        let plan = |app: &CadApp| app.session.state().unwrap().view_dir.is_plan();
        drag(&mut app, l.center, l.center + vec2(0.0, -l.ring));
        assert!(plan(&app), "a drag from the face doesn't orbit");
        let north = l.center + vec2(0.0, -l.ring);
        drag(&mut app, north, north + vec2(-80.0, 0.0));
        assert!(!plan(&app), "a drag from the ring orbits out of it");
    }

    #[test]
    fn views_are_named_by_their_preset_or_custom() {
        let v = |x, y, z| ViewDir::new(Vec3::new(x, y, z));
        assert_eq!(view_name(ViewDir::PLAN), "Top");
        assert_eq!(view_name(v(-2.0, -2.0, 2.0)), "SW Isometric");
        assert_eq!(view_name(v(1.0, 0.0, 0.0)), "Right");
        assert_eq!(view_name(v(1.0, 0.0, 0.3)), "Custom View");
        assert_eq!((face_name(v(-1.0, -1.0, 1.0)), face_name(v(0.0, 0.0, -1.0)), face_name(v(0.2, -1.0, 0.1))), ("TOP", "BOTTOM", "FRONT"));
        assert_eq!((face_name(v(0.0, 1.0, 0.5)), face_name(v(-1.0, 0.2, 0.0)), face_name(v(1.0, 0.0, 0.0))), ("BACK", "LEFT", "RIGHT"));
    }

    #[test]
    fn ucs_icon_and_wcs_pill_open_the_coordinate_system_menu() {
        let (mut app, ctx) = app_and_ctx();
        let x = ucs_arms(canvas(&app), &app.canvas.xf.unwrap())[0];
        click(&mut app, &ctx, x.center(), egui::PointerButton::Primary);
        assert!(menu_shown(&ctx), "UCS icon menu");
        assert!(app.session.pending_window.is_none(), "the click stayed on the icon");
        let l = Layout::new(canvas(&app));
        click(&mut app, &ctx, l.pill.center(), egui::PointerButton::Primary);
        assert!(menu_shown(&ctx), "WCS menu");
        assert!(app.session.pending_window.is_none());
    }

    /// In a 3D view the UCS icon is the construction plane's three axes, further in from the
    /// corner: a click on its Z arm (up the screen there) opens the menu, not a selection window.
    #[test]
    fn the_3d_ucs_icon_is_clickable_where_it_is_drawn() {
        let (mut app, ctx) = app_and_ctx();
        let _ = app.run("view.swiso", json!({}));
        for _ in 0..500 {
            frame(&mut app, &ctx, Vec::new());
            if !app.canvas.building() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        frame(&mut app, &ctx, Vec::new());
        let xf = app.canvas.xf.unwrap();
        assert!(!xf.is_plan());
        let (o, arms) = crate::canvas::ucs_icon_arms(canvas(&app), &xf);
        let z = arms[2].1;
        assert!(z.y < o.y - 30.0, "Z points up the screen");
        click(&mut app, &ctx, o + (z - o) * 0.6, egui::PointerButton::Primary);
        assert!(menu_shown(&ctx), "UCS icon menu");
        assert!(app.session.pending_window.is_none());
    }
}
