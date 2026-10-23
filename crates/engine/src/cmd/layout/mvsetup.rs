//! MVSETUP: set up a layout's viewports. Create (a single viewport, the standard engineering
//! 2 × 2 arrangement or an array of viewports, in a bounding area with gaps between them; Delete
//! objects), Scale viewports (paper units per model units) and Align (pan a viewport so a point
//! lines up horizontally or vertically with a base point).
// ponytail: Align's Angled and Rotate view, Options (layer, limits, units, xref) and Title block
// (AutoCAD reads its title blocks from its own files) are not here; Std. Engineering's four
// viewports all show the plan, as layout viewports have no view direction yet.

use cadcraft_doc::{EntityKind, Handle, Space};
use cadcraft_geom::{Bounds2, Vec2};
use serde_json::{Value, json};

use super::viewports::{new_viewport, next_viewport_id, viewport_at};
use super::{layout_viewports, parse_scale, printable, target_layout};
use crate::cmd::machines::{SelOutcome, SelectPhase};
use crate::cmd::{CommandSpec, bad, point_param, str_param, targets};
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("mvsetup", "Set Up Viewports", run_mvsetup)
            .params(
                "{option: create|scale|align|delete, layout?, \
                 create: arrangement?: single|standard|array (1|2|3), p1?, p2? (bounding area; default the printable area), cols?, rows? (array, 1-16), dx?, dy? (gaps) → {viewports}; \
                 scale: handles? (viewports; default all), paper? (default 1), model? (default 1) | scale?: \"1:50\"; \
                 align: mode: horizontal|vertical, base: [x,y], point: [x,y] (paper points; the viewport under `point` pans); \
                 delete: handles (paper-space objects to erase)}",
            )
            .interactive(|_| Ok(Box::new(MvsetupM::default()))),
    ]
}

/// Most viewports one direction of an array has.
const MAX_ARRAY: usize = 16;

/// `r` split into `cols` × `rows` viewports with gaps `dx`, `dy`, top row first.
fn grid(r: Bounds2, cols: usize, rows: usize, dx: f64, dy: f64) -> Option<Vec<Bounds2>> {
    let (c, n) = (cols as f64, rows as f64);
    let w = (r.width() - dx * (c - 1.0)) / c;
    let h = (r.height() - dy * (n - 1.0)) / n;
    if !(w.is_finite() && h.is_finite() && w > 1e-6 && h > 1e-6) {
        return None;
    }
    let mut out = Vec::with_capacity(cols * rows);
    for row in 0..rows {
        for col in 0..cols {
            let x0 = r.min.x + col as f64 * (w + dx);
            let y1 = r.max.y - row as f64 * (h + dy);
            out.push(Bounds2::new(Vec2::new(x0, y1 - h), Vec2::new(x0 + w, y1)));
        }
    }
    Some(out)
}

fn count(cmd: &str, p: &Value, key: &str) -> Result<usize> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(1),
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (1..=MAX_ARRAY).contains(n))
            .ok_or_else(|| bad(cmd, format!("`{key}` must be 1 to {MAX_ARRAY}"))),
    }
}

fn gap(cmd: &str, p: &Value, key: &str) -> Result<f64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(0.0),
        Some(v) => v.as_f64().filter(|x| x.is_finite() && *x >= 0.0 && *x < 1e7).ok_or_else(|| bad(cmd, format!("`{key}` must be zero or positive"))),
    }
}

fn create(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "mvsetup";
    let space = target_layout(s, cmd, p)?;
    let Space::Paper(name) = &space else { return Err(bad(cmd, "not a layout")) };
    let area = match (point_param(p, "p1"), point_param(p, "p2")) {
        (Some(a), Some(b)) => Bounds2::new(a, b),
        _ => printable(s, name)?,
    };
    let arrangement = match p.get("arrangement") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(t)) => t.trim().to_ascii_lowercase(),
        _ => "single".into(),
    };
    let (cols, rows) = match arrangement.as_str() {
        "1" | "single" => (1, 1),
        "2" | "standard" | "std. engineering" | "engineering" => (2, 2),
        "3" | "array" => (count(cmd, p, "cols")?, count(cmd, p, "rows")?),
        "0" | "none" => return Ok(json!({ "viewports": [] })),
        o => return Err(bad(cmd, format!("unknown arrangement `{o}` (single, standard or array)"))),
    };
    let (dx, dy) = if cols * rows == 1 { (0.0, 0.0) } else { (gap(cmd, p, "dx")?, gap(cmd, p, "dy")?) };
    if area.width() > 1e7 || area.height() > 1e7 {
        return Err(bad(cmd, "the bounding area is too large"));
    }
    let rects = grid(area, cols, rows, dx, dy).ok_or_else(|| bad(cmd, "the viewports have no area (a smaller gap or a larger bounding area)"))?;
    let common = s.current_common();
    let d = s.doc_mut()?;
    let mut out = Vec::new();
    for r in rects {
        let id = next_viewport_id(d);
        let vp = new_viewport(d, r, id);
        out.push(d.add(&space, common.clone(), EntityKind::Viewport(vp))?.hex());
    }
    Ok(json!({ "viewports": out }))
}

/// The viewports `p` names (or all of the current layout's).
fn chosen_viewports(s: &Session, p: &Value) -> Result<Vec<Handle>> {
    let all: Vec<Handle> = layout_viewports(s)?.into_iter().map(|(h, _)| h).collect();
    if p.get("handles").is_none() && p.get("handle").is_none() {
        return Ok(all);
    }
    Ok(targets(s, p)?.into_iter().filter(|h| all.contains(h)).collect())
}

fn scale(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "mvsetup";
    let k = match p.get("scale") {
        Some(v) => parse_scale(v).ok_or_else(|| bad(cmd, "`scale` must be a positive number or \"a:b\""))?,
        None => {
            let unit = |key: &str| match p.get(key) {
                None | Some(Value::Null) => Ok(1.0),
                Some(v) => v.as_f64().filter(|x| x.is_finite() && *x > 0.0).ok_or_else(|| bad(cmd, format!("`{key}` must be positive"))),
            };
            unit("paper")? / unit("model")?
        }
    };
    if !(k.is_finite() && k > 0.0) {
        return Err(bad(cmd, "the scale must be positive"));
    }
    let hs = chosen_viewports(s, p)?;
    if hs.is_empty() {
        return Err(bad(cmd, "no viewport to scale"));
    }
    let d = s.doc_mut()?;
    let mut done = Vec::new();
    for h in hs {
        let _ = d.modify_entity(h, |e| {
            if let EntityKind::Viewport(v) = &mut e.kind
                && !v.locked
                && (v.height / k).is_finite()
            {
                v.view_height = v.height / k;
                done.push(h.hex());
            }
        });
    }
    Ok(json!({ "viewports": done, "scale": k }))
}

/// Pan the viewport under `point` so `point` lines up with `base` (paper points).
fn align(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "mvsetup";
    let horizontal = match str_param(p, "mode").unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "horizontal" | "h" => true,
        "vertical" | "v" | "vertical alignment" => false,
        _ => return Err(bad(cmd, "`mode`: horizontal or vertical")),
    };
    let (Some(base), Some(at)) = (point_param(p, "base"), point_param(p, "point")) else {
        return Err(bad(cmd, "`base` and `point` ([x, y]) are required"));
    };
    let h = viewport_at(s, at).ok_or_else(|| bad(cmd, "`point` is not inside a viewport"))?;
    let d = s.doc_mut()?;
    let mut moved = false;
    let _ = d.modify_entity(h, |e| {
        if let EntityKind::Viewport(v) = &mut e.kind
            && !v.locked
            && v.height > 0.0
            && v.view_height > 0.0
        {
            let k = v.view_height / v.height;
            let shift = if horizontal { Vec2::new(0.0, base.y - at.y) } else { Vec2::new(base.x - at.x, 0.0) };
            let c = v.view_center - shift * k;
            if c.is_finite() {
                v.view_center = c;
                moved = true;
            }
        }
    });
    if !moved {
        return Err(bad(cmd, "the viewport is locked"));
    }
    Ok(json!({ "viewport": h.hex() }))
}

fn run_mvsetup(s: &mut Session, p: &Value) -> Result<Value> {
    match str_param(p, "option").unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "create" | "c" => create(s, p),
        "scale" | "s" | "scale viewports" => scale(s, p),
        "align" | "a" => align(s, p),
        "delete" | "d" | "delete objects" => {
            let hs = targets(s, p)?;
            let n = crate::cmd::oops::erase(s, &hs)?;
            Ok(json!({ "erased": n }))
        }
        _ => Err(bad("mvsetup", "`option`: create, scale, align or delete")),
    }
}

/// Where MVSETUP typed at the command line is.
#[derive(Default)]
enum Stage {
    /// In the Model tab: "Enable paper space?"
    #[default]
    PaperSpace,
    Main,
    Create,
    Delete(SelectPhase),
    Layout,
    Corner {
        layout: usize,
        first: Option<Vec2>,
    },
    Cols {
        area: Bounds2,
    },
    Rows {
        area: Bounds2,
        cols: usize,
    },
    GapX {
        layout: usize,
        area: Bounds2,
        cols: usize,
        rows: usize,
    },
    GapY {
        layout: usize,
        area: Bounds2,
        cols: usize,
        rows: usize,
        dx: f64,
    },
    ScaleSelect(SelectPhase),
    ScaleMode(Vec<Handle>),
    PaperUnits(Vec<Handle>),
    ModelUnits(Vec<Handle>, f64),
    Align,
    AlignBase {
        horizontal: bool,
    },
    AlignPoint {
        horizontal: bool,
        base: Vec2,
    },
}

#[derive(Default)]
pub(super) struct MvsetupM {
    stage: Stage,
}

fn word(i: &Input) -> Option<String> {
    match i {
        Input::Keyword(k) | Input::Text(k) => Some(k.trim().to_ascii_lowercase()),
        _ => None,
    }
}

/// A number typed, or Enter for `default`.
fn num_or(i: &Input, default: f64) -> Option<f64> {
    match i {
        Input::Enter => Some(default),
        Input::Text(t) => t.trim().parse::<f64>().ok().filter(|v| v.is_finite()),
        _ => None,
    }
}

const LAYOUTS: &[&str] = &["0: None", "1: Single", "2: Std. Engineering", "3: Array of Viewports"];

/// An engine refusal as a re-prompt message.
fn retry(e: EngineError) -> EngineError {
    match e {
        EngineError::BadParams { msg, .. } => EngineError::Other(msg),
        other => other,
    }
}

impl MvsetupM {
    fn create(s: &mut Session, layout: usize, area: Bounds2, cols: usize, rows: usize, dx: f64, dy: f64) -> Result<()> {
        let arrangement = match layout {
            1 => "single",
            2 => "standard",
            _ => "array",
        };
        let p = json!({ "option": "create", "arrangement": arrangement, "p1": [area.min.x, area.min.y], "p2": [area.max.x, area.max.y], "cols": cols, "rows": rows, "dx": dx, "dy": dy });
        run_mvsetup(s, &p).map_err(retry)?;
        Ok(())
    }
}

impl Interactive for MvsetupM {
    fn name(&self) -> &'static str {
        "MVSETUP"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if s.layout_space() != Space::Model {
            if let Ok(st) = s.state_mut() {
                st.mspace = None;
            }
            self.stage = Stage::Main;
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match &self.stage {
            Stage::PaperSpace => Prompt::new("Enable paper space?", Accept::WORD).kw(&["No", "Yes"]).default("Yes"),
            Stage::Main => Prompt::new("Enter an option", Accept::WORD).kw(&["Align", "Create", "Scale viewports"]),
            Stage::Create => Prompt::new("Enter an option", Accept::WORD).kw(&["Delete objects", "Create viewports"]).default("Create viewports"),
            Stage::Delete(sel) | Stage::ScaleSelect(sel) => sel.prompt(),
            Stage::Layout => Prompt::new("Enter layout number to load", Accept::WORD).kw(&["Redisplay"]),
            Stage::Corner { first: None, .. } => Prompt::new("Specify first corner of bounding area for viewport(s)", Accept::POINT),
            Stage::Corner { first: Some(a), .. } => Prompt::new("Specify opposite corner", Accept::POINT).corner(*a),
            Stage::Cols { .. } => Prompt::new("Enter number of viewports in X direction", Accept::NUMBER).default("1"),
            Stage::Rows { .. } => Prompt::new("Enter number of viewports in Y direction", Accept::NUMBER).default("1"),
            Stage::GapX { .. } => Prompt::new("Specify distance between viewports in X direction", Accept::NUMBER).default("0.0"),
            Stage::GapY { .. } => Prompt::new("Specify distance between viewports in Y direction", Accept::NUMBER).default("0.0"),
            Stage::ScaleMode(_) => Prompt::new("Set zoom scale factors for viewports", Accept::WORD).kw(&["Uniform"]).default("Uniform"),
            Stage::PaperUnits(_) => Prompt::new("Enter the number of paper space units", Accept::NUMBER).default("1.0"),
            Stage::ModelUnits(..) => Prompt::new("Enter the number of model space units", Accept::NUMBER).default("1.0"),
            Stage::Align => Prompt::new("Enter an option", Accept::WORD).kw(&["Horizontal", "Vertical alignment"]),
            Stage::AlignBase { .. } => Prompt::new("Specify basepoint", Accept::POINT),
            Stage::AlignPoint { base, .. } => Prompt::new("Specify point in viewport to be panned", Accept::POINT).base(*base),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let stage = std::mem::take(&mut self.stage);
        let again = |me: &mut Self, st: Stage, msg: &str| {
            me.stage = st;
            Err(EngineError::Other(msg.into()))
        };
        match stage {
            Stage::PaperSpace => match (matches!(i, Input::Enter), word(&i).as_deref()) {
                (true, _) | (_, Some("yes" | "y")) => {
                    let first = s.doc()?.layouts.iter().min_by_key(|l| l.tab_order).map(|l| l.name.clone());
                    let Some(name) = first else {
                        s.echo("The drawing has no layout.");
                        return Ok(Step::Done);
                    };
                    super::run_layout(s, &json!({ "option": "set", "name": name })).map_err(retry)?;
                    self.stage = Stage::Main;
                    Ok(Step::Continue)
                }
                (_, Some("no" | "n")) => {
                    s.echo("MVSETUP in model space (units, scale and a paper border) is not available yet.");
                    Ok(Step::Done)
                }
                _ => again(self, Stage::PaperSpace, "Requires Yes or No."),
            },
            Stage::Main => match word(&i).as_deref() {
                _ if matches!(i, Input::Enter) => Ok(Step::Done),
                Some("create" | "c") => {
                    self.stage = Stage::Create;
                    Ok(Step::Continue)
                }
                Some("scale viewports" | "s") => {
                    let mut sel = SelectPhase::default();
                    // Selected viewports are the ones to scale.
                    let pre: Vec<Handle> = layout_viewports(s)
                        .map(|v| v.into_iter().map(|(h, _)| h).filter(|h| s.selection().contains(h)).collect())
                        .unwrap_or_default();
                    if !pre.is_empty() {
                        self.stage = Stage::ScaleMode(pre);
                        return Ok(Step::Continue);
                    }
                    sel.done = false;
                    s.echo("Select the viewports to scale...");
                    self.stage = Stage::ScaleSelect(sel);
                    Ok(Step::Continue)
                }
                Some("align" | "a") => {
                    self.stage = Stage::Align;
                    Ok(Step::Continue)
                }
                _ => again(self, Stage::Main, "Requires an option keyword."),
            },
            Stage::Create => match (matches!(i, Input::Enter), word(&i).as_deref()) {
                (true, _) | (_, Some("create viewports" | "c")) => {
                    for l in LAYOUTS {
                        s.echo(*l);
                    }
                    self.stage = Stage::Layout;
                    Ok(Step::Continue)
                }
                (_, Some("delete objects" | "d")) => {
                    self.stage = Stage::Delete(SelectPhase::default());
                    Ok(Step::Continue)
                }
                _ => again(self, Stage::Create, "Requires an option keyword."),
            },
            Stage::Delete(mut sel) => match sel.feed(s, &i)? {
                SelOutcome::More => {
                    self.stage = Stage::Delete(sel);
                    Ok(Step::Continue)
                }
                SelOutcome::Empty => {
                    self.stage = Stage::Main;
                    Ok(Step::Continue)
                }
                SelOutcome::Done(hs) => {
                    run_mvsetup(s, &json!({ "option": "delete", "handles": hs.iter().map(|h| h.hex()).collect::<Vec<_>>() })).map_err(retry)?;
                    self.stage = Stage::Main;
                    Ok(Step::Continue)
                }
            },
            Stage::Layout => match word(&i).as_deref() {
                _ if matches!(i, Input::Enter) => {
                    self.stage = Stage::Main;
                    Ok(Step::Continue)
                }
                Some("redisplay" | "r") => {
                    for l in LAYOUTS {
                        s.echo(*l);
                    }
                    self.stage = Stage::Layout;
                    Ok(Step::Continue)
                }
                Some("0") => {
                    self.stage = Stage::Main;
                    Ok(Step::Continue)
                }
                Some(n @ ("1" | "2" | "3")) => {
                    self.stage = Stage::Corner { layout: n.parse().unwrap_or(1), first: None };
                    Ok(Step::Continue)
                }
                _ => again(self, Stage::Layout, "Enter 0, 1, 2 or 3."),
            },
            Stage::Corner { layout, first } => match (first, i) {
                (None, Input::Point(a)) => {
                    self.stage = Stage::Corner { layout, first: Some(a) };
                    Ok(Step::Continue)
                }
                (Some(a), Input::Point(b)) => {
                    let area = Bounds2::new(a, b);
                    self.stage = match layout {
                        1 => {
                            Self::create(s, 1, area, 1, 1, 0.0, 0.0)?;
                            Stage::Main
                        }
                        2 => Stage::GapX { layout, area, cols: 2, rows: 2 },
                        _ => Stage::Cols { area },
                    };
                    Ok(Step::Continue)
                }
                (first, _) => {
                    self.stage = Stage::Corner { layout, first };
                    Ok(Step::Continue)
                }
            },
            Stage::Cols { area } => match num_or(&i, 1.0).filter(|n| n.fract() == 0.0 && (1.0..=MAX_ARRAY as f64).contains(n)) {
                Some(n) => {
                    self.stage = Stage::Rows { area, cols: n as usize };
                    Ok(Step::Continue)
                }
                None => again(self, Stage::Cols { area }, "Enter a whole number from 1 to 16."),
            },
            Stage::Rows { area, cols } => match num_or(&i, 1.0).filter(|n| n.fract() == 0.0 && (1.0..=MAX_ARRAY as f64).contains(n)) {
                Some(n) if cols * n as usize == 1 => {
                    Self::create(s, 3, area, 1, 1, 0.0, 0.0)?;
                    self.stage = Stage::Main;
                    Ok(Step::Continue)
                }
                Some(n) => {
                    self.stage = Stage::GapX { layout: 3, area, cols, rows: n as usize };
                    Ok(Step::Continue)
                }
                None => again(self, Stage::Rows { area, cols }, "Enter a whole number from 1 to 16."),
            },
            Stage::GapX { layout, area, cols, rows } => match num_or(&i, 0.0).filter(|v| *v >= 0.0) {
                Some(dx) => {
                    self.stage = Stage::GapY { layout, area, cols, rows, dx };
                    Ok(Step::Continue)
                }
                None => again(self, Stage::GapX { layout, area, cols, rows }, "Requires a distance of zero or more."),
            },
            Stage::GapY { layout, area, cols, rows, dx } => match num_or(&i, 0.0).filter(|v| *v >= 0.0) {
                Some(dy) => {
                    if let Err(e) = Self::create(s, layout, area, cols, rows, dx, dy) {
                        self.stage = Stage::Main;
                        return Err(e);
                    }
                    self.stage = Stage::Main;
                    Ok(Step::Continue)
                }
                None => again(self, Stage::GapY { layout, area, cols, rows, dx }, "Requires a distance of zero or more."),
            },
            Stage::ScaleSelect(mut sel) => match sel.feed(s, &i)? {
                SelOutcome::More => {
                    self.stage = Stage::ScaleSelect(sel);
                    Ok(Step::Continue)
                }
                SelOutcome::Empty => {
                    self.stage = Stage::Main;
                    Ok(Step::Continue)
                }
                SelOutcome::Done(hs) => {
                    let vps: Vec<Handle> = layout_viewports(s)?.into_iter().map(|(h, _)| h).filter(|h| hs.contains(h)).collect();
                    if vps.is_empty() {
                        s.echo("No viewports selected.");
                        self.stage = Stage::Main;
                    } else {
                        self.stage = Stage::ScaleMode(vps);
                    }
                    Ok(Step::Continue)
                }
            },
            Stage::ScaleMode(vps) => match (matches!(i, Input::Enter), word(&i).as_deref()) {
                (true, _) | (_, Some("uniform" | "u")) => {
                    self.stage = Stage::PaperUnits(vps);
                    Ok(Step::Continue)
                }
                _ => again(self, Stage::ScaleMode(vps), "Requires Uniform."),
            },
            Stage::PaperUnits(vps) => match num_or(&i, 1.0).filter(|v| *v > 0.0) {
                Some(v) => {
                    self.stage = Stage::ModelUnits(vps, v);
                    Ok(Step::Continue)
                }
                None => again(self, Stage::PaperUnits(vps), "Requires a positive number."),
            },
            Stage::ModelUnits(vps, paper) => match num_or(&i, 1.0).filter(|v| *v > 0.0) {
                Some(model) => {
                    let hs: Vec<String> = vps.iter().map(|h| h.hex()).collect();
                    run_mvsetup(s, &json!({ "option": "scale", "handles": hs, "paper": paper, "model": model })).map_err(retry)?;
                    self.stage = Stage::Main;
                    Ok(Step::Continue)
                }
                None => again(self, Stage::ModelUnits(vps, paper), "Requires a positive number."),
            },
            Stage::Align => match word(&i).as_deref() {
                _ if matches!(i, Input::Enter) => {
                    self.stage = Stage::Main;
                    Ok(Step::Continue)
                }
                Some("horizontal" | "h") => {
                    self.stage = Stage::AlignBase { horizontal: true };
                    Ok(Step::Continue)
                }
                Some("vertical alignment" | "v") => {
                    self.stage = Stage::AlignBase { horizontal: false };
                    Ok(Step::Continue)
                }
                _ => again(self, Stage::Align, "Requires an option keyword."),
            },
            Stage::AlignBase { horizontal } => match i {
                Input::Point(base) => {
                    self.stage = Stage::AlignPoint { horizontal, base };
                    Ok(Step::Continue)
                }
                Input::Enter => {
                    self.stage = Stage::Align;
                    Ok(Step::Continue)
                }
                _ => again(self, Stage::AlignBase { horizontal }, "Requires a point."),
            },
            Stage::AlignPoint { horizontal, base } => match i {
                Input::Point(at) => {
                    let mode = if horizontal { "horizontal" } else { "vertical" };
                    if let Err(e) = run_mvsetup(s, &json!({ "option": "align", "mode": mode, "base": [base.x, base.y], "point": [at.x, at.y] })) {
                        self.stage = Stage::AlignPoint { horizontal, base };
                        return Err(retry(e));
                    }
                    self.stage = Stage::Align;
                    Ok(Step::Continue)
                }
                _ => again(self, Stage::AlignPoint { horizontal, base }, "Requires a point."),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewports(s: &Session) -> Vec<cadcraft_doc::Viewport> {
        layout_viewports(s).unwrap().into_iter().map(|(_, v)| v).collect()
    }

    fn on_layout() -> Session {
        let mut s = Session::new();
        s.execute("circle", &json!({ "center": [0, 0], "radius": 10 })).unwrap();
        s.execute("layout", &json!({ "option": "set", "name": "Layout1" })).unwrap();
        // Start from an empty sheet.
        let hs: Vec<String> = layout_viewports(&s).unwrap().iter().map(|(h, _)| h.hex()).collect();
        s.execute("mvsetup", &json!({ "option": "delete", "handles": hs })).unwrap();
        assert!(viewports(&s).is_empty());
        s
    }

    #[test]
    fn create_arranges_viewports_in_the_bounding_area() {
        let mut s = on_layout();
        let r = s
            .execute("mvsetup", &json!({ "option": "create", "arrangement": "standard", "p1": [0, 0], "p2": [210, 110], "dx": 10, "dy": 10 }))
            .unwrap();
        assert_eq!(r["viewports"].as_array().unwrap().len(), 4);
        let vs = viewports(&s);
        assert!(vs.iter().all(|v| (v.width - 100.0).abs() < 1e-9 && (v.height - 50.0).abs() < 1e-9));
        // Top left first, then top right.
        assert!((vs[0].center.x - 50.0).abs() < 1e-9 && (vs[0].center.y - 85.0).abs() < 1e-9);
        assert!((vs[1].center.x - 160.0).abs() < 1e-9);
        let r =
            s.execute("mvsetup", &json!({ "option": "create", "arrangement": 3, "p1": [0, 200], "p2": [90, 230], "cols": 3, "rows": 1 })).unwrap();
        assert_eq!(r["viewports"].as_array().unwrap().len(), 3);
        for bad in [
            json!({ "option": "create", "arrangement": 3, "cols": 99 }),
            json!({ "option": "create", "arrangement": "standard", "p1": [0, 0], "p2": [10, 10], "dx": 20 }),
            json!({ "option": "create", "arrangement": "weird" }),
            json!({ "option": "nothing" }),
            json!({ "option": "scale", "paper": -1 }),
            json!({ "option": "align", "mode": "horizontal", "base": [0, 0], "point": [-999, -999] }),
        ] {
            assert!(s.execute("mvsetup", &bad).is_err(), "{bad}");
        }
        // Not in the Model tab without a layout.
        let mut m = Session::new();
        assert!(m.execute("mvsetup", &json!({ "option": "create" })).is_err());
    }

    #[test]
    fn scale_and_align_set_the_viewport_views() {
        let mut s = on_layout();
        s.execute("mvsetup", &json!({ "option": "create", "arrangement": "array", "p1": [0, 0], "p2": [200, 100], "cols": 2, "rows": 1, "dx": 0 }))
            .unwrap();
        s.execute("mvsetup", &json!({ "option": "scale", "paper": 1, "model": 2 })).unwrap();
        assert!(viewports(&s).iter().all(|v| (v.view_height - 200.0).abs() < 1e-9));
        s.execute("mvsetup", &json!({ "option": "scale", "scale": "1:4" })).unwrap();
        assert!(viewports(&s).iter().all(|v| (v.view_height - 400.0).abs() < 1e-9));
        // Pan the right viewport so its point (150, 40) lines up with (50, 60): up by 20 on paper.
        let before = viewports(&s)[1].view_center;
        s.execute("mvsetup", &json!({ "option": "align", "mode": "horizontal", "base": [50, 60], "point": [150, 40] })).unwrap();
        let after = viewports(&s)[1].view_center;
        assert!((after.y - (before.y - 20.0 * 4.0)).abs() < 1e-9 && (after.x - before.x).abs() < 1e-12, "{before:?} → {after:?}");
    }

    #[test]
    fn typed_mvsetup_creates_and_scales() {
        let mut s = on_layout();
        for line in ["mvsetup", "c", "", "3", "0,0", "200,100", "2", "2", "10", "10", "s"] {
            s.cmdline(line).unwrap();
        }
        assert_eq!(viewports(&s).len(), 4);
        let all: Vec<Handle> = layout_viewports(&s).unwrap().into_iter().map(|(h, _)| h).collect();
        s.input(Input::Pick(all)).unwrap();
        for line in ["", "", "1", "10", ""] {
            s.cmdline(line).unwrap();
        }
        assert!(s.running.is_none(), "Enter at the main prompt ends MVSETUP");
        let vs = viewports(&s);
        assert!(vs.iter().all(|v| (v.height / v.view_height - 0.1).abs() < 1e-9), "{:?}", vs.iter().map(|v| v.view_height).collect::<Vec<_>>());
        // One undo step takes the whole setup back.
        s.execute("undo", &json!({})).unwrap();
        assert!(viewports(&s).is_empty());
        // From the Model tab it offers paper space first.
        s.execute("layout", &json!({ "option": "set", "name": "Model" })).ok();
        s.state_mut().unwrap().space = Space::Model;
        for line in ["mvsetup", "y", "c", "", "1", "0,0", "100,50", ""] {
            s.cmdline(line).unwrap();
        }
        assert_eq!(viewports(&s).len(), 1);
    }
}
