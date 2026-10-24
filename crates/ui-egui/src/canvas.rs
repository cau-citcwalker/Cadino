//! The drawing area: grid, entities, selection highlighting, grips, rubber bands, object-snap
//! markers, crosshair cursor, ViewCube, UCS icon and mouse handling.

use cadcraft_color::Rgb;
use cadcraft_doc::Handle;
use cadcraft_engine::Input;
use cadcraft_engine::snap::{self, SnapHit};
use cadcraft_geom::{Bounds2, Mat3, Mat4, Vec2, ViewDir};
use cadcraft_render::{DisplayList, Kind};
use egui::{Color32, Pos2, Rect, Sense, Shape, Stroke, pos2, vec2};

use crate::CadApp;
use crate::theme::Tokens;

/// World ↔ screen transform for the canvas rect.
///
/// The drawing points it gives and takes ([`Xf::to_world`], [`Xf::to_screen`]) are in the
/// coordinates of its construction plane (CPlane, Rhino's; AutoCAD's UCS): on the CPlane's XY
/// plane, where the view ray through the cursor meets it. With the World CPlane (the identity)
/// they are world XY points, as they always were. In a perspective view the view plane is the
/// camera's target plane and the rays come from the eye ([`ViewDir::ray`]).
#[derive(Clone, Copy, Debug)]
pub struct Xf {
    pub rect: Rect,
    /// View-plane centre (display lists are in view-plane coordinates).
    pub center: Vec2,
    /// Pixels (points) per drawing unit.
    pub scale: f64,
    /// CPlane XY plane → view plane: the identity in plan view with the World CPlane, the view
    /// direction's projection of the CPlane in a 3D view (its parallel projection: a perspective
    /// view maps points through the eye instead).
    plane: Mat3,
    /// View plane → CPlane XY plane: `plane` inverted, or where the view sees the CPlane edge-on
    /// the least-squares inverse (the nearest point of the plane's line, [`unproject`]).
    unplane: Mat3,
    /// The view direction display lists for this transform are built with.
    dir: ViewDir,
    /// The construction plane: CPlane coordinates → world (identity: World, the WCS XY plane).
    cplane: Mat4,
}

impl Xf {
    pub fn new(rect: Rect, center: Vec2, scale: f64) -> Self {
        Xf { rect, center, scale, plane: Mat3::IDENTITY, unplane: Mat3::IDENTITY, dir: ViewDir::PLAN, cplane: Mat4::IDENTITY }
    }
    /// The same transform looking along `dir`.
    pub fn looking(self, dir: ViewDir) -> Self {
        Xf { dir, ..self }.on(self.cplane)
    }
    /// The same transform drawing on construction plane `c` (CPlane → world).
    pub fn on(self, c: Mat4) -> Self {
        let par = self.dir.parallel();
        if c == Mat4::IDENTITY {
            // `unproject_xy` is linear in parallel: its matrix is where it takes the axes.
            let (x, y) = (par.unproject_xy(Vec2::new(1.0, 0.0)), par.unproject_xy(Vec2::new(0.0, 1.0)));
            let unplane = Mat3 { a: x.x, b: x.y, c: y.x, d: y.y, e: 0.0, f: 0.0 };
            return Xf { plane: par.plane_xf(), unplane, cplane: c, ..self };
        }
        let plane = par.rotation().mul(&c).plane_xy();
        Xf { plane, unplane: unproject(&plane), cplane: c, ..self }
    }
    /// The same view drawing on the World plane: [`Xf::to_screen`] then takes world XY points
    /// (for overlays of world geometry: constraints, compare boxes).
    pub fn world(self) -> Self {
        self.on(Mat4::IDENTITY)
    }
    /// The construction plane (CPlane → world).
    pub fn cplane(&self) -> Mat4 {
        self.cplane
    }
    /// The view direction.
    pub fn dir(&self) -> ViewDir {
        self.dir
    }
    /// Screen position of a drawing point (on the CPlane's XY plane).
    pub fn to_screen(&self, p: Vec2) -> Pos2 {
        if self.dir.persp.is_some() {
            return self.world_to_screen(self.cplane.apply(p.to3(0.0)));
        }
        self.view_to_screen(self.plane.apply(p))
    }
    /// Where segment `a`–`b` (drawing points, on the CPlane's XY plane) shows on screen: in
    /// perspective only what is in front of the eye, `None` when none of it is.
    pub fn segment_to_screen(&self, a: Vec2, b: Vec2) -> Option<[Pos2; 2]> {
        let Some(c) = self.dir.persp else { return Some([self.to_screen(a), self.to_screen(b)]) };
        let (mut p, mut q) = (self.dir.view_of(self.cplane.apply(a.to3(0.0))), self.dir.view_of(self.cplane.apply(b.to3(0.0))));
        let lim = c.eye.z - c.near() * 1.001;
        if p.z > lim && q.z > lim {
            return None;
        }
        let cut = |s: cadcraft_geom::Vec3, t: cadcraft_geom::Vec3| {
            let x = s + (t - s) * ((lim - s.z) / (t.z - s.z));
            cadcraft_geom::Vec3::new(x.x, x.y, lim)
        };
        if p.z > lim {
            p = cut(p, q);
        } else if q.z > lim {
            q = cut(q, p);
        }
        let (Some((pa, _)), Some((pb, _))) = (c.map(p), c.map(q)) else { return None };
        Some([self.view_to_screen(pa), self.view_to_screen(pb)])
    }
    /// Screen position of a CPlane point with its height (z along the CPlane's normal).
    pub fn local_to_screen(&self, p: cadcraft_geom::Vec3) -> Pos2 {
        self.world_to_screen(self.cplane.apply(p))
    }
    /// Screen position of a world point.
    pub fn world_to_screen(&self, p: cadcraft_geom::Vec3) -> Pos2 {
        self.view_to_screen(self.dir.project(p))
    }
    /// Screen position of a view-plane (display list) point.
    pub fn view_to_screen(&self, p: Vec2) -> Pos2 {
        let c = self.rect.center();
        pos2(c.x + ((p.x - self.center.x) * self.scale) as f32, c.y - ((p.y - self.center.y) * self.scale) as f32)
    }
    /// The view-plane point under a screen position.
    pub fn to_view(&self, p: Pos2) -> Vec2 {
        let c = self.rect.center();
        Vec2::new(self.center.x + f64::from(p.x - c.x) / self.scale, self.center.y - f64::from(p.y - c.y) / self.scale)
    }
    /// The drawing point under a screen position: where the view ray meets the CPlane, in its
    /// coordinates (seen edge-on, or in perspective above its horizon, the nearest point of the
    /// plane's line in the parallel view).
    pub fn to_world(&self, p: Pos2) -> Vec2 {
        if self.dir.persp.is_some()
            && let Some(w) = self.ray_on(p, self.cplane.col(3), self.cplane.col(2))
            && let Some(ci) = self.cplane.inverse()
        {
            let q = ci.apply(w).xy();
            if q.is_finite() {
                return q;
            }
        }
        self.unplane.apply(self.to_view(p))
    }
    /// The world point where the view ray under screen position `at` meets the plane through `o`
    /// with normal `n`; `None` when the view sees that plane edge-on (or, in perspective, the ray
    /// meets it behind the eye).
    pub fn ray_on(&self, at: Pos2, o: cadcraft_geom::Vec3, n: cadcraft_geom::Vec3) -> Option<cadcraft_geom::Vec3> {
        let v = self.to_view(at);
        if self.dir.persp.is_some() {
            return self.dir.ray_hit(v, o, n);
        }
        let (x, y, d) = self.dir.axes();
        let p0 = x * v.x + y * v.y;
        let nd = n.dot(d);
        if nd.abs() < 1e-6 * n.len() {
            return None;
        }
        let p = p0 + d * (n.dot(o - p0) / nd);
        p.is_finite().then_some(p)
    }
    /// A plan view (looking down world Z): the canvas works as AutoCAD's 2D drawing area, with
    /// AutoCAD's picks, grips and UCS icon, whatever the construction plane (a UCS moved or
    /// turned in plan keeps them; points still arrive in its coordinates). Every other direction
    /// is a 3D view with Rhino's ways (gumball, picking what is drawn, the 3D UCS icon).
    pub fn is_plan(&self) -> bool {
        self.dir.is_plan()
    }
    /// Whether picks and selection windows take what is drawn under the cursor and in the
    /// rectangle on screen ([`list_pick`], [`list_window`]): in a 3D view, and in plan with a
    /// tilted construction plane, where the engine would pick and window in the plane's
    /// coordinates, not as the screen shows them. Plan with a level UCS (moved or turned) is the
    /// engine's, which selects the screen's windows there itself.
    fn picks_drawn(&self) -> bool {
        let n = self.cplane.col(2);
        !self.is_plan() || n.x.hypot(n.y) > 1e-9
    }
    /// Drawing points are view-plane points: plan view with the World CPlane, where the old 2D
    /// shortcuts (an axis-aligned grid, a drag's matrix as it is) hold.
    fn flat(&self) -> bool {
        self.plane == Mat3::IDENTITY && self.dir.persp.is_none()
    }
    /// Whether the view sees the CPlane edge-on: points picked land on its line. In perspective,
    /// when the eye is in the plane.
    pub fn edge_on(&self) -> bool {
        match self.dir.eye() {
            Some(e) => {
                let n = self.cplane.col(2);
                n.dot(e - self.cplane.col(3)).abs() <= 1e-9 * (1.0 + e.len())
            }
            None => self.plane.det().abs() <= 1e-6,
        }
    }
    /// The visible area in view-plane coordinates.
    pub fn view_bounds(&self) -> Bounds2 {
        Bounds2::new(self.to_view(self.rect.left_bottom()), self.to_view(self.rect.right_top()))
    }
}

/// View plane → plane for `m` (plane → view plane): its inverse, or where the view sees the plane
/// edge-on (or nearly) the least-squares one, onto the plane's projected line (as
/// [`ViewDir::unproject_xy`]).
fn unproject(m: &Mat3) -> Mat3 {
    if m.det().abs() > 1e-6
        && let Some(i) = m.inverse()
    {
        return i;
    }
    // Rank one: the pseudo-inverse is the transpose over the squared Frobenius norm.
    let n2 = m.a * m.a + m.b * m.b + m.c * m.c + m.d * m.d;
    if n2 < 1e-24 {
        return Mat3::IDENTITY;
    }
    let (a, b, c, d) = (m.a / n2, m.c / n2, m.b / n2, m.d / n2);
    Mat3 { a, b, c, d, e: -(a * m.e + c * m.f), f: -(b * m.e + d * m.f) }
}

/// `m`, a transform of CPlane coordinates (a MOVE, ROTATE … drag), in the world: C·M·C⁻¹, z scaled
/// with the plane as [`cadcraft_doc::Entity::transform`] scales it.
fn in_world(c: &Mat4, m: &Mat3) -> Mat4 {
    let mut t = Mat4::from_mat3(m);
    t.m[2][2] = m.det().abs().sqrt();
    match c.inverse() {
        Some(ci) => c.mul(&t).mul(&ci),
        None => t,
    }
}

/// Preview entities built in CPlane coordinates (a command's rubber band), placed in the world.
/// ponytail: one that can't be placed (a dimension tilted out of the horizontal) previews flat.
fn placed(ents: Vec<cadcraft_doc::Entity>, c: &Mat4) -> Vec<cadcraft_doc::Entity> {
    if *c == Mat4::IDENTITY {
        return ents;
    }
    ents.into_iter()
        .map(|mut e| {
            let _ = e.transform3(c);
            e
        })
        .collect()
}

/// The model view's transform for the canvas `rect` at `view` (its centre is in the view plane),
/// and the one the canvas shows, picks and snaps with: the same, except while the list for a
/// new view direction builds in the background (see [`ensure_list`]), when it keeps the current
/// list's direction about the same point. Plan outside model space (layouts, MSPACE viewports).
fn view_xfs(app: &CadApp, rect: Rect, view: cadcraft_engine::View) -> (Xf, Xf) {
    let (dir, pivot) = match app.session.state() {
        Ok(st) if st.space == cadcraft_doc::Space::Model => (st.look().projection(view), st.orbit_target),
        _ => (ViewDir::PLAN, None),
    };
    let cplane = app.session.cplane();
    let scale = f64::from(rect.height()) / view.height.max(1e-12);
    let now = Xf::new(rect, view.center, scale).looking(dir).on(cplane);
    let built = app.canvas.list_dir;
    if app.canvas.list.is_none() || built == dir {
        return (now, now);
    }
    // The same direction through another camera (a perspective view panned or dollied): the
    // target plane stays, so the current list shows it where it is now.
    if built.dir == dir.dir {
        return (now, Xf { dir: built, ..now });
    }
    // The world point at the centre of the view, as the current list sees it: the point the
    // last orbit turned about, else the one on the CPlane.
    let center = match pivot {
        Some(t) => built.project(t),
        None if cplane == Mat4::IDENTITY && built.persp.is_none() => built.plane_xf().apply(now.unplane.apply(view.center)),
        None => built.project(cplane.apply(now.unplane.apply(view.center).to3(0.0))),
    };
    (now, Xf::new(rect, center, scale).looking(built).on(cplane))
}

/// Whether `v` is inside triangle `abc` (either winding); never for a zero-area triangle, which
/// draws nothing.
fn in_tri(v: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    let s = |p: Vec2, q: Vec2| (q - p).cross(v - p);
    let (d1, d2, d3) = (s(a, b), s(b, c), s(c, a));
    (b - a).cross(c - a) != 0.0 && ((d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0))
}

/// The topmost object drawn within `ap` of view-plane point `v`: how a 3D view picks (the
/// engine's spatial index is in plan).
/// ponytail: scans the whole list per pick; a spatial index over the list if 3D views get large.
fn list_pick(list: &DisplayList, v: Vec2, ap: f64) -> Option<Handle> {
    let near = |a: Vec2, b: Vec2| cadcraft_geom::Line::new(a, b).closest(v).dist(v) <= ap;
    // A solid's faces in the Hidden style are masks: picked like shaded faces (a wipeout isn't).
    let face = |i: usize| list.faces.iter().any(|r| (r.start as usize..r.end as usize).contains(&i));
    list.prims.iter().enumerate().rev().filter(|(_, p)| p.handle != cadcraft_render::VIEWPORT_CONTENT).find_map(|(i, prim)| {
        let pts = list.points(prim);
        let hit = match prim.kind {
            Kind::Polyline => pts.windows(2).any(|w| matches!(w, [a, b] if near(*a, *b))),
            Kind::Tris => pts.chunks_exact(3).any(|t| matches!(t, [a, b, c] if in_tri(v, *a, *b, *c))),
            Kind::Mask if face(i) => pts.chunks_exact(3).any(|t| matches!(t, [a, b, c] if in_tri(v, *a, *b, *c))),
            Kind::Point => pts.first().is_some_and(|q| q.dist(v) <= ap),
            Kind::Mask | Kind::Infinite { .. } => false,
        };
        hit.then_some(prim.handle)
    })
}

/// Whether segment `ab` is inside or crosses box `bx`.
fn seg_hits_box(a: Vec2, b: Vec2, bx: &Bounds2) -> bool {
    let c = bx.corners();
    let ab = cadcraft_geom::Line::new(a, b);
    bx.contains(a)
        || bx.contains(b)
        || c.iter().zip(c.iter().cycle().skip(1)).any(|(p, q)| cadcraft_geom::line_line(&ab, &cadcraft_geom::Line::new(*p, *q)).is_some())
}

/// The objects the list draws in view-plane box `bx`: wholly inside it (a window), or with
/// `crossing` inside or crossing it. How a 3D view selects with a window (the engine selects in
/// plan).
fn list_window(list: &DisplayList, bx: Bounds2, crossing: bool) -> Vec<Handle> {
    // Per object: every part inside, any part touching.
    let mut seen: std::collections::BTreeMap<Handle, (bool, bool)> = std::collections::BTreeMap::new();
    for prim in list.prims.iter().filter(|p| p.handle != cadcraft_render::VIEWPORT_CONTENT) {
        let pts = list.points(prim);
        if pts.is_empty() {
            continue;
        }
        let inside = !matches!(prim.kind, Kind::Infinite { .. }) && pts.iter().all(|q| bx.contains(*q));
        let touches = inside
            || (crossing
                && match prim.kind {
                    Kind::Polyline => pts.windows(2).any(|w| matches!(w, [a, b] if seg_hits_box(*a, *b, &bx))) || pts.iter().any(|q| bx.contains(*q)),
                    Kind::Tris | Kind::Mask => pts.chunks_exact(3).any(|t| {
                        matches!(t, [a, b, c] if seg_hits_box(*a, *b, &bx) || seg_hits_box(*b, *c, &bx) || seg_hits_box(*c, *a, &bx)
                            || bx.corners().iter().any(|k| in_tri(*k, *a, *b, *c)))
                    }),
                    Kind::Point => false,
                    Kind::Infinite { ray } => match pts {
                        [base, d, ..] => {
                            let far = (bx.width() + bx.height()) * 2.0 + base.dist(bx.center());
                            seg_hits_box(if ray { *base } else { *base - *d * far }, *base + *d * far, &bx)
                        }
                        _ => false,
                    },
                });
        let e = seen.entry(prim.handle).or_insert((true, false));
        *e = (e.0 && inside, e.1 || touches);
    }
    seen.into_iter().filter(|(_, (all, any))| if crossing { *any } else { *all }).map(|(h, _)| h).collect()
}

/// [`list_pick`] at the cursor with the pick box.
fn view_pick_at(app: &CadApp, xf: &Xf, hover: Option<Pos2>) -> Option<Handle> {
    let ap = app.session.settings.pickbox.max(1.0) * 1.5 / xf.scale;
    hover.zip(app.canvas.list.as_ref()).and_then(|(hp, l)| list_pick(l, xf.to_view(hp), ap))
}

/// Open a selection window at screen point `at` (see [`cadcraft_engine::Session::begin_window`]),
/// remembering where it is in the view plane; returns whether a window was opened.
fn begin_view_window(app: &mut CadApp, xf: &Xf, at: Pos2) -> bool {
    let w = xf.to_world(at);
    let opened = app.session.begin_window(w);
    if opened {
        app.canvas.window_view = Some((w, xf.to_view(at), xf.dir));
    }
    opened
}

/// The pending selection window's first corner in `xf`'s view plane.
fn window_corner(app: &CadApp, xf: &Xf) -> Option<Vec2> {
    let pw = app.session.pending_window?;
    Some(match app.canvas.window_view {
        Some((w, v, dir)) if w == pw.corner && dir == xf.dir => v,
        _ if xf.dir.persp.is_some() => xf.dir.project(xf.cplane.apply(pw.corner.to3(0.0))),
        _ => xf.plane.apply(pw.corner),
    })
}

/// Close the pending selection window at screen point `at` where the view picks what is drawn
/// ([`Xf::picks_drawn`]) with what is drawn in it (see [`list_window`]; dragged right to left, a
/// crossing window): handed to the command asking for objects, or selected.
fn close_view_window(app: &mut CadApp, xf: &Xf, at: Pos2, shift: bool) -> cadcraft_engine::Result<()> {
    let Some(corner) = window_corner(app, xf) else { return Ok(()) };
    let end = xf.to_view(at);
    let hs = app.canvas.list.as_ref().map(|l| list_window(l, Bounds2::new(corner, end), end.x < corner.x)).unwrap_or_default();
    app.session.pending_window = None;
    app.canvas.window_view = None;
    if app.session.prompting() {
        app.session.input_view_pick(hs)
    } else {
        app.session.idle_window_pick(hs, shift);
        Ok(())
    }
}

/// Degrees the view turns per point dragged when orbiting.
pub(crate) const ORBIT_DEG_PER_PX: f64 = 0.4;

/// What a display list was built from: drawing uid, revision, tessellation band, hatch level of
/// detail band, lineweight display, document, active viewport view and visual style, and view
/// direction (see [`ensure_list`]).
type ListKey = (u64, u64, i32, i32, bool, bool, usize, u64, [u64; 3]);

/// [`ListKey`]'s last field for projection `v`: its direction's bits, mixed with its camera's in
/// perspective and its twist and clipping planes (DVIEW).
fn dir_key(v: &ViewDir) -> [u64; 3] {
    let d = v.dir;
    let mut k = [d.x.to_bits(), d.y.to_bits(), d.z.to_bits()];
    if v.persp.is_some() || v.twist != 0.0 || !v.clip.is_off() {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        if let Some(c) = v.persp {
            [c.eye.x, c.eye.y, c.eye.z, c.focal].map(f64::to_bits).hash(&mut h);
        }
        let (lo, hi) = v.clip.range();
        [v.twist, lo, hi].map(f64::to_bits).hash(&mut h);
        let m = h.finish() | 1;
        k = k.map(|b| b ^ m);
    }
    k
}

/// Whether two list keys are for the same drawing content (they may differ only in the view).
fn same_content(a: &ListKey, b: &ListKey) -> bool {
    (a.0, a.1, a.4, a.5, a.6, a.7) == (b.0, b.1, b.4, b.5, b.6, b.7)
}

#[derive(Default)]
pub struct CanvasState {
    pub list: Option<DisplayList>,
    key: ListKey,
    /// The view direction `list` was built in (see [`view_xfs`]).
    list_dir: ViewDir,
    /// The pending selection window as a 3D view opened it: its world corner, the view-plane
    /// corner and the view direction (an edge-on view's world point has lost its height).
    window_view: Option<(Vec2, Vec2, ViewDir)>,
    /// The area `list` was built for (hatch patterns exist only inside it); a view leaving it
    /// rebuilds the list.
    window: Option<Bounds2>,
    /// A rebuild for a new view running on a worker thread (see [`rebuild`]); `list` is drawn
    /// meanwhile.
    job: Option<rebuild::Job>,
    /// Bumped whenever `list` is rebuilt (identifies it for [`PrimIndex`]).
    list_gen: u64,
    prim_index: PrimIndex,
    /// Set when the app runs on wgpu: entities are drawn by [`crate::gpu`]; otherwise on the CPU.
    pub gpu: Option<crate::gpu::GpuTarget>,
    /// The wgpu device, for background rebuilds to create their buffers (see [`rebuild`]).
    pub gpu_device: Option<egui_wgpu::wgpu::Device>,
    /// The mesh last handed to the GPU (see [`crate::gpu::CanvasCallback::key`]).
    mesh_key: Option<u64>,
    mesh_slot: crate::gpu::MeshSlot,
    mesh_origin: Vec2,
    /// The dragged objects of a MOVE/COPY/ROTATE/SCALE preview, uploaded once per drag (see
    /// [`draw_drag_gpu`]).
    drag_key: Option<u64>,
    drag_slot: crate::gpu::MeshSlot,
    drag_origin: Vec2,
    /// The selection highlight on the GPU (see [`draw_highlight_gpu`]).
    hl_key: Option<u64>,
    hl_slot: crate::gpu::MeshSlot,
    hl_origin: Vec2,
    /// Selected infinite lines (clipped and drawn on the CPU each frame).
    hl_infinite: Vec<usize>,
    /// The selection and hover highlights inside the active MSPACE viewport (see [`VpHighlight`]).
    vp_sel: VpHighlight,
    vp_hover: VpHighlight,
    /// Model space seen through the active MSPACE viewport (see [`VpContent`]).
    vp: VpContent,
    /// Indices of infinite-line primitives in `list` (drawn on the CPU in GPU mode).
    infinite: Vec<usize>,
    /// Milliseconds spent converting the display list to vertex data (last rebuild).
    pub mesh_ms: f64,
    pub rect: Option<Rect>,
    pub xf: Option<Xf>,
    pub hover: Option<Handle>,
    pub snap: Option<SnapHit>,
    /// The height of `snap` when it is on a solid in a 3D view ([`snap3d`]): its point is the
    /// world point's XY.
    pub snap_z: Option<f64>,
    /// Effective cursor point after snaps/ortho/polar.
    pub cursor: Option<Vec2>,
    /// The polar or object snap tracking path(s) the cursor is on.
    pub track: Option<snap::tracking::Track>,
    pan_last: Option<Pos2>,
    /// True when the latest scroll came from a trackpad (or other precise-scrolling device):
    /// scrolling then pans and pinch zooms. A notched mouse wheel clears it so the wheel zooms.
    scroll_pans: bool,
    /// Hot grips (object, grip index): the ones Shift+clicked while idle, then the ones the
    /// running GRIP edit works on (the engine's grip modes take them from there).
    pub hot_grips: Vec<(Handle, usize)>,
    /// Grip dimensional input: the value fields of the grip being stretched.
    pub grip_dyn: crate::grip_dyn::GripDyn,
    /// A GRIP edit was running last frame (its grips go cold when it ends).
    grip_editing: bool,
    /// The grip whose menu (Stretch, Add Vertex, Lengthen…) a right-click opened.
    grip_menu: Option<(Handle, usize)>,
    /// Dynamic block grips of the selection (cached), the one being dragged, and the list grip
    /// whose choices are open (and whether it opened this frame).
    pub(crate) dyn_grips: crate::dyngrips::Cache,
    pub(crate) dyn_hot: Option<(Handle, i32, cadcraft_engine::doc::dynblock::GripPart)>,
    pub(crate) dyn_menu: Option<(Handle, cadcraft_engine::doc::dynblock::DynGrip)>,
    pub(crate) dyn_menu_fresh: bool,
    /// Zoom to extents once the canvas size is known (after opening a drawing).
    pub zoom_pending: bool,
    pub build_ms: f64,
    pub draw_ms: f64,
    hover_at: Option<Pos2>,
    /// Constraint glyphs for the parametric overlay (cached per drawing revision).
    pub param: crate::parametric::Cache,
    /// A selection window opened by a press-and-drag, to close where the button is let go.
    drag_window: bool,
    /// The right-click that opened the shortcut menu held Shift or Ctrl: it is the object snap
    /// menu ([`crate::context_menu`]).
    pub snap_menu: bool,
    /// Tab was pressed over a snap marker: show the next candidate ([`SnapCycle`]).
    pub snap_tab: bool,
    pub snap_cycle: Option<SnapCycle>,
    /// The lists of the model tiles that look another way than the current one ([`tiles`]).
    tile_lists: Vec<tiles::TileList>,
    /// A right-button press on the canvas: where, and whether it has become a Rhino-style drag
    /// ([`tiles::right_drag`]).
    rnav: Option<(Pos2, bool)>,
    /// The model look `list` was last built for ([`tiles::trade`]).
    last_look: Option<cadcraft_engine::TileLook>,
    /// Rhino's gumball on the selection in a 3D view ([`gumball`]).
    pub(crate) gumball: gumball::Gumball,
    /// The edge-on construction plane warning is showing (it is given once per hover).
    edge_warned: bool,
    /// While a grip edit in a 3D view (or plan with a tilted CPlane) drags a grip off the
    /// construction plane or of a tilted object: the plane (world point, normal) the cursor moves
    /// it in ([`grip_plane`], [`drag_plane`], [`grip_drag_plane`], [`point_at`]).
    pub(crate) grip_plane: Option<(cadcraft_geom::Vec3, cadcraft_geom::Vec3)>,
    /// While a grip is dragged in a plane of its own: the height above the CPlane of the point
    /// under the cursor there ([`grip_height`]), sent with an unsnapped point.
    pub(crate) grip_z: Option<f64>,
    /// The drawing the engine's object snaps run on in a 3D view ([`snap3d::ground_doc`]).
    pub(crate) ground: snap3d::Ground,
}

impl CanvasState {
    /// The active MSPACE viewport's contents when they are drawn apart from the layout's list
    /// (see [`VpContent`]): where they are on paper at the viewport's current view, and how many
    /// times they have been built.
    pub fn viewport_content(&self) -> Option<(Bounds2, u64)> {
        self.vp.key.and(self.vp.paper_bounds()).map(|b| (b, self.vp.builds))
    }

    /// Whether a rebuild for the current view is still running in the background (the canvas
    /// shows the previous list until it lands).
    pub fn building(&self) -> bool {
        self.job.is_some()
    }

    /// Put a finished background build in place, with its GPU vertex data when it has some.
    fn install(&mut self, key: ListKey, window: Option<Bounds2>, built: rebuild::Built) {
        self.list_gen = self.list_gen.wrapping_add(1);
        if let Some(m) = built.mesh {
            if let Ok(mut slot) = self.mesh_slot.lock() {
                *slot = Some(m.data);
            }
            self.mesh_key = Some(list_mesh_key(&key, self.list_gen, m.origin, m.bg, m.lwdisplay));
            self.mesh_origin = m.origin;
            self.mesh_ms = m.ms;
            self.infinite = m.infinite;
        }
        self.list = Some(built.list);
        self.list_dir = built.view_dir;
        self.key = key;
        self.window = window;
        self.build_ms = built.build_ms;
    }
}

/// Tab cycling through the candidate snaps under the cursor: where it started and which
/// candidate shows. Moving the cursor out of the aperture ends it.
#[derive(Clone, Copy, Debug)]
pub struct SnapCycle {
    anchor: Vec2,
    index: usize,
}

/// Primitives of `list` sorted by entity handle, built on the first highlight after each list
/// rebuild. Highlighting the hovered entity used to scan every primitive on every frame: ~2.5 ms
/// per frame on a 270k-entity drawing, whether or not anything changed.
#[derive(Default)]
struct PrimIndex {
    list_gen: Option<u64>,
    by_handle: Vec<(Handle, usize)>,
}

impl PrimIndex {
    /// Indices of the primitives drawn for `handles` (in list order).
    fn prims(&mut self, list_gen: u64, list: &DisplayList, handles: &[Handle]) -> Vec<usize> {
        if handles.is_empty() {
            return Vec::new();
        }
        if self.list_gen != Some(list_gen) {
            self.by_handle = list.prims.iter().enumerate().map(|(i, p)| (p.handle, i)).collect();
            self.by_handle.sort_unstable();
            self.list_gen = Some(list_gen);
        }
        let mut out = Vec::new();
        for h in handles {
            let from = self.by_handle.partition_point(|(k, _)| k < h);
            out.extend(self.by_handle.get(from..).unwrap_or(&[]).iter().take_while(|(k, _)| k == h).map(|(_, i)| *i));
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Indices of the primitives in `list` drawn for `handles` (a scan: the reference [`PrimIndex`]
/// is tested against).
#[cfg(test)]
fn prims_of(list: &DisplayList, handles: &[Handle]) -> Vec<usize> {
    if handles.is_empty() {
        return Vec::new();
    }
    let set: std::collections::HashSet<Handle> = handles.iter().copied().collect();
    list.prims.iter().enumerate().filter(|(_, pr)| set.contains(&pr.handle)).map(|(i, _)| i).collect()
}

/// Build options for previews at `scale` pixels per drawing unit (opaque).
fn preview_options(scale: f64, view_dir: ViewDir) -> cadcraft_render::Options {
    cadcraft_render::Options { tolerance: 0.5 / scale, min_dash: 2.0 / scale, view_dir, ..Default::default() }
}

/// The display list of preview entities the session builds with the current properties
/// ([`cadcraft_engine::Session::preview`], the extras of [`cadcraft_engine::Session::drag`]): the
/// command's rubber band and rebuilt copies of existing objects. They carry CLAYER's properties,
/// not the objects', so they are drawn opaque (the GPU drag of the objects themselves keeps their
/// transparency, see [`draw_drag_gpu`]).
fn rebuilt_preview<'a>(
    doc: &cadcraft_doc::Drawing,
    ents: impl IntoIterator<Item = &'a cadcraft_doc::Entity>,
    scale: f64,
    view: ViewDir,
) -> DisplayList {
    cadcraft_render::build_entities(doc, ents, &preview_options(scale, view))
}

fn color32(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

/// A primitive's colour on `bg`, with its transparency.
fn prim_color32(prim: &cadcraft_render::DPrim, bg: Rgb) -> Color32 {
    let c = prim.display_rgb(bg);
    Color32::from_rgba_unmultiplied(c.0, c.1, c.2, prim.alpha())
}

/// Rebuild the display list when needed. `xf`: the current view; `views`: every area on screen
/// that shows the list (the current view, other model tiles) with its drawing units per pixel.
///
/// A rebuild only the view asks for (a new zoom band, a pan out of the built area, a new view
/// direction) runs in the background while the current list is drawn (see [`rebuild`]); a
/// changed drawing, or no list yet, is built at once.
fn ensure_list(app: &mut CadApp, xf: &Xf, views: &[(Bounds2, f64)], ctx: &egui::Context) {
    let px = 1.0 / xf.scale;
    let Ok(st) = app.session.state() else { return };
    // Rebuild when the drawing changes, the zoom moves by more than 2x (tessellation band) or a
    // view leaves the area the list was built for.
    let band = |px: f64| px.max(1e-300).log2().floor() as i32;
    // Hatch level of detail follows the finest view showing the list.
    let lod_band = views.iter().map(|v| band(v.1)).fold(band(px), i32::min);
    // The active MSPACE viewport's contents are drawn apart (see `VpContent`), so panning and
    // zooming inside it leave this list alone. Without a stencil buffer a clipped viewport's
    // contents stay in the list, which then follows its view: realtime navigation changes it
    // without a new revision.
    let split = split_viewport(st, stencil_clip(app)).map(|(h, ..)| h);
    let vp_view = list_style(st, split);
    // The view direction is part of the view: orbiting builds in the background (see `view_xfs`).
    // A perspective view's camera too: panning and zooming it build again.
    let view_dir = st.screen_dir();
    let (lwdisplay, transparency_display) = (app.session.settings.lwdisplay, app.session.settings.transparency_display);
    let key: ListKey = (
        st.uid,
        st.revision,
        band(px),
        lod_band,
        lwdisplay,
        transparency_display,
        std::sync::Arc::as_ptr(&st.doc) as usize,
        vp_view,
        dir_key(&view_dir),
    );
    // A background build that finished: use it unless the drawing has changed since it started
    // (or the list was dropped to force a rebuild).
    if let Some(job) = &app.canvas.job {
        match job.poll() {
            rebuild::Poll::Pending => {}
            rebuild::Poll::Done(built) => {
                let (job_key, window) = (job.key, job.window);
                app.canvas.job = None;
                if app.canvas.list.is_some() && same_content(&job_key, &key) {
                    app.canvas.install(job_key, window, *built);
                }
            }
            rebuild::Poll::Lost => {
                log::warn!("canvas: a background rebuild failed; rebuilding on the UI thread");
                app.canvas.job = None;
                app.canvas.list = None;
            }
        }
    }
    let visible = views.iter().fold(Bounds2::EMPTY, |acc, v| acc.union(&v.0));
    let inside = app.canvas.window.is_none_or(|w| visible.is_empty() || w.contains_box(&visible));
    if app.canvas.list.is_some() && app.canvas.key == key && inside {
        return;
    }
    // Only the view moved: keep drawing the current list and build the new one in the background.
    let view_only = app.canvas.list.is_some() && same_content(&app.canvas.key, &key);
    // While the view turns, a build for an earlier direction runs on and lands first, so an orbit
    // follows as fast as the builds finish instead of waiting for it to stop.
    let turning = |j: &rebuild::Job| {
        let mut k = j.key;
        k.8 = key.8;
        j.key.8 != key.8 && k == key
    };
    if view_only && app.canvas.job.as_ref().is_some_and(|j| j.serves(&key, &visible) || turning(j)) {
        return;
    }
    // A build for another view, or for a drawing that has since changed, is no longer wanted:
    // dropping it cancels it.
    app.canvas.job = None;
    let t0 = crate::now_ms();
    let opts = cadcraft_render::Options {
        tolerance: 2f64.powi(band(px)) * 0.5,
        // Two pixels at the band's finest zoom: the list serves every zoom in the band, so its
        // level of detail must hold at the finest one.
        min_dash: 2f64.powi(lod_band) * 2.0,
        text: true,
        fill: true,
        lineweights: lwdisplay,
        view_height: st.paper_view().height,
        plot_style_table: None,
        view: build_window(views),
        transparency: app.session.settings.transparency_display,
        skip_viewport: split,
        view_dir,
        ..cadcraft_render::Options::look(st.shaded, st.shaded_edges, st.hide)
    };
    if view_only {
        let t = Tokens::get();
        let mesh = app.canvas.gpu.is_some().then(|| rebuild::MeshRequest {
            view_center: xf.center,
            view_height: f64::from(xf.rect.height()) / xf.scale.max(1e-300),
            model_bg: Rgb(t.canvas.r(), t.canvas.g(), t.canvas.b()),
            paper_bg: Rgb(t.paper.r(), t.paper.g(), t.paper.b()),
            lwdisplay,
            device: app.canvas.gpu_device.clone(),
        });
        let window = opts.view;
        let req = rebuild::Request { doc: st.doc.clone(), space: st.space.clone(), opts: opts.clone(), mesh };
        if let Some(job) = rebuild::spawn(req, key, window, ctx) {
            app.canvas.job = Some(job);
            return;
        }
    }
    let space = st.space.clone();
    app.canvas.list = Some(cadcraft_render::build(&st.doc, &space, &opts));
    app.canvas.list_dir = view_dir;
    app.canvas.key = key;
    app.canvas.window = opts.view;
    app.canvas.list_gen = app.canvas.list_gen.wrapping_add(1);
    app.canvas.build_ms = crate::now_ms() - t0;
}

/// What the canvas's list depends on besides the drawing and the view direction ([`ListKey`]'s
/// eighth field): the MSPACE viewport drawn apart (`split`) or the active viewport's view, and
/// the visual style.
fn list_style(st: &cadcraft_engine::DocState, split: Option<Handle>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut s = std::collections::hash_map::DefaultHasher::new();
    split.hash(&mut s);
    if split.is_none()
        && let Some((h, vp)) = st.active_viewport()
    {
        (h, vp.view_center.x.to_bits(), vp.view_center.y.to_bits(), vp.view_height.to_bits()).hash(&mut s);
    }
    (st.shaded, st.shaded_edges, st.hide).hash(&mut s);
    s.finish()
}

/// The active MSPACE viewport when its contents are drawn apart from the layout's list (see
/// [`VpContent`]), with its clip outline when it is clipped. A clipped viewport is clipped with
/// the GPU's stencil buffer (`stencil`); without one its contents stay in the list.
fn split_viewport(
    st: &cadcraft_engine::DocState,
    stencil: bool,
) -> Option<(Handle, cadcraft_doc::Viewport, Option<cadcraft_render::clip::PolyClip>)> {
    let (h, vp) = st.active_viewport()?;
    let clip = cadcraft_render::viewport_clip(&st.doc, h, &vp);
    (clip.is_none() || stencil).then_some((h, vp, clip))
}

/// Whether the canvas can clip a viewport to any outline (a GPU with a stencil buffer).
fn stencil_clip(app: &CadApp) -> bool {
    app.canvas.gpu.is_some_and(|g| g.stencil.is_some())
}

/// How long the view inside the active viewport stays still before a zoomed [`VpContent`] is
/// rebuilt at the exact view (dash lengths and curve detail follow the viewport's scale).
const VP_SETTLE_MS: f64 = 150.0;

/// Model space seen through the active MSPACE viewport, drawn apart from the layout's list. It is
/// built for an area around the viewport at one view and drawn clipped to the viewport, moved
/// and scaled to the current view, so panning and zooming inside the viewport redraw without
/// rebuilding anything. It is rebuilt when the drawing changes, when the zoom has moved 2x from
/// the view it was built for, when the view leaves the area, and once a zoom comes to rest.
#[derive(Default)]
struct VpContent {
    list: DisplayList,
    /// What the list was built from: drawing, revision, document, viewport, tessellation band,
    /// lineweight and transparency display, and visual style.
    key: Option<(u64, u64, usize, Handle, i32, bool, bool, bool, bool)>,
    /// The viewport's view (centre, height) the list was built for, and the paper area it covers.
    view: (Vec2, f64),
    area: Bounds2,
    /// The view on the previous frame and when it last changed (ms).
    last_view: (Vec2, f64),
    moved_at: f64,
    /// Scale and offset drawing the list at the viewport's current view (see [`vp_motion`]).
    motion: Option<(f64, Vec2)>,
    /// Bumped on each rebuild.
    builds: u64,
    mesh_key: Option<u64>,
    mesh_slot: crate::gpu::MeshSlot,
    mesh_origin: Vec2,
    /// A clipped viewport's outline on the GPU (see [`draw_vp_mask`]).
    mask_key: Option<u64>,
    mask_slot: crate::gpu::MeshSlot,
    mask_origin: Vec2,
}

impl VpContent {
    /// Where the contents are on paper at the viewport's current view.
    fn paper_bounds(&self) -> Option<Bounds2> {
        let (k, b) = self.motion?;
        (!self.list.bounds.is_empty()).then(|| Bounds2::new(self.list.bounds.min * k + b, self.list.bounds.max * k + b))
    }
}

/// How a list built for view `built` (centre, height) of viewport `vp` maps to its current
/// view, as `p = k·p0 + b` on paper. A model point m is at p0 = c + s0 (m − c0) in the list and
/// belongs at p = c + s (m − vc), with c the viewport's centre and s, s0 paper units per model
/// unit, so k = s / s0 and b = c (1 − k) + s (c0 − vc).
fn vp_motion(vp: &cadcraft_doc::Viewport, built: (Vec2, f64)) -> Option<(f64, Vec2)> {
    let (s, k) = (vp.height / vp.view_height, built.1 / vp.view_height);
    let b = vp.center.xy() * (1.0 - k) + (built.0 - vp.view_center) * s;
    (k.is_finite() && k > 0.0 && b.is_finite()).then_some((k, b))
}

/// How to draw the active viewport's [`VpContent`] this frame.
struct VpFrame {
    /// Draws the list at the viewport's current view.
    xf: Xf,
    /// The viewport's rectangle (a clipped one's outline bounds) on screen.
    on_screen: Rect,
    /// A clipped viewport's outline on paper, masked with the stencil buffer.
    outline: Option<Vec<Vec2>>,
}

/// Build or reuse the active viewport's [`VpContent`] for this frame.
fn ensure_vp_content(app: &mut CadApp, xf_paper: &Xf, ctx: &egui::Context) -> Option<VpFrame> {
    let (lwdisplay, transparency) = (app.session.settings.lwdisplay, app.session.settings.transparency_display);
    let stencil = stencil_clip(app);
    let c = &mut app.canvas.vp;
    let Some((st, (h, vp, clip))) = app.session.state().ok().and_then(|st| Some((st, split_viewport(st, stencil)?))) else {
        // Not working in a viewport drawn apart: let the geometry go.
        if c.key.is_some() {
            *c = VpContent::default();
        }
        return None;
    };
    let half = Vec2::new(vp.width / 2.0, vp.height / 2.0);
    let rect = match &clip {
        Some(p) => p.bounds(),
        None => Bounds2::new(vp.center.xy() - half, vp.center.xy() + half),
    };
    let px = 1.0 / xf_paper.scale.max(1e-300);
    let band = px.max(1e-300).log2().floor() as i32;
    let key =
        (st.uid, st.revision, std::sync::Arc::as_ptr(&st.doc) as usize, h, band, lwdisplay, transparency, st.shaded, st.shaded_edges || st.hide);
    let now = crate::now_ms();
    let view = (vp.view_center, vp.view_height);
    if view != c.last_view {
        c.last_view = view;
        c.moved_at = now;
    }
    let rebuild = match vp_motion(&vp, c.view).filter(|_| c.key == Some(key)) {
        Some((k, b)) => {
            // The viewport in the list's own coordinates.
            let visible = Bounds2::new((rect.min - b) / k, (rect.max - b) / k);
            let zoomed = (k - 1.0).abs() > 1e-9;
            let still = now - c.moved_at;
            if zoomed && still < VP_SETTLE_MS {
                ctx.request_repaint_after(std::time::Duration::from_millis((VP_SETTLE_MS - still).max(1.0) as u64));
            }
            !(0.5..2.0).contains(&k) || !c.area.contains_box(&visible) || (zoomed && still >= VP_SETTLE_MS)
        }
        None => true,
    };
    if rebuild {
        let t0 = crate::now_ms();
        // The viewport with half its larger side of margin all round: panning by up to that much
        // only moves the geometry.
        let area = rect.expand(rect.width().max(rect.height()) * 0.5);
        let opts = cadcraft_render::Options {
            tolerance: 2f64.powi(band) * 0.5,
            min_dash: 2f64.powi(band) * 2.0,
            text: true,
            fill: true,
            lineweights: lwdisplay,
            view_height: st.paper_view().height,
            plot_style_table: None,
            view: Some(area),
            skip_viewport: None,
            transparency,
            view_dir: cadcraft_geom::ViewDir::PLAN,
            ..cadcraft_render::Options::look(st.shaded, st.shaded_edges, st.hide)
        };
        c.list = cadcraft_render::viewport_content(&st.doc, &st.space, h, area, &opts);
        c.key = Some(key);
        c.view = view;
        c.area = area;
        c.builds = c.builds.wrapping_add(1);
        app.canvas.build_ms = crate::now_ms() - t0;
    }
    let c = &mut app.canvas.vp;
    let (k, b) = vp_motion(&vp, c.view)?;
    c.motion = Some((k, b));
    // Paper p = k·p0 + b, so the list is drawn by the paper transform scaled by k about b.
    let xf = Xf::new(xf_paper.rect, (xf_paper.center - b) / k, xf_paper.scale * k);
    let on_screen = Rect::from_two_pos(xf_paper.to_screen(Vec2::new(rect.min.x, rect.max.y)), xf_paper.to_screen(Vec2::new(rect.max.x, rect.min.y)));
    Some(VpFrame { xf, on_screen, outline: clip.map(|p| p.polygon().to_vec()) })
}

/// Draw the active viewport's contents (see [`VpContent`]) as [`ensure_vp_content`] set out;
/// `p` clips them to the viewport's rectangle, and a clipped viewport's outline is masked with
/// the stencil buffer first.
fn draw_vp_content(c: &mut VpContent, gpu: bool, p: &egui::Painter, f: &VpFrame, xf_paper: &Xf, bg: Rgb, lwdisplay: bool) {
    let xf = &f.xf;
    images::paint(p, xf, &c.list);
    if !gpu {
        draw_list(p, xf, &c.list, bg, lwdisplay);
        return;
    }
    let mut stencil = crate::gpu::Stencil::Off;
    if let Some(outline) = &f.outline {
        draw_vp_mask(c, p, outline, xf_paper);
        stencil = crate::gpu::Stencil::Inside;
    }
    let origin = crate::gpu::choose_origin(&c.list.bounds, xf.center, f64::from(xf.rect.height()) / xf.scale.max(1e-300));
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (c.builds, origin.x.to_bits(), origin.y.to_bits(), bg.0, bg.1, bg.2, lwdisplay).hash(&mut h);
        h.finish()
    };
    if c.mesh_key != Some(key) {
        let mesh = crate::gpu::build_mesh(&c.list, origin, bg, lwdisplay);
        if let Ok(mut slot) = c.mesh_slot.lock() {
            *slot = Some(mesh);
        }
        c.mesh_key = Some(key);
        c.mesh_origin = origin;
    }
    let center = xf.rect.center();
    p.add(egui_wgpu::Callback::new_paint_callback(
        xf.rect,
        crate::gpu::CanvasCallback {
            key,
            slot: c.mesh_slot.clone(),
            offset: [(c.mesh_origin.x - xf.center.x) as f32, (c.mesh_origin.y - xf.center.y) as f32],
            scale: xf.scale as f32,
            center: [center.x, center.y],
            layer: crate::gpu::LAYER_VIEWPORT,
            xform: crate::gpu::IDENTITY_XFORM,
            dash: 0.0,
            stencil,
        },
    ));
}

/// Mask a clipped viewport's `outline` (paper units) in the stencil buffer: a triangle fan from
/// its first point, each triangle flipping the stencil bit, leaves the bit set inside the outline
/// (even-odd, concave or not), with no triangulation.
fn draw_vp_mask(c: &mut VpContent, p: &egui::Painter, outline: &[Vec2], xf_paper: &Xf) {
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        outline.iter().for_each(|q| (q.x.to_bits(), q.y.to_bits()).hash(&mut h));
        h.finish()
    };
    if c.mask_key != Some(key) {
        let mut fan = DisplayList::default();
        if let Some((&first, rest)) = outline.split_first() {
            for w in rest.windows(2) {
                if let [a, b] = w {
                    fan.tris.extend_from_slice(&[first, *a, *b]);
                }
            }
        }
        fan.bounds = Bounds2::from_points(outline.iter().copied());
        let len = u32::try_from(fan.tris.len()).unwrap_or(0);
        fan.prims.push(cadcraft_render::DPrim {
            handle: Handle(0),
            color: Rgb(0, 0, 0),
            aci7: false,
            lw: 0.0,
            transparency: 0,
            kind: Kind::Tris,
            start: 0,
            len,
        });
        let origin = if fan.bounds.is_empty() { Vec2::ZERO } else { fan.bounds.center() };
        if let Ok(mut slot) = c.mask_slot.lock() {
            *slot = Some(crate::gpu::build_mesh(&fan, origin, Rgb(0, 0, 0), false));
        }
        c.mask_key = Some(key);
        c.mask_origin = origin;
    }
    let center = xf_paper.rect.center();
    p.add(egui_wgpu::Callback::new_paint_callback(
        xf_paper.rect,
        crate::gpu::CanvasCallback {
            key,
            slot: c.mask_slot.clone(),
            offset: [(c.mask_origin.x - xf_paper.center.x) as f32, (c.mask_origin.y - xf_paper.center.y) as f32],
            scale: xf_paper.scale as f32,
            center: [center.x, center.y],
            layer: crate::gpu::LAYER_MASK,
            xform: crate::gpu::IDENTITY_XFORM,
            dash: 0.0,
            stencil: crate::gpu::Stencil::Mask,
        },
    ));
}

/// Tessellation tolerance for the highlights inside the active viewport: `scale` (pixels per
/// drawing unit) snapped to its 2x zoom band, as in [`ensure_list`], so zooming and panning
/// inside the band doesn't rebuild the geometry.
fn viewport_highlight_tolerance(scale: f64) -> f64 {
    2f64.powi((1.0 / scale.max(1e-300)).log2().floor() as i32) * 0.5
}

/// The selected or the hovered objects tessellated in model units for the active MSPACE
/// viewport, whose contents can't be looked up by handle in the layout's lists. The selection and
/// the hover are kept apart, so moving the cursor over objects doesn't tessellate the selection
/// again; each is rebuilt only when the drawing, the objects or the zoom band change.
#[derive(Default)]
struct VpHighlight {
    key: Option<u64>,
    list: DisplayList,
    /// Bumped on each rebuild (identifies `list` for [`draw_highlight_gpu`]).
    list_gen: u64,
}

impl VpHighlight {
    fn update(&mut self, st: &cadcraft_engine::DocState, handles: &[Handle], scale: f64) {
        let tolerance = viewport_highlight_tolerance(scale);
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (st.uid, st.revision, std::sync::Arc::as_ptr(&st.doc) as usize, tolerance.to_bits(), handles).hash(&mut h);
            h.finish()
        };
        if self.key != Some(key) {
            let ents = handles.iter().filter_map(|h| st.doc.model.get(*h)).map(|e| e.as_ref());
            self.list = cadcraft_render::build_entities(&st.doc, ents, &cadcraft_render::Options { tolerance, ..Default::default() });
            self.key = Some(key);
            self.list_gen = self.list_gen.wrapping_add(1);
        }
    }
}

/// The area a display list is built for: each view with half its larger side of margin all
/// round (panning by up to that much doesn't rebuild), joined. `None` (everything) when a view
/// isn't usable.
fn build_window(views: &[(Bounds2, f64)]) -> Option<Bounds2> {
    let mut w = Bounds2::EMPTY;
    for (v, _) in views {
        let side = v.width().max(v.height());
        if v.is_empty() || !side.is_finite() {
            return None;
        }
        w = w.union(&v.expand(side * 0.5));
    }
    (!w.is_empty()).then_some(w)
}

/// Grid spacing adapted to the zoom: multiply the unit until lines are at least `min_px` apart.
fn adaptive(unit: f64, scale: f64, min_px: f64) -> f64 {
    let mut u = unit.max(1e-12);
    let mut guard = 0;
    while u * scale < min_px && guard < 40 {
        u *= 5.0;
        guard += 1;
    }
    u
}

fn draw_grid(app: &CadApp, p: &egui::Painter, xf: &Xf) {
    // ponytail: in a UCS moved or turned in plan the grid follows the UCS but is never isometric
    // (SNAPSTYL 1 draws the rectangular one there); turn `iso::grid_lines` with it if asked.
    if !xf.flat() {
        draw_cplane_grid(app, p, xf);
        return;
    }
    let t = Tokens::get();
    let s = app.session.drafting_settings();
    let wb = xf.view_bounds();
    if s.gridmode && s.isodraft {
        draw_iso_grid(app, p, xf);
    } else if s.gridmode {
        let major_n = f64::from(s.gridmajor.max(1));
        let unit = if s.gridunit.x == 0.0 { s.snapunit.x } else { s.gridunit.x };
        let minor = adaptive(unit, xf.scale, 8.0);
        let major = minor * major_n;
        let mut lines = Vec::new();
        let x0 = (wb.min.x / minor).floor() as i64;
        let x1 = (wb.max.x / minor).ceil() as i64;
        let y0 = (wb.min.y / minor).floor() as i64;
        let y1 = (wb.max.y / minor).ceil() as i64;
        if (x1 - x0) < 2000 && (y1 - y0) < 2000 {
            for i in x0..=x1 {
                let x = i as f64 * minor;
                let is_major = (x / major).round() * major - x == 0.0 || ((x / major) - (x / major).round()).abs() < 1e-6;
                let sx = xf.to_screen(Vec2::new(x, 0.0)).x;
                lines.push(Shape::line_segment(
                    [pos2(sx, xf.rect.top()), pos2(sx, xf.rect.bottom())],
                    Stroke::new(1.0, if is_major { t.grid_major } else { t.grid_minor }),
                ));
            }
            for j in y0..=y1 {
                let y = j as f64 * minor;
                let is_major = ((y / major) - (y / major).round()).abs() < 1e-6;
                let sy = xf.to_screen(Vec2::new(0.0, y)).y;
                lines.push(Shape::line_segment(
                    [pos2(xf.rect.left(), sy), pos2(xf.rect.right(), sy)],
                    Stroke::new(1.0, if is_major { t.grid_major } else { t.grid_minor }),
                ));
            }
        }
        p.extend(lines);
    }
    // Axes through the origin.
    let o = xf.to_screen(Vec2::ZERO);
    if xf.rect.y_range().contains(o.y) {
        p.line_segment([pos2(xf.rect.left(), o.y), pos2(xf.rect.right(), o.y)], Stroke::new(1.0, t.axis_x));
    }
    if xf.rect.x_range().contains(o.x) {
        p.line_segment([pos2(o.x, xf.rect.top()), pos2(o.x, xf.rect.bottom())], Stroke::new(1.0, t.axis_y));
    }
}

/// The construction plane's grid in a 3D view, as Rhino draws it: minor and major lines (GRIDMODE,
/// GRIDUNIT, GRIDMAJOR) about the point of the plane at the view's centre, its x axis red and its
/// y axis green. Nothing where the view sees the plane edge-on.
/// ponytail: the grid reaches a view's size around the centre point; a plane receding to the
/// horizon in a steep view ends there (Rhino's grid has a fixed extent too).
fn draw_cplane_grid(app: &CadApp, p: &egui::Painter, xf: &Xf) {
    if xf.edge_on() {
        return;
    }
    let t = Tokens::get();
    let s = app.session.drafting_settings();
    let unit = if s.gridunit.x == 0.0 { s.snapunit.x } else { s.gridunit.x };
    // Spaced for the more foreshortened axis.
    let short = xf.plane.apply_vec(Vec2::X).len().min(xf.plane.apply_vec(Vec2::Y).len()).max(0.2);
    let minor = adaptive(unit, xf.scale * short, 8.0);
    let major = minor * f64::from(s.gridmajor.max(1));
    let c = xf.to_world(xf.rect.center());
    let reach = f64::from(xf.rect.width().max(xf.rect.height())) / xf.scale / short;
    let span = |v: f64| (((v - reach) / minor).floor(), ((v + reach) / minor).ceil());
    let ((i0, i1), (j0, j1)) = (span(c.x), span(c.y));
    if !(i1 - i0 < 2000.0 && j1 - j0 < 2000.0 && i0.is_finite() && j0.is_finite()) {
        return;
    }
    let (x0, x1, y0, y1) = (i0 * minor, i1 * minor, j0 * minor, j1 * minor);
    // In perspective only what is in front of the eye.
    let seg = |a: Vec2, b: Vec2, col: Color32| xf.segment_to_screen(a, b).map_or(Shape::Noop, |s| Shape::line_segment(s, Stroke::new(1.0, col)));
    let kind = |v: f64| if ((v / major) - (v / major).round()).abs() < 1e-6 { t.grid_major } else { t.grid_minor };
    let mut lines = Vec::new();
    if s.gridmode {
        // Counted steps (at most 2000 each way, checked above): far from the origin, adding 1.0
        // to a huge float no longer changes it, so a float loop would never end.
        let (ni, nj) = ((i1 - i0).max(0.0) as usize, (j1 - j0).max(0.0) as usize);
        for k in 0..=ni {
            let x = (i0 + k as f64) * minor;
            lines.push(seg(Vec2::new(x, y0), Vec2::new(x, y1), kind(x)));
        }
        for k in 0..=nj {
            let y = (j0 + k as f64) * minor;
            lines.push(seg(Vec2::new(x0, y), Vec2::new(x1, y), kind(y)));
        }
    }
    if (y0..=y1).contains(&0.0) {
        lines.push(seg(Vec2::new(x0, 0.0), Vec2::new(x1, 0.0), t.axis_x));
    }
    if (x0..=x1).contains(&0.0) {
        lines.push(seg(Vec2::new(0.0, y0), Vec2::new(0.0, y1), t.axis_y));
    }
    p.extend(lines);
}

/// The isometric grid (SNAPSTYL 1): lines at 30°, 90° and 150° through the snap lattice, GRIDUNIT
/// Y apart along the axes, coarsened while they would crowd closer than 8 pixels.
fn draw_iso_grid(app: &CadApp, p: &egui::Painter, xf: &Xf) {
    use cadcraft_engine::snap::iso;
    let t = Tokens::get();
    let s = app.session.drafting_settings();
    let mut d = s.gridunit.y.max(1e-12);
    for _ in 0..40 {
        if d * 0.866 * xf.scale >= 8.0 {
            break;
        }
        d *= 5.0;
    }
    let (origin, angle) = app.session.doc().map(cadcraft_engine::snap::grid_frame).unwrap_or((Vec2::ZERO, 0.0));
    let lines = iso::grid_lines(xf.view_bounds(), d, origin, angle, s.gridmajor);
    p.extend(
        lines.into_iter().map(|l| {
            Shape::line_segment([xf.to_screen(l.a), xf.to_screen(l.b)], Stroke::new(1.0, if l.major { t.grid_major } else { t.grid_minor }))
        }),
    );
}

fn clip_infinite(xf: &Xf, base: Vec2, dir: Vec2, ray: bool) -> Option<[Pos2; 2]> {
    let wb = xf.view_bounds();
    let far = (wb.width() + wb.height()) * 2.0 + base.dist(wb.center());
    let a = if ray { base } else { base - dir * far };
    let b = base + dir * far;
    Some([xf.view_to_screen(a), xf.view_to_screen(b)])
}

/// Draw a display list.
pub(crate) fn draw_list(p: &egui::Painter, xf: &Xf, list: &DisplayList, bg: Rgb, lwdisplay: bool) {
    let vis = xf.view_bounds();
    let mut shapes: Vec<Shape> = Vec::with_capacity(list.prims.len());
    let px = 1.0 / xf.scale;
    for prim in &list.prims {
        let col = prim_color32(prim, bg);
        let pts = list.points(prim);
        match prim.kind {
            Kind::Polyline => {
                let bb = Bounds2::from_points(pts.iter().copied());
                if !bb.intersects(&vis) {
                    continue;
                }
                let w = if lwdisplay && prim.lw > 0.0 { (prim.lw * 3.78).clamp(1.0, 12.0) } else { 1.0 };
                // Skip sub-pixel objects except as a dot.
                if bb.width() < px && bb.height() < px {
                    shapes.push(Shape::rect_filled(Rect::from_center_size(xf.view_to_screen(bb.center()), vec2(1.0, 1.0)), 0.0, col));
                    continue;
                }
                let screen: Vec<Pos2> = pts.iter().map(|q| xf.view_to_screen(*q)).collect();
                if screen.len() == 2 {
                    if let [a, b] = screen.as_slice() {
                        shapes.push(Shape::line_segment([*a, *b], Stroke::new(w, col)));
                    }
                } else {
                    shapes.push(Shape::line(screen, Stroke::new(w, col)));
                }
            }
            Kind::Tris | Kind::Mask => {
                // A wipeout's mask hides what was drawn before it with the background colour.
                let col = if prim.kind == Kind::Mask { color32(bg) } else { col };
                let mut mesh = egui::Mesh::default();
                for tri in pts.chunks(3) {
                    if let [a, b, c] = tri {
                        let i = mesh.vertices.len() as u32;
                        for q in [a, b, c] {
                            mesh.colored_vertex(xf.view_to_screen(*q), col);
                        }
                        mesh.add_triangle(i, i + 1, i + 2);
                    }
                }
                if !mesh.vertices.is_empty() {
                    shapes.push(Shape::mesh(mesh));
                }
            }
            Kind::Point => {
                if let Some(q) = pts.first()
                    && vis.contains(*q)
                {
                    shapes.push(Shape::rect_filled(Rect::from_center_size(xf.view_to_screen(*q), vec2(1.5, 1.5)), 0.0, col));
                }
            }
            Kind::Infinite { ray } => {
                if let (Some(b), Some(d)) = (pts.first(), pts.get(1))
                    && let Some(seg) = clip_infinite(xf, *b, *d, ray)
                {
                    shapes.push(Shape::line_segment(seg, Stroke::new(1.0, col)));
                }
            }
        }
    }
    // The solids' edges (Shaded with Edges, Hidden) where no face hides them: with no depth
    // buffer here (the tiles other than the current one, or no GPU), tested against the faces'
    // depths at this view's resolution, as the raster does.
    let w = |x: f32| x.round().clamp(0.0, 16384.0) as u32;
    let view = cadcraft_render::raster::View { center: xf.center, scale: xf.scale, width: w(xf.rect.width()), height: w(xf.rect.height()) };
    for (i, run) in cadcraft_render::raster::visible_edges(list, &view) {
        let Some(e) = list.edges.get(i) else { continue };
        let screen: Vec<Pos2> = run.iter().map(|q| xf.view_to_screen(*q)).collect();
        shapes.push(Shape::line(screen, Stroke::new(1.0, prim_color32(e, bg))));
    }
    p.extend(shapes);
}

/// Draw the selection highlight (as `look` says) through the GPU: its vertex data is
/// rebuilt only when the display list, the selection or the vertex origin changes. Dashing every
/// selected line and tessellating it as egui shapes each frame made large selections sluggish.
/// `prims`: the selection's primitives in `list`, asked for only when the vertex data is rebuilt.
/// `stencil`: [`crate::gpu::Stencil::Inside`] keeps it inside a clipped viewport's outline.
#[allow(clippy::too_many_arguments)]
fn draw_highlight_gpu(
    p: &egui::Painter,
    xf: &Xf,
    list: &DisplayList,
    list_gen: u64,
    prims: impl FnOnce() -> Vec<usize>,
    sel: &[Handle],
    color: Color32,
    look: Look,
    hl: (&mut Option<u64>, &crate::gpu::MeshSlot, &mut Vec2, &mut Vec<usize>),
    stencil: crate::gpu::Stencil,
) {
    let (hl_key, hl_slot, hl_origin, hl_infinite) = hl;
    let origin = crate::gpu::choose_origin(&list.bounds, xf.center, f64::from(xf.rect.height()) / xf.scale.max(1e-300));
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (list_gen, sel, origin.x.to_bits(), origin.y.to_bits(), color.to_array(), look.width.to_bits(), look.dashed).hash(&mut h);
        h.finish()
    };
    if *hl_key != Some(key) {
        let prims = prims();
        let mesh = crate::gpu::build_highlight_mesh(list, &prims, origin, color.to_array(), look.width, look.dashed);
        *hl_infinite = prims.into_iter().filter(|&i| list.prims.get(i).is_some_and(|p| matches!(p.kind, Kind::Infinite { .. }))).collect();
        if let Ok(mut slot) = hl_slot.lock() {
            *slot = Some(mesh);
        }
        *hl_key = Some(key);
        *hl_origin = origin;
    }
    let center = xf.rect.center();
    p.add(egui_wgpu::Callback::new_paint_callback(
        xf.rect,
        crate::gpu::CanvasCallback {
            key,
            slot: hl_slot.clone(),
            offset: [(hl_origin.x - xf.center.x) as f32, (hl_origin.y - xf.center.y) as f32],
            scale: xf.scale as f32,
            center: [center.x, center.y],
            layer: crate::gpu::LAYER_HIGHLIGHT,
            xform: crate::gpu::IDENTITY_XFORM,
            dash: if look.dashed { 10.0 } else { 0.0 },
            stencil,
        },
    ));
}

/// Draw a MOVE/COPY/ROTATE/SCALE preview the way the drawing itself is drawn: the dragged
/// objects are tessellated and uploaded once (again only when the drawing, the selection or the
/// zoom band changes), and each frame only the transform in the view uniform moves. Rebuilding
/// the transformed copies as egui shapes every frame was slow with heavy selections and could
/// overflow the GPU buffer limit.
/// `clipboard` is the session's clipboard and its serial (dragged when `drag.clipboard`, by PASTE).
/// `transparency`: TRANSPARENCYDISPLAY (dragged objects keep their transparency).
#[allow(clippy::too_many_arguments)]
fn draw_drag_gpu(
    c: &mut CanvasState,
    p: &egui::Painter,
    xf: &Xf,
    doc: &cadcraft_doc::Drawing,
    drag: &cadcraft_engine::Drag,
    clipboard: (&[cadcraft_doc::Entity], u64),
    bg: Rgb,
    transparency: bool,
    view: ViewDir,
) {
    // Tessellate for a power-of-two zoom band so zooming mid-drag rebuilds only every 2x.
    let band = xf.scale.max(1e-300).log2().round() as i32;
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (c.key, band, bg.0, bg.1, bg.2, &drag.handles, transparency).hash(&mut h);
        if drag.clipboard {
            clipboard.1.hash(&mut h);
        }
        h.finish()
    };
    if c.drag_key != Some(key) {
        let band_scale = 2f64.powi(band);
        let opts = cadcraft_render::Options { transparency, ..preview_options(band_scale, view) };
        let list = if drag.clipboard {
            cadcraft_render::build_entities(doc, clipboard.0.iter(), &opts)
        } else {
            cadcraft_render::build_entities(doc, drag.handles.iter().filter_map(|h| doc.entity(*h)).map(|e| e.as_ref()), &opts)
        };
        let origin = if list.bounds.is_empty() { Vec2::ZERO } else { list.bounds.center() };
        let mesh = crate::gpu::build_mesh(&list, origin, bg, false);
        if let Ok(mut slot) = c.drag_slot.lock() {
            *slot = Some(mesh);
        }
        c.drag_key = Some(key);
        c.drag_origin = origin;
    }
    // Positions are relative to the origin o: M(o + p) = L·p + (M(o) − o). The list is in the
    // view plane, so the world transform M acts there as P·M·P⁻¹ (exact on the XY plane; see
    // [`solid_drag_preview`] for what isn't on it).
    let m = if xf.flat() { drag.matrix } else { xf.plane.then_before(drag.matrix).then_before(xf.unplane) };
    let o = c.drag_origin;
    let t = m.apply(o) - o;
    let center = xf.rect.center();
    p.add(egui_wgpu::Callback::new_paint_callback(
        xf.rect,
        crate::gpu::CanvasCallback {
            key,
            slot: c.drag_slot.clone(),
            offset: [(o.x - xf.center.x) as f32, (o.y - xf.center.y) as f32],
            scale: xf.scale as f32,
            center: [center.x, center.y],
            layer: crate::gpu::LAYER_DRAG,
            xform: [m.a as f32, m.b as f32, m.c as f32, m.d as f32, t.x as f32, t.y as f32],
            dash: 0.0,
            stencil: crate::gpu::Stencil::Off,
        },
    ));
}

/// A drag of solids in a 3D view, as transformed copies rebuilt in the view: the GPU drag
/// ([`draw_drag_gpu`]) moves the view plane, which carries a solid's height along wrongly when it
/// turns or mirrors (anything but a move or a positive uniform scale), and flattens it in a view
/// that sees the XY plane edge-on. `None`: the GPU drag draws it.
/// ponytail: rebuilds every dragged object each frame; split the solids out if mixed drags lag.
/// With a construction plane other than World the drag's matrix is in CPlane coordinates: every
/// object is rebuilt, moved by it in the world ([`in_world`]). A perspective view rebuilds every
/// drag (no 2D transform moves what it shows).
fn solid_drag_preview(doc: &cadcraft_doc::Drawing, drag: &cadcraft_engine::Drag, clipboard: &[cadcraft_doc::Entity], xf: &Xf) -> Option<DisplayList> {
    let m = drag.matrix;
    let persp = xf.dir.persp.is_some();
    // Plan view with a level CPlane sees every drag as the 2D transform the GPU drag draws.
    let level = (xf.cplane.apply_vec(cadcraft_geom::Vec3::Z) - cadcraft_geom::Vec3::Z).len() < 1e-9;
    if !persp && ((xf.is_plan() && level) || (m.b == 0.0 && m.c == 0.0 && m.a == m.d && m.a > 0.0 && xf.plane.det().abs() > 1e-6)) {
        return None;
    }
    let ents: Vec<&cadcraft_doc::Entity> =
        if drag.clipboard { clipboard.iter().collect() } else { drag.handles.iter().filter_map(|h| doc.entity(*h)).map(|e| e.as_ref()).collect() };
    let world = xf.cplane == Mat4::IDENTITY;
    if world && !persp && !ents.iter().any(|e| matches!(e.kind, cadcraft_doc::EntityKind::Solid3d(_))) {
        return None;
    }
    let m3 = in_world(&xf.cplane, &m);
    let moved: Vec<cadcraft_doc::Entity> = ents
        .into_iter()
        .map(|e| {
            // One that can't take the transform (see `Entity::transform`) previews unmoved.
            let mut e = e.clone();
            let _ = if world { e.transform(&m) } else { e.transform3(&m3) };
            e
        })
        .collect();
    Some(rebuilt_preview(doc, moved.iter(), xf.scale, xf.dir))
}

/// Identifies the GPU vertex data of list `key` / `list_gen` converted at `origin` against `bg`.
fn list_mesh_key(key: &ListKey, list_gen: u64, origin: Vec2, bg: Rgb, lwdisplay: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (key, list_gen, origin.x.to_bits(), origin.y.to_bits(), bg.0, bg.1, bg.2, lwdisplay).hash(&mut h);
    h.finish()
}

/// Draw the display list through the GPU paint callback (uploading it when it changed), plus
/// the infinite lines, which are clipped to the view on the CPU every frame.
fn draw_list_gpu(c: &mut CanvasState, p: &egui::Painter, xf: &Xf, bg: Rgb, lwdisplay: bool) {
    let Some(list) = &c.list else { return };
    let origin = crate::gpu::choose_origin(&list.bounds, xf.center, f64::from(xf.rect.height()) / xf.scale.max(1e-300));
    let key = list_mesh_key(&c.key, c.list_gen, origin, bg, lwdisplay);
    if c.mesh_key != Some(key) {
        let t0 = crate::now_ms();
        let mesh = crate::gpu::build_mesh(list, origin, bg, lwdisplay);
        if let Ok(mut slot) = c.mesh_slot.lock() {
            *slot = Some(mesh);
        }
        c.infinite = list.prims.iter().enumerate().filter(|(_, pr)| matches!(pr.kind, Kind::Infinite { .. })).map(|(i, _)| i).collect();
        c.mesh_key = Some(key);
        c.mesh_origin = origin;
        c.mesh_ms = crate::now_ms() - t0;
    }
    let center = xf.rect.center();
    p.add(egui_wgpu::Callback::new_paint_callback(
        xf.rect,
        crate::gpu::CanvasCallback {
            key,
            slot: c.mesh_slot.clone(),
            offset: [(c.mesh_origin.x - xf.center.x) as f32, (c.mesh_origin.y - xf.center.y) as f32],
            scale: xf.scale as f32,
            center: [center.x, center.y],
            layer: crate::gpu::LAYER_DRAWING,
            xform: crate::gpu::IDENTITY_XFORM,
            dash: 0.0,
            stencil: crate::gpu::Stencil::Off,
        },
    ));
    let mut shapes = Vec::new();
    for prim in c.infinite.iter().filter_map(|i| list.prims.get(*i)) {
        let pts = list.points(prim);
        if let (Kind::Infinite { ray }, Some(b), Some(d)) = (prim.kind, pts.first(), pts.get(1))
            && let Some(seg) = clip_infinite(xf, *b, *d, ray)
        {
            shapes.push(Shape::line_segment(seg, Stroke::new(1.0, prim_color32(prim, bg))));
        }
    }
    p.extend(shapes);
}

/// How a highlight is drawn: lines `width` points wide, and either solid with fills filled or
/// dashed with fills outlined.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Look {
    pub width: f32,
    pub dashed: bool,
}

impl Look {
    /// Selected objects (HIGHLIGHT, SELECTIONEFFECT); `None`: not highlighted.
    pub(crate) fn selection(s: &cadcraft_engine::Settings) -> Option<Look> {
        s.highlight.then_some(Look { width: 1.5, dashed: s.selectioneffect == 0 })
    }
    /// The object under the cursor (PREVIEWEFFECT: 0 dashed, 1 thicker, 2 both).
    pub(crate) fn preview(s: &cadcraft_engine::Settings) -> Look {
        Look { width: if s.previeweffect == 0 { 1.0 } else { 2.0 }, dashed: s.previeweffect != 1 }
    }
}

/// Draw selected/hovered entities as highlight overlays: `prims` are indices into `list` (from
/// [`PrimIndex::prims`], [`prims_of`], or all of a [`VpHighlight`] list), redrawn in `color` as `look` says. A wipeout is always
/// outlined, since filling it would hide the drawing under it.
pub(crate) fn draw_highlight(p: &egui::Painter, xf: &Xf, list: &DisplayList, prims: &[usize], color: Color32, look: Look) {
    let stroke = Stroke::new(look.width, color);
    let line = |pts: Vec<Pos2>, out: &mut Vec<Shape>| {
        if look.dashed {
            out.extend(Shape::dashed_line(&pts, stroke, 6.0, 4.0));
        } else {
            out.push(Shape::line(pts, stroke));
        }
    };
    let mut shapes = Vec::new();
    let mut fill = egui::Mesh::default();
    let fill_color = color.gamma_multiply(crate::gpu::HIGHLIGHT_FILL_ALPHA);
    for prim in prims.iter().filter_map(|&i| list.prims.get(i)) {
        let pts = list.points(prim);
        match prim.kind {
            Kind::Polyline => line(pts.iter().map(|q| xf.view_to_screen(*q)).collect(), &mut shapes),
            Kind::Infinite { ray } => {
                if let (Some(b), Some(d)) = (pts.first(), pts.get(1))
                    && let Some(seg) = clip_infinite(xf, *b, *d, ray)
                {
                    line(seg.to_vec(), &mut shapes);
                }
            }
            Kind::Point => {
                if let Some(q) = pts.first() {
                    shapes.push(Shape::circle_stroke(xf.view_to_screen(*q), 3.0, Stroke::new(1.0, color)));
                }
            }
            Kind::Tris if !look.dashed => {
                for q in pts.as_chunks::<3>().0.iter().flatten() {
                    let i = fill.vertices.len() as u32;
                    fill.colored_vertex(xf.view_to_screen(*q), fill_color);
                    fill.indices.push(i);
                }
            }
            Kind::Tris | Kind::Mask => {
                for outline in list.fill_outlines(prim) {
                    line(outline.iter().map(|q| xf.view_to_screen(*q)).collect(), &mut shapes);
                }
            }
        }
    }
    if !fill.is_empty() {
        shapes.push(Shape::mesh(fill));
    }
    p.extend(shapes);
}

/// The fill of a selection window or crossing area (SELECTIONAREA, WINDOWAREACOLOR,
/// CROSSINGAREACOLOR, SELECTIONAREAOPACITY); `None` when areas aren't filled.
fn selection_area_fill(s: &cadcraft_engine::Settings, crossing: bool) -> Option<Color32> {
    let aci = if crossing { s.crossingareacolor } else { s.windowareacolor };
    let c = cadcraft_color::aci_rgb(aci);
    s.selectionarea.then(|| Color32::from_rgb(c.0, c.1, c.2).gamma_multiply(f32::from(s.selectionareaopacity.min(100)) / 100.0))
}

/// The rubber band of selection mode `m` (typed W, C, BOX, WP, CP or F) with its points so far
/// and the cursor at `cursor`, as AutoCAD draws them: a window (solid) or crossing (dashed)
/// rectangle, a window (solid) or crossing (dashed) polygon, filled as the selection area
/// settings say, or a dashed fence. A point clicked in this view shows where it was clicked.
fn draw_select_mode(p: &egui::Painter, xf: &Xf, m: &cadcraft_engine::select::SelectMode, cursor: Pos2, s: &cadcraft_engine::Settings) {
    use cadcraft_engine::select::SelectKind as K;
    let t = Tokens::get();
    let at = |(q, seen): (&Vec2, &Option<(ViewDir, Vec2)>)| match seen {
        Some((d, v)) if *d == xf.dir => xf.view_to_screen(*v),
        _ => xf.to_screen(*q),
    };
    let mut pts: Vec<Pos2> = m.pts.iter().zip(m.seen.iter().chain(std::iter::repeat(&None))).map(at).collect();
    let Some(first) = pts.first().copied() else { return };
    pts.push(cursor);
    let dashed = |p: &egui::Painter, pts: &[Pos2], st: Stroke| p.extend(Shape::dashed_line(pts, st, 5.0, 3.0));
    match m.kind {
        K::Window | K::Crossing | K::Box => {
            let crossing = m.kind == K::Crossing || (m.kind == K::Box && cursor.x < first.x);
            let r = Rect::from_two_pos(first, cursor);
            if let Some(fill) = selection_area_fill(s, crossing) {
                p.rect_filled(r, 0.0, fill);
            }
            let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
            if crossing {
                dashed(p, &corners, Stroke::new(1.0, t.crossing_stroke));
            } else {
                p.rect_stroke(r, 0.0, Stroke::new(1.0, t.window_stroke), egui::StrokeKind::Middle);
            }
        }
        K::WPolygon | K::CPolygon => {
            let crossing = m.kind == K::CPolygon;
            if pts.len() >= 3
                && let Some(fill) = selection_area_fill(s, crossing)
            {
                let ring: Vec<Vec2> = pts.iter().map(|q| Vec2::new(f64::from(q.x), f64::from(q.y))).collect();
                let mut mesh = egui::Mesh::default();
                for tri in cadcraft_render::triangulate_evenodd(&[ring]).chunks_exact(3) {
                    for q in tri {
                        mesh.colored_vertex(pos2(q.x as f32, q.y as f32), fill);
                    }
                }
                mesh.indices = (0..mesh.vertices.len() as u32).collect();
                p.add(Shape::mesh(mesh));
            }
            pts.push(first);
            if crossing {
                dashed(p, &pts, Stroke::new(1.0, t.crossing_stroke));
            } else {
                p.add(Shape::line(pts, Stroke::new(1.0, t.window_stroke)));
            }
        }
        K::Fence => dashed(p, &pts, Stroke::new(1.0, t.crossing_stroke)),
        K::Group => {}
    }
}

fn draw_snap_marker(p: &egui::Painter, at: Pos2, hit: &SnapHit) {
    snap_glyph(p, at, hit.mode, crate::theme::snap_marker_half(), Stroke::new(2.0, Tokens::get().snap));
    p.text(at + vec2(12.0, 12.0), egui::Align2::LEFT_TOP, crate::i18n::t(hit.name), crate::theme::small(), Color32::BLACK);
    let galley = p.layout_no_wrap(crate::i18n::t(hit.name).to_string(), crate::theme::small(), Color32::BLACK);
    let r = Rect::from_min_size(at + vec2(10.0, 10.0), galley.size() + vec2(6.0, 2.0));
    p.rect_filled(r, 2.0, Color32::from_rgb(0xff, 0xff, 0xe1));
    p.galley(r.min + vec2(3.0, 1.0), galley, Color32::BLACK);
}

/// The marker of object snap `mode` (an OSMODE bit) at `at`, `s` from its centre to its sides:
/// on the canvas and beside the modes in Drafting Settings.
pub(crate) fn snap_glyph(p: &egui::Painter, at: Pos2, m: u32, s: f32, st: Stroke) {
    use snap::mode;
    match m {
        mode::END => {
            p.rect_stroke(Rect::from_center_size(at, vec2(2.0 * s, 2.0 * s)), 0.0, st, egui::StrokeKind::Middle);
        }
        mode::MID => {
            p.add(Shape::closed_line(vec![at + vec2(0.0, -s), at + vec2(s, s * 0.8), at + vec2(-s, s * 0.8)], st));
        }
        mode::CEN => {
            p.circle_stroke(at, s, st);
        }
        mode::GCEN => {
            p.circle_stroke(at, s, st);
            p.circle_filled(at, 1.5, st.color);
        }
        mode::QUA => {
            p.add(Shape::closed_line(vec![at + vec2(0.0, -s), at + vec2(s, 0.0), at + vec2(0.0, s), at + vec2(-s, 0.0)], st));
        }
        mode::INT | mode::APP => {
            p.line_segment([at + vec2(-s, -s), at + vec2(s, s)], st);
            p.line_segment([at + vec2(s, -s), at + vec2(-s, s)], st);
            if m == mode::APP {
                p.rect_stroke(Rect::from_center_size(at, vec2(2.0 * s, 2.0 * s)), 0.0, st, egui::StrokeKind::Middle);
            }
        }
        mode::PER => {
            p.line(vec![at + vec2(-s, -s), at + vec2(-s, s), at + vec2(s, s)], st);
            p.line(vec![at + vec2(-s, 0.0), at + vec2(0.0, 0.0), at + vec2(0.0, s)], st);
        }
        mode::TAN => {
            p.circle_stroke(at, s * 0.8, st);
            p.line_segment([at + vec2(-s, -s), at + vec2(s, -s)], st);
        }
        mode::NOD => {
            p.circle_stroke(at, s, st);
            p.line_segment([at + vec2(-s, -s), at + vec2(s, s)], st);
            p.line_segment([at + vec2(s, -s), at + vec2(-s, s)], st);
        }
        mode::INS => {
            p.line(
                vec![
                    at + vec2(-s, -s),
                    at + vec2(0.0, -s),
                    at + vec2(0.0, 0.0),
                    at + vec2(s, 0.0),
                    at + vec2(s, s),
                    at + vec2(-s, s),
                    at + vec2(-s, -s),
                ],
                st,
            );
        }
        // Extension: a dotted line; Parallel: two slanted strokes.
        mode::EXT => {
            for x in [-s, 0.0, s] {
                p.circle_filled(at + vec2(x, 0.0), 1.2, st.color);
            }
        }
        mode::PAR => {
            p.line_segment([at + vec2(-s, s), at + vec2(-0.2 * s, -s)], st);
            p.line_segment([at + vec2(0.2 * s, s), at + vec2(s, -s)], st);
        }
        // Nearest (and anything else): an hourglass.
        _ => {
            p.line(vec![at + vec2(-s, -s), at + vec2(s, -s), at + vec2(-s, s), at + vec2(s, s), at + vec2(-s, -s)], st);
        }
    }
}

/// Acquired tracking points (small `+`, `//` for a Parallel line), the dotted paths the cursor
/// is on and the tooltip naming them ("Endpoint: 5.0000 < 90°").
fn draw_tracking(p: &egui::Painter, xf: &Xf, acquired: &[snap::tracking::Acquired], track: Option<&snap::tracking::Track>, far: f64) {
    use snap::tracking::{Acquired, PathShape};
    let t = Tokens::get();
    let st = Stroke::new(1.0, t.snap);
    for a in acquired {
        let c = xf.to_screen(a.at());
        if let Acquired::Parallel { .. } = a {
            p.line_segment([c + vec2(-5.0, 3.0), c + vec2(-1.0, -5.0)], st);
            p.line_segment([c + vec2(1.0, 3.0), c + vec2(5.0, -5.0)], st);
        } else {
            p.line_segment([c + vec2(-4.0, 0.0), c + vec2(4.0, 0.0)], st);
            p.line_segment([c + vec2(0.0, -4.0), c + vec2(0.0, 4.0)], st);
        }
    }
    let Some(tr) = track else { return };
    let mut tips = Vec::new();
    for path in &tr.paths {
        // Around the cursor only (a path's origin may be far off screen).
        let pts: Vec<Pos2> = match path.shape {
            PathShape::Ray { origin, dir } => {
                let back = (tr.point - origin).dot(dir).clamp(0.0, far);
                xf.segment_to_screen(tr.point - dir * back, tr.point + dir * far).map_or_else(Vec::new, Vec::from)
            }
            // In perspective only what is in front of the eye.
            PathShape::Line { dir, .. } => xf.segment_to_screen(tr.point - dir * far, tr.point + dir * far).map_or_else(Vec::new, Vec::from),
            PathShape::Circle { center, radius, .. } => {
                let (a0, span) = (center.angle_to(tr.point), (far / radius).min(std::f64::consts::PI));
                (0..=96).map(|i| xf.to_screen(Vec2::polar(center, radius, a0 - span + span * f64::from(i) / 48.0))).collect()
            }
            PathShape::Ellipse { center, major, ratio, .. } => {
                let e = cadcraft_geom::Ellipse::full(center, major, ratio);
                let (t0, span) = (e.param_of(tr.point), (far / (major.len() * ratio.min(1.0))).min(std::f64::consts::PI));
                (0..=96).map(|i| xf.to_screen(e.at_param(t0 - span + span * f64::from(i) / 48.0))).collect()
            }
        };
        p.extend(Shape::dotted_line(&pts, t.snap, 6.0, 1.0));
        let (distance, angle) = (
            cadcraft_engine::units::format_distance(path.from.dist(tr.point), 2, 4),
            (path.from.angle_to(tr.point).to_degrees().round() as i64).rem_euclid(360),
        );
        tips.push(if path.name == "Polar" {
            crate::tf!("Polar: {distance} < {angle}°", distance = distance, angle = angle)
        } else {
            crate::tf!("{name}: {distance} < {angle}°", name = crate::i18n::t(path.name), distance = distance, angle = angle)
        });
    }
    if let Some(o) = tr.object {
        tips.push(crate::i18n::t(o).to_string());
    }
    let at = xf.to_screen(tr.point);
    if tr.paths.len() > 1 || tr.object.is_some() {
        p.line_segment([at + vec2(-4.0, -4.0), at + vec2(4.0, 4.0)], st);
        p.line_segment([at + vec2(4.0, -4.0), at + vec2(-4.0, 4.0)], st);
    }
    tooltip(p, at + vec2(16.0, 18.0), &tips.join(", "));
}

/// The rubber-band and snap-adjusted cursor point for the current prompt (the engine's
/// `Session::snap_cursor`: object snaps, acquiring tracking points, grid, tracking paths, ortho).
fn effective_point(app: &mut CadApp, raw: Vec2, xf: &Xf, ctx: &egui::Context) -> Vec2 {
    let prompt = app.session.current_prompt();
    let wants_point = prompt.as_ref().is_some_and(cadcraft_engine::pointmod::accepts);
    let base = prompt.as_ref().and_then(|p| p.base);
    let deferred = prompt.as_ref().is_some_and(|p| p.deferred);
    let corner = prompt.as_ref().is_some_and(|p| p.corner);
    app.canvas.snap = None;
    app.canvas.snap_z = None;
    app.canvas.track = None;
    if !wants_point || app.session.pending_window.is_some() {
        if !wants_point {
            app.session.tracking.clear();
        }
        return raw;
    }
    let (now, shift) = ctx.input(|i| (i.time, i.modifiers.shift));
    let q =
        snap::tracking::CursorQuery { base, deferred, corner, aperture: app.session.settings.aperture / xf.scale, tol: 6.0 / xf.scale, now, shift };
    let r = app.session.snap_cursor(raw, &q);
    if r.wait {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(snap::tracking::DWELL / 4.0));
    }
    app.canvas.snap = r.snap;
    app.canvas.track = r.track;
    // In a 3D view the engine snaps in the construction plane, where an object off it has only
    // its shadow: its snap stands only where an object in the plane gives it too (snap3d snaps the
    // others where they are drawn).
    // ponytail: a shadow snap dropped here leaves the cursor raw for that frame (no tracking, grid
    // or ortho) and may have acquired a tracking point; both go once the engine's snap can skip
    // objects off the plane (see `snap3d::ground_doc`).
    if r.snap.is_some()
        && let Some(g) = snap3d::ground_doc(app, xf)
        && let Ok(space) = app.session.state().map(|st| st.edit_space())
    {
        let s = &app.session.settings;
        let osmode = s.osmode | snap::mode::NO_SOLIDS | if s.osnaphatch { snap::mode::HATCH } else { 0 };
        let hit = snap::osnap(&g, &space, raw, q.aperture, osmode, base, deferred);
        app.canvas.snap = hit;
        return hit.map_or(raw, |h| h.point);
    }
    r.point
}

/// The vertical wheel scroll this frame that zooms, in points, read from the raw wheel events so
/// each notch zooms by its whole step at once, as in standard CAD (#660). egui's smoothed delta
/// spreads a notch over several frames, so quick notches lag behind the wheel. Units convert to
/// points as egui does (a line is `line_scroll_speed` points, a page the view's height), and
/// events egui turns into a pinch zoom (⌘/Ctrl) or a sideways scroll (⇧) are left out, as it does.
fn wheel_zoom_points(events: &[egui::Event], opts: &egui::InputOptions, page: f32) -> f32 {
    let points: f32 = events
        .iter()
        .filter_map(|e| match e {
            egui::Event::MouseWheel { unit, delta, modifiers, .. }
                if !modifiers.matches_any(opts.zoom_modifier) && !modifiers.matches_any(opts.horizontal_scroll_modifier) =>
            {
                let per = match unit {
                    egui::MouseWheelUnit::Point => 1.0,
                    egui::MouseWheelUnit::Line => opts.line_scroll_speed,
                    egui::MouseWheelUnit::Page => page,
                };
                Some(delta.y * per)
            }
            _ => None,
        })
        .sum();
    if points.is_finite() { points } else { 0.0 }
}

pub fn show(app: &mut CadApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    let full = ui.available_rect_before_wrap();
    // Tiled model viewports (VPORTS): the current tile is the drawing area below; `draw_tiles`
    // draws the others. A tile that became current takes its own list ([`tiles::trade`]).
    tiles::trade(app);
    let mut tiles = tile_rects(app, full);
    let resp = ui.interact(full, ui.id().with("canvas"), Sense::click_and_drag());
    // A click in another tile makes it current (or answers -VPORTS Join's prompts); in a 3D
    // tile it goes on to pick there, below, as in Rhino. Its title takes its own clicks.
    match tiles::click(app, ui, &resp, &tiles) {
        Some(true) => tiles = tile_rects(app, full),
        Some(false) => return,
        None => {}
    }
    let rect = tiles.iter().find(|(_, tile)| tile.active).map_or(full, |(r, _)| *r);
    let painter = ui.painter_at(rect);
    app.canvas.rect = Some(rect);
    app.session.viewport_px = (f64::from(rect.width()).max(1.0), f64::from(rect.height()).max(1.0));
    if app.canvas.zoom_pending {
        app.canvas.zoom_pending = false;
        let _ = app.session.zoom_extents();
    }
    let Ok(st) = app.session.state() else { return };
    let view = st.view();
    let scale = f64::from(rect.height()) / view.height.max(1e-12);
    // Navigation works in the current view (the canvas may still show the last one).
    let xf = view_xfs(app, rect, view).0;
    app.canvas.xf = Some(xf);

    // ---------- input ----------
    let (hover_pos, scroll, mods, middle_down, pressed_primary, pressed_secondary, dbl_middle, dbl_primary) = ui.input(|i| {
        (
            i.pointer.hover_pos(),
            i.smooth_scroll_delta,
            i.modifiers,
            i.pointer.middle_down(),
            i.pointer.primary_clicked(),
            i.pointer.secondary_clicked(),
            i.pointer.button_double_clicked(egui::PointerButton::Middle),
            i.pointer.button_double_clicked(egui::PointerButton::Primary),
        )
    });
    let inside = hover_pos.is_some_and(|p| rect.contains(p)) && resp.hovered();
    // Which device is scrolling? On macOS, trackpads (and Magic Mouse) report precise point
    // deltas; notched mouse wheels report lines. The answer is sticky because egui spreads a wheel
    // notch over several frames. Elsewhere (browsers, Wayland smooth-scroll mice) ordinary wheels
    // also report points, so the wheel keeps zooming there.
    ui.input(|i| {
        for e in &i.raw.events {
            if let egui::Event::MouseWheel { unit, .. } = e {
                app.canvas.scroll_pans = cfg!(target_os = "macos") && *unit == egui::MouseWheelUnit::Point;
            }
        }
    });
    if inside && app.canvas.scroll_pans && scroll != egui::Vec2::ZERO {
        // Two-finger swipe pans: the drawing follows the fingers, like a middle-drag.
        // Pinch (and Cmd+scroll) still zoom via `zoom_delta` below.
        if let Ok(st) = app.session.state_mut() {
            let v = st.view();
            st.set_view_quiet(cadcraft_engine::View {
                center: v.center - Vec2::new(f64::from(scroll.x) / scale, -f64::from(scroll.y) / scale),
                height: v.height,
            });
        }
    }
    let opts = ui.ctx().options(|o| o.input_options);
    let wheel = ui.input(|i| wheel_zoom_points(&i.raw.events, &opts, rect.height()));
    if inside
        && !app.canvas.scroll_pans
        && wheel != 0.0
        && let Some(hp) = hover_pos
    {
        // Zoom with the mouse wheel about the cursor, each notch at once.
        let f = app.session.wheel_zoom_factor(f64::from(wheel));
        let about = xf.to_view(hp);
        let _ = app.session.zoom_about(f, about);
    }
    let zoom_pinch = ui.input(|i| crate::animation::zoom_delta(i, &opts, app.shows_animations()));
    if inside
        && (zoom_pinch - 1.0).abs() > 1e-4
        && let Some(hp) = hover_pos
    {
        let _ = app.session.zoom_about(f64::from(zoom_pinch), xf.to_view(hp));
    }
    // Pan with the middle button (or Shift+right-drag / two-finger drag on trackpads is scroll).
    let middle_pressed = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Middle));
    let focused = ui.input(|i| i.focused);
    if middle_down
        && focused
        && (app.canvas.pan_last.is_some() || (middle_pressed && inside))
        && let Some(hp) = hover_pos
    {
        if let Some(last) = app.canvas.pan_last {
            let d = hp - last;
            // Shift+middle drag orbits the model view (3DORBIT's transparent form); the Model tab
            // only, as MSPACE viewports show plan.
            if mods.shift && app.session.layout_space() == cadcraft_doc::Space::Model {
                let _ = cadcraft_engine::orbit(&mut app.session, -f64::from(d.x) * ORBIT_DEG_PER_PX, f64::from(d.y) * ORBIT_DEG_PER_PX);
            } else if let Ok(st) = app.session.state_mut() {
                let v = st.view();
                st.set_view_quiet(cadcraft_engine::View {
                    center: v.center - Vec2::new(f64::from(d.x) / scale, -f64::from(d.y) / scale),
                    height: v.height,
                });
            }
        }
        app.canvas.pan_last = Some(hp);
    } else {
        app.canvas.pan_last = None;
    }
    if dbl_middle && inside {
        let _ = app.session.zoom_extents();
    }
    // ZOOM Realtime: dragging up zooms in, down zooms out.
    if resp.dragged_by(egui::PointerButton::Primary) && app.session.realtime_zoom_active() {
        let _ = app.session.realtime_zoom(f64::from(resp.drag_delta().y));
    }
    // Rhino's right-drag navigation in 3D tiles (one starting in another tile makes it current).
    if tiles::right_drag(app, ui, &resp, rect, &tiles) {
        return;
    }
    // Re-read the view after navigation.
    let view = app.session.state().map(|s| s.view()).unwrap_or(view);
    let scale = f64::from(rect.height()) / view.height.max(1e-12);
    // `xf_now` is the current view, which the list is built for; `xf` the one shown.
    let (xf_now, xf) = view_xfs(app, rect, view);
    app.canvas.xf = Some(xf);
    // The paper (display) transform; equal to `xf` except inside an MSPACE viewport.
    let (xf_paper, active_vp) = match app.session.state() {
        Ok(st) => {
            let pv = st.paper_view();
            let xp = Xf::new(rect, pv.center, f64::from(rect.height()) / pv.height.max(1e-12));
            (if st.mspace.is_some() { xp } else { xf }, st.active_viewport().map(|(_, v)| v))
        }
        Err(_) => (xf, None),
    };
    let mspace = app.session.state().is_ok_and(|st| st.mspace.is_some());
    // A clipped active viewport's outline (picks and its heavy border follow it).
    let active_clip = app.session.state().ok().and_then(|st| st.active_viewport().and_then(|(h, v)| cadcraft_render::viewport_clip(&st.doc, h, &v)));
    // Double-click a viewport to work inside it; double-click the sheet outside it to return.
    if inside
        && dbl_primary
        && !app.session.prompting()
        && matches!(app.session.layout_space(), cadcraft_doc::Space::Paper(_))
        && let Some(hp) = hover_pos
    {
        let pp = xf_paper.to_world(hp);
        let in_rect = |v: &cadcraft_doc::Viewport| (pp.x - v.center.x).abs() <= v.width / 2.0 && (pp.y - v.center.y).abs() <= v.height / 2.0;
        let in_active = active_vp.as_ref().is_some_and(|v| active_clip.as_ref().map_or(in_rect(v), |c| c.contains(pp)));
        if !in_active {
            app.canvas.hot_grips.clear();
            app.session.pending_window = None;
            app.canvas.drag_window = false;
            if app.run("mspace", serde_json::json!({"at": [pp.x, pp.y]})).is_err() && active_vp.is_some() {
                let _ = app.run("pspace", serde_json::json!({}));
            }
            app.session.pending_window = None;
            app.canvas.drag_window = false;
            app.canvas.list = None;
            return;
        }
    }
    // Double-click text to edit it, as in AutoCAD: TEXTEDIT with that object already picked.
    if inside
        && dbl_primary
        && app.session.running.is_none()
        && let Some(hp) = hover_pos
        && edit_text_at(app, xf.to_world(hp))
    {
        return;
    }

    // Double-click a hatch (no command running) to edit it in the Hatch Edit dialog. The click is
    // used up: it must not also start a grip edit on the hatch's centre grip below.
    let mut hatch_dialog = false;
    if inside
        && dbl_primary
        && app.session.running.is_none()
        && let Some(hp) = hover_pos
        && let Some(h) = hatch_at(app, xf.to_world(hp), scale)
    {
        app.session.set_selection(vec![h]);
        crate::hatchdlg::open_edit(app);
        hatch_dialog = true;
    }

    // While a command asks for a point, every click places one, however fast the drafting (#815).
    // egui drops a click whose button comes up a few pixels from where it went down (a drag to it),
    // and eframe hands a slow frame's clicks over together, which egui reports as one.
    let point_prompt = app.session.current_prompt().is_some_and(|p| cadcraft_engine::pointmod::accepts(&p))
        && app.session.pending_window.is_none()
        && !app.session.realtime_zoom_active();
    let pressed_primary = pressed_primary || (point_prompt && resp.drag_stopped_by(egui::PointerButton::Primary));
    // As Rhino does, warn that a view seeing the construction plane edge-on puts points on its line.
    let edge_on = point_prompt && inside && xf.edge_on();
    if edge_on && !app.canvas.edge_warned {
        app.set_status(crate::tl!(
            "The construction plane is seen edge-on: points land on its line. Pick in another view or set a CPlane facing this one."
        ));
    }
    app.canvas.edge_warned = edge_on;
    if point_prompt && inside && pressed_primary && !middle_down && app.cmd.options.is_none() && app.cmd.buffer.is_empty() {
        let releases: Vec<egui::Pos2> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, .. } => Some(*pos),
                    _ => None,
                })
                .collect()
        });
        // The last one is placed below, at the cursor, like any click.
        if let Some((_, earlier)) = releases.split_last() {
            for pos in earlier.iter().filter(|p| rect.contains(**p)) {
                let w = effective_point(app, point_at(app, &xf, *pos), &xf, ui.ctx());
                snap3d::apply(app, &xf, *pos);
                if let Err(e) = snap3d::send_point(app, w) {
                    app.session.echo(e.to_string());
                }
            }
        }
    }

    let raw_world = hover_pos.filter(|_| inside).map(|p| point_at(app, &xf, p));
    // A grip dragged in a plane of its own sends the height it is at there with its point.
    app.canvas.grip_z = hover_pos.filter(|_| inside).and_then(|p| grip_height(app, &xf, p));
    let dyn_frame = crate::cmdline::dyn_frame(app);
    if let Some(w) = raw_world {
        let mut eff = effective_point(app, w, &xf, ui.ctx());
        // A locked Dynamic Input value (`40,` or `40<` typed) holds the cursor and overrides snaps.
        if let Some(f) = dyn_frame
            && let Some(e) = crate::dyninput::parse(&app.cmd.buffer, &f)
            && e.first_locked().is_some()
        {
            eff = crate::dyninput::constrain(&e, &f, eff, app.cmd.second_locked());
            app.canvas.snap = None;
            app.canvas.track = None;
        }
        // Grip dimensional input: typed lengths and angles hold the grip and override snaps.
        let grip_held = crate::grip_dyn::constrain(app, eff);
        if let Some(q) = grip_held {
            eff = q;
            app.canvas.snap = None;
            app.canvas.track = None;
        }
        app.canvas.cursor = Some(eff);
        app.session.cursor = eff;
        app.session.cursor_deferred = app.canvas.snap.and_then(|h| h.deferred);
        // Typed or menu snap overrides (END, NON…) and Tab cycling; a locked Dynamic Input value wins.
        if dyn_frame.and_then(|f| crate::dyninput::parse(&app.cmd.buffer, &f)).is_none_or(|e| e.first_locked().is_none()) {
            override_cursor(app, w, &xf, hover_pos.unwrap_or_else(|| xf.to_screen(w)));
            // A 3D view also snaps to solids where it draws them (the engine snaps in plan).
            if grip_held.is_none()
                && let Some(hp) = hover_pos
            {
                snap3d::apply(app, &xf, hp);
            }
        }
        // Typed grip dimensions win over snap overrides too.
        if let Some(q) = grip_held {
            app.canvas.cursor = Some(q);
            app.session.cursor = q;
            app.canvas.snap = None;
            app.session.cursor_deferred = None;
        }
    } else {
        app.canvas.cursor = None;
        app.canvas.snap = None;
        app.canvas.snap_z = None;
        app.session.cursor_deferred = None;
    }

    // Rhino's gumball takes presses on its handles, and the drag and keys that follow.
    let gumball = gumball::input(app, ui.ctx(), &xf, hover_pos, inside);
    // A click on the Dynamic Input options menu picks the keyword; elsewhere it closes the menu.
    let mut picked = false;
    if pressed_primary && app.cmd.options.is_some() {
        match hover_pos.and_then(|h| app.cmd.option_at(h)) {
            Some(k) => {
                crate::cmdline::pick_option(app, k);
                picked = true;
            }
            None => app.cmd.options = None,
        }
    }
    // A double-click with no command running (on a layout viewport, say) leaves no selection
    // window behind; while a command asks for points, every click still counts (#815).
    let idle_dbl = dbl_primary && app.session.running.is_none();
    if idle_dbl {
        app.session.pending_window = None;
        app.canvas.drag_window = false;
    }
    let pressed_primary = pressed_primary && !picked && !hatch_dialog && !idle_dbl && !gumball;
    // Clicks.
    if inside
        && pressed_primary
        && !middle_down
        && let Some(p) = app.canvas.cursor
    {
        if app.session.prompting() {
            // Ctrl held while picking the point of a grip edit makes copies (the Copy option).
            if mods.ctrl && app.session.running.as_ref().is_some_and(|r| r.id == "grip") {
                let _ = app.session.input(Input::Keyword("Copy".into()));
            }
            // Clicking places the point; a value typed into the Dynamic Input boxes is used up.
            if let Some(f) = dyn_frame
                && crate::dyninput::parse(&app.cmd.buffer, &f).is_some()
            {
                app.cmd.buffer.clear();
            }
            // In a 3D view an object prompt picks what is drawn under the cursor, and its window
            // takes what is drawn in it (the engine selects in plan).
            let view_select =
                xf.picks_drawn() && app.session.select_mode.is_none() && app.session.current_prompt().is_some_and(|q| q.picks_objects());
            let at = hover_pos.filter(|_| view_select);
            // Where the click is in such a view: a pick at a point-only object prompt becomes a
            // point on the object, and typed selection modes select what is drawn.
            app.session.view_at = hover_pos.filter(|_| xf.picks_drawn()).map(|hp| (xf.dir(), xf.to_view(hp)));
            let result = if let Some(hp) = at.filter(|_| app.session.pending_window.is_some()) {
                close_view_window(app, &xf, hp, mods.shift)
            } else if let Some(h) = at.and_then(|hp| view_pick_at(app, &xf, Some(hp))) {
                app.session.input_view_pick(vec![h])
            } else if at.is_some_and(|hp| begin_view_window(app, &xf, hp)) {
                // Nothing drawn there: "Select objects" opens a window.
                Ok(())
            } else {
                // Preserve a snap's viewport origin for DIST, its deferred curve where needed, and
                // a 3D snap's height.
                snap3d::send_point(app, p)
            };
            app.session.view_at = None;
            if let Err(e) = result {
                app.session.echo(e.to_string());
            }
        } else if let Some(g) = crate::dyngrips::at(app, &xf, hover_pos) {
            crate::dyngrips::click(app, g);
        } else if let Some(g) = grip_at(app, &xf, hover_pos) {
            click_grip(app, g, mods.shift);
        } else if let Some(hp) = hover_pos.filter(|_| xf.picks_drawn()) {
            // A 3D view picks and windows what is drawn (the engine selects in plan).
            if app.session.pending_window.is_some() {
                if let Err(e) = close_view_window(app, &xf, hp, mods.shift) {
                    app.session.echo(e.to_string());
                }
            } else if let Some(h) = view_pick_at(app, &xf, Some(hp)) {
                app.session.idle_pick(h, mods.shift);
            } else {
                begin_view_window(app, &xf, hp);
            }
        } else if let Err(e) = app.session.idle_click(raw_world.unwrap_or(p), mods.shift) {
            app.session.echo(e.to_string());
        }
    }
    // Press-and-drag selection window (AutoCAD's PICKDRAG = 2; click-click still works): a drag
    // that starts while objects are being selected opens a window where the button went down,
    // and letting go closes it. Drags on grips and drags while drawing are left alone.
    if resp.drag_started_by(egui::PointerButton::Primary)
        && !middle_down
        && !gumball
        && app.session.pending_window.is_none()
        && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        && rect.contains(origin)
        && grip_at(app, &xf, Some(origin)).is_none()
        && crate::dyngrips::at(app, &xf, Some(origin)).is_none()
    {
        app.canvas.drag_window = begin_view_window(app, &xf, origin);
    }
    if app.canvas.drag_window && resp.drag_stopped_by(egui::PointerButton::Primary) {
        app.canvas.drag_window = false;
        if app.session.pending_window.is_some()
            && let Some(end) = ui.input(|i| i.pointer.interact_pos())
        {
            let done = if xf.picks_drawn() {
                close_view_window(app, &xf, end, mods.shift)
            } else if app.session.prompting() {
                app.session.point_z = None;
                app.session.input(Input::Point(xf.to_world(end)))
            } else {
                app.session.idle_click(xf.to_world(end), mods.shift)
            };
            if let Err(e) = done {
                app.session.echo(e.to_string());
            }
        }
    }
    // Right-click acts as Enter while a command runs (the classic CAD default; held, it opens the
    // command menu); otherwise it opens the shortcut menu (Repeat, Clipboard, …).
    // Shift/Ctrl+right-click opens the object snap menu.
    let idle = !app.session.prompting();
    if inside && pressed_secondary {
        app.canvas.snap_menu = mods.shift || mods.ctrl;
    }
    if crate::context_menu::in_command(app, ui, inside, pressed_secondary) {
        let _ = app.session.input(Input::Enter);
    }
    // Right-click on a grip with options (polyline vertex, line end…) opens the grip menu.
    if inside && pressed_secondary && idle && !app.canvas.snap_menu {
        app.canvas.grip_menu = grip_at(app, &xf, hover_pos).filter(|g| !grip_options(app, *g).is_empty());
    }
    crate::dyngrips::menu(app, ui.ctx(), &xf);
    if idle && !app.canvas.snap_menu && app.canvas.grip_menu.is_some() {
        grip_menu(app, &resp);
    } else if idle || app.canvas.snap_menu {
        crate::context_menu::show(app, &resp);
    }

    // Hover highlight (throttled to cursor movement).
    // The cursor picks objects (pick box, rollover highlight): "Select objects" and single-object
    // prompts, or no command with PICKFIRST on; SELECTIONPREVIEW says in which of those it shows.
    let selecting = app.session.previews_selection();
    if inside && selecting && app.session.pending_window.is_none() {
        if hover_pos != app.canvas.hover_at
            && let (Some(w), Ok(st)) = (raw_world, app.session.state())
        {
            let filter = app.session.settings.previewfilter;
            let ap = app.session.settings.pickbox.max(1.0) * 1.5 / scale;
            // Plan picks as the engine does, in the construction plane's coordinates.
            let picked = if !xf.picks_drawn() {
                app.session.pick_doc().ok().and_then(|d| cadcraft_engine::select::pick(&d, &st.edit_space(), w, ap))
            } else {
                hover_pos.zip(app.canvas.list.as_ref()).and_then(|(hp, l)| list_pick(l, xf.to_view(hp), ap))
            };
            app.canvas.hover = picked.filter(|h| st.doc.entity(*h).is_none_or(|e| !cadcraft_engine::select::preview_filtered(&st.doc, e, filter)));
            app.canvas.hover_at = hover_pos;
        }
    } else {
        app.canvas.hover = None;
        app.canvas.hover_at = None;
    }

    // ---------- paint ----------
    painter.rect_filled(rect, 0.0, t.canvas);
    let t0 = crate::now_ms();
    // The list is built for the current view, and the tiles that look the same way.
    let xf_build = if mspace { xf_paper } else { xf_now };
    let mut views = vec![(xf_build.view_bounds(), 1.0 / xf_build.scale)];
    let look = app.session.state().map(|st| st.look()).ok();
    let shares = |tile: &cadcraft_engine::ModelTile| Some(tile.look) == look && tile.look.projection(tile.view).persp == xf_build.dir.persp;
    views.extend(tiles.iter().filter(|(r, tile)| !tile.active && shares(tile) && r.width() >= 1.0 && r.height() >= 1.0).map(|(r, tile)| {
        let xf = tile_xf(*r, tile, xf_build.dir);
        (xf.view_bounds(), 1.0 / xf.scale)
    }));
    ensure_list(app, &xf_build, &views, ui.ctx());
    app.canvas.last_look = look.filter(|_| app.session.state().is_ok_and(|st| st.space == cadcraft_doc::Space::Model));
    tiles::ensure_lists(app, &tiles, ui.ctx());
    // A build that landed turns the view shown.
    let xf = view_xfs(app, rect, view).1;
    let xf_paper = if mspace { xf_paper } else { xf };
    app.canvas.xf = Some(xf);
    let sheet = app.canvas.list.as_ref().and_then(|l| l.sheet);
    let bg = match sheet {
        Some(sh) => {
            // Paper space: grey surround, the sheet with a shadow, the printable area dashed.
            painter.rect_filled(rect, 0.0, Color32::from_rgb(0x50, 0x57, 0x63));
            let a = xf_paper.to_screen(Vec2::new(0.0, sh.size.y));
            let b = xf_paper.to_screen(Vec2::new(sh.size.x, 0.0));
            let paper = Rect::from_two_pos(a, b);
            painter.rect_filled(paper.translate(vec2(5.0, 5.0)), 0.0, Color32::from_black_alpha(110));
            painter.rect_filled(paper, 0.0, t.paper);
            let pa = Rect::from_two_pos(
                xf_paper.to_screen(Vec2::new(sh.printable.min.x, sh.printable.max.y)),
                xf_paper.to_screen(Vec2::new(sh.printable.max.x, sh.printable.min.y)),
            );
            let pts = [pa.left_top(), pa.right_top(), pa.right_bottom(), pa.left_bottom(), pa.left_top()];
            painter.extend(Shape::dashed_line(&pts, Stroke::new(1.0, Color32::from_gray(150)), 4.0, 4.0));
            Rgb(t.paper.r(), t.paper.g(), t.paper.b())
        }
        None => {
            blockedit::wash(app, &painter, rect);
            draw_grid(app, &painter, &xf);
            Rgb(t.canvas.r(), t.canvas.g(), t.canvas.b())
        }
    };
    let sel = app.session.selection();
    // How the object under the cursor and the selected ones are highlighted (PREVIEWEFFECT,
    // HIGHLIGHT, SELECTIONEFFECT); with HIGHLIGHT off only their grips show.
    let (preview_look, selection_look) = (Look::preview(&app.session.settings), Look::selection(&app.session.settings));
    // The active viewport's contents, drawn apart from the layout's list, under its own objects.
    // `masked`: its outline is in the stencil buffer this frame.
    let mut masked = false;
    if let Some(f) = ensure_vp_content(app, &xf_paper, ui.ctx()) {
        let gpu = app.canvas.gpu.is_some();
        let clipped = painter.with_clip_rect(f.on_screen.intersect(rect));
        draw_vp_content(&mut app.canvas.vp, gpu, &clipped, &f, &xf_paper, bg, app.session.settings.lwdisplay);
        masked = gpu && f.outline.is_some();
    }
    if let Some(list) = &app.canvas.list {
        images::paint(&painter, &xf_paper, list);
    }
    if app.canvas.gpu.is_some() {
        draw_list_gpu(&mut app.canvas, &painter, &xf_paper, bg, app.session.settings.lwdisplay);
    } else if let Some(list) = &app.canvas.list {
        draw_list(&painter, &xf_paper, list, bg, app.session.settings.lwdisplay);
    }
    if let Some(vp) = &active_vp {
        // The active viewport: a heavy border, highlights in model units clipped to it.
        let r = Rect::from_two_pos(
            xf_paper.to_screen(Vec2::new(vp.center.x - vp.width / 2.0, vp.center.y + vp.height / 2.0)),
            xf_paper.to_screen(Vec2::new(vp.center.x + vp.width / 2.0, vp.center.y - vp.height / 2.0)),
        );
        let heavy = Stroke::new(3.0, Color32::from_rgb(0x2a, 0x2a, 0x2a));
        match &active_clip {
            Some(c) => {
                let mut pts: Vec<egui::Pos2> = c.polygon().iter().map(|q| xf_paper.to_screen(*q)).collect();
                pts.extend(pts.first().copied());
                painter.add(egui::Shape::line(pts, heavy));
            }
            None => {
                painter.rect_stroke(r, 0.0, heavy, egui::StrokeKind::Middle);
            }
        }
        if let Ok(st) = app.session.state() {
            // A clipped viewport's outline bounds (its outline is masked when `masked`).
            let bounds = active_clip.as_ref().map_or(r, |c| {
                let b = c.bounds();
                Rect::from_two_pos(xf_paper.to_screen(Vec2::new(b.min.x, b.max.y)), xf_paper.to_screen(Vec2::new(b.max.x, b.min.y)))
            });
            let clipped = painter.with_clip_rect(bounds.intersect(rect));
            let c = &mut app.canvas;
            if let Some(h) = c.hover
                && !sel.contains(&h)
            {
                c.vp_hover.update(st, &[h], scale);
                let list = &c.vp_hover.list;
                draw_highlight(&clipped, &xf, list, &(0..list.prims.len()).collect::<Vec<_>>(), t.hover, preview_look);
            }
            if let Some(look) = selection_look
                && !sel.is_empty()
            {
                c.vp_sel.update(st, &sel, scale);
                let list = &c.vp_sel.list;
                let all = || (0..list.prims.len()).collect::<Vec<_>>();
                if c.gpu.is_some() {
                    // Shares the GPU highlight layer with the layout's own selection; the high bit
                    // keeps this list's generations apart from the layout list's.
                    let list_id = c.vp_sel.list_gen | 1 << 63;
                    let stencil = if masked { crate::gpu::Stencil::Inside } else { crate::gpu::Stencil::Off };
                    let hl = (&mut c.hl_key, &c.hl_slot, &mut c.hl_origin, &mut c.hl_infinite);
                    draw_highlight_gpu(&clipped, &xf, list, list_id, all, &sel, t.selection, look, hl, stencil);
                    draw_highlight(&clipped, &xf, list, &c.hl_infinite, t.selection, look);
                } else {
                    draw_highlight(&clipped, &xf, list, &all(), t.selection, look);
                }
            }
        }
    } else if let CanvasState { list: Some(list), list_gen, prim_index, hover, gpu, hl_key, hl_slot, hl_origin, hl_infinite, .. } = &mut app.canvas {
        if let Some(h) = *hover
            && !sel.contains(&h)
        {
            draw_highlight(&painter, &xf, list, &prim_index.prims(*list_gen, list, &[h]), t.hover, preview_look);
        }
        if let Some(look) = selection_look {
            if gpu.is_some() && !sel.is_empty() {
                let list_id = *list_gen;
                let hl = (hl_key, &*hl_slot, hl_origin, &mut *hl_infinite);
                draw_highlight_gpu(
                    &painter,
                    &xf,
                    list,
                    list_id,
                    || prim_index.prims(list_id, list, &sel),
                    &sel,
                    t.selection,
                    look,
                    hl,
                    crate::gpu::Stencil::Off,
                );
                draw_highlight(&painter, &xf, list, hl_infinite, t.selection, look);
            } else {
                draw_highlight(&painter, &xf, list, &prim_index.prims(*list_gen, list, &sel), t.selection, look);
            }
        }
    }
    app.canvas.draw_ms = crate::now_ms() - t0;
    // Both overlays draw world geometry: on the World plane whatever the construction plane.
    compare::paint(app, ui.ctx(), &painter, &xf_paper.world(), rect);
    blockedit::paint(app, ui.ctx(), &painter, &xf_paper, rect);
    // Constraint bars and dynamic dimensional constraints (model space, or inside a viewport).
    if sheet.is_none() || active_vp.is_some() {
        let clip = match &active_vp {
            Some(vp) => Rect::from_two_pos(
                xf_paper.to_screen(Vec2::new(vp.center.x - vp.width / 2.0, vp.center.y + vp.height / 2.0)),
                xf_paper.to_screen(Vec2::new(vp.center.x + vp.width / 2.0, vp.center.y - vp.height / 2.0)),
            )
            .intersect(rect),
            None => rect,
        };
        let p = painter.with_clip_rect(clip);
        crate::parametric::draw_overlay(app, ui, &p, &xf.world());
    }
    // Grips on selected objects (when idle or editing grips).
    let grip_edit = app.session.running.as_ref().is_some_and(|r| r.id == "grip");
    if (!grip_edit && (app.canvas.grip_editing || app.session.prompting())) || sel.is_empty() {
        app.canvas.hot_grips.clear();
    }
    if !grip_edit {
        app.canvas.dyn_hot = None;
    }
    app.canvas.grip_editing = grip_edit;
    if !grip_edit {
        app.canvas.grip_plane = None;
    }
    if (!app.session.prompting() || grip_edit)
        && app.session.shows_grips(sel.len())
        && let Ok(doc) = app.session.pick_doc()
    {
        let gs = app.session.settings.gripsize as f32;
        for h in sel.iter().take(200) {
            // A solid's grips are on the XY plane, which a 3D view shows away from the solid.
            if let Some(e) = doc.entity(*h).filter(|e| xf.is_plan() || !matches!(e.kind, cadcraft_doc::EntityKind::Solid3d(_))) {
                for (gi, sp) in grip_spots(e, &xf).into_iter().enumerate() {
                    if rect.contains(sp) {
                        let r = Rect::from_center_size(sp, vec2(gs * 2.0, gs * 2.0));
                        let hot = app.canvas.hot_grips.contains(&(*h, gi));
                        let warm = !hot && hover_pos.is_some_and(|hp| r.expand(2.0).contains(hp));
                        painter.rect_filled(
                            r,
                            0.0,
                            if hot {
                                t.grip_hot
                            } else if warm {
                                Color32::from_rgb(0xff, 0x7f, 0x9f)
                            } else {
                                t.grip
                            },
                        );
                        painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_rgb(0x10, 0x30, 0x80)), egui::StrokeKind::Middle);
                    }
                }
            }
        }
        crate::dyngrips::draw(app, &painter, &xf, rect, hover_pos);
    }
    gumball::paint(app, &painter, &xf, hover_pos.filter(|_| inside), bg);
    // Rubber band preview of the active command.
    // `xf` is the space being edited: paper space, or model space inside the active viewport.
    let drag = match (app.canvas.cursor, app.canvas.gpu.is_some()) {
        (Some(c), true) => app.session.drag(c),
        _ => None,
    };
    if let Some((drag, extra)) = drag {
        if let Ok(st) = app.session.state() {
            let serial = app.session.clipboard_source.as_ref().map_or(0, |c| c.serial);
            let transparency = app.session.settings.transparency_display;
            match solid_drag_preview(&st.doc, &drag, &app.session.clipboard, &xf) {
                Some(list) => draw_list(&painter, &xf, &list, bg, false),
                None => draw_drag_gpu(&mut app.canvas, &painter, &xf, &st.doc, &drag, (&app.session.clipboard, serial), bg, transparency, xf.dir),
            }
            let list = rebuilt_preview(&st.doc, placed(extra, &xf.cplane).iter(), scale, xf.dir);
            draw_list(&painter, &xf, &list, bg, false);
        }
    } else if let (Some(c), true) = (app.canvas.cursor, app.session.prompting()) {
        let ents = placed(lift_grip_band(app, &xf, c, app.session.preview(c)), &xf.cplane);
        if !ents.is_empty()
            && let Ok(st) = app.session.state()
        {
            let list = rebuilt_preview(&st.doc, ents.iter(), scale, xf.dir);
            draw_list(&painter, &xf, &list, bg, false);
        }
    }
    // Pending selection window: a crossing window when dragged to the left on screen.
    if let (Some(corner), Some(b)) = (window_corner(app, &xf), hover_pos.filter(|_| raw_world.is_some())) {
        let a = xf.view_to_screen(corner);
        let r = Rect::from_two_pos(a, b);
        let crossing = b.x < a.x;
        if let Some(fill) = selection_area_fill(&app.session.settings, crossing) {
            painter.rect_filled(r, 0.0, fill);
        }
        let st = Stroke::new(1.0, if crossing { t.crossing_stroke } else { t.window_stroke });
        if crossing {
            let pts = vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
            painter.extend(Shape::dashed_line(&pts, st, 5.0, 3.0));
        } else {
            painter.rect_stroke(r, 0.0, st, egui::StrokeKind::Middle);
        }
    }
    // A typed W, C, BOX, WP, CP or F: its outline through the points so far to the cursor.
    if let (Some(m), Some(b)) = (&app.session.select_mode, hover_pos.filter(|_| raw_world.is_some())) {
        draw_select_mode(&painter, &xf, m, b, &app.session.settings);
    }
    // Acquired tracking points and the polar / tracking paths under the cursor.
    if app.canvas.cursor.is_some() {
        draw_tracking(&painter, &xf, &app.session.tracking.acquired, app.canvas.track.as_ref(), view.height * 4.0);
    }
    if let (Some(hit), Some(_)) = (app.canvas.snap, app.canvas.cursor) {
        let at = app.canvas.snap_z.map_or_else(|| xf.to_screen(hit.point), |z| xf.local_to_screen(hit.point.to3(z)));
        draw_snap_marker(&painter, at, &hit);
    }
    draw_tiles(app, ui, &tiles);
    // UCS icon, ViewCube and viewport label.
    if app.session.settings.ucsicon {
        draw_ucs_icon(&painter, rect, &xf);
        crate::viewcube::ucs_icon(app, ui, rect, &xf);
    }
    if app.ui.show_viewcube && sheet.is_none() {
        draw_viewcube(app, ui, rect);
    }
    // The view's title (the Model tab's opens the viewport menu); the pointer over it is a hand.
    let mut over_title = false;
    if let Ok(st) = app.session.state() {
        let look = cadcraft_engine::TileLook { dir: xf.dir.parallel(), ..st.look() };
        let tile = tiles.iter().position(|(_, t)| t.active).map(|i| (i, true));
        over_title = tiles::title(app, ui, rect, tile, look);
    }
    // Typing and Escape reach the command line even when its overlay is hidden.
    crate::cmdline::keyboard(app, ui.ctx());
    // Command line overlay.
    if app.ui.show_command_line {
        crate::cmdline::show(app, ui, full);
    }
    // Crosshair cursor.
    if inside && !over_title {
        // Hide the OS cursor under the drawn crosshair, unless assistive tech needs it (#39).
        ui.ctx().set_cursor_icon(if app.ui.system_cursor { egui::CursorIcon::Crosshair } else { egui::CursorIcon::None });
        if let Some(hp) = hover_pos {
            draw_crosshair(app, &painter, rect, hp, selecting);
        }
        if app.session.settings.dynmode
            && let Some(hp) = hover_pos
        {
            // A stretched grip shows its dimensions instead of the pointer boxes.
            if !crate::grip_dyn::draw(app, &painter) {
                dynamic_input(app, &painter, hp);
            }
        }
    }
    // No continuous repaint while hovering: input events repaint by themselves. Repainting every
    // frame kept a vsync swapchain's queue full (every frame waited behind queued ones) and would
    // spin the CPU with the low-latency present mode (`gpu::surface_config`).
}

/// The model space tiles on screen (none without tiling): each tile's rectangle in `full`.
/// Start TEXTEDIT on the text, mtext or attribute definition at `p` (the objects its shortcut menu
/// offers Edit Text for); false when there is none.
fn edit_text_at(app: &mut CadApp, p: Vec2) -> bool {
    use cadcraft_engine::doc::EntityKind;
    let is_text = cadcraft_engine::cmd::curves::pick_at(&app.session, p).and_then(|h| {
        app.session.doc().ok()?.entity(h).map(|e| matches!(e.kind, EntityKind::Text(_) | EntityKind::MText(_) | EntityKind::AttDef(_)))
    });
    if is_text != Some(true) {
        return false;
    }
    app.start("textedit");
    app.session.point_z = None;
    if let Err(e) = app.session.input(Input::Point(p)) {
        app.session.echo(e.to_string());
    }
    true
}

fn tile_rects(app: &CadApp, full: Rect) -> Vec<(Rect, cadcraft_engine::ModelTile)> {
    let x = |f: f64| full.left() + full.width() * f.clamp(0.0, 1.0) as f32;
    let y = |f: f64| full.bottom() - full.height() * f.clamp(0.0, 1.0) as f32;
    app.session
        .model_tiles()
        .into_iter()
        .map(|t| (Rect::from_min_max(pos2(x(t.min.x), y(t.max.y)), pos2(x(t.max.x), y(t.min.y))).shrink(1.0), t))
        .collect()
}

/// The world ↔ screen transform of a model tile drawn in `r`, looking along `dir` (the
/// direction of the list it draws).
fn tile_xf(r: Rect, tile: &cadcraft_engine::ModelTile, dir: ViewDir) -> Xf {
    Xf::new(r, tile.view.center, f64::from(r.height()) / tile.view.height.max(1e-12)).looking(dir).on(tile.cplane)
}

/// The tiles other than the current one (each with its own view, look and grid, the
/// construction plane's in 3D, clipped to it), then the tile borders (thin, and heavy around the
/// current tile) and the other tiles' titles.
fn draw_tiles(app: &mut CadApp, ui: &egui::Ui, tiles: &[(Rect, cadcraft_engine::ModelTile)]) {
    let t = Tokens::get();
    let bg = Rgb(t.canvas.r(), t.canvas.g(), t.canvas.b());
    let others = || tiles.iter().enumerate().filter(|(_, (r, tile))| !tile.active && r.width() >= 1.0 && r.height() >= 1.0);
    for (_, (r, tile)) in others() {
        let p = ui.painter_at(*r);
        let (list, dir) = tiles::list_for(app, tile);
        let xf = tile_xf(*r, tile, dir);
        p.rect_filled(*r, 0.0, t.canvas);
        draw_grid(app, &p, &xf);
        if let Some(list) = list {
            draw_list(&p, &xf, list, bg, app.session.settings.lwdisplay);
        }
    }
    let p = ui.painter();
    for (r, tile) in tiles {
        let (w, c) = if tile.active { (2.0, t.accent) } else { (1.0, t.border) };
        p.rect_stroke(*r, 0.0, Stroke::new(w, c), egui::StrokeKind::Outside);
    }
    for (i, (r, tile)) in others() {
        tiles::title(app, ui, *r, Some((i, false)), tile.look);
    }
}

fn tooltip(p: &egui::Painter, at: Pos2, text: &str) {
    let galley = p.layout_no_wrap(text.to_string(), crate::theme::small(), Color32::from_rgb(0x20, 0x20, 0x20));
    let r = Rect::from_min_size(at, galley.size() + vec2(8.0, 4.0));
    p.rect_filled(r, 2.0, Color32::from_rgb(0xf4, 0xf4, 0xf4));
    p.galley(r.min + vec2(4.0, 2.0), galley, Color32::BLACK);
}

/// The hatch under world point `w` within the pick aperture (PICKBOX × 1.5 pixels at `scale`
/// pixels per unit), if any.
fn hatch_at(app: &CadApp, w: Vec2, scale: f64) -> Option<Handle> {
    let st = app.session.state().ok()?;
    // `w` is in the construction plane's coordinates: pick in the drawing as it sees it.
    let d = app.session.pick_doc().ok()?;
    let aperture = app.session.settings.pickbox.max(1.0) * 1.5 / scale.max(1e-12);
    let h = cadcraft_engine::select::pick(&d, &st.edit_space(), w, aperture)?;
    matches!(d.entity(h)?.kind, cadcraft_doc::EntityKind::Hatch(_)).then_some(h)
}

/// Half the side of the pick box: the pick aperture (PICKBOX × 1.5 pixels, as `select::pick` is
/// called with), so the square shows what a click hits. `None` when PICKBOX is 0 (no box).
/// The candidate Tab has cycled to, if cycling: a Tab press starts or advances it, and the
/// cursor leaving the aperture where it started ends it.
fn snap_cycle(app: &mut CadApp, raw: Vec2, ap: f64) -> Option<usize> {
    let tab = std::mem::take(&mut app.canvas.snap_tab);
    let c = &mut app.canvas.snap_cycle;
    if c.is_some_and(|c| c.anchor.dist(raw) > ap) {
        *c = None;
    }
    if tab {
        *c = Some(c.map_or(SnapCycle { anchor: raw, index: 1 }, |c| SnapCycle { index: c.index + 1, ..c }));
    }
    c.map(|c| c.index)
}

/// The cursor under a one-pick object snap override (`Session::snap_override`: END, NON…) or
/// while Tab cycles the candidates: the snap of that mode (or the cycled candidate) replaces
/// what the running snaps found; with none, a running snap no longer holds the cursor.
fn override_cursor(app: &mut CadApp, raw: Vec2, xf: &Xf, at: Pos2) {
    let over = app.session.snap_override();
    let ap = app.session.settings.aperture / xf.scale;
    let cycle = snap_cycle(app, raw, ap);
    let prompt = app.session.current_prompt();
    let wants_point = prompt.as_ref().is_some_and(cadcraft_engine::pointmod::accepts);
    if (over.is_none() && cycle.is_none()) || !wants_point || app.session.pending_window.is_some() {
        return;
    }
    let base = prompt.as_ref().and_then(|p| p.base);
    let deferred = prompt.as_ref().is_some_and(|p| p.deferred);
    let s = &app.session.settings;
    let osmode = over.unwrap_or(s.osmode) | if s.osnaphatch { snap::mode::HATCH } else { 0 };
    // In a 3D view solids and objects off the construction plane are snapped where they are
    // drawn (snap3d), never at their shadow on it.
    let osmode = osmode | if xf.is_plan() { 0 } else { snap::mode::NO_SOLIDS };
    // `raw` is in the construction plane's coordinates: snap in the drawing as it sees it.
    let Some(d) = snap3d::ground_doc(app, xf).or_else(|| app.session.pick_doc().ok()) else { return };
    let Ok(space) = app.session.state().map(|st| st.edit_space()) else { return };
    let hit = match cycle {
        Some(i) => {
            let cands = cadcraft_engine::pointmod::snap_candidates(&d, &space, raw, ap, osmode, base, deferred);
            // A 3D view also cycles through the snaps where objects are drawn.
            let all = snap3d::cycle_list(app, xf, at, cands);
            match all.get(i % all.len().max(1)) {
                Some(snap3d::Pick::World(p, m)) => return snap3d::set(app, xf, *p, *m),
                Some(snap3d::Pick::Plane(h)) => Some(*h),
                None => None,
            }
        }
        None => snap::osnap(&d, &space, raw, ap, osmode, base, deferred),
    };
    let p = match (hit, app.canvas.snap) {
        (Some(h), _) => {
            app.canvas.snap = Some(h);
            h.point
        }
        (None, Some(_)) => {
            app.canvas.snap = None;
            raw
        }
        (None, None) => return,
    };
    app.canvas.cursor = Some(p);
    app.session.cursor = p;
    app.session.cursor_deferred = app.canvas.snap.and_then(|h| h.deferred);
}

fn pickbox_half(pickbox: f64) -> Option<f32> {
    let half = (pickbox * 1.5) as f32;
    (half.is_finite() && half >= 1.0).then_some(half)
}

fn draw_crosshair(app: &CadApp, p: &egui::Painter, rect: Rect, at: Pos2, pickbox: bool) {
    let st = Stroke::new(1.0, Tokens::get().crosshair);
    let len = (rect.height() * app.session.settings.cursorsize as f32 / 100.0).max(12.0);
    let pb = if pickbox { pickbox_half(app.session.settings.pickbox) } else { None };
    let gap = pb.unwrap_or(0.0);
    // Along the isometric plane's axes in isometric drafting.
    if app.session.settings.isodraft {
        for a in cadcraft_engine::snap::iso::cursor_axes(&app.session.settings) {
            let dir = vec2(a.cos() as f32, -(a.sin() as f32));
            p.line_segment([at - dir * len, at - dir * gap], st);
            p.line_segment([at + dir * gap, at + dir * len], st);
        }
    } else {
        p.line_segment([pos2(at.x - len, at.y), pos2(at.x - gap, at.y)], st);
        p.line_segment([pos2(at.x + gap, at.y), pos2(at.x + len, at.y)], st);
        p.line_segment([pos2(at.x, at.y - len), pos2(at.x, at.y - gap)], st);
        p.line_segment([pos2(at.x, at.y + gap), pos2(at.x, at.y + len)], st);
    }
    if let Some(pb) = pb {
        p.rect_stroke(Rect::from_center_size(at, vec2(pb * 2.0, pb * 2.0)), 0.0, st, egui::StrokeKind::Middle);
    }
}

/// The Dynamic Input tooltip beside the cursor, styled like the command line. At a point prompt
/// it shows the pair of value boxes, a view of the command-line text (see [`crate::dyninput`]);
/// otherwise the prompt with what is being typed, or the live distance and angle.
fn dynamic_input(app: &mut CadApp, p: &egui::Painter, hover: Pos2) {
    app.cmd.option_rows.clear();
    // While the options menu is open the tooltip stays where it opened, so the menu can be reached.
    let at = match app.cmd.options {
        Some(_) => *app.cmd.options_anchor.get_or_insert(hover),
        None => {
            app.cmd.options_anchor = None;
            hover
        }
    };
    let Some(prompt) = app.session.current_prompt() else { return };
    // DYNPROMPT = 0: no prompt beside the cursor (the value boxes stay).
    let show_prompt = app.session.settings.dynprompt;
    let options = show_prompt && !prompt.keywords.is_empty();
    if let Some(f) = crate::cmdline::dyn_frame(app)
        && let Some(e) = crate::dyninput::parse(&app.cmd.buffer, &f)
        && let Some(c) = app.canvas.cursor
    {
        let message = if show_prompt { crate::i18n::line(&prompt.message) } else { String::new() };
        let r = dynamic_input_boxes(p, at, &message, &f, &e, c, app.cmd.second_locked(), options, length_units(&app.session));
        options_menu(app, p, r, hover);
        return;
    }
    if !show_prompt {
        return;
    }
    // Prompts built with format! translate as the command line's do.
    let message = crate::i18n::line(&prompt.message);
    let mut text = message.clone();
    if !app.cmd.buffer.is_empty() {
        text = format!("{message}: {}", app.cmd.buffer);
    } else if let (Some(base), Some(c)) = (prompt.base, app.canvas.cursor) {
        text = format!("{message}   {}  <  {}°", distance_text(&app.session, base.dist(c)), (base.angle_to(c).to_degrees() * 10.0).round() / 10.0);
    } else if let Some(c) = app.canvas.cursor {
        text = format!("{message}   {}, {}", distance_text(&app.session, c.x), distance_text(&app.session, c.y));
    }
    let t = Tokens::get();
    let galley = p.layout_no_wrap(text, crate::theme::small(), t.text);
    let hint = if options { OPTIONS_HINT_W } else { 0.0 };
    let r = tooltip_frame(p, at, galley.size() + vec2(12.0 + hint, 8.0));
    p.galley(r.min + vec2(6.0, 4.0), galley, t.text);
    if options {
        options_hint(p, r);
    }
    options_menu(app, p, r, hover);
}

/// The drawing's length units: (LUNITS, LUPREC).
pub(crate) fn length_units(session: &cadcraft_engine::Session) -> (i64, i64) {
    session.doc().map(|d| (d.header.i64("LUNITS", 2), d.header.i64("LUPREC", 4))).unwrap_or((2, 4))
}

/// A distance in the drawing's length units.
fn distance_text(session: &cadcraft_engine::Session, v: f64) -> String {
    let (lu, lp) = length_units(session);
    cadcraft_engine::units::format_distance(v, lu, lp)
}

/// Room for the down-arrow sign that a prompt has options.
const OPTIONS_HINT_W: f32 = 14.0;

/// The down-arrow sign at the right end of the tooltip `r`: the prompt has options, and Down
/// Arrow lists them.
fn options_hint(p: &egui::Painter, r: Rect) {
    let t = Tokens::get();
    let c = pos2(r.right() - OPTIONS_HINT_W / 2.0 - 2.0, r.center().y + 1.0);
    p.add(Shape::convex_polygon(vec![pos2(c.x - 4.0, c.y - 2.5), pos2(c.x + 4.0, c.y - 2.5), pos2(c.x, c.y + 2.5)], t.text_dim, Stroke::NONE));
}

/// The options menu under the tooltip `r` (DYNPROMPT): one row per keyword, the highlighted one
/// marked; the rows are kept for clicks.
fn options_menu(app: &mut CadApp, p: &egui::Painter, r: Rect, hover: Pos2) {
    let Some(mut menu) = app.cmd.options.clone() else { return };
    let t = Tokens::get();
    let font = crate::theme::small();
    let rows: Vec<_> = menu.keywords.iter().map(|k| p.layout_no_wrap(k.clone(), font.clone(), t.text)).collect();
    let row_h = rows.iter().map(|g| g.size().y).fold(12.0, f32::max) + 6.0;
    let w = rows.iter().map(|g| g.size().x).fold(60.0, f32::max) + 28.0;
    let h = row_h * rows.len() as f32 + 4.0;
    let m = Rect::from_min_size(pos2(r.left(), r.bottom() + 2.0), vec2(w, h));
    p.rect_filled(m, 3.0, t.cmd_bg);
    p.rect_stroke(m, 3.0, Stroke::new(1.0, t.cmd_border), egui::StrokeKind::Inside);
    let row_at = |i: usize| Rect::from_min_size(pos2(m.left() + 2.0, m.top() + 2.0 + row_h * i as f32), vec2(w - 4.0, row_h));
    // The row under the pointer is the highlighted one.
    if let Some(i) = (0..menu.keywords.len()).find(|i| row_at(*i).contains(hover)) {
        menu.index = i;
        app.cmd.options = Some(menu.clone());
    }
    for (i, (g, k)) in rows.into_iter().zip(&menu.keywords).enumerate() {
        let row = row_at(i);
        if i == menu.index {
            p.rect_filled(row, 2.0, t.cmd_keyword_hover);
            p.circle_filled(pos2(row.left() + 9.0, row.center().y), 2.5, t.accent);
        }
        p.galley(pos2(row.left() + 20.0, row.center().y - g.size().y / 2.0), g, t.text);
        app.cmd.option_rows.push((row, k.clone()));
    }
}

/// The tooltip background, matching the command line bar; returns its rectangle.
fn tooltip_frame(p: &egui::Painter, at: Pos2, size: egui::Vec2) -> Rect {
    let t = Tokens::get();
    let r = Rect::from_min_size(at + vec2(18.0, -size.y - 14.0), size);
    p.rect_filled(r, 3.0, t.cmd_bg);
    p.rect_stroke(r, 3.0, Stroke::new(1.0, t.cmd_border), egui::StrokeKind::Inside);
    r
}

/// The Dynamic Input value boxes: X and Y, or distance and angle. Typed values show in full
/// text colour, a locked value (a separator typed after it) gets a lock, the active box gets the
/// accent outline, and open boxes show the cursor's live value, dimmed. Values measured from the
/// last point are marked with `@`.
#[allow(clippy::too_many_arguments)]
fn dynamic_input_boxes(
    p: &egui::Painter,
    at: Pos2,
    message: &str,
    f: &crate::dyninput::Frame,
    e: &crate::dyninput::Entry<'_>,
    cursor: Vec2,
    second_locked: bool,
    options: bool,
    (lu, lp): (i64, i64),
) -> Rect {
    use crate::dyninput::Format;
    let t = Tokens::get();
    let font = crate::theme::small();
    let dist = |v: f64| cadcraft_engine::units::format_distance(v, lu, lp);
    let (live_first, live_second) = f.live(e.format, cursor);
    let live_second = match e.format {
        Format::Cartesian => dist(live_second),
        Format::Polar => format!("{}°", (live_second.to_degrees() * 10.0).round() / 10.0),
    };
    let (label_first, label_second) = match e.format {
        Format::Cartesian => ("X", "Y"),
        Format::Polar => ("", "<"),
    };
    // (label, text, typed, locked, active)
    let cells = [
        (
            label_first,
            if e.first.is_empty() { dist(live_first) } else { e.first.to_owned() },
            !e.first.is_empty(),
            e.first_locked().is_some(),
            !e.split,
        ),
        (
            label_second,
            if e.second.is_empty() { live_second } else { e.second.to_owned() },
            !e.second.is_empty(),
            second_locked,
            e.split && !second_locked,
        ),
    ];

    let msg = p.layout_no_wrap(message.to_owned(), font.clone(), t.text_dim);
    let marker = p.layout_no_wrap(if f.relative() { "@".to_owned() } else { String::new() }, font.clone(), t.text_faint);
    let lock_w = 10.0;
    let laid: Vec<_> = cells
        .into_iter()
        .map(|(label, text, typed, locked, active)| {
            let label = p.layout_no_wrap(label.to_owned(), font.clone(), t.text_faint);
            let value = p.layout_no_wrap(text, font.clone(), if typed { t.text } else { t.text_faint });
            let w = (value.size().x + 10.0 + if locked { lock_w } else { 0.0 }).max(48.0);
            (label, value, w, locked, active)
        })
        .collect();

    // Measure, then draw.
    let pad = 6.0;
    let gap = 6.0;
    let box_h = msg.size().y + 6.0;
    let mut w = pad + msg.size().x + gap + marker.size().x;
    for (label, _, bw, _, _) in &laid {
        w += gap + label.size().x + 3.0 + bw;
    }
    let hint = if options { OPTIONS_HINT_W } else { 0.0 };
    let r = tooltip_frame(p, at, vec2(w + pad + hint, box_h + 2.0 * pad - 2.0));
    if options {
        options_hint(p, r);
    }
    let cy = r.center().y;
    let mut x = r.left() + pad;
    let (msg_w, msg_h) = (msg.size().x, msg.size().y);
    p.galley(pos2(x, cy - msg_h / 2.0), msg, t.text_dim);
    x += msg_w + gap;
    let mw = marker.size().x;
    p.galley(pos2(x, cy - marker.size().y / 2.0), marker, t.text_faint);
    x += mw;
    for (label, value, bw, locked, active) in laid {
        x += gap;
        let lw = label.size().x;
        p.galley(pos2(x, cy - label.size().y / 2.0), label, t.text_faint);
        x += lw + 3.0;
        let b = Rect::from_min_size(pos2(x, cy - box_h / 2.0), vec2(bw, box_h));
        p.rect_filled(b, 2.0, t.chrome_dark);
        let stroke = if active { Stroke::new(1.5, t.accent) } else { Stroke::new(1.0, t.border) };
        p.rect_stroke(b, 2.0, stroke, egui::StrokeKind::Inside);
        let vw = value.size().x;
        p.galley(pos2(b.left() + 5.0, cy - value.size().y / 2.0), value, t.text);
        if active {
            let cx = b.left() + 6.0 + vw;
            p.line_segment([pos2(cx, b.top() + 3.0), pos2(cx, b.bottom() - 3.0)], Stroke::new(1.0, t.text));
        }
        if locked {
            draw_lock(p, pos2(b.right() - lock_w / 2.0 - 3.0, cy), t.accent);
        }
        x += bw;
    }
    r
}

/// A small padlock, drawn in code: body plus shackle.
fn draw_lock(p: &egui::Painter, c: Pos2, color: Color32) {
    p.rect_filled(Rect::from_center_size(c + vec2(0.0, 1.5), vec2(7.0, 5.0)), 1.0, color);
    p.add(Shape::line(
        vec![pos2(c.x - 2.0, c.y - 1.0), pos2(c.x - 2.0, c.y - 4.0), pos2(c.x + 2.0, c.y - 4.0), pos2(c.x + 2.0, c.y - 1.0)],
        Stroke::new(1.2, color),
    ));
}

/// The UCS icon in canvas `rect` for view `xf`: its origin and its arms (label, tip). In plan,
/// AutoCAD's 2D icon: X and Y along the construction plane's axes as plan sees them (World:
/// right and up); in a 3D view, with room all round, the plane's X, Y and Z axes as the view sees
/// them (an axis seen end-on is a dot). [`crate::viewcube::ucs_icon`] makes it clickable there.
pub(crate) fn ucs_icon_arms(rect: Rect, xf: &Xf) -> (Pos2, Vec<(&'static str, Pos2)>) {
    use cadcraft_geom::Vec3;
    let (o, len, n) = if xf.is_plan() {
        (pos2(rect.left() + 36.0, rect.bottom() - 34.0), 60.0, 2)
    } else {
        (pos2(rect.left() + 70.0, rect.bottom() - 70.0), 50.0, 3)
    };
    let axes = [(crate::tl!("X"), Vec3::X), (crate::tl!("Y"), Vec3::Y), (crate::tl!("Z"), Vec3::Z)];
    let arms = axes
        .into_iter()
        .take(n)
        .map(|(label, axis)| {
            let v = xf.dir.parallel().project(xf.cplane.apply_vec(axis).normalized());
            (label, o + vec2(v.x as f32, -v.y as f32) * len)
        })
        .collect();
    (o, arms)
}

fn draw_ucs_icon(p: &egui::Painter, rect: Rect, xf: &Xf) {
    let t = Tokens::get();
    let (o, arms) = ucs_icon_arms(rect, xf);
    let f = egui::FontId::proportional(13.0);
    if !xf.is_plan() {
        // A 3D view: x red, y green and the plane's normal blue.
        for ((label, tip), color) in arms.into_iter().zip(t.gumball) {
            p.line_segment([o, tip], Stroke::new(1.5, color));
            p.text(o + (tip - o) * 1.25, egui::Align2::CENTER_CENTER, label, f.clone(), color);
        }
        return;
    }
    let st = Stroke::new(1.0, t.canvas_ink);
    let world = xf.cplane == Mat4::IDENTITY;
    for (i, (label, tip)) in arms.into_iter().enumerate() {
        p.line_segment([o, tip], st);
        // World: the labels beside the arms' ends; a UCS turns them with the arms.
        let (at, align) = match (world, i) {
            (true, 0) => (o + vec2(70.0, 0.0), egui::Align2::LEFT_CENTER),
            (true, _) => (o + vec2(0.0, -70.0), egui::Align2::CENTER_BOTTOM),
            _ => (o + (tip - o) * 1.2, egui::Align2::CENTER_CENTER),
        };
        p.text(at, align, label, f.clone(), t.canvas_ink);
    }
    // The square marks the World Coordinate System, as AutoCAD's icon does.
    if world {
        p.rect_stroke(Rect::from_center_size(o, vec2(9.0, 9.0)), 0.0, st, egui::StrokeKind::Middle);
    }
}

fn draw_viewcube(app: &mut CadApp, ui: &mut egui::Ui, rect: Rect) {
    let l = crate::viewcube::Layout::new(rect);
    let (c, ring) = (l.center, l.ring);
    let p = ui.painter_at(rect);
    crate::viewcube::show(app, ui, &l);
    // WCS pill.
    let pill = Rect::from_center_size(c + vec2(0.0, ring + 26.0), vec2(56.0, 16.0));
    p.rect_filled(pill, 8.0, Color32::from_rgb(0x48, 0x50, 0x5c));
    p.text(pill.center(), egui::Align2::CENTER_CENTER, crate::tl!("WCS ⌄"), crate::theme::small(), Tokens::DARK.canvas_ink);
}

/// A click on grip `g` (idle): Shift+click makes it hot or cold again; a plain click starts
/// editing it, with the other hot grips when it is one of them.
fn click_grip(app: &mut CadApp, g: (Handle, usize), shift: bool) {
    let hot = &mut app.canvas.hot_grips;
    if shift {
        match hot.iter().position(|x| *x == g) {
            Some(i) => {
                hot.remove(i);
            }
            None => hot.push(g),
        }
        return;
    }
    if !hot.contains(&g) {
        hot.clear();
    }
    hot.retain(|x| *x != g);
    hot.insert(0, g);
    let hs = hot.clone();
    match app.session.start_grip_edit(&hs) {
        Ok(()) => app.canvas.grip_plane = app.canvas.xf.and_then(|xf| grip_plane(app, &xf, g)),
        Err(e) => {
            app.canvas.hot_grips.clear();
            app.session.echo(e.to_string());
        }
    }
}

/// The multifunctional options of grip `g` (Add Vertex, Lengthen…; empty: Stretch only).
fn grip_options(app: &CadApp, (h, i): (Handle, usize)) -> Vec<cadcraft_engine::grips::GripAction> {
    let Ok(d) = app.session.pick_doc() else { return Vec::new() };
    d.entity(h).map(|e| cadcraft_engine::grips::grip_actions(&e.kind, i)).unwrap_or_default()
}

/// The menu of a multifunctional grip: Stretch and its options. A choice makes the grip hot and
/// runs the option at the grip prompt, as typing its keyword would.
fn grip_menu(app: &mut CadApp, resp: &egui::Response) {
    let Some(g) = app.canvas.grip_menu else { return };
    let options = grip_options(app, g);
    let mut chosen: Option<Option<cadcraft_engine::grips::GripAction>> = None;
    let open = resp
        .context_menu(|ui| {
            let vertex = options.contains(&cadcraft_engine::grips::GripAction::RemoveVertex);
            if ui.button(crate::i18n::t(if vertex { "Stretch Vertex" } else { "Stretch" })).clicked() {
                chosen = Some(None);
                ui.close();
            }
            for a in &options {
                if ui.button(crate::i18n::t(a.label())).clicked() {
                    chosen = Some(Some(*a));
                    ui.close();
                }
            }
        })
        .is_some();
    if !open || chosen.is_some() {
        app.canvas.grip_menu = None;
    }
    let Some(choice) = chosen else { return };
    app.canvas.hot_grips = vec![g];
    app.canvas.grip_plane = app.canvas.xf.and_then(|xf| grip_plane(app, &xf, g));
    let r = app.session.start_grip_edit(&[g]).and_then(|()| match choice {
        Some(a) => app.session.input(Input::Keyword(a.keyword().to_string())),
        None => Ok(()),
    });
    if let Err(e) = r {
        app.canvas.hot_grips.clear();
        app.canvas.grip_plane = None;
        app.session.echo(e.to_string());
    }
}

/// Where `xf` shows the grips of object `e`, as the construction plane sees it
/// ([`cadcraft_engine::Session::pick_doc`]): the engine's grip points and indices
/// ([`cadcraft_engine::Session::grips_of`], what grip edits start from), placed through the plane
/// at their height above it, so a raised or tilted object's grips are where it is drawn (not on
/// its shadow) and the grip under the cursor is the one the engine edits. Plan view with the
/// World CPlane: AutoCAD's own, where plan draws them.
/// A grip behind a perspective view's eye is nowhere (NaN: not drawn, never under the cursor).
fn grip_spots(e: &cadcraft_doc::Entity, xf: &Xf) -> Vec<Pos2> {
    let g = e.grips();
    if xf.flat() {
        return g.into_iter().map(|p| xf.to_screen(p)).collect();
    }
    g.iter()
        .zip(grip_heights(e, g.len()))
        .map(|(p, z)| {
            let w = xf.cplane.apply(p.to3(z));
            if xf.dir.sees(w) { xf.world_to_screen(w) } else { Pos2::new(f32::NAN, f32::NAN) }
        })
        .collect()
}

/// The heights above the construction plane of the `n` grips of object `e` (in the plane's
/// coordinates): the z of its 3D grips ([`snap3d::grips3`], in the same order).
fn grip_heights(e: &cadcraft_doc::Entity, n: usize) -> Vec<f64> {
    let g3 = snap3d::grips3(e);
    if g3.len() != n {
        return vec![0.0; n];
    }
    g3.into_iter().map(|q| if q.z.is_finite() { q.z } else { 0.0 }).collect()
}

/// The plane a grip edit in view `xf` moves grip `(h, i)` in ([`drag_plane`]), so the grip lands
/// under the cursor: a tilted object's own plane (the engine takes the CPlane x, y it is given
/// into that plane, `Entity::plan_to_local`), else level with the CPlane through the grip (the
/// engine keeps grip heights). While the edit transforms its objects instead, the level one
/// ([`grip_drag_plane`]).
/// A click during the drag sends the point's height in that plane too ([`grip_height`]).
fn grip_plane(app: &CadApp, xf: &Xf, (h, i): (Handle, usize)) -> Option<(cadcraft_geom::Vec3, cadcraft_geom::Vec3)> {
    if !xf.picks_drawn() {
        return None;
    }
    let d = app.session.pick_doc().ok()?;
    let e = d.entity(h)?;
    let g = e.grips();
    let z = grip_heights(e, g.len()).get(i).copied()?;
    let tilted = e.plan_xf().map(|_| xf.cplane.apply_vec(e.common.extrusion));
    drag_plane(xf, xf.cplane.apply(g.get(i)?.to3(z)), tilted)
}

/// The plane (world point, normal) a grip at world point `p` moves in under the cursor in view
/// `xf`: its object's plane, normal `tilted` (one not level with the construction plane), unless
/// the view sees that nearly edge-on; else the plane through `p` level with the CPlane when `p` is
/// off it. `None` in plan with a level CPlane (the view ray meets every level plane at the same
/// x, y), and for a grip on the CPlane (points land on it).
pub(crate) fn drag_plane(xf: &Xf, p: cadcraft_geom::Vec3, tilted: Option<cadcraft_geom::Vec3>) -> Option<(cadcraft_geom::Vec3, cadcraft_geom::Vec3)> {
    if !xf.picks_drawn() {
        return None;
    }
    if let Some(n) = tilted.map(|n| n.normalized()).filter(|n| n.dot(xf.dir.axes().2).abs() > 0.1) {
        return Some((p, n));
    }
    let z = xf.cplane.inverse()?.apply(p).z;
    (z.abs() > 1e-9 * (1.0 + p.len())).then(|| (p, xf.cplane.apply_vec(cadcraft_geom::Vec3::Z)))
}

/// The plane the cursor moves the dragged grip in now: the one its drag began with
/// ([`CanvasState::grip_plane`]) while the edit moves the grip on its object (STRETCH, a
/// multifunctional option, a dynamic grip); level with the CPlane through the grip while it moves,
/// rotates, scales or mirrors the objects in the construction plane (the engine keeps their
/// heights), and at those modes' base and reference points.
fn grip_drag_plane(app: &CadApp, xf: &Xf) -> Option<(cadcraft_geom::Vec3, cadcraft_geom::Vec3)> {
    let (o, n) = app.canvas.grip_plane?;
    let on_object = app
        .session
        .current_prompt()
        .is_some_and(|p| p.message.starts_with("** ") && !["** MOVE", "** ROTATE", "** SCALE", "** MIRROR"].iter().any(|m| p.message.starts_with(m)));
    if on_object { Some((o, n)) } else { drag_plane(xf, o, None) }
}

/// A grip edit's rubber band (the engine's line from the base grip to the cursor `c`, the last of
/// the preview `ents` in CPlane coordinates, on the CPlane) in the plane the grip moves in
/// ([`grip_drag_plane`]): from the grip where it is drawn to where it lands, not on its shadow. For a
/// plane square to the CPlane, at the grip's height (where the engine puts it).
/// ponytail: a tilted object's stretched or moved shape still previews flat on the CPlane (the
/// engine's grip preview is its plan view); upgrade: the engine previews it in its own plane.
fn lift_grip_band(app: &CadApp, xf: &Xf, c: Vec2, mut ents: Vec<cadcraft_doc::Entity>) -> Vec<cadcraft_doc::Entity> {
    let Some((o, n)) = grip_drag_plane(app, xf) else { return ents };
    let Some(ci) = xf.cplane.inverse() else { return ents };
    let (o, n) = (ci.apply(o), ci.apply_vec(n));
    let z = |q: Vec2| {
        let h = o.z - (n.x * (q.x - o.x) + n.y * (q.y - o.y)) / n.z;
        if n.z.abs() > 1e-9 * n.len() && h.is_finite() { h } else { o.z }
    };
    if let Some(cadcraft_doc::EntityKind::Line(l)) = ents.last_mut().map(|e| &mut e.kind) {
        // Within rounding: a drag run in World hands its band back through the plane.
        let tol = 1e-9 * (1.0 + c.len() + l.a.xy().len());
        if l.b.xy().near(c, tol) && l.a.z.abs() <= tol && l.b.z.abs() <= tol {
            l.a.z = z(l.a.xy());
            l.b.z = z(c);
        }
    }
    ents
}

/// The drawing point under screen point `at`: on the construction plane ([`Xf::to_world`]), or
/// while a grip is dragged in a plane of its own, on that plane ([`grip_drag_plane`]), so the
/// grip follows the cursor.
fn point_at(app: &CadApp, xf: &Xf, at: Pos2) -> Vec2 {
    if let Some((o, n)) = grip_drag_plane(app, xf)
        && let Some(w) = xf.ray_on(at, o, n)
        && let Some(ci) = xf.cplane.inverse()
    {
        let p = ci.apply(w).xy();
        if p.is_finite() {
            return p;
        }
    }
    xf.to_world(at)
}

/// The height above the construction plane of the point under screen point `at` in the plane a
/// grip is being dragged in ([`grip_drag_plane`]), so an object standing square to the CPlane
/// stretches in its own plane, up and down too. `None` without such a drag.
fn grip_height(app: &CadApp, xf: &Xf, at: Pos2) -> Option<f64> {
    let (o, n) = grip_drag_plane(app, xf)?;
    let z = xf.cplane.inverse()?.apply(xf.ray_on(at, o, n)?).z;
    z.is_finite().then_some(z)
}

/// The grip of a selected object under the cursor, if any: (object, grip index).
fn grip_at(app: &CadApp, xf: &Xf, hover: Option<Pos2>) -> Option<(Handle, usize)> {
    let hp = hover?;
    let doc = app.session.pick_doc().ok()?;
    let gs = app.session.settings.gripsize as f32 + 3.0;
    let sel = app.session.selection();
    if !app.session.shows_grips(sel.len()) {
        return None;
    }
    for h in sel.iter().take(200) {
        // An object the construction plane can't hold has no grips there (the engine can't edit
        // it in that plane).
        let Some(e) = doc.entity(*h) else { continue };
        // No solid grips in a 3D view (see `show`).
        if doc.layer(&e.common.layer).is_some_and(|l| l.locked) || (!xf.is_plan() && matches!(e.kind, cadcraft_doc::EntityKind::Solid3d(_))) {
            continue;
        }
        for (i, sp) in grip_spots(e, xf).into_iter().enumerate() {
            if (sp.x - hp.x).abs() <= gs && (sp.y - hp.y).abs() <= gs {
                return Some((*h, i));
            }
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;
    use cadcraft_render::DPrim;

    fn list(handles: &[u64]) -> DisplayList {
        let prim =
            |h| DPrim { handle: Handle(h), color: Rgb(255, 255, 255), aci7: true, lw: 0.0, transparency: 0, kind: Kind::Polyline, start: 0, len: 0 };
        DisplayList { prims: handles.iter().map(|&h| prim(h)).collect(), ..Default::default() }
    }

    fn hs(v: &[u64]) -> Vec<Handle> {
        v.iter().map(|&h| Handle(h)).collect()
    }

    /// A dragged object keeps its transparency in the GPU preview when TRANSPARENCYDISPLAY is
    /// on, and is opaque when it is off.
    #[test]
    fn dragged_objects_keep_their_transparency() {
        let mut d = cadcraft_doc::Drawing::new_metric();
        let common = cadcraft_doc::Common { transparency: cadcraft_doc::Transparency::Percent(50), ..Default::default() };
        let line = cadcraft_doc::Line { a: cadcraft_geom::Vec3::ZERO, b: cadcraft_geom::Vec3::new(10.0, 0.0, 0.0) };
        let h = d.add(&cadcraft_doc::Space::Model, common, cadcraft_doc::EntityKind::Line(line)).unwrap();
        let drag = cadcraft_engine::Drag { handles: vec![h], matrix: cadcraft_geom::Mat3::IDENTITY, extra: Vec::new(), clipboard: false };
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(200.0, 100.0));
        let xf = Xf::new(rect, Vec2::new(5.0, 0.0), 10.0);
        let painter = egui::Painter::new(egui::Context::default(), egui::LayerId::background(), rect);
        // One canvas across the toggle: the cached drag mesh must be rebuilt.
        let mut c = CanvasState::default();
        let mut alpha = |transparency: bool| {
            draw_drag_gpu(&mut c, &painter, &xf, &d, &drag, (&[], 0), Rgb(0, 0, 0), transparency, ViewDir::PLAN);
            let mesh = c.drag_slot.lock().unwrap().take().unwrap();
            // The first line vertex's colour alpha (after its 2 × f32 position).
            mesh.lines[11]
        };
        assert_eq!(alpha(true), 128);
        assert_eq!(alpha(false), 255);
        assert_eq!(alpha(true), 128);
    }

    /// Previews the session rebuilds with the current properties (the rubber band, copies of
    /// objects) stay opaque even when CLAYER is transparent: they were opaque before transparency
    /// was drawn, and CLAYER's transparency isn't the objects'.
    #[test]
    fn rebuilt_previews_stay_opaque_on_a_transparent_current_layer() {
        let mut s = cadcraft_engine::Session::new();
        let d = s.doc_mut().unwrap();
        d.layers.push(cadcraft_doc::Layer { name: "Glass".into(), transparency: 40, ..Default::default() });
        d.header.set_str("CLAYER", "Glass");
        assert!(s.settings.transparency_display);
        // LINE: the segment from the first point to the cursor.
        s.cmdline("line").unwrap();
        s.cmdline("0,0").unwrap();
        let ents = s.preview(Vec2::new(10.0, 5.0));
        assert!(!ents.is_empty());
        assert_eq!(ents[0].common.layer, "Glass");
        let doc = s.doc().unwrap();
        // Built with transparency, the preview would take CLAYER's 40 %.
        let with = cadcraft_render::build_entities(
            doc,
            ents.iter(),
            &cadcraft_render::Options { transparency: true, ..preview_options(1.0, ViewDir::PLAN) },
        );
        assert!(with.prims.iter().all(|p| p.transparency == 40));
        let list = rebuilt_preview(doc, ents.iter(), 1.0, ViewDir::PLAN);
        assert!(!list.prims.is_empty() && list.prims.iter().all(|p| p.transparency == 0));
        s.cancel();
        // MOVE's base-to-cursor rubber band (a drag extra).
        s.execute("line", &serde_json::json!({"points": [[0, 0], [10, 0]]})).unwrap();
        s.cmdline("move").unwrap();
        s.cmdline("all").unwrap();
        s.cmdline("").unwrap();
        s.cmdline("0,0").unwrap();
        let (_, extra) = s.drag(Vec2::new(4.0, 4.0)).unwrap();
        assert!(!extra.is_empty());
        let list = rebuilt_preview(s.doc().unwrap(), extra.iter(), 1.0, ViewDir::PLAN);
        assert!(!list.prims.is_empty() && list.prims.iter().all(|p| p.transparency == 0));
    }

    #[test]
    fn viewport_highlight_tolerance_is_constant_within_a_zoom_band() {
        assert_eq!(viewport_highlight_tolerance(4.0), viewport_highlight_tolerance(3.0));
        assert_ne!(viewport_highlight_tolerance(4.0), viewport_highlight_tolerance(1.5));
        for s in [0.3, 1.0, 3.0, 4.0, 250.0] {
            assert!(viewport_highlight_tolerance(s) <= 0.5 / s);
        }
    }

    #[test]
    fn selection_area_fill_follows_the_area_effect_settings() {
        let mut s = cadcraft_engine::Settings::default();
        // Window: ACI 150 at 25 %; crossing: ACI 100.
        let rgb = |c: Rgb| Color32::from_rgb(c.0, c.1, c.2);
        assert_eq!(selection_area_fill(&s, false), Some(rgb(cadcraft_color::aci_rgb(150)).gamma_multiply(0.25)));
        assert_eq!(selection_area_fill(&s, true), Some(rgb(cadcraft_color::aci_rgb(100)).gamma_multiply(0.25)));
        s.selectionareaopacity = 100;
        assert_eq!(selection_area_fill(&s, false), Some(rgb(cadcraft_color::aci_rgb(150))));
        s.selectionarea = false;
        assert_eq!(selection_area_fill(&s, false), None, "Indicate selection area off: no fill");
    }

    #[test]
    fn viewport_highlight_is_tessellated_once_per_selection_and_zoom_band() {
        use cadcraft_doc::{Circle, Common, Drawing, EntityKind, Space};
        let mut d = Drawing::new_imperial();
        let circle = |r| EntityKind::Circle(Circle { center: cadcraft_geom::Vec3::new(0.0, 0.0, 0.0), radius: r });
        let a = d.add(&Space::Model, Common::default(), circle(10.0)).unwrap();
        let b = d.add(&Space::Model, Common::default(), circle(5.0)).unwrap();
        let st = cadcraft_engine::DocState::new(d, "highlight", None);
        let points = |l: &DisplayList| l.prims.iter().map(|p| l.points(p).len()).sum::<usize>();
        let (mut sel, mut hover) = (VpHighlight::default(), VpHighlight::default());
        sel.update(&st, &[a], 10.0);
        let (first, segments) = (sel.list_gen, points(&sel.list));
        assert!(!sel.list.prims.is_empty());
        // Every frame of a pan, zooming within the 2x band, and the hover moving: the same list.
        sel.update(&st, &[a], 10.0);
        sel.update(&st, &[a], 12.0);
        hover.update(&st, &[b], 12.0);
        hover.update(&st, &[a], 12.0);
        assert_eq!(sel.list_gen, first);
        // Zooming in past the band tessellates finer.
        sel.update(&st, &[a], 40.0);
        assert_ne!(sel.list_gen, first);
        assert!(points(&sel.list) > segments);
        // A different selection is rebuilt.
        let zoomed = sel.list_gen;
        sel.update(&st, &[a, b], 40.0);
        assert_ne!(sel.list_gen, zoomed);
        assert_eq!(sel.list.prims.iter().map(|p| p.handle).collect::<std::collections::BTreeSet<_>>().len(), 2);
    }

    #[test]
    fn prim_index_matches_a_scan_and_follows_list_rebuilds() {
        let a = list(&[3, 1, 3, 2, 7, 3]);
        let mut ix = PrimIndex::default();
        assert_eq!(ix.prims(1, &a, &hs(&[3])), vec![0, 2, 5]);
        assert_eq!(ix.prims(1, &a, &hs(&[3])), prims_of(&a, &hs(&[3])));
        assert_eq!(ix.prims(1, &a, &hs(&[2, 1, 2])), vec![1, 3]);
        assert!(ix.prims(1, &a, &hs(&[9])).is_empty());
        assert!(ix.prims(1, &a, &[]).is_empty());
        // A rebuilt list (new generation) is re-indexed.
        let b = list(&[3, 9]);
        assert_eq!(ix.prims(2, &b, &hs(&[9, 3])), vec![0, 1]);
    }

    fn wheel(unit: egui::MouseWheelUnit, dy: f32, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::MouseWheel { unit, delta: vec2(0.0, dy), phase: egui::TouchPhase::Move, modifiers }
    }

    /// Wheel events convert to points as egui does; ⌘/Ctrl (egui's pinch zoom) and ⇧ (a sideways
    /// scroll) don't zoom here, and every notch of the frame counts (#660).
    #[test]
    fn wheel_zoom_reads_every_notch_of_the_frame() {
        use egui::{Modifiers, MouseWheelUnit as U};
        let o = egui::InputOptions::default();
        let none = Modifiers::NONE;
        assert_eq!(wheel_zoom_points(&[wheel(U::Line, 1.0, none)], &o, 500.0), o.line_scroll_speed);
        assert_eq!(wheel_zoom_points(&[wheel(U::Line, -2.0, none)], &o, 500.0), -2.0 * o.line_scroll_speed);
        assert_eq!(wheel_zoom_points(&[wheel(U::Point, 7.5, none)], &o, 500.0), 7.5);
        assert_eq!(wheel_zoom_points(&[wheel(U::Page, 1.0, none)], &o, 500.0), 500.0);
        // Three quick notches in one frame zoom three steps at once.
        let three = [wheel(U::Line, 1.0, none), wheel(U::Line, 1.0, none), wheel(U::Line, 1.0, none)];
        assert_eq!(wheel_zoom_points(&three, &o, 500.0), 3.0 * o.line_scroll_speed);
        assert_eq!(wheel_zoom_points(&[wheel(U::Line, 1.0, Modifiers::COMMAND)], &o, 500.0), 0.0);
        assert_eq!(wheel_zoom_points(&[wheel(U::Line, 1.0, Modifiers::SHIFT)], &o, 500.0), 0.0);
        assert_eq!(wheel_zoom_points(&[wheel(U::Line, f32::NAN, none)], &o, 500.0), 0.0);
        assert_eq!(wheel_zoom_points(&[egui::Event::PointerGone], &o, 500.0), 0.0);
    }

    /// The frame a notch arrives, egui's smoothed scroll holds only part of it (the rest trickles
    /// in over later frames, which made quick notches lag), while the raw reading zooms the
    /// whole notch at once (#660).
    #[test]
    fn a_wheel_notch_zooms_whole_in_its_own_frame() {
        let ctx = egui::Context::default();
        let o = ctx.options(|o| o.input_options);
        let input = egui::RawInput { events: vec![wheel(egui::MouseWheelUnit::Line, 1.0, egui::Modifiers::NONE)], ..Default::default() };
        let mut seen = (0.0, 0.0);
        ctx.run_ui(input, |ui| {
            seen = ui.input(|i| (i.smooth_scroll_delta.y, wheel_zoom_points(&i.raw.events, &o, 500.0)));
        })
        .textures_delta
        .clear();
        let (smoothed, raw) = seen;
        assert_eq!(raw, o.line_scroll_speed, "the whole notch");
        assert!(smoothed < raw, "egui's smoothed delta lags behind: {smoothed} of {raw}");
    }

    /// The text painted by one frame of the app at `pointer`.
    pub(crate) fn painted_text(app: &mut CadApp, ctx: &egui::Context, pointer: Pos2) -> String {
        fn collect(shape: &Shape, text: &mut String) {
            match shape {
                Shape::Text(s) => {
                    text.push_str(s.galley.text());
                    text.push('\n');
                }
                Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, text)),
                _ => {}
            }
        }
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1400.0, 900.0))),
            events: vec![egui::Event::PointerMoved(pointer)],
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            app.logic(ui.ctx());
            app.ui(ui);
        });
        out.textures_delta.clear();
        let mut text = String::new();
        out.shapes.iter().for_each(|s| collect(&s.shape, &mut text));
        text
    }

    /// The Dynamic Input distance box shows the cursor's live distance in the drawing's length
    /// units (LUNITS/LUPREC), as DIST and the status bar do (#841).
    #[test]
    fn dynamic_input_distance_follows_the_drawing_units() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.session.execute("units", &serde_json::json!({"lunits": 4, "luprec": 4})).unwrap();
        let ctx = egui::Context::default();
        painted_text(&mut app, &ctx, pos2(700.0, 450.0));
        app.session.cmdline("line").unwrap();
        app.session.input(Input::Point(Vec2::ZERO)).unwrap();
        let at = app.canvas.xf.unwrap().to_screen(Vec2::ZERO) + vec2(130.0, -70.0);
        painted_text(&mut app, &ctx, at);
        let text = painted_text(&mut app, &ctx, at);
        let c = app.canvas.cursor.unwrap();
        let arch = |v: f64| cadcraft_engine::units::format_distance(v, 4, 4);
        // The status bar shows X and Y in the same units: the distance must be neither.
        assert!(arch(c.len()) != arch(c.x) && arch(c.len()) != arch(c.y), "{c:?}");
        assert!(text.contains(&arch(c.len())), "no {} in:\n{text}", arch(c.len()));
        // Without the value boxes (a window corner pending) the tooltip shows the distance too.
        app.session.pending_window = Some(cadcraft_engine::PendingWindow { corner: Vec2::ZERO, during_command: true });
        let text = painted_text(&mut app, &ctx, at);
        assert!(text.contains(&format!("   {}  <", arch(c.len()))), "no {} in:\n{text}", arch(c.len()));
    }

    /// Drives the app frame by frame, a tenth of a second apart, in a 1600×1000 window.
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
        fn button(&mut self, app: &mut CadApp, at: Pos2, pressed: bool) {
            let e = egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::default() };
            self.run(app, vec![e]);
        }
        /// A click, well apart from the one before (not a double click).
        fn click(&mut self, app: &mut CadApp, at: Pos2) {
            self.time += 1.0;
            self.run(app, vec![egui::Event::PointerMoved(at)]);
            self.button(app, at, true);
            self.button(app, at, false);
            self.run(app, Vec::new());
        }
        fn drag(&mut self, app: &mut CadApp, from: Pos2, to: Pos2) {
            self.time += 1.0;
            self.run(app, vec![egui::Event::PointerMoved(from)]);
            self.button(app, from, true);
            self.run(app, vec![egui::Event::PointerMoved(from + (to - from) / 2.0)]);
            self.run(app, vec![egui::Event::PointerMoved(to)]);
            self.button(app, to, false);
            self.run(app, Vec::new());
        }
    }

    /// A line in model space between the world points under screen points `a` and `b`.
    fn line_under(app: &mut CadApp, xf: &Xf, a: Pos2, b: Pos2) -> Handle {
        let (a, b) = (xf.to_world(a).to3(0.0), xf.to_world(b).to3(0.0));
        let d = app.session.doc_mut().unwrap();
        d.add(&cadcraft_doc::Space::Model, cadcraft_doc::Common::default(), cadcraft_doc::EntityKind::Line(cadcraft_doc::Line { a, b })).unwrap()
    }

    /// A solid that is only an edge from (0, 0, 20) to (10, 0, 20): drawn 20 above its plan.
    fn raised_edge(app: &mut CadApp) -> Handle {
        let g = cadcraft_geom::Vec3::new;
        let mesh =
            cadcraft_geom::Mesh3 { verts: Vec::new(), tris: Vec::new(), edges: vec![vec![g(0.0, 0.0, 20.0), g(10.0, 0.0, 20.0)]], open: false };
        let s =
            cadcraft_doc::Solid3d { brep: "".into(), mesh: std::sync::Arc::new(mesh), xform: cadcraft_geom::Mat4::IDENTITY, ..Default::default() };
        let d = app.session.doc_mut().unwrap();
        d.add(&cadcraft_doc::Space::Model, cadcraft_doc::Common::default(), cadcraft_doc::EntityKind::Solid3d(s)).unwrap()
    }

    /// A solid moved 5 up: an open edge (0, 0, 10)–(4, 0, 10)–(4, 3, 10), a circle of radius 3
    /// about (20, 5, 10) upright in the XZ plane and an ellipse about (40, -20, 10).
    fn snap_solid() -> cadcraft_doc::Solid3d {
        let g = cadcraft_geom::Vec3::new;
        let ring = |c: cadcraft_geom::Vec3, rx: f64, rz: f64| -> Vec<cadcraft_geom::Vec3> {
            (0..=24).map(|i| f64::from(i % 24) * std::f64::consts::TAU / 24.0).map(|t| g(c.x + rx * t.cos(), c.y, c.z + rz * t.sin())).collect()
        };
        let edges = vec![
            vec![g(0.0, 0.0, 10.0), g(4.0, 0.0, 10.0), g(4.0, 3.0, 10.0)],
            ring(g(20.0, 5.0, 10.0), 3.0, 3.0),
            ring(g(40.0, -20.0, 10.0), 3.0, 2.0),
        ];
        let mut xform = cadcraft_geom::Mat4::IDENTITY;
        xform.m[2][3] = 5.0;
        let mesh = cadcraft_geom::Mesh3 { verts: Vec::new(), tris: Vec::new(), edges, open: false };
        cadcraft_doc::Solid3d { brep: "".into(), mesh: std::sync::Arc::new(mesh), xform, ..Default::default() }
    }

    /// The app looking along `dir` at world point `at`, 60 units high on screen, LINE asking for
    /// its first point with only the object snaps `osmode` running.
    fn app_snapping(f: &mut Frames, dir: [f64; 3], at: cadcraft_geom::Vec3, osmode: u32) -> CadApp {
        let mut app = app_looking(f, dir);
        let st = app.session.state_mut().unwrap();
        let center = st.view_dir.project(at);
        st.set_view_quiet(cadcraft_engine::View { center, height: 60.0 });
        app.session.settings.osmode = osmode;
        app.session.cmdline("line").unwrap();
        f.run(&mut app, Vec::new());
        app
    }

    /// The snaps of a solid are where it is in the world: its edges' ends, the point halfway along
    /// an open edge, the centre of a closed edge that is a circle; only of the modes asked for,
    /// with no midpoint on a closed edge and no centre for an ellipse.
    #[test]
    fn a_solid_snaps_to_its_edge_ends_midpoints_and_circle_centres() {
        use snap::mode::{CEN, END, MID};
        let (s, g) = (snap_solid(), cadcraft_geom::Vec3::new);
        let has = |c: &[(cadcraft_geom::Vec3, u32)], p: cadcraft_geom::Vec3, m: u32| c.iter().any(|(q, k)| *k == m && (*q - p).len() < 1e-9);
        let all = snap3d::candidates(&s, END | MID | CEN);
        assert!(has(&all, g(0.0, 0.0, 15.0), END) && has(&all, g(4.0, 3.0, 15.0), END) && has(&all, g(23.0, 5.0, 15.0), END), "{all:?}");
        assert!(has(&all, g(3.5, 0.0, 15.0), MID) && has(&all, g(20.0, 5.0, 15.0), CEN), "{all:?}");
        assert_eq!(snap3d::candidates(&s, MID).len(), 1);
        assert_eq!(snap3d::candidates(&s, CEN).len(), 1);
        assert_eq!(snap3d::candidates(&s, END).len(), 4);
    }

    /// Solids on layers turned off or frozen aren't snapped to.
    #[test]
    fn solids_on_hidden_layers_are_not_snapped_to() {
        let mut d = cadcraft_doc::Drawing::new_metric();
        d.layers.push(cadcraft_doc::Layer::new("Shut"));
        let common = cadcraft_doc::Common { layer: "Shut".into(), ..Default::default() };
        d.add(&cadcraft_doc::Space::Model, common, cadcraft_doc::EntityKind::Solid3d(snap_solid())).unwrap();
        let xf = Xf::new(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0)), Vec2::ZERO, 5.0)
            .looking(ViewDir::new(cadcraft_geom::Vec3::new(-1.0, -1.0, 1.0)));
        let p = cadcraft_geom::Vec3::new(0.0, 0.0, 15.0);
        let at = snap3d::screen_of(&xf, p);
        let near = |d: &cadcraft_doc::Drawing| snap3d::nearest(d, &cadcraft_doc::Space::Model, &xf, at, 10.0, snap::mode::END, None);
        assert_eq!(near(&d), Some((p, snap::mode::END)));
        let shut: [fn(&mut cadcraft_doc::Layer); 2] = [|l| l.on = false, |l| l.frozen = true];
        for f in shut {
            let mut d = d.clone();
            d.layers.iter_mut().filter(|l| l.name == "Shut").for_each(f);
            assert_eq!(near(&d), None);
        }
    }

    /// In a 3D view a point prompt snaps to a raised solid's vertex where it is drawn: the marker
    /// is there, and the command gets the vertex's XY with its height (`Session::point_z`). With
    /// the running snaps off (F3) it gets the point on the XY plane under the cursor, no height.
    #[test]
    fn a_3d_view_snaps_to_a_raised_solid_vertex_with_its_height() {
        let mut f = Frames::default();
        let top = cadcraft_geom::Vec3::new(10.0, 0.0, 20.0);
        let mut app = app_snapping(&mut f, [-1.0, -1.0, 1.0], top, snap::mode::END);
        raised_edge(&mut app);
        let xf = app.canvas.xf.unwrap();
        let at = snap3d::screen_of(&xf, top) + vec2(3.0, -2.0);
        app.session.settings.osmode |= snap::mode::OFF;
        app.session.point_z = Some(1.0);
        f.click(&mut app, at);
        assert_eq!((app.canvas.snap.is_none(), app.session.point_z), (true, None));
        assert!(app.session.last_point.dist(xf.to_world(at)) < 1e-6, "{:?}", app.session.last_point);
        app.session.settings.osmode = snap::mode::END;
        f.run(&mut app, vec![egui::Event::PointerMoved(at + vec2(1.0, 0.0))]);
        let hit = app.canvas.snap.unwrap();
        assert_eq!((hit.point, hit.mode, app.canvas.snap_z), (Vec2::new(10.0, 0.0), snap::mode::END, Some(20.0)));
        f.click(&mut app, at);
        assert_eq!((app.session.last_point, app.session.point_z), (Vec2::new(10.0, 0.0), Some(20.0)));
    }

    /// In a 3D view the cursor snaps to the point halfway along a solid's edge and to the centre of
    /// its circular edge where they are drawn; a typed CEN override takes the centre with its
    /// height (the engine would look for it in plan, where the circle is a line).
    #[test]
    fn a_3d_view_snaps_to_solid_midpoints_and_centres() {
        use snap::mode::{CEN, MID};
        let mut f = Frames::default();
        let (mid, cen) = (cadcraft_geom::Vec3::new(3.5, 0.0, 15.0), cadcraft_geom::Vec3::new(20.0, 5.0, 15.0));
        let mut app = app_snapping(&mut f, [-1.0, -1.0, 1.0], cen, MID | CEN);
        app.session
            .doc_mut()
            .unwrap()
            .add(&cadcraft_doc::Space::Model, cadcraft_doc::Common::default(), cadcraft_doc::EntityKind::Solid3d(snap_solid()))
            .unwrap();
        let xf = app.canvas.xf.unwrap();
        for (p, m) in [(mid, MID), (cen, CEN)] {
            f.run(&mut app, vec![egui::Event::PointerMoved(snap3d::screen_of(&xf, p) + vec2(2.0, 2.0))]);
            let hit = app.canvas.snap.unwrap();
            assert_eq!((hit.point, hit.mode, app.canvas.snap_z), (p.xy(), m, Some(15.0)));
        }
        app.session.settings.osmode = 0;
        app.session.cmdline("cen").unwrap();
        f.click(&mut app, snap3d::screen_of(&xf, cen) + vec2(2.0, 1.0));
        assert_eq!((app.session.last_point, app.session.point_z), (cen.xy(), Some(15.0)));
        assert!(app.session.point_mods.is_empty());
    }

    /// Plan view keeps the engine's snaps: a raised solid's vertex snaps where its plan is, and the
    /// point given has no height (a height left from a 3D snap is cleared).
    #[test]
    fn plan_view_snaps_in_plan_without_a_height() {
        let mut f = Frames::default();
        let mut app = app_snapping(&mut f, [0.0, 0.0, 1.0], cadcraft_geom::Vec3::new(10.0, 0.0, 0.0), snap::mode::END);
        raised_edge(&mut app);
        app.session.point_z = Some(7.0);
        let xf = app.canvas.xf.unwrap();
        let at = xf.to_screen(Vec2::new(10.0, 0.0)) + vec2(3.0, -2.0);
        f.run(&mut app, vec![egui::Event::PointerMoved(at)]);
        assert_eq!(app.canvas.snap.map(|h| (h.point, h.mode)), Some((Vec2::new(10.0, 0.0), snap::mode::END)));
        assert_eq!(app.canvas.snap_z, None);
        f.click(&mut app, at);
        assert_eq!((app.session.last_point, app.session.point_z), (Vec2::new(10.0, 0.0), None));
    }

    /// In a 3D view, the candidate Tab cycled to stays: a solid's point nearer on screen doesn't
    /// take the cursor back, and the click gives the cycled point with no height.
    #[test]
    fn a_tab_cycled_snap_stays_over_a_nearer_solid_point() {
        let mut f = Frames::default();
        let top = cadcraft_geom::Vec3::new(10.0, 0.0, 20.0);
        let mut app = app_snapping(&mut f, [-1.0, -1.0, 1.0], top, snap::mode::END);
        raised_edge(&mut app);
        let xf = app.canvas.xf.unwrap();
        let at = snap3d::screen_of(&xf, top) + vec2(3.0, -2.0);
        let q = at + vec2(5.0, 4.0);
        line_under(&mut app, &xf, q, q + vec2(150.0, 80.0));
        let end = xf.to_world(q);
        f.run(&mut app, vec![egui::Event::PointerMoved(at)]);
        assert_eq!(app.canvas.snap_z, Some(20.0), "the solid's vertex is nearer than the line's end");
        app.canvas.snap_tab = true;
        f.run(&mut app, vec![egui::Event::PointerMoved(at)]);
        assert_eq!((app.canvas.snap.map(|h| h.point), app.canvas.snap_z), (Some(end), None));
        f.click(&mut app, at);
        assert_eq!((app.session.last_point, app.session.point_z), (end, None));
    }

    /// Looking for solid snaps skips the other objects before looking their layer up (by name,
    /// along the layer table): lines on a layer behind thousands of others cost next to nothing.
    #[test]
    fn solid_snaps_skip_other_objects_before_their_layer() {
        let g = cadcraft_geom::Vec3::new;
        let mut d = cadcraft_doc::Drawing::new_metric();
        let common = cadcraft_doc::Common { layer: "Drawing lines 9999".into(), ..Default::default() };
        for i in 0..50_000 {
            let l = cadcraft_doc::Line { a: g(f64::from(i), 0.0, 0.0), b: g(f64::from(i), 1.0, 0.0) };
            d.add(&cadcraft_doc::Space::Model, common.clone(), cadcraft_doc::EntityKind::Line(l)).unwrap();
        }
        d.add(&cadcraft_doc::Space::Model, cadcraft_doc::Common::default(), cadcraft_doc::EntityKind::Solid3d(snap_solid())).unwrap();
        // Layers named alike ahead of the lines' one: each lookup compares names to their end.
        d.layers.splice(0..0, (0..4000).map(|i| cadcraft_doc::Layer::new(&format!("Drawing lines {i:04}"))));
        let xf = Xf::new(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0)), Vec2::ZERO, 5.0)
            .looking(ViewDir::new(cadcraft_geom::Vec3::new(-1.0, -1.0, 1.0)));
        let p = g(0.0, 0.0, 15.0);
        let t = std::time::Instant::now();
        let hit = snap3d::nearest(&d, &cadcraft_doc::Space::Model, &xf, snap3d::screen_of(&xf, p), 10.0, snap::mode::END, None);
        let took = t.elapsed();
        assert_eq!(hit, Some((p, snap::mode::END)));
        assert!(took < std::time::Duration::from_millis(250), "{took:?}");
    }

    /// Crossings pair up the pieces of the curves under the cursor: a crowd of round solid edges
    /// (a zoomed-out view of furniture with round parts) stays quick with INT and APP running.
    #[test]
    fn three_d_crossings_stay_quick_under_a_crowd_of_round_edges() {
        let g = cadcraft_geom::Vec3::new;
        let ring = |r: f64, z: f64| -> Vec<cadcraft_geom::Vec3> {
            (0..=70).map(|i| f64::from(i % 70) * std::f64::consts::TAU / 70.0).map(|t| g(r * t.cos(), r * t.sin(), z)).collect()
        };
        let mut d = cadcraft_doc::Drawing::new_metric();
        for k in 0..200 {
            let edges = (0..4).map(|j| ring(1.0 + f64::from(j) * 0.1, f64::from(k) * 0.01)).collect();
            let mesh = cadcraft_geom::Mesh3 { verts: Vec::new(), tris: Vec::new(), edges, open: false };
            let solid = cadcraft_doc::Solid3d { brep: "".into(), mesh: std::sync::Arc::new(mesh), ..Default::default() };
            d.add(&cadcraft_doc::Space::Model, cadcraft_doc::Common::default(), cadcraft_doc::EntityKind::Solid3d(solid)).unwrap();
        }
        // Half a point to the unit: every ring is within the aperture.
        let xf = Xf::new(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0)), Vec2::ZERO, 0.5).looking(ViewDir::new(g(-1.0, -1.0, 1.0)));
        let t = std::time::Instant::now();
        let at = snap3d::screen_of(&xf, g(0.0, 0.0, 0.0));
        let _ = snap3d::nearest(&d, &cadcraft_doc::Space::Model, &xf, at, 10.0, snap::mode::INT | snap::mode::APP, None);
        let took = t.elapsed();
        assert!(took < std::time::Duration::from_millis(500), "{took:?}");
    }

    /// The app with an empty drawing looking along `dir`, its list built (at once, not in the
    /// background).
    fn app_looking(f: &mut Frames, dir: [f64; 3]) -> CadApp {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.session.state_mut().unwrap().view_dir = ViewDir::new(cadcraft_geom::Vec3::new(dir[0], dir[1], dir[2]));
        f.run(&mut app, Vec::new());
        app
    }

    /// A view far from the origin (a hostile drawing's extents) draws its CPlane grid and
    /// finishes: stepping a huge float by 1.0 doesn't change it, so the grid counts its lines.
    #[test]
    fn a_cplane_grid_far_from_the_origin_finishes() {
        let mut f = Frames::default();
        let mut app = app_looking(&mut f, [-1.0, -1.0, 1.0]);
        app.session.settings.gridmode = true;
        for c in [1e17, 1e300, -3e18] {
            let st = app.session.state_mut().unwrap();
            st.set_view_quiet(cadcraft_engine::View { center: Vec2::new(c, c), height: 10.0 });
            f.run(&mut app, Vec::new());
        }
    }

    /// Rebuild the list at once (edits made straight in the drawing don't bump the revision).
    fn rebuild_now(f: &mut Frames, app: &mut CadApp) {
        app.canvas.list = None;
        f.run(app, Vec::new());
    }

    /// Pointing in a view that sees the XY plane edge-on lands on the plane's line right under the
    /// cursor, in whichever direction the view looks (#35, #36).
    #[test]
    fn edge_on_views_point_under_the_cursor() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(200.0, 100.0));
        for d in [[0.0, -1.0, 0.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [-1.0, -1.0, 1.0]] {
            let dir = ViewDir::new(cadcraft_geom::Vec3::new(d[0], d[1], d[2]));
            let xf = Xf::new(rect, Vec2::new(3.0, 1.0), 10.0).looking(dir);
            for s in [pos2(30.0, 20.0), pos2(170.0, 80.0)] {
                let back = xf.to_screen(xf.to_world(s));
                assert!((back.x - s.x).abs() < 1e-3, "{d:?}: {s:?} came back at {back:?}");
                if d[2] != 0.0 {
                    assert!((back.y - s.y).abs() < 1e-3, "{d:?}: {s:?} came back at {back:?}");
                }
            }
        }
    }

    /// A zero-area triangle (a SOLID with its corners together, a fill seen edge-on) draws nothing
    /// and picks nothing (#42).
    #[test]
    fn list_pick_ignores_zero_area_triangles() {
        let prim = |h, kind, len| DPrim { handle: Handle(h), color: Rgb(255, 255, 255), aci7: true, lw: 0.0, transparency: 0, kind, start: 0, len };
        let l = DisplayList {
            prims: vec![prim(1, Kind::Polyline, 2), prim(2, Kind::Tris, 3)],
            verts: vec![Vec2::ZERO, Vec2::new(10.0, 0.0)],
            tris: vec![Vec2::new(5.0, 5.0); 3],
            ..Default::default()
        };
        assert_eq!(list_pick(&l, Vec2::new(50.0, 50.0), 1.0), None);
        assert_eq!(list_pick(&l, Vec2::new(5.0, 0.5), 1.0), Some(Handle(1)));
    }

    #[test]
    fn list_window_takes_what_is_inside_or_crossing() {
        let prim =
            |h, kind, start, len| DPrim { handle: Handle(h), color: Rgb(255, 255, 255), aci7: true, lw: 0.0, transparency: 0, kind, start, len };
        let v = Vec2::new;
        let l = DisplayList {
            // 1 inside, 2 crossing, 3 outside, 4 a fill around the whole box, 5 (zero area) in it.
            prims: vec![
                prim(1, Kind::Polyline, 0, 2),
                prim(2, Kind::Polyline, 2, 2),
                prim(3, Kind::Polyline, 4, 2),
                prim(4, Kind::Tris, 0, 3),
                prim(5, Kind::Tris, 3, 3),
            ],
            verts: vec![v(1.0, 1.0), v(2.0, 2.0), v(3.0, 3.0), v(9.0, 3.0), v(7.0, 7.0), v(8.0, 8.0)],
            tris: vec![v(-50.0, -50.0), v(50.0, -50.0), v(0.0, 50.0), v(1.0, 1.0), v(1.0, 1.0), v(1.0, 1.0)],
            ..Default::default()
        };
        let bx = Bounds2::new(v(0.0, 0.0), v(4.0, 4.0));
        assert_eq!(list_window(&l, bx, false), vec![Handle(1), Handle(5)]);
        assert_eq!(list_window(&l, bx, true), vec![Handle(1), Handle(2), Handle(4), Handle(5)]);
    }

    /// In a 3D view a selection window takes what is drawn in the rectangle on screen: a window
    /// when dragged to the right, a crossing window to the left, wherever world X points (to the
    /// left in the NE isometric view) (#22, #25, #37).
    #[test]
    fn a_3d_view_selects_what_is_drawn_in_the_window() {
        let mut f = Frames::default();
        let mut app = app_looking(&mut f, [1.0, 1.0, 1.0]);
        let xf = app.canvas.xf.unwrap();
        let c = xf.rect.center();
        // `a` inside the window, `b` across its right side.
        let a = line_under(&mut app, &xf, c + vec2(-50.0, -20.0), c + vec2(50.0, 0.0));
        let b = line_under(&mut app, &xf, c + vec2(0.0, 60.0), c + vec2(250.0, 60.0));
        rebuild_now(&mut f, &mut app);
        f.drag(&mut app, c + vec2(-100.0, -60.0), c + vec2(150.0, 100.0));
        assert_eq!(app.session.selection(), vec![a]);
        app.session.set_selection(Vec::new());
        f.drag(&mut app, c + vec2(150.0, 100.0), c + vec2(-100.0, -60.0));
        let mut sel = app.session.selection();
        sel.sort();
        assert_eq!(sel, vec![a, b]);
        assert!(app.session.pending_window.is_none());
    }

    /// At "Select objects" in a 3D view, a window's second corner on an object closes the window
    /// (it used to pick that object and leave the window open) (#38).
    #[test]
    fn a_3d_window_closes_on_an_object() {
        let mut f = Frames::default();
        let mut app = app_looking(&mut f, [-1.0, -1.0, 1.0]);
        let xf = app.canvas.xf.unwrap();
        let c = xf.rect.center();
        let a = line_under(&mut app, &xf, c + vec2(-50.0, -20.0), c + vec2(50.0, 0.0));
        let b = line_under(&mut app, &xf, c + vec2(0.0, 60.0), c + vec2(250.0, 60.0));
        rebuild_now(&mut f, &mut app);
        app.session.cmdline("erase").unwrap();
        f.click(&mut app, c + vec2(-100.0, -60.0));
        assert!(app.session.pending_window.is_some());
        f.click(&mut app, c + vec2(150.0, 60.0));
        assert!(app.session.pending_window.is_none());
        app.session.input(Input::Enter).unwrap();
        let d = app.session.doc().unwrap();
        assert!(d.entity(a).is_none() && d.entity(b).is_some());
    }

    /// In a 3D view a click on a raised line at OFFSET's "Select object to offset" (a prompt that
    /// takes a point) picks that line, and a typed W window round where it is drawn selects it.
    #[test]
    fn a_3d_view_picks_at_point_prompts_and_windows_typed_modes_as_drawn() {
        let mut f = Frames::default();
        let mut app = app_looking(&mut f, [-1.0, -1.0, 1.0]);
        let g = cadcraft_geom::Vec3::new;
        let (a, b) = (g(0.0, 0.0, 30.0), g(100.0, 0.0, 30.0));
        let up = app.session.add_entity_world(cadcraft_doc::EntityKind::Line(cadcraft_doc::Line { a, b })).unwrap();
        let dir = app.session.state().unwrap().view_dir;
        app.session.state_mut().unwrap().set_view_quiet(cadcraft_engine::View { center: dir.project(g(50.0, 0.0, 30.0)), height: 200.0 });
        rebuild_now(&mut f, &mut app);
        let xf = app.canvas.xf.unwrap();
        app.session.cmdline("offset").unwrap();
        app.session.cmdline("10").unwrap();
        f.click(&mut app, xf.world_to_screen(g(50.0, 0.0, 30.0)));
        assert_eq!(app.session.selection(), vec![up], "{:?}", app.session.current_prompt().map(|p| p.display()));
        app.session.cancel();
        app.session.cmdline("erase").unwrap();
        app.session.cmdline("w").unwrap();
        let (sa, sb) = (xf.world_to_screen(a), xf.world_to_screen(b));
        f.click(&mut app, pos2(sa.x.min(sb.x) - 10.0, sa.y.max(sb.y) + 10.0));
        f.click(&mut app, pos2(sa.x.max(sb.x) + 10.0, sa.y.min(sb.y) - 10.0));
        assert!(app.session.view_at.is_none());
        app.session.input(Input::Enter).unwrap();
        assert!(app.session.doc().unwrap().entity(up).is_none(), "{:?}", app.session.log.last());
    }

    /// Clicking where nothing is drawn in a 3D view starts a window: it doesn't pick a solid by
    /// its outline on the XY plane, far from where it is drawn (#59); and a solid's grips, which
    /// are on that plane, don't show or catch the cursor there (#46).
    #[test]
    fn a_3d_view_ignores_where_solids_would_be_in_plan() {
        let mut f = Frames::default();
        let mut app = app_looking(&mut f, [-1.0, -1.0, 1.0]);
        let s = raised_edge(&mut app);
        rebuild_now(&mut f, &mut app);
        let xf = app.canvas.xf.unwrap();
        f.click(&mut app, xf.to_screen(Vec2::new(5.0, 0.0)));
        assert!(app.session.selection().is_empty());
        assert!(app.session.pending_window.is_some());
        app.session.pending_window = None;
        app.session.set_selection(vec![s]);
        let grip = xf.to_screen(Vec2::ZERO);
        assert_eq!(grip_at(&app, &xf, Some(grip)), None);
        let plan = Xf::new(xf.rect, Vec2::ZERO, xf.scale);
        assert_eq!(grip_at(&app, &plan, Some(plan.to_screen(Vec2::ZERO))), Some((s, 0)));
    }

    /// A circle standing upright (extrusion along X): world YZ, as `Entity::plane` places it.
    fn standing_circle(app: &mut CadApp, elevation: f64, radius: f64) -> Handle {
        let common = cadcraft_doc::Common { extrusion: cadcraft_geom::Vec3::new(1.0, 0.0, 0.0), ..Default::default() };
        let c = cadcraft_doc::Circle { center: cadcraft_geom::Vec3::new(0.0, 0.0, elevation), radius };
        let d = app.session.doc_mut().unwrap();
        d.add(&cadcraft_doc::Space::Model, common, cadcraft_doc::EntityKind::Circle(c)).unwrap()
    }

    /// The grips of an object in a standing plane are where plan draws it (`Entity::grips`), not
    /// at its own 2D coordinates.
    #[test]
    fn grips_of_a_standing_circle_are_where_plan_draws_them() {
        let mut f = Frames::default();
        let mut app = app_looking(&mut f, [0.0, 0.0, 1.0]);
        app.session.execute("view.set", &serde_json::json!({ "center": [0, 0], "height": 30 })).unwrap();
        let h = standing_circle(&mut app, 0.0, 5.0);
        app.session.set_selection(vec![h]);
        f.run(&mut app, Vec::new());
        let xf = app.canvas.xf.unwrap();
        // Its quadrant along OCS X is world (0, 5); along OCS Y it stands up over the centre.
        assert_eq!(grip_at(&app, &xf, Some(xf.to_screen(Vec2::new(0.0, 5.0)))), Some((h, 1)));
        assert_eq!(grip_at(&app, &xf, Some(xf.to_screen(Vec2::new(5.0, 0.0)))), None);
    }

    /// A drag that rebuilds its objects (a solid turned in a 3D view) turns a standing circle in
    /// the world (`Entity::transform`), not in its own plane's coordinates.
    #[test]
    fn a_turned_drag_previews_a_standing_circle_turned_in_the_world() {
        let mut f = Frames::default();
        let mut app = app_looking(&mut f, [0.0, -1.0, 0.0]);
        let s = raised_edge(&mut app);
        let c = standing_circle(&mut app, 3.0, 2.0);
        let xf = app.canvas.xf.unwrap();
        let drag =
            cadcraft_engine::Drag { handles: vec![s, c], matrix: Mat3::rotate(std::f64::consts::FRAC_PI_2), extra: Vec::new(), clipboard: false };
        let list = solid_drag_preview(app.session.doc().unwrap(), &drag, &[], &xf).unwrap();
        let pts: Vec<Vec2> = list.prims.iter().filter(|p| p.handle == c).flat_map(|p| list.points(p).to_vec()).collect();
        let b = Bounds2::from_points(pts);
        // Turned a quarter about Z it faces the front view: a whole circle, not a line seen edge-on.
        assert!(b.width() > 3.9 && b.height() > 3.9, "{b:?}");
    }

    /// Rotating a solid in a 3D view previews it rebuilt as it will be: moving the view plane on
    /// the GPU would carry its height along sideways (#41, #58, #62).
    #[test]
    fn rotating_a_solid_in_a_3d_view_previews_its_height_upright() {
        let mut f = Frames::default();
        let mut app = app_looking(&mut f, [-1.0, -1.0, 1.0]);
        let s = raised_edge(&mut app);
        let st = app.session.state().unwrap();
        let xf = app.canvas.xf.unwrap();
        let drag = |matrix| cadcraft_engine::Drag { handles: vec![s], matrix, extra: Vec::new(), clipboard: false };
        let turned = solid_drag_preview(&st.doc, &drag(Mat3::rotate(std::f64::consts::FRAC_PI_2)), &[], &xf).unwrap();
        // (10, 0, 20) turned about Z to (0, 10, 20).
        let want = st.view_dir.project(cadcraft_geom::Vec3::new(0.0, 10.0, 20.0));
        assert!(turned.verts.iter().any(|q| q.dist(want) < 1e-9), "{:?} not in {:?}", want, turned.verts);
        // A move is drawn by the GPU drag, exact for it; so is anything in plan.
        assert!(solid_drag_preview(&st.doc, &drag(Mat3::translate(Vec2::new(1.0, 2.0))), &[], &xf).is_none());
        let plan = Xf::new(xf.rect, Vec2::ZERO, xf.scale);
        assert!(solid_drag_preview(&st.doc, &drag(Mat3::rotate(1.0)), &[], &plan).is_none());
    }

    /// A new view direction is built in the background; until it lands the canvas shows, picks
    /// and snaps in the direction of the list it has, about the same point (#43).
    #[test]
    fn orbiting_builds_in_the_background() {
        let mut f = Frames::default();
        let mut app = app_looking(&mut f, [0.0, 0.0, 1.0]);
        let xf = app.canvas.xf.unwrap();
        line_under(&mut app, &xf, xf.rect.center(), xf.rect.center() + vec2(100.0, 0.0));
        rebuild_now(&mut f, &mut app);
        let target = xf.to_world(xf.rect.center());
        cadcraft_engine::orbit(&mut app.session, 30.0, -30.0).unwrap();
        f.run(&mut app, Vec::new());
        assert!(app.canvas.building());
        let shown = app.canvas.xf.unwrap();
        assert!(shown.is_plan());
        assert!(shown.to_world(shown.rect.center()).dist(target) < 1e-6);
        for _ in 0..500 {
            if !app.canvas.building() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            f.run(&mut app, Vec::new());
        }
        f.run(&mut app, Vec::new());
        let now = app.canvas.xf.unwrap();
        assert!(!now.is_plan());
        assert_eq!(app.canvas.list_dir, app.session.state().unwrap().view_dir);
        assert!(now.to_world(now.rect.center()).dist(target) < 1e-6);
    }

    /// The Front construction plane: x along world X, y up world Z, its normal toward -Y.
    fn front_cplane() -> Mat4 {
        let g = cadcraft_geom::Vec3::new;
        Mat4::from_cols(g(1.0, 0.0, 0.0), g(0.0, 0.0, 1.0), g(0.0, -1.0, 0.0), g(0.0, 0.0, 0.0))
    }

    /// The cursor finds its point where the view ray meets the construction plane, in the plane's
    /// coordinates: from the front the Front CPlane gives world X and Z; from an isometric view a
    /// lifted, turned plane gives the point on it under the cursor. The World CPlane seen from the
    /// front is edge-on, and plan with the World CPlane is the plain 2D transform.
    #[test]
    fn points_land_on_the_construction_plane_in_its_coordinates() {
        let g = cadcraft_geom::Vec3::new;
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0));
        let front = Xf::new(rect, Vec2::new(2.0, 3.0), 10.0).looking(ViewDir::new(g(0.0, -1.0, 0.0)));
        assert!(front.edge_on());
        let on = front.on(front_cplane());
        assert!(!on.edge_on() && !on.is_plan());
        let at = on.world_to_screen(g(3.0, 9.0, 7.0));
        assert!(on.to_world(at).dist(Vec2::new(3.0, 7.0)) < 1e-9, "{:?}", on.to_world(at));
        assert!(on.to_screen(Vec2::new(3.0, 7.0)).distance(at) < 1e-3);
        // A plane through (1, 2, 5) turned 30° about Z, seen from the south-west.
        let c = Mat4::translate(g(1.0, 2.0, 5.0)).mul(&Mat4::rotation(g(0.0, 0.0, 1.0), 30f64.to_radians()));
        let iso = Xf::new(rect, Vec2::ZERO, 10.0).looking(ViewDir::new(g(-1.0, -1.0, 1.0))).on(c);
        let s = pos2(250.0, 120.0);
        let l = iso.to_world(s);
        assert!(iso.world_to_screen(c.apply(l.to3(0.0))).distance(s) < 1e-3);
        assert!(iso.local_to_screen(l.to3(0.0)).distance(s) < 1e-3);
        let plan = Xf::new(rect, Vec2::ZERO, 10.0).looking(ViewDir::PLAN);
        assert!(plan.is_plan() && plan.on(Mat4::IDENTITY).is_plan());
        // `world` takes world XY points again (the overlays of world geometry).
        let w = iso.world();
        assert!(w.to_screen(Vec2::new(3.0, 4.0)).distance(iso.world_to_screen(g(3.0, 4.0, 0.0))) < 1e-3);
    }

    /// The app looking along `dir` at a line from (0, 0, 0) to (10, 0, 0), selected: the gumball
    /// shows at (5, 0, 0). Returns the line and where the gumball's centre is on screen.
    fn gumball_on_a_line(f: &mut Frames, dir: [f64; 3]) -> (CadApp, Handle, Pos2) {
        let mut app = app_looking(f, dir);
        let o = cadcraft_geom::Vec3::new(5.0, 0.0, 0.0);
        let st = app.session.state_mut().unwrap();
        let center = st.view_dir.project(o);
        st.set_view_quiet(cadcraft_engine::View { center, height: 60.0 });
        let line = cadcraft_doc::Line { a: cadcraft_geom::Vec3::ZERO, b: cadcraft_geom::Vec3::new(10.0, 0.0, 0.0) };
        let h = app
            .session
            .doc_mut()
            .unwrap()
            .add(&cadcraft_doc::Space::Model, cadcraft_doc::Common::default(), cadcraft_doc::EntityKind::Line(line))
            .unwrap();
        app.session.set_selection(vec![h]);
        rebuild_now(f, &mut app);
        let at = app.canvas.xf.unwrap().world_to_screen(o);
        (app, h, at)
    }

    fn line_ends(app: &CadApp, h: Handle) -> (cadcraft_geom::Vec3, cadcraft_geom::Vec3) {
        match &app.session.doc().unwrap().entity(h).unwrap().kind {
            cadcraft_doc::EntityKind::Line(l) => (l.a, l.b),
            k => panic!("{k:?}"),
        }
    }

    /// Dragging the gumball's X arrow moves the selection along world X by the distance dragged
    /// (3DMOVE, one undo step); a number typed during the drag sets the exact distance; Esc
    /// cancels the drag and keeps the selection.
    #[test]
    fn dragging_a_gumball_arrow_moves_along_its_axis() {
        let g = cadcraft_geom::Vec3::new;
        let mut f = Frames::default();
        let (mut app, h, o) = gumball_on_a_line(&mut f, [0.0, -1.0, 0.0]);
        let scale = app.canvas.xf.unwrap().scale;
        f.drag(&mut app, o + vec2(60.0, 0.0), o + vec2(110.0, 20.0));
        let (a, b) = line_ends(&app, h);
        let dx = 50.0 / scale;
        assert!((a - g(dx, 0.0, 0.0)).len() < 1e-4 && (b - g(10.0 + dx, 0.0, 0.0)).len() < 1e-4, "{a:?} {b:?}");
        assert_eq!(app.session.selection(), vec![h], "the selection stays, with the gumball on it");
        app.session.execute("u", &serde_json::json!({})).unwrap();
        assert_eq!(line_ends(&app, h), (g(0.0, 0.0, 0.0), g(10.0, 0.0, 0.0)), "one undo step");
        // Typed: exactly 7 up world Z (the Z arrow points up the screen from the front).
        app.session.set_selection(vec![h]);
        f.run(&mut app, Vec::new());
        let from = o + vec2(0.0, -60.0);
        f.run(&mut app, vec![egui::Event::PointerMoved(from)]);
        f.button(&mut app, from, true);
        f.run(&mut app, vec![egui::Event::PointerMoved(from + vec2(0.0, -30.0))]);
        f.run(&mut app, vec![egui::Event::Text("7".into())]);
        f.button(&mut app, from + vec2(0.0, -30.0), false);
        f.run(&mut app, Vec::new());
        assert_eq!(line_ends(&app, h), (g(0.0, 0.0, 7.0), g(10.0, 0.0, 7.0)));
        assert!(app.cmd.buffer.is_empty(), "the number went to the gumball, not the command line");
        // Esc during a drag cancels it.
        let o = app.canvas.xf.unwrap().world_to_screen(g(5.0, 0.0, 7.0));
        f.run(&mut app, vec![egui::Event::PointerMoved(o + vec2(60.0, 0.0))]);
        f.button(&mut app, o + vec2(60.0, 0.0), true);
        f.run(&mut app, vec![egui::Event::PointerMoved(o + vec2(120.0, 0.0))]);
        let esc = egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        f.run(&mut app, vec![esc]);
        f.button(&mut app, o + vec2(120.0, 0.0), false);
        f.run(&mut app, Vec::new());
        assert_eq!(line_ends(&app, h), (g(0.0, 0.0, 7.0), g(10.0, 0.0, 7.0)));
        assert_eq!(app.session.selection(), vec![h]);
    }

    /// A gumball arrow dragged onto another object's end snaps to it, taken along its axis: the
    /// line moves 9 along X to bring its middle (5, 0, 0) level with the end at (14, 0, 3); with
    /// the snaps off (`ui.gumball` `snap`) the same drag moves by the distance dragged.
    #[test]
    fn a_gumball_drag_snaps_to_an_object_snap() {
        let g = cadcraft_geom::Vec3::new;
        let mut f = Frames::default();
        let (mut app, h, o) = gumball_on_a_line(&mut f, [0.0, -1.0, 0.0]);
        let other = cadcraft_doc::Line { a: g(14.0, 0.0, 3.0), b: g(14.0, 0.0, 9.0) };
        app.session
            .doc_mut()
            .unwrap()
            .add(&cadcraft_doc::Space::Model, cadcraft_doc::Common::default(), cadcraft_doc::EntityKind::Line(other))
            .unwrap();
        app.session.set_selection(vec![h]);
        rebuild_now(&mut f, &mut app);
        let xf = app.canvas.xf.unwrap();
        let end = xf.world_to_screen(g(14.0, 0.0, 3.0));
        f.drag(&mut app, o + vec2(60.0, 0.0), end + vec2(2.0, 1.0));
        let (a, _) = line_ends(&app, h);
        assert!((a - g(9.0, 0.0, 0.0)).len() < 1e-9, "{a:?}");
        app.session.execute("u", &serde_json::json!({})).unwrap();
        app.run("ui.gumball", serde_json::json!({ "snap": false })).unwrap();
        app.session.set_selection(vec![h]);
        f.run(&mut app, Vec::new());
        f.drag(&mut app, o + vec2(60.0, 0.0), end + vec2(2.0, 1.0));
        let (a, _) = line_ends(&app, h);
        let dragged = f64::from(end.x + 2.0 - o.x - 60.0) / xf.scale;
        assert!((a.x - dragged).abs() < 1e-4 && (a.x - 9.0).abs() > 1e-3, "{a:?} {dragged}");
    }

    /// Dragging a gumball arc turns the selection about that world axis through the gumball's
    /// centre (3DROTATE): 90° typed about Y from the front stands the line up, once.
    #[test]
    fn dragging_a_gumball_arc_turns_about_its_axis() {
        let g = cadcraft_geom::Vec3::new;
        let mut f = Frames::default();
        let (mut app, h, o) = gumball_on_a_line(&mut f, [0.0, -1.0, 0.0]);
        // The Y arc from the front: a quarter circle between +Z (up) and +X (right).
        let at = o + vec2(1.0, -1.0) * (55.0 * std::f32::consts::FRAC_1_SQRT_2);
        f.run(&mut app, vec![egui::Event::PointerMoved(at)]);
        f.button(&mut app, at, true);
        f.run(&mut app, vec![egui::Event::PointerMoved(at + vec2(30.0, 30.0))]);
        f.run(&mut app, vec![egui::Event::Text("90".into())]);
        // Enter runs it at once (it doesn't repeat the last command); letting go adds nothing.
        app.session.last_command = Some("line".into());
        let enter = egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        f.run(&mut app, vec![enter]);
        assert!(app.session.running.is_none());
        f.button(&mut app, at + vec2(30.0, 30.0), false);
        f.run(&mut app, Vec::new());
        let (a, b) = line_ends(&app, h);
        assert!((a - g(5.0, 0.0, 5.0)).len() < 1e-9 && (b - g(5.0, 0.0, -5.0)).len() < 1e-9, "{a:?} {b:?}");
    }

    /// Plan views show no gumball: a press where its arrow would be opens a selection window.
    /// Turned off (`ui.gumball`), it doesn't show in 3D views either.
    #[test]
    fn plan_views_have_no_gumball() {
        let mut f = Frames::default();
        let (mut app, h, o) = gumball_on_a_line(&mut f, [0.0, 0.0, 1.0]);
        f.drag(&mut app, o + vec2(60.0, 0.0), o + vec2(110.0, 20.0));
        assert_eq!(line_ends(&app, h).0, cadcraft_geom::Vec3::ZERO);
        let (mut app, h, o) = gumball_on_a_line(&mut f, [0.0, -1.0, 0.0]);
        assert_eq!(app.run("ui.gumball", serde_json::json!({ "on": false })).unwrap()["on"], false);
        f.drag(&mut app, o + vec2(60.0, 0.0), o + vec2(110.0, 20.0));
        assert_eq!(line_ends(&app, h).0, cadcraft_geom::Vec3::ZERO);
    }

    /// In an isometric view a point prompt snaps to the centre of a raised circle where it is
    /// drawn, giving its construction-plane coordinates with its height.
    #[test]
    fn a_3d_view_snaps_to_a_raised_circle_centre_with_its_height() {
        let mut f = Frames::default();
        let c = cadcraft_geom::Vec3::new(3.0, 4.0, 6.0);
        let mut app = app_snapping(&mut f, [-1.0, -1.0, 1.0], c, snap::mode::CEN);
        let circle = cadcraft_doc::Circle { center: c, radius: 2.0 };
        app.session
            .doc_mut()
            .unwrap()
            .add(&cadcraft_doc::Space::Model, cadcraft_doc::Common::default(), cadcraft_doc::EntityKind::Circle(circle))
            .unwrap();
        let xf = app.canvas.xf.unwrap();
        let at = xf.world_to_screen(c) + vec2(2.0, -3.0);
        f.run(&mut app, vec![egui::Event::PointerMoved(at)]);
        let hit = app.canvas.snap.unwrap();
        assert_eq!((hit.point, hit.mode, app.canvas.snap_z), (Vec2::new(3.0, 4.0), snap::mode::CEN, Some(6.0)));
        f.click(&mut app, at);
        assert_eq!((app.session.last_point, app.session.point_z), (Vec2::new(3.0, 4.0), Some(6.0)));
    }

    /// The 3D snap points of curves: a line's ends and middle, a 3D polyline's vertices and
    /// segment middles, and a circle standing upright: its centre and quadrants in its own plane,
    /// where it is in the world.
    #[test]
    fn curves_snap_where_they_are_in_the_world() {
        use snap::mode::{CEN, END, MID, QUA};
        let g = cadcraft_geom::Vec3::new;
        let ent =
            |kind, extrusion| cadcraft_doc::Entity { handle: Handle(1), common: cadcraft_doc::Common { extrusion, ..Default::default() }, kind };
        let pts = |e: &cadcraft_doc::Entity, m| {
            let mut out = Vec::new();
            snap3d::snaps_of(e, m, &mut out);
            out
        };
        let line = ent(cadcraft_doc::EntityKind::Line(cadcraft_doc::Line { a: g(0.0, 0.0, 1.0), b: g(4.0, 0.0, 3.0) }), cadcraft_geom::Vec3::Z);
        assert_eq!(pts(&line, END | MID), vec![(g(0.0, 0.0, 1.0), END), (g(4.0, 0.0, 3.0), END), (g(2.0, 0.0, 2.0), MID)]);
        let poly = cadcraft_doc::Polyline3d { points: vec![g(0.0, 0.0, 0.0), g(2.0, 0.0, 2.0), g(2.0, 2.0, 2.0)], closed: true };
        let poly = ent(cadcraft_doc::EntityKind::Polyline3d(poly), cadcraft_geom::Vec3::Z);
        assert_eq!(pts(&poly, MID).len(), 3, "a closed 3D polyline has its closing segment's middle too");
        // Standing in world YZ (extrusion +X), 5 along X.
        let circle = cadcraft_doc::Circle { center: g(0.0, 0.0, 5.0), radius: 2.0 };
        let circle = ent(cadcraft_doc::EntityKind::Circle(circle), g(1.0, 0.0, 0.0));
        let got = pts(&circle, CEN | QUA);
        assert!(got.iter().any(|(p, m)| *m == CEN && (*p - g(5.0, 0.0, 0.0)).len() < 1e-9), "{got:?}");
        let quadrant = |p: &cadcraft_geom::Vec3| (p.x - 5.0).abs() < 1e-9 && ((p.y.abs() - 2.0).abs() < 1e-9 || (p.z.abs() - 2.0).abs() < 1e-9);
        assert_eq!(got.iter().filter(|(p, m)| *m == QUA && quadrant(p)).count(), 4, "{got:?}");
    }

    /// A drawing of model space objects `kinds` (level, default layer) and a view of it looking
    /// down from the south-west, 5 points to the unit.
    fn snap_scene(kinds: Vec<cadcraft_doc::EntityKind>) -> (cadcraft_doc::Drawing, Xf) {
        let mut d = cadcraft_doc::Drawing::new_metric();
        for k in kinds {
            d.add(&cadcraft_doc::Space::Model, cadcraft_doc::Common::default(), k).unwrap();
        }
        let xf = Xf::new(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0)), Vec2::ZERO, 5.0)
            .looking(ViewDir::new(cadcraft_geom::Vec3::new(-1.0, -1.0, 1.0)));
        (d, xf)
    }

    fn line3(a: cadcraft_geom::Vec3, b: cadcraft_geom::Vec3) -> cadcraft_doc::EntityKind {
        cadcraft_doc::EntityKind::Line(cadcraft_doc::Line { a, b })
    }

    /// In 3D, lines snap where they are: the nearest point along one, an intersection where two
    /// meet in the world, an apparent intersection (on the one in front) where one passes behind
    /// the other on screen, and none where only INT runs; an end beats a nearest point.
    #[test]
    fn three_d_snaps_nearest_intersection_and_apparent_intersection() {
        use snap::mode::{APP, END, INT, NEA};
        let g = cadcraft_geom::Vec3::new;
        let toward = ViewDir::new(g(-1.0, -1.0, 1.0)).dir;
        // C stands in front of A's (1, 0, 10), crossing it on screen only.
        let front = g(1.0, 0.0, 10.0) + toward * 4.0;
        let (d, xf) = snap_scene(vec![
            line3(g(0.0, 0.0, 10.0), g(10.0, 0.0, 10.0)),
            line3(g(5.0, -5.0, 10.0), g(5.0, 5.0, 10.0)),
            line3(front - g(0.0, 0.0, 3.0), front + g(0.0, 0.0, 3.0)),
        ]);
        let near = |p: cadcraft_geom::Vec3, m: u32| {
            snap3d::nearest(&d, &cadcraft_doc::Space::Model, &xf, snap3d::screen_of(&xf, p) + vec2(2.0, 1.0), 10.0, m, None)
        };
        assert_eq!(near(g(5.0, 0.0, 10.0), INT), Some((g(5.0, 0.0, 10.0), INT)));
        let (p, m) = near(g(1.0, 0.0, 10.0), INT | APP).unwrap();
        assert!(m == APP && (p - front).len() < 1e-9, "{p:?} {m}");
        assert_eq!(near(g(1.0, 0.0, 10.0), INT), None, "they don't meet");
        let (p, m) = near(g(7.0, 0.0, 10.0), NEA).unwrap();
        assert!(m == NEA && p.y.abs() < 1e-9 && (p.z - 10.0).abs() < 1e-9 && (p.x - 7.0).abs() < 0.5, "{p:?}");
        assert_eq!(near(g(10.0, 0.0, 10.0), END | NEA), Some((g(10.0, 0.0, 10.0), END)));
    }

    /// Along a raised circle the nearest point is on the circle itself (not on a chord of it), and
    /// a line through its plane meets it exactly; the perpendicular from a base point is the
    /// circle's point square to it, and a line's foot.
    #[test]
    fn three_d_snaps_on_a_raised_circle_are_on_it() {
        use snap::mode::{INT, NEA, PER};
        let g = cadcraft_geom::Vec3::new;
        let circle = cadcraft_doc::EntityKind::Circle(cadcraft_doc::Circle { center: g(0.0, 0.0, 5.0), radius: 4.0 });
        let (d, xf) = snap_scene(vec![circle, line3(g(4.0, 0.0, 0.0), g(4.0, 0.0, 10.0)), line3(g(-5.0, 10.0, 20.0), g(10.0, 10.0, 20.0))]);
        let near = |p: cadcraft_geom::Vec3, m: u32, base| {
            snap3d::nearest(&d, &cadcraft_doc::Space::Model, &xf, snap3d::screen_of(&xf, p) + vec2(1.5, -1.0), 10.0, m, base)
        };
        let on = g(4.0 * 2.0_f64.cos(), 4.0 * 2.0_f64.sin(), 5.0);
        let (p, m) = near(on, NEA, None).unwrap();
        assert!(m == NEA && ((p - g(0.0, 0.0, 5.0)).len() - 4.0).abs() < 1e-9 && (p.z - 5.0).abs() < 1e-12, "{p:?}");
        let (p, m) = near(g(4.0, 0.0, 5.0), INT, None).unwrap();
        assert!(m == INT && (p - g(4.0, 0.0, 5.0)).len() < 1e-9, "{p:?}");
        let (p, m) = near(on, PER, Some(g(0.0, 0.0, 5.0) + (on - g(0.0, 0.0, 5.0)) * 3.0 + g(0.0, 0.0, 7.0))).unwrap();
        assert!(m == PER && (p - on).len() < 1e-9, "{p:?}");
        let (p, m) = near(g(0.0, 10.0, 20.0), PER, Some(g(0.0, 0.0, 0.0))).unwrap();
        assert!(m == PER && (p - g(0.0, 10.0, 20.0)).len() < 1e-9, "{p:?}");
    }

    /// In a 3D view LINE's second point snaps perpendicular to a raised line from the first, with
    /// its height; Tab reaches a vertex hidden behind another on screen, and the click gives it.
    #[test]
    fn a_3d_view_snaps_perpendicular_and_tab_reaches_a_hidden_vertex() {
        use snap::mode::{END, PER};
        let g = cadcraft_geom::Vec3::new;
        let mut f = Frames::default();
        let foot = g(0.0, 10.0, 20.0);
        let mut app = app_snapping(&mut f, [-1.0, -1.0, 1.0], foot, PER);
        let raised = line3(g(-5.0, 10.0, 20.0), g(10.0, 10.0, 20.0));
        app.session.doc_mut().unwrap().add(&cadcraft_doc::Space::Model, cadcraft_doc::Common::default(), raised).unwrap();
        app.session.cmdline("0,0").unwrap();
        let xf = app.canvas.xf.unwrap();
        f.run(&mut app, vec![egui::Event::PointerMoved(snap3d::screen_of(&xf, foot) + vec2(2.0, 2.0))]);
        let hit = app.canvas.snap.unwrap();
        assert!(hit.point.dist(foot.xy()) < 1e-9 && hit.mode == PER && app.canvas.snap_z.is_some_and(|z| (z - 20.0).abs() < 1e-9), "{hit:?}");
        app.session.cancel();

        // Two raised ends one behind the other along the line of sight.
        let toward = ViewDir::new(g(-1.0, -1.0, 1.0)).dir;
        let (front, back) = (g(10.0, 0.0, 20.0), g(10.0, 0.0, 20.0) - toward * 6.0);
        let d = app.session.doc_mut().unwrap();
        for (a, b) in [(front, front + g(5.0, 0.0, 0.0)), (back, back + g(0.0, -5.0, 0.0))] {
            d.add(&cadcraft_doc::Space::Model, cadcraft_doc::Common::default(), line3(a, b)).unwrap();
        }
        app.session.settings.osmode = END;
        app.session.cmdline("line").unwrap();
        let at = snap3d::screen_of(&xf, front) + vec2(2.0, -1.0);
        f.run(&mut app, vec![egui::Event::PointerMoved(at)]);
        assert_eq!(app.canvas.snap_z, Some(20.0), "the end in front");
        app.canvas.snap_tab = true;
        f.run(&mut app, vec![egui::Event::PointerMoved(at)]);
        let hit = app.canvas.snap.unwrap();
        assert!(hit.point.dist(back.xy()) < 1e-9 && app.canvas.snap_z.is_some_and(|z| (z - back.z).abs() < 1e-9), "{hit:?}");
        f.click(&mut app, at);
        assert!(app.session.last_point.dist(back.xy()) < 1e-9 && app.session.point_z.is_some_and(|z| (z - back.z).abs() < 1e-9));
    }

    /// In Rhino's four views the Front tile draws on the Front construction plane: a point clicked
    /// there arrives in its coordinates (world X and Z), with no height; the grid and the 3D UCS
    /// icon follow the plane, and the status bar shows the CPlane coordinates.
    #[test]
    fn points_in_a_front_cplane_tile_arrive_in_its_coordinates() {
        let g = cadcraft_geom::Vec3::new;
        let mut f = Frames::default();
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        f.run(&mut app, Vec::new());
        app.session.execute("viewports.rhino", &serde_json::json!({})).unwrap();
        let front = app.session.model_tiles().iter().position(|t| t.look.dir.dir == g(0.0, -1.0, 0.0)).unwrap();
        app.session.activate_tile(front).unwrap();
        assert_eq!(app.session.cplane(), front_cplane());
        let st = app.session.state_mut().unwrap();
        let center = st.view_dir.project(g(3.0, 0.0, 7.0));
        st.set_view_quiet(cadcraft_engine::View { center, height: 60.0 });
        rebuild_now(&mut f, &mut app);
        let xf = app.canvas.xf.unwrap();
        assert!(!xf.is_plan() && !xf.edge_on() && xf.cplane() == front_cplane());
        app.session.cmdline("line").unwrap();
        let at = xf.world_to_screen(g(3.0, 0.0, 7.0));
        f.click(&mut app, at);
        assert!(app.session.last_point.dist(Vec2::new(3.0, 7.0)) < 1e-6, "{:?}", app.session.last_point);
        assert_eq!(app.session.point_z, None);
        assert!(app.canvas.cursor.is_some_and(|c| c.dist(Vec2::new(3.0, 7.0)) < 1e-6));
    }

    /// With the Front construction plane in an isometric view, a raised circle's centre snaps in
    /// the plane's coordinates (world X, world Z) with its height along the plane's normal (-Y).
    #[test]
    fn a_3d_snap_is_given_in_the_construction_plane_coordinates() {
        let mut f = Frames::default();
        let c = cadcraft_geom::Vec3::new(3.0, 4.0, 6.0);
        let mut app = app_snapping(&mut f, [-1.0, -1.0, 1.0], c, snap::mode::CEN);
        app.session.cancel();
        app.session.execute("cplane", &serde_json::json!({ "preset": "front" })).unwrap();
        let circle = cadcraft_doc::Circle { center: c, radius: 2.0 };
        app.session
            .doc_mut()
            .unwrap()
            .add(&cadcraft_doc::Space::Model, cadcraft_doc::Common::default(), cadcraft_doc::EntityKind::Circle(circle))
            .unwrap();
        app.session.cmdline("line").unwrap();
        rebuild_now(&mut f, &mut app);
        let xf = app.canvas.xf.unwrap();
        assert_eq!(xf.cplane(), front_cplane());
        let at = xf.world_to_screen(c) + vec2(2.0, -3.0);
        f.run(&mut app, vec![egui::Event::PointerMoved(at)]);
        let hit = app.canvas.snap.unwrap();
        assert!(
            hit.point.dist(Vec2::new(3.0, 6.0)) < 1e-9 && app.canvas.snap_z.is_some_and(|z| (z + 4.0).abs() < 1e-9),
            "{hit:?} {:?}",
            app.canvas.snap_z
        );
        // The marker is drawn where the centre is.
        assert!(xf.local_to_screen(hit.point.to3(-4.0)).distance(xf.world_to_screen(c)) < 1e-3);
        f.click(&mut app, at);
        assert!(app.session.last_point.dist(Vec2::new(3.0, 6.0)) < 1e-9);
        assert!(app.session.point_z.is_some_and(|z| (z + 4.0).abs() < 1e-9));
    }

    fn add(app: &mut CadApp, kind: cadcraft_doc::EntityKind) -> Handle {
        app.session.doc_mut().unwrap().add(&cadcraft_doc::Space::Model, cadcraft_doc::Common::default(), kind).unwrap()
    }

    /// A UCS moved and turned in plan view keeps AutoCAD's 2D canvas: no gumball, the 2D UCS icon
    /// (turned with the UCS, without the WCS square), grips where the engine has them (in the
    /// UCS's coordinates), the rollover pick there too; points still arrive in UCS coordinates.
    #[test]
    fn a_ucs_in_plan_keeps_autocad_ways() {
        let g = cadcraft_geom::Vec3::new;
        let mut f = Frames::default();
        let mut app = app_looking(&mut f, [0.0, 0.0, 1.0]);
        app.session.execute("view.set", &serde_json::json!({ "center": [5, 10], "height": 60 })).unwrap();
        // Origin (5, 5), x up world Y: UCS (x, y) is world (5 - y, 5 + x).
        app.session.execute("cplane", &serde_json::json!({ "origin": [5, 5, 0], "xaxis": [0, 1, 0] })).unwrap();
        let h = add(&mut app, cadcraft_doc::EntityKind::Line(cadcraft_doc::Line { a: g(5.0, 5.0, 0.0), b: g(5.0, 15.0, 0.0) }));
        app.session.set_selection(vec![h]);
        rebuild_now(&mut f, &mut app);
        let xf = app.canvas.xf.unwrap();
        assert!(xf.is_plan() && xf.cplane() != Mat4::IDENTITY);
        let grips = app.session.grips_of(h).unwrap();
        assert!(grips[2].dist(Vec2::new(10.0, 0.0)) < 1e-9, "{grips:?}");
        let end = xf.world_to_screen(g(5.0, 15.0, 0.0));
        assert!(xf.to_screen(grips[2]).distance(end) < 1e-3);
        assert_eq!(grip_at(&app, &xf, Some(end)), Some((h, 2)));
        let (o, arms) = ucs_icon_arms(xf.rect, &xf);
        assert_eq!(arms.len(), 2, "the 2D icon");
        assert!(arms[0].1.distance(o + vec2(0.0, -60.0)) < 1e-3, "its X arm points up the screen with the UCS");
        // No gumball: a drag from where its X arrow would be opens a window and moves nothing.
        let mid = xf.world_to_screen(g(5.0, 10.0, 0.0));
        f.drag(&mut app, mid + vec2(60.0, 0.0), mid + vec2(110.0, 20.0));
        assert_eq!(line_ends(&app, h), (g(5.0, 5.0, 0.0), g(5.0, 15.0, 0.0)));
        // The rollover finds the line where plan draws it.
        app.session.set_selection(Vec::new());
        f.run(&mut app, vec![egui::Event::PointerMoved(mid + vec2(0.0, -20.0))]);
        assert_eq!(app.canvas.hover, Some(h));
        // A typed snap override snaps in the UCS too: the line's end (10, 0).
        app.session.cmdline("line").unwrap();
        app.session.cmdline("end").unwrap();
        f.run(&mut app, vec![egui::Event::PointerMoved(end + vec2(2.0, 1.0))]);
        assert!(app.canvas.snap.is_some_and(|s| s.point.dist(Vec2::new(10.0, 0.0)) < 1e-9), "{:?}", app.canvas.snap);
        app.session.cancel();
        // A point clicked arrives in UCS coordinates.
        app.session.cmdline("line").unwrap();
        f.click(&mut app, xf.world_to_screen(g(1.0, 8.0, 0.0)));
        assert!(app.session.last_point.dist(Vec2::new(3.0, 4.0)) < 1e-3, "{:?}", app.session.last_point);
    }

    /// A polyline 20 above the ground, seen from the south-west, selected.
    fn raised_polyline(f: &mut Frames) -> (CadApp, Handle) {
        let mut app = app_looking(f, [-1.0, -1.0, 1.0]);
        let st = app.session.state_mut().unwrap();
        let center = st.view_dir.project(cadcraft_geom::Vec3::new(5.0, 0.0, 10.0));
        st.set_view_quiet(cadcraft_engine::View { center, height: 80.0 });
        let vertices = vec![cadcraft_geom::PolyVertex::new(Vec2::ZERO), cadcraft_geom::PolyVertex::new(Vec2::new(10.0, 0.0))];
        let p = cadcraft_doc::LwPolyline { vertices, closed: false, const_width: 0.0, elevation: 20.0, plinegen: false };
        let h = add(&mut app, cadcraft_doc::EntityKind::LwPolyline(p));
        rebuild_now(f, &mut app);
        (app, h)
    }

    /// In a 3D view a raised object's grips are where it is drawn, not on its shadow at z = 0, and
    /// a grip dragged there follows the cursor at the object's height.
    #[test]
    fn a_raised_objects_grips_are_where_it_is_drawn() {
        let g = cadcraft_geom::Vec3::new;
        let mut f = Frames::default();
        let (mut app, h) = raised_polyline(&mut f);
        app.session.settings.osmode = 0;
        app.session.settings.polarmode = false;
        app.session.set_selection(vec![h]);
        f.run(&mut app, Vec::new());
        let xf = app.canvas.xf.unwrap();
        let corner = xf.world_to_screen(g(10.0, 0.0, 20.0));
        assert_eq!(grip_at(&app, &xf, Some(corner)), Some((h, 2)));
        assert_eq!(grip_at(&app, &xf, Some(xf.world_to_screen(g(10.0, 0.0, 0.0)))), None, "nothing on the shadow");
        f.click(&mut app, corner);
        assert_eq!(app.session.running.as_ref().map(|r| r.id.as_str()), Some("grip"));
        let to = corner + vec2(30.0, -10.0);
        let want = xf.ray_on(to, g(0.0, 0.0, 20.0), cadcraft_geom::Vec3::Z).unwrap();
        // The rubber band runs at the polyline's height, from the grip to the cursor.
        let (a, b) = grip_band(&mut f, &mut app, to);
        assert!(a.distance(corner) < 1e-2 && b.distance(to) < 1e-2, "{a:?} {b:?}");
        f.click(&mut app, to);
        let d = app.session.doc().unwrap();
        let cadcraft_doc::EntityKind::LwPolyline(p) = &d.entity(h).unwrap().kind else { panic!() };
        assert!(p.vertices[1].p.dist(want.xy()) < 1e-6 && p.elevation == 20.0, "{:?} {} want {want:?}", p.vertices[1].p, p.elevation);
        assert!(p.vertices[0].p == Vec2::ZERO);
    }

    /// With the cursor at `to` during a grip edit, where the grip edit's rubber band (the preview's
    /// last line, ending at the cursor) starts and ends on screen.
    fn grip_band(f: &mut Frames, app: &mut CadApp, to: Pos2) -> (Pos2, Pos2) {
        f.run(app, vec![egui::Event::PointerMoved(to)]);
        let (xf, c) = (app.canvas.xf.unwrap(), app.canvas.cursor.unwrap());
        let band = lift_grip_band(app, &xf, c, app.session.preview(c));
        let Some(cadcraft_doc::EntityKind::Line(l)) = band.last().map(|e| &e.kind) else { panic!("{band:?}") };
        assert_eq!(l.b.xy(), c);
        (xf.local_to_screen(l.a), xf.local_to_screen(l.b))
    }

    /// In a 3D view a tilted object's grip lands under the cursor: the cursor moves it in the
    /// object's own plane, where the engine takes the point given; its rubber band runs from the
    /// grip to there.
    #[test]
    fn a_tilted_objects_grip_lands_under_the_cursor() {
        let g = cadcraft_geom::Vec3::new;
        let mut f = Frames::default();
        let mut app = app_looking(&mut f, [-1.0, -1.0, 1.0]);
        app.session.settings.osmode = 0;
        app.session.settings.polarmode = false;
        let st = app.session.state_mut().unwrap();
        let center = st.view_dir.project(g(5.0, 0.0, 5.0));
        st.set_view_quiet(cadcraft_engine::View { center, height: 80.0 });
        // Tilted 45° about world X, 5 along its normal.
        let vertices = vec![cadcraft_geom::PolyVertex::new(Vec2::ZERO), cadcraft_geom::PolyVertex::new(Vec2::new(10.0, 0.0))];
        let p = cadcraft_doc::LwPolyline { vertices, closed: false, const_width: 0.0, elevation: 5.0, plinegen: false };
        let common = cadcraft_doc::Common { extrusion: g(0.0, -1.0, 1.0).normalized(), ..Default::default() };
        let h = app.session.doc_mut().unwrap().add(&cadcraft_doc::Space::Model, common, cadcraft_doc::EntityKind::LwPolyline(p)).unwrap();
        app.session.set_selection(vec![h]);
        rebuild_now(&mut f, &mut app);
        let xf = app.canvas.xf.unwrap();
        let plane = app.session.doc().unwrap().entity(h).unwrap().plane().unwrap();
        let corner = xf.world_to_screen(plane.apply(g(10.0, 0.0, 0.0)));
        assert_eq!(grip_at(&app, &xf, Some(corner)).map(|(o, _)| o), Some(h));
        f.click(&mut app, corner);
        assert_eq!(app.session.running.as_ref().map(|r| r.id.as_str()), Some("grip"));
        let to = corner + vec2(30.0, -10.0);
        let (a, b) = grip_band(&mut f, &mut app, to);
        assert!(a.distance(corner) < 1e-2 && b.distance(to) < 1e-2, "{a:?} {b:?}");
        f.click(&mut app, to);
        let d = app.session.doc().unwrap();
        let e = d.entity(h).unwrap();
        let cadcraft_doc::EntityKind::LwPolyline(p) = &e.kind else { panic!() };
        let at = xf.world_to_screen(e.plane().unwrap().apply(p.vertices[1].p.to3(0.0)));
        assert!(at.distance(to) < 1e-2, "{at:?} want {to:?}");
        assert_eq!(p.vertices[0].p, Vec2::ZERO);
    }

    /// In a 3D view ** MOVE ** (Space at a grip) moves a tilted object level with the construction
    /// plane, as the engine does, so its grip lands under the cursor: the cursor moves the grip
    /// level through it, not in the object's own plane (where only STRETCH takes it).
    #[test]
    fn a_grip_move_of_a_tilted_object_lands_under_the_cursor() {
        let g = cadcraft_geom::Vec3::new;
        let mut f = Frames::default();
        let mut app = app_looking(&mut f, [-1.0, -1.0, 1.0]);
        app.session.settings.osmode = 0;
        app.session.settings.polarmode = false;
        let st = app.session.state_mut().unwrap();
        let center = st.view_dir.project(g(5.0, 0.0, 5.0));
        st.set_view_quiet(cadcraft_engine::View { center, height: 80.0 });
        let vertices = vec![cadcraft_geom::PolyVertex::new(Vec2::ZERO), cadcraft_geom::PolyVertex::new(Vec2::new(10.0, 0.0))];
        let p = cadcraft_doc::LwPolyline { vertices, closed: false, const_width: 0.0, elevation: 5.0, plinegen: false };
        let common = cadcraft_doc::Common { extrusion: g(0.0, -1.0, 1.0).normalized(), ..Default::default() };
        let h = app.session.doc_mut().unwrap().add(&cadcraft_doc::Space::Model, common, cadcraft_doc::EntityKind::LwPolyline(p)).unwrap();
        app.session.set_selection(vec![h]);
        rebuild_now(&mut f, &mut app);
        let xf = app.canvas.xf.unwrap();
        let ends = |app: &CadApp| {
            let d = app.session.doc().unwrap();
            let e = d.entity(h).unwrap();
            let cadcraft_doc::EntityKind::LwPolyline(p) = &e.kind else { panic!() };
            let pl = e.plane().unwrap();
            (pl.apply(p.vertices[0].p.to3(0.0)), pl.apply(p.vertices[1].p.to3(0.0)))
        };
        let (a0, b0) = ends(&app);
        let corner = xf.world_to_screen(b0);
        f.click(&mut app, corner);
        // STRETCH's Base point is taken on the object's plane, as its stretch point is.
        app.session.input(Input::Keyword("Base point".into())).unwrap();
        let q = app.session.current_prompt().map(|q| q.message);
        assert!(app.canvas.grip_plane.is_some() && grip_drag_plane(&app, &xf) == app.canvas.grip_plane, "{q:?}");
        app.session.input(Input::Enter).unwrap();
        app.session.input(Input::Enter).unwrap();
        assert!(app.session.current_prompt().is_some_and(|q| q.message.starts_with("** MOVE **")));
        // MOVE's is taken level with the construction plane, as its move point is.
        app.session.input(Input::Keyword("Base point".into())).unwrap();
        assert_ne!(grip_drag_plane(&app, &xf), app.canvas.grip_plane);
        app.session.input(Input::Enter).unwrap();
        let to = corner + vec2(30.0, -10.0);
        let (a, b) = grip_band(&mut f, &mut app, to);
        assert!(a.distance(corner) < 1e-2 && b.distance(to) < 1e-2, "{a:?} {b:?}");
        f.click(&mut app, to);
        let (a1, b1) = ends(&app);
        assert!(xf.world_to_screen(b1).distance(to) < 1e-2, "{:?} want {to:?}", xf.world_to_screen(b1));
        assert!(((a1 - a0) - (b1 - b0)).len() < 1e-9 && (b1.z - b0.z).abs() < 1e-9, "moved whole, level");
    }

    /// In plan with a tilted construction plane a grip off the plane lands under the cursor (the
    /// cursor moves it level through it, as in a 3D view), and a single-object prompt picks what
    /// is drawn under the cursor, as in a 3D view: DIMLINEAR's "Select object to dimension" takes
    /// the line clicked off the plane, where its shadow on the plane isn't.
    #[test]
    fn plan_with_a_tilted_cplane_drags_grips_under_the_cursor_and_picks_what_is_drawn() {
        let r = 30f64.to_radians();
        let mut f = Frames::default();
        let mut app = app_looking(&mut f, [0.0, 0.0, 1.0]);
        app.session.execute("view.set", &serde_json::json!({ "center": [0, 0], "height": 60 })).unwrap();
        let plane = serde_json::json!({ "origin": [3, 2, 1], "xaxis": [-1, 0, 0], "ypoint": [3, 2.0 - r.cos(), 1.0 + r.sin()] });
        app.session.execute("cplane", &plane).unwrap();
        app.session.settings.osmode = 0;
        app.session.settings.polarmode = false;
        f.run(&mut app, Vec::new());
        let xf = app.canvas.xf.unwrap();
        assert!(xf.is_plan() && xf.picks_drawn());
        let c = xf.rect.center();
        // A line in the plane, and one 5 above it.
        let at = |s: Pos2, z: f64| xf.cplane().apply(xf.to_world(s).to3(z));
        let line = |app: &mut CadApp, z: f64| {
            add(app, cadcraft_doc::EntityKind::Line(cadcraft_doc::Line { a: at(c + vec2(-80.0, 0.0), z), b: at(c + vec2(80.0, 0.0), z) }))
        };
        let flat = line(&mut app, 0.0);
        let raised = line(&mut app, 5.0);
        rebuild_now(&mut f, &mut app);
        app.session.set_selection(vec![raised]);
        f.run(&mut app, Vec::new());
        let end = |app: &CadApp| match &app.session.doc().unwrap().entity(raised).unwrap().kind {
            cadcraft_doc::EntityKind::Line(l) => l.b,
            k => panic!("{k:?}"),
        };
        let grip = xf.world_to_screen(end(&app));
        f.click(&mut app, grip);
        assert_eq!(app.session.running.as_ref().map(|r| r.id.as_str()), Some("grip"));
        let to = grip + vec2(-30.0, 40.0);
        f.click(&mut app, to);
        assert!(xf.world_to_screen(end(&app)).distance(to) < 1e-2, "{:?} want {to:?}", xf.world_to_screen(end(&app)));
        app.session.set_selection(Vec::new());
        app.session.cmdline("dimlinear").unwrap();
        app.session.input(Input::Enter).unwrap();
        assert_eq!(app.session.current_prompt().map(|q| q.message), Some("Select object to dimension".to_string()));
        let a = match &app.session.doc().unwrap().entity(raised).unwrap().kind {
            cadcraft_doc::EntityKind::Line(l) => l.a,
            k => panic!("{k:?}"),
        };
        let on_raised = xf.world_to_screen(a + (end(&app) - a) * 0.2);
        f.click(&mut app, on_raised);
        let q = app.session.current_prompt().map(|q| q.message).unwrap_or_default();
        assert!(q.starts_with("Specify dimension line location"), "{q} ({flat:?})");
    }

    /// The DOOR block of the engine's dynamic grip tests (its Width grip, node 2, at own (136, -4)),
    /// referenced at (100, 0); osnaps and polar off.
    fn door(f: &mut Frames) -> (CadApp, Handle) {
        let grip =
            "0 BLOCKLINEARGRIP\n5 68\n330 61\n100 AcDbEvalExpr\n90 9\n98 33\n99 232\n100 AcDbBlockElement\n300 End Grip\n98 33\n99 232\n1071 0\n";
        let src = include_str!("../../io/tests/data/dynblock_door.txt")
            .replace("\r\n", "\n")
            .replace("170 4\n91 0\n91 0\n", "170 4\n91 8\n91 9\n")
            .replace("1030 0\n93 0\n170 0\n171 0\n100 AcDbBlockVisibilityParameter", "1030 0\n93 10\n170 0\n171 0\n100 AcDbBlockVisibilityParameter")
            .replace("91 5\n93 32\n95 6\n360 67", "91 5\n93 32\n95 6\n360 67\n92 -1\n92 -1\n92 -1\n92 -1\n91 6\n93 32\n95 9\n360 68")
            .replace("95 1\n333 63\n", "95 2\n333 63\n333 68\n")
            .replace("0 BLOCKFLIPGRIP", &format!("{grip}0 BLOCKFLIPGRIP"));
        let text: String = src
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| {
                let (code, value) = l.split_once(' ').unwrap_or((l, ""));
                format!("{code}\n{value}\n")
            })
            .collect();
        let mut app = app_looking(f, [0.0, 0.0, 1.0]);
        app.session.open_drawing(cadcraft_io::read_dxf(text.as_bytes()).unwrap(), "door", None);
        let r = app.session.execute("insert", &serde_json::json!({ "name": "Door", "at": [100, 0] })).unwrap();
        app.session.settings.osmode = 0;
        app.session.settings.polarmode = false;
        (app, Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap())
    }

    fn door_width(app: &mut CadApp, h: Handle) -> String {
        let r = app.session.execute("blocks.reference", &serde_json::json!({ "handle": h.hex() })).unwrap();
        r["properties"].as_array().unwrap().iter().find(|p| p["name"] == "Width").and_then(|p| p["value"].as_str()).unwrap_or_default().to_string()
    }

    /// Under a level UCS (moved and turned) a dynamic grip drags in plan as in World: the Width
    /// grip dropped on world (130.4, 7) gives Width 30. A tilted construction plane refuses the
    /// drag and leaves the drawing as it was.
    #[test]
    fn dynamic_grips_drag_under_a_level_ucs() {
        let g = cadcraft_geom::Vec3::new;
        let mut f = Frames::default();
        let (mut app, h) = door(&mut f);
        app.session.execute("view.set", &serde_json::json!({ "center": [120, 0], "height": 80 })).unwrap();
        app.session.execute("cplane", &serde_json::json!({ "origin": [5, 5, 0], "xaxis": [0, 1, 0] })).unwrap();
        app.session.set_selection(vec![h]);
        f.run(&mut app, Vec::new());
        let xf = app.canvas.xf.unwrap();
        assert!(xf.is_plan() && xf.cplane() != Mat4::IDENTITY);
        f.click(&mut app, xf.world_to_screen(g(136.0, -4.0, 0.0)));
        assert_eq!(app.session.running.as_ref().map(|r| r.id.as_str()), Some("grip"));
        f.click(&mut app, xf.world_to_screen(g(130.4, 7.0, 0.0)));
        assert_eq!(door_width(&mut app, h), "30.0000");
        // A plane tilted out of the plan view's: refused, nothing changes.
        app.session.execute("cplane", &serde_json::json!({ "preset": "front" })).unwrap();
        app.session.set_selection(vec![h]);
        f.run(&mut app, Vec::new());
        let xf = app.canvas.xf.unwrap();
        f.click(&mut app, xf.world_to_screen(g(130.0, -4.0, 0.0)));
        assert!(app.session.running.is_none(), "{:?}", app.session.log.last());
        assert_eq!(door_width(&mut app, h), "30.0000");
    }

    /// In a 3D view a tilted reference's dynamic grip sits on its plane and drags there: dropped
    /// where its own point (130.4, 7) is drawn it gives Width 30, as dropped there in plan.
    #[test]
    fn a_tilted_references_dynamic_grip_drags_on_its_plane() {
        let mut f = Frames::default();
        let (mut app, h) = door(&mut f);
        app.session.doc_mut().unwrap().modify_entity(h, |e| e.common.extrusion = cadcraft_geom::Vec3::new(0.0, -0.6, 0.8)).unwrap();
        let st = app.session.state_mut().unwrap();
        st.view_dir = ViewDir::new(cadcraft_geom::Vec3::new(-1.0, -1.0, 1.0));
        let center = st.view_dir.project(cadcraft_geom::Vec3::new(130.0, 0.0, 0.0));
        st.set_view_quiet(cadcraft_engine::View { center, height: 40.0 });
        app.session.set_selection(vec![h]);
        rebuild_now(&mut f, &mut app);
        let xf = app.canvas.xf.unwrap();
        assert!(!xf.is_plan());
        let plane = app.session.doc().unwrap().entity(h).unwrap().plane().unwrap();
        let on = |own: Vec2| xf.world_to_screen(plane.apply(own.to3(0.0)));
        f.click(&mut app, on(Vec2::new(136.0, -4.0)));
        assert_eq!(app.session.running.as_ref().map(|r| r.id.as_str()), Some("grip"));
        f.click(&mut app, on(Vec2::new(130.4, 7.0)));
        assert_eq!(door_width(&mut app, h), "30.0000");
    }

    /// In plan with a turned UCS a selection window takes what is in the rectangle drawn on screen:
    /// a window dragged to the right, a crossing window to the left, as AutoCAD selects; not a box
    /// in UCS coordinates, where with the UCS turned 180° a drag to the right would cross. Click,
    /// click at "Select objects" too; turned 30°, and with a construction plane tilted 30° (what
    /// is drawn in the rectangle).
    #[test]
    fn a_turned_ucs_selects_the_window_drawn_on_screen() {
        let r = 30f64.to_radians();
        for plane in [
            serde_json::json!({ "origin": [3, 2, 0], "xaxis": [-1, 0, 0] }),
            serde_json::json!({ "origin": [3, 2, 0], "xaxis": [r.cos(), r.sin(), 0] }),
            serde_json::json!({ "origin": [3, 2, 1], "xaxis": [-1, 0, 0], "ypoint": [3, 2.0 - r.cos(), 1.0 + r.sin()] }),
        ] {
            let mut f = Frames::default();
            let mut app = app_looking(&mut f, [0.0, 0.0, 1.0]);
            app.session.execute("view.set", &serde_json::json!({ "center": [0, 0], "height": 60 })).unwrap();
            app.session.execute("cplane", &plane).unwrap();
            f.run(&mut app, Vec::new());
            let xf = app.canvas.xf.unwrap();
            assert!(xf.is_plan() && !xf.flat());
            let c = xf.rect.center();
            let world = |s: Pos2| xf.cplane().apply(xf.to_world(s).to3(0.0));
            let line = |app: &mut CadApp, s: Pos2, e: Pos2| add(app, cadcraft_doc::EntityKind::Line(cadcraft_doc::Line { a: world(s), b: world(e) }));
            // `a` inside the window, `b` across its right side.
            let a = line(&mut app, c + vec2(-50.0, -20.0), c + vec2(50.0, 0.0));
            let b = line(&mut app, c + vec2(0.0, 60.0), c + vec2(250.0, 60.0));
            rebuild_now(&mut f, &mut app);
            f.drag(&mut app, c + vec2(-100.0, -60.0), c + vec2(150.0, 100.0));
            assert_eq!(app.session.selection(), vec![a], "{plane}");
            app.session.set_selection(Vec::new());
            f.drag(&mut app, c + vec2(150.0, 100.0), c + vec2(-100.0, -60.0));
            let mut sel = app.session.selection();
            sel.sort();
            assert_eq!(sel, vec![a, b], "{plane}");
            app.session.set_selection(Vec::new());
            app.session.cmdline("erase").unwrap();
            f.click(&mut app, c + vec2(-100.0, -60.0));
            assert!(app.session.pending_window.is_some());
            f.click(&mut app, c + vec2(150.0, 100.0));
            assert!(app.session.pending_window.is_none());
            app.session.input(Input::Enter).unwrap();
            let d = app.session.doc().unwrap();
            assert!(d.entity(a).is_none() && d.entity(b).is_some(), "{plane}");
        }
    }

    /// In a 3D view an object off the construction plane gives no object snap on its shadow (the
    /// engine snaps in the plane): the snap is where it is drawn, with its height.
    #[test]
    fn a_raised_objects_shadow_gives_no_snap() {
        let g = cadcraft_geom::Vec3::new;
        let mut f = Frames::default();
        let (mut app, _) = raised_polyline(&mut f);
        app.session.settings.osmode = snap::mode::END;
        app.session.cmdline("line").unwrap();
        f.run(&mut app, Vec::new());
        let xf = app.canvas.xf.unwrap();
        f.run(&mut app, vec![egui::Event::PointerMoved(xf.world_to_screen(g(10.0, 0.0, 0.0)) + vec2(1.0, 1.0))]);
        assert_eq!(app.canvas.snap, None, "{:?}", app.canvas.snap);
        f.run(&mut app, vec![egui::Event::PointerMoved(xf.world_to_screen(g(10.0, 0.0, 20.0)) + vec2(1.0, 1.0))]);
        assert!(app.canvas.snap.is_some_and(|s| s.point.dist(Vec2::new(10.0, 0.0)) < 1e-9), "{:?}", app.canvas.snap);
        assert!(app.canvas.snap_z.is_some_and(|z| (z - 20.0).abs() < 1e-9));
        // A Tab-cycled or typed snap override looks only in the plane too.
        app.session.cmdline("end").unwrap();
        f.run(&mut app, vec![egui::Event::PointerMoved(xf.world_to_screen(g(10.0, 0.0, 0.0)) + vec2(1.0, 1.0))]);
        assert_eq!(app.canvas.snap, None);
    }

    /// A grip under a gumball handle stays AutoCAD's: a click on a line's end grip (on the X
    /// arrow) starts stretching it, it doesn't move the line.
    #[test]
    fn grips_win_over_gumball_handles() {
        let g = cadcraft_geom::Vec3::new;
        let mut f = Frames::default();
        let (mut app, h, _) = gumball_on_a_line(&mut f, [0.0, -1.0, 0.0]);
        let st = app.session.state_mut().unwrap();
        let center = st.view_dir.project(g(5.0, 0.0, 0.0));
        st.set_view_quiet(cadcraft_engine::View { center, height: 100.0 });
        f.run(&mut app, Vec::new());
        let xf = app.canvas.xf.unwrap();
        let (o, end) = (xf.world_to_screen(g(5.0, 0.0, 0.0)), xf.world_to_screen(g(10.0, 0.0, 0.0)));
        assert!(end.x - o.x > 20.0 && end.x - o.x < 75.0, "the end grip is on the X arrow");
        f.click(&mut app, end);
        assert_eq!(app.session.running.as_ref().map(|r| r.id.as_str()), Some("grip"));
        f.click(&mut app, end + vec2(30.0, 0.0));
        let (a, b) = line_ends(&app, h);
        assert_eq!(a, cadcraft_geom::Vec3::ZERO, "stretched, not moved");
        assert!(b.x > 10.5, "{b:?}");
    }

    /// Every key during a gumball drag is the gumball's: Space doesn't repeat the last command and
    /// letters don't reach the command line; letting go still moves the selection.
    #[test]
    fn keys_under_a_gumball_drag_are_its_own() {
        let mut f = Frames::default();
        let (mut app, h, o) = gumball_on_a_line(&mut f, [0.0, -1.0, 0.0]);
        app.session.last_command = Some("line".into());
        let from = o + vec2(60.0, 0.0);
        f.run(&mut app, vec![egui::Event::PointerMoved(from)]);
        f.button(&mut app, from, true);
        f.run(&mut app, vec![egui::Event::PointerMoved(from + vec2(50.0, 0.0))]);
        assert!(app.canvas.gumball.dragging());
        let space = egui::Event::Key { key: egui::Key::Space, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        f.run(&mut app, vec![space, egui::Event::Text(" ".into()), egui::Event::Text("x".into())]);
        assert!(app.session.running.is_none(), "Space didn't repeat LINE");
        assert!(app.cmd.buffer.is_empty());
        f.button(&mut app, from + vec2(50.0, 0.0), false);
        f.run(&mut app, Vec::new());
        assert!(line_ends(&app, h).0.x > 0.0, "the drag moved the line");
    }

    /// The UCS icon in a 3D view is the construction plane's three axes; in plan with the World
    /// CPlane AutoCAD's 2D icon, right and up.
    #[test]
    fn the_ucs_icon_shows_the_construction_plane() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0));
        let plan = Xf::new(rect, Vec2::ZERO, 10.0);
        let (o, arms) = ucs_icon_arms(rect, &plan);
        assert_eq!(arms.iter().map(|(_, t)| *t - o).collect::<Vec<_>>(), vec![vec2(60.0, 0.0), vec2(0.0, -60.0)]);
        let front = plan.looking(ViewDir::new(cadcraft_geom::Vec3::new(0.0, -1.0, 0.0))).on(front_cplane());
        let (o, arms) = ucs_icon_arms(rect, &front);
        assert_eq!(arms.len(), 3);
        // Front's x is right, its y up the screen and its normal (toward -Y) at the viewer: a dot.
        assert!((arms[0].1 - o - vec2(50.0, 0.0)).length() < 1e-3 && (arms[1].1 - o - vec2(0.0, -50.0)).length() < 1e-3);
        assert!((arms[2].1 - o).length() < 1e-3);
    }

    /// The viewport label and the ViewCube name the view and the visual style shown (#60).
    #[test]
    fn the_viewport_label_names_the_view_and_style() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        let ctx = egui::Context::default();
        let at = pos2(700.0, 450.0);
        assert!(painted_text(&mut app, &ctx, at).contains("+  |  Top  |  2D Wireframe"));
        app.session.state_mut().unwrap().view_dir = ViewDir::new(cadcraft_geom::Vec3::new(-1.0, -1.0, 1.0));
        app.session.state_mut().unwrap().shaded = true;
        app.canvas.list = None;
        let text = painted_text(&mut app, &ctx, at);
        assert!(text.contains("+  |  SW Isometric  |  Shaded"), "{text}");
        app.session.state_mut().unwrap().view_dir = ViewDir::new(cadcraft_geom::Vec3::new(0.2, -1.0, 0.1));
        app.canvas.list = None;
        let text = painted_text(&mut app, &ctx, at);
        // (The ViewCube's face labels are meshes laid on its faces: `viewcube` tests them.)
        assert!(text.contains("+  |  Custom View  |  Shaded"), "{text}");
        app.session.state_mut().unwrap().shaded_edges = true;
        let text = painted_text(&mut app, &ctx, at);
        assert!(text.contains("+  |  Custom View  |  Shaded with Edges"), "{text}");
        // A perspective view is Perspective, as in Rhino.
        app.session.state_mut().unwrap().camera = Some(cadcraft_geom::Camera::new(50.0, 0.0));
        let text = painted_text(&mut app, &ctx, at);
        assert!(text.contains("+  |  Perspective  |  Shaded with Edges"), "{text}");
    }
}

/// The Block Editor's tint, base point and bar.
mod blockedit;
/// The COMPARE overlay.
mod compare;
/// Raster images' pictures as textures.
mod images;

/// Background rebuilds of the display list for a new view.
mod rebuild;

/// Object snaps on solids and curves in 3D views.
mod snap3d;

/// Rhino's gumball in 3D views.
mod gumball;

/// Rhino-style model tiles: their own lists, titles and right-drag navigation.
mod tiles;

#[cfg(test)]
mod persp_tests;

#[cfg(test)]
mod hatch_pick_tests {
    use cadcraft_engine::Session;
    use cadcraft_geom::Vec2;
    use serde_json::json;

    use crate::{CadApp, Services};

    #[test]
    fn hatch_at_finds_a_hatch_under_the_cursor_only() {
        let mut app = CadApp::new(Session::new(), Services::default());
        app.session.execute("rectang", &json!({ "p1": [0, 0], "p2": [10, 10] })).unwrap();
        let hx = app.session.execute("hatch", &json!({ "points": [[5, 5]], "pattern": "SOLID" })).unwrap()["handle"].as_str().unwrap().to_string();
        app.session.execute("line", &json!({ "points": [[20, 0], [30, 0]] })).unwrap();
        assert_eq!(super::hatch_at(&app, Vec2::new(5.0, 5.0), 10.0).map(|h| h.hex()), Some(hx.clone()));
        assert_eq!(super::hatch_at(&app, Vec2::new(25.0, 0.0), 10.0), None, "a line is not a hatch");
        assert_eq!(super::hatch_at(&app, Vec2::new(60.0, 60.0), 10.0), None);
        // With a UCS moved to (100, 100) the point is in its coordinates.
        app.session.execute("cplane", &json!({ "origin": [100, 100, 0] })).unwrap();
        assert_eq!(super::hatch_at(&app, Vec2::new(-95.0, -95.0), 10.0).map(|h| h.hex()), Some(hx));
        assert_eq!(super::hatch_at(&app, Vec2::new(5.0, 5.0), 10.0), None);
    }

    #[test]
    fn double_clicking_a_hatch_opens_the_hatch_edit_dialog() {
        let mut app = CadApp::new(Session::new(), Services::default());
        app.session.execute("rectang", &json!({ "p1": [0, 0], "p2": [10, 10] })).unwrap();
        let hx = app.session.execute("hatch", &json!({ "points": [[5, 5]], "pattern": "SOLID" })).unwrap()["handle"].as_str().unwrap().to_string();
        let _ = app.session.zoom_extents();
        let ctx = egui::Context::default();
        let screen = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0)));
        let frame = |app: &mut CadApp, events: Vec<egui::Event>, time: f64| {
            app.logic(&ctx);
            let input = egui::RawInput { screen_rect: screen, events, time: Some(time), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| app.ui(ui));
            out.textures_delta.clear();
        };
        frame(&mut app, Vec::new(), 0.0);
        let at = app.canvas.xf.unwrap().to_screen(Vec2::new(5.0, 5.0));
        let button =
            |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::default() };
        frame(&mut app, vec![egui::Event::PointerMoved(at)], 0.1);
        frame(&mut app, vec![button(true), button(false)], 0.2);
        frame(&mut app, vec![button(true), button(false)], 0.3);
        frame(&mut app, Vec::new(), 0.4);
        assert_eq!(app.ui.dialog.as_deref(), Some("hatch"));
        assert_eq!(app.hatch_dlg.as_ref().map(|s| s.edit.iter().map(|h| h.hex()).collect::<Vec<_>>()), Some(vec![hx]));
        // The double-click (at the hatch's centre grip) only opens the dialog: no grip edit.
        assert!(app.session.running.is_none(), "{:?}", app.session.running.as_ref().map(|r| r.machine.name()));
    }
}
