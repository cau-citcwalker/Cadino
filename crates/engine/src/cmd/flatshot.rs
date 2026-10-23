//! FLATSHOT (CAD-rhino): the 3D solids as the current view shows them, with hidden-line removal,
//! drawn flat as a block of 2D lines on the XY plane: inserted as a new block, replacing a
//! block's contents, or written to a file. SOLVIEW and SOLDRAW run VIEWBASE, which lays out
//! drawing views with their hidden lines on a layout in one step.

use cadcraft_geom::{Vec2, ViewDir};
use serde_json::{Value, json};

use super::curves::KW;
use super::section::{beside, by_block, define_block, flat_kind, is_kw, run_json};
use super::solids::{kernel, p3_value, placed};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("flatshot", "Flatshot", run_flatshot)
            .menu(&["Draw", "Modeling", "Flatshot"])
            .alias(&["fshot"])
            .params(
                "{handles? (default every 3D solid on a visible layer), view?: [x,y,z] (toward the viewer) | top|front|back|left|right|iso (default the current view), \
                 destination?: \"insert\" (default) | \"replace\" | \"export\", block?: name (insert: the new block's; replace: the block to redefine), \
                 at?: [x,y] (insert: the drawing's lower-left corner; default beside the model), path? (export: a .dwg or .dxf file), \
                 hidden?: bool (default false: the obscured lines too), hiddenLinetype? (default HIDDEN), hiddenLayer? (default 0)} \
                 → {block?, handle?, path?, visible, hidden}",
            )
            .interactive(|_| Ok(Box::new(FlatshotM::default()))),
        CommandSpec::new("solview", "Solid View", run_solview)
            .menu(&["Draw", "Modeling", "Setup", "View"])
            .params("{…}: VIEWBASE's (views of the 3D solids, with hidden lines, on a layout)")
            .interactive(|s| via_viewbase(s, "SOLVIEW")),
        CommandSpec::new("soldraw", "Solid Drawing", run_solview)
            .menu(&["Draw", "Modeling", "Setup", "Drawing"])
            .params("{…}: VIEWBASE's (views of the 3D solids, with hidden lines, on a layout)")
            .interactive(|s| via_viewbase(s, "SOLDRAW")),
    ]
}

fn view_param(s: &Session, cmd: &str, p: &Value) -> Result<ViewDir> {
    match p.get("view").filter(|v| !v.is_null()) {
        None => Ok(s.state()?.screen_dir()),
        Some(Value::String(n)) => {
            ViewDir::named(n).ok_or_else(|| bad(cmd, format!("`view` is top, front, back, left, right, iso or [x, y, z], not {n}")))
        }
        Some(v) => {
            let d = p3_value(v).filter(|d| d.len() > 1e-9).ok_or_else(|| bad(cmd, "`view` must be a direction [x, y, z] toward the viewer"))?;
            Ok(ViewDir::new(d))
        }
    }
}

/// The linetype `name`, loaded from the library when the drawing doesn't have it; its name as
/// the drawing has it.
fn linetype(s: &mut Session, cmd: &str, name: &str) -> Result<String> {
    let d = s.doc_mut()?;
    if let Some(l) = d.linetypes.iter().find(|l| l.name.eq_ignore_ascii_case(name)) {
        return Ok(l.name.clone());
    }
    if ["bylayer", "byblock", "continuous"].contains(&name.to_ascii_lowercase().as_str()) {
        return Ok(name.to_string());
    }
    let lt = cadcraft_doc::library::standard_linetypes_for(d)
        .into_iter()
        .find(|l| l.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| bad(cmd, format!("there is no linetype {name}")))?;
    let n = lt.name.clone();
    d.linetypes.push(lt);
    Ok(n)
}

fn text<'a>(cmd: &str, p: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match p.get(key).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v.as_str().map(str::trim).filter(|t| !t.is_empty()).map(Some).ok_or_else(|| bad(cmd, format!("`{key}` must be text"))),
    }
}

// ponytail: smooth (tangent) edges show only as silhouettes; AutoCAD's "Include tangential
// edges" when the kernel's flatshot passes them (OpenCASCADE's Rg1 edges).
fn run_flatshot(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "flatshot";
    let dir = view_param(s, CMD, p)?;
    let dest = text(CMD, p, "destination")?.unwrap_or("insert").to_ascii_lowercase();
    if !["insert", "replace", "export"].contains(&dest.as_str()) {
        return Err(bad(CMD, format!("`destination` is \"insert\", \"replace\" or \"export\", not \"{dest}\"")));
    }
    let block = text(CMD, p, "block")?.map(str::to_string);
    let path = text(CMD, p, "path")?.map(str::to_string);
    let at = match p.get("at").filter(|v| !v.is_null()) {
        Some(v) => Some(point_value(v).ok_or_else(|| bad(CMD, "`at` must be [x, y]"))?),
        None => None,
    };
    let exists = |s: &Session, b: &str| s.doc().is_ok_and(|d| d.block(b).is_some());
    match (dest.as_str(), &block, &path) {
        ("replace", None, _) => return Err(bad(CMD, "`block` (the block to replace) is required")),
        ("replace", Some(b), _) if !exists(s, b) => return Err(bad(CMD, format!("there is no block {b} to replace"))),
        ("insert", Some(b), _) if exists(s, b) => {
            return Err(bad(CMD, format!("there is a block {b} already (destination \"replace\" redefines it)")));
        }
        ("export", _, None) => return Err(bad(CMD, "`path` (the file to write) is required")),
        ("export", ..) => crate::limits::files(CMD)?,
        _ => {}
    }
    let hidden = bool_or(p, "hidden", false);
    let hidden_layer = text(CMD, p, "hiddenLayer")?.unwrap_or("0").to_string();
    if hidden_layer.chars().any(|c| "<>/\\\":;?*|,=`".contains(c)) {
        return Err(bad(CMD, format!("{hidden_layer} is not a valid layer name")));
    }
    let xs = super::furniture::chosen(s, p, CMD, true)?;
    let (right, _, toward) = dir.axes();
    let flat = cadcraft_kernel::flatshot(&xs.iter().map(placed).collect::<Vec<_>>(), toward, right).map_err(kernel)?;
    let visible: Vec<_> = flat.visible.iter().filter_map(flat_kind).collect();
    let obscured: Vec<_> = if hidden { flat.hidden.iter().filter_map(flat_kind).collect() } else { Vec::new() };
    if visible.is_empty() && obscured.is_empty() {
        return Err(EngineError::Other("The solids have no edges to draw.".into()));
    }
    let (nv, nh) = (visible.len(), obscured.len());
    let before = s.state()?.doc.clone();
    let dashes = if nh > 0 { Some(linetype(s, CMD, text(CMD, p, "hiddenLinetype")?.unwrap_or("HIDDEN"))?) } else { None };
    let mut items: Vec<_> = visible.into_iter().map(|k| (by_block(None), k)).collect();
    items.extend(obscured.into_iter().map(|k| (cadcraft_doc::Common { layer: hidden_layer.clone(), ..by_block(dashes.as_deref()) }, k)));
    let name = match &block {
        Some(b) => b.clone(),
        None => s.doc()?.anonymous_block_name("FLATSHOT"),
    };
    let name = define_block(s, CMD, &name, items)?;
    let mut out = json!({ "block": name, "visible": nv, "hidden": nh });
    match dest.as_str() {
        "insert" => {
            let a = at.unwrap_or_else(|| beside(&xs));
            out["handle"] = json!(super::blocks::insert(s, &name, a, 1.0, 0.0, &serde_json::Map::new())?.hex());
        }
        "export" => {
            let path = path.unwrap_or_default();
            let r = run_body(s, "wblock", &json!({ "path": path, "name": name }));
            // The block (and the linetype loaded for it) was only the file's contents: the
            // drawing stays as it was.
            s.state_mut()?.doc = before;
            r?;
            out = json!({ "path": path, "visible": nv, "hidden": nh, "message": format!("Wrote the flat view to {path}.") });
        }
        _ => {}
    }
    Ok(out)
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Dest {
    #[default]
    Ask,
    Insert,
    Replace,
    Export,
}

/// FLATSHOT at the command line (its options are a dialog in AutoCAD): the destination, the
/// block to replace or the file to write, whether to show obscured lines, then for a new block
/// its insertion point.
#[derive(Default)]
struct FlatshotM {
    dest: Dest,
    target: Option<String>,
    hidden: Option<bool>,
}

impl FlatshotM {
    fn run(&self, s: &mut Session, at: Option<Vec2>) -> Result<Step> {
        let mut p = json!({ "hidden": self.hidden.unwrap_or(false) });
        match self.dest {
            Dest::Replace => {
                p["destination"] = json!("replace");
                p["block"] = json!(self.target);
            }
            Dest::Export => {
                p["destination"] = json!("export");
                p["path"] = json!(self.target);
            }
            _ => {
                if let Some(a) = at {
                    p["at"] = json!([a.x, a.y]);
                }
            }
        }
        run_json(s, "flatshot", p)
    }
}

impl Interactive for FlatshotM {
    fn name(&self) -> &'static str {
        "FLATSHOT"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match (self.dest, &self.target, self.hidden) {
            (Dest::Ask, _, _) => Prompt::new("Enter an option", KW).kw(&["Insert", "Replace", "Export"]).default("Insert"),
            (Dest::Replace, None, _) => Prompt::new("Enter name of block to replace", Accept::TEXT),
            (Dest::Export, None, _) => Prompt::new("Enter name of file to write", Accept::TEXT).picker(crate::Picker::File {
                save: true,
                label: "Drawing",
                exts: &["dwg", "dxf"],
            }),
            (_, _, None) => Prompt::new("Show obscured lines?", KW).kw(&["Yes", "No"]).default("No"),
            _ => Prompt::new("Specify insertion point", Accept::POINT),
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
        if self.dest == Dest::Ask {
            self.dest = match &i {
                Input::Enter => Dest::Insert,
                _ if is_kw(&word, "insert") => Dest::Insert,
                _ if is_kw(&word, "replace") => Dest::Replace,
                _ if is_kw(&word, "export") => Dest::Export,
                _ if word.is_empty() => return Ok(Step::Continue),
                _ => return Err(EngineError::Other("Requires Insert, Replace or Export.".into())),
            };
            return Ok(Step::Continue);
        }
        if matches!(self.dest, Dest::Replace | Dest::Export) && self.target.is_none() {
            let name = word.trim_matches('"').trim();
            if name.is_empty() {
                return Ok(if matches!(i, Input::Enter) { Step::Cancel } else { Step::Continue });
            }
            if self.dest == Dest::Replace && s.doc()?.block(name).is_none() {
                return Err(EngineError::Other(format!("Block \"{name}\" not found.")));
            }
            self.target = Some(name.to_string());
            return Ok(Step::Continue);
        }
        if self.hidden.is_none() {
            self.hidden = Some(match &i {
                Input::Enter => false,
                _ if is_kw(&word, "yes") => true,
                _ if is_kw(&word, "no") => false,
                _ => return Ok(Step::Continue),
            });
            return if self.dest == Dest::Insert { Ok(Step::Continue) } else { self.run(s, None) };
        }
        match i {
            Input::Point(a) => self.run(s, Some(a)),
            Input::Enter => self.run(s, None),
            _ => Ok(Step::Continue),
        }
    }
}

// ---------------- SOLVIEW and SOLDRAW ----------------

/// SOLVIEW and SOLDRAW (AutoCAD's two-step layout views of solids: viewports, then their
/// profiles and hidden lines) are one step here: VIEWBASE draws the views with hidden lines
/// on a layout.
fn run_solview(s: &mut Session, p: &Value) -> Result<Value> {
    run_body(s, "viewbase", p)
}

/// VIEWBASE's prompts, after a line saying so.
fn via_viewbase(s: &Session, name: &'static str) -> Result<Box<dyn Interactive>> {
    let make = find_command("viewbase").and_then(|c| c.interactive).ok_or_else(|| EngineError::UnknownCommand("viewbase".into()))?;
    Ok(Box::new(Via { name, inner: make(s)? }))
}

struct Via {
    name: &'static str,
    inner: Box<dyn Interactive>,
}

impl Interactive for Via {
    fn name(&self) -> &'static str {
        self.name
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        s.echo("SOLVIEW and SOLDRAW are one step here: VIEWBASE makes the drawing views and their hidden lines.");
        self.inner.begin(s)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        self.inner.prompt(s)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        self.inner.input(s, i)
    }
    fn preview(&self, s: &Session, cursor: Vec2) -> Vec<cadcraft_doc::EntityKind> {
        self.inner.preview(s, cursor)
    }
}

#[cfg(test)]
#[path = "flatshot_tests.rs"]
mod tests;
