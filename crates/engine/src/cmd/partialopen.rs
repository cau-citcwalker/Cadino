//! PARTIALOPEN opens a drawing with only part of its model space loaded: the objects on the
//! layers asked for, within a saved view or a window. PARTIALOAD loads more of a partially open
//! drawing. FULLOPEN (read-only) is 0 while a drawing is partially open.
//!
//! ponytail: the objects not loaded are read and kept (so saving never loses them) but hidden,
//! as HIDEOBJECTS hides; this saves the drawing time, not memory. UNISOLATEOBJECTS also shows
//! them.

use std::collections::BTreeSet;

use cadcraft_doc::{Drawing, Handle, entity_bounds};
use cadcraft_geom::{Bounds2, Vec2};
use serde_json::{Value, json};

use super::qselect::wildcard;
use super::{CommandSpec, bad, bool_or, points_param, run_body, str_param};
use crate::{Accept, EngineError, Input, Interactive, Picker, Prompt, Result, Session, Step};

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("partialopen", "Partial Open...", run_partialopen)
            .params(
                "{path: DWG/DXF file, layers?: [names] (wildcards; default all), view?: a saved view's name | window?: [[x,y],[x,y]] (model space; default everything), \
                 unloadXrefs?: bool} → {index, loaded, notLoaded}",
            )
            .enabled(super::always)
            .noundo()
            .interactive(|_| Ok(Box::new(PartialM::open(false)))),
        CommandSpec::new("-partialopen", "Partial Open (command line)", run_partialopen)
            .params("as PARTIALOPEN")
            .enabled(super::always)
            .noundo()
            .interactive(|_| Ok(Box::new(PartialM::open(true)))),
        CommandSpec::new("partiaload", "Partial Load", run_partiaload)
            .params("{layers?: [names] (wildcards; default all), view?: a saved view's name | window?: [[x,y],[x,y]] (default everything)} → {loaded, notLoaded} (a partially open drawing)")
            .interactive(|_| Ok(Box::new(PartialM::load()))),
        CommandSpec::new("-partiaload", "Partial Load (command line)", run_partiaload)
            .params("as PARTIALOAD")
            .interactive(|_| Ok(Box::new(PartialM::load()))),
    ]
}

/// The layer patterns a JSON call asks for; `None`: every layer.
fn layers(cmd: &str, p: &Value) -> Result<Option<Vec<String>>> {
    match p.get("layers") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) => Ok(Some(t.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_str().map(|t| t.trim().to_string()).ok_or_else(|| bad(cmd, "`layers` must be layer names")))
            .collect::<Result<Vec<_>>>()
            .map(Some),
        Some(_) => Err(bad(cmd, "`layers` must be layer names")),
    }
}

/// The model area a JSON call asks for in `d`; `None`: everywhere.
fn area(cmd: &str, d: &Drawing, p: &Value) -> Result<Option<Bounds2>> {
    if let Some(w) = points_param(p, "window") {
        return match w.as_slice() {
            [a, b] => Ok(Some(Bounds2::new(Vec2::new(a.x.min(b.x), a.y.min(b.y)), Vec2::new(a.x.max(b.x), a.y.max(b.y))))),
            _ => Err(bad(cmd, "`window` must be two corners")),
        };
    }
    match str_param(p, "view").map(str::trim).filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("*extents*")) {
        None => Ok(None),
        Some(name) => {
            let v = d
                .views
                .iter()
                .find(|v| v.name.eq_ignore_ascii_case(name) && v.layout.is_none())
                .ok_or_else(|| bad(cmd, format!("no model space view named \"{name}\"")))?;
            let half = Vec2::new(v.width.abs(), v.height.abs()) / 2.0;
            Ok(Some(Bounds2::new(v.center - half, v.center + half)))
        }
    }
}

/// The model space objects of `d` that `layers` and `area` leave out.
fn left_out(d: &Drawing, layers: Option<&[String]>, area: Option<Bounds2>) -> BTreeSet<Handle> {
    d.model
        .iter()
        .filter(|e| {
            let on = layers.is_none_or(|ls| ls.iter().any(|l| wildcard(l, &e.common.layer)));
            let inside = area.is_none_or(|a| {
                let b = entity_bounds(d, e, 0);
                b.is_empty() || b.intersects(&a)
            });
            !(on && inside)
        })
        .map(|e| e.handle)
        .collect()
}

/// Hide `hidden` in drawing `i` and set FULLOPEN, without counting it as a change.
fn set_loaded(s: &mut Session, i: usize, hidden: BTreeSet<Handle>) -> Value {
    let Some(st) = s.docs.get_mut(i) else { return json!({}) };
    let clean = std::sync::Arc::ptr_eq(&st.doc, &st.saved);
    let d = std::sync::Arc::make_mut(&mut st.doc);
    let not_loaded = hidden.len();
    let loaded = d.model.len().saturating_sub(not_loaded);
    d.header.set_i64("FULLOPEN", i64::from(hidden.is_empty()));
    d.hidden_objects = hidden;
    if clean {
        st.saved = st.doc.clone();
    }
    st.revision += 1;
    json!({ "loaded": loaded, "notLoaded": not_loaded })
}

fn run_partialopen(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "partialopen";
    let path = str_param(p, "path").map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(cmd, "`path` (a DWG or DXF file) is required"))?;
    let layers = layers(cmd, p)?;
    let opened = run_body(s, "open", &json!({ "path": path }))?;
    let i = opened.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).unwrap_or(s.active);
    let d = s.docs.get(i).map(|st| st.doc.clone()).ok_or(EngineError::NoDocument)?;
    let area = match area(cmd, &d, p) {
        Ok(a) => a,
        Err(e) => {
            // Nothing half done: the drawing closes again.
            let _ = run_body(s, "close", &json!({ "index": i }));
            return Err(e);
        }
    };
    let hidden = left_out(&d, layers.as_deref(), area);
    if bool_or(p, "unloadXrefs", false) && d.blocks.values().any(|b| b.xref_path.is_some()) {
        run_body(s, "-xref", &json!({ "option": "unload", "names": "*" }))?;
    }
    let mut r = set_loaded(s, i, hidden);
    r["index"] = json!(i);
    Ok(r)
}

fn run_partiaload(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "partiaload";
    let d = s.doc()?;
    if d.header.i64("FULLOPEN", 1) != 0 {
        return Err(bad(cmd, "the drawing is fully open (PARTIALOAD loads more of a drawing opened with PARTIALOPEN)"));
    }
    let layers = layers(cmd, p)?;
    let area = area(cmd, d, p)?;
    let more = left_out(d, layers.as_deref(), area);
    let hidden: BTreeSet<Handle> = d.hidden_objects.intersection(&more).copied().collect();
    let i = s.active;
    Ok(set_loaded(s, i, hidden))
}

enum PStage {
    File,
    View,
    Corner(Option<Vec2>),
    Layers,
    Xrefs,
}

/// PARTIALOPEN and PARTIALOAD typed (-PARTIALOPEN's and -PARTIALOAD's prompts): the drawing
/// (PARTIALOPEN), the view to load (or a window), the layers (one or more answers; Enter ends,
/// none given loads every layer), then whether to unload xrefs (PARTIALOPEN).
/// ponytail: no Partial Open and Partial Load dialogs; their choices are these prompts.
struct PartialM {
    opening: bool,
    typed: bool,
    stage: PStage,
    path: String,
    view: Option<String>,
    window: Option<[Vec2; 2]>,
    layers: Vec<String>,
}

impl PartialM {
    fn open(typed: bool) -> Self {
        PartialM { opening: true, typed, stage: PStage::File, path: String::new(), view: None, window: None, layers: Vec::new() }
    }
    fn load() -> Self {
        PartialM { opening: false, stage: PStage::View, ..PartialM::open(true) }
    }
    fn finish(&mut self, s: &mut Session, unload_xrefs: bool) -> Result<Step> {
        let mut p = json!({ "unloadXrefs": unload_xrefs });
        if !self.layers.is_empty() {
            p["layers"] = json!(self.layers);
        }
        if let Some(v) = &self.view {
            p["view"] = json!(v);
        }
        if let Some([a, b]) = self.window {
            p["window"] = json!([[a.x, a.y], [b.x, b.y]]);
        }
        let r = if self.opening {
            p["path"] = json!(self.path);
            run_partialopen(s, &p)?
        } else {
            run_partiaload(s, &p)?
        };
        s.echo(format!("Loaded {} object(s); {} not loaded.", r["loaded"].as_u64().unwrap_or(0), r["notLoaded"].as_u64().unwrap_or(0)));
        Ok(Step::Done)
    }
}

impl Interactive for PartialM {
    fn name(&self) -> &'static str {
        if self.opening { "PARTIALOPEN" } else { "PARTIALOAD" }
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.stage {
            PStage::File => {
                let p = Prompt::new("Enter name of drawing to open", Accept::TEXT);
                if self.typed { p } else { p.picker(Picker::File { save: false, label: "Drawings (DWG, DXF)", exts: &["dwg", "dxf"] }) }
            }
            PStage::View if self.opening => Prompt::new("Enter view to load", Accept::TEXT).default("*Extents*"),
            PStage::View => Prompt::new("Enter view to load", Accept::TEXT).kw(&["Window"]).default("*Extents*"),
            PStage::Corner(None) => Prompt::new("Specify first corner", Accept::POINT),
            PStage::Corner(Some(a)) => Prompt::new("Specify opposite corner", Accept::POINT).corner(a),
            PStage::Layers => Prompt::new("Enter layers to load", Accept::TEXT).default(if self.layers.is_empty() { "*" } else { "done" }),
            PStage::Xrefs => Prompt::new("Unload all Xrefs on open?", Accept::WORD).kw(&["Yes", "No"]).default("No"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let text = match &i {
            Input::Text(t) | Input::Keyword(t) => Some(t.trim().trim_matches('"').trim().to_string()).filter(|t| !t.is_empty()),
            _ => None,
        };
        match self.stage {
            PStage::File => {
                let Some(t) = text else { return Ok(Step::Done) };
                self.path = t;
                self.stage = PStage::View;
            }
            PStage::View => {
                if let Some(t) = text {
                    let l = t.to_ascii_lowercase();
                    if !self.opening && (l == "window" || l == "w") {
                        self.stage = PStage::Corner(None);
                        return Ok(Step::Continue);
                    }
                    if l != "*extents*" {
                        self.view = Some(t);
                    }
                }
                self.stage = PStage::Layers;
            }
            PStage::Corner(first) => match (first, i) {
                (None, Input::Point(a)) => self.stage = PStage::Corner(Some(a)),
                (Some(a), Input::Point(b)) => {
                    self.window = Some([a, b]);
                    self.stage = PStage::Layers;
                }
                _ => {}
            },
            PStage::Layers => match text {
                Some(t) if t != "." && !t.eq_ignore_ascii_case("done") => {
                    self.layers.extend(t.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()));
                }
                _ if self.opening => self.stage = PStage::Xrefs,
                _ => return self.finish(s, false),
            },
            PStage::Xrefs => {
                let yes = text.is_some_and(|t| t.to_ascii_lowercase().starts_with('y'));
                if text_is_bad(&i) {
                    return Err(EngineError::Other("Requires Yes or No.".into()));
                }
                return self.finish(s, yes);
            }
        }
        Ok(Step::Continue)
    }
}

/// An answer to a Yes/No question that is neither.
fn text_is_bad(i: &Input) -> bool {
    match i {
        Input::Text(t) | Input::Keyword(t) => !matches!(t.trim().to_ascii_lowercase().chars().next(), Some('y' | 'n')),
        _ => false,
    }
}
