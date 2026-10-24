//! The 3D view (CAD-rhino): VPOINT, PLAN, the preset views, 3DORBIT, VSCURRENT, and the
//! perspective camera (PERSPECTIVE, LENSLENGTH; DVIEW is [`super::dview`]).
//!
//! The view direction belongs to the Model tab ([`crate::DocState::view_dir`]; layouts and their
//! viewports are drawn in plan); view centres are in its view plane, so a new direction zooms to
//! extents, and an orbit keeps the point at the centre of the screen where it is. A perspective
//! view ([`crate::DocState::camera`]) keeps its target plane: zooming dollies the eye toward it,
//! an orbit turns about the target, and zooming to extents backs the eye off until the model's
//! box is in view.

use cadcraft_doc::{Drawing, Space};
use cadcraft_geom::{Camera, Vec3, ViewDir};
use serde_json::{Value, json};

use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

/// The preset views: name, keyword, direction toward the viewer.
const PRESETS: &[(&str, &str, [f64; 3])] = &[
    ("top", "Top", [0.0, 0.0, 1.0]),
    ("bottom", "Bottom", [0.0, 0.0, -1.0]),
    ("front", "Front", [0.0, -1.0, 0.0]),
    ("back", "BAck", [0.0, 1.0, 0.0]),
    ("left", "Left", [-1.0, 0.0, 0.0]),
    ("right", "Right", [1.0, 0.0, 0.0]),
    ("swiso", "SWiso", [-1.0, -1.0, 1.0]),
    ("seiso", "SEiso", [1.0, -1.0, 1.0]),
    ("neiso", "NEiso", [1.0, 1.0, 1.0]),
    ("nwiso", "NWiso", [-1.0, 1.0, 1.0]),
];

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("vpoint", "Viewpoint", run_vpoint)
            .menu(&["View", "3D Views", "Viewpoint"])
            .alias(&["-vp"])
            .params("{direction: [x,y,z]} | {azimuth, elevation} (degrees) | {preset: top|bottom|front|back|left|right|swiso|seiso|neiso|nwiso}")
            .noundo()
            .interactive(|_| Ok(Box::new(VpointM))),
        CommandSpec::new("plan", "Plan View", |s, _| set(s, ViewDir::PLAN)).menu(&["View", "3D Views", "Plan View"]).noundo(),
        CommandSpec::new("view.top", "Top", |s, _| preset(s, "top")).menu(&["View", "3D Views", "Top"]).noundo(),
        CommandSpec::new("view.bottom", "Bottom", |s, _| preset(s, "bottom")).menu(&["View", "3D Views", "Bottom"]).noundo(),
        CommandSpec::new("view.front", "Front", |s, _| preset(s, "front")).menu(&["View", "3D Views", "Front"]).noundo(),
        CommandSpec::new("view.back", "Back", |s, _| preset(s, "back")).menu(&["View", "3D Views", "Back"]).noundo(),
        CommandSpec::new("view.left", "Left", |s, _| preset(s, "left")).menu(&["View", "3D Views", "Left"]).noundo(),
        CommandSpec::new("view.right", "Right", |s, _| preset(s, "right")).menu(&["View", "3D Views", "Right"]).noundo(),
        CommandSpec::new("view.swiso", "SW Isometric", |s, _| preset(s, "swiso")).menu(&["View", "3D Views", "SW Isometric"]).noundo(),
        CommandSpec::new("view.seiso", "SE Isometric", |s, _| preset(s, "seiso")).menu(&["View", "3D Views", "SE Isometric"]).noundo(),
        CommandSpec::new("view.neiso", "NE Isometric", |s, _| preset(s, "neiso")).menu(&["View", "3D Views", "NE Isometric"]).noundo(),
        CommandSpec::new("view.nwiso", "NW Isometric", |s, _| preset(s, "nwiso")).menu(&["View", "3D Views", "NW Isometric"]).noundo(),
        CommandSpec::new("3dorbit", "Orbit", run_orbit)
            .menu(&["View", "Orbit", "Constrained Orbit"])
            .alias(&["3do", "orbit"])
            .params("{azimuth?, elevation?}: degrees to turn the view by (the canvas orbits with Shift+middle drag)")
            .noundo(),
        CommandSpec::new("vscurrent", "Visual Styles", run_vscurrent)
            .menu(&["View", "Visual Styles", "Shaded"])
            .alias(&["vs"])
            .params("{style: 2dwireframe | wireframe | shaded | shadedwithedges | realistic | conceptual}")
            .noundo()
            .interactive(|_| Ok(Box::new(VscurrentM))),
        CommandSpec::new("vscurrent.wireframe", "2D Wireframe", |s, _| run_vscurrent(s, &json!({ "style": "2dwireframe" })))
            .menu(&["View", "Visual Styles", "2D Wireframe"])
            .noundo(),
        CommandSpec::new("vscurrent.edges", "Shaded with Edges", |s, _| run_vscurrent(s, &json!({ "style": "shadedwithedges" })))
            .menu(&["View", "Visual Styles", "Shaded with Edges"])
            .noundo(),
    ]
}

pub(crate) fn model_only(s: &Session) -> Result<()> {
    if s.layout_space() == Space::Model { Ok(()) } else { Err(EngineError::Other("3D views are available on the Model tab only.".into())) }
}

fn dir_json(v: ViewDir) -> Value {
    json!({ "direction": [v.dir.x, v.dir.y, v.dir.z] })
}

/// Look along `v` and zoom to extents (ZOOM Previous returns to the view and direction before).
pub(crate) fn set(s: &mut Session, v: ViewDir) -> Result<Value> {
    model_only(s)?;
    let st = s.state_mut()?;
    let before = st.view();
    let dir = st.look().projection(before);
    let v = v.parallel();
    st.view_dir = v;
    st.orbit_target = None;
    // PLAN (and the Top view) turns perspective off, as AutoCAD's PLAN does: plan is the 2D
    // drawing area. Other directions keep it, framing the model through the lens.
    if v.is_plan() {
        st.camera = None;
    }
    let f = match persp_extents(s)? {
        Some((f, cam)) => {
            s.state_mut()?.camera = Some(cam);
            f
        }
        None => s.extents_frame()?,
    };
    let st = s.state_mut()?;
    st.push_history(before, dir);
    st.set_view_quiet(f);
    Ok(dir_json(v))
}

fn preset(s: &mut Session, name: &str) -> Result<Value> {
    let [x, y, z] = PRESETS.iter().find(|p| p.0 == name).map(|p| p.2).ok_or_else(|| bad("vpoint", format!("unknown view `{name}`")))?;
    set(s, ViewDir::new(Vec3::new(x, y, z)))
}

fn run_vpoint(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(name) = str_param(p, "preset") {
        return preset(s, &name.to_ascii_lowercase());
    }
    if let Some(a) = p.get("direction").and_then(Value::as_array) {
        let c = |i: usize| a.get(i).and_then(Value::as_f64).filter(|v| v.is_finite());
        let (Some(x), Some(y), Some(z)) = (c(0), c(1), c(2)) else { return Err(bad("vpoint", "`direction` is [x, y, z]")) };
        let d = Vec3::new(x, y, z);
        if d.len() < 1e-12 {
            return Err(bad("vpoint", "the direction must not be zero"));
        }
        return set(s, ViewDir::new(d));
    }
    let az = f64_req("vpoint", p, "azimuth")?;
    let el = f64_req("vpoint", p, "elevation")?;
    set(s, ViewDir::from_angles(az.to_radians(), el.clamp(-90.0, 90.0).to_radians()))
}

/// The model's box in 3D: its plan extents, as low and high as a front view draws it (block
/// contents, meshes, solids and raised or standing objects included; 2D objects lie at z = 0).
/// `None` for an empty model.
pub(crate) fn model_box(d: &Drawing) -> Option<(Vec3, Vec3)> {
    let ext = d.extents(&Space::Model);
    if ext.is_empty() {
        return None;
    }
    // A front view's up is world Z, so its display list's height is the model's.
    let tolerance = (ext.width() + ext.height()).max(1e-9) * 1e-3;
    let opts = cadcraft_render::Options { tolerance, view_dir: ViewDir::new(Vec3::new(0.0, -1.0, 0.0)), ..Default::default() };
    let side = cadcraft_render::build(d, &Space::Model, &opts).bounds;
    let (lo, hi) = if side.is_empty() || !(side.min.y.is_finite() && side.max.y.is_finite()) { (0.0, 0.0) } else { (side.min.y, side.max.y) };
    Some((ext.min.to3(lo), ext.max.to3(hi)))
}

/// The middle of the model in 3D: of its plan extents, halfway up its solids (2D objects lie at
/// z = 0).
pub(crate) fn model_center(d: &Drawing) -> Vec3 {
    model_box(d).map_or(Vec3::ZERO, |(lo, hi)| (lo + hi) * 0.5)
}

/// A perspective with lens `lens` looking along `dir` at the whole model in an area `aspect`
/// times as wide as high ([`persp_fit`] of its box's corners). An empty model is looked at as a
/// unit box at the origin.
pub(crate) fn persp_frame(d: &Drawing, dir: ViewDir, lens: f64, aspect: f64) -> (crate::View, Camera) {
    let (lo, hi) = model_box(d).unwrap_or((Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0)));
    let pick = |i: u32, a: f64, b: f64| if i == 0 { a } else { b };
    let corners: Vec<Vec3> = (0..8u32).map(|i| Vec3::new(pick(i & 1, lo.x, hi.x), pick(i & 2, lo.y, hi.y), pick(i & 4, lo.z, hi.z))).collect();
    persp_fit(&corners, dir, lens, aspect).unwrap_or_else(|| {
        let cam = Camera::new(lens, 0.0);
        (crate::View { center: cadcraft_geom::Vec2::ZERO, height: cam.height_at(2.0) }, cam)
    })
}

/// A perspective with lens `lens` looking along `dir` at `points` in an area `aspect` times as
/// wide as high: the view and camera, its target plane through the middle of their box and its
/// eye backed off until every point is in view, with a little room. `None` without a finite
/// point.
pub(crate) fn persp_fit(points: &[Vec3], dir: ViewDir, lens: f64, aspect: f64) -> Option<(crate::View, Camera)> {
    let dir = dir.parallel();
    let pts: Vec<Vec3> = points.iter().copied().filter(|p| p.is_finite()).collect();
    let first = *pts.first()?;
    let (lo, hi) = pts.iter().fold((first, first), |(lo, hi), p| {
        (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
    });
    let c = (lo + hi) * 0.5;
    let cam = Camera::new(lens, dir.depth(c));
    let cv = dir.view_of(c);
    // Half the view's height per unit of distance, and half its width.
    let tan_h = cadcraft_geom::FILM / 2.0 / cam.lens;
    let tan_w = tan_h * if aspect.is_finite() && aspect > 0.0 { aspect } else { 1.0 };
    let mut need = 0.0f64;
    for p in &pts {
        // Seen from D in front of the target plane, a point (x, y) at depth z shows at
        // (x, y) D / (D - z): in view when D >= z + |y| / tan_h and D >= z + |x| / tan_w.
        let v = dir.view_of(*p) - cv;
        need = need.max(v.z + v.y.abs() / tan_h).max(v.z + v.x.abs() / tan_w);
    }
    let distance = if need.is_finite() && need > 0.0 { need * 1.05 } else { (hi - lo).len().max(1.0) * 2.0 };
    Some((crate::View { center: cv.xy(), height: cam.height_at(distance) }, cam))
}

fn persp_extents(s: &Session) -> Result<Option<(crate::View, Camera)>> {
    s.persp_extents()
}

impl Session {
    /// What ZOOM Extents sets in a perspective view: [`persp_frame`] with the view's lens. `None`
    /// for a parallel view, and outside the Model tab.
    pub(crate) fn persp_extents(&self) -> Result<Option<(crate::View, Camera)>> {
        let st = self.state()?;
        let Some(cam) = st.camera.filter(|_| st.space == Space::Model && st.mspace.is_none()) else { return Ok(None) };
        let (w, h) = self.viewport_px;
        Ok(Some(persp_frame(&st.doc, st.view_dir, cam.lens, st.zoom_aspect(w / h.max(1.0)))))
    }

    /// PERSPECTIVE: turn the current view's perspective on, a camera with lens `lens` (default
    /// the lens it has, else [`Camera::LENS`]) whose target plane passes through the middle of
    /// the model, so the view of the model's middle stays as it was; or off (parallel
    /// projection, keeping the view of the target plane). The Model tab only.
    pub fn set_perspective(&mut self, on: bool, lens: Option<f64>) -> Result<()> {
        if on {
            model_only(self)?;
        }
        let st = self.state_mut()?;
        match (on, st.camera) {
            (true, Some(_)) => {
                if let Some(l) = lens {
                    st.set_lens(l);
                }
            }
            (true, None) => {
                let target = st.view_dir.depth(model_center(&st.doc));
                st.camera = Some(Camera::new(lens.unwrap_or(Camera::LENS), target));
            }
            (false, _) => st.camera = None,
        }
        Ok(())
    }
}

impl crate::DocState {
    /// LENSLENGTH (DVIEW Zoom) in a perspective view: the camera stays where it is and sees more
    /// with a shorter lens, less with a longer one, so the view's height follows. Nothing in a
    /// parallel view.
    pub fn set_lens(&mut self, lens: f64) {
        let Some(old) = self.camera else { return };
        let cam = Camera::new(lens, old.target);
        let v = self.view();
        let height = v.height * old.lens / cam.lens;
        self.camera = Some(cam);
        self.set_view_quiet(crate::View { center: v.center, height });
    }
}

/// Turn the view by `azimuth` (about Z) and `elevation` degrees about the point at the centre of
/// the screen as deep as the middle of the model, which stays at the centre. No turn changes
/// nothing.
pub fn orbit(s: &mut Session, azimuth: f64, elevation: f64) -> Result<()> {
    model_only(s)?;
    if !(azimuth.is_finite() && elevation.is_finite()) || (azimuth == 0.0 && elevation == 0.0) {
        return Ok(());
    }
    let st = s.state_mut()?;
    let old = st.view_dir;
    let view = st.view();
    // On the centre's line of sight (the view axes are orthonormal, so `old` projects it back to
    // the centre whatever the direction, edge-on views included).
    let (x, y, d) = old.axes();
    let center = match st.orbit_center {
        Some((rev, c)) if rev == st.revision => c,
        _ => {
            let c = model_center(&st.doc);
            st.orbit_center = Some((st.revision, c));
            c
        }
    };
    // A perspective view turns about its target.
    let depth = st.camera.map_or_else(|| old.depth(center), |c| c.target);
    let target = x * view.center.x + y * view.center.y + d * if depth.is_finite() { depth } else { 0.0 };
    let (az, el) = old.angles();
    // Short of straight up or down, so "up" (world Z) stays defined while orbiting.
    let limit = 89.9f64.to_radians();
    let el = (el + elevation.to_radians()).clamp(-limit, limit);
    let new = ViewDir::from_angles(az + azimuth.to_radians(), el);
    st.view_dir = new;
    st.orbit_target = Some(target);
    if let Some(c) = st.camera {
        st.camera = Some(Camera { target: new.depth(target), ..c });
    }
    st.set_view_quiet(crate::View { center: new.project(target), height: view.height });
    Ok(())
}

fn run_orbit(s: &mut Session, p: &Value) -> Result<Value> {
    orbit(s, f64_or(p, "azimuth", 0.0), f64_or(p, "elevation", 0.0))?;
    Ok(dir_json(s.state()?.view_dir))
}

fn run_vscurrent(s: &mut Session, p: &Value) -> Result<Value> {
    let style = str_param(p, "style").unwrap_or("shaded").trim().to_ascii_lowercase();
    // (shaded, with edges)
    let (shaded, edges) = match style.as_str() {
        "2dwireframe" | "2" | "wireframe" | "w" => (false, false),
        "shaded" | "s" | "realistic" | "r" | "shades of gray" | "g" => (true, false),
        "shadedwithedges" | "shaded with edges" | "e" | "conceptual" | "c" => (true, true),
        "hidden" | "h" => (false, true),
        other => return Err(bad("vscurrent", format!("unknown visual style `{other}`"))),
    };
    let st = s.state_mut()?;
    st.shaded = shaded;
    st.shaded_edges = edges;
    st.hide = false;
    Ok(
        json!({ "style": match (shaded, edges) { (false, false) => "2dWireframe", (false, true) => "Hidden", (true, false) => "Shaded", (true, true) => "ShadedWithEdges" } }),
    )
}

/// VPOINT at the command line: a direction as `x,y,z`, or a preset view.
struct VpointM;

impl Interactive for VpointM {
    fn name(&self) -> &'static str {
        "VPOINT"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let d = s.state().map(|st| st.view_dir.dir).unwrap_or(Vec3::Z);
        let kws: Vec<&str> = PRESETS.iter().map(|p| p.1).collect();
        Prompt::new("Specify a view point (x,y,z)", Accept::TEXT).kw(&kws).default(format!("{:.4},{:.4},{:.4}", d.x, d.y, d.z))
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Keyword(k) => {
                let name = PRESETS.iter().find(|p| p.1.eq_ignore_ascii_case(&k)).map(|p| p.0).unwrap_or("top");
                preset(s, name)?;
                Ok(Step::Done)
            }
            Input::Text(t) => {
                let v: Vec<f64> = t.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                let [x, y, z] = v.as_slice() else { return Err(EngineError::Other("Specify the view point as x,y,z.".into())) };
                run_vpoint(s, &json!({ "direction": [x, y, z] }))?;
                Ok(Step::Done)
            }
            Input::Enter | Input::Cancel => Ok(Step::Done),
            _ => Ok(Step::Continue),
        }
    }
}

/// VSCURRENT at the command line.
struct VscurrentM;

impl Interactive for VscurrentM {
    fn name(&self) -> &'static str {
        "VSCURRENT"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter an option", Accept::TEXT).kw(&[
            "2dwireframe",
            "Wireframe",
            "Hidden",
            "Shaded",
            "shaded with Edges",
            "Realistic",
            "Conceptual",
        ])
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Keyword(k) | Input::Text(k) => {
                run_vscurrent(s, &json!({ "style": k }))?;
                Ok(Step::Done)
            }
            Input::Enter | Input::Cancel => Ok(Step::Done),
            _ => Ok(Step::Continue),
        }
    }
}
