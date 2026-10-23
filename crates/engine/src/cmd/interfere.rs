//! INTERFERE (CAD-rhino): which 3D solids overlap (two sets, or every pair in one), by how much,
//! and the shared volumes as solids: for furniture, parts that collide (a drawer runner through a
//! side panel, a shelf too deep for its carcass).
//!
//! INTERFERE makes the interference solids on a layer of their own (red), selects them and asks
//! whether to delete them when it ends, as AutoCAD's dialog does on Close. -INTERFERE reports and
//! asks whether to make them, as AutoCAD's command line did.

use std::sync::Arc as Shared;

use cadcraft_color::Color;
use cadcraft_doc::{Drawing, EntityKind, Handle, Layer, Solid3d};
use cadcraft_geom::Mat4;
use serde_json::{Value, json};

use super::curves::{KW, handle_value};
use super::machines::{SelOutcome, SelectPhase};
use super::section::{is_kw, run_json};
use super::solids::{kernel, placed, solids};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

/// The layer interference solids go on when none is named.
const LAYER: &str = "INTERFERENCE";

pub fn specs() -> Vec<CommandSpec> {
    const PARAMS: &str = "{first?: [handles] (default `handles`, else the selection, else every 3D solid in model space), second?: [handles] (default: \
                          check the first set against itself), create?: bool (default true: the shared volumes as solids), layer? (default INTERFERENCE, \
                          made red)} → {pairs: [{first, second, volume, handle?}], volume, handles, checked}";
    vec![
        CommandSpec::new("interfere", "Interference Checking", run_interfere)
            .menu(&["Modify", "3D Operations", "Interference Checking"])
            .alias(&["inf"])
            .params(PARAMS)
            .interactive(|_| Ok(Box::new(InterfereM::new(false)))),
        CommandSpec::new("-interfere", "Interference Checking (command line)", run_interfere)
            .params(PARAMS)
            .interactive(|_| Ok(Box::new(InterfereM::new(true)))),
    ]
}

fn handles_of(cmd: &str, p: &Value, key: &str) -> Result<Option<Vec<Handle>>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| handle_value(v).ok_or_else(|| bad(cmd, format!("`{key}`: {v} is not an object handle"))))
            .collect::<Result<_>>()
            .map(Some),
        Some(v) => Err(bad(cmd, format!("`{key}` must be an array of object handles, not {v}"))),
    }
}

/// The closed solids among `hs`, each once (surfaces enclose nothing).
fn closed(s: &Session, hs: &[Handle]) -> Result<Vec<(Handle, Solid3d)>> {
    Ok(solids(s, hs)?.into_iter().filter(|(_, x)| !x.mesh.open).collect())
}

/// A solid as a report names it: its part name and handle.
fn label(h: Handle, x: &Solid3d) -> String {
    if x.part.name.trim().is_empty() { h.hex() } else { format!("{} [{}]", x.part.name.trim(), h.hex()) }
}

fn run_interfere(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "interfere";
    let mut first = match handles_of(CMD, p, "first")? {
        Some(hs) => hs,
        None => targets(s, p)?,
    };
    if first.is_empty() {
        first = s.doc()?.model.iter().map(|e| e.handle).collect();
    }
    let first = closed(s, &first)?;
    let second: Vec<(Handle, Solid3d)> = match handles_of(CMD, p, "second")? {
        Some(hs) => closed(s, &hs)?.into_iter().filter(|(h, _)| !first.iter().any(|(f, _)| f == h)).collect(),
        None => Vec::new(),
    };
    if first.is_empty() {
        return Err(bad(CMD, "select 3D solids"));
    }
    if second.is_empty() && first.len() < 2 {
        return Err(bad(CMD, "select at least two 3D solids, or a second set"));
    }
    let create = bool_or(p, "create", true);
    let layer = match p.get("layer").filter(|v| !v.is_null()) {
        None => LAYER.to_string(),
        Some(v) => {
            let t = v.as_str().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(CMD, "`layer` must be a layer name"))?;
            if t.chars().any(|c| "<>/\\\":;?*|,=`".contains(c)) {
                return Err(bad(CMD, format!("{t} is not a valid layer name")));
            }
            t.to_string()
        }
    };
    let fp: Vec<_> = first.iter().map(|(_, x)| placed(x)).collect();
    let sp: Vec<_> = second.iter().map(|(_, x)| placed(x)).collect();
    let clashes = cadcraft_kernel::interference(&fp, &sp).map_err(kernel)?;
    let other = if second.is_empty() { &first } else { &second };
    if create && !clashes.is_empty() {
        add_layer(s.doc_mut()?, &layer);
    }
    let (mut pairs, mut made, mut total) = (Vec::new(), Vec::new(), 0.0);
    for c in clashes {
        let (Some((ha, xa)), Some((hb, xb))) = (first.get(c.a), other.get(c.b)) else { continue };
        total += c.volume;
        let mut pair =
            json!({ "first": ha.hex(), "second": hb.hex(), "volume": c.volume, "firstName": label(*ha, xa), "secondName": label(*hb, xb) });
        if create {
            let kind = EntityKind::Solid3d(Solid3d {
                brep: Shared::from(c.solid.brep),
                mesh: Shared::new(c.solid.mesh),
                xform: Mat4::IDENTITY,
                ..Solid3d::default()
            });
            let h = s.add_entity_world(kind)?;
            s.doc_mut()?.modify_entity(h, |e| {
                e.common.layer.clone_from(&layer);
                e.common.color = Color::ByLayer;
            })?;
            pair["handle"] = json!(h.hex());
            made.push(h.hex());
        }
        pairs.push(pair);
    }
    Ok(json!({ "pairs": pairs, "volume": total, "handles": made, "checked": [first.len(), second.len()] }))
}

/// Layer `name`, red, when the drawing hasn't one.
fn add_layer(d: &mut Drawing, name: &str) {
    if d.layer(name).is_none() {
        d.layers.push(Layer { color: Color::Index(1), ..Layer::new(name) });
    }
}

/// The report at the command line: the sets, each pair with its volume, and the count.
fn report(s: &mut Session, r: &Value) {
    let prec = s.doc().map_or(4, |d| d.header.i64("LUPREC", 4).clamp(0, 8)) as usize;
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    let checked = &r["checked"];
    let (a, b) = (n(&checked[0]), n(&checked[1]));
    if b == 0 {
        s.echo(format!("Checking {a} solids against each other."));
    } else {
        s.echo(format!("Comparing {a} solids against {b} solids."));
    }
    let pairs = r["pairs"].as_array().cloned().unwrap_or_default();
    for p in &pairs {
        let (x, y, v) = (p["firstName"].as_str().unwrap_or(""), p["secondName"].as_str().unwrap_or(""), p["volume"].as_f64().unwrap_or(0.0));
        s.echo(format!("{x} and {y} interfere: volume {v:.prec$}."));
    }
    if pairs.is_empty() {
        s.echo("Solids do not interfere.");
    } else {
        s.echo(format!("Interfering pairs: {}", pairs.len()));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    First,
    Second,
    /// INTERFERE: delete the interference solids made? -INTERFERE: make them?
    Ask,
}

/// INTERFERE and -INTERFERE: the first set, the second (Enter checks the first set against
/// itself), then INTERFERE makes the interference solids, selects them and asks whether to delete
/// them; -INTERFERE asks whether to make them.
/// ponytail: AutoCAD's Nested selection (solids inside blocks) and Settings (the interference
/// objects' visual style and colour), and zooming to each pair; the layer is red instead.
struct InterfereM {
    dash: bool,
    stage: Stage,
    sel: SelectPhase,
    first: Vec<Handle>,
    second: Vec<Handle>,
    made: Vec<Handle>,
    /// The drawing before the interference solids were made, to put back.
    before: Option<std::sync::Arc<Drawing>>,
}

impl InterfereM {
    fn new(dash: bool) -> Self {
        InterfereM { dash, stage: Stage::First, sel: SelectPhase::default(), first: Vec::new(), second: Vec::new(), made: Vec::new(), before: None }
    }

    fn params(&self, create: bool) -> Value {
        let hex = |hs: &[Handle]| hs.iter().map(|h| h.hex()).collect::<Vec<_>>();
        let mut p = json!({ "first": hex(&self.first), "create": create });
        if !self.second.is_empty() {
            p["second"] = json!(hex(&self.second));
        }
        p
    }

    /// Check the sets: INTERFERE makes the solids and asks about deleting them, -INTERFERE asks
    /// about making them; no interference ends the command.
    fn check(&mut self, s: &mut Session) -> Result<Step> {
        let spec = find_command("interfere").ok_or_else(|| EngineError::UnknownCommand("interfere".into()))?;
        let before = s.state()?.doc.clone();
        let r = (spec.run)(s, &self.params(!self.dash))?;
        report(s, &r);
        if r["pairs"].as_array().is_none_or(Vec::is_empty) {
            s.set_selection(Vec::new());
            return Ok(Step::Done);
        }
        self.made = r["handles"].as_array().map(|a| a.iter().filter_map(handle_value).collect()).unwrap_or_default();
        self.before = Some(before);
        s.set_selection(self.made.clone());
        self.stage = Stage::Ask;
        Ok(Step::Continue)
    }
}

impl Interactive for InterfereM {
    fn name(&self) -> &'static str {
        if self.dash { "-INTERFERE" } else { "INTERFERE" }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.first = self.sel.picked.clone();
            self.sel = SelectPhase::default();
            self.stage = Stage::Second;
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.stage {
            _ if self.sel.removing => self.sel.prompt(),
            Stage::First => Prompt::new("Select first set of objects", Accept::SELECT),
            Stage::Second => Prompt::new("Select second set of objects", Accept::SELECT).kw(&["checK first set"]).default("checK"),
            Stage::Ask if self.dash => Prompt::new("Create interference solids?", KW).kw(&["Yes", "No"]).default("No"),
            Stage::Ask => Prompt::new("Delete interference objects created?", KW).kw(&["Yes", "No"]).default("Yes"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        let word = match &i {
            Input::Text(t) | Input::Keyword(t) => t.trim().to_string(),
            _ => String::new(),
        };
        match self.stage {
            Stage::First => Ok(match self.sel.feed(s, &i)? {
                SelOutcome::More => Step::Continue,
                SelOutcome::Empty => Step::Done,
                SelOutcome::Done(hs) => {
                    self.first = hs;
                    self.sel = SelectPhase::default();
                    self.stage = Stage::Second;
                    Step::Continue
                }
            }),
            // K or the whole word: C alone is a crossing window here.
            Stage::Second if matches!(word.to_ascii_lowercase().as_str(), "k" | "check" | "check first set") => self.check(s),
            Stage::Second => match self.sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => self.check(s),
                SelOutcome::Done(hs) => {
                    self.second = hs;
                    self.check(s)
                }
            },
            Stage::Ask => {
                let yes = match &i {
                    Input::Enter => !self.dash,
                    _ if is_kw(&word, "yes") => true,
                    _ if is_kw(&word, "no") => false,
                    _ => return Ok(Step::Continue),
                };
                if self.dash {
                    // Made only when asked for.
                    s.set_selection(Vec::new());
                    return if yes { run_json(s, "interfere", self.params(true)) } else { Ok(Step::Done) };
                }
                if yes && let Some(before) = self.before.take() {
                    s.state_mut()?.doc = before;
                }
                s.set_selection(Vec::new());
                Ok(Step::Done)
            }
        }
    }
}

#[cfg(test)]
#[path = "interfere_tests.rs"]
mod tests;
