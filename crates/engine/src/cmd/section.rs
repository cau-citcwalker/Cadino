//! Sections of 3D solids (CAD-rhino): SECTION draws where a plane cuts solids and surfaces;
//! SECTIONPLANE makes a named section object, LIVESECTION switches its live flag, and
//! SECTIONPLANETOBLOCK (GENERATESECTION) draws its 2D section as a block: the cut outlines, a
//! hatch on the cut faces and the edges seen beyond the plane.
//!
//! A section object is a closed rectangle (an LWPOLYLINE) standing in its plane and marked with
//! [`APP`] xdata (its name and live flag), so it saves and opens as any polyline does, other
//! programs see a rectangle, and MOVE, ROTATE and 3DROTATE take the plane along. Its extrusion,
//! the plane's normal, points toward the viewer: a section looks at the material behind it.

use std::sync::Arc as Shared;

use cadcraft_color::Color;
use cadcraft_doc::{Common, Entity, EntityKind, Handle, Hatch, HatchLoop, RawTag, Solid3d};
use cadcraft_geom::{Arc, Bounds2, Mat4, PolyVertex, Spline, Vec2, Vec3, ViewDir};
use cadcraft_kernel::{EdgeCurve, Wire3};
use serde_json::{Value, json};

use super::curves::{KW, handle_value, is_locked, pick_at};
use super::machines::{SelOutcome, SelectPhase};
use super::solids::{kernel, p3_value, placed, solids};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

/// The xdata application marking a section object.
pub(super) const APP: &str = "CADINO_SECTION";
const MARK: &str = "SECTIONPLANE";
/// The orthographic section planes: name, keyword, direction toward the viewer.
const ORTHO: &[(&str, &str, [f64; 3])] = &[
    ("front", "Front", [0.0, -1.0, 0.0]),
    ("back", "bAck", [0.0, 1.0, 0.0]),
    ("top", "Top", [0.0, 0.0, 1.0]),
    ("bottom", "Bottom", [0.0, 0.0, -1.0]),
    ("left", "Left", [-1.0, 0.0, 0.0]),
    ("right", "Right", [1.0, 0.0, 0.0]),
];
const PLANE_PARAMS: &str = "origin: [x,y,z], normal: [x,y,z] | points: [p1,p2,p3] | plane: \"xy\"|\"yz\"|\"zx\"|\"view\", point?: [x,y,z] | object: handle (a circle, arc, ellipse, 2D spline or 2D polyline)";

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("section", "Section", run_section)
            .menu(&["Draw", "Modeling", "Sections", "Section"])
            .alias(&["sec"])
            .params(
                "{handles?, origin: [x,y,z], normal: [x,y,z] | points: [p1,p2,p3] | plane: \"xy\"|\"yz\"|\"zx\"|\"view\", point?: [x,y,z] | object: handle} \
                 (world): the cut outlines as polylines in the plane, on the current layer → {handles}",
            )
            .interactive(|_| Ok(Box::new(SectionM::default()))),
        CommandSpec::new("sectionplane", "Section Plane", run_sectionplane)
            .menu(&["Draw", "Modeling", "Section Plane"])
            .alias(&["splane"])
            .params(
                "{origin: [x,y,z], normal: [x,y,z] (toward the viewer) | points | plane, point? | object | orthographic: \"front\"|\"back\"|\"top\"|\"bottom\"|\"left\"|\"right\" \
                 (through the middle of the solids), name?, live?: bool (default false)} → {handle, name, live}",
            )
            .interactive(|_| Ok(Box::new(SectionPlaneM::default()))),
        CommandSpec::new("livesection", "Live Section", run_livesection)
            .menu(&["Draw", "Modeling", "Sections", "Live Section"])
            .params("{handle? (a section plane; default the selected one, else the only one), on?: bool (default: switch)} → {handle, name, live}")
            .interactive(|_| Ok(Box::new(PickSectionM { id: "livesection", name: "LIVESECTION", chosen: None }))),
        CommandSpec::new("sectionplanetoblock", "Generate Section", run_toblock)
            .menu(&["Draw", "Modeling", "Sections", "Generate Section"])
            .alias(&["generatesection"])
            .params(
                "{section? (a section plane's handle; default the selected one, else the only one), handles?: solids (default every 3D solid on a visible layer), \
                 block?: name (an existing block is redefined), at?: [x,y,z?] (insertion point; default beside the model; with an existing block, inserted only when given), \
                 hatch?: pattern | \"none\" (default ANSI31), hatchScale?, background?: bool (default true: the edges seen beyond the plane)} → {block, handle?, regions, background}",
            )
            .interactive(|_| Ok(Box::new(PickSectionM { id: "sectionplanetoblock", name: "SECTIONPLANETOBLOCK", chosen: None }))),
    ]
}

// ---------------- planes ----------------

fn unit(cmd: &str, v: Vec3, what: &str) -> Result<Vec3> {
    let u = v.normalized();
    if u.is_finite() && u.len() > 0.5 { Ok(u) } else { Err(bad(cmd, what.to_string())) }
}

/// The cutting plane a JSON form gives, in the world, as a point and a unit normal: `origin` and
/// `normal`; `points` (three, not on one line); `plane` ("xy", "yz", "zx", or "view": square to
/// the current view) through `point` (default the origin); or `object`, the plane of a circle,
/// arc, ellipse, 2D spline or 2D polyline.
pub(crate) fn plane_param(s: &Session, cmd: &str, p: &Value) -> Result<(Vec3, Vec3)> {
    let given = |k: &str| p.get(k).filter(|v| !v.is_null());
    let point = |k: &str| -> Result<Vec3> {
        match given(k) {
            Some(v) => p3_value(v).ok_or_else(|| bad(cmd, format!("`{k}` must be [x, y] or [x, y, z]"))),
            None => Ok(Vec3::ZERO),
        }
    };
    if let Some(v) = given("object") {
        let h = handle_value(v).ok_or_else(|| bad(cmd, "`object` must be an object handle"))?;
        let e = s.doc()?.entity(h).ok_or_else(|| bad(cmd, "the `object` does not exist"))?;
        let plane = match &e.kind {
            EntityKind::Circle(_) | EntityKind::Arc(_) | EntityKind::Ellipse(_) | EntityKind::Spline(_) | EntityKind::LwPolyline(_) => e.plane(),
            _ => None,
        };
        let m = plane.ok_or_else(|| bad(cmd, "the `object` must be a circle, arc, ellipse, 2D spline or 2D polyline"))?;
        return Ok((m.apply(Vec3::ZERO), unit(cmd, m.apply_vec(Vec3::Z), "the object's plane has no direction")?));
    }
    if let Some(v) = given("points") {
        let pts: Option<Vec<Vec3>> = v.as_array().filter(|a| a.len() == 3).and_then(|a| a.iter().map(p3_value).collect());
        let Some([a, b, c]) = pts.as_deref() else { return Err(bad(cmd, "`points` must be three points [[x,y,z],[x,y,z],[x,y,z]]")) };
        let n = (*b - *a).cross(*c - *a);
        let reach = (*b - *a).len().max((*c - *a).len());
        if !n.is_finite() || n.len() <= 1e-12 * reach * reach {
            return Err(bad(cmd, "the three points are on one line"));
        }
        return Ok((*a, unit(cmd, n, "the three points are on one line")?));
    }
    if let Some(v) = given("plane") {
        let n = match v.as_str().map(str::to_ascii_lowercase).as_deref() {
            Some("xy") => Vec3::Z,
            Some("yz") => Vec3::X,
            Some("zx" | "xz") => Vec3::Y,
            Some("view") => s.state()?.screen_dir().dir,
            _ => return Err(bad(cmd, format!("`plane` is \"xy\", \"yz\", \"zx\" or \"view\", not {v}"))),
        };
        return Ok((point("point")?, n));
    }
    if let Some(v) = given("normal") {
        let n = p3_value(v).ok_or_else(|| bad(cmd, "`normal` must be a direction [x, y, z]"))?;
        return Ok((point("origin")?, unit(cmd, n, "`normal` must not be zero")?));
    }
    Err(bad(cmd, format!("give the plane: {PLANE_PARAMS}")))
}

/// How far the solid reaches behind and in front of the plane through `o` square to `n`.
fn reach(x: &Solid3d, o: Vec3, n: Vec3) -> (f64, f64) {
    x.mesh.points().map(|q| n.dot(x.world(q) - o)).fold((f64::MAX, f64::MIN), |(lo, hi), d| (lo.min(d), hi.max(d)))
}

/// A tolerance for the solid's size: a millionth of its box.
fn tol_of(x: &Solid3d) -> f64 {
    let b = x.bounds();
    1e-6 * (1.0 + b.width() + b.height())
}

/// The box round the solids in the world.
fn box_of(xs: &[Solid3d]) -> Option<(Vec3, Vec3)> {
    let mut it = xs.iter().flat_map(|x| x.mesh.points().map(|q| x.world(q))).filter(|q| q.is_finite());
    let first = it.next()?;
    Some(it.fold((first, first), |(lo, hi), q| {
        (Vec3::new(lo.x.min(q.x), lo.y.min(q.y), lo.z.min(q.z)), Vec3::new(hi.x.max(q.x), hi.y.max(q.y), hi.z.max(q.z)))
    }))
}

// ---------------- curves in a plane ----------------

/// An orthonormal frame: a world point's 2D coordinates along `x` and `y`, and its height along
/// `z`.
#[derive(Clone, Copy)]
pub(super) struct Frame {
    pub x: Vec3,
    pub y: Vec3,
    pub z: Vec3,
}

impl Frame {
    /// The object coordinate system of normal `n` (AutoCAD's arbitrary axis algorithm): the
    /// coordinates a planar object with extrusion `n` keeps.
    pub(super) fn ocs(n: Vec3) -> Frame {
        let m = Mat4::ocs(n);
        Frame { x: m.apply_vec(Vec3::X), y: m.apply_vec(Vec3::Y), z: m.apply_vec(Vec3::Z) }
    }
    /// A view looking against `toward`: right and up as the screen shows them ([`ViewDir::axes`]).
    pub(super) fn view(toward: Vec3) -> Frame {
        let (x, y, z) = ViewDir::new(toward).axes();
        Frame { x, y, z }
    }
    pub(super) fn at(&self, p: Vec3) -> Vec2 {
        Vec2::new(self.x.dot(p), self.y.dot(p))
    }
}

/// One curve of a chain in 2D: its vertices with the bulge to the next point, and its end.
#[derive(Clone)]
struct Run {
    vs: Vec<PolyVertex>,
    end: Vec2,
}

impl Run {
    fn start(&self) -> Vec2 {
        self.vs.first().map_or(self.end, |v| v.p)
    }
    fn reversed(&self) -> Run {
        let mut pts: Vec<Vec2> = self.vs.iter().map(|v| v.p).collect();
        pts.push(self.end);
        let n = self.vs.len();
        let vs = (1..=n).rev().filter_map(|i| Some(PolyVertex::with_bulge(*pts.get(i)?, -self.vs.get(i - 1)?.bulge))).collect();
        Run { vs, end: pts.first().copied().unwrap_or(self.end) }
    }
}

/// Points along an ellipse (or part of one) in 3D.
fn ellipse_points(center: Vec3, major: Vec3, ratio: f64, normal: Vec3, start: f64, end: f64) -> Vec<Vec3> {
    let minor = normal.normalized().cross(major) * ratio;
    let mut sweep = end - start;
    if !sweep.is_finite() {
        return Vec::new();
    }
    if sweep <= 1e-12 {
        sweep += std::f64::consts::TAU;
    }
    let n = ((48.0 * sweep / std::f64::consts::TAU).ceil() as usize).clamp(4, 96);
    (0..=n).map(|i| start + sweep * i as f64 / n as f64).map(|t| center + major * t.cos() + minor * t.sin()).collect()
}

/// A curve in frame `f`'s 2D coordinates: lines and arcs exactly (as bulges), circles as two half
/// circles, ellipses and splines as points along them.
/// ponytail: ellipses and splines (an oblique cut of a cylinder, a lofted part's cut) become
/// polylines through points; ELLIPSE and SPLINE objects of their own if exact outlines matter.
fn run_of(c: &EdgeCurve, f: &Frame) -> Option<Run> {
    let points = |pts: &[Vec3]| {
        let mut it = pts.iter().map(|q| f.at(*q));
        let first = it.next()?;
        let mut vs = vec![PolyVertex::new(first)];
        let mut end = first;
        for q in it {
            vs.push(PolyVertex::new(q));
            end = q;
        }
        vs.pop();
        (!vs.is_empty()).then_some(Run { vs, end })
    };
    match c {
        EdgeCurve::Line { a, b } => Some(Run { vs: vec![PolyVertex::new(f.at(*a))], end: f.at(*b) }),
        EdgeCurve::Arc { a, mid, b } => {
            let (a, m, b) = (f.at(*a), f.at(*mid), f.at(*b));
            let bulge = Arc::from_3_points(a, m, b).map_or(0.0, |arc| {
                let t = (arc.sweep() / 4.0).tan();
                if arc.start_point().dist(a) <= arc.end_point().dist(a) { t } else { -t }
            });
            Some(Run { vs: vec![PolyVertex::with_bulge(a, if bulge.is_finite() { bulge } else { 0.0 })], end: b })
        }
        EdgeCurve::Circle { center, radius, .. } => {
            let (c, r) = (f.at(*center), Vec2::new(*radius, 0.0));
            Some(Run { vs: vec![PolyVertex::with_bulge(c + r, 1.0), PolyVertex::with_bulge(c - r, 1.0)], end: c + r })
        }
        EdgeCurve::Ellipse { center, major, ratio, normal, start, end } => points(&ellipse_points(*center, *major, *ratio, *normal, *start, *end)),
        EdgeCurve::Spline { points: pts, .. } => points(pts),
    }
}

/// A chain's vertices in frame `f` (each curve turned to run on from the one before) and whether
/// it is closed.
fn chain(w: &Wire3, f: &Frame) -> Option<(Vec<PolyVertex>, bool)> {
    let mut runs: Vec<Run> = w.curves.iter().filter_map(|c| run_of(c, f)).collect();
    let size = runs.iter().flat_map(|r| r.vs.iter().map(|v| v.p).chain([r.end])).map(|p| p.x.abs().max(p.y.abs())).fold(0.0, f64::max);
    let tol = 1e-6 * (1.0 + size);
    let near = |a: Vec2, b: Vec2| a.dist(b) <= tol;
    if let [first, second, ..] = runs.as_slice() {
        let touches = |p: Vec2| near(p, second.start()) || near(p, second.end);
        if !touches(first.end) && touches(first.start()) {
            runs[0] = first.reversed();
        }
    }
    for i in 1..runs.len() {
        let (Some(prev), Some(r)) = (runs.get(i - 1), runs.get(i)) else { continue };
        if !near(r.start(), prev.end) && near(r.end, prev.end) {
            runs[i] = r.reversed();
        }
    }
    let mut vs: Vec<PolyVertex> = runs.iter().flat_map(|r| r.vs.iter().copied()).collect();
    let end = runs.last()?.end;
    let closed = w.closed || vs.first().is_some_and(|v| near(v.p, end));
    if !closed {
        vs.push(PolyVertex::new(end));
    }
    (vs.len() >= 2).then_some((vs, closed))
}

/// A chain as a polyline in frame `f`, at height `z`.
fn polyline(w: &Wire3, f: &Frame, z: f64) -> Option<EntityKind> {
    let (vs, closed) = chain(w, f)?;
    let EntityKind::LwPolyline(mut pl) = helpers::lwpoly(vs, closed) else { return None };
    pl.elevation = z;
    Some(EntityKind::LwPolyline(pl))
}

/// A curve of FLATSHOT's view-plane drawing as a 2D object: a line, arc, circle, ellipse or spline.
pub(super) fn flat_kind(c: &EdgeCurve) -> Option<EntityKind> {
    let ok = |k: EntityKind| Some(k);
    match c {
        EdgeCurve::Line { a, b } => (a.xy().dist(b.xy()) > 1e-12).then(|| helpers::line(a.xy(), b.xy())),
        EdgeCurve::Arc { a, mid, b } => match Arc::from_3_points(a.xy(), mid.xy(), b.xy()) {
            Some(arc) if arc.radius.is_finite() => ok(helpers::arc(&arc)),
            _ => (a.xy().dist(b.xy()) > 1e-12).then(|| helpers::line(a.xy(), b.xy())),
        },
        EdgeCurve::Circle { center, radius, .. } => (*radius > 0.0).then(|| helpers::circle(center.xy(), *radius)),
        EdgeCurve::Ellipse { center, major, ratio, normal, start, end } => {
            // Seen from below (normal −Z) the ellipse runs clockwise: the same points from −end to −start.
            let (start, end) = if normal.z < 0.0 { (-*end, -*start) } else { (*start, *end) };
            let el = cadcraft_doc::Ellipse {
                center: Vec3::new(center.x, center.y, 0.0),
                major: Vec3::new(major.x, major.y, 0.0),
                ratio: *ratio,
                start,
                end,
            };
            (*ratio > 0.0 && *ratio <= 1.0 + 1e-9 && major.xy().len() > 1e-12).then_some(EntityKind::Ellipse(el))
        }
        EdgeCurve::Spline { degree, poles, weights, knots, points } => {
            let n = usize::try_from(*degree).unwrap_or(0);
            let exact = n >= 1 && poles.len() > n && knots.len() == poles.len() + n + 1 && (weights.is_empty() || weights.len() == poles.len());
            if exact {
                let control = poles.iter().map(|q| q.xy()).collect();
                return ok(EntityKind::Spline(Spline { degree: n, knots: knots.clone(), control, weights: weights.clone(), ..Spline::default() }));
            }
            let pts: Vec<PolyVertex> = points.iter().map(|q| PolyVertex::new(q.xy())).collect();
            (pts.len() >= 2).then(|| helpers::lwpoly(pts, false))
        }
    }
}

/// Add `kind`, made in the object coordinates of normal `n` ([`Frame::ocs`]), standing in its plane.
fn add_in_plane(s: &mut Session, kind: EntityKind, n: Vec3) -> Result<Handle> {
    let h = s.add_entity_world(kind)?;
    s.doc_mut()?.modify_entity(h, |e| e.common.extrusion = n)?;
    Ok(h)
}

// ---------------- SECTION ----------------

fn run_section(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "section";
    let hs = targets(s, p)?;
    let (o, n) = plane_param(s, CMD, p)?;
    let xs = solids(s, &hs)?;
    if xs.is_empty() {
        return Err(bad(CMD, "select 3D solids or surfaces"));
    }
    let f = Frame::ocs(n);
    let z = f.z.dot(o);
    // The solids the plane reaches, cut in one kernel call.
    let cut: Vec<&Solid3d> = xs
        .iter()
        .map(|(_, x)| x)
        .filter(|x| {
            let ((lo, hi), t) = (reach(x, o, n), tol_of(x));
            lo <= t && hi >= -t
        })
        .collect();
    let mut outlines = Vec::new();
    for c in cadcraft_kernel::sections(&cut.iter().map(|x| placed(x)).collect::<Vec<_>>(), o, n, false).map_err(kernel)? {
        let (c, _) = c.map_err(kernel)?;
        outlines.extend(c.wires.iter().filter_map(|w| polyline(w, &f, z)));
    }
    if outlines.is_empty() {
        return Err(EngineError::Other("The plane does not cut the selected objects.".into()));
    }
    let mut made = Vec::new();
    for k in outlines {
        made.push(add_in_plane(s, k, f.z)?.hex());
    }
    Ok(json!({ "handles": made }))
}

// ---------------- section objects ----------------

/// A section object as the drawing holds it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SectionObj {
    pub handle: Handle,
    pub name: String,
    pub live: bool,
    /// A point of the plane and its unit normal, toward the viewer.
    pub origin: Vec3,
    pub normal: Vec3,
}

/// `e` as a section object: a polyline with [`APP`] xdata.
pub(crate) fn section_obj(e: &Entity) -> Option<SectionObj> {
    if !matches!(e.kind, EntityKind::LwPolyline(_)) {
        return None;
    }
    let x = &e.common.xdata;
    let at = x.iter().position(|t| t.code == 1001 && t.value.trim().eq_ignore_ascii_case(APP))?;
    let mut ours = x.get(at + 1..)?.iter().take_while(|t| t.code != 1001);
    if !ours.next().is_some_and(|t| t.code == 1000 && t.value.trim() == MARK) {
        return None;
    }
    let name = ours.next().filter(|t| t.code == 1000).map(|t| t.value.clone()).unwrap_or_default();
    let live = ours.next().is_some_and(|t| t.code == 1070 && t.value.trim().parse::<i64>().is_ok_and(|v| v != 0));
    let m = e.plane()?;
    let normal = m.apply_vec(Vec3::Z).normalized();
    (normal.len() > 0.5).then(|| SectionObj { handle: e.handle, name, live, origin: m.apply(Vec3::ZERO), normal })
}

fn marks(name: &str, live: bool) -> Vec<RawTag> {
    let tag = |code: i32, value: &str| RawTag { code, value: value.to_string() };
    vec![tag(1001, APP), tag(1000, MARK), tag(1000, name), tag(1070, if live { "1" } else { "0" })]
}

/// `xdata` with our group (from its 1001 tag to the next application's) made `ours`; the other
/// applications' groups (hyperlinks, other programs' data) are kept as they were.
fn with_marks(xdata: &[RawTag], ours: Vec<RawTag>) -> Vec<RawTag> {
    let mine = |t: &RawTag| t.code == 1001 && t.value.trim().eq_ignore_ascii_case(APP);
    let Some(at) = xdata.iter().position(mine) else { return xdata.iter().cloned().chain(ours).collect() };
    let end = xdata.get(at + 1..).and_then(|rest| rest.iter().position(|t| t.code == 1001)).map_or(xdata.len(), |n| at + 1 + n);
    xdata.get(..at).unwrap_or_default().iter().cloned().chain(ours).chain(xdata.get(end..).unwrap_or_default().iter().cloned()).collect()
}

/// Every section object in model space.
fn section_objs(s: &Session) -> Result<Vec<SectionObj>> {
    Ok(s.doc()?.model.iter().filter_map(|e| section_obj(e)).collect())
}

/// The section object a command acts on: the one `key` names, else the selected one, else the
/// drawing's only one.
fn section_target(s: &Session, cmd: &str, p: &Value, key: &str) -> Result<SectionObj> {
    let d = s.doc()?;
    let of = |h: Handle| d.entity(h).and_then(|e| section_obj(e));
    if let Some(v) = p.get(key).filter(|v| !v.is_null()) {
        let h = handle_value(v).ok_or_else(|| bad(cmd, format!("{v} is not an object handle")))?;
        return of(h).ok_or_else(|| bad(cmd, format!("{} is not a section plane", h.hex())));
    }
    if let Some(x) = s.selection().into_iter().find_map(of) {
        return Ok(x);
    }
    let mut all = section_objs(s)?;
    match all.len() {
        1 => Ok(all.remove(0)),
        0 => Err(bad(cmd, "there is no section plane (SECTIONPLANE makes one)")),
        _ => Err(bad(cmd, format!("the drawing has several section planes: give `{key}` (a handle) or select one"))),
    }
}

/// Every 3D solid in model space, placed in the world (those in block references too).
fn model_solids(s: &Session, cmd: &str, p: &Value) -> Result<Vec<Solid3d>> {
    super::furniture::chosen(s, p, cmd, true)
}

fn run_sectionplane(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sectionplane";
    let all = model_solids(s, CMD, &json!({})).unwrap_or_default();
    let bx = box_of(&all);
    let (o, n) = match p.get("orthographic").filter(|v| !v.is_null()) {
        Some(v) => {
            let k = v.as_str().map(str::to_ascii_lowercase).unwrap_or_default();
            let d = ORTHO
                .iter()
                .find(|(n, _, _)| *n == k)
                .map(|(_, _, d)| Vec3::new(d[0], d[1], d[2]))
                .ok_or_else(|| bad(CMD, format!("`orthographic` is front, back, top, bottom, left or right, not {v}")))?;
            let (lo, hi) = bx.ok_or_else(|| bad(CMD, "there are no 3D solids to place an orthographic section plane in"))?;
            ((lo + hi) * 0.5, d)
        }
        None => plane_param(s, CMD, p)?,
    };
    let taken = section_objs(s)?;
    let name = match p.get("name").filter(|v| !v.is_null()) {
        Some(v) => {
            let t = v.as_str().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(CMD, "`name` must be text"))?;
            if t.chars().count() > 250 {
                return Err(bad(CMD, "`name` is longer than 250 characters"));
            }
            if taken.iter().any(|x| x.name.eq_ignore_ascii_case(t)) {
                return Err(bad(CMD, format!("there is a section plane named {t} already")));
            }
            t.to_string()
        }
        None => (1..=taken.len() + 1)
            .map(|i| format!("Section Plane({i})"))
            .find(|n| !taken.iter().any(|x| x.name.eq_ignore_ascii_case(n)))
            .unwrap_or_else(|| "Section Plane".into()),
    };
    let live = bool_or(p, "live", false);
    // The boundary: the solids' box as the plane sees it, a tenth bigger all round.
    let f = Frame::ocs(n);
    let seen = match bx {
        Some((lo, hi)) => Bounds2::from_points([0, 1, 2, 3, 4, 5, 6, 7].map(|i| {
            f.at(Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z }))
        })),
        None => Bounds2::from_points([f.at(o)]),
    };
    let grow = (seen.width().max(seen.height()) * 0.1).max(if bx.is_some() { 1e-3 } else { 50.0 });
    let (a, b) = (seen.min - Vec2::new(grow, grow), seen.max + Vec2::new(grow, grow));
    if !(a.is_finite() && b.is_finite()) {
        return Err(bad(CMD, "the section plane is out of range"));
    }
    let pl =
        cadcraft_doc::LwPolyline { vertices: helpers::rect_vertices(a, b), closed: true, const_width: 0.0, elevation: f.z.dot(o), plinegen: false };
    let h = add_in_plane(s, EntityKind::LwPolyline(pl), f.z)?;
    s.doc_mut()?.modify_entity(h, |e| e.common.xdata = marks(&name, live))?;
    Ok(json!({ "handle": h.hex(), "name": name, "live": live }))
}

fn run_livesection(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "livesection";
    let x = section_target(s, CMD, p, "handle")?;
    let on = match p.get("on") {
        None | Some(Value::Null) => !x.live,
        Some(Value::Bool(b)) => *b,
        Some(v) => return Err(bad(CMD, format!("`on` is true or false, not {v}"))),
    };
    if is_locked(s, x.handle) {
        return Err(super::solids::locked_err());
    }
    s.doc_mut()?.modify_entity(x.handle, |e| e.common.xdata = with_marks(&e.common.xdata, marks(&x.name, on)))?;
    // ponytail: the flag is kept (saved and switched) but views don't cut solids at live section
    // planes yet; that is the canvas's and the render crate's (a clip plane per tile).
    Ok(json!({
        "handle": x.handle.hex(),
        "live": on,
        "message": if on { format!("Live section on for {}.", x.name) } else { format!("Live section off for {}.", x.name) },
        "name": x.name,
    }))
}

// ---------------- the 2D section block ----------------

/// The common properties of a block's objects: layer 0, colour and linetype ByBlock (they take
/// the reference's), or the given linetype.
pub(super) fn by_block(linetype: Option<&str>) -> Common {
    Common { layer: "0".into(), color: Color::ByBlock, linetype: linetype.unwrap_or("ByBlock").into(), ..Common::default() }
}

/// Define block `name` (redefining one of that name) with `items`, its base point at their
/// lower-left corner; the block's name as stored.
pub(super) fn define_block(s: &mut Session, cmd: &str, name: &str, items: Vec<(Common, EntityKind)>) -> Result<String> {
    if name.trim().is_empty() || name.len() > 255 || name.starts_with('*') || name.chars().any(|c| "<>/\\\":;?*|,=`".contains(c)) {
        return Err(bad(cmd, format!("{name} is not a valid block name")));
    }
    let d = s.doc_mut()?;
    let name = d.blocks.keys().find(|k| k.eq_ignore_ascii_case(name)).cloned().unwrap_or_else(|| name.to_string());
    let mut b = cadcraft_doc::Block::new(&name);
    let mut bounds = Bounds2::EMPTY;
    for (common, kind) in items {
        d.ensure_layer(&common.layer);
        let e = Entity { handle: d.new_handle(), common, kind };
        bounds = bounds.union(&cadcraft_doc::entity_bounds(d, &e, 0));
        b.entities.push(e);
    }
    if !bounds.is_empty() && bounds.min.is_finite() {
        b.base = bounds.min.to3(0.0);
    }
    d.blocks.insert(name.clone(), Shared::new(b));
    Ok(name)
}

/// A hatch scale for a section `size` across: the pattern's lines about an 80th of it apart,
/// rounded down to 1, 2 or 5 times a power of ten (HPSCALE's 1 draws ANSI31 3 mm apart in a
/// metric drawing, and solid in a 700 mm cabinet drawn in an inch template).
fn hatch_scale(d: &cadcraft_doc::Drawing, pattern: &str, size: f64) -> f64 {
    let base =
        cadcraft_doc::library::pattern_for(d, pattern).and_then(|p| p.lines.first().map(|l| l.delta.1.abs())).filter(|b| *b > 1e-12).unwrap_or(1.0);
    let want = size / 80.0 / base;
    if !want.is_finite() || want <= 0.0 {
        return 1.0;
    }
    let p = 10f64.powf(want.log10().floor());
    let m = want / p;
    (if m >= 5.0 {
        5.0
    } else if m >= 2.0 {
        2.0
    } else {
        1.0
    }) * p
}

/// Where a drawing made from the solids goes by default: beside them in plan.
pub(super) fn beside(xs: &[Solid3d]) -> Vec2 {
    let b = xs.iter().fold(Bounds2::EMPTY, |a, x| a.union(&x.bounds()));
    if b.is_empty() || !b.min.is_finite() || !b.max.is_finite() { Vec2::ZERO } else { Vec2::new(b.max.x + 0.2 * b.width().max(b.height()), b.min.y) }
}

fn run_toblock(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sectionplanetoblock";
    let sec = section_target(s, CMD, p, "section")?;
    let xs = model_solids(s, CMD, p)?;
    let (o, n) = (sec.origin, sec.normal);
    let f = Frame::view(n);
    let hdr = s.doc().map(|d| (d.header.str("HPNAME", "ANSI31"), d.header.f64("HPSCALE", 1.0))).unwrap_or(("ANSI31".into(), 1.0));
    let pattern = match str_param(p, "hatch").map(str::trim) {
        Some(t) if t.eq_ignore_ascii_case("none") || t.is_empty() => None,
        Some(t) => Some(t.to_ascii_uppercase()),
        None if hdr.0.trim().is_empty() || hdr.0.starts_with('_') => Some("ANSI31".to_string()),
        None => Some(hdr.0.to_ascii_uppercase()),
    };
    let scale = size_param(CMD, p, "hatchScale", false)?;
    let background = bool_or(p, "background", true);
    let at = match p.get("at").filter(|v| !v.is_null()) {
        Some(v) => Some(p3_value(v).ok_or_else(|| bad(CMD, "`at` must be [x, y] or [x, y, z]"))?),
        None => None,
    };
    let block = match p.get("block").filter(|v| !v.is_null()) {
        Some(v) => Some(v.as_str().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(CMD, "`block` must be a block name"))?.to_string()),
        None => None,
    };
    // Everything is computed before the drawing changes; the solids the plane cuts are cut (and
    // their pieces behind it found) in one kernel call.
    let (mut items, mut behind, mut regions) = (Vec::new(), Vec::new(), 0usize);
    let mut cut_by = Vec::new();
    for x in &xs {
        let (lo, hi) = reach(x, o, n);
        let t = tol_of(x);
        if lo > t {
            continue;
        }
        if hi < -t {
            behind.push(x.clone());
            continue;
        }
        cut_by.push(x);
    }
    let cuts = cadcraft_kernel::sections(&cut_by.iter().map(|x| placed(x)).collect::<Vec<_>>(), o, n, background).map_err(kernel)?;
    for (x, c) in cut_by.into_iter().zip(cuts) {
        let (cut, back) = c.map_err(kernel)?;
        if x.mesh.open {
            items.extend(cut.wires.iter().filter_map(|w| polyline(w, &f, 0.0)).map(|k| (by_block(None), k)));
        }
        for r in &cut.regions {
            let loops: Vec<HatchLoop> = std::iter::once((&r.outer, true))
                .chain(r.holes.iter().map(|h| (h, false)))
                .filter_map(|(w, outer)| chain(w, &f).map(|(vertices, _)| HatchLoop { vertices, outer, sources: Vec::new() }))
                .collect();
            if loops.is_empty() {
                continue;
            }
            regions += 1;
            items.extend(loops.iter().map(|l| (by_block(None), helpers::lwpoly(l.vertices.clone(), true))));
            if let Some(pat) = &pattern {
                let solid = pat == "SOLID";
                let hatch = Hatch {
                    pattern: pat.clone(),
                    solid,
                    loops,
                    // Set below, once the section's size is known.
                    scale: 1.0,
                    angle: 0.0,
                    associative: false,
                    style: 0,
                    elevation: 0.0,
                    gradient: None,
                    origin: Vec2::ZERO,
                    background: None,
                    // ponytail: the standard library's patterns; a pattern loaded from a .pat file
                    // draws as ANSI31 here until its lines are looked up as HATCH does.
                    pattern_lines: Vec::new(),
                };
                items.push((by_block(None), EntityKind::Hatch(hatch)));
            }
        }
        if let Some(back) = back {
            behind.push(Solid3d { brep: Shared::from(back.brep), mesh: Shared::new(back.mesh), xform: Mat4::IDENTITY, ..Solid3d::default() });
        }
    }
    let mut seen = 0usize;
    if background && !behind.is_empty() {
        let flat = cadcraft_kernel::flatshot(&behind.iter().map(placed).collect::<Vec<_>>(), n, f.x).map_err(kernel)?;
        let lines: Vec<EntityKind> = flat.visible.iter().filter_map(flat_kind).collect();
        seen = lines.len();
        items.extend(lines.into_iter().map(|k| (by_block(None), k)));
    }
    if items.is_empty() {
        return Err(EngineError::Other("The section plane does not cut the solids or see any beyond it.".into()));
    }
    let scale = match (scale, &pattern) {
        (Some(k), _) => k,
        (None, Some(pat)) if hdr.1 == 1.0 => {
            let d = s.doc()?;
            let size = items.iter().fold(Bounds2::EMPTY, |b, (_, k)| b.union(&cadcraft_doc::entity_bounds(d, &Entity::new(Handle(0), k.clone()), 0)));
            hatch_scale(d, pat, size.width().max(size.height()))
        }
        _ if hdr.1.is_finite() && hdr.1 > 0.0 => hdr.1,
        _ => 1.0,
    };
    for (_, k) in &mut items {
        if let EntityKind::Hatch(h) = k {
            h.scale = scale;
        }
    }
    let existing = block.as_deref().is_some_and(|b| s.doc().is_ok_and(|d| d.block(b).is_some()));
    let name = block.unwrap_or_else(|| s.doc().map(|d| d.anonymous_block_name("SECTION")).unwrap_or_else(|_| "SECTION1".into()));
    let name = define_block(s, CMD, &name, items)?;
    let insert = match at {
        Some(a) => Some(a.xy()),
        None if existing => None,
        None => Some(beside(&xs)),
    };
    let h = match insert {
        Some(a) => Some(super::blocks::insert(s, &name, a, 1.0, 0.0, &serde_json::Map::new())?.hex()),
        None => None,
    };
    Ok(json!({ "block": name, "handle": h, "section": sec.handle.hex(), "regions": regions, "background": seen, "redefined": existing }))
}

// ---------------- prompts ----------------

/// Runs a command's JSON form from its prompts (echoing its message) and clears the selection.
pub(super) fn run_json(s: &mut Session, id: &str, p: Value) -> Result<Step> {
    let spec = find_command(id).ok_or_else(|| EngineError::UnknownCommand(id.into()))?;
    let r = (spec.run)(s, &p)?;
    if let Some(m) = r.get("message").and_then(Value::as_str).filter(|m| !m.is_empty()) {
        s.echo(m.to_string());
    }
    s.set_selection(Vec::new());
    Ok(Step::Done)
}

/// Typed text that starts `word` (a keyword's shortcut or more of it).
pub(super) fn is_kw(t: &str, word: &str) -> bool {
    let t = t.trim().to_ascii_lowercase();
    !t.is_empty() && word.to_ascii_lowercase().starts_with(&t)
}

fn json3(p: Vec3) -> Value {
    json!([p.x, p.y, p.z])
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PlaneBy {
    #[default]
    Ask,
    ThreePoints,
    Object,
    Zaxis,
    View,
    /// The construction plane's XY, YZ or ZX plane: its name and its normal there.
    Named(&'static str, [i8; 3]),
}

/// AutoCAD's cutting-plane prompts (SECTION's, and SLICE's options): three points, a planar
/// object, a point and the normal (Zaxis), the view, or the construction plane's XY, YZ or ZX
/// plane through a point. Points are in the construction plane with their height
/// ([`Session::point_z`]); the plane comes out as the world JSON [`plane_param`] reads.
#[derive(Default)]
pub(crate) struct PlaneAsk {
    by: PlaneBy,
    pts: Vec<Vec3>,
}

impl PlaneAsk {
    pub(crate) fn prompt(&self) -> Prompt {
        let base = self.pts.last().map(|p| p.xy());
        match (self.by, self.pts.len()) {
            (PlaneBy::Ask, _) => Prompt::new("Specify first point on Section plane", Accept::POINT)
                .kw(&["Object", "Zaxis", "View", "XY", "YZ", "ZX", "3points"])
                .default("3points"),
            (PlaneBy::ThreePoints, 0) => Prompt::new("Specify first point on plane", Accept::POINT),
            (PlaneBy::ThreePoints, 1) => Prompt::new("Specify second point on plane", Accept::POINT).base_opt(base),
            (PlaneBy::ThreePoints, _) => Prompt::new("Specify third point on plane", Accept::POINT).base_opt(base),
            (PlaneBy::Object, _) => Prompt::new("Select a circle, ellipse, arc, 2D-spline, or 2D-polyline", Accept::POINT),
            (PlaneBy::Zaxis, 0) => Prompt::new("Specify a point on the section plane", Accept::POINT),
            (PlaneBy::Zaxis, _) => Prompt::new("Specify a point on the Z-axis (normal) of the plane", Accept::POINT).base_opt(base),
            (PlaneBy::View, _) => Prompt::new("Specify a point on the current view plane", Accept::POINT).default("0,0,0"),
            (PlaneBy::Named("XY", _), _) => Prompt::new("Specify a point on the XY-plane", Accept::POINT).default("0,0,0"),
            (PlaneBy::Named("YZ", _), _) => Prompt::new("Specify a point on the YZ-plane", Accept::POINT).default("0,0,0"),
            (PlaneBy::Named(..), _) => Prompt::new("Specify a point on the ZX-plane", Accept::POINT).default("0,0,0"),
        }
    }

    /// The plane once it is known (world JSON), `None` while more is asked.
    pub(crate) fn input(&mut self, s: &mut Session, i: &Input) -> Result<Option<Value>> {
        let c = s.cplane();
        let word = match i {
            Input::Text(t) | Input::Keyword(t) => t.trim().to_ascii_lowercase(),
            _ => String::new(),
        };
        if self.by == PlaneBy::Ask {
            self.by = match i {
                Input::Enter => PlaneBy::ThreePoints,
                Input::Point(_) => PlaneBy::ThreePoints,
                _ => match word.as_str() {
                    "xy" => PlaneBy::Named("XY", [0, 0, 1]),
                    "yz" => PlaneBy::Named("YZ", [1, 0, 0]),
                    "zx" | "xz" => PlaneBy::Named("ZX", [0, 1, 0]),
                    w if is_kw(w, "object") => PlaneBy::Object,
                    w if is_kw(w, "zaxis") => PlaneBy::Zaxis,
                    w if is_kw(w, "view") => PlaneBy::View,
                    w if is_kw(w, "3points") => PlaneBy::ThreePoints,
                    _ => return Err(EngineError::Other("Requires a point or an option keyword.".into())),
                },
            };
            if !matches!(i, Input::Point(_)) {
                return Ok(None);
            }
        }
        if self.by == PlaneBy::Object {
            let h = match i {
                Input::Point(p) => pick_at(s, *p),
                Input::Pick(hs) => hs.first().copied(),
                _ => return Ok(None),
            };
            let Some(h) = h else { return Err(EngineError::Other("No object found.".into())) };
            return Ok(Some(json!({ "object": h.hex() })));
        }
        let p = match i {
            Input::Point(p) => p.to3(s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0)),
            Input::Enter if matches!(self.by, PlaneBy::View | PlaneBy::Named(..)) => Vec3::ZERO,
            _ => return Ok(None),
        };
        if self.pts.iter().any(|q| (p - *q).len() <= 1e-9 * (1.0 + p.len())) {
            return Err(EngineError::Other("The points must be different.".into()));
        }
        match (self.by, self.pts.as_slice()) {
            (PlaneBy::View, _) => Ok(Some(json!({ "plane": "view", "point": json3(c.apply(p)) }))),
            (PlaneBy::Named(_, d), _) => {
                let n = c.apply_vec(Vec3::new(f64::from(d[0]), f64::from(d[1]), f64::from(d[2])));
                Ok(Some(json!({ "origin": json3(c.apply(p)), "normal": json3(n) })))
            }
            (PlaneBy::Zaxis, [a]) => Ok(Some(json!({ "origin": json3(c.apply(*a)), "normal": json3(c.apply_vec(p - *a)) }))),
            (PlaneBy::ThreePoints, [a, b]) => {
                let n = (*b - *a).cross(p - *a);
                if !n.is_finite() || n.len() <= 1e-12 * (*b - *a).len().max((p - *a).len()).powi(2) {
                    return Err(EngineError::Other("The three points are on one line: specify a third point off it.".into()));
                }
                Ok(Some(json!({ "points": [json3(c.apply(*a)), json3(c.apply(*b)), json3(c.apply(p))] })))
            }
            _ => {
                self.pts.push(p);
                Ok(None)
            }
        }
    }
}

/// SECTION: the objects, then the plane.
#[derive(Default)]
struct SectionM {
    sel: SelectPhase,
    hs: Vec<Handle>,
    plane: PlaneAsk,
}

impl Interactive for SectionM {
    fn name(&self) -> &'static str {
        "SECTION"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.hs = self.sel.picked.clone();
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.hs.is_empty() { self.sel.prompt() } else { self.plane.prompt() }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_empty() {
            return Ok(match self.sel.feed(s, &i)? {
                SelOutcome::More => Step::Continue,
                SelOutcome::Empty => Step::Done,
                SelOutcome::Done(hs) => {
                    self.hs = hs;
                    Step::Continue
                }
            });
        }
        let Some(mut p) = self.plane.input(s, &i)? else { return Ok(Step::Continue) };
        p["handles"] = json!(self.hs.iter().map(|h| h.hex()).collect::<Vec<_>>());
        let r = run_json(s, "section", p);
        if r.is_err() {
            self.plane = PlaneAsk::default();
        }
        r
    }
}

/// SECTIONPLANE: a point and a through point (a plane standing square to the construction
/// plane), or Orthographic and a view.
/// ponytail: AutoCAD's face pick, Draw section (a jogged section line) and Type (slice, boundary,
/// volume) when sections need them; a point picked on a solid's face with a 3D snap takes its
/// height.
#[derive(Default)]
struct SectionPlaneM {
    /// The first point, in the construction plane and in the world.
    first: Option<(Vec2, Vec3)>,
    ortho: bool,
}

impl Interactive for SectionPlaneM {
    fn name(&self) -> &'static str {
        "SECTIONPLANE"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match (self.ortho, self.first) {
            (true, _) => Prompt::new("Enter an option", KW).kw(&["Front", "bAck", "Top", "Bottom", "Left", "Right"]).default("Top"),
            (false, Some((a, _))) => Prompt::new("Specify through point", Accept::POINT).base(a),
            (false, None) => Prompt::new("Specify any point to locate section line", Accept::POINT).kw(&["Orthographic"]),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        let word = match &i {
            Input::Text(t) | Input::Keyword(t) => t.trim().to_ascii_lowercase(),
            _ => String::new(),
        };
        if self.ortho {
            let name = match &i {
                Input::Enter => "top",
                _ => match ORTHO
                    .iter()
                    .find(|(n, k, _)| word == *n || word == k.chars().filter(char::is_ascii_uppercase).collect::<String>().to_ascii_lowercase())
                {
                    Some((n, _, _)) => n,
                    None if word.is_empty() => return Ok(Step::Continue),
                    None => return Err(EngineError::Other("Requires Front, bAck, Top, Bottom, Left or Right.".into())),
                },
            };
            return run_json(s, "sectionplane", json!({ "orthographic": name }));
        }
        let c = s.cplane();
        match (&i, self.first) {
            (Input::Point(p), None) => {
                let w = c.apply(p.to3(s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0)));
                self.first = Some((*p, w));
                Ok(Step::Continue)
            }
            (Input::Point(p), Some((_, a))) => {
                // The plane holds the line between the points and stands square to the
                // construction plane; the viewer is on the line's right, seen from above.
                let b = c.apply(p.to3(s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0)));
                let n = (b - a).cross(c.apply_vec(Vec3::Z));
                if !n.is_finite() || n.len() <= 1e-9 * (1.0 + (b - a).len()) {
                    return Err(EngineError::Other("The points must be different.".into()));
                }
                run_json(s, "sectionplane", json!({ "origin": json3(a), "normal": json3(n) }))
            }
            (Input::Text(_) | Input::Keyword(_), None) if is_kw(&word, "orthographic") => {
                self.ortho = true;
                Ok(Step::Continue)
            }
            _ => Ok(Step::Continue),
        }
    }
}

/// LIVESECTION and SECTIONPLANETOBLOCK: the section object (a selected one, or Enter: the
/// drawing's only one), then for the block its insertion point (Enter: beside the model).
struct PickSectionM {
    id: &'static str,
    name: &'static str,
    chosen: Option<Handle>,
}

fn is_section(s: &Session, h: Handle) -> bool {
    s.doc().is_ok_and(|d| d.entity(h).and_then(|e| section_obj(e)).is_some())
}

impl PickSectionM {
    fn take(&mut self, s: &mut Session, h: Handle) -> Result<Step> {
        if self.id == "livesection" {
            return run_json(s, self.id, json!({ "handle": h.hex() }));
        }
        self.chosen = Some(h);
        Ok(Step::Continue)
    }
}

impl Interactive for PickSectionM {
    fn name(&self) -> &'static str {
        self.name
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        match s.selection().into_iter().find(|h| is_section(s, *h)) {
            Some(h) => self.take(s, h),
            None => Ok(Step::Continue),
        }
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.chosen.is_some() {
            Prompt::new("Specify insertion point", Accept::POINT)
        } else {
            Prompt::new("Select section object", Accept::POINT)
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if let Some(h) = self.chosen {
            let mut p = json!({ "section": h.hex() });
            match i {
                Input::Point(a) => p["at"] = json!([a.x, a.y]),
                Input::Enter => {}
                _ => return Ok(Step::Continue),
            }
            return run_json(s, self.id, p);
        }
        let h = match &i {
            Input::Point(p) => pick_at(s, *p),
            Input::Pick(hs) => hs.iter().copied().find(|h| is_section(s, *h)),
            Input::Enter => match section_objs(s)?.as_slice() {
                [only] => Some(only.handle),
                _ => None,
            },
            _ => return Ok(Step::Continue),
        };
        match h.filter(|h| is_section(s, *h)) {
            Some(h) => self.take(s, h),
            None => Err(EngineError::Other("Select a section object.".into())),
        }
    }
}

#[cfg(test)]
#[path = "section_tests.rs"]
mod tests;
