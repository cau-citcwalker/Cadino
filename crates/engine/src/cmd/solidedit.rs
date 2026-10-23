//! SOLIDEDIT (CAD-rhino): AutoCAD's face, edge and body edits of a 3D solid; SHELL (its Body
//! Shell on its own) and IMPRINT. Faces and edges are picked in the views ([`super::subobj`]) or
//! named in JSON by index or by a point on them. Every edit replaces the solid in place, so it
//! keeps its layer, properties and furniture part.

use std::sync::Arc;

use cadcraft_doc::{Drawing, Entity, EntityKind, Handle, Solid3d};
use cadcraft_geom::{Mat4, Vec2, Vec3};
use cadcraft_kernel::{Built, Placed};
use serde_json::{Value, json};

use super::curves::{KW, is_locked, pick_at};
use super::solids::{curve_of, distance, kernel, p3, p3_value, per_solid, placed, reshape, solid, solids};
use super::subobj::{Picked, Picks, Sub, edge_entity, named, solid_of, target, topology};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

const CMD: &str = "solidedit";

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("solidedit", "Solid Editing", run_solidedit)
            .menu(&["Modify", "Solid Editing", "Solid Edit"])
            .params(
                "{handle, op, …}: op \"extrude\" {faces, height, taper? (degrees, + leans in)} | \"move\" {faces, delta: [dx,dy,dz]} | \
                 \"rotate\" {faces, axis: [[x,y,z],[x,y,z]], angle (degrees, counterclockwise looking from the second point toward the first)} | \
                 \"offset\" {faces, distance (+ grows the solid)} | \"taper\" {faces, base: [x,y,z], point2: [x,y,z] (along the axis of tapering), angle (degrees, + leans in)} | \
                 \"delete\" {faces} | \"copy\" {faces | edges, delta?} | \"imprint\" {objects: [handles], delete?} | \"separate\" | \
                 \"shell\" {thickness, faces? (removed)} | \"clean\" | \"check\". Faces and edges: indices, or points [x,y,z] on them",
            )
            .interactive(|_| Ok(Box::new(SolidEditM::new()))),
        CommandSpec::new("shell", "Shell", run_shell)
            .menu(&["Modify", "Solid Editing", "Shell"])
            .params(
                "{handle, thickness, faces: [index | [x,y,z]] (removed; [] for a closed hollow)} | {handle | handles?, thickness, removeTop?: bool (default true)}: hollows inward, open at its top faces",
            )
            .interactive(|_| Ok(Box::new(SolidEditM::shell()))),
        CommandSpec::new("imprint", "Imprint", run_imprint)
            .menu(&["Modify", "Solid Editing", "Imprint Edges"])
            .params("{handle (3D solid or surface), objects: [handles] (curves, solids, surfaces), delete?: bool (erase them; default false)}")
            .interactive(|_| Ok(Box::new(SolidEditM::imprint()))),
    ]
}

fn deg(cmd: &str, p: &Value, key: &str, default: Option<f64>) -> Result<f64> {
    match (p.get(key).filter(|v| !v.is_null()), default) {
        (None, Some(d)) => Ok(d.to_radians()),
        _ => Ok(f64_req(cmd, p, key)?.to_radians()),
    }
}

fn p3_key(p: &Value, key: &str) -> Result<Vec3> {
    p3(p, key).ok_or_else(|| bad(CMD, format!("`{key}` ([x, y, z]) is required")))
}

/// The edited shape in place of solid `h`.
fn replace(s: &mut Session, h: Handle, b: Built) -> Result<Value> {
    s.doc_mut()?.modify_entity(h, |e| reshape(e, b))?;
    Ok(json!({ "handle": h.hex() }))
}

/// A new object like `from` (its space, layer and properties).
fn add_like(s: &mut Session, from: Handle, kind: EntityKind) -> Result<Handle> {
    let d = s.doc_mut()?;
    let (Some(e), Some(space)) = (d.entity(from).map(|e| e.common.clone()), d.space_of(from)) else {
        return Err(EngineError::Other("no such object".into()));
    };
    Ok(d.add(&space, e, kind)?)
}

fn run_solidedit(s: &mut Session, p: &Value) -> Result<Value> {
    let op = str_param(p, "op").map(str::to_ascii_lowercase).ok_or_else(|| bad(CMD, "`op` is required (extrude, move, rotate, …)"))?;
    if op == "imprint" {
        return run_imprint(s, p);
    }
    let (h, x) = target(s, CMD, p)?;
    let at = placed(&x);
    let faces = || named(CMD, &x, p, "faces", Sub::Face);
    let b = match op.as_str() {
        "extrude" => cadcraft_kernel::extrude_faces(at, &faces()?, f64_req(CMD, p, "height")?, deg(CMD, p, "taper", Some(0.0))?),
        "move" => cadcraft_kernel::move_faces(at, &faces()?, p3_key(p, "delta")?),
        "rotate" => {
            let pts: Option<Vec<Vec3>> = p.get("axis").and_then(Value::as_array).map(|a| a.iter().filter_map(p3_value).collect());
            let Some([a, b]) = pts.as_deref() else { return Err(bad(CMD, "`axis` ([[x,y,z],[x,y,z]]) is required")) };
            cadcraft_kernel::rotate_faces(at, &faces()?, *a, *b - *a, deg(CMD, p, "angle", None)?)
        }
        "offset" => cadcraft_kernel::offset_faces(at, &faces()?, f64_req(CMD, p, "distance")?),
        "taper" => {
            let base = p3_key(p, "base")?;
            cadcraft_kernel::taper_faces(at, &faces()?, base, p3_key(p, "point2")? - base, deg(CMD, p, "angle", None)?)
        }
        "delete" => cadcraft_kernel::delete_faces(at, &faces()?),
        "shell" => cadcraft_kernel::shell_faces(at, f64_req(CMD, p, "thickness")?, &faces()?),
        "clean" => cadcraft_kernel::clean(at),
        "copy" => return copy(s, h, &x, p),
        "separate" => return separate(s, h, at),
        "check" => return check(at),
        "color" | "colour" | "material" => {
            return Err(EngineError::Other("Face and edge colors and materials aren't available yet: a solid has one color and material.".into()));
        }
        other => return Err(bad(CMD, format!("unknown `op` \"{other}\""))),
    }
    .map_err(kernel)?;
    replace(s, h, b)
}

/// Face Copy (surfaces) or Edge Copy (lines, arcs, circles, splines), moved by `delta`.
fn copy(s: &mut Session, h: Handle, x: &Solid3d, p: &Value) -> Result<Value> {
    let by = match p.get("delta").filter(|v| !v.is_null()) {
        Some(_) => p3_key(p, "delta")?,
        None => Vec3::ZERO,
    };
    let m = Mat4::translate(by);
    let mut made = Vec::new();
    if p.get("edges").is_some_and(|v| !v.is_null()) {
        let edges = named(CMD, x, p, "edges", Sub::Edge)?;
        let t = topology(x)?;
        let d = s.doc()?;
        let (Some(common), Some(space)) = (d.entity(h).map(|e| e.common.clone()), d.space_of(h)) else {
            return Err(EngineError::Other("no such object".into()));
        };
        let ents: Vec<Entity> = edges.iter().filter_map(|i| t.edges.get(*i as usize)).filter_map(|e| edge_entity(&e.curve, &common)).collect();
        for mut e in ents {
            e.transform3(&m);
            made.push(s.doc_mut()?.add(&space, e.common, e.kind)?);
        }
    } else {
        let faces = named(CMD, x, p, "faces", Sub::Face)?;
        for b in cadcraft_kernel::copy_faces(placed(x), &faces).map_err(kernel)? {
            let mut k = solid(b);
            if let EntityKind::Solid3d(sx) = &mut k {
                sx.xform = m;
            }
            made.push(add_like(s, h, k)?);
        }
    }
    Ok(json!({ "handle": h.hex(), "handles": made.iter().map(|h| h.hex()).collect::<Vec<_>>() }))
}

/// Body seParate solids: the first lump stays the solid, each other one becomes a solid like it.
fn separate(s: &mut Session, h: Handle, at: Placed) -> Result<Value> {
    let mut lumps = cadcraft_kernel::separate(at).map_err(kernel)?.into_iter();
    let Some(first) = lumps.next() else { return Err(EngineError::Other("The solid has no volume to separate.".into())) };
    let rest: Vec<Built> = lumps.collect();
    if rest.is_empty() {
        return Ok(json!({ "handle": h.hex(), "handles": [h.hex()], "message": "The solid is one piece: there is nothing to separate." }));
    }
    let mut made = vec![h];
    for b in rest {
        let d = s.doc_mut()?;
        let (Some(mut e), Some(space)) = (d.entity(h).map(|e| (**e).clone()), d.space_of(h)) else { break };
        reshape(&mut e, b);
        made.push(d.add(&space, e.common, e.kind)?);
    }
    s.doc_mut()?.modify_entity(h, |e| reshape(e, first))?;
    let n = made.len();
    Ok(json!({ "handle": h.hex(), "handles": made.iter().map(|h| h.hex()).collect::<Vec<_>>(), "message": format!("Separated into {n} solids.") }))
}

fn check(at: Placed) -> Result<Value> {
    let c = cadcraft_kernel::check(at).map_err(kernel)?;
    let message = if c.valid {
        "This object is a valid 3D solid.".to_string()
    } else {
        format!("This object is not a valid 3D solid: {}", c.problems.join("; "))
    };
    Ok(json!({ "valid": c.valid, "problems": c.problems, "message": message }))
}

fn run_shell(s: &mut Session, p: &Value) -> Result<Value> {
    let thickness = f64_req("shell", p, "thickness")?;
    if p.get("faces").is_some_and(|v| !v.is_null()) {
        let (h, x) = target(s, "shell", p)?;
        let faces = named("shell", &x, p, "faces", Sub::Face)?;
        let b = cadcraft_kernel::shell_faces(placed(&x), thickness, &faces).map_err(kernel)?;
        replace(s, h, b)?;
        return Ok(json!({ "shelled": 1, "handle": h.hex(), "locked": 0 }));
    }
    let hs = targets(s, p)?;
    let remove_top = bool_or(p, "removeTop", true);
    let (n, locked) = per_solid(s, "shell", &hs, |x| cadcraft_kernel::shell(x, thickness, remove_top))?;
    Ok(json!({ "shelled": n, "locked": locked }))
}

/// IMPRINT: curves lying on the solid's faces, and where solids and surfaces cut them, become
/// edges; the objects are erased with `delete`.
fn run_imprint(s: &mut Session, p: &Value) -> Result<Value> {
    let (h, x) = target(s, "imprint", p)?;
    let objs = match p.get("objects") {
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| super::curves::handle_value(v).ok_or_else(|| bad("imprint", format!("`objects`: {v} is not an object handle"))))
            .collect::<Result<Vec<_>>>()?,
        _ => return Err(bad("imprint", "`objects` (an array of handles) is required")),
    };
    let d = s.doc()?;
    let (mut tools, mut curves) = (Vec::new(), Vec::new());
    for o in &objs {
        if *o == h {
            return Err(bad("imprint", "a solid can't be imprinted on itself"));
        }
        match d.entity(*o) {
            Some(e) => match &e.kind {
                EntityKind::Solid3d(t) if !t.brep.is_empty() => tools.push(t.clone()),
                _ => match curve_of(e)? {
                    Some(c) => curves.push(c),
                    None => return Err(bad("imprint", format!("{} is not a curve, solid or surface", o.hex()))),
                },
            },
            None => return Err(bad("imprint", format!("there is no object {}", o.hex()))),
        }
    }
    let tp: Vec<Placed> = tools.iter().map(placed).collect();
    let b = cadcraft_kernel::imprint(placed(&x), &tp, &curves).map_err(kernel)?;
    replace(s, h, b)?;
    if bool_or(p, "delete", false) {
        for o in objs {
            if !is_locked(s, o) {
                s.doc_mut()?.remove_entity(o);
            }
        }
    }
    Ok(json!({ "handle": h.hex() }))
}

// ---------------- the prompts ----------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Level {
    Top,
    Face,
    Edge,
    Body,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Extrude,
    Move,
    Rotate,
    Offset,
    Taper,
    Delete,
    Copy,
    EdgeCopy,
    Imprint,
    Separate,
    Shell,
    Clean,
    Check,
}

enum Phase {
    /// The level's options.
    Menu,
    /// Faces or edges for the op.
    Pick(Box<Picks>),
    /// "Select a 3D solid".
    Solid,
    /// The op's points and values.
    Values,
    /// IMPRINT's objects, one at a time.
    ImprintObj,
    /// Whether to erase the object just picked.
    ImprintDelete(Handle),
}

/// SOLIDEDIT at the command line, as AutoCAD's: Face, Edge and Body options, each edit asked for
/// in turn (faces picked, then its points and values) and done at once, back at the options
/// until eXit; Undo takes back the last edit. SHELL and IMPRINT start at their Body option and
/// end after it.
/// ponytail: no Extrude Path, Rotate "Axis by object" or Reference, and no face or edge colors
/// and materials (a solid has one); they say so.
struct SolidEditM {
    level: Level,
    op: Option<Op>,
    phase: Phase,
    /// The faces or edges picked for the op, on `solid`.
    picked: Vec<u32>,
    solid: Option<Handle>,
    /// Points given, in construction-plane coordinates with their heights.
    pts: Vec<Vec3>,
    nums: Vec<f64>,
    /// A rotation axis by name (X, Y, Z, View), as a world direction.
    axis: Option<Vec3>,
    /// The drawing before each edit, for Undo.
    undo: Vec<Arc<Drawing>>,
    /// SHELL or IMPRINT on its own: done after one edit.
    once: bool,
}

impl SolidEditM {
    fn new() -> Self {
        SolidEditM {
            level: Level::Top,
            op: None,
            phase: Phase::Menu,
            picked: Vec::new(),
            solid: None,
            pts: Vec::new(),
            nums: Vec::new(),
            axis: None,
            undo: Vec::new(),
            once: false,
        }
    }
    fn shell() -> Self {
        SolidEditM { level: Level::Body, op: Some(Op::Shell), phase: Phase::Solid, once: true, ..SolidEditM::new() }
    }
    fn imprint() -> Self {
        SolidEditM { level: Level::Body, op: Some(Op::Imprint), phase: Phase::Solid, once: true, ..SolidEditM::new() }
    }
    /// Back at the level's options, ready for the next edit (or done, for SHELL and IMPRINT).
    fn menu(&mut self) -> Step {
        self.op = None;
        self.phase = Phase::Menu;
        self.picked.clear();
        self.solid = None;
        self.pts.clear();
        self.nums.clear();
        self.axis = None;
        if self.once { Step::Done } else { Step::Continue }
    }
    /// Run the edit as JSON, remembering the drawing for Undo; back at the options either way.
    fn run(&mut self, s: &mut Session, mut p: Value) -> Result<Step> {
        let Some(h) = self.solid else { return Ok(self.menu()) };
        p["handle"] = json!(h.hex());
        let before = s.state()?.doc.clone();
        s.echo("Solid validation started.");
        let r = run_body(s, CMD, &p);
        let step = self.menu();
        let r = r?;
        s.echo("Solid validation completed.");
        if let Some(m) = r.get("message").and_then(Value::as_str) {
            s.echo(m.to_string());
        }
        if !Arc::ptr_eq(&before, &s.state()?.doc) {
            self.undo.push(before);
        }
        Ok(step)
    }
    /// The op's JSON once its values are in; `None` while more are needed.
    fn ready(&self, s: &Session) -> Option<Value> {
        let c = s.cplane();
        let w = |p: &Vec3| {
            let q = c.apply(*p);
            json!([q.x, q.y, q.z])
        };
        let v = |p: Vec3| json!([p.x, p.y, p.z]);
        let faces = json!(self.picked);
        match (self.op?, self.pts.as_slice(), self.nums.as_slice()) {
            (Op::Extrude, _, [h, t]) => Some(json!({ "op": "extrude", "faces": faces, "height": h, "taper": t })),
            (Op::Offset, _, [d]) => Some(json!({ "op": "offset", "faces": faces, "distance": d })),
            (Op::Shell, _, [t]) => Some(json!({ "op": "shell", "faces": faces, "thickness": t })),
            (Op::Taper, [a, b], [t]) => Some(json!({ "op": "taper", "faces": faces, "base": w(a), "point2": w(b), "angle": t })),
            (Op::Rotate, [a, b], [t]) if self.axis.is_none() => Some(json!({ "op": "rotate", "faces": faces, "axis": [w(a), w(b)], "angle": t })),
            (Op::Rotate, [a], [t]) => {
                let o = c.apply(*a);
                Some(json!({ "op": "rotate", "faces": faces, "axis": [v(o), v(o + self.axis?)], "angle": t }))
            }
            // Two points: the move between them. Enter at the second (marked in `nums`): the first is the move.
            (Op::Move | Op::Copy | Op::EdgeCopy, [a, b], _) => Some(self.moved(c.apply_vec(*b - *a))),
            (Op::Move | Op::Copy | Op::EdgeCopy, [a], [_]) => Some(self.moved(c.apply_vec(*a))),
            _ => None,
        }
    }
    fn moved(&self, by: Vec3) -> Value {
        let by = json!([by.x, by.y, by.z]);
        match self.op {
            Some(Op::Move) => json!({ "op": "move", "faces": self.picked, "delta": by }),
            Some(Op::EdgeCopy) => json!({ "op": "copy", "edges": self.picked, "delta": by }),
            _ => json!({ "op": "copy", "faces": self.picked, "delta": by }),
        }
    }
    /// The op chosen at the level's options: what it asks for first.
    fn start(&mut self, op: Op) {
        self.op = Some(op);
        self.phase = match op {
            Op::Imprint | Op::Separate | Op::Shell | Op::Clean | Op::Check => Phase::Solid,
            Op::EdgeCopy => Phase::Pick(Box::new(Picks::new(Sub::Edge))),
            _ => Phase::Pick(Box::new(Picks::new(Sub::Face))),
        };
    }
    /// After the faces or edges: the values, or the edit at once.
    fn picked_all(&mut self, s: &mut Session, picks: Box<Picks>) -> Result<Step> {
        // SHELL may remove no faces: a closed hollow.
        if picks.picked.is_empty() && self.op != Some(Op::Shell) {
            return Ok(self.menu());
        }
        self.solid = picks.handle();
        self.picked = picks.picked;
        if self.op == Some(Op::Delete) {
            let faces = json!(self.picked);
            return self.run(s, json!({ "op": "delete", "faces": faces }));
        }
        self.phase = Phase::Values;
        Ok(Step::Continue)
    }
    fn values_prompt(&self) -> Prompt {
        let base = self.pts.first().map(|p| p.xy());
        match (self.op, self.pts.len(), self.nums.len()) {
            (Some(Op::Extrude), _, 0) => Prompt::new("Specify height of extrusion", Accept::NUMBER).kw(&["Path"]),
            (Some(Op::Extrude), _, _) => Prompt::new("Specify angle of taper for extrusion", Accept::NUMBER).default("0"),
            (Some(Op::Offset), ..) => Prompt::new("Specify the offset distance", Accept::NUMBER),
            (Some(Op::Shell), ..) => Prompt::new("Enter the shell offset distance", Accept::NUMBER),
            (Some(Op::Taper), 0, _) => Prompt::new("Specify the base point", Accept::POINT),
            (Some(Op::Taper), 1, _) => Prompt::new("Specify another point along the axis of tapering", Accept::POINT).base_opt(base),
            (Some(Op::Taper), ..) => Prompt::new("Specify the taper angle", Accept::NUMBER),
            (Some(Op::Rotate), 0, _) if self.axis.is_some() => Prompt::new("Specify the origin of the rotation", Accept::POINT).default("0,0,0"),
            (Some(Op::Rotate), 0, _) => {
                Prompt::new("Specify an axis point", Accept::POINT).kw(&["Axis by object", "View", "Xaxis", "Yaxis", "Zaxis"]).default("2points")
            }
            (Some(Op::Rotate), 1, _) if self.axis.is_none() => {
                Prompt::new("Specify the second point on the rotation axis", Accept::POINT).base_opt(base)
            }
            (Some(Op::Rotate), ..) => Prompt::new("Specify a rotation angle", Accept::NUMBER),
            (_, 0, _) => Prompt::new("Specify a base point or displacement", Accept::POINT),
            _ => Prompt::new("Specify a second point of displacement", Accept::POINT).base_opt(base),
        }
    }
    fn values_input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let prompt = self.values_prompt();
        let kw = match &i {
            Input::Text(t) | Input::Keyword(t) => prompt.match_keyword(t),
            _ => None,
        };
        match (kw.as_deref(), i) {
            (Some("Path"), _) => return Err(EngineError::Other("Extruding faces along a path isn't available yet.".into())),
            (Some("Axis by object"), _) => return Err(EngineError::Other("An axis by object isn't available yet: pick two points.".into())),
            (Some("View"), _) => self.axis = s.state().map(|st| st.screen_dir().dir).ok(),
            (Some(k @ ("Xaxis" | "Yaxis" | "Zaxis")), _) => {
                let local = match k {
                    "Xaxis" => Vec3::X,
                    "Yaxis" => Vec3::Y,
                    _ => Vec3::Z,
                };
                self.axis = Some(s.cplane().apply_vec(local));
            }
            (_, Input::Point(p)) if prompt.accept.point => {
                let q = p.to3(s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0));
                if self.pts.iter().any(|a| (*a - q).len() <= cadcraft_geom::EPS) && self.op != Some(Op::Rotate) {
                    return Err(EngineError::Other("The points must be different.".into()));
                }
                self.pts.push(q);
            }
            (_, Input::Text(t)) if prompt.accept.number => {
                let angle = matches!(self.op, Some(Op::Taper | Op::Rotate)) || self.nums.len() == 1;
                let v = if angle {
                    super::machines::number(&t).filter(|v| v.is_finite()).ok_or_else(|| EngineError::Other("Requires an angle.".into()))?
                } else {
                    distance(&t)?
                };
                self.nums.push(v);
            }
            (_, Input::Enter) => match (self.op, self.pts.len(), self.nums.len()) {
                // A default: the taper angle 0, the origin 0,0,0, two points for the axis.
                (Some(Op::Extrude), _, 1) => self.nums.push(0.0),
                (Some(Op::Rotate), 0, _) if self.axis.is_some() => self.pts.push(Vec3::ZERO),
                (Some(Op::Rotate), 0, _) => {}
                (Some(Op::Move | Op::Copy | Op::EdgeCopy), 1, _) => self.nums.push(0.0),
                _ => return Ok(Step::Continue),
            },
            _ => return Ok(Step::Continue),
        }
        match self.ready(s) {
            Some(p) => self.run(s, p),
            None => Ok(Step::Continue),
        }
    }
    fn menu_prompt(&self) -> Prompt {
        match self.level {
            Level::Top => Prompt::new("Enter a solids editing option", KW).kw(&["Face", "Edge", "Body", "Undo", "eXit"]).default("eXit"),
            Level::Face => Prompt::new("Enter a face editing option", KW)
                .kw(&["Extrude", "Move", "Rotate", "Offset", "Taper", "Delete", "Copy", "coLor", "mAterial", "Undo", "eXit"])
                .default("eXit"),
            Level::Edge => Prompt::new("Enter an edge editing option", KW).kw(&["Copy", "coLor", "Undo", "eXit"]).default("eXit"),
            Level::Body => Prompt::new("Enter a body editing option", KW)
                .kw(&["Imprint", "seParate solids", "Shell", "cLean", "Check", "Undo", "eXit"])
                .default("eXit"),
        }
    }
    fn menu_input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let kw = match &i {
            Input::Text(t) | Input::Keyword(t) => self.menu_prompt().match_keyword(t),
            Input::Enter => Some("eXit".into()),
            _ => return Ok(Step::Continue),
        };
        let Some(kw) = kw else { return Err(EngineError::Other("Invalid option keyword.".into())) };
        match (self.level, kw.as_str()) {
            (_, "Undo") => match self.undo.pop() {
                Some(d) => s.state_mut()?.doc = d,
                None => s.echo("Nothing to undo."),
            },
            (Level::Top, "eXit") => return Ok(Step::Done),
            (_, "eXit") => self.level = Level::Top,
            (Level::Top, "Face") => self.level = Level::Face,
            (Level::Top, "Edge") => self.level = Level::Edge,
            (Level::Top, "Body") => self.level = Level::Body,
            (Level::Top, _) => {}
            (_, "coLor" | "mAterial") => {
                return Err(EngineError::Other(
                    "Face and edge colors and materials aren't available yet: a solid has one color and material.".into(),
                ));
            }
            (Level::Face, k) => {
                let op = match k {
                    "Extrude" => Op::Extrude,
                    "Move" => Op::Move,
                    "Rotate" => Op::Rotate,
                    "Offset" => Op::Offset,
                    "Taper" => Op::Taper,
                    "Delete" => Op::Delete,
                    _ => Op::Copy,
                };
                self.start(op);
            }
            (Level::Edge, _) => self.start(Op::EdgeCopy),
            (Level::Body, k) => {
                let op = match k {
                    "Imprint" => Op::Imprint,
                    "seParate solids" => Op::Separate,
                    "Shell" => Op::Shell,
                    "cLean" => Op::Clean,
                    _ => Op::Check,
                };
                self.start(op);
            }
        }
        Ok(Step::Continue)
    }
    /// The solid an input picks at "Select a 3D solid".
    fn solid_at(s: &Session, i: &Input) -> Option<Handle> {
        let solid_h = |h: &Handle| s.doc().ok().and_then(|d| d.entity(*h)).is_some_and(|e| matches!(e.kind, EntityKind::Solid3d(_)));
        match i {
            Input::Pick(hs) => hs.iter().find(|h| solid_h(h)).copied(),
            // The solid in front where the click (or the point's height) is; a solid's outline in plan.
            Input::Point(p) => super::subobj::pick(s, i, Sub::Face, None).ok().flatten().map(|(h, ..)| h).or_else(|| pick_at(s, *p).filter(solid_h)),
            _ => None,
        }
    }
}

impl Interactive for SolidEditM {
    fn name(&self) -> &'static str {
        match (self.once, self.op) {
            (true, Some(Op::Shell)) => "SHELL",
            (true, _) => "IMPRINT",
            _ => "SOLIDEDIT",
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if !self.once {
            s.echo("Solids editing automatic checking:  SOLIDCHECK=1");
        }
        // A solid selected first is the one SHELL and IMPRINT edit.
        if self.once
            && let [h] = solids(s, &s.selection())?.iter().map(|(h, _)| *h).collect::<Vec<_>>().as_slice()
        {
            let h = *h;
            s.set_selection(Vec::new());
            return self.input(s, Input::Pick(vec![h]));
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match &self.phase {
            Phase::Menu => self.menu_prompt(),
            Phase::Pick(p) => p.prompt(),
            Phase::Solid if self.op == Some(Op::Imprint) => Prompt::new("Select a 3D solid or surface", Accept::POINT),
            Phase::Solid => Prompt::new("Select a 3D solid", Accept::POINT),
            Phase::Values => self.values_prompt(),
            Phase::ImprintObj => Prompt::new("Select an object to imprint", Accept::POINT),
            Phase::ImprintDelete(_) => Prompt::new("Delete the source object?", KW).kw(&["Yes", "No"]).default("N"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        match std::mem::replace(&mut self.phase, Phase::Menu) {
            Phase::Menu => self.menu_input(s, i),
            Phase::Pick(mut picks) => match picks.feed(s, CMD, &i) {
                Ok(Picked::More) => {
                    self.phase = Phase::Pick(picks);
                    Ok(Step::Continue)
                }
                Ok(Picked::Done) => self.picked_all(s, picks),
                Err(e) => {
                    self.phase = Phase::Pick(picks);
                    Err(e)
                }
            },
            Phase::Solid => {
                if matches!(i, Input::Enter) {
                    return Ok(self.menu());
                }
                let Some(h) = Self::solid_at(s, &i) else {
                    self.phase = Phase::Solid;
                    return Err(EngineError::Other("Select a 3D solid.".into()));
                };
                if let Err(e) = solid_of(s, CMD, h) {
                    self.phase = Phase::Solid;
                    return Err(e);
                }
                self.solid = Some(h);
                match self.op {
                    Some(Op::Shell) => {
                        self.phase = Phase::Pick(Box::new(Picks::removal(s, CMD, h)?));
                        Ok(Step::Continue)
                    }
                    Some(Op::Imprint) => {
                        self.phase = Phase::ImprintObj;
                        Ok(Step::Continue)
                    }
                    Some(Op::Separate) => self.run(s, json!({ "op": "separate" })),
                    Some(Op::Clean) => self.run(s, json!({ "op": "clean" })),
                    _ => self.run(s, json!({ "op": "check" })),
                }
            }
            // A bad value is asked again; a failed edit is back at the options.
            Phase::Values => {
                self.phase = Phase::Values;
                self.values_input(s, i)
            }
            Phase::ImprintObj => {
                let picked = match &i {
                    Input::Enter => return Ok(self.menu()),
                    Input::Pick(hs) => hs.iter().find(|h| Some(**h) != self.solid).copied(),
                    Input::Point(p) => pick_at(s, *p).filter(|h| Some(*h) != self.solid),
                    _ => None,
                };
                match picked {
                    Some(h) => self.phase = Phase::ImprintDelete(h),
                    None => self.phase = Phase::ImprintObj,
                }
                Ok(Step::Continue)
            }
            Phase::ImprintDelete(o) => {
                let erase = match &i {
                    Input::Enter => false,
                    Input::Text(t) | Input::Keyword(t) => match Prompt::new("", KW).kw(&["Yes", "No"]).match_keyword(t).as_deref() {
                        Some("Yes") => true,
                        Some("No") => false,
                        _ => {
                            self.phase = Phase::ImprintDelete(o);
                            return Err(EngineError::Other("Invalid option keyword.".into()));
                        }
                    },
                    _ => {
                        self.phase = Phase::ImprintDelete(o);
                        return Ok(Step::Continue);
                    }
                };
                let Some(h) = self.solid else { return Ok(self.menu()) };
                let before = s.state()?.doc.clone();
                let r = run_body(s, "imprint", &json!({ "handle": h.hex(), "objects": [o.hex()], "delete": erase }));
                // More objects for the same solid, until Enter.
                self.phase = Phase::ImprintObj;
                r?;
                if !Arc::ptr_eq(&before, &s.state()?.doc) {
                    self.undo.push(before);
                }
                Ok(Step::Continue)
            }
        }
    }
    fn preview(&self, _s: &Session, _cursor: Vec2) -> Vec<EntityKind> {
        match &self.phase {
            Phase::Pick(p) => p.preview(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_read_as_autocads() {
        let mut s = Session::new();
        s.start("solidedit").unwrap();
        assert!(s.log.iter().any(|l| l.contains("SOLIDCHECK=1")));
        assert_eq!(s.current_prompt().unwrap().display(), "Enter a solids editing option [Face/Edge/Body/Undo/eXit] <eXit>:");
        s.input(Input::Text("F".into())).unwrap();
        assert_eq!(
            s.current_prompt().unwrap().display(),
            "Enter a face editing option [Extrude/Move/Rotate/Offset/Taper/Delete/Copy/coLor/mAterial/Undo/eXit] <eXit>:"
        );
        s.input(Input::Text("e".into())).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Select faces or [Undo/Remove]:");
        // Nothing picked: back at the face options.
        s.input(Input::Enter).unwrap();
        let _ = s.input(Input::Text("L".into()));
        assert!(s.log.last().unwrap().contains("aren't available yet"), "{:?}", s.log);
        s.input(Input::Text("x".into())).unwrap();
        s.input(Input::Text("b".into())).unwrap();
        assert_eq!(
            s.current_prompt().unwrap().display(),
            "Enter a body editing option [Imprint/seParate solids/Shell/cLean/Check/Undo/eXit] <eXit>:"
        );
        s.input(Input::Text("p".into())).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Select a 3D solid:");
        s.input(Input::Enter).unwrap();
        s.input(Input::Text("x".into())).unwrap();
        s.input(Input::Text("E".into())).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Enter an edge editing option [Copy/coLor/Undo/eXit] <eXit>:");
        s.input(Input::Text("c".into())).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Select edges or [Undo/Remove]:");
        s.input(Input::Enter).unwrap();
        s.input(Input::Enter).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(s.running.is_none());
    }

    #[test]
    fn json_refusals() {
        let mut s = Session::new();
        let h = s.add_entity(EntityKind::Solid3d(Solid3d::default())).unwrap();
        let line = s.execute("line", &json!({ "points": [[0, 0], [1, 1]] })).unwrap()["handles"][0].clone();
        let err = |s: &mut Session, id: &str, p: Value| s.execute(id, &p).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err(&mut s, "solidedit", json!({ "handle": h.hex() })).contains("`op`"));
        assert!(err(&mut s, "solidedit", json!({ "handle": line, "op": "clean" })).contains("not a 3D solid"));
        assert!(err(&mut s, "solidedit", json!({ "handle": h.hex(), "op": "clean" })).contains("mesh only"));
        assert!(err(&mut s, "imprint", json!({ "handle": line, "objects": [] })).contains("not a 3D solid"));
        s.doc_mut().unwrap().layer_mut("0").unwrap().locked = true;
        let r = s.add_entity(EntityKind::Solid3d(Solid3d { brep: Arc::from("CASCADE"), ..Default::default() })).unwrap();
        assert!(err(&mut s, "solidedit", json!({ "handle": r.hex(), "op": "clean" })).contains("locked"));
    }
}

#[cfg(test)]
#[cfg(feature = "occt")]
mod occt_tests {
    use cadcraft_doc::Part;
    use cadcraft_geom::ViewDir;

    use super::*;

    fn panel(s: &mut Session, c1: [f64; 3], c2: [f64; 3]) -> Handle {
        let r = s.execute("box", &json!({ "corner1": c1, "corner2": c2 })).unwrap();
        let h = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
        s.doc_mut()
            .unwrap()
            .modify_entity(h, |e| {
                if let EntityKind::Solid3d(x) = &mut e.kind {
                    x.part = Part { name: "Side".into(), material: "Birch 18".into() }
                }
            })
            .unwrap();
        h
    }

    fn solid_x(s: &Session, h: Handle) -> Solid3d {
        let EntityKind::Solid3d(x) = &s.doc().unwrap().entity(h).unwrap().kind else { panic!() };
        x.clone()
    }

    fn zmax(x: &Solid3d) -> f64 {
        x.mesh.verts.iter().map(|p| x.world(*p).z).fold(f64::MIN, f64::max)
    }

    #[test]
    fn face_edits_by_json() {
        let mut s = Session::new();
        let h = panel(&mut s, [0.0, 0.0, 0.0], [18.0, 400.0, 700.0]);
        let top = json!([[9, 200, 700]]);
        s.execute("solidedit", &json!({ "handle": h.hex(), "op": "extrude", "faces": top, "height": 50 })).unwrap();
        assert!((zmax(&solid_x(&s, h)) - 750.0).abs() < 1e-6);
        assert_eq!(solid_x(&s, h).part.name, "Side");
        s.execute("solidedit", &json!({ "handle": h.hex(), "op": "move", "faces": [[9, 200, 750]], "delta": [0, 0, -100] })).unwrap();
        assert!((zmax(&solid_x(&s, h)) - 650.0).abs() < 1e-6);
        s.execute("solidedit", &json!({ "handle": h.hex(), "op": "offset", "faces": [[9, 200, 650]], "distance": 10 })).unwrap();
        assert!((zmax(&solid_x(&s, h)) - 660.0).abs() < 1e-6);
        // Copy a face up as a surface, an edge as a line.
        let r = s.execute("solidedit", &json!({ "handle": h.hex(), "op": "copy", "faces": [[9, 200, 660]], "delta": [0, 0, 100] })).unwrap();
        let c = Handle::parse_hex(r["handles"][0].as_str().unwrap()).unwrap();
        assert!(solid_x(&s, c).mesh.open && (zmax(&solid_x(&s, c)) - 760.0).abs() < 1e-6);
        let r = s.execute("solidedit", &json!({ "handle": h.hex(), "op": "copy", "edges": [[0, 200, 0]] })).unwrap();
        let l = Handle::parse_hex(r["handles"][0].as_str().unwrap()).unwrap();
        assert!(matches!(s.doc().unwrap().entity(l).unwrap().kind, EntityKind::Line(_)));
        let r = s.execute("solidedit", &json!({ "handle": h.hex(), "op": "check" })).unwrap();
        assert_eq!(r["valid"], true, "{r}");
        // A union of two apart, separated again.
        let b = panel(&mut s, [100.0, 0.0, 0.0], [118.0, 400.0, 700.0]);
        s.execute("union", &json!({ "handles": [h.hex(), b.hex()] })).unwrap();
        let r = s.execute("solidedit", &json!({ "handle": h.hex(), "op": "separate" })).unwrap();
        assert_eq!(r["handles"].as_array().unwrap().len(), 2, "{r}");
        // Shell with the top removed: a box with walls.
        s.execute("shell", &json!({ "handle": h.hex(), "faces": [[9, 200, 660]], "thickness": 2 })).unwrap();
        assert!(cadcraft_kernel::topology(placed(&solid_x(&s, h))).unwrap().faces.len() > 6);
    }

    /// SHELL at the prompts: the solid, the faces to remove (typed points on them), the wall.
    #[test]
    fn shell_prompts_remove_faces() {
        let mut s = Session::new();
        let h = panel(&mut s, [0.0, 0.0, 0.0], [600.0, 400.0, 300.0]);
        s.start("shell").unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Select a 3D solid:");
        s.input(Input::Pick(vec![h])).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Remove faces or [Undo/Add/ALL]:");
        s.point_z = Some(300.0);
        s.input(Input::Point(Vec2::new(300.0, 200.0))).unwrap();
        s.point_z = None;
        assert!(s.log.iter().any(|l| l == "1 face removed."), "{:?}", s.log);
        s.input(Input::Enter).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Enter the shell offset distance:");
        s.input(Input::Text("18".into())).unwrap();
        assert!(s.running.is_none(), "{:?}", s.log);
        let x = solid_x(&s, h);
        assert_eq!(cadcraft_kernel::topology(placed(&x)).unwrap().faces.len(), 11, "five faces out, five in, and the rim");
        assert_eq!(x.part.name, "Side");
    }

    /// SOLIDEDIT Face Extrude picked in the SW iso view, Undo inside the command, then IMPRINT a
    /// circle on the top face and PRESSPULL-free check of its new face.
    #[test]
    fn prompts_extrude_a_picked_face_and_undo() {
        let mut s = Session::new();
        let h = panel(&mut s, [0.0, 0.0, 0.0], [600.0, 400.0, 18.0]);
        s.start("solidedit").unwrap();
        s.input(Input::Text("f".into())).unwrap();
        s.input(Input::Text("e".into())).unwrap();
        let iso = ViewDir::named("iso").unwrap();
        s.view_at = Some((iso, iso.project(Vec3::new(300.0, 200.0, 18.0))));
        s.input(Input::Pick(vec![h])).unwrap();
        s.view_at = None;
        assert!(s.log.iter().any(|l| l == "1 face found."), "{:?}", s.log);
        assert!(!s.preview(Vec2::ZERO).is_empty(), "the face is outlined");
        s.input(Input::Enter).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Specify height of extrusion or [Path]:");
        s.input(Input::Text("30".into())).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Specify angle of taper for extrusion <0>:");
        s.input(Input::Enter).unwrap();
        assert!((zmax(&solid_x(&s, h)) - 48.0).abs() < 1e-6, "{:?}", s.log);
        assert!(s.current_prompt().unwrap().message.starts_with("Enter a face editing option"));
        s.input(Input::Text("u".into())).unwrap();
        assert!((zmax(&solid_x(&s, h)) - 18.0).abs() < 1e-6);
        s.input(Input::Text("x".into())).unwrap();
        s.input(Input::Text("x".into())).unwrap();
        assert!(s.running.is_none());

        // IMPRINT a circle drawn on the top.
        let c = s.add_entity(EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::new(300.0, 200.0, 18.0), radius: 50.0 })).unwrap();
        let before = cadcraft_kernel::topology(placed(&solid_x(&s, h))).unwrap().faces.len();
        s.start("imprint").unwrap();
        s.input(Input::Pick(vec![h])).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Select an object to imprint:");
        s.input(Input::Pick(vec![c])).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Delete the source object? [Yes/No] <N>:");
        s.input(Input::Text("y".into())).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(s.running.is_none(), "{:?}", s.log);
        assert!(s.doc().unwrap().entity(c).is_none(), "the circle is erased");
        assert_eq!(cadcraft_kernel::topology(placed(&solid_x(&s, h))).unwrap().faces.len(), before + 1);
        s.undo().unwrap();
        assert!(s.doc().unwrap().entity(c).is_some());
    }
}
