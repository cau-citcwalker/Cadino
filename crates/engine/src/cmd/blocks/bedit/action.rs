//! BACTION (AC) and BACTIONTOOL: dynamic block actions added in the Block Editor. An action
//! follows a parameter and says what changing it does to a selection of the block's objects (and
//! of its other parameters): move, scale, stretch inside a frame, polar stretch, array, rotate,
//! flip, or look up other values in a table. Points are the editor's.

use cadcraft_doc::Handle;
use cadcraft_doc::dynblock::{ActionKind, DynamicBlock, LookupColumn, ParameterKind, author};
use cadcraft_geom::{Vec2, Vec3};
use serde_json::{Map, Value, json};

use super::param::pick_parameter;
use super::{authored, authoring, authoring_enabled, parameter_named, to_block, to_editor};
use crate::cmd::curves::KW;
use crate::cmd::machines::{SelOutcome, SelectPhase};
use crate::cmd::{CommandSpec, bad, point_param, points_param, str_param, targets};
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub(super) fn specs() -> Vec<CommandSpec> {
    const PARAMS: &str = "{type: move|scale|stretch|polarstretch|array|rotate|flip|lookup (one of those the parameter takes), parameter: label or node, handles? (default the selection), parameters?: [labels] (parameters the action carries along), point?: start|end (which point of a two-point parameter a move or stretch follows; default end), frame? ([[x,y],[x,y]] corners or a polygon: stretch, polarstretch), rotate?: [handles] (polarstretch: objects only rotated), at? (where the editor shows it), multiplier?, angleOffset? deg, base? (scale, rotate: an independent base point), column?, row? (array), columns?: [{parameter, property?, lookup?: bool, unmatched?}], rows?: [[cells]] (lookup)} → {node, name}; points in the Block Editor's coordinates";
    vec![
        CommandSpec::new("baction", "Action", run_baction)
            .menu(&["Tools", "Block Editor", "Action"])
            .alias(&["ac"])
            .params(PARAMS)
            .enabled(authoring_enabled)
            .interactive(|_| Ok(Box::new(BactionM::default()))),
        CommandSpec::new("bactiontool", "Action Tool", run_baction)
            .params(PARAMS)
            .enabled(authoring_enabled)
            .interactive(|_| Ok(Box::new(BactionM { type_first: true, ..BactionM::default() }))),
    ]
}

/// The action types a parameter kind takes (JSON names).
fn types_for(k: &ParameterKind) -> &'static [&'static str] {
    match k {
        ParameterKind::Linear { .. } | ParameterKind::Xy { .. } => &["move", "scale", "stretch", "array"],
        ParameterKind::Polar { .. } => &["move", "scale", "stretch", "polarstretch", "array"],
        ParameterKind::Point { .. } => &["move", "stretch"],
        ParameterKind::Rotation { .. } => &["rotate"],
        ParameterKind::Flip { .. } => &["flip"],
        ParameterKind::Lookup { .. } => &["lookup"],
        _ => &[],
    }
}

/// A parameter kind's name in messages.
fn kind_name(k: &ParameterKind) -> &'static str {
    match k {
        ParameterKind::Linear { .. } => "linear",
        ParameterKind::Polar { .. } => "polar",
        ParameterKind::Xy { .. } => "XY",
        ParameterKind::Rotation { .. } => "rotation",
        ParameterKind::Alignment { .. } => "alignment",
        ParameterKind::Flip { .. } => "flip",
        ParameterKind::Point { .. } => "point",
        ParameterKind::Visibility { .. } => "visibility",
        ParameterKind::Lookup { .. } => "lookup",
        ParameterKind::BasePoint { .. } => "base point",
        ParameterKind::Other { .. } => "other",
    }
}

/// The lookup table property a parameter kind gives a column, when it can be in one.
fn lookup_property(k: &ParameterKind) -> Option<&'static str> {
    match k {
        ParameterKind::Linear { .. } => Some("UpdatedDistance"),
        ParameterKind::Visibility { .. } => Some("VisibilityState"),
        ParameterKind::Lookup { .. } => Some("lookupString"),
        _ => None,
    }
}

fn number(p: &Value, key: &str) -> Option<f64> {
    p.get(key).and_then(Value::as_f64).filter(|v| v.is_finite())
}

fn run_baction(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "baction";
    let (_, base) = authoring(s)?;
    let m = authored(s)?.ok_or_else(|| bad(cmd, "the block has no parameters: add one first (BPARAMETER)"))?;
    let node =
        p.get("parameter").and_then(|v| parameter_named(&m, v)).ok_or_else(|| bad(cmd, "`parameter` must name one of the block's parameters"))?;
    let param = m.parameters.iter().find(|x| x.node == node).ok_or_else(|| bad(cmd, "no such parameter"))?;
    let allowed = types_for(&param.kind);
    let ty = match str_param(p, "type").map(|t| t.trim().to_ascii_lowercase().replace([' ', '_'], "")) {
        Some(t) => t,
        None if allowed.len() == 1 => allowed.first().map(|t| (*t).to_string()).unwrap_or_default(),
        None => return Err(bad(cmd, "`type` is required")),
    };
    if !allowed.contains(&ty.as_str()) {
        let what = kind_name(&param.kind);
        return Err(bad(
            cmd,
            if allowed.is_empty() {
                format!("a {what} parameter takes no actions")
            } else {
                format!("a {what} parameter takes {}", allowed.join(", "))
            },
        ));
    }
    // The objects: the editor's, and the parameters carried along.
    let model_handles: Vec<Handle> = {
        let d = s.doc()?;
        let mut hs = if p.get("handles").is_some() || p.get("handle").is_some() || ty != "lookup" { targets(s, p)? } else { Vec::new() };
        hs.retain(|h| d.model.contains(*h));
        hs
    };
    let carried: Vec<Handle> = match p.get("parameters").and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .map(|v| parameter_named(&m, v).and_then(|n| m.parameters.iter().find(|x| x.node == n)).map(|x| x.handle))
            .collect::<Option<_>>()
            .ok_or_else(|| bad(cmd, "`parameters` must name the block's parameters"))?,
        None => Vec::new(),
    };
    if model_handles.is_empty() && carried.is_empty() && ty != "lookup" {
        return Err(bad(cmd, "an action needs objects: give `handles` or select them"));
    }
    let two = matches!(param.kind, ParameterKind::Linear { .. } | ParameterKind::Polar { .. } | ParameterKind::Xy { .. });
    let start = matches!(str_param(p, "point").map(|t| t.trim().to_ascii_lowercase()).as_deref(), Some("start" | "base" | "first"));
    let follow = |x: &str, y: &str| -> Vec<(i32, String)> {
        let pre = if !two {
            ""
        } else if start {
            "Base"
        } else {
            "End"
        };
        vec![(node, format!("{pre}{x}Delta")), (node, format!("{pre}{y}Delta"))]
    };
    let frame = || -> Result<(Vec<Vec2>, Vec<Handle>, Vec<(Handle, Vec<i32>)>)> {
        let f = points_param(p, "frame").filter(|f| f.len() >= 2).ok_or_else(|| bad(cmd, "`frame` (two corners or a polygon) is required"))?;
        let f: Vec<Vec2> = f.into_iter().take(10_000).collect();
        let (objects, vertices) = author::stretch_vertices(&s.doc()?.model, &model_handles, &f);
        Ok((f.iter().map(|q| *q + base).collect(), objects, vertices))
    };
    let mut selection = model_handles.clone();
    let (kind, connections) = match ty.as_str() {
        "move" => {
            let k = ActionKind::Move {
                multiplier: number(p, "multiplier").unwrap_or(1.0),
                angle_offset: number(p, "angleOffset").unwrap_or(0.0).to_radians(),
            };
            (k, follow("X", "Y"))
        }
        "stretch" => {
            let (frame, objects, vertices) = frame()?;
            selection = objects;
            let k = ActionKind::Stretch {
                frame,
                vertices,
                multiplier: number(p, "multiplier").unwrap_or(1.0),
                angle_offset: number(p, "angleOffset").unwrap_or(0.0).to_radians(),
            };
            (k, follow("X", "Y"))
        }
        "polarstretch" => {
            let (frame, objects, vertices) = frame()?;
            let rotate: Vec<Handle> = match p.get("rotate") {
                Some(v) => targets(s, &json!({ "handles": v }))?,
                None => Vec::new(),
            };
            selection = objects;
            for h in &rotate {
                if !selection.contains(h) {
                    selection.push(*h);
                }
            }
            (ActionKind::PolarStretch { frame, vertices, rotate }, follow("X", "Y"))
        }
        "scale" | "rotate" => {
            let b = point_param(p, "base").map(|q| to_block(base, q));
            let k = if ty == "scale" { ActionKind::Scale { base: b } } else { ActionKind::Rotate { base: b } };
            let what = if ty == "scale" { "UpdatedDistance" } else { "UpdatedAngle" };
            (k, vec![(node, what.to_string()), (node, "UpdatedBase".to_string())])
        }
        "flip" => (ActionKind::Flip, vec![(node, "UpdatedFlip".to_string()), (node, "UpdatedBase".to_string())]),
        "array" => {
            let column =
                number(p, "column").filter(|c| *c > 0.0).ok_or_else(|| bad(cmd, "`column` (the distance between columns, positive) is required"))?;
            let row = number(p, "row").unwrap_or(0.0);
            (ActionKind::Array { column, row }, vec![(node, "UpdatedDistance".to_string()), (node, "UpdatedBase".to_string())])
        }
        "lookup" => (lookup_table(cmd, &m, node, p)?, vec![(node, "lookupString".to_string())]),
        other => return Err(bad(cmd, format!("unknown action type `{other}`"))),
    };
    selection.extend(carried);
    let at = match point_param(p, "at") {
        Some(q) => to_block(base, q),
        None => default_at(&param.kind),
    };
    let added = super::author(s, |d, block| author::add_action(d, block, author::NewAction { kind, connections, selection, at }))?;
    s.echo(format!("Action {} added.", added.1));
    Ok(json!({ "node": added.0, "name": added.1 }))
}

/// Where an action shows when no location is given: beside its parameter's end point.
fn default_at(k: &ParameterKind) -> Vec3 {
    let (b, e) = k.points().unwrap_or_default();
    let u = e - b;
    let off = if u.len() > 1e-9 { u * (0.25 / u.len().max(1.0)) } else { Vec3::new(1.0, 1.0, 0.0) };
    e + Vec3::new(-off.y, off.x, 0.0) * 4.0 + off
}

/// A lookup action's table from JSON: columns naming parameters (the lookup parameter's column
/// is its lookup column), rows of cells.
fn lookup_table(cmd: &str, m: &DynamicBlock, node: i32, p: &Value) -> Result<ActionKind> {
    let cols = p.get("columns").and_then(Value::as_array).ok_or_else(|| bad(cmd, "`columns` is required for a lookup action"))?;
    let mut columns = Vec::new();
    for c in cols.iter().take(100) {
        let n = c
            .get("parameter")
            .and_then(|v| parameter_named(m, v))
            .ok_or_else(|| bad(cmd, "each column's `parameter` must name one of the block's parameters"))?;
        let k = &m.parameters.iter().find(|x| x.node == n).ok_or_else(|| bad(cmd, "no such parameter"))?.kind;
        let property = match str_param(c, "property") {
            Some(t) => t.to_string(),
            None => lookup_property(k).ok_or_else(|| bad(cmd, format!("a {} parameter can't be in a lookup table yet", kind_name(k))))?.to_string(),
        };
        let lookup = c.get("lookup").and_then(Value::as_bool).unwrap_or(n == node);
        let unmatched = str_param(c, "unmatched").unwrap_or("Custom").to_string();
        columns.push(LookupColumn { node: n, property, lookup, unmatched });
    }
    if !columns.iter().any(|c| c.lookup && c.node == node) || !columns.iter().any(|c| !c.lookup) {
        return Err(bad(cmd, "a lookup table needs an input column and the lookup parameter's column"));
    }
    let rows: Vec<Vec<String>> = match p.get("rows").and_then(Value::as_array) {
        Some(rs) => rs
            .iter()
            .take(10_000)
            .map(|r| {
                r.as_array()
                    .map(|a| a.iter().map(|c| c.as_str().map_or_else(|| c.to_string(), str::to_string)).collect::<Vec<_>>())
                    .filter(|r| r.len() == columns.len())
            })
            .collect::<Option<_>>()
            .ok_or_else(|| bad(cmd, "each row of `rows` must have a cell per column"))?,
        None => Vec::new(),
    };
    Ok(ActionKind::Lookup { columns, rows })
}

// ---------------- interactive ----------------

/// BACTION's prompts, after "Select parameter".
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Phase {
    #[default]
    Parameter,
    Type,
    Point,
    Corner,
    Opposite,
    Polygon,
    Objects,
    RotateOnly,
    Column,
    Row,
    Location,
    Multiplier,
    Offset,
    BaseType,
    BasePoint,
}

/// The action types' keywords: (keyword, JSON name).
const TYPE_KWS: &[(&str, &str)] = &[
    ("Array", "array"),
    ("Lookup", "lookup"),
    ("Flip", "flip"),
    ("Move", "move"),
    ("Rotate", "rotate"),
    ("Scale", "scale"),
    ("sTretch", "stretch"),
    ("Polar stretch", "polarstretch"),
];

/// BACTION: the parameter, the action type it takes, the point it follows, the frame, the
/// objects and the action's location, as AutoCAD prompts them; BACTIONTOOL asks for the type
/// first. The answers make the JSON form's parameters.
#[derive(Default)]
struct BactionM {
    type_first: bool,
    node: Option<i32>,
    ty: Option<&'static str>,
    phase: Phase,
    sel: SelectPhase,
    handles: Vec<Handle>,
    parameters: Vec<String>,
    corner: Option<Vec2>,
    polygon: Vec<Vec2>,
    p: Map<String, Value>,
}

impl BactionM {
    fn kind(&self, s: &Session) -> Option<ParameterKind> {
        let m = authored(s).ok()??;
        m.parameters.into_iter().find(|p| Some(p.node) == self.node).map(|p| p.kind)
    }
    fn type_keywords(&self, s: &Session) -> Vec<&'static str> {
        let allowed: Vec<&str> = self.kind(s).map(|k| types_for(&k).to_vec()).unwrap_or_else(|| TYPE_KWS.iter().map(|(_, t)| *t).collect());
        TYPE_KWS.iter().filter(|(_, t)| allowed.contains(t)).map(|(k, _)| *k).collect()
    }
    fn ty(&self) -> &'static str {
        self.ty.unwrap_or("move")
    }
    /// The next prompt once the parameter and the type are known.
    fn after_type(&mut self, s: &Session) {
        let two = matches!(self.kind(s), Some(ParameterKind::Linear { .. } | ParameterKind::Polar { .. } | ParameterKind::Xy { .. }));
        self.phase = match self.ty() {
            "move" | "stretch" | "polarstretch" if two => Phase::Point,
            "stretch" | "polarstretch" => Phase::Corner,
            "lookup" => Phase::Location,
            _ => Phase::Objects,
        };
        self.sel = SelectPhase::default();
    }
    fn after_point(&mut self) {
        self.phase = if matches!(self.ty(), "stretch" | "polarstretch") { Phase::Corner } else { Phase::Objects };
    }
    fn finish(&mut self, s: &mut Session) -> Result<Step> {
        let Some(node) = self.node else { return Ok(Step::Cancel) };
        self.p.insert("type".into(), json!(self.ty()));
        self.p.insert("parameter".into(), json!(node));
        self.p.insert("handles".into(), json!(self.handles.iter().map(|h| h.hex()).collect::<Vec<_>>()));
        if !self.parameters.is_empty() {
            self.p.insert("parameters".into(), json!(self.parameters));
        }
        if self.ty() == "lookup" {
            // ponytail: AutoCAD's Property Lookup Table dialog isn't here yet; the table is given
            // in the JSON form (`baction {type: lookup, columns, rows}`).
            return Err(EngineError::Other(
                "Lookup tables are given in the JSON form for now: baction {type: \"lookup\", parameter, columns, rows}.".into(),
            ));
        }
        run_baction(s, &Value::Object(std::mem::take(&mut self.p)))?;
        s.set_selection(Vec::new());
        Ok(Step::Done)
    }
    fn select_prompt(&self) -> Prompt {
        self.sel.prompt()
    }
    /// A selection answer: a parameter typed by its label joins the selection.
    fn select(&mut self, s: &mut Session, i: &Input) -> Result<Option<Vec<Handle>>> {
        if let Input::Text(t) | Input::Keyword(t) = i
            && let Some(m) = authored(s)?
            && let Some(n) = parameter_named(&m, &json!(t.trim()))
        {
            let name = super::parameter_name(&m, n);
            if !self.parameters.contains(&name) {
                self.parameters.push(name.clone());
            }
            s.echo(format!("Parameter {name} added to the selection."));
            return Ok(None);
        }
        match self.sel.feed(s, i)? {
            SelOutcome::More => Ok(None),
            SelOutcome::Done(hs) => Ok(Some(hs)),
            SelOutcome::Empty => Ok(Some(Vec::new())),
        }
    }
}

impl Interactive for BactionM {
    fn name(&self) -> &'static str {
        if self.type_first { "BACTIONTOOL" } else { "BACTION" }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if authored(s)?.is_none_or(|m| m.parameters.is_empty()) {
            return Err(EngineError::Other("The block has no parameters: add one first (BPARAMETER).".into()));
        }
        if self.type_first {
            self.phase = Phase::Type;
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let base = self.corner;
        match self.phase {
            Phase::Parameter => Prompt::new("Select parameter", Accept { point: true, number: false, text: true, select: false, enter: true }),
            Phase::Type => {
                let msg = if self.type_first { "Enter action type" } else { "Select action type" };
                Prompt::new(msg, KW).kw(&self.type_keywords(s))
            }
            Phase::Point => Prompt::new("Specify parameter point to associate with action or enter", Accept::POINT)
                .kw(&["sTart point", "Second point"])
                .default("Start"),
            Phase::Corner => Prompt::new("Specify first corner of stretch frame", Accept::POINT).kw(&["CPolygon"]),
            Phase::Opposite => Prompt::new("Specify opposite corner", Accept::POINT).corner(base.unwrap_or_default()),
            Phase::Polygon => Prompt::new("Specify endpoint of line", Accept::POINT).base_opt(self.polygon.last().copied()),
            Phase::Objects | Phase::RotateOnly => self.select_prompt(),
            Phase::Column => Prompt::new("Enter the distance between columns (|||)", Accept::POINT_OR_NUMBER),
            Phase::Row => Prompt::new("Enter the distance between rows or specify unit cell (---)", Accept::POINT_OR_NUMBER).default("0"),
            Phase::Location => match self.ty() {
                "move" | "stretch" | "polarstretch" => Prompt::new("Specify action location", Accept::POINT).kw(&["Multiplier", "Offset"]),
                "scale" | "rotate" => Prompt::new("Specify action location", Accept::POINT).kw(&["Base type"]),
                _ => Prompt::new("Specify action location", Accept::POINT),
            },
            Phase::Multiplier => Prompt::new("Enter distance multiplier", Accept::NUMBER).default("1.0000"),
            Phase::Offset => Prompt::new("Enter angle offset", Accept::NUMBER).default("0"),
            Phase::BaseType => Prompt::new("Enter base point type", KW).kw(&["Dependent", "Independent"]).default("Dependent"),
            Phase::BasePoint => Prompt::new("Specify base point location", Accept::POINT),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        let point = match &i {
            Input::Point(p) => Some(*p),
            Input::Deferred(d) => Some(d.at),
            _ => None,
        };
        let word = match &i {
            Input::Keyword(k) | Input::Text(k) => Some(k.trim().to_string()),
            _ => None,
        };
        let kw = |list: &[&str], w: &str| Prompt::new("", KW).kw(list).match_keyword(w);
        match self.phase {
            Phase::Parameter => {
                if matches!(i, Input::Enter) {
                    return Ok(Step::Cancel);
                }
                let Some(node) = pick_parameter(s, &i)? else {
                    s.echo("No parameter there.");
                    return Ok(Step::Continue);
                };
                self.node = Some(node);
                let kws = self.type_keywords(s);
                if let Some(t) = self.ty {
                    let fits = self.kind(s).is_some_and(|k| types_for(&k).contains(&t));
                    if !fits {
                        self.node = None;
                        return Err(EngineError::Other("That parameter doesn't take this action.".into()));
                    }
                    self.after_type(s);
                } else if kws.is_empty() {
                    self.node = None;
                    return Err(EngineError::Other("That parameter takes no actions.".into()));
                } else if kws.len() == 1 {
                    self.ty = TYPE_KWS.iter().find(|(k, _)| Some(k) == kws.first()).map(|(_, t)| *t);
                    self.after_type(s);
                } else {
                    self.phase = Phase::Type;
                }
            }
            Phase::Type => {
                let Some(w) = word else { return Ok(Step::Continue) };
                let k = kw(&self.type_keywords(s), &w).ok_or_else(|| EngineError::Other("Invalid option keyword.".into()))?;
                self.ty = TYPE_KWS.iter().find(|(kk, _)| *kk == k).map(|(_, t)| *t);
                if self.node.is_some() { self.after_type(s) } else { self.phase = Phase::Parameter }
            }
            Phase::Point => {
                let start = match (point, word.as_deref(), &i) {
                    (Some(q), _, _) => {
                        // The point of the parameter nearer the pick.
                        let (_, base) = authoring(s)?;
                        let (a, b) = self.kind(s).and_then(|k| k.points()).unwrap_or_default();
                        (to_editor(base, a) - q).len() <= (to_editor(base, b) - q).len()
                    }
                    (_, Some(w), _) => kw(&["sTart point", "Second point"], w).is_some_and(|k| k.starts_with('s')),
                    (_, _, Input::Enter) => true,
                    _ => return Ok(Step::Continue),
                };
                self.p.insert("point".into(), json!(if start { "start" } else { "end" }));
                self.after_point();
            }
            Phase::Corner => match (point, word.as_deref()) {
                (Some(q), _) => {
                    self.corner = Some(q);
                    self.phase = Phase::Opposite;
                }
                (None, Some(_)) => {
                    self.polygon.clear();
                    self.phase = Phase::Polygon;
                }
                _ => {}
            },
            Phase::Opposite => {
                let (Some(a), Some(b)) = (self.corner, point) else { return Err(EngineError::Other("Requires a point.".into())) };
                self.p.insert("frame".into(), json!([[a.x, a.y], [b.x, b.y]]));
                s.echo("Specify objects to stretch");
                self.phase = Phase::Objects;
            }
            Phase::Polygon => match (point, &i) {
                (Some(q), _) if self.polygon.len() < 10_000 => self.polygon.push(q),
                (_, Input::Enter) if self.polygon.len() >= 3 => {
                    self.p.insert("frame".into(), json!(self.polygon.iter().map(|q| [q.x, q.y]).collect::<Vec<_>>()));
                    s.echo("Specify objects to stretch");
                    self.phase = Phase::Objects;
                }
                (_, Input::Enter) => return Err(EngineError::Other("A polygon needs at least 3 points.".into())),
                _ => {}
            },
            Phase::Objects => {
                if let Some(hs) = self.select(s, &i)? {
                    self.handles = hs;
                    self.sel = SelectPhase::default();
                    self.phase = match self.ty() {
                        "polarstretch" => {
                            s.echo("Specify objects to rotate only");
                            Phase::RotateOnly
                        }
                        "array" => Phase::Column,
                        _ => Phase::Location,
                    };
                }
            }
            Phase::RotateOnly => {
                if let Some(hs) = self.select(s, &i)? {
                    self.p.insert("rotate".into(), json!(hs.iter().map(|h| h.hex()).collect::<Vec<_>>()));
                    self.phase = Phase::Location;
                }
            }
            Phase::Column | Phase::Row => {
                let from = self.kind(s).and_then(|k| k.points()).map(|(a, _)| a);
                let (_, base) = authoring(s)?;
                let v = match (point, word.as_deref(), &i) {
                    (Some(q), _, _) => from.map_or(0.0, |a| (q - to_editor(base, a)).len()),
                    (_, Some(w), _) => crate::cmd::machines::number(w).ok_or_else(|| EngineError::Other("Requires a distance.".into()))?,
                    (_, _, Input::Enter) if self.phase == Phase::Row => 0.0,
                    _ => return Ok(Step::Continue),
                };
                if self.phase == Phase::Column {
                    if v.is_nan() || v <= 0.0 {
                        return Err(EngineError::Other("The distance between columns must be more than 0.".into()));
                    }
                    self.p.insert("column".into(), json!(v));
                    self.phase = if matches!(self.kind(s), Some(ParameterKind::Xy { .. })) { Phase::Row } else { Phase::Location };
                } else {
                    self.p.insert("row".into(), json!(v));
                    self.phase = Phase::Location;
                }
            }
            Phase::Location => match (point, word.as_deref()) {
                (Some(q), _) => {
                    self.p.insert("at".into(), json!([q.x, q.y]));
                    return self.finish(s);
                }
                (None, Some(w)) => match kw(&["Multiplier", "Offset", "Base type"], w).as_deref() {
                    Some("Multiplier") => self.phase = Phase::Multiplier,
                    Some("Offset") => self.phase = Phase::Offset,
                    Some("Base type") => self.phase = Phase::BaseType,
                    _ => return Err(EngineError::Other("Point or option keyword required.".into())),
                },
                _ => {}
            },
            Phase::Multiplier | Phase::Offset => {
                let key = if self.phase == Phase::Multiplier { "multiplier" } else { "angleOffset" };
                if let Some(w) = word {
                    let v = crate::cmd::machines::number(&w).ok_or_else(|| EngineError::Other("Requires a number.".into()))?;
                    self.p.insert(key.into(), json!(v));
                }
                self.phase = Phase::Location;
            }
            Phase::BaseType => {
                let independent = word.as_deref().and_then(|w| kw(&["Dependent", "Independent"], w)).is_some_and(|k| k == "Independent");
                self.phase = if independent { Phase::BasePoint } else { Phase::Location };
                if !independent {
                    self.p.remove("base");
                }
            }
            Phase::BasePoint => {
                let Some(q) = point else { return Err(EngineError::Other("Requires a point.".into())) };
                self.p.insert("base".into(), json!([q.x, q.y]));
                self.phase = Phase::Location;
            }
        }
        Ok(Step::Continue)
    }
}
