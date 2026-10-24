//! BPARAMETER (PARAM), BGRIPSET and the JSON forms that change or remove a parameter: dynamic
//! block parameters added in the Block Editor. A parameter is a property a reference to the block
//! can change (a distance, an angle, a position, a flip state, a visibility state, a lookup
//! value); its actions (BACTION) say what the change does. Points are the editor's (the block's
//! base point at the origin); [`cadcraft_doc::dynblock::author`] writes them in block coordinates.

use cadcraft_doc::dynblock::{ParameterKind, ValueSet, VisibilityState, author};
use cadcraft_geom::{Vec2, Vec3};
use serde_json::{Map, Value, json};

use super::{authored, authoring, authoring_enabled, parameter_at, parameter_name, parameter_named, to_block};
use crate::cmd::curves::{KW, pick_aperture};
use crate::cmd::{CommandSpec, bad, bool_or, point_param, str_param};
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("bparameter", "Parameter", run_bparameter)
            .menu(&["Tools", "Block Editor", "Parameter"])
            .alias(&["param"])
            .params(
                "{type: linear|point|polar|xy|rotation|alignment|flip|visibility|lookup|basepoint, base?/end? (two-point parameters; rotation: base, radius?, angle? deg, baseAngle? deg), at? (one-point parameters), label?, label2? (a polar parameter's angle, an XY parameter's vertical distance), description?, chain?: bool, palette?: bool, grips?: n, valueSet?/valueSet2?: {list?: [values], increment?, minimum?, maximum?} (angles in degrees), midpoint?: bool, labelAt? (flip), perpendicular?: bool (alignment), states?: [names] (visibility), name? (the element name)} → {node, label}; points in the Block Editor's coordinates",
            )
            .enabled(authoring_enabled)
            .interactive(|_| Ok(Box::new(BparamM::default()))),
        CommandSpec::new("bparameter.set", "Parameter Properties", run_set)
            .params("{parameter: label or node, label?, label2?, description?, chain?: bool, palette?: bool, valueSet?/valueSet2? (as BPARAMETER; null clears)} → {node, label}")
            .enabled(authoring_enabled),
        CommandSpec::new("bparameter.delete", "Delete Parameter or Action", run_delete)
            .params("{parameter? (label or node) | action? (name or node)} → {removed: [names]}; a parameter goes with its grips and actions")
            .enabled(authoring_enabled),
        CommandSpec::new("bgripset", "Grip Set", run_gripset)
            .menu(&["Tools", "Block Editor", "Grip Set"])
            .params("{parameter: label or node, grips: n} → {grips}")
            .enabled(authoring_enabled)
            .interactive(|_| Ok(Box::new(GripsetM::default()))),
    ]
}

/// The parameter types BPARAMETER offers, with their keyword and JSON name.
const TYPES: &[(&str, &str)] = &[
    ("Alignment", "alignment"),
    ("Base", "basepoint"),
    ("pOint", "point"),
    ("Linear", "linear"),
    ("Polar", "polar"),
    ("XY", "xy"),
    ("Rotation", "rotation"),
    ("Flip", "flip"),
    ("Visibility", "visibility"),
    ("looKup", "lookup"),
];

/// A value set from JSON: a list, an increment with optional limits (angles in degrees).
fn value_set(cmd: &str, v: Option<&Value>, angle: bool, current: f64) -> Result<ValueSet> {
    let Some(v) = v.filter(|v| !v.is_null()) else { return Ok(ValueSet::default()) };
    let conv = |x: f64| if angle { x.to_radians() } else { x };
    let num = |k: &str| -> Result<Option<f64>> {
        match v.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(x) => x.as_f64().filter(|x| x.is_finite()).map(|x| Some(conv(x))).ok_or_else(|| bad(cmd, format!("`{k}` must be a number"))),
        }
    };
    let mut set = ValueSet::default();
    if let Some(list) = v.get("list") {
        let a = list.as_array().ok_or_else(|| bad(cmd, "`list` must be an array of numbers"))?;
        let mut xs: Vec<f64> = a
            .iter()
            .take(10_000)
            .map(|x| x.as_f64().filter(|x| x.is_finite()).map(conv))
            .collect::<Option<_>>()
            .ok_or_else(|| bad(cmd, "`list` must be an array of numbers"))?;
        // The parameter's own value is always one of them, as AutoCAD adds it.
        xs.push(current);
        xs.sort_by(f64::total_cmp);
        xs.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        set.flags |= 8;
        set.list = xs;
    }
    if let Some(i) = num("increment")? {
        if i <= 0.0 {
            return Err(bad(cmd, "`increment` must be positive"));
        }
        set.flags |= 4;
        set.increment = i;
    }
    if let Some(x) = num("minimum")? {
        set.flags |= 1;
        set.minimum = x;
    }
    if let Some(x) = num("maximum")? {
        set.flags |= 2;
        set.maximum = x;
    }
    Ok(set)
}

fn text(p: &Value, key: &str) -> Option<String> {
    str_param(p, key).map(|t| t.trim().to_string())
}

fn run_bparameter(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "bparameter";
    let (_, base) = authoring(s)?;
    let m = authored(s)?;
    let ty = str_param(p, "type").map(|t| t.trim().to_ascii_lowercase()).ok_or_else(|| bad(cmd, "`type` is required"))?;
    let pt = |k: &str| point_param(p, k).map(|q| to_block(base, q));
    let req = |k: &str| pt(k).ok_or_else(|| bad(cmd, format!("`{k}` ([x, y]) is required")));
    let at = || pt("at").or_else(|| pt("base")).unwrap_or_else(|| to_block(base, Vec2::ZERO));
    let label =
        |dflt: &str, numbered: bool| text(p, "label").filter(|l| !l.is_empty()).unwrap_or_else(|| author::next_label(m.as_ref(), dflt, numbered));
    let label2 =
        |dflt: &str, numbered: bool| text(p, "label2").filter(|l| !l.is_empty()).unwrap_or_else(|| author::next_label(m.as_ref(), dflt, numbered));
    let description = text(p, "description").unwrap_or_default();
    let two = || -> Result<(Vec3, Vec3)> {
        let (b, e) = (req("base")?, req("end")?);
        if (e - b).len() < 1e-9 {
            return Err(bad(cmd, "`base` and `end` must differ"));
        }
        Ok((b, e))
    };
    let (kind, grips) = match ty.as_str() {
        "linear" => {
            let (base, end) = two()?;
            let values = value_set(cmd, p.get("valueSet"), false, (end - base).len())?;
            let from_midpoint = bool_or(p, "midpoint", false);
            (ParameterKind::Linear { base, end, label: label("Distance", true), description, values, from_midpoint }, 2)
        }
        "polar" | "xy" => {
            let (base, end) = two()?;
            let polar = ty == "polar";
            let (first, second) =
                if polar { ((end - base).len(), (end.xy() - base.xy()).angle().to_degrees()) } else { (end.x - base.x, end.y - base.y) };
            let values = value_set(cmd, p.get("valueSet"), false, first)?;
            let values2 = value_set(cmd, p.get("valueSet2"), polar, if polar { second.to_radians() } else { second })?;
            let description2 = text(p, "description2").unwrap_or_default();
            if polar {
                let (label, label2) = (label("Distance", true), label2("Angle", true));
                (ParameterKind::Polar { base, end, label, description, label2, description2, values, values2 }, 2)
            } else {
                let (label, label2) = (label("Horizontal", false), label2("Vertical", false));
                (ParameterKind::Xy { base, end, label, description, label2, description2, values, values2 }, 1)
            }
        }
        "rotation" => {
            let base = req("base")?;
            let base_angle = p.get("baseAngle").and_then(Value::as_f64).filter(|a| a.is_finite()).unwrap_or(0.0).to_radians();
            let end = match pt("end") {
                Some(e) => e,
                None => {
                    let r = p
                        .get("radius")
                        .and_then(Value::as_f64)
                        .filter(|r| r.is_finite() && *r > 0.0)
                        .ok_or_else(|| bad(cmd, "`radius` (positive) or `end` is required"))?;
                    let a = p.get("angle").and_then(Value::as_f64).filter(|a| a.is_finite()).unwrap_or(0.0).to_radians();
                    base + Vec2::polar(Vec2::ZERO, r, base_angle + a).to3(0.0)
                }
            };
            if (end - base).len() < 1e-9 {
                return Err(bad(cmd, "the radius must be more than 0"));
            }
            let values = value_set(cmd, p.get("valueSet"), true, (end.xy() - base.xy()).angle() - base_angle)?;
            (ParameterKind::Rotation { base, end, label: label("Angle", true), description, base_angle, values }, 1)
        }
        "alignment" => {
            let base = req("base")?;
            let end = pt("end").unwrap_or(base + Vec3::new(1.0, 0.0, 0.0));
            (ParameterKind::Alignment { base, end, perpendicular: bool_or(p, "perpendicular", true) }, 1)
        }
        "flip" => {
            let (base, end) = two()?;
            let label_at = pt("labelAt").unwrap_or((base + end) * 0.5);
            let states = ["Not Flipped".to_string(), "Flipped".to_string()];
            (ParameterKind::Flip { base, end, label: label("Flip state", true), description, label_at, states }, 1)
        }
        "point" => (ParameterKind::Point { at: at(), label: label("Position", true), description }, 1),
        "visibility" => {
            let names: Vec<String> = match p.get("states").and_then(Value::as_array) {
                Some(a) => a.iter().filter_map(Value::as_str).map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).take(1000).collect(),
                None => vec!["VisibilityState0".to_string()],
            };
            if names.is_empty() {
                return Err(bad(cmd, "`states` must name at least one visibility state"));
            }
            let states = names.into_iter().map(|name| VisibilityState { name, ..Default::default() }).collect();
            (ParameterKind::Visibility { at: at(), label: label("Visibility", true), description, controlled: Vec::new(), states }, 1)
        }
        "lookup" => (ParameterKind::Lookup { at: at(), label: label("Lookup", true), description }, 1),
        "basepoint" | "base" => (ParameterKind::BasePoint { at: at() }, 0),
        other => return Err(bad(cmd, format!("unknown parameter type `{other}`"))),
    };
    let grips = match p.get("grips") {
        None | Some(Value::Null) => grips,
        Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(cmd, "`grips` must be 0, 1, 2 or 4"))?,
    };
    let chain = bool_or(p, "chain", false);
    let palette = bool_or(p, "palette", true);
    let name = text(p, "name").filter(|n| !n.is_empty());
    let label = kind.label().map(str::to_string);
    let node = super::author(s, |d, block| {
        let node = author::add_parameter(d, block, kind, grips, chain)?;
        if !palette || name.is_some() {
            author::set_parameter(d, block, node, |p| {
                p.show_properties = palette;
                if let Some(n) = name {
                    p.element = n;
                }
                Ok(())
            })?;
        }
        Ok(node)
    })?;
    let shown = label.clone().unwrap_or_else(|| ty.clone());
    s.echo(format!("Parameter {shown} added."));
    Ok(json!({ "node": node, "label": label }))
}

/// `bparameter.set`: a parameter's properties, as the Properties palette changes them.
fn run_set(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "bparameter.set";
    let m = authored(s)?.ok_or_else(|| bad(cmd, "the block has no parameters"))?;
    let node =
        p.get("parameter").and_then(|v| parameter_named(&m, v)).ok_or_else(|| bad(cmd, "`parameter` must name one of the block's parameters"))?;
    let has = |k: &str| p.get(k).is_some();
    let set = |k: &str, angle: bool, current: f64| -> Result<Option<ValueSet>> {
        if has(k) { value_set(cmd, p.get(k), angle, current).map(Some) } else { Ok(None) }
    };
    let param = m.parameters.iter().find(|x| x.node == node).cloned().ok_or_else(|| bad(cmd, "no such parameter"))?;
    let (b, e) = param.kind.points().unwrap_or_default();
    let (v1, v2) = match &param.kind {
        ParameterKind::Polar { .. } => (set("valueSet", false, (e - b).len())?, set("valueSet2", true, (e.xy() - b.xy()).angle())?),
        ParameterKind::Xy { .. } => (set("valueSet", false, e.x - b.x)?, set("valueSet2", false, e.y - b.y)?),
        ParameterKind::Rotation { base_angle, .. } => (set("valueSet", true, (e.xy() - b.xy()).angle() - base_angle)?, None),
        _ => (set("valueSet", false, (e - b).len())?, None),
    };
    let (l1, l2, desc) = (text(p, "label").filter(|l| !l.is_empty()), text(p, "label2").filter(|l| !l.is_empty()), text(p, "description"));
    let (chain, palette) = (p.get("chain").and_then(Value::as_bool), p.get("palette").and_then(Value::as_bool));
    super::author(s, |d, block| {
        author::set_parameter(d, block, node, |x| {
            if let Some(c) = chain {
                x.chain = c;
            }
            if let Some(c) = palette {
                x.show_properties = c;
            }
            match &mut x.kind {
                ParameterKind::Linear { label, description, values, .. } | ParameterKind::Rotation { label, description, values, .. } => {
                    replace(label, l1, description, desc, values, v1);
                }
                ParameterKind::Polar { label, description, label2, values, values2, .. }
                | ParameterKind::Xy { label, description, label2, values, values2, .. } => {
                    replace(label, l1, description, desc, values, v1);
                    if let Some(l) = l2 {
                        *label2 = l;
                    }
                    if let Some(v) = v2 {
                        *values2 = v;
                    }
                }
                ParameterKind::Point { label, description, .. }
                | ParameterKind::Visibility { label, description, .. }
                | ParameterKind::Lookup { label, description, .. }
                | ParameterKind::Flip { label, description, .. } => {
                    replace(label, l1, description, desc, &mut ValueSet::default(), None);
                }
                _ => {}
            }
            Ok(())
        })
    })?;
    let label = authored(s)?.map(|m| parameter_name(&m, node));
    Ok(json!({ "node": node, "label": label }))
}

fn replace(label: &mut String, l: Option<String>, description: &mut String, d: Option<String>, values: &mut ValueSet, v: Option<ValueSet>) {
    if let Some(l) = l {
        *label = l;
    }
    if let Some(d) = d {
        *description = d;
    }
    if let Some(v) = v {
        *values = v;
    }
}

fn run_delete(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "bparameter.delete";
    let m = authored(s)?.ok_or_else(|| bad(cmd, "the block has no parameters or actions"))?;
    let node = match (p.get("parameter"), p.get("action")) {
        (Some(v), _) => parameter_named(&m, v).ok_or_else(|| bad(cmd, "`parameter` must name one of the block's parameters"))?,
        (None, Some(v)) => {
            let found = match v {
                Value::Number(n) => m.actions.iter().find(|a| n.as_i64() == Some(i64::from(a.node))),
                Value::String(t) => m.actions.iter().find(|a| a.name.eq_ignore_ascii_case(t.trim())),
                _ => None,
            };
            found.map(|a| a.node).ok_or_else(|| bad(cmd, "`action` must name one of the block's actions"))?
        }
        (None, None) => return Err(bad(cmd, "`parameter` or `action` is required")),
    };
    // A visibility parameter's objects are all shown again without it.
    let shown: Vec<cadcraft_doc::Handle> = m
        .parameters
        .iter()
        .find(|p| p.node == node)
        .and_then(|p| match &p.kind {
            ParameterKind::Visibility { controlled, .. } => Some(controlled.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let removed = super::author(s, |d, block| {
        let removed = author::remove(d, block, node)?;
        for h in &shown {
            if d.model.get(*h).is_some_and(|e| !e.common.visible) {
                let _ = d.modify_entity(*h, |e| e.common.visible = true);
            }
        }
        Ok(removed)
    })?;
    s.echo(format!("Removed {}.", removed.join(", ")));
    Ok(json!({ "removed": removed }))
}

fn run_gripset(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "bgripset";
    let m = authored(s)?.ok_or_else(|| bad(cmd, "the block has no parameters"))?;
    let node =
        p.get("parameter").and_then(|v| parameter_named(&m, v)).ok_or_else(|| bad(cmd, "`parameter` must name one of the block's parameters"))?;
    let n = p.get("grips").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(cmd, "`grips` (a count) is required"))?;
    let n = super::author(s, |d, block| author::set_grips(d, block, node, n))?;
    Ok(json!({ "grips": n }))
}

// ---------------- interactive ----------------

/// A question an option of BPARAMETER asks before the main prompts go on.
#[derive(Clone, Debug, PartialEq)]
enum Ask {
    Name,
    Label(&'static str, String),
    Description,
    Chain,
    Palette,
    BaseLocation,
    SetType(&'static str, bool),
    SetList(&'static str, bool),
    SetIncrement(&'static str, bool),
    SetMinimum(&'static str, bool),
    SetMaximum(&'static str, bool),
    BaseAngle,
}

/// BPARAMETER's main prompts.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Phase {
    #[default]
    Type,
    First,
    End,
    Radius,
    Angle,
    Direction,
    LabelAt,
    Grips,
}

/// BPARAMETER: the type, then its points and options, as AutoCAD prompts them; the answers make
/// the JSON form's parameters.
#[derive(Default)]
struct BparamM {
    ty: Option<&'static str>,
    phase: Phase,
    asks: Vec<Ask>,
    p: Map<String, Value>,
}

/// The type BPARAMETER offered last (its default next time).
static LAST_TYPE: std::sync::Mutex<&str> = std::sync::Mutex::new("linear");

impl BparamM {
    fn ty(&self) -> &'static str {
        self.ty.unwrap_or("linear")
    }
    fn two_point(&self) -> bool {
        matches!(self.ty(), "linear" | "polar" | "xy" | "flip")
    }
    /// The options the first prompt offers for the type.
    fn options(&self) -> &'static [&'static str] {
        match self.ty() {
            "linear" => &["Name", "Label", "Chain", "Description", "Base", "Palette", "Value set"],
            "polar" | "xy" | "rotation" => &["Name", "Label", "Chain", "Description", "Palette", "Value set"],
            "point" => &["Name", "Label", "Chain", "Description", "Palette"],
            "flip" | "visibility" | "lookup" => &["Name", "Label", "Description", "Palette"],
            _ => &["Name"],
        }
    }
    fn first_message(&self) -> &'static str {
        match self.ty() {
            "linear" | "polar" => "Specify start point",
            "xy" | "rotation" => "Specify base point",
            "alignment" => "Specify base point of alignment",
            "flip" => "Specify base point of reflection line",
            _ => "Specify parameter location",
        }
    }
    fn grip_choices(&self) -> (&'static [&'static str], &'static str) {
        match self.ty() {
            "linear" | "polar" => (&["0", "1", "2"], "2"),
            "xy" => (&["0", "1", "2", "4"], "1"),
            _ => (&["0", "1"], "1"),
        }
    }
    fn set(&mut self, k: &str, v: Value) {
        self.p.insert(k.to_string(), v);
    }
    fn set_field(&mut self, set: &'static str, k: &str, v: Value) {
        let e = self.p.entry(set.to_string()).or_insert_with(|| json!({}));
        if let Some(o) = e.as_object_mut() {
            o.insert(k.to_string(), v);
        }
    }
    /// Questions an option asks.
    fn option(&mut self, kw: &str) {
        let polar_or_xy = matches!(self.ty(), "polar" | "xy");
        self.asks = match kw {
            "Name" => vec![Ask::Name],
            "Label" => {
                let (first, second) = match self.ty() {
                    "polar" => ("distance", Some("angle")),
                    "xy" => ("horizontal distance", Some("vertical distance")),
                    "rotation" => ("angle", None),
                    "flip" => ("flip", None),
                    "point" => ("position", None),
                    "visibility" => ("visibility", None),
                    "lookup" => ("lookup", None),
                    _ => ("distance", None),
                };
                let mut v = vec![Ask::Label("label", first.into())];
                if let Some(s) = second {
                    v.push(Ask::Label("label2", s.into()));
                }
                v
            }
            "Description" => vec![Ask::Description],
            "Chain" => vec![Ask::Chain],
            "Palette" => vec![Ask::Palette],
            "Base" if self.ty() == "linear" => vec![Ask::BaseLocation],
            "Value set" => {
                let angle = matches!(self.ty(), "rotation");
                let mut v = vec![Ask::SetType("valueSet", angle)];
                if polar_or_xy {
                    v.push(Ask::SetType("valueSet2", self.ty() == "polar"));
                }
                v
            }
            _ => Vec::new(),
        };
    }
    fn finish(&mut self, s: &mut Session) -> Result<Step> {
        self.set("type", json!(self.ty()));
        if let Ok(mut last) = LAST_TYPE.lock() {
            *last = self.ty();
        }
        run_bparameter(s, &Value::Object(std::mem::take(&mut self.p)))?;
        Ok(Step::Done)
    }
    /// After the first point: the next prompt for the type.
    fn after_first(&mut self, s: &mut Session) -> Result<Step> {
        self.phase = match self.ty() {
            "linear" | "polar" | "xy" | "flip" => Phase::End,
            "rotation" => Phase::Radius,
            "alignment" => Phase::Direction,
            "point" => Phase::LabelAt,
            "visibility" | "lookup" => Phase::Grips,
            _ => return self.finish(s),
        };
        Ok(Step::Continue)
    }
    fn answer(&mut self, s: &mut Session, ask: Ask, i: &Input) -> Result<()> {
        let t = match i {
            Input::Text(t) | Input::Keyword(t) => t.trim().to_string(),
            Input::Enter => String::new(),
            _ => return Err(EngineError::Other("Requires an answer.".into())),
        };
        let yes = |t: &str, dflt: bool| if t.is_empty() { dflt } else { t.to_ascii_uppercase().starts_with('Y') };
        let num = |t: &str| crate::cmd::machines::number(t).ok_or_else(|| EngineError::Other("Requires a number.".into()));
        match ask {
            Ask::Name => self.set("name", json!(t)),
            Ask::Label(k, _) if !t.is_empty() => self.set(k, json!(t)),
            Ask::Description => self.set("description", json!(t)),
            Ask::Chain => self.set("chain", json!(yes(&t, false))),
            Ask::Palette => self.set("palette", json!(yes(&t, true))),
            Ask::BaseLocation => self.set("midpoint", json!(t.to_ascii_uppercase().starts_with('M'))),
            Ask::SetType(k, angle) => match t.to_ascii_uppercase().chars().next() {
                Some('L') => self.asks.insert(0, Ask::SetList(k, angle)),
                Some('I') => {
                    self.asks.splice(0..0, [Ask::SetIncrement(k, angle), Ask::SetMinimum(k, angle), Ask::SetMaximum(k, angle)]);
                }
                _ => {
                    self.p.remove(k);
                }
            },
            Ask::SetList(k, angle) => {
                let parse = |x: &str| if angle { crate::units::parse_angle(x).map(f64::to_degrees) } else { crate::cmd::machines::number(x) };
                let xs: Vec<f64> = t
                    .split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(parse)
                    .collect::<Option<_>>()
                    .ok_or_else(|| EngineError::Other("Enter numbers separated by commas.".into()))?;
                self.set_field(k, "list", json!(xs));
            }
            Ask::SetIncrement(k, _) if !t.is_empty() => self.set_field(k, "increment", json!(num(&t)?)),
            Ask::SetMinimum(k, _) if !t.is_empty() => self.set_field(k, "minimum", json!(num(&t)?)),
            Ask::SetMaximum(k, _) if !t.is_empty() => self.set_field(k, "maximum", json!(num(&t)?)),
            Ask::BaseAngle => {
                let a = if t.is_empty() {
                    0.0
                } else {
                    crate::units::parse_angle(&t).ok_or_else(|| EngineError::Other("Requires an angle.".into()))?.to_degrees()
                };
                self.set("baseAngle", json!(a));
            }
            _ => {}
        }
        let _ = s;
        Ok(())
    }
}

fn point_json(p: Vec2) -> Value {
    json!([p.x, p.y])
}

impl Interactive for BparamM {
    fn name(&self) -> &'static str {
        "BPARAMETER"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if let Some(a) = self.asks.first() {
            return match a {
                Ask::Name => Prompt::new("Enter parameter name", Accept::TEXT),
                Ask::Label(_, what) => Prompt::new(format!("Enter {what} property label"), Accept::TEXT),
                Ask::Description => Prompt::new("Enter property description", Accept::TEXT),
                Ask::Chain => {
                    Prompt::new("Evaluate associated actions when parameter is edited by another action?", KW).kw(&["Yes", "No"]).default("No")
                }
                Ask::Palette => Prompt::new("Display property in Properties palette?", KW).kw(&["Yes", "No"]).default("Yes"),
                Ask::BaseLocation => Prompt::new("Enter base point location", KW).kw(&["Startpoint", "Midpoint"]).default("Startpoint"),
                Ask::SetType(k, angle) => {
                    let what = if *angle {
                        "angle"
                    } else if *k == "valueSet2" {
                        "vertical distance"
                    } else {
                        "distance"
                    };
                    Prompt::new(format!("Enter {what} value set type"), KW).kw(&["None", "List", "Increment"]).default("None")
                }
                Ask::SetList(_, angle) => {
                    Prompt::new(format!("Enter list of {} values (separated by commas)", if *angle { "angle" } else { "distance" }), Accept::TEXT)
                }
                Ask::SetIncrement(_, angle) => Prompt::new(format!("Enter {} increment", if *angle { "angle" } else { "distance" }), Accept::NUMBER),
                Ask::SetMinimum(_, angle) => Prompt::new(format!("Enter minimum {}", if *angle { "angle" } else { "distance" }), Accept::NUMBER),
                Ask::SetMaximum(_, angle) => Prompt::new(format!("Enter maximum {}", if *angle { "angle" } else { "distance" }), Accept::NUMBER),
                Ask::BaseAngle => Prompt::new("Specify base angle", Accept::POINT_OR_NUMBER).default("0"),
            };
        }
        let base = point_param(&Value::Object(self.p.clone()), "base");
        match self.phase {
            Phase::Type => {
                let kws: Vec<&str> = TYPES.iter().map(|(k, _)| *k).collect();
                let last = LAST_TYPE.lock().map(|l| *l).unwrap_or("linear");
                let dflt = TYPES.iter().find(|(_, t)| *t == last).map_or("Linear", |(k, _)| *k);
                Prompt::new("Enter parameter type", KW).kw(&kws).default(dflt)
            }
            Phase::First => Prompt::new(self.first_message(), Accept::POINT).kw(self.options()).deferred(),
            Phase::End => Prompt::new(if self.ty() == "flip" { "Specify endpoint of reflection line" } else { "Specify endpoint" }, Accept::POINT)
                .base_opt(base),
            Phase::Radius => Prompt::new("Specify radius of parameter", Accept::POINT_OR_NUMBER).base_opt(base),
            Phase::Angle => Prompt::new("Specify default rotation angle", Accept::POINT_OR_NUMBER).kw(&["Base angle"]).default("0").base_opt(base),
            Phase::Direction => Prompt::new("Specify alignment direction or alignment type", Accept::POINT).kw(&["Type"]).base_opt(base),
            Phase::LabelAt => Prompt::new("Specify label location", Accept::POINT),
            Phase::Grips => {
                let (choices, dflt) = self.grip_choices();
                Prompt::new("Enter number of grip objects for parameter", KW).kw(choices).default(dflt)
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if !self.asks.is_empty() {
            let ask = self.asks.remove(0);
            self.answer(s, ask, &i)?;
            return Ok(Step::Continue);
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
        match self.phase {
            Phase::Type => {
                let t = match (&i, word.as_deref()) {
                    (Input::Enter, _) => LAST_TYPE.lock().map(|l| *l).unwrap_or("linear"),
                    (_, Some(w)) => {
                        let pr = Prompt::new("", KW).kw(&TYPES.iter().map(|(k, _)| *k).collect::<Vec<_>>());
                        let k = pr.match_keyword(w).ok_or_else(|| EngineError::Other("Invalid option keyword.".into()))?;
                        TYPES.iter().find(|(kw, _)| *kw == k).map_or("linear", |(_, t)| *t)
                    }
                    _ => return Ok(Step::Continue),
                };
                self.ty = Some(t);
                self.phase = Phase::First;
                Ok(Step::Continue)
            }
            Phase::First => {
                if let Some(p) = point {
                    self.set(if self.two_point() || matches!(self.ty(), "rotation" | "alignment") { "base" } else { "at" }, point_json(p));
                    return self.after_first(s);
                }
                if let Some(w) = word {
                    let pr = Prompt::new("", KW).kw(self.options());
                    let k = pr.match_keyword(&w).ok_or_else(|| EngineError::Other("Point or option keyword required.".into()))?;
                    self.option(&k);
                }
                Ok(Step::Continue)
            }
            Phase::End => {
                let Some(p) = point else { return Err(EngineError::Other("Requires a point.".into())) };
                self.set("end", point_json(p));
                self.phase = if self.ty() == "xy" { Phase::Grips } else { Phase::LabelAt };
                Ok(Step::Continue)
            }
            Phase::Radius => {
                let base = point_param(&Value::Object(self.p.clone()), "base").unwrap_or_default();
                let r = match (&i, point) {
                    (_, Some(p)) => (p - base).len(),
                    (Input::Text(t), _) => crate::cmd::machines::number(t).ok_or_else(|| EngineError::Other("Requires a distance.".into()))?,
                    _ => return Ok(Step::Continue),
                };
                if !(r > 0.0 && r.is_finite()) {
                    return Err(EngineError::Other("The radius must be more than 0.".into()));
                }
                self.set("radius", json!(r));
                self.phase = Phase::Angle;
                Ok(Step::Continue)
            }
            Phase::Angle => {
                let base = point_param(&Value::Object(self.p.clone()), "base").unwrap_or_default();
                let a = match (&i, point, word.as_deref()) {
                    (_, Some(p), _) => (p - base).angle().to_degrees(),
                    (Input::Keyword(k), _, _) if k == "Base angle" => {
                        self.asks.push(Ask::BaseAngle);
                        return Ok(Step::Continue);
                    }
                    (_, _, Some(w)) => crate::units::parse_angle(w).ok_or_else(|| EngineError::Other("Requires an angle.".into()))?.to_degrees(),
                    (Input::Enter, _, _) => 0.0,
                    _ => return Ok(Step::Continue),
                };
                // The angle is measured from the base angle.
                let from = self.p.get("baseAngle").and_then(Value::as_f64).unwrap_or(0.0);
                self.set("angle", json!(a - from));
                self.phase = Phase::LabelAt;
                Ok(Step::Continue)
            }
            Phase::Direction => {
                match (point, word.as_deref()) {
                    (Some(p), _) => {
                        self.set("end", point_json(p));
                        return self.finish(s);
                    }
                    (None, Some(_)) => {
                        let perpendicular = !self.p.get("perpendicular").and_then(Value::as_bool).unwrap_or(true);
                        self.set("perpendicular", json!(perpendicular));
                        s.echo(if perpendicular { "Alignment type = Perpendicular" } else { "Alignment type = Tangent" });
                    }
                    _ => {}
                }
                Ok(Step::Continue)
            }
            Phase::LabelAt => {
                let Some(p) = point else { return Err(EngineError::Other("Requires a point.".into())) };
                // ponytail: the label location is kept for a flip parameter (its grip); the
                // others show their label by their points.
                if self.ty() == "flip" {
                    self.set("labelAt", point_json(p));
                }
                self.phase = Phase::Grips;
                Ok(Step::Continue)
            }
            Phase::Grips => {
                let (choices, dflt) = self.grip_choices();
                let n = match (&i, word.as_deref()) {
                    (Input::Enter, _) => dflt.to_string(),
                    (_, Some(w)) => w.to_string(),
                    _ => return Ok(Step::Continue),
                };
                if !choices.contains(&n.as_str()) {
                    return Err(EngineError::Other(format!("Enter {}.", choices.join(", "))));
                }
                self.set("grips", json!(n.parse::<u64>().unwrap_or(1)));
                self.finish(s)
            }
        }
    }
}

/// BGRIPSET: a parameter, then how many grips it shows.
#[derive(Default)]
struct GripsetM {
    node: Option<i32>,
}

impl Interactive for GripsetM {
    fn name(&self) -> &'static str {
        "BGRIPSET"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.node {
            None => Prompt::new("Select parameter", Accept { point: true, number: false, text: true, select: false, enter: true }),
            Some(_) => Prompt::new("Enter number of grip objects for parameter", KW).kw(&["0", "1", "2", "4"]),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match (self.node, i) {
            (_, Input::Cancel) | (None, Input::Enter) => Ok(Step::Cancel),
            (None, i) => {
                self.node = pick_parameter(s, &i)?;
                if self.node.is_none() {
                    s.echo("No parameter there.");
                }
                Ok(Step::Continue)
            }
            (Some(node), Input::Keyword(t) | Input::Text(t)) => {
                let n = t.trim().parse::<u64>().map_err(|_| EngineError::Other("Enter 0, 1, 2 or 4.".into()))?;
                run_gripset(s, &json!({ "parameter": node, "grips": n }))?;
                Ok(Step::Done)
            }
            _ => Ok(Step::Continue),
        }
    }
}

/// The parameter a "Select parameter" answer names: picked near it, or typed by its label.
pub(super) fn pick_parameter(s: &Session, i: &Input) -> Result<Option<i32>> {
    let (_, base) = authoring(s)?;
    let Some(m) = authored(s)? else { return Err(EngineError::Other("The block has no parameters: add one first (BPARAMETER).".into())) };
    Ok(match i {
        Input::Point(p) => parameter_at(&m, base, *p, pick_aperture(s) * 2.0),
        Input::Deferred(d) => parameter_at(&m, base, d.at, pick_aperture(s) * 2.0),
        Input::Text(t) | Input::Keyword(t) => parameter_named(&m, &json!(t.trim())),
        _ => None,
    })
}
