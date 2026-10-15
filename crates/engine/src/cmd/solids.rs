//! 3D solids (CAD-rhino): BOX, CYLINDER, CONE, SPHERE, TORUS, EXTRUDE, REVOLVE, FILLETEDGE,
//! the solid side of UNION, SUBTRACT and INTERSECT, and STEP/STL exchange.
//!
//! Shapes come from [`cadcraft_kernel`]; the document keeps each as a [`Solid3d`]. A boolean's
//! result replaces the first solid (keeping its properties) and erases the others.

use std::path::Path;
use std::sync::Arc;

use cadcraft_doc::{EntityKind, Handle, Solid3d};
use cadcraft_geom::{Mat4, Polyline, Segment, Vec2, Vec3};
use cadcraft_kernel::{BoolOp, Built, Placed, Primitive, Seg};
use serde_json::{Map, Value, json};

use super::machines::{SelOutcome, SelectPhase, number};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

/// One prompt of a primitive: the JSON key it fills and its message.
#[derive(Clone, Copy)]
enum Q {
    Point(&'static str, &'static str),
    /// A distance: typed, or picked as the distance from the first point.
    Dist(&'static str, &'static str),
}

const BOX: &[Q] = &[Q::Point("corner1", "Specify first corner"), Q::Point("corner2", "Specify other corner"), Q::Dist("height", "Specify height")];
const CYLINDER: &[Q] =
    &[Q::Point("center", "Specify center point of base"), Q::Dist("radius", "Specify base radius"), Q::Dist("height", "Specify height")];
const CONE: &[Q] =
    &[Q::Point("center", "Specify center point of base"), Q::Dist("radius", "Specify base radius"), Q::Dist("height", "Specify height")];
const SPHERE: &[Q] = &[Q::Point("center", "Specify center point"), Q::Dist("radius", "Specify radius")];
const TORUS: &[Q] = &[Q::Point("center", "Specify center point"), Q::Dist("radius", "Specify radius"), Q::Dist("tube", "Specify tube radius")];

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("box", "Box", run_box)
            .menu(&["Draw", "Modeling", "Box"])
            .params("{corner1: [x,y,z?], corner2: [x,y,z?], height?} | {corner1, length, width, height}")
            .interactive(|_| Ok(Box::new(Ask::new("box", "BOX", BOX)))),
        CommandSpec::new("cylinder", "Cylinder", run_cylinder)
            .menu(&["Draw", "Modeling", "Cylinder"])
            .alias(&["cyl"])
            .params("{center: [x,y,z?], radius, height}")
            .interactive(|_| Ok(Box::new(Ask::new("cylinder", "CYLINDER", CYLINDER)))),
        CommandSpec::new("cone", "Cone", run_cone)
            .menu(&["Draw", "Modeling", "Cone"])
            .params("{center: [x,y,z?], radius, topRadius?, height}")
            .interactive(|_| Ok(Box::new(Ask::new("cone", "CONE", CONE)))),
        CommandSpec::new("sphere", "Sphere", run_sphere)
            .menu(&["Draw", "Modeling", "Sphere"])
            .params("{center: [x,y,z?], radius}")
            .interactive(|_| Ok(Box::new(Ask::new("sphere", "SPHERE", SPHERE)))),
        CommandSpec::new("torus", "Torus", run_torus)
            .menu(&["Draw", "Modeling", "Torus"])
            .alias(&["tor"])
            .params("{center: [x,y,z?], radius, tube}")
            .interactive(|_| Ok(Box::new(Ask::new("torus", "TORUS", TORUS)))),
        CommandSpec::new("extrude", "Extrude", run_extrude)
            .menu(&["Draw", "Modeling", "Extrude"])
            .alias(&["ext"])
            .params("{handles?, height} (closed polylines, circles, single-loop regions; the profiles are erased)")
            .interactive(|_| Ok(Box::new(ProfileM::new(ProfileOp::Extrude)))),
        CommandSpec::new("revolve", "Revolve", run_revolve)
            .menu(&["Draw", "Modeling", "Revolve"])
            .alias(&["rev"])
            .params("{handles?, axis: [[x,y],[x,y]], angle? (degrees, default 360)}")
            .interactive(|_| Ok(Box::new(ProfileM::new(ProfileOp::Revolve)))),
        CommandSpec::new("filletedge", "Fillet Edge", run_filletedge)
            .menu(&["Modify", "Solid Editing", "Fillet Edges"])
            .params("{handle | handles?, radius}: rounds every edge of each solid")
            .interactive(|_| Ok(Box::new(ProfileM::new(ProfileOp::Fillet)))),
        CommandSpec::new("stepout", "Export STEP", run_stepout)
            .menu(&["File", "Export", "STEP"])
            .params("{path, handles?} (3D solids; all of them when none are given)")
            .noundo(),
        CommandSpec::new("stlout", "Export STL", run_stlout)
            .menu(&["File", "Export", "STL"])
            .params("{path, handles?} (3D solids; all of them when none are given)")
            .noundo(),
        CommandSpec::new("stepin", "Import STEP", run_stepin).menu(&["File", "Import", "STEP"]).params("{path}"),
    ]
}

fn kernel(e: cadcraft_kernel::KernelError) -> EngineError {
    EngineError::Other(e.to_string())
}

fn solid(b: Built) -> EntityKind {
    EntityKind::Solid3d(Solid3d { brep: Arc::from(b.brep), mesh: Arc::new(b.mesh), xform: Mat4::IDENTITY })
}

fn add(s: &mut Session, b: Built) -> Result<Value> {
    let h = s.add_entity(solid(b))?;
    Ok(json!({ "handle": h.hex() }))
}

/// A point as `[x, y]` or `[x, y, z]` (z = 0 when left out).
fn p3(p: &Value, key: &str) -> Option<Vec3> {
    let v = p.get(key)?;
    let z = v.as_array().and_then(|a| a.get(2)).and_then(Value::as_f64).unwrap_or(0.0);
    let q = point_value(v)?.to3(z);
    q.is_finite().then_some(q)
}

fn p3_req(cmd: &str, p: &Value, key: &str) -> Result<Vec3> {
    p3(p, key).ok_or_else(|| bad(cmd, format!("`{key}` ([x, y] or [x, y, z]) is required")))
}

fn run_box(s: &mut Session, p: &Value) -> Result<Value> {
    let a = p3_req("box", p, "corner1")?;
    let b = match p3(p, "corner2") {
        Some(c) => match p.get("height").and_then(Value::as_f64) {
            Some(h) => Vec3::new(c.x, c.y, a.z + h),
            None if (c.z - a.z).abs() > 1e-12 => c,
            None => return Err(bad("box", "`height` is required when both corners are on one plane")),
        },
        None => a + Vec3::new(f64_req("box", p, "length")?, f64_req("box", p, "width")?, f64_req("box", p, "height")?),
    };
    add(s, cadcraft_kernel::primitive(&Primitive::Box { a, b }).map_err(kernel)?)
}

fn run_cylinder(s: &mut Session, p: &Value) -> Result<Value> {
    let base = p3_req("cylinder", p, "center")?;
    let (radius, height) = (f64_req("cylinder", p, "radius")?, f64_req("cylinder", p, "height")?);
    add(s, cadcraft_kernel::primitive(&Primitive::Cylinder { base, radius, height }).map_err(kernel)?)
}

fn run_cone(s: &mut Session, p: &Value) -> Result<Value> {
    let base = p3_req("cone", p, "center")?;
    let (base_radius, height) = (f64_req("cone", p, "radius")?, f64_req("cone", p, "height")?);
    let top_radius = f64_or(p, "topRadius", 0.0);
    add(s, cadcraft_kernel::primitive(&Primitive::Cone { base, base_radius, top_radius, height }).map_err(kernel)?)
}

fn run_sphere(s: &mut Session, p: &Value) -> Result<Value> {
    let center = p3_req("sphere", p, "center")?;
    add(s, cadcraft_kernel::primitive(&Primitive::Sphere { center, radius: f64_req("sphere", p, "radius")? }).map_err(kernel)?)
}

fn run_torus(s: &mut Session, p: &Value) -> Result<Value> {
    let center = p3_req("torus", p, "center")?;
    let (radius, tube) = (f64_req("torus", p, "radius")?, f64_req("torus", p, "tube")?);
    add(s, cadcraft_kernel::primitive(&Primitive::Torus { center, radius, tube }).map_err(kernel)?)
}

/// A closed planar curve as a kernel profile; `None` for other objects.
fn profile(k: &EntityKind) -> Option<Vec<Seg>> {
    let from_polyline = |pl: Polyline, z: f64| -> Vec<Seg> {
        pl.segments()
            .iter()
            .map(|s| match s {
                Segment::Line(l) => Seg::Line { a: l.a.to3(z), b: l.b.to3(z) },
                Segment::Arc { .. } => Seg::Arc { a: s.start().to3(z), mid: s.mid().to3(z), b: s.end().to3(z) },
            })
            .collect()
    };
    match k {
        EntityKind::Circle(c) => Some(vec![Seg::Circle { center: c.center, radius: c.radius, normal: Vec3::Z }]),
        EntityKind::LwPolyline(p) if p.closed && p.vertices.len() >= 2 => {
            Some(from_polyline(Polyline { vertices: p.vertices.clone(), closed: true }, p.elevation))
        }
        EntityKind::Region(r) => match r.loops.as_slice() {
            [l] => Some(from_polyline(Polyline { vertices: l.vertices.clone(), closed: true }, 0.0)),
            _ => None,
        },
        _ => None,
    }
}

/// Replace each profile among `hs` by the solid `make` builds from it (keeping its layer and
/// other properties). Objects that aren't closed planar curves are skipped.
fn from_profiles(s: &mut Session, cmd: &str, hs: &[Handle], make: impl Fn(&[Seg]) -> cadcraft_kernel::Result<Built>) -> Result<Value> {
    let mut made = Vec::new();
    let mut skipped = 0usize;
    for h in hs {
        let Some(e) = s.doc()?.entity(*h).cloned() else { continue };
        let Some(prof) = profile(&e.kind) else {
            skipped += 1;
            continue;
        };
        let b = make(&prof).map_err(kernel)?;
        let space = s.state()?.edit_space();
        let d = s.doc_mut()?;
        made.push(d.add(&space, e.common.clone(), solid(b))?.hex());
        d.remove_entity(*h);
    }
    if made.is_empty() {
        return Err(bad(cmd, "select closed polylines, circles or single-loop regions"));
    }
    Ok(json!({ "handles": made, "skipped": skipped }))
}

fn run_extrude(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let h = f64_req("extrude", p, "height")?;
    from_profiles(s, "extrude", &hs, |prof| cadcraft_kernel::extrude(prof, Vec3::new(0.0, 0.0, h)))
}

fn run_revolve(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let axis = points_param(p, "axis").filter(|a| a.len() == 2).ok_or_else(|| bad("revolve", "`axis` ([[x,y],[x,y]]) is required"))?;
    let (Some(a), Some(b)) = (axis.first().copied(), axis.get(1).copied()) else { return Err(bad("revolve", "`axis` needs two points")) };
    let angle = f64_or(p, "angle", 360.0).to_radians();
    from_profiles(s, "revolve", &hs, |prof| cadcraft_kernel::revolve(prof, a.to3(0.0), (b - a).to3(0.0), angle))
}

/// The 3D solids among `hs`, in order.
fn solids(s: &Session, hs: &[Handle]) -> Result<Vec<(Handle, Solid3d)>> {
    let d = s.doc()?;
    Ok(hs
        .iter()
        .filter_map(|h| match &d.entity(*h)?.kind {
            EntityKind::Solid3d(x) => Some((*h, x.clone())),
            _ => None,
        })
        .collect())
}

fn placed(x: &Solid3d) -> Placed<'_> {
    Placed { brep: &x.brep, xform: &x.xform }
}

fn run_filletedge(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let r = f64_req("filletedge", p, "radius")?;
    let xs = solids(s, &hs)?;
    if xs.is_empty() {
        return Err(bad("filletedge", "select 3D solids"));
    }
    for (h, x) in &xs {
        let b = cadcraft_kernel::fillet(placed(x), r).map_err(kernel)?;
        s.doc_mut()?.modify_entity(*h, |e| e.kind = solid(b))?;
    }
    Ok(json!({ "filleted": xs.len() }))
}

/// UNION and INTERSECT of the solids among `hs`; `None` when there are none (regions then).
pub(crate) fn fold(s: &mut Session, hs: &[Handle], op: BoolOp) -> Result<Option<Value>> {
    let xs = solids(s, hs)?;
    let Some(((h0, first), rest)) = xs.split_first() else { return Ok(None) };
    if rest.is_empty() {
        return Err(EngineError::Other("Select at least two 3D solids.".into()));
    }
    let mut acc = first.clone();
    for (_, x) in rest {
        acc = combine(&acc, x, op)?;
    }
    finish(s, *h0, acc, &xs.iter().map(|(h, _)| *h).collect::<Vec<_>>())
}

/// SUBTRACT on solids; `None` when `from` holds none (regions then).
pub(crate) fn subtract(s: &mut Session, from: &[Handle], remove: &[Handle]) -> Result<Option<Value>> {
    let (a, b) = (solids(s, from)?, solids(s, remove)?);
    let Some(((h0, first), rest)) = a.split_first() else { return Ok(None) };
    if b.iter().any(|(h, _)| a.iter().any(|(x, _)| x == h)) {
        return Err(EngineError::Other("A solid can't be subtracted from itself.".into()));
    }
    let mut acc = first.clone();
    for (_, x) in rest {
        acc = combine(&acc, x, BoolOp::Union)?;
    }
    for (_, x) in &b {
        acc = combine(&acc, x, BoolOp::Subtract)?;
    }
    let all: Vec<Handle> = a.iter().chain(&b).map(|(h, _)| *h).collect();
    finish(s, *h0, acc, &all)
}

fn combine(a: &Solid3d, b: &Solid3d, op: BoolOp) -> Result<Solid3d> {
    let r = cadcraft_kernel::boolean(op, placed(a), placed(b)).map_err(kernel)?;
    Ok(Solid3d { brep: Arc::from(r.brep), mesh: Arc::new(r.mesh), xform: Mat4::IDENTITY })
}

fn finish(s: &mut Session, keep: Handle, r: Solid3d, all: &[Handle]) -> Result<Option<Value>> {
    let d = s.doc_mut()?;
    for h in all.iter().filter(|h| **h != keep) {
        d.remove_entity(*h);
    }
    d.modify_entity(keep, |e| e.kind = EntityKind::Solid3d(r))?;
    Ok(Some(json!({ "handle": keep.hex(), "message": "" })))
}

/// The solids to export: `handles` (or the selection), else every solid in model space.
fn export_set(s: &Session, p: &Value) -> Result<Vec<Solid3d>> {
    let hs = targets(s, p)?;
    let hs = if hs.is_empty() { s.doc()?.model.iter().map(|e| e.handle).collect() } else { hs };
    let xs: Vec<Solid3d> = solids(s, &hs)?.into_iter().map(|(_, x)| x).collect();
    if xs.is_empty() { Err(EngineError::Other("There are no 3D solids to export.".into())) } else { Ok(xs) }
}

fn path_req<'a>(cmd: &str, p: &'a Value) -> Result<&'a str> {
    str_param(p, "path").filter(|x| !x.trim().is_empty()).ok_or_else(|| bad(cmd, "`path` is required"))
}

fn run_stepout(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_req("stepout", p)?;
    let xs = export_set(s, p)?;
    cadcraft_kernel::export_step(&xs.iter().map(placed).collect::<Vec<_>>(), Path::new(path)).map_err(kernel)?;
    Ok(json!({ "exported": xs.len(), "path": path }))
}

fn run_stlout(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_req("stlout", p)?;
    let xs = export_set(s, p)?;
    cadcraft_kernel::export_stl(&xs.iter().map(placed).collect::<Vec<_>>(), Path::new(path)).map_err(kernel)?;
    Ok(json!({ "exported": xs.len(), "path": path }))
}

fn run_stepin(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_req("stepin", p)?;
    add(s, cadcraft_kernel::import_step(Path::new(path)).map_err(kernel)?)
}

/// The prompts of a primitive, then its JSON form.
struct Ask {
    id: &'static str,
    name: &'static str,
    qs: &'static [Q],
    vals: Map<String, Value>,
    base: Option<Vec2>,
}

impl Ask {
    fn new(id: &'static str, name: &'static str, qs: &'static [Q]) -> Self {
        Ask { id, name, qs, vals: Map::new(), base: None }
    }
}

/// A typed distance.
fn distance(t: &str) -> Result<f64> {
    crate::units::parse_distance(t).or_else(|| number(t)).filter(|v| v.is_finite()).ok_or_else(|| EngineError::Other("Requires a distance.".into()))
}

impl Interactive for Ask {
    fn name(&self) -> &'static str {
        self.name
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.qs.get(self.vals.len()) {
            Some(Q::Point(_, m)) => Prompt::new(*m, Accept::POINT).base_opt(self.base),
            Some(Q::Dist(_, m)) => Prompt::new(*m, Accept::POINT_OR_NUMBER).base_opt(self.base),
            None => Prompt::new("", Accept::POINT),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let Some(q) = self.qs.get(self.vals.len()).copied() else { return Ok(Step::Done) };
        let (key, v) = match (q, i) {
            (_, Input::Cancel) => return Ok(Step::Cancel),
            (Q::Point(k, _), Input::Point(p)) => {
                self.base.get_or_insert(p);
                (k, json!([p.x, p.y]))
            }
            (Q::Dist(k, _), Input::Point(p)) => (k, json!(self.base.map_or(0.0, |b| b.dist(p)))),
            (Q::Dist(k, _), Input::Text(t)) => (k, json!(distance(&t)?)),
            _ => return Ok(Step::Continue),
        };
        self.vals.insert(key.into(), v);
        if self.vals.len() < self.qs.len() {
            return Ok(Step::Continue);
        }
        let spec = find_command(self.id).ok_or_else(|| EngineError::UnknownCommand(self.id.into()))?;
        (spec.run)(s, &Value::Object(self.vals.clone()))?;
        Ok(Step::Done)
    }
    fn preview(&self, _s: &Session, cursor: Vec2) -> Vec<EntityKind> {
        // The base outline as the cursor moves: BOX's rectangle, the others' base circle.
        let Some(b) = self.base else { return Vec::new() };
        match (self.id, self.vals.len()) {
            ("box", 1) => vec![super::helpers::lwpoly(super::helpers::rect_vertices(b, cursor), true)],
            (_, 1) if b.dist(cursor) > 1e-12 => vec![super::helpers::circle(b, b.dist(cursor))],
            _ => Vec::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProfileOp {
    Extrude,
    Revolve,
    Fillet,
}

/// EXTRUDE, REVOLVE and FILLETEDGE at the command line: select objects, then the height, the
/// axis and angle, or the radius.
struct ProfileM {
    op: ProfileOp,
    sel: SelectPhase,
    hs: Vec<Handle>,
    axis: Vec<Vec2>,
}

impl ProfileM {
    fn new(op: ProfileOp) -> Self {
        ProfileM { op, sel: SelectPhase::default(), hs: Vec::new(), axis: Vec::new() }
    }
    fn run(&self, s: &mut Session, extra: Value) -> Result<Step> {
        let mut p = json!({ "handles": self.hs.iter().map(|h| h.hex()).collect::<Vec<_>>() });
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        let id = match self.op {
            ProfileOp::Extrude => "extrude",
            ProfileOp::Revolve => "revolve",
            ProfileOp::Fillet => "filletedge",
        };
        let spec = find_command(id).ok_or_else(|| EngineError::UnknownCommand(id.into()))?;
        (spec.run)(s, &p)?;
        s.set_selection(Vec::new());
        Ok(Step::Done)
    }
}

impl Interactive for ProfileM {
    fn name(&self) -> &'static str {
        match self.op {
            ProfileOp::Extrude => "EXTRUDE",
            ProfileOp::Revolve => "REVOLVE",
            ProfileOp::Fillet => "FILLETEDGE",
        }
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
        match (self.op, self.axis.len()) {
            (ProfileOp::Extrude, _) => Prompt::new("Specify height of extrusion", Accept::NUMBER),
            (ProfileOp::Revolve, 0) => Prompt::new("Specify axis start point", Accept::POINT),
            (ProfileOp::Revolve, 1) => Prompt::new("Specify axis endpoint", Accept::POINT).base_opt(self.axis.first().copied()),
            (ProfileOp::Revolve, _) => Prompt::new("Specify angle of revolution", Accept::NUMBER).default("360"),
            (ProfileOp::Fillet, _) => Prompt::new("Specify fillet radius", Accept::NUMBER),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_empty() {
            return match self.sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Done),
                SelOutcome::Done(hs) => {
                    self.hs = hs;
                    Ok(Step::Continue)
                }
            };
        }
        match (self.op, i) {
            (ProfileOp::Revolve, Input::Point(p)) if self.axis.len() < 2 => {
                self.axis.push(p);
                Ok(Step::Continue)
            }
            (ProfileOp::Revolve, Input::Enter) if self.axis.len() == 2 => self.run(s, json!({ "axis": axis_json(&self.axis) })),
            (ProfileOp::Revolve, Input::Text(t)) if self.axis.len() == 2 => {
                let a = number(&t).ok_or_else(|| EngineError::Other("Requires an angle.".into()))?;
                self.run(s, json!({ "axis": axis_json(&self.axis), "angle": a }))
            }
            (ProfileOp::Extrude, Input::Text(t)) => self.run(s, json!({ "height": distance(&t)? })),
            (ProfileOp::Fillet, Input::Text(t)) => self.run(s, json!({ "radius": distance(&t)? })),
            _ => Ok(Step::Continue),
        }
    }
}

fn axis_json(a: &[Vec2]) -> Value {
    Value::Array(a.iter().map(|p| json!([p.x, p.y])).collect())
}

#[cfg(all(test, feature = "occt"))]
mod tests {
    use cadcraft_doc::Space;

    use super::*;

    fn solids_in(s: &Session) -> Vec<Solid3d> {
        s.doc()
            .unwrap()
            .model
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Solid3d(x) => Some(x.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn box_cylinder_and_subtract() {
        let mut s = Session::new();
        let b = s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [10, 10], "height": 10 })).unwrap();
        let c = s.execute("cylinder", &json!({ "center": [5, 5, -1], "radius": 3, "height": 12 })).unwrap();
        assert_eq!(solids_in(&s).len(), 2);
        s.execute("subtract", &json!({ "from": [b["handle"]], "remove": [c["handle"]] })).unwrap();
        let left = solids_in(&s);
        assert_eq!(left.len(), 1);
        assert!(left[0].mesh.tris.len() > 12, "the hole adds faces");
        s.undo().unwrap();
        assert_eq!(solids_in(&s).len(), 2);
    }

    #[test]
    fn moved_solids_union_where_they_are() {
        let mut s = Session::new();
        let a = s.execute("box", &json!({ "corner1": [0, 0, 0], "length": 2, "width": 2, "height": 2 })).unwrap();
        let b = s.execute("box", &json!({ "corner1": [10, 0, 0], "length": 2, "width": 2, "height": 2 })).unwrap();
        // Disjoint: their union is two lumps; moved onto the first it overlaps.
        s.execute("move", &json!({ "handles": [b["handle"]], "from": [10, 0], "to": [1, 1] })).unwrap();
        s.execute("union", &json!({ "handles": [a["handle"], b["handle"]] })).unwrap();
        let u = solids_in(&s);
        assert_eq!(u.len(), 1);
        let bb = u[0].bounds();
        assert!((bb.max.x - 3.0).abs() < 1e-6 && (bb.max.y - 3.0).abs() < 1e-6, "{bb:?}");
    }

    #[test]
    fn extrude_circle_replaces_the_profile() {
        let mut s = Session::new();
        s.cmdline("circle 0,0 5").unwrap();
        let h = s.doc().unwrap().model.iter().next().unwrap().handle;
        s.execute("extrude", &json!({ "handles": [h.hex()], "height": 8 })).unwrap();
        let xs = solids_in(&s);
        assert_eq!(xs.len(), 1);
        assert_eq!(s.doc().unwrap().model.iter().count(), 1, "the circle is gone");
    }

    #[test]
    fn iso_view_zooms_to_projected_extents() {
        let mut s = Session::new();
        s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [4, 4, 4] })).unwrap();
        s.execute("view.swiso", &json!({})).unwrap();
        assert!(!s.state().unwrap().view_dir.is_plan());
        let ext = s.view_extents(&Space::Model).unwrap();
        // The cube's iso silhouette is taller than its 4-unit footprint.
        assert!(ext.height() > 4.5, "{ext:?}");
        s.execute("plan", &json!({})).unwrap();
        assert!(s.state().unwrap().view_dir.is_plan());
    }

    #[test]
    fn bad_sizes_are_errors() {
        let mut s = Session::new();
        assert!(s.execute("sphere", &json!({ "center": [0, 0], "radius": -2 })).is_err());
        assert!(s.execute("box", &json!({ "corner1": [0, 0], "corner2": [1, 1] })).is_err());
        assert!(solids_in(&s).is_empty());
    }
}
