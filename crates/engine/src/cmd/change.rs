//! CHANGE (-CH) and CHPROP: AutoCAD's command-line property changers. CHPROP sets colour, layer,
//! linetype, linetype scale, lineweight, thickness, transparency, plot style and the annotative
//! property; CHANGE also sets the elevation, or moves a change point: line ends, circle radii, and
//! the insertion point, style, height, rotation and text of texts, attribute definitions and block
//! references.

use std::collections::VecDeque;

use cadcraft_doc::{Entity, EntityKind, Handle};
use cadcraft_geom::{Mat3, Mat4, Vec2, Vec3};
use serde_json::{Map, Value, json};

use super::curves::KW;
use super::machines::{SelOutcome, SelectPhase, number};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

/// Properties properties.set changes for us.
const DELEGATED: &[&str] = &["color", "layer", "linetype", "lineweight", "ltscale", "transparency", "annotative"];

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("change", "Change", run_change)
            .alias(&["-ch"])
            .params(
                "{handles?, <CHPROP's properties>, elevation?, point?: [x,y] (the change point: line ends nearest it move there, circles pass through it, texts and block references are placed there), radius? (circles), style?, height?, rotation? (degrees; texts and block references), text? (a text's contents, an attribute definition's default), tag?, prompt? (attribute definitions)} → {changed}",
            )
            .interactive(|_| Ok(Box::new(ChangeM::new(false)))),
        CommandSpec::new("chprop", "Change Properties", run_chprop)
            .params("{handles?, color?, layer? (an existing layer), linetype? (loaded), ltscale?, lineweight?, thickness?, transparency?: \"ByLayer\"|\"ByBlock\"|0..90, plotStyle?, annotative?: bool} → {changed} (objects on locked layers are skipped)")
            .interactive(|_| Ok(Box::new(ChangeM::new(true)))),
    ]
}

fn other(m: impl Into<String>) -> EngineError {
    EngineError::Other(m.into())
}

fn hexes(hs: &[Handle]) -> Vec<String> {
    hs.iter().map(|h| h.hex()).collect()
}

/// The objects `p` names that exist and are not on a locked layer, and how many were locked.
fn editable(s: &Session, p: &Value) -> Result<(Vec<Handle>, usize)> {
    let d = s.doc()?;
    let hs: Vec<Handle> = targets(s, p)?.into_iter().filter(|h| d.entity(*h).is_some()).collect();
    let n = hs.len();
    let hs: Vec<Handle> = hs.into_iter().filter(|h| !super::curves::is_locked(s, *h)).collect();
    let locked = n - hs.len();
    Ok((hs, locked))
}

fn finite(cmd: &str, p: &Value, key: &str) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().filter(|x| x.is_finite() && x.abs() < 1e15).map(Some).ok_or_else(|| bad(cmd, format!("`{key}` must be a number"))),
    }
}

/// Set CHPROP's properties (and CHANGE's elevation) on `hs`.
fn set_props(cmd: &str, s: &mut Session, hs: &[Handle], p: &Value) -> Result<()> {
    if p.get("material").is_some() {
        return Err(bad(cmd, "materials are not available yet"));
    }
    if let Some(l) = str_param(p, "layer")
        && s.doc()?.layer(l).is_none()
    {
        return Err(bad(cmd, format!("Cannot find layer \"{l}\".")));
    }
    if p.get("ltscale").is_some() && size_param(cmd, p, "ltscale", false)?.is_none() {
        return Err(bad(cmd, "`ltscale` must be positive"));
    }
    let thickness = finite(cmd, p, "thickness")?;
    let elevation = finite(cmd, p, "elevation")?;
    let plot_style = match p.get("plotStyle") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| bad(cmd, "`plotStyle` must be a plot style name"))?
                .chars()
                .take(255)
                .collect::<String>(),
        ),
    };
    let mut q: Map<String, Value> = DELEGATED.iter().filter_map(|k| Some((k.to_string(), p.get(*k)?.clone()))).collect();
    if !q.is_empty() && !hs.is_empty() {
        q.insert("handles".into(), json!(hexes(hs)));
        run_body(s, "properties.set", &Value::Object(q))?;
    }
    let d = s.doc_mut()?;
    for h in hs {
        d.modify_entity(*h, |e| {
            if let Some(t) = thickness {
                e.common.thickness = t;
            }
            if let Some(ps) = &plot_style {
                e.common.plot_style.clone_from(ps);
            }
            // CHANGE's Elev: a level object moves along Z to the new height (left as it is when
            // it can't, as AutoCAD leaves objects whose points are not all at one Z).
            if let Some(z) = elevation
                && let Some(dz) = level_z(e).map(|now| z - now).filter(|dz| *dz != 0.0)
            {
                e.transform3(&Mat4::translate(Vec3::new(0.0, 0.0, dz)));
            }
        })?;
    }
    Ok(())
}

/// The Z of an object whose points all lie at one Z with the default extrusion (what CHANGE's
/// Elev can set); `None` for the others (sloped lines, tilted planes, solids, faces, regions).
fn level_z(e: &Entity) -> Option<f64> {
    match &e.kind {
        EntityKind::Point(p) => Some(p.p.z),
        EntityKind::Polyline3d(p) => {
            let z = p.points.first()?.z;
            p.points.iter().all(|q| q.z == z).then_some(z)
        }
        EntityKind::Region(_) | EntityKind::Wipeout(_) => None,
        k => (e.common.extrusion == Vec3::Z && e.plane().is_some()).then(|| k.elevation()),
    }
}

fn echo_locked(s: &mut Session, locked: usize) {
    if locked > 0 {
        s.echo(format!("{locked} object{} on a locked layer.", if locked == 1 { " was" } else { "s were" }));
    }
}

fn result(s: &mut Session, n: usize, locked: usize) -> Value {
    echo_locked(s, locked);
    json!({ "changed": n, "locked": locked })
}

/// The selected objects not on a locked layer; how many were is said on the command line.
fn unlocked(s: &mut Session, hs: Vec<Handle>) -> Vec<Handle> {
    let n = hs.len();
    let hs: Vec<Handle> = hs.into_iter().filter(|h| !super::curves::is_locked(s, *h)).collect();
    echo_locked(s, n - hs.len());
    hs
}

fn run_chprop(s: &mut Session, p: &Value) -> Result<Value> {
    if p.get("elevation").is_some() {
        return Err(bad("chprop", "CHPROP has no elevation; use CHANGE"));
    }
    let (hs, locked) = editable(s, p)?;
    set_props("chprop", s, &hs, p)?;
    Ok(result(s, hs.len(), locked))
}

/// CHANGE's change point and per-object values on one object.
fn change_geometry(s: &mut Session, h: Handle, p: &Value) -> Result<()> {
    const CMD: &str = "change";
    let point = point_param(p, "point");
    let radius = size_param(CMD, p, "radius", false)?;
    let height = size_param(CMD, p, "height", false)?;
    let rotation = finite(CMD, p, "rotation")?.map(f64::to_radians);
    let style = match str_param(p, "style") {
        Some(n) => {
            Some(s.doc()?.text_style(n.trim()).map(|t| t.name.clone()).ok_or_else(|| bad(CMD, format!("Cannot find text style \"{}\".", n.trim())))?)
        }
        None => None,
    };
    let text = str_param(p, "text").map(str::to_string);
    let tag = match str_param(p, "tag") {
        Some(t) if t.trim().is_empty() || t.contains(' ') => return Err(bad(CMD, "a tag must be non-empty without spaces")),
        t => t.map(|t| t.trim().to_ascii_uppercase()),
    };
    let prompt = str_param(p, "prompt").map(str::to_string);
    let text_edit = |t: &mut cadcraft_doc::Text| {
        if let Some(to) = point {
            let d = to - t.justify_point();
            for q in std::iter::once(&mut t.insert).chain(t.align_pt.as_mut()) {
                q.x += d.x;
                q.y += d.y;
            }
        }
        if let Some(st) = &style {
            t.style.clone_from(st);
        }
        if let Some(h) = height {
            t.height = h;
        }
        if let Some(r) = rotation {
            // An aligned or fit text turns its second point about its first.
            if let Some(a2) = t.align_pt.as_mut().filter(|_| matches!(t.halign, cadcraft_doc::HAlign::Aligned | cadcraft_doc::HAlign::Fit)) {
                let q = a2.xy().rotate_about(t.insert.xy(), r - t.rotation);
                (a2.x, a2.y) = (q.x, q.y);
            }
            t.rotation = r;
        }
        if let Some(v) = &text {
            t.value.clone_from(v);
        }
    };
    s.doc_mut()?.modify_entity(h, |e: &mut Entity| {
        if let EntityKind::Insert(i) = &e.kind {
            if point.is_none() && rotation.is_none() {
                return;
            }
            let base = i.insert.xy();
            let turn = rotation.map_or(0.0, |r| r - i.rotation);
            // Attributes go along, as MOVE and ROTATE take them.
            let m = Mat3::rotate_about(base, turn).then(Mat3::translate(point.unwrap_or(base) - base));
            e.transform(&m);
            return;
        }
        match &mut e.kind {
            EntityKind::Line(l) => {
                if let Some(to) = point {
                    let end = if l.a.xy().dist(to) <= l.b.xy().dist(to) { &mut l.a } else { &mut l.b };
                    (end.x, end.y) = (to.x, to.y);
                }
            }
            EntityKind::Circle(c) => {
                if let Some(r) = radius.or_else(|| point.map(|q| c.center.xy().dist(q)).filter(|r| *r > 1e-12)) {
                    c.radius = r;
                }
            }
            EntityKind::Text(t) => text_edit(t),
            EntityKind::AttDef(a) => {
                text_edit(&mut a.text);
                if let Some(t) = &tag {
                    a.tag.clone_from(t);
                }
                if let Some(pr) = &prompt {
                    a.prompt.clone_from(pr);
                }
            }
            _ => {}
        }
    })?;
    Ok(())
}

fn run_change(s: &mut Session, p: &Value) -> Result<Value> {
    let (hs, locked) = editable(s, p)?;
    set_props("change", s, &hs, p)?;
    for h in &hs {
        change_geometry(s, *h, p)?;
    }
    Ok(result(s, hs.len(), locked))
}

// ---------------- typed ----------------

/// A property asked at "Enter property to change".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prop {
    Color,
    Truecolor,
    Elev,
    Layer,
    Ltype,
    LtScale,
    LWeight,
    Thickness,
    Transparency,
    PlotStyle,
    Annotative,
}

/// A value CHANGE asks for one object after the change point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Radius,
    TextPoint,
    Style,
    Height,
    Rotation,
    Text,
    Tag,
    PromptText,
    Default,
    BlockPoint,
    BlockRotation,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ask {
    Select,
    Point,
    Props,
    Prop(Prop),
    Each,
}

/// CHANGE and CHPROP typed: objects, then (CHANGE) a change point or Properties, then property
/// after property until Enter, or the values of each object the change point doesn't settle.
struct ChangeM {
    chprop: bool,
    ask: Ask,
    sel: SelectPhase,
    picked: Vec<Handle>,
    queue: VecDeque<(Handle, Field)>,
}

impl ChangeM {
    fn new(chprop: bool) -> Self {
        ChangeM { chprop, ask: Ask::Select, sel: SelectPhase::default(), picked: Vec::new(), queue: VecDeque::new() }
    }
    /// Take the selection (less locked objects); done when nothing is left.
    fn after_select(&mut self, s: &mut Session, hs: Vec<Handle>) -> Step {
        self.picked = unlocked(s, hs);
        self.ask = if self.chprop { Ask::Props } else { Ask::Point };
        if self.picked.is_empty() { Step::Done } else { Step::Continue }
    }
    fn first(&self, s: &Session) -> Option<Entity> {
        let d = s.doc().ok()?;
        self.picked.iter().find_map(|h| d.entity(*h)).map(|e| (**e).clone())
    }
    /// Set a property on all the objects, back to the property prompt.
    fn set(&mut self, s: &mut Session, key: &str, v: Value) -> Result<Step> {
        let mut p = json!({ "handles": hexes(&self.picked) });
        if let Some(o) = p.as_object_mut() {
            o.insert(key.into(), v);
        }
        set_props(if self.chprop { "chprop" } else { "change" }, s, &self.picked, &p)?;
        self.ask = Ask::Props;
        Ok(Step::Continue)
    }
    /// One object's value; next object or value.
    fn each(&mut self, s: &mut Session, key: &str, v: Value) -> Result<Step> {
        if let Some((h, _)) = self.queue.front().copied() {
            let mut p = Map::new();
            p.insert(key.into(), v);
            change_geometry(s, h, &Value::Object(p))?;
        }
        self.next(s)
    }
    fn next(&mut self, s: &mut Session) -> Result<Step> {
        self.queue.pop_front();
        if self.queue.is_empty() {
            s.set_selection(Vec::new());
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    /// The change point (or Enter): lines and circles take it at once; texts, attribute
    /// definitions, block references (and circles without a point) are asked about one by one.
    fn change_point(&mut self, s: &mut Session, at: Option<Vec2>) -> Result<Step> {
        let d = s.doc()?;
        let ents: Vec<Entity> = self.picked.iter().filter_map(|h| d.entity(*h)).map(|e| (**e).clone()).collect();
        if let Some(p) = at {
            for e in ents.iter().filter(|e| matches!(e.kind, EntityKind::Line(_) | EntityKind::Circle(_))) {
                change_geometry(s, e.handle, &json!({ "point": [p.x, p.y] }))?;
            }
        }
        for e in &ents {
            let fields: &[Field] = match e.kind {
                EntityKind::Circle(_) if at.is_none() => &[Field::Radius],
                EntityKind::Text(_) => &[Field::TextPoint, Field::Style, Field::Height, Field::Rotation, Field::Text],
                EntityKind::AttDef(_) => {
                    &[Field::TextPoint, Field::Style, Field::Height, Field::Rotation, Field::Tag, Field::PromptText, Field::Default]
                }
                EntityKind::Insert(_) => &[Field::BlockPoint, Field::BlockRotation],
                _ => &[],
            };
            self.queue.extend(fields.iter().map(|f| (e.handle, *f)));
        }
        if self.queue.is_empty() {
            s.set_selection(Vec::new());
            return Ok(Step::Done);
        }
        self.ask = Ask::Each;
        Ok(Step::Continue)
    }
}

/// The text of a text or attribute definition.
fn text_of(e: &Entity) -> Option<&cadcraft_doc::Text> {
    match &e.kind {
        EntityKind::Text(t) => Some(t),
        EntityKind::AttDef(a) => Some(&a.text),
        _ => None,
    }
}

const PROP_KW: &[&str] =
    &["Color", "Elev", "LAyer", "LType", "ltScale", "LWeight", "Thickness", "TRansparency", "Material", "PLotstyle", "Annotative"];

impl Interactive for ChangeM {
    fn name(&self) -> &'static str {
        if self.chprop { "CHPROP" } else { "CHANGE" }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            return Ok(self.after_select(s, self.sel.picked.clone()));
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let first = self.first(s);
        let f4 = |v: f64| format!("{v:.4}");
        match self.ask {
            Ask::Select => self.sel.prompt(),
            Ask::Point => Prompt::new("Specify change point", Accept::POINT).kw(&["Properties"]),
            Ask::Props => {
                let kw: Vec<&str> = PROP_KW.iter().copied().filter(|k| !(self.chprop && *k == "Elev")).collect();
                Prompt::new("Enter property to change", KW).kw(&kw)
            }
            Ask::Prop(p) => {
                let c = first.as_ref().map(|e| e.common.clone()).unwrap_or_default();
                match p {
                    Prop::Color => Prompt::new("Enter new color", Accept { number: true, ..Accept::TEXT })
                        .kw(&["Truecolor", "COlorbook"])
                        .default(c.color.name().to_ascii_uppercase()),
                    Prop::Truecolor => Prompt::new("Red, Green, Blue", Accept { number: true, ..Accept::TEXT }),
                    Prop::Elev => Prompt::new("Specify new elevation", Accept::NUMBER).default(f4(first.as_ref().and_then(level_z).unwrap_or(0.0))),
                    Prop::Layer => Prompt::new("Enter new layer name", Accept::WORD).default(c.layer),
                    Prop::Ltype => Prompt::new("Enter new linetype name", Accept::WORD).default(c.linetype),
                    Prop::LtScale => Prompt::new("Specify new linetype scale", Accept::NUMBER).default(f4(c.ltscale)),
                    Prop::LWeight => Prompt::new("Enter new lineweight", Accept::WORD).default(c.lineweight.name()),
                    Prop::Thickness => Prompt::new("Specify new thickness", Accept::NUMBER).default(f4(c.thickness)),
                    Prop::Transparency => {
                        Prompt::new("Enter transparency value (0-90)", Accept::NUMBER).kw(&["ByLayer", "ByBlock"]).default(c.transparency.name())
                    }
                    Prop::PlotStyle => Prompt::new("Enter plot style", Accept::WORD).default(c.plot_style),
                    Prop::Annotative => Prompt::new("Make annotative?", KW)
                        .kw(&["Yes", "No"])
                        .default(if first.as_ref().is_some_and(cadcraft_doc::is_annotative) { "Yes" } else { "No" }),
                }
            }
            Ask::Each => {
                let Some((h, field)) = self.queue.front().copied() else { return Prompt::new("", Accept::POINT) };
                let e = s.doc().ok().and_then(|d| d.entity(h)).map(|e| (**e).clone());
                let t = e.as_ref().and_then(text_of);
                let deg = |r: f64| crate::units::format_angle(r, 0, 0);
                match field {
                    Field::Radius => {
                        let r = match e.as_ref().map(|e| &e.kind) {
                            Some(EntityKind::Circle(c)) => c.radius,
                            _ => 0.0,
                        };
                        Prompt::new("Specify new circle radius", Accept::POINT_OR_NUMBER).default(f4(r))
                    }
                    Field::TextPoint => Prompt::new("Specify new text insertion point", Accept::POINT).default("no change"),
                    Field::Style => Prompt::new("Enter new text style", Accept::WORD).default(t.map_or_else(String::new, |t| t.style.clone())),
                    Field::Height => Prompt::new("Specify new height", Accept::NUMBER).default(f4(t.map_or(0.0, |t| t.height))),
                    Field::Rotation => Prompt::new("Specify new rotation angle", Accept::NUMBER).default(deg(t.map_or(0.0, |t| t.rotation))),
                    Field::Text => Prompt::new("Enter new text", Accept::TEXT).default(t.map_or_else(String::new, |t| t.value.clone())),
                    Field::Tag | Field::PromptText | Field::Default => {
                        let a = match e.as_ref().map(|e| &e.kind) {
                            Some(EntityKind::AttDef(a)) => Some(a),
                            _ => None,
                        };
                        match field {
                            Field::Tag => Prompt::new("Enter new tag", Accept::WORD).default(a.map_or_else(String::new, |a| a.tag.clone())),
                            Field::PromptText => {
                                Prompt::new("Enter new prompt", Accept::TEXT).default(a.map_or_else(String::new, |a| a.prompt.clone()))
                            }
                            _ => Prompt::new("Enter new default value", Accept::TEXT).default(a.map_or_else(String::new, |a| a.text.value.clone())),
                        }
                    }
                    Field::BlockPoint => Prompt::new("Specify new block insertion point", Accept::POINT),
                    Field::BlockRotation => {
                        let r = match e.as_ref().map(|e| &e.kind) {
                            Some(EntityKind::Insert(i)) => i.rotation,
                            _ => 0.0,
                        };
                        Prompt::new("Specify new block rotation angle", Accept::NUMBER).default(deg(r))
                    }
                }
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        match (self.ask, i) {
            (Ask::Select, i) => match self.sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Done),
                SelOutcome::Done(hs) => Ok(self.after_select(s, hs)),
            },
            (Ask::Point, Input::Keyword(_)) => {
                self.ask = Ask::Props;
                Ok(Step::Continue)
            }
            (Ask::Point, Input::Point(p)) => self.change_point(s, Some(p)),
            (Ask::Point, Input::Enter) => self.change_point(s, None),
            (Ask::Props, Input::Keyword(k)) => {
                self.ask = Ask::Prop(match k.as_str() {
                    "Color" => Prop::Color,
                    "Elev" => Prop::Elev,
                    "LAyer" => Prop::Layer,
                    "LType" => Prop::Ltype,
                    "ltScale" => Prop::LtScale,
                    "LWeight" => Prop::LWeight,
                    "Thickness" => Prop::Thickness,
                    "TRansparency" => Prop::Transparency,
                    "PLotstyle" => Prop::PlotStyle,
                    "Annotative" => Prop::Annotative,
                    _ => return Err(other("Materials are not available yet.")),
                });
                Ok(Step::Continue)
            }
            (Ask::Props, Input::Enter) => {
                s.set_selection(Vec::new());
                Ok(Step::Done)
            }
            (Ask::Prop(_), Input::Enter) => {
                self.ask = Ask::Props;
                Ok(Step::Continue)
            }
            (Ask::Prop(Prop::Color), Input::Keyword(k)) if k == "Truecolor" => {
                self.ask = Ask::Prop(Prop::Truecolor);
                Ok(Step::Continue)
            }
            (Ask::Prop(Prop::Color), Input::Keyword(_)) => Err(other("Color books are not available.")),
            (Ask::Prop(Prop::Color | Prop::Truecolor), Input::Text(t)) => {
                if cadcraft_color::Color::parse(&t).is_none() {
                    return Err(other("Invalid color."));
                }
                self.set(s, "color", json!(t.trim()))
            }
            (Ask::Prop(Prop::Transparency), Input::Keyword(k)) => self.set(s, "transparency", json!(k)),
            (Ask::Prop(Prop::Annotative), Input::Keyword(k)) => self.set(s, "annotative", json!(k == "Yes")),
            (Ask::Prop(p), Input::Text(t)) => {
                let t = t.trim().trim_matches('"').to_string();
                let num = || number(&t).ok_or_else(|| other("Requires a number."));
                match p {
                    Prop::Elev => self.set(s, "elevation", json!(num()?)),
                    Prop::Layer => self.set(s, "layer", json!(t)),
                    Prop::Ltype => self.set(s, "linetype", json!(t)),
                    Prop::LtScale => self.set(s, "ltscale", json!(num()?)),
                    Prop::LWeight => {
                        let v = json!(t);
                        if super::layer::parse_lw(&v).is_none() {
                            return Err(other("Invalid lineweight."));
                        }
                        self.set(s, "lineweight", v)
                    }
                    Prop::Thickness => self.set(s, "thickness", json!(num()?)),
                    Prop::Transparency => self.set(s, "transparency", json!(num()?)),
                    Prop::PlotStyle => self.set(s, "plotStyle", json!(t)),
                    _ => Err(other("Invalid option keyword.")),
                }
            }
            (Ask::Each, i) => {
                let Some((h, field)) = self.queue.front().copied() else { return Ok(Step::Done) };
                match (field, i) {
                    (_, Input::Enter) => self.next(s),
                    (Field::Radius, Input::Point(p)) => {
                        let c = match s.doc()?.entity(h).map(|e| &e.kind) {
                            Some(EntityKind::Circle(c)) => c.center.xy(),
                            _ => p,
                        };
                        let r = c.dist(p);
                        if r <= 1e-12 {
                            return Err(other("Requires a positive value."));
                        }
                        self.each(s, "radius", json!(r))
                    }
                    (Field::TextPoint | Field::BlockPoint, Input::Point(p)) => self.each(s, "point", json!([p.x, p.y])),
                    (Field::Radius | Field::Height, Input::Text(t)) => {
                        let v = number(&t).filter(|v| *v > 0.0).ok_or_else(|| other("Requires a positive value."))?;
                        self.each(s, if field == Field::Radius { "radius" } else { "height" }, json!(v))
                    }
                    (Field::Rotation | Field::BlockRotation, Input::Text(t)) => {
                        let a = s.angle_settings().direction(&t).ok_or_else(|| other("Requires a valid angle."))?;
                        self.each(s, "rotation", json!(a.to_degrees()))
                    }
                    (Field::Style, Input::Text(t)) => self.each(s, "style", json!(t.trim())),
                    (Field::Text | Field::Default, Input::Text(t)) => self.each(s, "text", json!(t)),
                    (Field::Tag, Input::Text(t)) => self.each(s, "tag", json!(t.trim())),
                    (Field::PromptText, Input::Text(t)) => self.each(s, "prompt", json!(t)),
                    _ => Ok(Step::Continue),
                }
            }
            (_, Input::Text(_)) => Err(other("Invalid option keyword.")),
            _ => Ok(Step::Continue),
        }
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::{EntityKind, Handle, Transparency};
    use cadcraft_geom::Vec2;
    use serde_json::json;

    use crate::{Input, Session};

    fn h(v: &serde_json::Value) -> Handle {
        Handle::parse_hex(v["handle"].as_str().or_else(|| v["handles"][0].as_str()).unwrap()).unwrap()
    }
    fn kind(s: &Session, h: Handle) -> EntityKind {
        s.doc().unwrap().entity(h).unwrap().kind.clone()
    }

    #[test]
    fn chprop_json_sets_properties_and_skips_locked_layers() {
        let mut s = Session::new();
        s.execute("layer.new", &json!({"name": "Walls"})).unwrap();
        let l = h(&s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap());
        let r = s.execute("chprop", &json!({"handles": [l.hex()], "color": "red", "layer": "Walls", "thickness": 2.5, "transparency": 40, "plotStyle": "Thin", "ltscale": 2})).unwrap();
        assert_eq!(r["changed"], 1);
        let e = s.doc().unwrap().entity(l).unwrap().clone();
        assert_eq!(e.common.layer, "Walls");
        assert_eq!(e.common.color, cadcraft_color::Color::Index(1));
        assert_eq!(e.common.thickness, 2.5);
        assert_eq!(e.common.transparency, Transparency::Percent(40));
        assert_eq!(e.common.plot_style, "Thin");
        assert_eq!(e.common.ltscale, 2.0);
        for bad in [json!({"layer": "Nope"}), json!({"ltscale": 0}), json!({"thickness": "x"}), json!({"material": "Wood"}), json!({"elevation": 3})]
        {
            let mut p = bad.clone();
            p["handles"] = json!([l.hex()]);
            assert!(s.execute("chprop", &p).is_err(), "{bad}");
        }
        s.execute("layer.set", &json!({"name": "Walls", "locked": true})).unwrap();
        let r = s.execute("chprop", &json!({"handles": [l.hex()], "color": 3})).unwrap();
        assert_eq!((r["changed"].as_u64(), r["locked"].as_u64()), (Some(0), Some(1)));
    }

    #[test]
    fn chprop_typed_property_after_property() {
        let mut s = Session::new();
        let l = h(&s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap());
        s.set_selection(vec![l]);
        s.cmdline("chprop").unwrap();
        assert!(
            s.prompt_text()
                .ends_with("Enter property to change or [Color/LAyer/LType/ltScale/LWeight/Thickness/TRansparency/Material/PLotstyle/Annotative]:"),
            "{}",
            s.prompt_text()
        );
        s.cmdline("c").unwrap();
        assert!(s.prompt_text().ends_with("Enter new color or [Truecolor/COlorbook] <BYLAYER>:"), "{}", s.prompt_text());
        s.cmdline("t").unwrap();
        s.cmdline("10,20,30").unwrap();
        s.cmdline("t 3").unwrap();
        s.cmdline("lw 0.35").unwrap();
        s.cmdline("tr bylayer").unwrap();
        s.cmdline("a y").unwrap();
        s.cmdline("").unwrap();
        assert!(s.running.is_none(), "{}", s.prompt_text());
        let e = s.doc().unwrap().entity(l).unwrap().clone();
        assert_eq!(e.common.color, cadcraft_color::Color::True(cadcraft_color::Rgb(10, 20, 30)));
        assert_eq!(e.common.thickness, 3.0);
        assert_eq!(e.common.lineweight, cadcraft_doc::Lineweight::Mm100(35));
        assert!(cadcraft_doc::is_annotative(&e) || !cadcraft_doc::can_be_annotative(&e.kind));
        // One undo step for the whole command.
        s.cmdline("u").unwrap();
        assert_eq!(s.doc().unwrap().entity(l).unwrap().common.thickness, 0.0);
    }

    #[test]
    fn typed_change_and_chprop_leave_locked_objects_alone() {
        let mut s = Session::new();
        s.execute("layer.new", &json!({"name": "Walls"})).unwrap();
        s.execute("layer.current", &json!({"name": "Walls"})).unwrap();
        let l = h(&s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap());
        s.execute("layer.set", &json!({"name": "Walls", "locked": true})).unwrap();
        let before = kind(&s, l);
        s.cmdline("change all  20,20").unwrap();
        assert!(s.running.is_none(), "{}", s.prompt_text());
        assert_eq!(kind(&s, l), before);
        assert!(s.log.iter().any(|m| m == "1 object was on a locked layer."), "{:?}", s.log);
        s.cmdline("chprop all  t 7").unwrap();
        s.cmdline("").unwrap();
        assert_eq!(s.doc().unwrap().entity(l).unwrap().common.thickness, 0.0);
    }

    #[test]
    fn elev_sets_the_height_of_level_objects_and_leaves_sloped_ones() {
        let mut s = Session::new();
        let p = h(&s.execute("point", &json!({"at": [1, 1]})).unwrap());
        let ln = cadcraft_doc::Line { a: cadcraft_geom::Vec3::ZERO, b: cadcraft_geom::Vec3::new(10.0, 0.0, 10.0) };
        let sloped = s.doc_mut().unwrap().add(&cadcraft_doc::Space::Model, Default::default(), EntityKind::Line(ln)).unwrap();
        let poly = h(&s.execute("3dpoly", &json!({"points": [[0, 0, 3], [5, 0, 3], [5, 5, 3]]})).unwrap());
        for _ in 0..2 {
            s.execute("change", &json!({"handles": [p.hex(), sloped.hex(), poly.hex()], "elevation": 10})).unwrap();
        }
        let EntityKind::Point(pt) = kind(&s, p) else { panic!() };
        assert_eq!(pt.p.z, 10.0);
        let EntityKind::Line(ln) = kind(&s, sloped) else { panic!() };
        assert_eq!((ln.a.z, ln.b.z), (0.0, 10.0), "a sloped line is left as it is");
        let EntityKind::Polyline3d(pl) = kind(&s, poly) else { panic!() };
        assert!(pl.points.iter().all(|q| q.z == 10.0), "{pl:?}");
        // The typed prompt offers the object's own height.
        s.set_selection(vec![p]);
        s.cmdline("change p e").unwrap();
        assert!(s.prompt_text().ends_with("Specify new elevation <10.0000>:"), "{}", s.prompt_text());
        s.cancel();
    }

    #[test]
    fn change_point_moves_line_ends_and_circle_radii() {
        let mut s = Session::new();
        let l = h(&s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap());
        let c = h(&s.execute("circle", &json!({"center": [0, 0], "radius": 1})).unwrap());
        s.execute("change", &json!({"handles": [l.hex(), c.hex()], "point": [12, 3]})).unwrap();
        match (kind(&s, l), kind(&s, c)) {
            (EntityKind::Line(ln), EntityKind::Circle(ci)) => {
                assert_eq!(ln.b.xy(), Vec2::new(12.0, 3.0));
                assert_eq!(ln.a.xy(), Vec2::ZERO);
                assert!((ci.radius - Vec2::new(12.0, 3.0).len()).abs() < 1e-9);
            }
            other => panic!("{other:?}"),
        }
        // Elev moves a planar object along Z.
        s.execute("change", &json!({"handles": [c.hex()], "elevation": 4})).unwrap();
        assert_eq!(kind(&s, c).elevation(), 4.0);

        // Typed as -CH: a change point; then Properties.
        s.set_selection(vec![l]);
        s.cmdline("-ch").unwrap();
        assert!(s.prompt_text().ends_with("Specify change point or [Properties]:"), "{}", s.prompt_text());
        s.cmdline("0,-5").unwrap();
        assert!(s.running.is_none());
        let EntityKind::Line(ln) = kind(&s, l) else { panic!() };
        assert_eq!(ln.a.xy(), Vec2::new(0.0, -5.0));
        s.set_selection(vec![l]);
        s.cmdline("change p e 2 la 0").unwrap();
        s.cmdline("").unwrap();
        assert!(s.running.is_none(), "{}", s.prompt_text());
        assert_eq!(kind(&s, l).elevation(), 2.0);
    }

    #[test]
    fn change_asks_text_and_block_values_one_by_one() {
        let mut s = Session::new();
        let t = h(&s.execute("text", &json!({"at": [0, 0], "text": "Old", "height": 1})).unwrap());
        s.execute("circle", &json!({"center": [0, 0], "radius": 1})).unwrap();
        s.set_selection(vec![t]);
        s.cmdline("change").unwrap();
        s.cmdline("").unwrap();
        assert!(s.prompt_text().ends_with("Specify new text insertion point <no change>:"), "{}", s.prompt_text());
        s.cmdline("5,5").unwrap();
        assert!(s.prompt_text().ends_with("Enter new text style <Standard>:"), "{}", s.prompt_text());
        s.cmdline("").unwrap();
        assert!(s.prompt_text().ends_with("Specify new height <1.0000>:"), "{}", s.prompt_text());
        s.cmdline("2.5").unwrap();
        assert!(s.prompt_text().ends_with("Specify new rotation angle <0>:"), "{}", s.prompt_text());
        s.cmdline("90").unwrap();
        assert!(s.prompt_text().ends_with("Enter new text <Old>:"), "{}", s.prompt_text());
        s.cmdline("New words").unwrap();
        assert!(s.running.is_none());
        let EntityKind::Text(tx) = kind(&s, t) else { panic!() };
        assert_eq!((tx.value.as_str(), tx.height, tx.insert.xy()), ("New words", 2.5, Vec2::new(5.0, 5.0)));
        assert!((tx.rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        assert!(s.execute("change", &json!({"handles": [t.hex()], "style": "Nope"})).is_err());

        // A circle with no change point asks for its radius; a block reference for point and angle.
        let c = h(&s.execute("circle", &json!({"center": [20, 0], "radius": 1})).unwrap());
        s.execute("block", &json!({"name": "Box", "base": [0, 0], "handles": [c.hex()], "keep": "retain"})).unwrap();
        let ins = h(&s.execute("insert", &json!({"name": "Box", "at": [30, 0]})).unwrap());
        s.set_selection(vec![c, ins]);
        s.cmdline("change").unwrap();
        s.cmdline("").unwrap();
        assert!(s.prompt_text().ends_with("Specify new circle radius <1.0000>:"), "{}", s.prompt_text());
        s.cmdline("3").unwrap();
        assert!(s.prompt_text().ends_with("Specify new block insertion point:"), "{}", s.prompt_text());
        s.input(Input::Point(Vec2::new(40.0, 0.0))).unwrap();
        s.cmdline("45").unwrap();
        assert!(s.running.is_none(), "{}", s.prompt_text());
        let (EntityKind::Circle(ci), EntityKind::Insert(i)) = (kind(&s, c), kind(&s, ins)) else { panic!() };
        assert_eq!(ci.radius, 3.0);
        assert_eq!(i.insert.xy(), Vec2::new(40.0, 0.0));
        assert!((i.rotation - std::f64::consts::FRAC_PI_4).abs() < 1e-9);
    }
}
