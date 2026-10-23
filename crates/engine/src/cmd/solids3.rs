//! 3D operations and conversions (CAD-rhino): 3DARRAY, 3DALIGN, POLYSOLID, CONVTOSOLID,
//! CONVTOSURFACE and 3DPRINT, and the closed planar loops PLANESURF and CONVTOSURFACE make faces
//! with holes from. Shapes come from [`cadcraft_kernel`] as in `solids`; arrays and alignments
//! move objects with [`Entity::transform3`] as 3DMOVE does.

use std::f64::consts::{PI, TAU};
use std::path::Path;

use cadcraft_doc::{Entity, EntityKind, Handle, MAX_ARRAY_ITEMS, Solid3d};
use cadcraft_geom::{EPS, Mat4, PolyVertex, Vec2, Vec3, arc_to_bulge, bulge_to_arc, point_in_polygon};
use cadcraft_kernel::{Built, Curve, Justify, MAX_PROFILE_SEGS, Seg};
use serde_json::{Map, Value, json};

use super::curves::{KW, is_locked, pick_at};
use super::machines::{SelOutcome, SelectPhase, SelectRun, number};
use super::solids::{
    add, all_or_nothing, circle_path, curve_of, curves_of, in_plane, kernel, locked_err, normal_of, p3_value, placed, profile, reshape, solids,
};
use super::solids2::{add_like, axis_param, feed, handle_opt, hexes, json3, once, plane_normal, run_json, transform3, type_name};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("3darray", "3D Array", run_3darray)
            .menu(&["Modify", "3D Operations", "3D Array"])
            .alias(&["3a"])
            .params(
                "{handles?, type?: \"rectangular\" (default) | \"polar\"; rectangular: rows?, columns?, levels? (1 by default), rowSpacing, columnSpacing, levelSpacing \
                 (along the construction plane's Y, X and Z; needed when there is more than one); polar: count (2 or more), fill? (degrees, default 360; negative: \
                 clockwise), rotate?: bool (default true), axis: [[x,y,z],[x,y,z]]}: copies, not an associative array",
            )
            .interactive(|_| Ok(Box::new(Array3M::default()))),
        CommandSpec::new("3dalign", "3D Align", run_3dalign)
            .menu(&["Modify", "3D Operations", "3D Align"])
            .alias(&["3al"])
            .params(
                "{handles?, source: [p1, p2?, p3?], destination: [p1, p2?, p3?] (world points), copy?: bool}: the base point onto the first destination point, \
                 the direction to the second onto the second's, the plane of the three onto theirs",
            )
            .interactive(|_| Ok(Box::new(Align3M::default()))),
        CommandSpec::new("polysolid", "Polysolid", run_polysolid)
            .menu(&["Draw", "Modeling", "Polysolid"])
            .alias(&["psolid"])
            .params(
                "{points: [[x,y,z?], …] (in the construction plane, at the first point's height), bulges?: [b, …] (each point's arc to the next), closed?: bool \
                 | handle: a line, arc, 2D polyline or circle to turn into a wall (it is erased); height?, width? (default PSOLHEIGHT, PSOLWIDTH), \
                 justify?: \"left\" | \"center\" | \"right\"}",
            )
            .interactive(|_| Ok(Box::new(PolysolidM::default()))),
        CommandSpec::new("convtosolid", "Convert to Solid", run_convtosolid)
            .menu(&["Modify", "3D Operations", "Convert to Solid"])
            .params("{handles?}: closed polylines and circles with thickness, wide polylines with thickness, meshes and watertight surfaces become 3D solids")
            .interactive(|_| Ok(Box::new(SelectRun::new("convtosolid", "CONVTOSOLID")))),
        CommandSpec::new("convtosurface", "Convert to Surface", run_convtosurface)
            .menu(&["Modify", "3D Operations", "Convert to Surface"])
            .params("{handles?}: 3D solids, meshes, regions, 2D solids, 3D faces, and lines, arcs, circles and polylines with thickness become surfaces")
            .interactive(|_| Ok(Box::new(SelectRun::new("convtosurface", "CONVTOSURFACE")))),
        CommandSpec::new("3dprint", "3D Print", run_3dprint)
            .menu(&["File", "3D Print"])
            .params("{path, handles?} (3D solids: those given or selected, else all of them): writes them to an STL file for a 3D printer or print service")
            .noundo()
            .interactive(|_| Ok(Box::new(PrintM::default()))),
    ]
}

// ---------------- 3DARRAY ----------------

/// A whole number from `min` to [`MAX_ARRAY_ITEMS`]; `default` when left out (required without).
fn count_param(cmd: &str, p: &Value, key: &str, min: u64, default: Option<u64>) -> Result<u64> {
    match p.get(key) {
        None | Some(Value::Null) => default.ok_or_else(|| bad(cmd, format!("`{key}` (a whole number) is required"))),
        Some(v) => v
            .as_f64()
            .filter(|x| x.fract() == 0.0 && *x >= min as f64 && *x <= MAX_ARRAY_ITEMS as f64)
            .map(|x| x as u64)
            .ok_or_else(|| bad(cmd, format!("`{key}` must be a whole number of at least {min}"))),
    }
}

/// The middle of the objects' extent: the point a polar array's copies carry round without
/// turning.
fn middle_of(s: &Session, hs: &[Handle]) -> Result<Vec3> {
    let d = s.doc()?;
    let mut pts: Vec<Vec3> = Vec::new();
    for h in hs {
        let Some(e) = d.entity(*h) else { continue };
        match (&e.kind, curve_of(e)) {
            (EntityKind::Solid3d(x), _) => pts.extend(x.mesh.verts.iter().map(|p| x.world(*p))),
            (_, Ok(Some(c))) => pts.extend(c.points()),
            _ => {
                let b = cadcraft_doc::entity_bounds(d, e, 0);
                pts.extend([b.min.to3(0.0), b.max.to3(0.0)]);
            }
        }
    }
    let pts: Vec<Vec3> = pts.into_iter().filter(|p| p.is_finite()).collect();
    let Some(first) = pts.first().copied() else { return Ok(Vec3::ZERO) };
    let (lo, hi) = pts.iter().fold((first, first), |(lo, hi), p| {
        (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
    });
    Ok((lo + hi) * 0.5)
}

/// 3DARRAY: copies of the objects in rows (the construction plane's Y), columns (X) and levels
/// (Z), or round an axis; each copy is an object of its own, as AutoCAD's 3DARRAY makes them.
fn run_3darray(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "3darray";
    let hs = once(targets(s, p)?);
    let polar = match p.get("type") {
        None | Some(Value::Null) => false,
        Some(Value::String(t)) if matches!(t.to_ascii_lowercase().as_str(), "rectangular" | "r") => false,
        Some(Value::String(t)) if matches!(t.to_ascii_lowercase().as_str(), "polar" | "p") => true,
        Some(t) => return Err(bad(CMD, format!("`type` is \"rectangular\" or \"polar\", not {t}"))),
    };
    let mats: Vec<Mat4> = if polar {
        let n = count_param(CMD, p, "count", 2, None)?;
        let fill = match p.get("fill") {
            None | Some(Value::Null) => 360.0,
            Some(v) => v
                .as_f64()
                .filter(|f| f.is_finite() && *f != 0.0 && f.abs() <= 360.0)
                .ok_or_else(|| bad(CMD, "`fill` is an angle from -360 to 360 degrees, not zero"))?,
        };
        let rotate = match p.get("rotate") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(b)) => *b,
            Some(v) => return Err(bad(CMD, format!("`rotate` is true or false, not {v}"))),
        };
        let (a, b) = axis_param(CMD, p)?;
        super::array::check_size(s, n, hs.len())?;
        // A whole turn spreads the items evenly; less puts the last at the end of the angle.
        let step = if fill.abs() >= 360.0 { fill / n as f64 } else { fill / (n - 1) as f64 };
        let mid = middle_of(s, &hs)?;
        (1..n)
            .map(|i| {
                let r = Mat4::rotate_about(a, b - a, (step * i as f64).to_radians());
                if rotate { r } else { Mat4::translate(r.apply(mid) - mid) }
            })
            .collect()
    } else {
        let (rows, cols, levels) =
            (count_param(CMD, p, "rows", 1, Some(1))?, count_param(CMD, p, "columns", 1, Some(1))?, count_param(CMD, p, "levels", 1, Some(1))?);
        let items = rows.checked_mul(cols).and_then(|n| n.checked_mul(levels)).unwrap_or(u64::MAX);
        super::array::check_size(s, items, hs.len())?;
        if items < 2 {
            return Err(bad(CMD, "give more than one row, column or level"));
        }
        let gap = |n: u64, key: &str| -> Result<f64> {
            if n < 2 {
                return Ok(0.0);
            }
            match f64_req(CMD, p, key)? {
                0.0 => Err(bad(CMD, format!("`{key}` must not be zero"))),
                d => Ok(d),
            }
        };
        let (dy, dx, dz) = (gap(rows, "rowSpacing")?, gap(cols, "columnSpacing")?, gap(levels, "levelSpacing")?);
        let c = s.cplane();
        let mut v = Vec::new();
        for l in 0..levels {
            for r in 0..rows {
                for k in 0..cols {
                    if (l, r, k) != (0, 0, 0) {
                        v.push(Mat4::translate(c.apply_vec(Vec3::new(k as f64 * dx, r as f64 * dy, l as f64 * dz))));
                    }
                }
            }
        }
        v
    };
    let items = mats.len() + 1;
    let (handles, locked) = all_or_nothing(s, |s| {
        let (mut out, mut locked) = (Vec::new(), 0);
        for m in &mats {
            let r = transform3(s, CMD, &hs, m, true)?;
            locked = r.get("locked").and_then(Value::as_u64).unwrap_or(0);
            out.extend(r.get("handles").and_then(Value::as_array).cloned().unwrap_or_default());
        }
        Ok((out, locked))
    })?;
    Ok(json!({ "handles": handles, "items": items, "locked": locked }))
}

/// One question of 3DARRAY at the command line: the JSON key it fills.
#[derive(Clone, Copy, PartialEq, Eq)]
enum A3 {
    Count(&'static str, &'static str, u64),
    Dist(&'static str, &'static str),
    Fill,
    Rotate,
    Center,
    Axis,
}

/// 3DARRAY: the objects, Rectangular (rows, columns, levels and the distances between them) or
/// Polar (items, angle to fill, whether the copies turn, and the axis by two points).
#[derive(Default)]
struct Array3M {
    sel: SelectPhase,
    hs: Vec<Handle>,
    polar: Option<bool>,
    vals: Map<String, Value>,
    /// The axis's first point (polar), in the construction plane.
    center: Option<Vec3>,
}

impl Array3M {
    fn next(&self) -> Option<A3> {
        let has = |k: &str| self.vals.contains_key(k);
        let n = |k: &str| self.vals.get(k).and_then(Value::as_u64).unwrap_or(1);
        let qs: Vec<A3> = if self.polar? {
            vec![A3::Count("count", "Enter the number of items in the array", 2), A3::Fill, A3::Rotate, A3::Center, A3::Axis]
        } else {
            vec![
                A3::Count("rows", "Enter the number of rows (---)", 1),
                A3::Count("columns", "Enter the number of columns (|||)", 1),
                A3::Count("levels", "Enter the number of levels (...)", 1),
                A3::Dist("rowSpacing", "Specify the distance between rows (---)"),
                A3::Dist("columnSpacing", "Specify the distance between columns (|||)"),
                A3::Dist("levelSpacing", "Specify the distance between levels (...)"),
            ]
        };
        qs.into_iter().find(|q| match q {
            A3::Count(k, ..) => !has(k),
            A3::Dist(k, _) => {
                let of = match *k {
                    "rowSpacing" => "rows",
                    "columnSpacing" => "columns",
                    _ => "levels",
                };
                n(of) > 1 && !has(k)
            }
            A3::Fill => !has("fill"),
            A3::Rotate => !has("rotate"),
            A3::Center => self.center.is_none(),
            A3::Axis => true,
        })
    }
    fn run(&mut self, s: &mut Session) -> Result<Step> {
        let mut p = Value::Object(self.vals.clone());
        p["handles"] = json!(hexes(&self.hs));
        p["type"] = json!(if self.polar == Some(true) { "polar" } else { "rectangular" });
        run_json(s, "3darray", p)
    }
}

impl Interactive for Array3M {
    fn name(&self) -> &'static str {
        "3DARRAY"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.hs = self.sel.picked.clone();
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.hs.is_empty() {
            return self.sel.prompt();
        }
        let Some(_) = self.polar else { return Prompt::new("Enter the type of array", KW).kw(&["Rectangular", "Polar"]).default("R") };
        match self.next() {
            Some(A3::Count(_, msg, min)) => {
                let p = Prompt::new(msg, Accept::NUMBER);
                if min == 1 { p.default("1") } else { p }
            }
            Some(A3::Dist(_, msg)) => Prompt::new(msg, Accept::NUMBER),
            Some(A3::Fill) => Prompt::new("Specify the angle to fill (+=ccw, -=cw)", Accept::NUMBER).default("360"),
            Some(A3::Rotate) => Prompt::new("Rotate arrayed objects?", KW).kw(&["Yes", "No"]).default("Y"),
            Some(A3::Center) => Prompt::new("Specify center point of array", Accept::POINT),
            Some(A3::Axis) | None => Prompt::new("Specify second point on axis of rotation", Accept::POINT).base_opt(self.center.map(|c| c.xy())),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_empty() {
            return feed(&mut self.sel, &mut self.hs, s, &i);
        }
        let word = match &i {
            Input::Text(t) | Input::Keyword(t) => self.prompt(s).match_keyword(t),
            _ => None,
        };
        if self.polar.is_none() {
            match (word.as_deref(), &i) {
                (Some("Polar"), _) => self.polar = Some(true),
                (Some("Rectangular"), _) | (_, Input::Enter) => self.polar = Some(false),
                _ => s.echo("Requires Rectangular or Polar."),
            }
            return Ok(Step::Continue);
        }
        let z = s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0);
        match (self.next(), &i) {
            (Some(A3::Count(k, _, min)), _) => {
                let max = MAX_ARRAY_ITEMS;
                let n = match &i {
                    Input::Enter if min == 1 => 1,
                    Input::Text(t) => number(t)
                        .filter(|v| v.fract() == 0.0 && *v >= min as f64 && *v <= max as f64)
                        .map(|v| v as u64)
                        .ok_or_else(|| EngineError::Other(format!("Requires an integer between {min} and {max}.")))?,
                    _ => return Ok(Step::Continue),
                };
                self.vals.insert(k.into(), json!(n));
            }
            (Some(A3::Dist(k, _)), Input::Text(t)) => {
                let d = number(t).filter(|d| d.is_finite() && *d != 0.0).ok_or_else(|| EngineError::Other("Requires a nonzero distance.".into()))?;
                self.vals.insert(k.into(), json!(d));
            }
            (Some(A3::Fill), Input::Enter) => {
                self.vals.insert("fill".into(), json!(360.0));
            }
            (Some(A3::Fill), Input::Text(t)) => {
                let a = number(t)
                    .filter(|a| a.is_finite() && *a != 0.0 && a.abs() <= 360.0)
                    .ok_or_else(|| EngineError::Other("Requires an angle from -360 to 360, not zero.".into()))?;
                self.vals.insert("fill".into(), json!(a));
            }
            (Some(A3::Rotate), _) => {
                let yes = match (word.as_deref(), &i) {
                    (Some("Yes"), _) | (_, Input::Enter) => true,
                    (Some("No"), _) => false,
                    _ => return Ok(Step::Continue),
                };
                self.vals.insert("rotate".into(), json!(yes));
            }
            (Some(A3::Center), Input::Point(p)) => self.center = Some(p.to3(z)),
            (Some(A3::Axis), Input::Point(p)) => {
                let (Some(a), b) = (self.center, p.to3(z)) else { return Ok(Step::Continue) };
                if (b - a).len() <= EPS {
                    return Err(EngineError::Other("The points must be different.".into()));
                }
                let c = s.cplane();
                self.vals.insert("axis".into(), json!([json3(c.apply(a)), json3(c.apply(b))]));
                return self.run(s);
            }
            _ => return Ok(Step::Continue),
        }
        if self.next().is_none() { self.run(s) } else { Ok(Step::Continue) }
    }
}

// ---------------- 3DALIGN ----------------

/// The rotation turning direction `u` onto `v` the shortest way.
fn turn(u: Vec3, v: Vec3) -> Mat4 {
    let (u, v) = (u.normalized(), v.normalized());
    let axis = u.cross(v);
    let (sin, cos) = (axis.len(), u.dot(v));
    if sin > 1e-12 {
        Mat4::rotation(axis, sin.atan2(cos))
    } else if cos > 0.0 {
        Mat4::IDENTITY
    } else {
        // Opposite: half a turn about a line square to them.
        let side = if u.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
        Mat4::rotation(u.cross(side), PI)
    }
}

/// 3DALIGN's motion: the first source point onto the first destination point, the direction to
/// the second onto the direction to the second, and the plane of the three onto the plane of the
/// three (for as many pairs as both have). It moves and turns, never scales.
pub(super) fn align_matrix(src: &[Vec3], dst: &[Vec3]) -> std::result::Result<Mat4, &'static str> {
    let pair = |i: usize| src.get(i).copied().zip(dst.get(i).copied());
    let Some((s1, d1)) = pair(0) else { return Err("give a source and a destination point") };
    let Some((s2, d2)) = pair(1) else { return Ok(Mat4::translate(d1 - s1)) };
    let apart = |a: Vec3, b: Vec3| (b - a).len() > EPS * (1.0 + a.len());
    if !apart(s1, s2) || !apart(d1, d2) {
        return Err("the first and second points must be different");
    }
    let Some((s3, d3)) = pair(2) else { return Ok(Mat4::translate(d1).mul(&turn(s2 - s1, d2 - d1)).mul(&Mat4::translate(-s1))) };
    let from = Mat4::frame(s1, s2 - s1, s3 - s1).ok_or("the three source points are on one line")?;
    let to = Mat4::frame(d1, d2 - d1, d3 - d1).ok_or("the three destination points are on one line")?;
    Ok(to.mul(&from.inverse().ok_or("the three source points are on one line")?))
}

/// One to three world points.
fn points3(cmd: &str, p: &Value, key: &str) -> Result<Vec<Vec3>> {
    let pts: Option<Vec<Vec3>> =
        p.get(key).and_then(Value::as_array).filter(|a| (1..=3).contains(&a.len())).and_then(|a| a.iter().map(p3_value).collect());
    pts.ok_or_else(|| bad(cmd, format!("`{key}` must be one to three points [[x,y,z], …]")))
}

fn run_3dalign(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "3dalign";
    let hs = targets(s, p)?;
    let (src, dst) = (points3(CMD, p, "source")?, points3(CMD, p, "destination")?);
    let m = align_matrix(&src, &dst).map_err(|e| bad(CMD, e))?;
    transform3(s, CMD, &hs, &m, bool_or(p, "copy", false))
}

/// 3DALIGN: the objects; the source base point (or Copy), second and third points (or
/// Continue); then as many destination points (or eXit). Points are in the construction plane.
#[derive(Default)]
struct Align3M {
    sel: SelectPhase,
    hs: Vec<Handle>,
    copy: bool,
    src: Vec<Vec3>,
    /// The source points are all given.
    src_done: bool,
    dst: Vec<Vec3>,
}

impl Align3M {
    fn picked(&mut self, s: &mut Session) {
        if !self.hs.is_empty() {
            s.echo("Specify source plane and orientation ...");
        }
    }
    fn source_done(&mut self, s: &mut Session) -> Result<Step> {
        self.src_done = true;
        s.echo("Specify destination plane and orientation ...");
        Ok(Step::Continue)
    }
    fn run(&self, s: &mut Session) -> Result<Step> {
        let c = s.cplane();
        let w = |ps: &[Vec3]| ps.iter().map(|p| json3(c.apply(*p))).collect::<Vec<_>>();
        run_json(s, "3dalign", json!({ "handles": hexes(&self.hs), "source": w(&self.src), "destination": w(&self.dst), "copy": self.copy }))
    }
}

impl Interactive for Align3M {
    fn name(&self) -> &'static str {
        "3DALIGN"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.hs = self.sel.picked.clone();
            self.picked(s);
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.hs.is_empty() {
            return self.sel.prompt();
        }
        let base = |v: &[Vec3]| v.last().map(|p| p.xy());
        if !self.src_done {
            return match self.src.len() {
                0 => Prompt::new("Specify base point", Accept::POINT).kw(&["Copy"]),
                1 => Prompt::new("Specify second point", Accept::POINT).kw(&["Continue"]).default("C").base_opt(base(&self.src)),
                _ => Prompt::new("Specify third point", Accept::POINT).kw(&["Continue"]).default("C").base_opt(base(&self.src)),
            };
        }
        match self.dst.len() {
            0 => Prompt::new("Specify first destination point", Accept::POINT),
            1 => Prompt::new("Specify second destination point", Accept::POINT).kw(&["eXit"]).default("X").base_opt(base(&self.dst)),
            _ => Prompt::new("Specify third destination point", Accept::POINT).kw(&["eXit"]).default("X").base_opt(base(&self.dst)),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_empty() {
            let step = feed(&mut self.sel, &mut self.hs, s, &i)?;
            self.picked(s);
            return Ok(step);
        }
        let word = match &i {
            Input::Text(t) | Input::Keyword(t) => self.prompt(s).match_keyword(t),
            _ => None,
        };
        let z = s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0);
        let pts = if self.src_done { &self.dst } else { &self.src };
        match (&i, word.as_deref()) {
            (Input::Point(p), _) => {
                let p = p.to3(z);
                if pts.iter().any(|q| (p - *q).len() <= EPS) {
                    return Err(EngineError::Other("The points must be different.".into()));
                }
                if let [a, b] = pts.as_slice()
                    && plane_normal(*a, *b, p).is_none()
                {
                    return Err(EngineError::Other("The three points are on one line: specify a third point off it.".into()));
                }
                if self.src_done {
                    self.dst.push(p);
                    if self.dst.len() >= self.src.len() {
                        return self.run(s);
                    }
                } else {
                    self.src.push(p);
                    if self.src.len() == 3 {
                        return self.source_done(s);
                    }
                }
                Ok(Step::Continue)
            }
            (_, Some("Copy")) => {
                self.copy = true;
                Ok(Step::Continue)
            }
            (Input::Enter, _) | (_, Some("Continue" | "eXit")) if !pts.is_empty() => {
                if self.src_done {
                    self.run(s)
                } else {
                    self.source_done(s)
                }
            }
            _ => Ok(Step::Continue),
        }
    }
}

// ---------------- POLYSOLID ----------------

/// PSOLHEIGHT or PSOLWIDTH: the drawing's, else AutoCAD's initial value (metric or imperial).
fn psol(s: &Session, var: &str) -> f64 {
    let metric = s.doc().is_ok_and(|d| d.header.i64("MEASUREMENT", 0) == 1);
    let initial = match (var == "PSOLHEIGHT", metric) {
        (true, true) => 80.0,
        (true, false) => 4.0,
        (false, true) => 5.0,
        (false, false) => 0.25,
    };
    s.doc().map(|d| d.header.f64(var, initial)).ok().filter(|v| v.is_finite() && *v > 0.0).unwrap_or(initial)
}

fn justify_param(cmd: &str, p: &Value) -> Result<Justify> {
    let j = match p.get("justify") {
        None | Some(Value::Null) => return Ok(Justify::Center),
        Some(Value::String(j)) => j.to_ascii_lowercase(),
        Some(j) => return Err(bad(cmd, format!("`justify` is \"left\", \"center\" or \"right\", not {j}"))),
    };
    match j.as_str() {
        "left" | "l" => Ok(Justify::Left),
        "center" | "c" => Ok(Justify::Center),
        "right" | "r" => Ok(Justify::Right),
        _ => Err(bad(cmd, format!("`justify` is \"left\", \"center\" or \"right\", not \"{j}\""))),
    }
}

/// POLYSOLID's drawn path: points in the construction plane (at the first one's height) joined by
/// lines, or by arcs where a point has a bulge (as a polyline's vertices).
fn drawn_path(cmd: &str, p: &Value) -> Result<Vec<Seg>> {
    let pts: Vec<Vec3> = p
        .get("points")
        .and_then(Value::as_array)
        .filter(|a| a.len() <= MAX_PROFILE_SEGS)
        .and_then(|a| a.iter().map(p3_value).collect::<Option<Vec<_>>>())
        .ok_or_else(|| bad(cmd, "`points` must be an array of points [[x,y,z?], …]"))?;
    let bulges: Vec<f64> = match p.get("bulges") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) if a.len() <= pts.len() => {
            a.iter().map(|b| b.as_f64().filter(|b| b.is_finite())).collect::<Option<Vec<_>>>().ok_or_else(|| bad(cmd, "`bulges` must be numbers"))?
        }
        Some(_) => return Err(bad(cmd, "`bulges` must be an array of numbers, one for each point")),
    };
    let closed = bool_or(p, "closed", false);
    let z = pts.first().map_or(0.0, |q| q.z);
    // A point on the one before it keeps that position and takes the later bulge.
    let mut vs: Vec<(Vec2, f64)> = Vec::with_capacity(pts.len());
    for (i, q) in pts.iter().enumerate() {
        let b = bulges.get(i).copied().unwrap_or(0.0);
        match vs.last_mut() {
            Some(l) if l.0.near(q.xy(), EPS) => l.1 = b,
            _ => vs.push((q.xy(), b)),
        }
    }
    if closed && vs.len() > 2 && vs.first().zip(vs.last()).is_some_and(|(a, b)| a.0.near(b.0, EPS)) {
        vs.pop();
    }
    let n = vs.len();
    if n < 2 {
        return Err(bad(cmd, "the path needs two different points"));
    }
    Ok((0..if closed { n } else { n - 1 })
        .filter_map(|i| {
            let ((a, bulge), (b, _)) = (*vs.get(i)?, *vs.get((i + 1) % n)?);
            // An arc too flat to tell from its chord is a line (as polyline profiles are).
            let radius = a.dist(b) * (1.0 + bulge * bulge) / (4.0 * bulge.abs());
            Some(if bulge.abs() >= 1e-6 && radius <= 1e8 {
                let mid = a.mid(b) - (b - a).perp() * (bulge / 2.0);
                Seg::Arc { a: a.to3(z), mid: mid.to3(z), b: b.to3(z) }
            } else {
                Seg::Line { a: a.to3(z), b: b.to3(z) }
            })
        })
        .collect())
}

/// POLYSOLID: a wall drawn along points (in the construction plane) or along a line, arc, 2D
/// polyline or circle, which it replaces (AutoCAD's default DELOBJ).
fn run_polysolid(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "polysolid";
    let width = size_param(CMD, p, "width", false)?.unwrap_or_else(|| psol(s, "PSOLWIDTH"));
    let height = size_param(CMD, p, "height", false)?.unwrap_or_else(|| psol(s, "PSOLHEIGHT"));
    let justify = justify_param(CMD, p)?;
    if p.get("points").is_some_and(|v| !v.is_null()) {
        let path = drawn_path(CMD, p)?;
        return add(s, cadcraft_kernel::polysolid(&path, width, height, justify, Vec3::Z).map_err(kernel)?);
    }
    let h = handle_opt(CMD, p, "handle")?
        .ok_or_else(|| bad(CMD, "give `points` to draw along, or the `handle` of a line, arc, 2D polyline or circle to convert"))?;
    let e = s.doc()?.entity(h).ok_or_else(|| bad(CMD, "the object does not exist"))?;
    let path = match &e.kind {
        EntityKind::Circle(c) => Some(in_plane(e, circle_path(c.center, c.radius))),
        EntityKind::Line(_) | EntityKind::Arc(_) | EntityKind::LwPolyline(_) => curve_of(e)?,
        _ => None,
    };
    let Some(Curve::Segs(path)) = path else {
        return Err(bad(CMD, format!("a {} can't be converted: select a line, arc, 2D polyline or circle", type_name(s, h))));
    };
    let up = normal_of(e);
    if is_locked(s, h) {
        return Err(locked_err());
    }
    let b = cadcraft_kernel::polysolid(&path, width, height, justify, up).map_err(kernel)?;
    let made = add_like(s, h, b)?;
    s.doc_mut()?.remove_entity(h);
    Ok(json!({ "handle": made.hex() }))
}

/// What POLYSOLID asks besides points.
#[derive(Clone, Copy, PartialEq)]
enum PsAsk {
    Height,
    Width,
    Justify,
    Object,
    /// Arc mode's Second point: the point on the arc, once given.
    Second(Option<Vec2>),
}

/// POLYSOLID: the start point (or Object, Height, Width, Justify), then next points with Arc,
/// Close and Undo; arcs run on tangent from the wall before them, or through a second point.
/// Points are in the construction plane, at the start point's height.
#[derive(Default)]
struct PolysolidM {
    verts: Vec<PolyVertex>,
    z: f64,
    arc: bool,
    ask: Option<PsAsk>,
    /// Left, Center or Right (None: Center).
    justify: Option<Justify>,
}

fn justify_name(j: Justify) -> &'static str {
    match j {
        Justify::Left => "Left",
        Justify::Center => "Center",
        Justify::Right => "Right",
    }
}

impl PolysolidM {
    fn justify(&self) -> Justify {
        self.justify.unwrap_or(Justify::Center)
    }
    /// The direction the path runs at its last point (+X before the second point).
    fn tangent(&self) -> Vec2 {
        let n = self.verts.len();
        let (Some(prev), Some(last)) = (n.checked_sub(2).and_then(|i| self.verts.get(i)), self.verts.last()) else { return Vec2::X };
        match bulge_to_arc(prev.p, last.p, prev.bulge) {
            Some((a, ccw)) => {
                let r = (last.p - a.center).perp().normalized();
                if ccw { r } else { -r }
            }
            None => (last.p - prev.p).normalized(),
        }
    }
    /// The bulge of the arc from the last point to `p` running on tangent.
    fn tangent_bulge(&self, p: Vec2) -> f64 {
        let Some(last) = self.verts.last() else { return 0.0 };
        let (dir, chord) = (self.tangent(), p - last.p);
        arc_to_bulge(2.0 * dir.cross(chord).atan2(dir.dot(chord)))
    }
    /// Adds the point `p`, reached by a line or an arc of `bulge` from the last point.
    fn push(&mut self, p: Vec2, bulge: f64) {
        if let Some(last) = self.verts.last_mut() {
            last.bulge = bulge;
        }
        self.verts.push(PolyVertex::new(p));
    }
    fn finish(&self, s: &mut Session, closed: bool) -> Result<Step> {
        if self.verts.len() < 2 {
            return Ok(Step::Done);
        }
        let points: Vec<Value> = self.verts.iter().map(|v| json!([v.p.x, v.p.y, self.z])).collect();
        let bulges: Vec<f64> = self.verts.iter().map(|v| v.bulge).collect();
        let j = justify_name(self.justify()).to_ascii_lowercase();
        run_json(s, "polysolid", json!({ "points": points, "bulges": bulges, "closed": closed, "justify": j }))
    }
}

/// A distance typed at a POLYSOLID option (Enter keeps `cur`).
fn size_answer(i: &Input, cur: f64) -> Result<Option<f64>> {
    match i {
        Input::Enter => Ok(Some(cur)),
        Input::Text(t) => {
            number(t).filter(|v| v.is_finite() && *v > 0.0).map(Some).ok_or_else(|| EngineError::Other("Requires a positive number.".into()))
        }
        _ => Ok(None),
    }
}

impl Interactive for PolysolidM {
    fn name(&self) -> &'static str {
        "POLYSOLID"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let (h, w) = (psol(s, "PSOLHEIGHT"), psol(s, "PSOLWIDTH"));
        s.echo(format!("Height = {h:.4}, Width = {w:.4}, Justification = {}", justify_name(self.justify())));
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let base = self.verts.last().map(|v| v.p);
        match self.ask {
            Some(PsAsk::Height) => return Prompt::new("Specify height", Accept::NUMBER).default(format!("{:.4}", psol(s, "PSOLHEIGHT"))),
            Some(PsAsk::Width) => return Prompt::new("Specify width", Accept::NUMBER).default(format!("{:.4}", psol(s, "PSOLWIDTH"))),
            Some(PsAsk::Justify) => {
                return Prompt::new("Enter justification", KW).kw(&["Left", "Center", "Right"]).default(justify_name(self.justify()));
            }
            Some(PsAsk::Object) => return Prompt::new("Select object", Accept::POINT),
            Some(PsAsk::Second(None)) => return Prompt::new("Specify second point on arc", Accept::POINT).base_opt(base),
            Some(PsAsk::Second(Some(_))) => return Prompt::new("Specify end point of arc", Accept::POINT).base_opt(base),
            None => {}
        }
        match (self.verts.len(), self.arc) {
            (0, _) => Prompt::new("Specify start point", Accept::POINT).kw(&["Object", "Height", "Width", "Justify"]).default("Object"),
            (n, false) => {
                let kw: &[&str] = if n >= 3 { &["Arc", "Close", "Undo"] } else { &["Arc", "Undo"] };
                Prompt::new("Specify next point", Accept::POINT).kw(kw).base_opt(base)
            }
            (n, true) => {
                let kw: &[&str] = if n >= 2 { &["Close", "Line", "Second point", "Undo"] } else { &["Line", "Second point", "Undo"] };
                Prompt::new("Specify endpoint of arc", Accept::POINT).kw(kw).base_opt(base)
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        let word = match &i {
            Input::Text(t) | Input::Keyword(t) => self.prompt(s).match_keyword(t),
            _ => None,
        };
        match self.ask {
            Some(a @ (PsAsk::Height | PsAsk::Width)) => {
                let var = if a == PsAsk::Height { "PSOLHEIGHT" } else { "PSOLWIDTH" };
                if let Some(v) = size_answer(&i, psol(s, var))? {
                    s.doc_mut()?.header.set_f64(var, v);
                    self.ask = None;
                }
                return Ok(Step::Continue);
            }
            Some(PsAsk::Justify) => {
                match (word.as_deref(), &i) {
                    (Some("Left"), _) => self.justify = Some(Justify::Left),
                    (Some("Center"), _) => self.justify = Some(Justify::Center),
                    (Some("Right"), _) => self.justify = Some(Justify::Right),
                    (_, Input::Enter) => {}
                    _ => return Ok(Step::Continue),
                }
                self.ask = None;
                return Ok(Step::Continue);
            }
            Some(PsAsk::Object) => {
                let h = match &i {
                    Input::Point(p) => pick_at(s, *p),
                    Input::Pick(hs) => hs.first().copied(),
                    _ => return Ok(Step::Continue),
                };
                let Some(h) = h else {
                    s.echo("No object found.");
                    return Ok(Step::Continue);
                };
                let j = justify_name(self.justify()).to_ascii_lowercase();
                return run_json(s, "polysolid", json!({ "handle": h.hex(), "justify": j }));
            }
            Some(PsAsk::Second(m)) => {
                let (Input::Point(p), Some(a)) = (&i, self.verts.last().map(|v| v.p)) else { return Ok(Step::Continue) };
                match m {
                    None => self.ask = Some(PsAsk::Second(Some(*p))),
                    Some(m) => {
                        let bulge = three_point_bulge(a, m, *p);
                        if bulge == 0.0 {
                            return Err(EngineError::Other("The three points of the arc are on one line.".into()));
                        }
                        self.push(*p, bulge);
                        self.ask = None;
                    }
                }
                return Ok(Step::Continue);
            }
            None => {}
        }
        match (&i, word.as_deref()) {
            (Input::Point(p), _) => {
                if self.verts.is_empty() {
                    self.z = s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0);
                    self.verts.push(PolyVertex::new(*p));
                } else if self.verts.last().is_some_and(|l| l.p.near(*p, EPS)) {
                    return Err(EngineError::Other("The points must be different.".into()));
                } else {
                    let b = if self.arc { self.tangent_bulge(*p) } else { 0.0 };
                    self.push(*p, b);
                }
                Ok(Step::Continue)
            }
            (Input::Enter, _) if self.verts.is_empty() => {
                self.ask = Some(PsAsk::Object);
                Ok(Step::Continue)
            }
            (Input::Enter, _) => self.finish(s, false),
            (_, Some(k @ ("Object" | "Height" | "Width" | "Justify"))) => {
                self.ask = Some(match k {
                    "Object" => PsAsk::Object,
                    "Height" => PsAsk::Height,
                    "Width" => PsAsk::Width,
                    _ => PsAsk::Justify,
                });
                Ok(Step::Continue)
            }
            (_, Some("Arc")) => {
                self.arc = true;
                Ok(Step::Continue)
            }
            (_, Some("Line")) => {
                self.arc = false;
                Ok(Step::Continue)
            }
            (_, Some("Second point")) => {
                self.ask = Some(PsAsk::Second(None));
                Ok(Step::Continue)
            }
            (_, Some("Close")) => {
                if self.arc
                    && let Some(first) = self.verts.first().map(|v| v.p)
                {
                    let b = self.tangent_bulge(first);
                    if let Some(last) = self.verts.last_mut() {
                        last.bulge = b;
                    }
                }
                self.finish(s, true)
            }
            (_, Some("Undo")) => {
                self.verts.pop();
                if let Some(l) = self.verts.last_mut() {
                    l.bulge = 0.0;
                }
                Ok(Step::Continue)
            }
            _ => Ok(Step::Continue),
        }
    }
    fn preview(&self, _s: &Session, c: Vec2) -> Vec<EntityKind> {
        let Some(last) = self.verts.last() else { return Vec::new() };
        let mut vs = self.verts.clone();
        let b = match self.ask {
            Some(PsAsk::Second(Some(m))) => three_point_bulge(last.p, m, c),
            None if self.arc => self.tangent_bulge(c),
            _ => 0.0,
        };
        if let Some(l) = vs.last_mut() {
            l.bulge = b;
        }
        vs.push(PolyVertex::new(c));
        vec![super::helpers::lwpoly(vs, false)]
    }
}

/// The bulge of the arc from `a` through `m` to `b` (0 when they are on one line).
fn three_point_bulge(a: Vec2, m: Vec2, b: Vec2) -> f64 {
    let (u, w) = (m - a, b - a);
    let turn = u.cross(w);
    if turn.is_nan() || turn.abs() <= 1e-12 * u.len() * w.len() {
        return 0.0;
    }
    // The angle at `m` between the chords to the ends is π less half the arc's sweep; the arc
    // runs counterclockwise when `m` is left of the chord from `a` to `b`.
    let (ma, mb) = (a - m, b - m);
    let sweep = TAU - 2.0 * ma.cross(mb).atan2(ma.dot(mb)).abs();
    arc_to_bulge(if turn > 0.0 { sweep } else { -sweep })
}

// ---------------- CONVTOSOLID, CONVTOSURFACE ----------------

/// A mesh-only solid's points in the world.
fn world_verts(x: &Solid3d) -> Vec<Vec3> {
    x.mesh.verts.iter().map(|p| x.world(*p)).collect()
}

/// The objects among the handles converted by `make` (`None`: not something it converts),
/// each replaced by its new shape with its layer and other properties. Objects on locked layers
/// are left alone; one the kernel refuses is skipped with its reason. Every shape is made before
/// the drawing changes.
fn convert(s: &mut Session, cmd: &str, p: &Value, what: &str, make: impl Fn(&Entity) -> Result<Option<Built>>) -> Result<Value> {
    let hs = once(targets(s, p)?);
    let (mut built, mut skipped, mut locked, mut why) = (Vec::new(), 0usize, 0usize, None);
    let d = s.doc()?;
    for h in &hs {
        let (Some(e), Some(space)) = (d.entity(*h), d.space_of(*h)) else { continue };
        if is_locked(s, *h) {
            locked += 1;
            continue;
        }
        match make(e) {
            // A solid or surface keeps its part name and material (the cut list's).
            Ok(Some(b)) => {
                let mut like = Entity::clone(e);
                reshape(&mut like, b);
                built.push((*h, space, e.common.clone(), like.kind));
            }
            Ok(None) => skipped += 1,
            Err(err) => {
                why.get_or_insert(err);
                skipped += 1;
            }
        }
    }
    if built.is_empty() {
        return Err(why.unwrap_or_else(|| if locked > 0 { locked_err() } else { bad(cmd, format!("select {what}")) }));
    }
    let d = s.doc_mut()?;
    let mut made = Vec::with_capacity(built.len());
    for (h, space, mut common, kind) in built {
        (common.thickness, common.extrusion) = (0.0, Vec3::Z);
        made.push(d.add(&space, common, kind)?.hex());
        d.remove_entity(h);
    }
    let message = match (&why, skipped) {
        (_, 0) => String::new(),
        (Some(e), n) => format!("{n} object(s) could not be converted: {e}"),
        (None, n) => format!("{n} object(s) could not be converted."),
    };
    Ok(json!({ "handles": made, "skipped": skipped, "locked": locked, "message": message }))
}

fn thickness(e: &Entity) -> Option<f64> {
    let t = e.common.thickness;
    (t.is_finite() && t.abs() > EPS).then_some(t)
}

fn shape(r: cadcraft_kernel::Result<Built>) -> Result<Option<Built>> {
    r.map(Some).map_err(kernel)
}

/// CONVTOSOLID of one object.
fn to_solid(e: &Entity) -> Result<Option<Built>> {
    match (&e.kind, thickness(e)) {
        (EntityKind::Solid3d(x), _) if x.brep.is_empty() && !x.mesh.open => {
            shape(cadcraft_kernel::mesh_shape(&world_verts(x), &x.mesh.tris, true, true))
        }
        (EntityKind::Solid3d(x), _) if x.mesh.open && !x.brep.is_empty() => shape(cadcraft_kernel::sew(&[placed(x)], true)),
        (EntityKind::PolyMesh(m), _) => {
            let m = m.mesh3();
            shape(cadcraft_kernel::mesh_shape(&m.verts, &m.tris, true, true))
        }
        // A wide polyline with thickness: a wall as wide and as high, round its centre line.
        (EntityKind::LwPolyline(pl), Some(t)) if pl.const_width > EPS => match curve_of(e)? {
            Some(Curve::Segs(path)) => shape(cadcraft_kernel::polysolid(&path, pl.const_width, t.abs(), Justify::Center, normal_of(e) * t.signum())),
            _ => Ok(None),
        },
        (EntityKind::LwPolyline(_) | EntityKind::Circle(_), Some(t)) => match profile(e, true)? {
            Some(prof) => shape(cadcraft_kernel::extrude(&prof, normal_of(e) * t)),
            None => Ok(None),
        },
        _ => Ok(None),
    }
}

fn run_convtosolid(s: &mut Session, p: &Value) -> Result<Value> {
    convert(s, "convtosolid", p, "closed polylines or circles with thickness, wide polylines with thickness, meshes or watertight surfaces", to_solid)
}

/// The flat surface inside closed loops (with the holes inside them), joined into one shape when
/// there are several.
fn loops_surface(loops: &[Loop]) -> Result<Option<Built>> {
    let mut faces = Vec::new();
    for (outer, holes) in nest(loops) {
        let Some(o) = loops.get(outer) else { continue };
        let holes: Vec<Curve> = holes.iter().filter_map(|i| loops.get(*i)).map(|l| l.curve.clone()).collect();
        faces.push(cadcraft_kernel::planar_surface(&o.curve, &holes).map_err(kernel)?);
    }
    match faces.len() {
        0 => Ok(None),
        1 => Ok(faces.pop()),
        _ => {
            let id = Mat4::IDENTITY;
            let at: Vec<cadcraft_kernel::Placed> = faces.iter().map(|f| cadcraft_kernel::Placed { brep: &f.brep, xform: &id }).collect();
            shape(cadcraft_kernel::sew(&at, false))
        }
    }
}

/// Corners without repeats: a triangle when two are one point.
fn corners(pts: &[Vec3]) -> Vec<Vec3> {
    let mut out: Vec<Vec3> = Vec::with_capacity(pts.len());
    for p in pts {
        if !out.iter().any(|q| (*q - *p).len() <= EPS * (1.0 + p.len())) {
            out.push(*p);
        }
    }
    out
}

/// The flat face inside a polygon's corners, or its two triangles when the four aren't flat.
fn face_surface(pts: &[Vec3]) -> Result<Option<Built>> {
    let c = corners(pts);
    if c.len() < 3 {
        return Ok(None);
    }
    let n = newell(&c).normalized();
    let o = c.first().copied().unwrap_or_default();
    let size = c.iter().map(|p| (*p - o).len()).fold(0.0, f64::max);
    if c.iter().all(|p| n.dot(*p - o).abs() <= 1e-9 * (1.0 + size)) {
        let segs = (0..c.len()).filter_map(|i| Some(Seg::Line { a: *c.get(i)?, b: *c.get((i + 1) % c.len())? })).collect();
        return shape(cadcraft_kernel::planar_surface(&Curve::Segs(segs), &[]));
    }
    shape(cadcraft_kernel::mesh_shape(&c, &[[0, 1, 2], [0, 2, 3]], false, false))
}

/// CONVTOSURFACE of one object.
fn to_surface(e: &Entity) -> Result<Option<Built>> {
    match (&e.kind, thickness(e)) {
        (EntityKind::Solid3d(x), _) if x.brep.is_empty() => shape(cadcraft_kernel::mesh_shape(&world_verts(x), &x.mesh.tris, false, true)),
        (EntityKind::Solid3d(x), _) if !x.mesh.open => shape(cadcraft_kernel::to_surface(placed(x))),
        (EntityKind::PolyMesh(m), _) => {
            let m = m.mesh3();
            shape(cadcraft_kernel::mesh_shape(&m.verts, &m.tris, false, true))
        }
        (EntityKind::Region(_), _) => {
            let loops: Vec<Loop> = closed_loops(e)?.into_iter().map(|(pts, curve)| Loop { owner: Handle(0), pts, curve }).collect();
            loops_surface(&loops)
        }
        // A 2D solid's corners go round as 1, 2, 4, 3.
        (EntityKind::Solid(sd) | EntityKind::Trace(sd), _) => {
            let pl = e.plane().unwrap_or(Mat4::IDENTITY);
            let at = |i: usize| sd.corners.get(i).map(|p| pl.apply(Vec3::new(p.x, p.y, 0.0)));
            face_surface(&[0, 1, 3, 2].into_iter().filter_map(at).collect::<Vec<_>>())
        }
        (EntityKind::Face3d(f), _) => face_surface(&f.corners),
        (EntityKind::LwPolyline(pl), Some(_)) if pl.const_width > EPS => Ok(None),
        // Thickness runs along the object's extrusion direction.
        (EntityKind::Line(_) | EntityKind::Arc(_) | EntityKind::Circle(_) | EntityKind::LwPolyline(_), Some(t)) => match curve_of(e)? {
            Some(c) => shape(cadcraft_kernel::tabulated_surface(&c, e.common.extrusion.normalized() * t)),
            None => Ok(None),
        },
        _ => Ok(None),
    }
}

/// A closed surface (a solid's faces, a closed mesh) comes back open all the same (the kernel
/// tells surfaces from solids by whether the B-rep holds a solid), as the drawing does.
fn run_convtosurface(s: &mut Session, p: &Value) -> Result<Value> {
    convert(s, "convtosurface", p, "3D solids, meshes, regions, 2D solids, 3D faces, or lines, arcs, circles or polylines with thickness", to_surface)
}

// ---------------- 3DPRINT ----------------

/// 3DPRINT: the closed solids among those given (or selected, or all of them) written to an STL
/// file, with their size for setting up the print. Surfaces are left out (they can't print).
// ponytail: no 3D Print Options dialog (scale, output dimensions): the STL is in drawing units.
fn run_3dprint(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "3dprint";
    crate::limits::files(CMD)?;
    let path = str_param(p, "path").map(str::trim).filter(|x| !x.is_empty()).ok_or_else(|| bad(CMD, "`path` is required"))?;
    // An empty list (as MCP clients send optional arrays) counts as none given.
    let given = ["handle", "handles"].iter().any(|k| p.get(k).is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty())));
    let mut hs = if given { targets(s, p)? } else { s.selection() };
    if !given && solids(s, &hs)?.is_empty() {
        hs = s.doc()?.model.iter().map(|e| e.handle).collect();
    }
    let (closed, open): (Vec<Solid3d>, Vec<Solid3d>) = solids(s, &hs)?.into_iter().map(|(_, x)| x).partition(|x| !x.mesh.open && !x.brep.is_empty());
    if closed.is_empty() {
        return Err(EngineError::Other("Select 3D solids to print (surfaces can't be printed).".into()));
    }
    cadcraft_kernel::export_stl(&closed.iter().map(placed).collect::<Vec<_>>(), Path::new(path)).map_err(kernel)?;
    let pts: Vec<Vec3> = closed.iter().flat_map(world_verts).filter(|p| p.is_finite()).collect();
    let first = pts.first().copied().unwrap_or_default();
    let (lo, hi) = pts.iter().fold((first, first), |(lo, hi), p| {
        (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
    });
    let (n, size) = (closed.len(), hi - lo);
    let mut message = format!("Wrote {n} solid(s) to {path} for 3D printing: {:.4} x {:.4} x {:.4} drawing units.", size.x, size.y, size.z);
    message.push_str("\nSet the units and scale in the printer's slicing software.");
    if !open.is_empty() {
        message.push_str(&format!("\n{} surface(s) left out: only closed solids can be printed.", open.len()));
    }
    Ok(json!({ "exported": n, "path": path, "size": json3(size), "surfaces": open.len(), "message": message }))
}

/// 3DPRINT at the command line: the solids (the pickfirst selection, if any), then the STL file.
#[derive(Default)]
struct PrintM {
    sel: SelectPhase,
}

impl Interactive for PrintM {
    fn name(&self) -> &'static str {
        "3DPRINT"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match (self.sel.done, self.sel.removing) {
            (true, _) => {
                Prompt::new("Enter name of STL file to write", Accept::TEXT).picker(crate::Picker::File { save: true, label: "STL", exts: &["stl"] })
            }
            (false, true) => self.sel.prompt(),
            (false, false) => Prompt::new("Select solids or watertight meshes", Accept::SELECT),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if !self.sel.done {
            return Ok(match self.sel.feed(s, &i)? {
                SelOutcome::Empty => Step::Done,
                SelOutcome::More | SelOutcome::Done(_) => Step::Continue,
            });
        }
        let path = match &i {
            Input::Text(t) => t.trim().trim_matches('"').to_string(),
            Input::Enter => return Ok(Step::Cancel),
            _ => return Ok(Step::Continue),
        };
        if path.is_empty() {
            return Ok(Step::Cancel);
        }
        run_json(s, "3dprint", json!({ "path": path, "handles": hexes(&self.sel.picked) }))
    }
}

// ---------------- closed planar loops ----------------

/// A closed planar curve of an object (`owner`): points round it and its kernel curve.
pub(super) struct Loop {
    pub owner: Handle,
    pub pts: Vec<Vec3>,
    pub curve: Curve,
}

/// Twice a closed polygon's area along its normal (Newell's method); zero when it has no area.
pub(super) fn newell(pts: &[Vec3]) -> Vec3 {
    let Some(o) = pts.first().copied() else { return Vec3::ZERO };
    pts.iter().zip(pts.iter().cycle().skip(1)).fold(Vec3::ZERO, |n, (a, b)| n + (*a - o).cross(*b - o))
}

/// The centre of the circle through three points and its normal, which `a`, `m`, `b` go round
/// counterclockwise; `None` when they are on one line.
fn circle3(a: Vec3, m: Vec3, b: Vec3) -> Option<(Vec3, Vec3)> {
    let (u, v) = (m - a, b - a);
    let n = u.cross(v);
    let nn = n.dot(n);
    if !(nn.is_finite() && nn > 1e-24 * u.dot(u) * v.dot(v)) {
        return None;
    }
    let o = a + (v.cross(n) * u.dot(u) + n.cross(u) * v.dot(v)) * (1.0 / (2.0 * nn));
    Some((o, n))
}

/// Points round a curve: its lines' ends, 16 points along each arc, 32 round a circle, a spline's
/// points.
fn sample(c: &Curve) -> Vec<Vec3> {
    let Curve::Segs(segs) = c else { return c.points() };
    let mut out = Vec::new();
    for s in segs {
        match *s {
            Seg::Line { a, .. } => out.push(a),
            Seg::Arc { a, mid, b } => {
                out.push(a);
                match circle3(a, mid, b) {
                    Some((o, n)) => {
                        let (u, w) = (a - o, b - o);
                        let sweep = n.normalized().dot(u.cross(w)).atan2(u.dot(w)).rem_euclid(TAU);
                        out.extend((1..16).map(|i| o + Mat4::rotation(n, sweep * f64::from(i) / 16.0).apply_vec(u)));
                    }
                    None => out.push(mid),
                }
            }
            Seg::Circle { center, radius, normal } => {
                let f = Mat4::ocs(normal);
                out.extend((0..32).map(|i| {
                    let t = TAU * f64::from(i) / 32.0;
                    center + f.apply_vec(Vec3::new(t.cos(), t.sin(), 0.0)) * radius
                }));
            }
        }
    }
    out
}

/// The closed curves of an object (a region's loops, any other closed curve whole) with points
/// round each; none for open curves and other objects.
pub(super) fn closed_loops(e: &Entity) -> Result<Vec<(Vec<Vec3>, Curve)>> {
    Ok(curves_of(e)?.into_iter().filter(Curve::is_closed).map(|c| (sample(&c), c)).collect())
}

/// Most loops [`nest`] looks for holes among (it compares every pair).
const MAX_NESTED: usize = 2_000;

/// The outer loops, each with the holes directly inside it (indices into `loops`). A loop inside
/// an odd number of others in its plane is a hole in the smallest of them; one inside an even
/// number (none, or an island in a hole) is an outer loop. Loops are taken not to cross.
/// ponytail: every pair is compared (beyond [`MAX_NESTED`] loops each is its own outer loop);
/// a bounding-box sweep if drawings select thousands of nested curves at once.
pub(super) fn nest(loops: &[Loop]) -> Vec<(usize, Vec<usize>)> {
    let n = loops.len();
    if n > MAX_NESTED {
        return (0..n).map(|i| (i, Vec::new())).collect();
    }
    // Each loop's Newell vector, a point on it, and its outline seen along its normal's largest
    // axis (a view that keeps what is inside it in its plane).
    let flat = |n: Vec3, p: Vec3| {
        if n.x.abs() >= n.y.abs() && n.x.abs() >= n.z.abs() {
            Vec2::new(p.y, p.z)
        } else if n.y.abs() >= n.z.abs() {
            Vec2::new(p.z, p.x)
        } else {
            Vec2::new(p.x, p.y)
        }
    };
    let info: Vec<(Vec3, Vec3, Vec<Vec2>)> = loops
        .iter()
        .map(|l| {
            let nv = newell(&l.pts);
            (nv, l.pts.first().copied().unwrap_or_default(), l.pts.iter().map(|p| flat(nv, *p)).collect())
        })
        .collect();
    let area = |i: usize| info.get(i).map_or(0.0, |x| x.0.len());
    let inside = |j: usize, i: usize| -> bool {
        let (Some((nj, pj, _)), Some((ni, pi, outline))) = (info.get(j), info.get(i)) else { return false };
        let (ai, aj) = (ni.len(), nj.len());
        if i == j || aj.partial_cmp(&ai) != Some(std::cmp::Ordering::Less) || ni.cross(*nj).len() > 1e-6 * ai * aj {
            return false;
        }
        let in_plane = (*pj - *pi).dot(*ni * (1.0 / ai)).abs() <= 1e-6 * (1.0 + pi.len() + ai.sqrt());
        in_plane && point_in_polygon(outline, flat(*ni, *pj))
    };
    let parents: Vec<Vec<usize>> = (0..n).map(|j| (0..n).filter(|i| inside(j, *i)).collect()).collect();
    let depth = |j: usize| parents.get(j).map_or(0, Vec::len);
    let mut out: Vec<(usize, Vec<usize>)> = (0..n).filter(|j| depth(*j) % 2 == 0).map(|j| (j, Vec::new())).collect();
    for j in (0..n).filter(|j| depth(*j) % 2 == 1) {
        let smallest = parents.get(j).and_then(|ps| ps.iter().copied().min_by(|a, b| area(*a).total_cmp(&area(*b))));
        if let Some(o) = smallest.and_then(|p| out.iter_mut().find(|(i, _)| *i == p)) {
            o.1.push(j);
        }
    }
    out
}

#[cfg(test)]
mod tests;
