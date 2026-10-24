//! DVIEW: AutoCAD's camera command. A view has a target (the point at the centre of the screen,
//! on the target plane) and a camera on its line of sight; DVIEW moves either, sets their
//! distance (which turns perspective on), the lens (perspective) or zoom (parallel), pans both
//! together, twists the view about the line of sight (TWist), sets front and back clipping
//! planes (CLip) and turns perspective Off. Each step changes the view at once; Undo steps back.
//!
//! The camera is [`crate::DocState::camera`]: in perspective the target is at its depth, else
//! (a parallel view) as deep as the middle of the model, where orbits pivot, and the camera
//! as far off as a 50 mm lens would put it. The twist and the clipping planes are the view
//! direction's ([`ViewDir::twist`], [`ViewDir::clip`]): the canvas, picks, snaps and RENDER
//! follow them, and the tile keeps them (DXF VPORT 51, 43, 44 and VIEWMODE). Moving the camera
//! keeps the twist and the planes' distances from the target; VPOINT, PLAN, the preset views
//! and orbiting give an upright view without planes.
//! ponytail: no slider drags (typed values and points only), and the objects asked for first are
//! not the only ones shown while it runs (the whole drawing is).

use cadcraft_geom::{Camera, Clip, Vec2, Vec3, ViewDir};
use serde_json::{Value, json};

use super::machines::{SelOutcome, SelectPhase};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step, TileLook, View};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("dview", "Dview", run_dview)
            .alias(&["dv"])
            .params(
                "{target?: [x,y,z], camera?: [x,y,z] (world points: POints), elevation?, azimuth? (degrees: CAmera, the camera's angle from the XY plane and in it from X about the target), distance? (camera to target: turns perspective on), lens? (mm, perspective), zoom? (factor, parallel), pan?: [dx,dy,dz], twist? (degrees, counter-clockwise about the line of sight), front?: distance from the target toward the camera | \"eye\" | \"off\", back?: distance from the target toward the camera (negative: beyond it) | \"off\", clip?: \"off\" (both planes off), hide?: bool, off?: bool (perspective off)} → {perspective, target, camera, distance, lens, direction, twist, front, back}",
            )
            .noundo()
            .interactive(|_| Ok(Box::new(DviewM::default()))),
    ]
}

/// A view's camera as DVIEW sees it: target and camera points (world), the lens, the twist
/// (radians) and the clipping planes' distances from the target toward the camera.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rig {
    target: Vec3,
    camera: Vec3,
    lens: f64,
    persp: bool,
    twist: f64,
    front: Option<f64>,
    back: Option<f64>,
}

impl Rig {
    fn distance(&self) -> f64 {
        (self.camera - self.target).len()
    }
}

/// The current view's rig.
fn rig(s: &Session) -> Result<Rig> {
    super::view3d::model_only(s)?;
    let st = s.state()?;
    let (dir, view) = (st.view_dir, st.view());
    let cam = st.camera.unwrap_or_else(|| Camera::new(Camera::LENS, dir.depth(super::view3d::model_center(&st.doc))));
    let target = dir.world_of(view.center.to3(cam.target));
    let camera = target + dir.dir * cam.focal(view.height);
    let from_target = |d: Option<f64>| d.map(|d| d - cam.target);
    let (front, back) = (from_target(dir.clip.front), from_target(dir.clip.back));
    Ok(Rig { target, camera, lens: cam.lens, persp: st.camera.is_some(), twist: dir.twist, front, back })
}

/// Show the view rig `r` makes: looking from its camera at its target, through its lens in
/// perspective, else a parallel view of the same height.
fn apply(s: &mut Session, r: Rig) -> Result<()> {
    let off = r.camera - r.target;
    if !(off.len() > 1e-9 && off.is_finite() && r.target.is_finite()) {
        return Err(EngineError::Other("The camera and the target must be apart.".into()));
    }
    let dir = ViewDir::new(off);
    let at = dir.depth(r.target);
    let twist = if r.twist.is_finite() { r.twist.rem_euclid(std::f64::consts::TAU) } else { 0.0 };
    let dir = ViewDir { twist, clip: Clip::new(r.front.map(|f| at + f), r.back.map(|b| at + b)), ..dir };
    let st = s.state_mut()?;
    let cam = Camera::new(r.lens, at);
    let height = if r.persp { cam.height_at(off.len()) } else { st.view().height };
    st.view_dir = dir;
    st.orbit_target = None;
    st.camera = r.persp.then_some(cam);
    st.set_view_quiet(View { center: dir.project(r.target), height });
    Ok(())
}

/// The direction from the XY plane angle `elevation` and the angle in it from X `azimuth`
/// (degrees).
fn angled(elevation: f64, azimuth: f64) -> Vec3 {
    ViewDir::from_angles(azimuth.to_radians(), elevation.clamp(-90.0, 90.0).to_radians()).dir
}

/// The angles (degrees) of direction `d`: from the XY plane, and in it from X.
fn angles_of(d: Vec3) -> (f64, f64) {
    let (az, el) = ViewDir::new(d).angles();
    (el.to_degrees(), az.to_degrees())
}

fn point3(p: &Value, key: &str) -> Result<Option<Vec3>> {
    let Some(v) = p.get(key).filter(|v| !v.is_null()) else { return Ok(None) };
    let n: Option<Vec<f64>> = v.as_array().filter(|a| (2..=3).contains(&a.len())).and_then(|a| a.iter().map(Value::as_f64).collect());
    match n.as_deref() {
        Some(&[x, y]) if x.is_finite() && y.is_finite() => Ok(Some(Vec3::new(x, y, 0.0))),
        Some(&[x, y, z]) if Vec3::new(x, y, z).is_finite() => Ok(Some(Vec3::new(x, y, z))),
        _ => Err(bad("dview", format!("`{key}` must be [x, y, z] (finite numbers)"))),
    }
}

fn opt_f64(p: &Value, key: &str) -> Result<Option<f64>> {
    match p.get(key).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v.as_f64().filter(|f| f.is_finite()).map(Some).ok_or_else(|| bad("dview", format!("`{key}` must be a number"))),
    }
}

/// The view's rig as JSON.
fn report(s: &Session) -> Result<Value> {
    let r = rig(s)?;
    let d = (r.camera - r.target).normalized();
    Ok(json!({
        "perspective": r.persp,
        "target": [r.target.x, r.target.y, r.target.z],
        "camera": [r.camera.x, r.camera.y, r.camera.z],
        "distance": r.distance(),
        "lens": r.lens,
        "direction": [d.x, d.y, d.z],
        "twist": r.twist.to_degrees(),
        "front": r.front,
        "back": r.back,
    }))
}

/// A clipping plane parameter: a distance from the target, "off", or for the front plane "eye"
/// (at the camera, `eye` from the target). `None` when not given.
fn plane_param(p: &Value, key: &str, eye: Option<f64>) -> Result<Option<Option<f64>>> {
    match p.get(key).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(Value::String(t)) if t.eq_ignore_ascii_case("off") => Ok(Some(None)),
        Some(Value::String(t)) if t.eq_ignore_ascii_case("eye") && eye.is_some() => Ok(Some(eye)),
        Some(v) => match v.as_f64().filter(|f| f.is_finite()) {
            Some(f) => Ok(Some(Some(f))),
            None => Err(bad("dview", format!("`{key}` must be a distance from the target, or \"off\""))),
        },
    }
}

fn run_dview(s: &mut Session, p: &Value) -> Result<Value> {
    if bool_or(p, "off", false) {
        s.set_perspective(false, None)?;
    }
    let mut r = rig(s)?;
    if let Some(t) = point3(p, "target")? {
        r.target = t;
    }
    if let Some(c) = point3(p, "camera")? {
        r.camera = c;
    }
    let (el, az) = (opt_f64(p, "elevation")?, opt_f64(p, "azimuth")?);
    if el.is_some() || az.is_some() {
        let (e0, a0) = angles_of(r.camera - r.target);
        r.camera = r.target + angled(el.unwrap_or(e0), az.unwrap_or(a0)) * r.distance();
    }
    if let Some(d) = opt_f64(p, "distance")? {
        if d <= 1e-9 {
            return Err(bad("dview", "`distance` must be positive"));
        }
        r.camera = r.target + (r.camera - r.target).normalized() * d;
        r.persp = true;
    }
    if let Some(l) = opt_f64(p, "lens")? {
        if l <= 0.0 {
            return Err(bad("dview", "`lens` must be positive"));
        }
        r.lens = l;
    }
    if let Some(d) = point3(p, "pan")? {
        r.target = r.target + d;
        r.camera = r.camera + d;
    }
    if let Some(t) = opt_f64(p, "twist")? {
        r.twist = t.to_radians().rem_euclid(std::f64::consts::TAU);
    }
    if let Some(f) = plane_param(p, "front", Some(r.distance()))? {
        r.front = f;
    }
    if let Some(b) = plane_param(p, "back", None)? {
        r.back = b;
    }
    match p.get("clip").filter(|v| !v.is_null()) {
        None => {}
        Some(Value::String(t)) if t.eq_ignore_ascii_case("off") => (r.front, r.back) = (None, None),
        Some(_) => return Err(bad("dview", "`clip` must be \"off\" (`front` and `back` set the planes)")),
    }
    let before = rig(s)?;
    if r != before {
        // A new lens in perspective keeps the camera and sees more or less (DVIEW Zoom).
        let lens_only = Rig { lens: before.lens, ..r } == before && r.persp;
        if lens_only {
            s.state_mut()?.set_lens(r.lens);
        } else {
            apply(s, r)?;
        }
    }
    if let Some(f) = opt_f64(p, "zoom")? {
        if f <= 0.0 {
            return Err(bad("dview", "`zoom` must be positive"));
        }
        let st = s.state_mut()?;
        let v = st.view();
        st.set_view_quiet(View { center: v.center, height: (v.height / f).clamp(1e-9, 1e12) });
    }
    if bool_or(p, "hide", false) {
        s.state_mut()?.hide = true;
    }
    report(s)
}

/// Where DVIEW at the command line is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Stage {
    /// "Select objects or <use DVIEWBLOCK>".
    #[default]
    Select,
    Main,
    /// CAmera (`true`) or TArget: the angle from the XY plane, or (toggled) in it.
    Angle {
        camera: bool,
        toggled: bool,
    },
    /// CAmera or TArget: the other angle, the first given.
    Second {
        camera: bool,
        toggled: bool,
        first: f64,
    },
    Distance,
    /// POints: the target, then the camera.
    Target,
    Camera,
    PanBase,
    PanTo(Vec3),
    Zoom,
    Twist,
    Clip,
    /// CLip Front (`true`) or Back: the plane's distance from the target.
    Plane(bool),
}

/// DVIEW at the command line.
#[derive(Default)]
struct DviewM {
    stage: Stage,
    sel: SelectPhase,
    /// The looks and views before each option, for Undo.
    undo: Vec<(TileLook, View)>,
    /// POints: the new target.
    target: Option<Vec3>,
}

const MAIN_KWS: &[&str] = &["CAmera", "TArget", "Distance", "POints", "PAn", "Zoom", "TWist", "CLip", "Hide", "Off", "Undo"];

fn f3(p: Vec3) -> String {
    format!("{:.4},{:.4},{:.4}", p.x, p.y, p.z)
}

impl DviewM {
    fn save(&mut self, s: &Session) -> Result<()> {
        let st = s.state()?;
        self.undo.push((st.look(), st.view()));
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        Ok(())
    }
    /// The world point a point input gives: CPlane coordinates with its height.
    fn world(s: &Session, p: Vec2) -> Vec3 {
        s.cplane().apply(p.to3(s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0)))
    }
    /// A number typed at a prompt.
    fn number(t: &str) -> Option<f64> {
        crate::units::parse_distance(t.trim()).filter(|v| v.is_finite())
    }
    /// The CAmera or TArget angles given: move the camera about the target, or turn the target
    /// about the camera.
    fn turn(&mut self, s: &mut Session, camera: bool, elevation: f64, azimuth: f64) -> Result<()> {
        self.save(s)?;
        let mut r = rig(s)?;
        let d = angled(elevation, azimuth) * r.distance();
        if camera {
            r.camera = r.target + d;
        } else {
            r.target = r.camera + d;
        }
        apply(s, r)
    }
    /// A point given for CAmera (the camera there) or TArget (the target there).
    fn place(&mut self, s: &mut Session, camera: bool, at: Vec3) -> Result<()> {
        self.save(s)?;
        let mut r = rig(s)?;
        if camera {
            r.camera = at;
        } else {
            r.target = at;
        }
        apply(s, r)
    }
    fn main(&mut self) -> Step {
        self.stage = Stage::Main;
        Step::Continue
    }
}

/// CAmera's (`camera`) or TArget's angle prompt: the angle from the XY plane (`from_xy`) or in
/// it, with the view's `(elevation, azimuth)` as the default and Toggle to the other angle.
fn angle_prompt(camera: bool, from_xy: bool, (elevation, azimuth): (f64, f64)) -> Prompt {
    let msg = match (camera, from_xy) {
        (true, true) => "Specify camera location, or enter angle from the XY plane, or",
        (true, false) => "Specify camera location, or enter angle in XY plane from X axis, or",
        (false, true) => "Specify target location, or enter angle from the XY plane, or",
        (false, false) => "Specify target location, or enter angle in XY plane from X axis, or",
    };
    let (kw, a) = if from_xy { ("Toggle (angle in)", elevation) } else { ("Toggle (angle from)", azimuth) };
    Prompt::new(msg, Accept::POINT_OR_NUMBER).kw(&[kw]).default(format!("{a:.4}"))
}

impl Interactive for DviewM {
    fn name(&self) -> &'static str {
        "DVIEW"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        rig(s)?;
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let r = rig(s).unwrap_or(Rig { target: Vec3::ZERO, camera: Vec3::Z, lens: Camera::LENS, persp: false, twist: 0.0, front: None, back: None });
        let (el, az) = angles_of(r.camera - r.target);
        // From the camera, the target's angles.
        let (tel, taz) = angles_of(r.target - r.camera);
        match self.stage {
            Stage::Select => Prompt::new("Select objects or <use DVIEWBLOCK>", Accept::SELECT),
            Stage::Main => Prompt::new("Enter option", Accept::TEXT).kw(MAIN_KWS),
            // The first angle (`toggled`: the one in the XY plane first), then the other.
            Stage::Angle { camera, toggled } => angle_prompt(camera, !toggled, if camera { (el, az) } else { (tel, taz) }),
            Stage::Second { camera, toggled, .. } => angle_prompt(camera, toggled, if camera { (el, az) } else { (tel, taz) }),
            Stage::Distance => Prompt::new("Specify new camera-target distance", Accept::NUMBER).default(format!("{:.4}", r.distance())),
            Stage::Target => Prompt::new("Specify target point", Accept::POINT).default(f3(r.target)),
            Stage::Camera => Prompt::new("Specify camera point", Accept::POINT).default(f3(r.camera)),
            Stage::PanBase => Prompt::new("Specify displacement base point", Accept::POINT),
            Stage::PanTo(_) => Prompt::new("Specify second point", Accept::POINT),
            Stage::Zoom if r.persp => Prompt::new("Specify lens length", Accept::NUMBER).default(format!("{:.3}mm", r.lens)),
            Stage::Zoom => Prompt::new("Specify zoom scale factor", Accept::NUMBER).default("1"),
            Stage::Twist => Prompt::new("Specify view twist angle", Accept::NUMBER).default(format!("{:.2}", r.twist.to_degrees())),
            Stage::Clip => Prompt::new("Enter clipping option", Accept::TEXT).kw(&["Back", "Front", "Off"]).default("Off"),
            // The current distance, or where ON puts the plane: the front at the camera, the
            // back at the target.
            Stage::Plane(front) => {
                let at = if front { r.front.unwrap_or(r.distance()) } else { r.back.unwrap_or(0.0) };
                let kws: &[&str] = if front { &["Eye", "ON", "OFF"] } else { &["ON", "OFF"] };
                Prompt::new("Specify distance from target", Accept::NUMBER).kw(kws).default(format!("{at:.4}"))
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Done);
        }
        let key = match &i {
            Input::Keyword(k) => Some(k.to_ascii_lowercase()),
            _ => None,
        };
        match self.stage {
            Stage::Select => match self.sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                // The objects only stand for the drawing while it runs; the drawing itself shows.
                SelOutcome::Empty | SelOutcome::Done(_) => Ok(self.main()),
            },
            Stage::Main => match (key.as_deref(), &i) {
                (_, Input::Enter) => Ok(Step::Done),
                (Some("camera"), _) => {
                    self.stage = Stage::Angle { camera: true, toggled: false };
                    Ok(Step::Continue)
                }
                (Some("target"), _) => {
                    self.stage = Stage::Angle { camera: false, toggled: false };
                    Ok(Step::Continue)
                }
                (Some("distance"), _) => {
                    self.stage = Stage::Distance;
                    Ok(Step::Continue)
                }
                (Some("points"), _) => {
                    self.stage = Stage::Target;
                    Ok(Step::Continue)
                }
                (Some("pan"), _) => {
                    self.stage = Stage::PanBase;
                    Ok(Step::Continue)
                }
                (Some("zoom"), _) => {
                    self.stage = Stage::Zoom;
                    Ok(Step::Continue)
                }
                (Some("twist"), _) => {
                    self.stage = Stage::Twist;
                    Ok(Step::Continue)
                }
                (Some("clip"), _) => {
                    self.stage = Stage::Clip;
                    Ok(Step::Continue)
                }
                (Some("hide"), _) => {
                    self.save(s)?;
                    s.state_mut()?.hide = true;
                    Ok(Step::Continue)
                }
                (Some("off"), _) => {
                    self.save(s)?;
                    s.set_perspective(false, None)?;
                    Ok(Step::Continue)
                }
                (Some("undo"), _) => {
                    match self.undo.pop() {
                        Some((look, view)) => {
                            let st = s.state_mut()?;
                            st.set_look(look);
                            st.hide = false;
                            st.set_view_quiet(view);
                        }
                        None => s.echo("Nothing to undo."),
                    }
                    Ok(Step::Continue)
                }
                _ => Ok(Step::Continue),
            },
            Stage::Angle { camera, toggled } => match (key.as_deref(), i) {
                (Some(k), _) if k.starts_with("toggle") => {
                    self.stage = Stage::Angle { camera, toggled: !toggled };
                    Ok(Step::Continue)
                }
                (_, Input::Point(p)) => {
                    self.place(s, camera, Self::world(s, p))?;
                    Ok(self.main())
                }
                (_, Input::Text(t)) => match Self::number(&t) {
                    Some(a) => {
                        self.stage = Stage::Second { camera, toggled, first: a };
                        Ok(Step::Continue)
                    }
                    None => {
                        s.echo("Specify a point or an angle in degrees.");
                        Ok(Step::Continue)
                    }
                },
                // Enter keeps the angle the view has.
                (_, Input::Enter) => {
                    let r = rig(s)?;
                    let (el, az) = if camera { angles_of(r.camera - r.target) } else { angles_of(r.target - r.camera) };
                    self.stage = Stage::Second { camera, toggled, first: if toggled { az } else { el } };
                    Ok(Step::Continue)
                }
                _ => Ok(Step::Continue),
            },
            Stage::Second { camera, toggled, first } => {
                let r = rig(s)?;
                let (el, az) = if camera { angles_of(r.camera - r.target) } else { angles_of(r.target - r.camera) };
                let second = match (key.as_deref(), i) {
                    (Some(k), _) if k.starts_with("toggle") => {
                        // Back to the first angle's prompt, the other way round.
                        self.stage = Stage::Angle { camera, toggled: !toggled };
                        return Ok(Step::Continue);
                    }
                    (_, Input::Point(p)) => {
                        self.place(s, camera, Self::world(s, p))?;
                        return Ok(self.main());
                    }
                    (_, Input::Text(t)) => match Self::number(&t) {
                        Some(a) => a,
                        None => {
                            s.echo("Specify a point or an angle in degrees.");
                            return Ok(Step::Continue);
                        }
                    },
                    (_, Input::Enter) => {
                        if toggled {
                            el
                        } else {
                            az
                        }
                    }
                    _ => return Ok(Step::Continue),
                };
                let (elevation, azimuth) = if toggled { (second, first) } else { (first, second) };
                self.turn(s, camera, elevation, azimuth)?;
                Ok(self.main())
            }
            Stage::Distance => match i {
                Input::Text(t) => match Self::number(&t).filter(|d| *d > 1e-9) {
                    Some(d) => {
                        self.save(s)?;
                        let mut r = rig(s)?;
                        r.camera = r.target + (r.camera - r.target).normalized() * d;
                        r.persp = true;
                        apply(s, r)?;
                        Ok(self.main())
                    }
                    None => {
                        s.echo("Requires a positive distance.");
                        Ok(Step::Continue)
                    }
                },
                // Enter takes the distance the camera is at.
                Input::Enter => {
                    self.save(s)?;
                    let r = rig(s)?;
                    apply(s, Rig { persp: true, ..r })?;
                    Ok(self.main())
                }
                _ => Ok(Step::Continue),
            },
            Stage::Target => match i {
                Input::Point(p) => {
                    self.target = Some(Self::world(s, p));
                    self.stage = Stage::Camera;
                    Ok(Step::Continue)
                }
                Input::Enter => {
                    self.target = None;
                    self.stage = Stage::Camera;
                    Ok(Step::Continue)
                }
                _ => Ok(Step::Continue),
            },
            Stage::Camera => {
                let at = match i {
                    Input::Point(p) => Some(Self::world(s, p)),
                    Input::Enter => None,
                    _ => return Ok(Step::Continue),
                };
                let mut r = rig(s)?;
                if let Some(t) = self.target.take() {
                    r.target = t;
                }
                if let Some(c) = at {
                    r.camera = c;
                }
                self.save(s)?;
                if let Err(e) = apply(s, r) {
                    s.echo(e.to_string());
                }
                Ok(self.main())
            }
            Stage::PanBase => match i {
                Input::Point(p) => {
                    self.stage = Stage::PanTo(Self::world(s, p));
                    Ok(Step::Continue)
                }
                Input::Enter => Ok(self.main()),
                _ => Ok(Step::Continue),
            },
            Stage::PanTo(from) => match i {
                Input::Point(p) => {
                    let d = Self::world(s, p) - from;
                    self.save(s)?;
                    let r = rig(s)?;
                    apply(s, Rig { target: r.target + d, camera: r.camera + d, ..r })?;
                    Ok(self.main())
                }
                Input::Enter => Ok(self.main()),
                _ => Ok(Step::Continue),
            },
            Stage::Zoom => match i {
                Input::Text(t) => match Self::number(t.trim().trim_end_matches("mm")).filter(|v| *v > 0.0) {
                    Some(v) => {
                        self.save(s)?;
                        let st = s.state_mut()?;
                        if st.camera.is_some() {
                            st.set_lens(v);
                        } else {
                            let view = st.view();
                            st.set_view_quiet(View { center: view.center, height: (view.height / v).clamp(1e-9, 1e12) });
                        }
                        Ok(self.main())
                    }
                    None => {
                        s.echo("Requires a positive number.");
                        Ok(Step::Continue)
                    }
                },
                Input::Enter => Ok(self.main()),
                _ => Ok(Step::Continue),
            },
            Stage::Twist => match i {
                Input::Text(t) => match crate::units::parse_angle(t.trim()).filter(|a| a.is_finite()) {
                    Some(a) => {
                        self.save(s)?;
                        let r = rig(s)?;
                        apply(s, Rig { twist: a.rem_euclid(std::f64::consts::TAU), ..r })?;
                        Ok(self.main())
                    }
                    None => {
                        s.echo("Requires an angle.");
                        Ok(Step::Continue)
                    }
                },
                _ => Ok(self.main()),
            },
            Stage::Clip => match (key.as_deref(), i) {
                (Some("front"), _) => {
                    self.stage = Stage::Plane(true);
                    Ok(Step::Continue)
                }
                (Some("back"), _) => {
                    self.stage = Stage::Plane(false);
                    Ok(Step::Continue)
                }
                // Off (the default): both planes off.
                (Some("off"), _) | (_, Input::Enter) => {
                    self.save(s)?;
                    let r = rig(s)?;
                    apply(s, Rig { front: None, back: None, ..r })?;
                    Ok(self.main())
                }
                _ => Ok(Step::Continue),
            },
            Stage::Plane(front) => {
                let r = rig(s)?;
                let current = if front { r.front } else { r.back };
                let at = match (key.as_deref(), i) {
                    (Some("off"), _) => None,
                    (Some("eye"), _) => Some(r.distance()),
                    // ON, or Enter: the plane where it is, else the camera's (front) or the
                    // target's (back).
                    (Some("on"), _) | (_, Input::Enter) => Some(current.unwrap_or(if front { r.distance() } else { 0.0 })),
                    (_, Input::Text(t)) => match Self::number(&t) {
                        Some(d) => Some(d),
                        None => {
                            s.echo("Requires a distance.");
                            return Ok(Step::Continue);
                        }
                    },
                    _ => return Ok(Step::Continue),
                };
                self.save(s)?;
                apply(s, if front { Rig { front: at, ..r } } else { Rig { back: at, ..r } })?;
                Ok(self.main())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_doc::{EntityKind, Line, Space};

    /// A drawing with a line across a cabinet-sized space, seen from the south-west.
    fn session() -> Session {
        let mut s = Session::new();
        s.viewport_px = (800.0, 600.0);
        let line = EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(600.0, 400.0, 700.0) });
        s.doc_mut().unwrap().add(&Space::Model, Default::default(), line).unwrap();
        s.execute("view.swiso", &json!({})).unwrap();
        s
    }

    fn prompt(s: &Session) -> String {
        s.prompt_text()
    }

    #[test]
    fn distance_turns_perspective_on_and_off_turns_it_off() {
        let mut s = session();
        assert_eq!(crate::sysvars::get(&s, "PERSPECTIVE"), Some(json!(0)));
        let r = s.execute("dview", &json!({ "distance": 3000 })).unwrap();
        assert_eq!(r["perspective"], json!(true));
        assert!((r["distance"].as_f64().unwrap() - 3000.0).abs() < 1e-6, "{r}");
        assert_eq!(crate::sysvars::get(&s, "PERSPECTIVE"), Some(json!(1)));
        assert_eq!(crate::sysvars::get(&s, "LENSLENGTH"), Some(json!(50.0)));
        // The eye is where DVIEW put it.
        let st = s.state().unwrap();
        let eye = st.screen_dir().eye().unwrap();
        let t = r["target"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
        assert!(((eye - Vec3::new(t[0], t[1], t[2])).len() - 3000.0).abs() < 1e-6);
        let r = s.execute("dview", &json!({ "off": true })).unwrap();
        assert_eq!(r["perspective"], json!(false));
        assert!(s.state().unwrap().screen_dir().persp.is_none());
    }

    #[test]
    fn points_look_from_the_camera_at_the_target() {
        let mut s = session();
        let r = s.execute("dview", &json!({ "target": [300, 200, 350], "camera": [300, -3000, 1500], "distance": 3000 })).unwrap();
        let d = r["direction"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
        let want = Vec3::new(0.0, -3200.0, 1150.0).normalized();
        assert!((Vec3::new(d[0], d[1], d[2]) - want).len() < 1e-9, "{r}");
        // The target is at the centre of the screen.
        let st = s.state().unwrap();
        assert!(st.screen_dir().project(Vec3::new(300.0, 200.0, 350.0)).dist(st.view().center) < 1e-6);
        // A lens change keeps the camera where it is.
        let eye = st.screen_dir().eye().unwrap();
        s.execute("dview", &json!({ "lens": 24 })).unwrap();
        let st = s.state().unwrap();
        assert!((st.screen_dir().eye().unwrap() - eye).len() < 1e-6);
        assert_eq!(crate::sysvars::get(&s, "LENSLENGTH"), Some(json!(24.0)));
        // A camera on the target is refused.
        assert!(s.execute("dview", &json!({ "camera": [300, 200, 350], "target": [300, 200, 350] })).is_err());
    }

    #[test]
    fn typed_dview_follows_autocads_prompts() {
        let mut s = session();
        s.cmdline("DVIEW").unwrap();
        assert!(prompt(&s).contains("Select objects or <use DVIEWBLOCK>"), "{}", prompt(&s));
        s.cmdline("").unwrap();
        assert!(prompt(&s).contains("[CAmera/TArget/Distance/POints/PAn/Zoom/TWist/CLip/Hide/Off/Undo]"), "{}", prompt(&s));
        s.cmdline("D").unwrap();
        assert!(prompt(&s).contains("Specify new camera-target distance <"), "{}", prompt(&s));
        s.cmdline("2500").unwrap();
        assert!(s.state().unwrap().camera.is_some(), "Distance turns perspective on");
        s.cmdline("Z").unwrap();
        assert!(prompt(&s).contains("Specify lens length <50.000mm>"), "{}", prompt(&s));
        s.cmdline("35").unwrap();
        assert_eq!(s.state().unwrap().camera.unwrap().lens, 35.0);
        // CAmera by angles: 30 above the XY plane, -60 in it.
        s.cmdline("CA").unwrap();
        assert!(prompt(&s).contains("enter angle from the XY plane"), "{}", prompt(&s));
        s.cmdline("30").unwrap();
        assert!(prompt(&s).contains("enter angle in XY plane from X axis"), "{}", prompt(&s));
        s.cmdline("-60").unwrap();
        let (az, el) = s.state().unwrap().view_dir.angles();
        assert!((el.to_degrees() - 30.0).abs() < 1e-9 && (az.to_degrees() + 60.0).abs() < 1e-9);
        // Undo takes the camera back; Off turns perspective off; Enter ends.
        s.cmdline("U").unwrap();
        let (_, el) = s.state().unwrap().view_dir.angles();
        assert!((el.to_degrees() - 30.0).abs() > 1.0);
        s.cmdline("O").unwrap();
        assert!(s.state().unwrap().camera.is_none());
        s.cmdline("TW").unwrap();
        assert!(prompt(&s).contains("Specify view twist angle <0.00>"), "{}", prompt(&s));
        s.cmdline("15").unwrap();
        assert!((s.state().unwrap().view_dir.twist.to_degrees() - 15.0).abs() < 1e-9);
        s.cmdline("").unwrap();
        assert!(s.running.is_none());
    }

    /// The direction of world Z on the screen, in degrees from the screen's X axis.
    fn z_on_screen(s: &Session) -> f64 {
        let st = s.state().unwrap();
        let d = st.screen_dir();
        let z = d.project(Vec3::Z) - d.project(Vec3::ZERO);
        z.y.atan2(z.x).to_degrees()
    }

    #[test]
    fn twist_turns_the_view_about_the_line_of_sight() {
        let mut s = session();
        let before = s.execute("dview", &json!({})).unwrap();
        assert!((z_on_screen(&s) - 90.0).abs() < 1e-9, "upright: Z up the screen");
        let r = s.execute("dview", &json!({ "twist": 30 })).unwrap();
        assert!((r["twist"].as_f64().unwrap() - 30.0).abs() < 1e-9, "{r}");
        assert!((z_on_screen(&s) - 120.0).abs() < 1e-9, "turned 30° counter-clockwise");
        // The target stays at the centre of the screen and the camera where it was.
        let point = |v: &Value| v.as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
        let t = point(&r["target"]);
        assert!(t.iter().zip(point(&before["target"])).all(|(a, b)| (a - b).abs() < 1e-6), "{r} {before}");
        let st = s.state().unwrap();
        assert!(st.screen_dir().project(Vec3::new(t[0], t[1], t[2])).dist(st.view().center) < 1e-6);
        assert!(!st.screen_dir().is_plan());
        // Moving the camera keeps the twist; perspective does too.
        s.execute("dview", &json!({ "elevation": 20, "distance": 4000 })).unwrap();
        assert!((s.state().unwrap().screen_dir().twist.to_degrees() - 30.0).abs() < 1e-9);
        assert!(s.state().unwrap().screen_dir().persp.is_some());
        // A twist of 0 is upright again; a preset view is upright.
        s.execute("dview", &json!({ "twist": -30 })).unwrap();
        assert!((s.state().unwrap().view_dir.twist.to_degrees() - 330.0).abs() < 1e-9);
        s.execute("view.swiso", &json!({})).unwrap();
        assert_eq!(s.state().unwrap().view_dir.twist, 0.0);
        // The tile keeps it through a VPORTS round trip of the look.
        s.execute("dview", &json!({ "twist": 45 })).unwrap();
        s.execute("-vports", &json!({ "option": "2", "arrangement": "vertical" })).unwrap();
        s.execute("vports.activate", &json!({ "index": 2 })).unwrap();
        s.execute("vports.activate", &json!({ "index": 1 })).unwrap();
        assert!(s.model_tiles().iter().any(|t| (t.look.dir.twist.to_degrees() - 45.0).abs() < 1e-9));
    }

    #[test]
    fn clipping_planes_hide_what_is_outside_them() {
        let mut s = session();
        let r = s.execute("dview", &json!({ "front": 100, "back": -50 })).unwrap();
        assert!((r["front"].as_f64().unwrap() - 100.0).abs() < 1e-6 && (r["back"].as_f64().unwrap() + 50.0).abs() < 1e-6, "{r}");
        let t = r["target"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
        let target = Vec3::new(t[0], t[1], t[2]);
        let toward = s.state().unwrap().view_dir.dir;
        let sees = |s: &Session, off: f64| s.state().unwrap().screen_dir().sees(target + toward * off);
        assert!(sees(&s, 0.0) && sees(&s, 99.0) && !sees(&s, 101.0) && sees(&s, -49.0) && !sees(&s, -51.0));
        // The planes keep their distances from the target when the camera moves.
        s.execute("dview", &json!({ "azimuth": 10, "distance": 5000 })).unwrap();
        let r = s.execute("dview", &json!({})).unwrap();
        assert!((r["front"].as_f64().unwrap() - 100.0).abs() < 1e-6 && (r["back"].as_f64().unwrap() + 50.0).abs() < 1e-6, "{r}");
        // Front at the eye, back off; then clip off.
        let r = s.execute("dview", &json!({ "front": "eye", "back": "off" })).unwrap();
        assert!((r["front"].as_f64().unwrap() - r["distance"].as_f64().unwrap()).abs() < 1e-6 && r["back"].is_null(), "{r}");
        let r = s.execute("dview", &json!({ "clip": "off" })).unwrap();
        assert!(r["front"].is_null() && r["back"].is_null());
        assert!(s.state().unwrap().view_dir.clip.is_off());
        // Typed: CLip Back 10, Front Eye, Off.
        s.cmdline("DVIEW").unwrap();
        s.cmdline("").unwrap();
        s.cmdline("CL").unwrap();
        assert!(prompt(&s).contains("Enter clipping option [Back/Front/Off] <Off>"), "{}", prompt(&s));
        s.cmdline("B").unwrap();
        assert!(prompt(&s).contains("Specify distance from target or [ON/OFF]"), "{}", prompt(&s));
        s.cmdline("10").unwrap();
        assert!(rig(&s).unwrap().back.is_some_and(|b| (b - 10.0).abs() < 1e-6));
        s.cmdline("CL").unwrap();
        s.cmdline("F").unwrap();
        assert!(prompt(&s).contains("[Eye/ON/OFF]"), "{}", prompt(&s));
        s.cmdline("E").unwrap();
        let r = rig(&s).unwrap();
        assert!(r.front.is_some_and(|f| (f - r.distance()).abs() < 1e-6));
        s.cmdline("CL").unwrap();
        s.cmdline("O").unwrap();
        assert!(s.state().unwrap().view_dir.clip.is_off());
        // Undo brings the planes back.
        s.cmdline("U").unwrap();
        assert!(!s.state().unwrap().view_dir.clip.is_off());
        s.cmdline("").unwrap();
    }

    #[test]
    fn hostile_dview_params_are_refused() {
        let mut s = session();
        for p in [
            json!({ "distance": -1 }),
            json!({ "lens": 0 }),
            json!({ "target": [1, "x"] }),
            json!({ "camera": [f64::MAX, f64::MAX, f64::MAX], "target": [-f64::MAX, -f64::MAX, -f64::MAX] }),
            json!({ "zoom": 0 }),
            json!({ "twist": "x" }),
            json!({ "front": "bogus" }),
            json!({ "back": "eye" }),
            json!({ "clip": 3 }),
            json!({ "twist": 1e308, "front": -1e308, "back": 1e308 }),
        ] {
            let _ = s.execute("dview", &p);
            let st = s.state().unwrap();
            assert!(st.view().height.is_finite() && st.view().center.is_finite(), "{p}");
        }
        // In a layout DVIEW is refused.
        s.execute("layout.set", &json!({ "name": "Layout1" })).ok();
        if s.layout_space() != Space::Model {
            assert!(s.execute("dview", &json!({ "distance": 10 })).is_err());
        }
    }
}
