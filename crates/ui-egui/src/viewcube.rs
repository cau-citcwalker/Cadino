//! The navigation overlays on the drawing area: the ViewCube (a shaded cube turned as the view
//! is, its compass ring, Home and the WCS menu) and the UCS icon.
//!
//! The cube is drawn in parallel projection along the current view's direction and twist (in
//! perspective too): its faces Top, Bottom, Front, Back, Left and Right, labelled on the faces.
//! Each face is split as AutoCAD's is into a middle, edge strips and corner squares; a click on a
//! middle looks straight at that face (the top is PLAN), on an edge between the two faces there,
//! on a corner from that corner (the south-west top one is the SW isometric). The compass ring
//! lies around the cube's foot on the XY plane, N toward +Y.
//!
//! Every overlay registers a widget that senses clicks *and* drags, so egui hands the pointer to
//! the overlay instead of the canvas underneath (a click on the ViewCube must never also pick or
//! start a selection window in the drawing). What the overlays do goes through commands:
//! `plan` and the `view.*` presets (faces), `vpoint` (edges and corners), `view.swiso` (Home),
//! `ui.toggle.viewcube`, `ui.toggle.ucsicon`; dragging the compass ring orbits the model view.
// ponytail: the cube follows the WCS, not the UCS (AutoCAD can orient it to the UCS), and has
// no rotate arrows in face views nor drag-to-orbit on the cube itself.

use cadcraft_geom::{Vec3, ViewDir};
use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, pos2, vec2};
use serde_json::json;

use crate::CadApp;
use crate::canvas::Xf;
use crate::theme::Tokens;

/// Half the cube's side on screen, in points.
const HALF: f32 = 26.0;

/// Where a face's middle ends and its edge strips begin, in the face's own coordinates (-1 to 1).
const STRIP: f64 = 0.6;

/// Where the ViewCube's parts are on the canvas.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub center: Pos2,
    /// Radius of the compass ring's centre line (in plan; the ring lies on the XY plane).
    pub ring: f32,
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
            home: Rect::from_center_size(center + vec2(-ring + 2.0, -ring + 2.0), vec2(20.0, 20.0)),
            pill: Rect::from_center_size(center + vec2(0.0, ring + 26.0), vec2(56.0, 16.0)),
            bounds: Rect::from_center_size(center, vec2(2.0 * (ring + 10.0), 2.0 * (ring + 10.0))),
        }
    }

    /// The cube as view `dir` shows it.
    pub fn cube(&self, dir: ViewDir) -> Cube {
        Cube { o: self.center, dir: dir.parallel(), ring: f64::from(self.ring / HALF) }
    }

    /// The part of the cube at `p` in view `dir` (not the pill, which is its own widget).
    pub fn part_at(&self, dir: ViewDir, p: Pos2) -> Option<Part> {
        if self.home.contains(p) {
            return Some(Part::Home);
        }
        let cube = self.cube(dir);
        if let Some(d) = cube.direction_at(p) {
            return Some(Part::Cube(d));
        }
        cube.on_ring(p).then_some(Part::Ring)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// A face's middle, an edge or a corner: the direction toward the viewer it looks from
    /// (each component -1, 0 or 1).
    Cube([i8; 3]),
    Home,
    Ring,
}

impl Part {
    fn tip(self) -> String {
        match self {
            Part::Cube(d) if d.iter().filter(|c| **c != 0).count() == 2 => {
                let mut names = FACES.iter().filter(|f| dot(f.n, d) > 0).map(|f| crate::i18n::t(f.title));
                format!("{} · {}", names.next().unwrap_or_default(), names.next().unwrap_or_default())
            }
            Part::Cube(d) => view_name(ViewDir::new(vec3(d))).to_string(),
            Part::Home => crate::tl!("Home: the south-west isometric view").to_string(),
            Part::Ring => crate::tl!("Compass: drag to orbit the view").to_string(),
        }
    }
}

/// A face of the cube: its outward normal, its right and up axes as its label reads seen from
/// its own view, its label and title (translated keys) and the command that looks at it.
struct Face {
    n: [i8; 3],
    right: [i8; 3],
    up: [i8; 3],
    title: &'static str,
    command: &'static str,
}

const FACES: [Face; 6] = [
    Face { n: [0, 0, 1], right: [1, 0, 0], up: [0, 1, 0], title: "Top", command: "plan" },
    Face { n: [0, 0, -1], right: [1, 0, 0], up: [0, -1, 0], title: "Bottom", command: "view.bottom" },
    Face { n: [0, -1, 0], right: [1, 0, 0], up: [0, 0, 1], title: "Front", command: "view.front" },
    Face { n: [0, 1, 0], right: [-1, 0, 0], up: [0, 0, 1], title: "Back", command: "view.back" },
    Face { n: [-1, 0, 0], right: [0, -1, 0], up: [0, 0, 1], title: "Left", command: "view.left" },
    Face { n: [1, 0, 0], right: [0, 1, 0], up: [0, 0, 1], title: "Right", command: "view.right" },
];

/// The label on face `i` (translated).
fn label(i: usize) -> &'static str {
    match i {
        0 => crate::tl!("TOP"),
        1 => crate::tl!("BOTTOM"),
        2 => crate::tl!("FRONT"),
        3 => crate::tl!("BACK"),
        4 => crate::tl!("LEFT"),
        _ => crate::tl!("RIGHT"),
    }
}

fn vec3(v: [i8; 3]) -> Vec3 {
    Vec3::new(f64::from(v[0]), f64::from(v[1]), f64::from(v[2]))
}

fn dot(a: [i8; 3], b: [i8; 3]) -> i32 {
    a.iter().zip(b).map(|(x, y)| i32::from(*x) * i32::from(y)).sum()
}

/// The cube on screen: centred on `o`, `HALF` points a unit, seen along `dir` (parallel). Its
/// corners are at ±1; the compass ring is `ring` units round on the plane of its foot.
#[derive(Clone, Copy, Debug)]
pub struct Cube {
    o: Pos2,
    dir: ViewDir,
    ring: f64,
}

impl Cube {
    /// Screen position of cube point `p`.
    pub fn at(&self, p: Vec3) -> Pos2 {
        let v = self.dir.view_of(p);
        self.o + vec2(v.x as f32 * HALF, -v.y as f32 * HALF)
    }
    /// How squarely face `f` faces the viewer (> 0: it shows).
    fn facing(&self, f: &Face) -> f64 {
        self.dir.dir.dot(vec3(f.n))
    }
    /// The point of face `f` at its own coordinates `a` (right) and `b` (up), each -1 to 1.
    fn on_face(f: &Face, a: f64, b: f64) -> Vec3 {
        vec3(f.n) + vec3(f.right) * a + vec3(f.up) * b
    }
    /// Face `f`'s own coordinates of screen point `p`; `None` seen edge-on.
    fn face_coords(&self, f: &Face, p: Pos2) -> Option<(f64, f64)> {
        let c = self.at(vec3(f.n));
        let (r, u) = (self.at(Self::on_face(f, 1.0, 0.0)) - c, self.at(Self::on_face(f, 0.0, 1.0)) - c);
        let cross = |x: egui::Vec2, y: egui::Vec2| f64::from(x.x) * f64::from(y.y) - f64::from(x.y) * f64::from(y.x);
        let det = cross(r, u);
        if det.abs() < 1.0 {
            return None;
        }
        let q = p - c;
        Some((cross(q, u) / det, cross(r, q) / det))
    }
    /// The direction (face, edge or corner) under screen point `p`, if the cube is there.
    pub fn direction_at(&self, p: Pos2) -> Option<[i8; 3]> {
        let strip = |a: f64| {
            if a > STRIP {
                1
            } else if a < -STRIP {
                -1
            } else {
                0
            }
        };
        FACES.iter().filter(|f| self.facing(f) > 1e-6).find_map(|f| {
            // A hair beyond the face too: a corner or edge point is on its faces.
            let (a, b) = self.face_coords(f, p).filter(|(a, b)| a.abs().max(b.abs()) <= 1.0 + 1e-3)?;
            let (sa, sb) = (strip(a), strip(b));
            Some([0, 1, 2].map(|i| f.n[i] + sa * f.right[i] + sb * f.up[i]))
        })
    }
    /// The compass ring's centre line on screen.
    fn ring_line(&self, radius: f64) -> Vec<Pos2> {
        (0..=72).map(|i| f64::from(i) * std::f64::consts::TAU / 72.0).map(|t| self.at(Vec3::new(radius * t.cos(), radius * t.sin(), -1.0))).collect()
    }
    /// Whether screen point `p` is on the compass ring (within 10 points of its centre line).
    fn on_ring(&self, p: Pos2) -> bool {
        self.ring_line(self.ring).windows(2).any(|w| {
            let (a, b) = (w[0], w[1]);
            let ab = b - a;
            let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
            (a + ab * t).distance(p) <= 10.0
        })
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

/// Look from direction `d` (a cube part): a face's own command, else VPOINT; zoomed to extents.
fn look(app: &mut CadApp, d: [i8; 3]) {
    match FACES.iter().find(|f| f.n == d) {
        Some(f) => {
            let _ = app.run(f.command, json!({}));
        }
        None => {
            let _ = app.run("vpoint", json!({ "direction": d.map(f64::from) }));
        }
    }
}

/// Face `f`'s region for part direction `d` (its middle, an edge strip or a corner square) as a
/// quad in the face's own coordinates.
fn region(f: &Face, d: [i8; 3]) -> [(f64, f64); 4] {
    let span = |s: i32| match s.signum() {
        1 => (STRIP, 1.0),
        -1 => (-1.0, -STRIP),
        _ => (-STRIP, STRIP),
    };
    let ((a0, a1), (b0, b1)) = (span(dot(f.right, d)), span(dot(f.up, d)));
    [(a0, b0), (a1, b0), (a1, b1), (a0, b1)]
}

/// Paint the ViewCube's ring, compass letters, cube and Home button, and handle the pointer. The
/// canvas paints the WCS pill itself.
pub fn show(app: &mut CadApp, ui: &mut egui::Ui, l: &Layout) {
    // The cube turns with the Model tab's view (layouts and their viewports show plan).
    let dir = match app.session.state() {
        Ok(st) if st.space == cadcraft_doc::Space::Model => st.screen_dir(),
        _ => ViewDir::PLAN,
    };
    let cube = l.cube(dir);
    let resp = ui.interact(l.bounds, ui.id().with("viewcube"), Sense::click_and_drag());
    let part = resp.hover_pos().and_then(|p| l.part_at(dir, p));
    let p = ui.painter_at(l.bounds.union(l.pill));
    paint_ring(&p, &cube, part == Some(Part::Ring));
    paint_cube(
        &p,
        ui.ctx(),
        &cube,
        match part {
            Some(Part::Cube(d)) => Some(d),
            _ => None,
        },
    );
    paint_home(&p, l.home, part == Some(Part::Home));

    let resp = match part {
        Some(part) => resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(part.tip()),
        None => resp,
    };
    let pressed_on = resp.interact_pointer_pos().and_then(|o| l.part_at(dir, o));
    // A drag belongs to the part it started on, wherever the pointer goes.
    let dragged_from = ui.input(|i| i.pointer.press_origin()).and_then(|o| l.part_at(dir, o));
    if resp.clicked() {
        match pressed_on {
            Some(Part::Cube(d)) => look(app, d),
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
        if ui.button(crate::tl!("Home")).clicked() {
            action = Some(("view.swiso", json!({})));
            ui.close();
        }
        if ui.button(crate::tl!("Hide ViewCube")).clicked() {
            action = Some(("ui.toggle.viewcube", json!({"on": false})));
            ui.close();
        }
    });
    if let Some((id, params)) = action {
        let _ = app.run(id, params);
    }
    // The WCS pill (painted by the canvas) opens the coordinate-system menu.
    let pill = ui.interact(l.pill, ui.id().with("viewcube_wcs"), Sense::click_and_drag()).on_hover_cursor(CursorIcon::PointingHand);
    let pill = pill.on_hover_text(crate::tl!("Coordinate system"));
    ucs_menu(app, egui::Popup::menu(&pill));
}

/// The compass ring round the cube's foot, as a band on the XY plane, with N, S, E and W on it.
fn paint_ring(p: &egui::Painter, cube: &Cube, hot: bool) {
    let col = if hot { Color32::from_rgb(0x5c, 0x67, 0x7c) } else { Color32::from_rgb(0x48, 0x50, 0x5c) };
    // 9 points wide in plan, foreshortened with the plane.
    let w = 4.5 / f64::from(HALF);
    let (inner, outer) = (cube.ring_line(cube.ring - w), cube.ring_line(cube.ring + w));
    let mut band = egui::Mesh::default();
    for (a, b) in inner.iter().zip(&outer) {
        band.colored_vertex(*a, col);
        band.colored_vertex(*b, col);
    }
    for i in 0..inner.len().saturating_sub(1) as u32 {
        let k = 2 * i;
        band.add_triangle(k, k + 1, k + 2);
        band.add_triangle(k + 1, k + 3, k + 2);
    }
    p.add(egui::Shape::mesh(band));
    p.add(egui::Shape::line(cube.ring_line(cube.ring + w), Stroke::new(1.0, Color32::from_rgb(0x5c, 0x65, 0x72))));
    let f = egui::FontId::proportional(15.0);
    let lc = Color32::from_rgb(0xc8, 0xcc, 0xd2);
    let r = cube.ring;
    for (letter, x, y) in [(crate::tl!("N"), 0.0, r), (crate::tl!("S"), 0.0, -r), (crate::tl!("E"), r, 0.0), (crate::tl!("W"), -r, 0.0)] {
        p.text(cube.at(Vec3::new(x, y, -1.0)), egui::Align2::CENTER_CENTER, letter, f.clone(), lc);
    }
}

/// The cube's faces the viewer sees, shaded by a light from the upper left, labelled, outlined,
/// and the part `hot` (under the pointer) highlighted on each face it touches.
fn paint_cube(p: &egui::Painter, ctx: &egui::Context, cube: &Cube, hot: Option<[i8; 3]>) {
    let light = Vec3::new(-0.35, 0.55, 1.0).normalized();
    let edge = Stroke::new(1.0, Color32::from_rgb(0x5a, 0x5e, 0x66));
    let quad = |f: &Face, r: [(f64, f64); 4]| r.map(|(a, b)| cube.at(Cube::on_face(f, a, b))).to_vec();
    for (i, f) in FACES.iter().enumerate() {
        let facing = cube.facing(f);
        if facing <= 1e-6 {
            continue;
        }
        let n = cube.dir.view_of(vec3(f.n));
        let k = (0.62 + 0.38 * n.dot(light).max(0.0)) as f32;
        let shade = |c: u8| (f32::from(c) * k).round().clamp(0.0, 255.0) as u8;
        let fill = Color32::from_rgb(shade(0xe4), shade(0xe7), shade(0xec));
        let corners = quad(f, [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]);
        p.add(egui::Shape::convex_polygon(corners.clone(), fill, Stroke::NONE));
        if let Some(d) = hot.filter(|d| dot(f.n, *d) > 0) {
            p.add(egui::Shape::convex_polygon(quad(f, region(f, d)), Color32::from_rgba_unmultiplied(0x3c, 0x8c, 0xe6, 170), Stroke::NONE));
        }
        if facing > 0.2 {
            p.add(egui::Shape::mesh(label_mesh(p, ctx, cube, f, label(i), Color32::from_rgb(0x3c, 0x40, 0x46))));
        }
        p.add(egui::Shape::closed_line(corners, edge));
    }
}

/// `text` laid on face `f`: its glyphs mapped onto the face as the cube shows it, upright as the
/// face is seen from its own view, at most 1.5 units wide.
fn label_mesh(p: &egui::Painter, ctx: &egui::Context, cube: &Cube, f: &Face, text: &str, color: Color32) -> egui::Mesh {
    let mut mesh = egui::Mesh::default();
    let galley = p.layout_no_wrap(text.to_owned(), egui::FontId::proportional(13.0), color);
    let size = galley.size();
    if size.x <= 0.0 || size.y <= 0.0 {
        return mesh;
    }
    // Face units per galley point.
    let k = f64::from((1.5 * HALF / size.x).min(0.55 * HALF / size.y).min(1.0) / HALF);
    let atlas = ctx.fonts(|f| f.font_image_size());
    watch_atlas(ctx, atlas);
    let [tw, th] = atlas.map(|v| v.max(1) as f32);
    for row in &galley.rows {
        let Ok(base) = u32::try_from(mesh.vertices.len()) else { break };
        for v in &row.visuals.mesh.vertices {
            let g = row.pos + v.pos.to_vec2();
            let (a, b) = (f64::from(g.x - size.x / 2.0) * k, f64::from(size.y / 2.0 - g.y) * k);
            let color = if v.color == Color32::PLACEHOLDER { color } else { v.color };
            mesh.vertices.push(egui::epaint::Vertex { pos: cube.at(Cube::on_face(f, a, b)), uv: pos2(v.uv.x / tw, v.uv.y / th), color });
        }
        mesh.indices.extend(row.visuals.mesh.indices.iter().map(|i| i + base));
    }
    mesh
}

/// The font atlas size the labels' texture coordinates were made with this pass.
fn atlas_id() -> egui::Id {
    egui::Id::new("cadino_viewcube_label_atlas")
}

/// Labels normalise their glyphs' texture coordinates with the font atlas size as they are
/// painted ([`label_mesh`]); egui's own text does it at the end of the frame. Text laid out later
/// in the pass can grow the atlas, which would leave the labels sampling the wrong glyphs, so the
/// end of the pass checks: if the atlas grew since, the pass is done again (and not shown).
// ponytail: egui's own end-of-pass plugins run after this one; one laying out new glyphs would
// slip by (none does today).
fn watch_atlas(ctx: &egui::Context, size: [usize; 2]) {
    let watched = egui::Id::new("cadino_viewcube_atlas_watch");
    let registered = ctx.data_mut(|d| {
        d.insert_temp(atlas_id(), size);
        let was = d.get_temp::<bool>(watched).unwrap_or(false);
        d.insert_temp(watched, true);
        was
    });
    if !registered {
        ctx.on_end_pass(
            "cadino viewcube labels",
            std::sync::Arc::new(|ui: &mut egui::Ui| {
                let ctx = ui.ctx();
                let used = ctx.data_mut(|d| d.remove_temp::<[usize; 2]>(atlas_id()));
                if used.is_some_and(|u| u != ctx.fonts(|f| f.font_image_size())) {
                    ctx.request_discard("the ViewCube's labels met a font atlas that grew");
                }
            }),
        );
    }
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

    fn dir(app: &CadApp) -> ViewDir {
        app.session.state().unwrap().screen_dir()
    }

    fn iso() -> ViewDir {
        ViewDir::named("iso").unwrap()
    }

    #[test]
    fn parts_are_found_where_they_are_drawn() {
        let l = Layout::new(Rect::from_min_size(pos2(200.0, 80.0), vec2(1000.0, 800.0)));
        let plan = ViewDir::PLAN;
        let at = |p: Pos2| l.part_at(plan, p);
        // In plan only the top shows: its middle, its edges and corners.
        assert_eq!(at(l.center), Some(Part::Cube([0, 0, 1])));
        assert_eq!(at(l.center + vec2(0.0, -0.8 * HALF)), Some(Part::Cube([0, 1, 1])), "the north edge (up the screen)");
        assert_eq!(at(l.center + vec2(0.8 * HALF, 0.8 * HALF)), Some(Part::Cube([1, -1, 1])), "the south-east corner");
        assert_eq!(at(l.home.center()), Some(Part::Home));
        assert_eq!(at(l.center + vec2(0.0, -l.ring)), Some(Part::Ring), "N");
        assert_eq!(at(l.center + vec2(l.ring + 3.0, 0.0)), Some(Part::Ring), "E");
        assert_eq!(at(l.center + vec2(30.0, 30.0)), None, "between the cube and the ring");
        assert!(l.bounds.contains_rect(l.home));
        // South-west isometric: the corner nearest the viewer is in the middle, the top above
        // it, the front to the lower right and the left to the lower left.
        let c = l.cube(iso());
        assert_eq!(l.part_at(iso(), l.center), Some(Part::Cube([-1, -1, 1])));
        assert_eq!(l.part_at(iso(), c.at(Vec3::new(0.0, 0.0, 1.0))), Some(Part::Cube([0, 0, 1])));
        assert_eq!(l.part_at(iso(), c.at(Vec3::new(0.0, -1.0, 0.0))), Some(Part::Cube([0, -1, 0])));
        assert_eq!(l.part_at(iso(), c.at(Vec3::new(-1.0, 0.0, 0.0))), Some(Part::Cube([-1, 0, 0])));
        assert!(c.at(Vec3::new(0.0, 0.0, 1.0)).y < l.center.y - 10.0);
        assert!(c.at(Vec3::new(0.0, -1.0, 0.0)).x > l.center.x && c.at(Vec3::new(-1.0, 0.0, 0.0)).x < l.center.x);
        assert_eq!(l.part_at(iso(), c.at(Vec3::new(0.0, -1.0, 1.0)) + vec2(0.0, 2.0)), Some(Part::Cube([0, -1, 1])), "the front top edge");
        // The ring lies round the cube's foot: its north end is up and behind the cube.
        assert_eq!(l.part_at(iso(), c.at(Vec3::new(0.0, -c.ring, -1.0))), Some(Part::Ring), "S");
        // Back, bottom and right never show from there.
        for d in [[0, 0, -1], [0, 1, 0], [1, 0, 0]] {
            let f = FACES.iter().find(|f| f.n == d).unwrap();
            assert!(c.facing(f) < 0.0);
        }
    }

    /// The cube turns with a twisted view: twisted 90°, the top's right (+X) points up the
    /// screen, and in a perspective view it follows the direction.
    #[test]
    fn the_cube_follows_twisted_and_perspective_views() {
        let l = Layout::new(Rect::from_min_size(pos2(200.0, 80.0), vec2(1000.0, 800.0)));
        let twisted = ViewDir { twist: std::f64::consts::FRAC_PI_2, ..ViewDir::PLAN };
        let c = l.cube(twisted);
        let x = c.at(Vec3::new(1.0, 0.0, 1.0)) - c.at(Vec3::new(0.0, 0.0, 1.0));
        assert!(x.y < -HALF + 0.01 && x.x.abs() < 0.01, "{x:?}");
        assert_eq!(l.part_at(twisted, l.center + vec2(0.0, -0.8 * HALF)), Some(Part::Cube([1, 0, 1])), "east is up the screen");
        let cam = cadcraft_geom::Camera::new(50.0, 0.0);
        let persp = iso().with_camera(Some(cam), cadcraft_geom::Vec2::ZERO, 100.0);
        assert_eq!(l.part_at(persp, l.center), Some(Part::Cube([-1, -1, 1])));
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
        assert!(!dir(&app).is_plan(), "Home is an isometric view");
        // In it the top face is above the middle: a click there is plan again.
        let top = l.cube(dir(&app)).at(Vec3::new(0.0, 0.0, 1.0));
        click(&mut app, &ctx, top, egui::PointerButton::Primary);
        assert!(dir(&app).is_plan());
        // The south-east corner of the top (lower right in plan) looks from the south-east.
        click(&mut app, &ctx, l.center + vec2(0.85 * HALF, 0.85 * HALF), egui::PointerButton::Primary);
        assert!((dir(&app).dir - ViewDir::named("iso_se").unwrap().dir).len() < 1e-9, "{:?}", dir(&app).dir);
        // Its front edge looks between the front and the top.
        let front_top = l.cube(dir(&app)).at(Vec3::new(0.0, -1.0, 1.0)) + vec2(0.0, 2.0);
        click(&mut app, &ctx, front_top, egui::PointerButton::Primary);
        assert!((dir(&app).dir - Vec3::new(0.0, -1.0, 1.0).normalized()).len() < 1e-9, "{:?}", dir(&app).dir);
        // Its front face is the front view.
        let front = l.cube(dir(&app)).at(Vec3::new(0.0, -1.0, 0.0));
        click(&mut app, &ctx, front, egui::PointerButton::Primary);
        assert!((dir(&app).dir - Vec3::new(0.0, -1.0, 0.0)).len() < 1e-9);
        click(&mut app, &ctx, l.center, egui::PointerButton::Primary);
        assert!((dir(&app).dir - Vec3::new(0.0, -1.0, 0.0)).len() < 1e-9, "the face looked at stays");
        // Back to plan; a click on the ring doesn't touch the drawing.
        let _ = app.run("plan", json!({}));
        let fitted = view(&app);
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
        assert_eq!(Part::Cube([0, -1, 1]).tip(), "Top · Front");
        assert_eq!(Part::Cube([1, 1, 1]).tip(), "NE Isometric");
    }

    /// A face's label is laid on the face: its glyphs' corners are inside the middle of the face
    /// as the cube shows it (here the front, foreshortened in the south-west isometric).
    #[test]
    fn face_labels_lie_on_their_faces() {
        let l = Layout::new(Rect::from_min_size(pos2(200.0, 80.0), vec2(1000.0, 800.0)));
        let cube = l.cube(iso());
        let ctx = egui::Context::default();
        let mut mesh = egui::Mesh::default();
        for _ in 0..2 {
            ctx.run_ui(Default::default(), |ui| mesh = label_mesh(ui.painter(), ui.ctx(), &cube, &FACES[2], "FRONT", Color32::BLACK))
                .textures_delta
                .clear();
        }
        assert!(mesh.vertices.len() >= 4 * 5, "a quad a letter");
        for v in &mesh.vertices {
            let (a, b) = cube.face_coords(&FACES[2], v.pos).unwrap();
            assert!(a.abs() <= 0.8 && b.abs() <= 0.35, "{a} {b}");
        }
    }

    /// Text laid out after the labels that grows the font atlas has egui do the pass again, so
    /// the labels' texture coordinates match the atlas the frame is drawn with.
    #[test]
    fn labels_redo_the_pass_when_the_font_atlas_grows_after_them() {
        let l = Layout::new(Rect::from_min_size(pos2(200.0, 80.0), vec2(1000.0, 800.0)));
        let cube = l.cube(iso());
        let ctx = egui::Context::default();
        let pass = |grow: bool| {
            let mut passes = 0;
            ctx.run_ui(Default::default(), |ui| {
                passes += 1;
                let _ = label_mesh(ui.painter(), ui.ctx(), &cube, &FACES[2], "FRONT", Color32::BLACK);
                if grow && passes == 1 {
                    for size in 20..48 {
                        let text = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789".to_string();
                        let _ = ui.painter().layout_no_wrap(text, egui::FontId::proportional(size as f32), Color32::BLACK);
                    }
                }
            })
            .textures_delta
            .clear();
            passes
        };
        assert_eq!(pass(false), 1);
        assert_eq!(pass(false), 1, "no new glyphs: one pass");
        let before = ctx.fonts(|f| f.font_image_size());
        assert_eq!(pass(true), 2, "the pass is done again");
        assert_ne!(ctx.fonts(|f| f.font_image_size()), before, "the atlas grew");
        assert_eq!(pass(false), 1);
    }

    /// The cube draws its faces' labels as meshes on the font texture, in every kind of view,
    /// without panicking.
    #[test]
    fn the_cube_paints_in_every_view() {
        let (mut app, ctx) = app_and_ctx();
        for v in ["view.swiso", "view.front", "view.bottom", "view.nwiso"] {
            let _ = app.run(v, json!({}));
            frame(&mut app, &ctx, Vec::new());
        }
        let _ = app.run("dview", json!({ "twist": 30, "distance": 3000 }));
        frame(&mut app, &ctx, Vec::new());
        assert!(dir(&app).persp.is_some() && dir(&app).twist != 0.0);
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
