//! The 3D view (CAD-rhino): VPOINT, PLAN, the preset views, 3DORBIT and VSCURRENT.
//!
//! The view direction belongs to the Model tab ([`crate::DocState::view_dir`]; layouts and their
//! viewports are drawn in plan); view centres are in its view plane, so a new direction zooms to
//! extents, and an orbit keeps the point at the centre of the screen where it is.

use cadcraft_doc::{Drawing, EntityKind, Space};
use cadcraft_geom::{Vec3, ViewDir};
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

fn model_only(s: &Session) -> Result<()> {
    if s.layout_space() == Space::Model { Ok(()) } else { Err(EngineError::Other("3D views are available on the Model tab only.".into())) }
}

fn dir_json(v: ViewDir) -> Value {
    json!({ "direction": [v.dir.x, v.dir.y, v.dir.z] })
}

/// Look along `v` and zoom to extents (ZOOM Previous returns to the view and direction before).
pub(crate) fn set(s: &mut Session, v: ViewDir) -> Result<Value> {
    model_only(s)?;
    let st = s.state_mut()?;
    let (before, dir) = (st.view(), st.view_dir);
    st.view_dir = v;
    st.orbit_target = None;
    let f = s.extents_frame()?;
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

/// The middle of the model in 3D: of its plan extents, halfway up its solids (2D objects lie at
/// z = 0).
fn model_center(d: &Drawing) -> Vec3 {
    let ext = d.extents(&Space::Model);
    if ext.is_empty() {
        return Vec3::ZERO;
    }
    let (lo, hi) = d
        .model
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Solid3d(x) => Some(x.corners()),
            _ => None,
        })
        .flatten()
        .fold((0.0f64, 0.0f64), |(lo, hi), p| (lo.min(p.z), hi.max(p.z)));
    ext.center().to3((lo + hi) / 2.0)
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
    let depth = old.depth(center);
    let target = x * view.center.x + y * view.center.y + d * if depth.is_finite() { depth } else { 0.0 };
    let (az, el) = old.angles();
    // Short of straight up or down, so "up" (world Z) stays defined while orbiting.
    let limit = 89.9f64.to_radians();
    let el = (el + elevation.to_radians()).clamp(-limit, limit);
    let new = ViewDir::from_angles(az + azimuth.to_radians(), el);
    st.view_dir = new;
    st.orbit_target = Some(target);
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
