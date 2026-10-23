//! COPYTOLAYER and -COPYTOLAYER: copies of objects on another layer, in place or moved.

use cadcraft_doc::Handle;
use cadcraft_geom::{Vec2, Vec3};
use serde_json::{Value, json};

use super::machines::{SelOutcome, SelectPhase};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

const PARAMS: &str = "{handles?, layer (created when missing), delta?: [dx,dy,dz?] | from?, to?} → {handles}: copies of the objects on `layer`, moved by `delta` (default in place)";

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("copytolayer", "Copy Objects to New Layer", run)
            .menu(&["Format", "Layer Tools", "Copy Objects to New Layer"])
            .params(PARAMS)
            .interactive(|_| Ok(Box::new(CopyToLayerM::new(false)))),
        CommandSpec::new("-copytolayer", "Copy Objects to New Layer", run).params(PARAMS).interactive(|_| Ok(Box::new(CopyToLayerM::new(true)))),
    ]
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "copytolayer";
    let layer = str_param(p, "layer").map(str::trim).filter(|l| !l.is_empty()).ok_or_else(|| bad(CMD, "`layer` is required"))?.to_string();
    if layer.len() > 255 || layer.chars().any(|c| "<>/\\\":;?*|=`".contains(c)) {
        return Err(bad(CMD, format!("\"{layer}\" is not a valid layer name")));
    }
    let delta = match super::solids::p3(p, "delta") {
        Some(d) => d,
        None => match (super::solids::p3(p, "from"), super::solids::p3(p, "to")) {
            (Some(a), Some(b)) => b - a,
            _ => Vec3::ZERO,
        },
    };
    let d = s.doc()?;
    let hs: Vec<Handle> = targets(s, p)?.into_iter().filter(|h| d.entity(*h).is_some()).collect();
    if hs.is_empty() {
        return Err(bad(CMD, "nothing selected"));
    }
    // The layer's own spelling when it exists.
    let layer = d.layer(&layer).map_or(layer, |l| l.name.clone());
    let copies = super::modify::displace(s, &hs, delta, true)?;
    let d = s.doc_mut()?;
    d.ensure_layer(&layer);
    for h in &copies {
        d.modify_entity(*h, |e| e.common.layer.clone_from(&layer))?;
    }
    let n = copies.len();
    let message = format!("{n} object{} copied and placed on layer \"{layer}\".", if n == 1 { "" } else { "s" });
    Ok(json!({ "handles": copies.iter().map(|h| h.hex()).collect::<Vec<_>>(), "message": message }))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ask {
    Select,
    Layer,
    Name,
    Base,
    Second(Vec2),
    Displacement,
}

/// COPYTOLAYER typed: objects, the destination layer (an object on it, or its Name), then where
/// the copies go: a base and second point, a Displacement, or eXit (in place). -COPYTOLAYER asks
/// for the layer name.
struct CopyToLayerM {
    ask: Ask,
    dash: bool,
    sel: SelectPhase,
    picked: Vec<Handle>,
    layer: String,
    /// The copies, once made (in place).
    copies: Vec<Handle>,
}

impl CopyToLayerM {
    fn new(dash: bool) -> Self {
        CopyToLayerM { ask: Ask::Select, dash, sel: SelectPhase::default(), picked: Vec::new(), layer: String::new(), copies: Vec::new() }
    }
    fn after_select(&mut self) {
        self.ask = if self.dash { Ask::Name } else { Ask::Layer };
    }
    fn copy(&mut self, s: &mut Session, layer: String) -> Result<Step> {
        let r = run(s, &json!({ "handles": self.picked.iter().map(|h| h.hex()).collect::<Vec<_>>(), "layer": layer }))?;
        s.echo(r["message"].as_str().unwrap_or_default().to_string());
        self.copies = r["handles"].as_array().into_iter().flatten().filter_map(|v| v.as_str().and_then(Handle::parse_hex)).collect();
        self.layer = layer;
        self.ask = Ask::Base;
        s.set_selection(Vec::new());
        Ok(Step::Continue)
    }
    fn place(&self, s: &mut Session, d: Vec2) -> Result<Step> {
        super::modify::displace(s, &self.copies, d.to3(0.0), false)?;
        Ok(Step::Done)
    }
}

fn current_layer(s: &Session) -> String {
    s.doc().map(|d| d.header.str("CLAYER", "0")).unwrap_or_else(|_| "0".into())
}

impl Interactive for CopyToLayerM {
    fn name(&self) -> &'static str {
        if self.dash { "-COPYTOLAYER" } else { "COPYTOLAYER" }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.picked = self.sel.picked.clone();
            self.after_select();
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match self.ask {
            Ask::Select if self.sel.removing => self.sel.prompt(),
            Ask::Select => Prompt::new("Select objects to copy", Accept::SELECT),
            Ask::Layer => Prompt::new("Select object on destination layer", Accept::SELECT).kw(&["Name"]).default("Name"),
            Ask::Name => Prompt::new("Enter layer name", Accept::WORD).default(current_layer(s)),
            Ask::Base => Prompt::new("Specify base point", Accept::POINT).kw(&["Displacement", "eXit"]).default("eXit"),
            Ask::Second(b) => Prompt::new("Specify second point of displacement or <use first point as displacement>", Accept::POINT).base(b),
            Ask::Displacement => Prompt::new("Specify displacement", Accept::POINT).default("0.0000,0.0000,0.0000"),
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
                SelOutcome::Done(hs) => {
                    self.picked = hs;
                    self.after_select();
                    Ok(Step::Continue)
                }
            },
            (Ask::Layer, Input::Keyword(_) | Input::Enter) => {
                self.ask = Ask::Name;
                Ok(Step::Continue)
            }
            (Ask::Layer, Input::Pick(hs)) => match hs.first().and_then(|h| s.doc().ok()?.entity(*h).map(|e| e.common.layer.clone())) {
                Some(l) => self.copy(s, l),
                None => Ok(Step::Continue),
            },
            (Ask::Name, Input::Text(t)) => self.copy(s, t.trim().trim_matches('"').to_string()),
            (Ask::Name, Input::Enter) => self.copy(s, current_layer(s)),
            (Ask::Base, Input::Keyword(k)) if k == "Displacement" => {
                self.ask = Ask::Displacement;
                Ok(Step::Continue)
            }
            (Ask::Base, Input::Keyword(_) | Input::Enter) => Ok(Step::Done),
            (Ask::Base, Input::Point(p)) => {
                self.ask = Ask::Second(p);
                Ok(Step::Continue)
            }
            (Ask::Second(b), Input::Point(p)) => self.place(s, p - b),
            (Ask::Second(b), Input::Enter) => self.place(s, b),
            (Ask::Displacement, Input::Point(p)) => self.place(s, p),
            (Ask::Displacement, Input::Enter) => Ok(Step::Done),
            (_, Input::Text(_)) => Err(EngineError::Other("Invalid input.".into())),
            _ => Ok(Step::Continue),
        }
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::{EntityKind, Handle};
    use cadcraft_geom::Vec2;
    use serde_json::json;

    use crate::{Input, Session};

    fn line(s: &mut Session, y: f64) -> Handle {
        Handle::parse_hex(s.execute("line", &json!({"points": [[0, y], [10, y]]})).unwrap()["handles"][0].as_str().unwrap()).unwrap()
    }
    fn on_layer(s: &Session, layer: &str) -> Vec<EntityKind> {
        s.doc().unwrap().model.iter().filter(|e| e.common.layer == layer).map(|e| e.kind.clone()).collect()
    }

    #[test]
    fn copies_go_to_the_layer_in_place_or_moved() {
        let mut s = Session::new();
        let a = line(&mut s, 0.0);
        let r = s.execute("copytolayer", &json!({"handles": [a.hex()], "layer": "Copies"})).unwrap();
        assert_eq!(r["message"], "1 object copied and placed on layer \"Copies\".");
        assert!(s.doc().unwrap().layer("Copies").is_some(), "created");
        assert_eq!(on_layer(&s, "Copies").len(), 1);
        assert_eq!(on_layer(&s, "0").len(), 1, "the original stays");
        s.execute("copytolayer", &json!({"handles": [a.hex()], "layer": "copies", "delta": [0, 5]})).unwrap();
        let moved = on_layer(&s, "Copies");
        assert!(moved.iter().any(|k| matches!(k, EntityKind::Line(l) if l.a.y == 5.0)), "{moved:?}");
        for bad in [json!({"handles": [a.hex()]}), json!({"handles": [a.hex()], "layer": "a*b"}), json!({"handles": [], "layer": "X"})] {
            assert!(s.execute("copytolayer", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn typed_forms_ask_layer_and_displacement() {
        let mut s = Session::new();
        s.execute("layer.new", &json!({"name": "Dest"})).unwrap();
        s.execute("layer.current", &json!({"name": "Dest"})).unwrap();
        let target = line(&mut s, 20.0);
        s.execute("layer.current", &json!({"name": "0"})).unwrap();
        let a = line(&mut s, 0.0);
        // Pick an object on the destination layer, then base and second point.
        s.cmdline("copytolayer").unwrap();
        assert!(s.prompt_text().ends_with("Select objects to copy:"), "{}", s.prompt_text());
        s.input(Input::Pick(vec![a])).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(s.prompt_text().ends_with("Select object on destination layer or [Name] <Name>:"), "{}", s.prompt_text());
        s.input(Input::Pick(vec![target])).unwrap();
        assert!(s.prompt_text().ends_with("Specify base point or [Displacement/eXit] <eXit>:"), "{}", s.prompt_text());
        s.input(Input::Point(Vec2::ZERO)).unwrap();
        s.input(Input::Point(Vec2::new(3.0, 0.0))).unwrap();
        assert!(s.running.is_none());
        assert!(on_layer(&s, "Dest").iter().any(|k| matches!(k, EntityKind::Line(l) if l.a.x == 3.0 && l.a.y == 0.0)));
        // -COPYTOLAYER: the name, then a displacement.
        s.set_selection(vec![a]);
        s.cmdline("-copytolayer New d 0,7").unwrap();
        assert!(s.running.is_none(), "{}", s.prompt_text());
        assert!(on_layer(&s, "New").iter().any(|k| matches!(k, EntityKind::Line(l) if l.a.y == 7.0)));
        // One undo step takes the copies back.
        s.cmdline("u").unwrap();
        assert!(on_layer(&s, "New").is_empty());
    }
}
