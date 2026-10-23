//! XCLIP: clip block and xref references to a rectangle or a polygon. New boundary, ON, OFF,
//! Clipdepth, Delete and generate Polyline. The clip is kept in each reference, in the block's
//! own coordinates ([`cadcraft_doc::xclip`]), so it moves, rotates, scales and copies with it;
//! display, picking and extents follow it, and DXF files carry it as a SPATIAL_FILTER.

use cadcraft_doc::xclip::{MAX_VERTICES, XClip};
use cadcraft_doc::{Drawing, Entity, EntityKind, Handle};
use cadcraft_geom::{Mat3, PolyVertex, Vec2};
use serde_json::{Value, json};

use crate::cmd::helpers::{lwpoly, rect_vertices};
use crate::cmd::machines::{SelOutcome, SelectPhase};
use crate::cmd::{CommandSpec, bad, curves, points_param, str_param, targets};
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("xclip", "Clip Xref", run_xclip)
            .menu(&["Modify", "Clip", "Xref"])
            .alias(&["xc"])
            .params(
                "{handles? (block or xref references; default: the selection), option?: new|on|off|clipdepth|delete|polyline (default new), \
                 points?: [[x,y]…] (new: two = a rectangle's opposite corners, three or more = a polygon), boundary?: a closed polyline's handle (new), \
                 front?, back? (clipdepth: clipping plane distances from the boundary), remove?: bool (clipdepth: no clipping planes)} \
                 → {handles, polylines? (generate Polyline)}",
            )
            .interactive(|_| Ok(Box::new(XclipM::default()))),
    ]
}

/// A reference's block-to-plan transform: block coordinates to the world XY seen from above.
fn block_xf(d: &Drawing, e: &Entity) -> Option<Mat3> {
    let EntityKind::Insert(ins) = &e.kind else { return None };
    let base = d.block(&ins.block).map(|b| b.base.xy()).unwrap_or(Vec2::ZERO);
    let m = ins.transform(base);
    Some(e.plan_xf().map_or(m, |a| a.then_before(m)))
}

/// The references among `hs`.
/// The block references among `hs` not on a locked layer.
fn references(s: &Session, hs: &[Handle]) -> Vec<Handle> {
    let Ok(d) = s.doc() else { return Vec::new() };
    hs.iter()
        .copied()
        .filter(|h| d.entity(*h).is_some_and(|e| matches!(e.kind, EntityKind::Insert(_))) && !crate::cmd::curves::is_locked(s, *h))
        .collect()
}

/// The world boundary a New boundary call gives: two corners (a rectangle) or a polygon.
fn new_boundary(cmd: &str, d: &Drawing, p: &Value) -> Result<Vec<Vec2>> {
    if let Some(pts) = points_param(p, "points") {
        let pts = crate::cmd::distinct_points(&pts);
        if pts.len() > MAX_VERTICES {
            return Err(bad(cmd, format!("at most {MAX_VERTICES} boundary points")));
        }
        return match pts.as_slice() {
            [a, b] if (a.x - b.x).abs() > 1e-9 && (a.y - b.y).abs() > 1e-9 => Ok(vec![*a, *b]),
            [_, _] => Err(bad(cmd, "the rectangle has no area")),
            vs if vs.len() >= 3 => Ok(vs.to_vec()),
            _ => Err(bad(cmd, "`points`: two corners or at least three polygon points")),
        };
    }
    let h = p.get("boundary").and_then(|v| v.as_str().and_then(Handle::parse_hex).or_else(|| v.as_u64().map(Handle)));
    let Some(h) = h else { return Err(bad(cmd, "`points` or `boundary` (a closed polyline) is required")) };
    let e = d.entity(h).ok_or_else(|| bad(cmd, format!("no object {}", h.hex())))?;
    if !matches!(e.kind, EntityKind::LwPolyline(_) | EntityKind::Polyline3d(_)) {
        return Err(bad(cmd, "the boundary must be a closed polyline"));
    }
    let vs = cadcraft_render::clip::boundary_polygon(&e.kind).ok_or_else(|| bad(cmd, "the boundary must be a closed polyline"))?;
    let vs: Vec<Vec2> = match e.plan_xf() {
        Some(a) => vs.iter().map(|v| a.apply(*v)).collect(),
        None => vs,
    };
    if vs.len() < 3 || vs.len() > MAX_VERTICES {
        return Err(bad(cmd, "the boundary must be a closed polyline"));
    }
    Ok(vs)
}

/// `world` in the block coordinates of a reference placed by `xf`. A rectangle stays two corners
/// while the reference is not rotated; otherwise it becomes its four corners.
fn to_block(world: &[Vec2], xf: &Mat3) -> Option<Vec<Vec2>> {
    let inv = xf.inverse()?;
    let square = xf.b.abs() < 1e-12 && xf.c.abs() < 1e-12;
    let pts: Vec<Vec2> = match world {
        [a, b] if !square => vec![*a, Vec2::new(b.x, a.y), *b, Vec2::new(a.x, b.y)],
        vs => vs.to_vec(),
    };
    let out: Vec<Vec2> = pts.iter().map(|p| inv.apply(*p)).collect();
    out.iter().all(|p| p.is_finite()).then_some(out)
}

fn set_clip(s: &mut Session, h: Handle, clip: Option<&XClip>) -> Result<()> {
    s.doc_mut()?.modify_entity(h, |e| XClip::put(&mut e.common, clip)).map_err(|e| EngineError::Other(e.to_string()))
}

fn hex(hs: &[Handle]) -> Vec<String> {
    hs.iter().map(|h| h.hex()).collect()
}

fn run_xclip(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "xclip";
    let refs = references(s, &targets(s, p)?);
    let d = s.doc()?;
    if refs.is_empty() {
        return Err(bad(cmd, "no block reference given (pass `handles` or select references)"));
    }
    let clips: Vec<(Handle, Option<XClip>)> = refs.iter().map(|h| (*h, d.entity(*h).and_then(|e| XClip::of(&e.common)))).collect();
    let option = str_param(p, "option").unwrap_or("new").trim().to_ascii_lowercase();
    let clipped: Vec<(Handle, XClip)> = clips.iter().filter_map(|(h, c)| Some((*h, c.clone()?))).collect();
    let need_clip = || if clipped.is_empty() { Err(bad(cmd, "the references have no clip boundary")) } else { Ok(()) };
    match option.as_str() {
        "new" | "n" | "new boundary" => {
            let world = new_boundary(cmd, d, p)?;
            let mut out = Vec::new();
            for h in &refs {
                let Some(xf) = d.entity(*h).and_then(|e| block_xf(d, e)) else { continue };
                let Some(boundary) = to_block(&world, &xf) else { continue };
                out.push((*h, XClip { boundary, on: true, front: None, back: None }));
            }
            if out.is_empty() {
                return Err(bad(cmd, "the references can't be clipped (zero scale)"));
            }
            for (h, x) in &out {
                set_clip(s, *h, Some(x))?;
            }
            let hs: Vec<Handle> = out.iter().map(|(h, _)| *h).collect();
            Ok(json!({ "handles": hex(&hs) }))
        }
        "on" | "off" => {
            need_clip()?;
            for (h, x) in &clipped {
                set_clip(s, *h, Some(&XClip { on: option == "on", ..x.clone() }))?;
            }
            Ok(json!({ "handles": hex(&clipped.iter().map(|(h, _)| *h).collect::<Vec<_>>()) }))
        }
        "clipdepth" | "c" => {
            need_clip()?;
            let dist = |k: &str| -> Result<Option<f64>> {
                match p.get(k) {
                    None | Some(Value::Null) => Ok(None),
                    Some(v) => {
                        v.as_f64().filter(|x| x.is_finite() && x.abs() < 1e12).map(Some).ok_or_else(|| bad(cmd, format!("`{k}` must be a number")))
                    }
                }
            };
            let remove = p.get("remove").and_then(Value::as_bool).unwrap_or(false);
            let (front, back) = (dist("front")?, dist("back")?);
            if !remove && front.is_none() && back.is_none() {
                return Err(bad(cmd, "`front`, `back` or `remove` is required"));
            }
            for (h, x) in &clipped {
                let x = if remove {
                    XClip { front: None, back: None, ..x.clone() }
                } else {
                    XClip { front: front.or(x.front), back: back.or(x.back), ..x.clone() }
                };
                set_clip(s, *h, Some(&x))?;
            }
            Ok(json!({ "handles": hex(&clipped.iter().map(|(h, _)| *h).collect::<Vec<_>>()) }))
        }
        "delete" | "d" => {
            need_clip()?;
            for (h, _) in &clipped {
                set_clip(s, *h, None)?;
            }
            Ok(json!({ "handles": hex(&clipped.iter().map(|(h, _)| *h).collect::<Vec<_>>()) }))
        }
        "polyline" | "p" | "generate polyline" => {
            need_clip()?;
            let common = s.current_common();
            let mut made = Vec::new();
            for (h, x) in &clipped {
                let d = s.doc()?;
                let (Some(e), Some(space)) = (d.entity(*h), d.space_of(*h)) else { continue };
                let Some(xf) = block_xf(d, e) else { continue };
                let vs: Vec<PolyVertex> = x.polygon().iter().map(|v| PolyVertex::new(xf.apply(*v))).collect();
                made.push(s.doc_mut()?.add(&space, common.clone(), lwpoly(vs, true)).map_err(|e| EngineError::Other(e.to_string()))?);
            }
            Ok(json!({ "handles": hex(&clipped.iter().map(|(h, _)| *h).collect::<Vec<_>>()), "polylines": hex(&made) }))
        }
        o => Err(bad(cmd, format!("unknown option `{o}` (new, on, off, clipdepth, delete or polyline)"))),
    }
}

/// Where XCLIP typed at the command line is.
#[derive(Default)]
enum Stage {
    #[default]
    Select,
    Option,
    /// "Delete old boundary(s)?" before a new one replaces them.
    DeleteOld,
    Kind,
    Corner(Option<Vec2>),
    Polygon(Vec<Vec2>),
    Polyline,
    /// Clipdepth: the front plane, then the back plane (`Some(front)` when given).
    Front {
        typing: bool,
    },
    Back {
        front: Option<f64>,
        typing: bool,
    },
}

/// XCLIP typed at the command line.
#[derive(Default)]
pub(super) struct XclipM {
    sel: SelectPhase,
    refs: Vec<Handle>,
    stage: Stage,
}

fn keyword(i: &Input) -> Option<String> {
    match i {
        Input::Keyword(k) | Input::Text(k) => Some(k.trim().to_ascii_lowercase()),
        _ => None,
    }
}

fn number(i: &Input) -> Option<f64> {
    match i {
        Input::Text(t) => t.trim().parse::<f64>().ok().filter(|v| v.is_finite()),
        _ => None,
    }
}

impl XclipM {
    fn run(&self, s: &mut Session, p: Value) -> Result<Value> {
        let mut p = p;
        if let Some(o) = p.as_object_mut() {
            o.insert("handles".into(), json!(hex(&self.refs)));
        }
        run_xclip(s, &p)
    }
    fn any_clipped(&self, s: &Session) -> bool {
        s.doc().is_ok_and(|d| self.refs.iter().any(|h| d.entity(*h).is_some_and(|e| XClip::of(&e.common).is_some())))
    }
    fn picked(&mut self, s: &mut Session, hs: Vec<Handle>) -> Step {
        self.refs = references(s, &hs);
        if self.refs.is_empty() {
            s.echo("No block references selected.");
            return Step::Done;
        }
        self.stage = Stage::Option;
        Step::Continue
    }
}

impl Interactive for XclipM {
    fn name(&self) -> &'static str {
        "XCLIP"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            let hs = self.sel.picked.clone();
            return Ok(self.picked(s, hs));
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match &self.stage {
            Stage::Select => self.sel.prompt(),
            Stage::Option => Prompt::new("Enter clipping option", Accept::WORD)
                .kw(&["ON", "OFF", "Clipdepth", "Delete", "generate Polyline", "New boundary"])
                .default("New boundary"),
            Stage::DeleteOld => Prompt::new("Delete old boundary(s)?", Accept::WORD).kw(&["Yes", "No"]).default("Yes"),
            Stage::Kind => {
                Prompt::new("Specify clipping boundary", Accept::WORD).kw(&["Select polyline", "Polygonal", "Rectangular"]).default("Rectangular")
            }
            Stage::Corner(None) => Prompt::new("Specify first corner", Accept::POINT),
            Stage::Corner(Some(a)) => Prompt::new("Specify opposite corner", Accept::POINT).corner(*a),
            Stage::Polygon(vs) if vs.is_empty() => Prompt::new("Specify first point", Accept::POINT),
            Stage::Polygon(vs) => Prompt::new("Specify next point", Accept::POINT).kw(&["Undo"]).base_opt(vs.last().copied()),
            Stage::Polyline => Prompt::new("Select polyline", Accept::POINT),
            Stage::Front { typing: true } | Stage::Back { typing: true, .. } => Prompt::new("Specify distance from boundary", Accept::NUMBER),
            Stage::Front { .. } => Prompt::new("Specify front clip distance", Accept::WORD).kw(&["Distance", "Remove"]),
            Stage::Back { .. } => Prompt::new("Specify back clip distance", Accept::WORD).kw(&["Distance", "Remove"]),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match std::mem::take(&mut self.stage) {
            Stage::Select => match self.sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Done),
                SelOutcome::Done(hs) => Ok(self.picked(s, hs)),
            },
            Stage::Option => {
                let k = if matches!(i, Input::Enter) { Some("new boundary".to_string()) } else { keyword(&i) };
                match k.as_deref() {
                    Some("on" | "off") => {
                        let on = k.as_deref() == Some("on");
                        self.run(s, json!({ "option": if on { "on" } else { "off" } }))?;
                        Ok(Step::Done)
                    }
                    Some("clipdepth" | "c") => {
                        if !self.any_clipped(s) {
                            s.echo("No clip boundary to set clipping planes on.");
                            return Ok(Step::Done);
                        }
                        self.stage = Stage::Front { typing: false };
                        Ok(Step::Continue)
                    }
                    Some("delete" | "d") => {
                        if self.any_clipped(s) {
                            self.run(s, json!({ "option": "delete" }))?;
                        } else {
                            s.echo("No clip boundary to delete.");
                        }
                        Ok(Step::Done)
                    }
                    Some("generate polyline" | "p") => {
                        if self.any_clipped(s) {
                            self.run(s, json!({ "option": "polyline" }))?;
                        } else {
                            s.echo("No clip boundary to trace.");
                        }
                        Ok(Step::Done)
                    }
                    Some("new boundary" | "n" | "new") => {
                        self.stage = if self.any_clipped(s) { Stage::DeleteOld } else { Stage::Kind };
                        Ok(Step::Continue)
                    }
                    _ => {
                        self.stage = Stage::Option;
                        Err(EngineError::Other("Requires an option keyword.".into()))
                    }
                }
            }
            Stage::DeleteOld => match (matches!(i, Input::Enter), keyword(&i).as_deref()) {
                (true, _) | (_, Some("yes" | "y")) => {
                    self.stage = Stage::Kind;
                    Ok(Step::Continue)
                }
                (_, Some("no" | "n")) => Ok(Step::Done),
                _ => {
                    self.stage = Stage::DeleteOld;
                    Err(EngineError::Other("Requires Yes or No.".into()))
                }
            },
            Stage::Kind => {
                let k = if matches!(i, Input::Enter) { Some("rectangular".to_string()) } else { keyword(&i) };
                self.stage = match k.as_deref() {
                    Some("rectangular" | "r") => Stage::Corner(None),
                    Some("polygonal" | "p") => Stage::Polygon(Vec::new()),
                    Some("select polyline" | "s") => Stage::Polyline,
                    _ => {
                        self.stage = Stage::Kind;
                        return Err(EngineError::Other("Requires an option keyword.".into()));
                    }
                };
                Ok(Step::Continue)
            }
            Stage::Corner(first) => match (first, i) {
                (None, Input::Point(a)) => {
                    self.stage = Stage::Corner(Some(a));
                    Ok(Step::Continue)
                }
                (Some(a), Input::Point(b)) => match self.run(s, json!({ "option": "new", "points": [[a.x, a.y], [b.x, b.y]] })) {
                    Ok(_) => Ok(Step::Done),
                    Err(e) => {
                        self.stage = Stage::Corner(Some(a));
                        Err(e)
                    }
                },
                (first, _) => {
                    self.stage = Stage::Corner(first);
                    Ok(Step::Continue)
                }
            },
            Stage::Polygon(mut vs) => match i {
                Input::Point(q) => {
                    if !vs.last().is_some_and(|l| l.near(q, 1e-12)) && vs.len() < MAX_VERTICES {
                        vs.push(q);
                    }
                    self.stage = Stage::Polygon(vs);
                    Ok(Step::Continue)
                }
                Input::Keyword(_) | Input::Text(_) if matches!(keyword(&i).as_deref(), Some("undo" | "u")) => {
                    vs.pop();
                    self.stage = Stage::Polygon(vs);
                    Ok(Step::Continue)
                }
                Input::Enter if vs.len() >= 3 => {
                    let pts: Vec<[f64; 2]> = vs.iter().map(|v| [v.x, v.y]).collect();
                    self.run(s, json!({ "option": "new", "points": pts }))?;
                    Ok(Step::Done)
                }
                Input::Enter => {
                    self.stage = Stage::Polygon(vs);
                    Err(EngineError::Other("A polygonal boundary needs at least three points.".into()))
                }
                _ => {
                    self.stage = Stage::Polygon(vs);
                    Ok(Step::Continue)
                }
            },
            Stage::Polyline => {
                let h = match &i {
                    Input::Point(p) => curves::pick_at(s, *p),
                    Input::Pick(hs) => hs.first().copied(),
                    Input::Enter => return Ok(Step::Done),
                    _ => None,
                };
                let Some(h) = h else {
                    self.stage = Stage::Polyline;
                    s.echo("No object found.");
                    return Ok(Step::Continue);
                };
                match self.run(s, json!({ "option": "new", "boundary": h.hex() })) {
                    Ok(_) => Ok(Step::Done),
                    Err(e) => {
                        self.stage = Stage::Polyline;
                        Err(e)
                    }
                }
            }
            Stage::Front { typing } => {
                if let Some(v) = number(&i) {
                    self.stage = Stage::Back { front: Some(v), typing: false };
                    return Ok(Step::Continue);
                }
                match (i, typing) {
                    (Input::Enter, false) => self.stage = Stage::Back { front: None, typing: false },
                    (i, false) if matches!(keyword(&i).as_deref(), Some("remove" | "r")) => {
                        self.run(s, json!({ "option": "clipdepth", "remove": true }))?;
                        return Ok(Step::Done);
                    }
                    (i, false) if matches!(keyword(&i).as_deref(), Some("distance" | "d")) => self.stage = Stage::Front { typing: true },
                    (_, typing) => {
                        self.stage = Stage::Front { typing };
                        return Err(EngineError::Other("Requires a distance or Remove.".into()));
                    }
                }
                Ok(Step::Continue)
            }
            Stage::Back { front, typing } => {
                let back = match (number(&i), &i, typing) {
                    (Some(v), _, _) => Some(v),
                    (None, Input::Enter, false) => None,
                    (None, i, false) if matches!(keyword(i).as_deref(), Some("remove" | "r")) => {
                        self.run(s, json!({ "option": "clipdepth", "remove": true }))?;
                        return Ok(Step::Done);
                    }
                    (None, i, false) if matches!(keyword(i).as_deref(), Some("distance" | "d")) => {
                        self.stage = Stage::Back { front, typing: true };
                        return Ok(Step::Continue);
                    }
                    _ => {
                        self.stage = Stage::Back { front, typing };
                        return Err(EngineError::Other("Requires a distance or Remove.".into()));
                    }
                };
                if front.is_some() || back.is_some() {
                    self.run(s, json!({ "option": "clipdepth", "front": front, "back": back }))?;
                }
                Ok(Step::Done)
            }
        }
    }
    fn preview(&self, _s: &Session, c: Vec2) -> Vec<EntityKind> {
        match &self.stage {
            Stage::Corner(Some(a)) => vec![lwpoly(rect_vertices(*a, c), true)],
            Stage::Polygon(vs) if !vs.is_empty() => {
                let mut pts: Vec<PolyVertex> = vs.iter().map(|v| PolyVertex::new(*v)).collect();
                pts.push(PolyVertex::new(c));
                vec![lwpoly(pts, true)]
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::Space;
    use cadcraft_geom::Bounds2;

    use super::*;

    /// A 100 × 20 block of two lines, inserted at (10, 10) twice the size.
    fn session() -> (Session, Handle) {
        let mut s = Session::new();
        s.execute("line", &json!({ "points": [[0, 0], [100, 0]] })).unwrap();
        s.execute("line", &json!({ "points": [[0, 20], [100, 20]] })).unwrap();
        s.execute("block", &json!({ "name": "Plan", "base": [0, 0], "handles": hex(&s.doc().unwrap().model.iter().map(|e| e.handle).collect::<Vec<_>>()), "keep": "delete" }))
            .unwrap();
        s.execute("insert", &json!({ "name": "Plan", "at": [10, 10], "scale": 2 })).unwrap();
        let h = s.doc().unwrap().model.iter().last().unwrap().handle;
        (s, h)
    }

    fn extents(s: &Session) -> Bounds2 {
        s.doc().unwrap().extents(&Space::Model)
    }

    #[test]
    fn a_rectangle_clips_display_picking_and_extents() {
        let (mut s, h) = session();
        assert!((extents(&s).max.x - 210.0).abs() < 1e-6);
        s.execute("xclip", &json!({ "handles": [h.hex()], "points": [[0, 0], [60, 60]] })).unwrap();
        let x = XClip::of(&s.doc().unwrap().entity(h).unwrap().common).unwrap();
        // World (0,0)-(60,60) in block coordinates of a reference at (10,10) scaled 2.
        assert_eq!(x.boundary, vec![Vec2::new(-5.0, -5.0), Vec2::new(25.0, 25.0)]);
        let ext = extents(&s);
        assert!((ext.max.x - 60.0).abs() < 1e-6 && (ext.min.x - 10.0).abs() < 1e-6, "{ext:?}");
        // The part clipped away is not drawn, so it can't be picked.
        let d = s.doc().unwrap();
        assert_eq!(crate::select::pick(d, &Space::Model, Vec2::new(30.0, 10.0), 0.5), Some(h));
        assert_eq!(crate::select::pick(d, &Space::Model, Vec2::new(150.0, 10.0), 0.5), None);
        let list = cadcraft_render::build(d, &Space::Model, &cadcraft_render::Options::default());
        assert!(list.bounds.max.x <= 60.0 + 1e-6, "{:?}", list.bounds);
        // XCLIPFRAME 2 (the default) draws the boundary on screen; 0 hides it.
        assert!(list.bounds.min.y.abs() < 1e-6 && (list.bounds.max.y - 60.0).abs() < 1e-6, "{:?}", list.bounds);
        s.execute("setvar", &json!({ "name": "XCLIPFRAME", "value": 0 })).unwrap();
        let list = cadcraft_render::build(s.doc().unwrap(), &Space::Model, &cadcraft_render::Options::default());
        assert!((list.bounds.min.y - 10.0).abs() < 1e-6 && (list.bounds.max.y - 50.0).abs() < 1e-6, "{:?}", list.bounds);
        let d = s.doc().unwrap();
        assert!(cadcraft_render::build_plot(d, &Space::Model, &cadcraft_render::Options::default()).bounds.max.y <= 50.0 + 1e-6);
        // OFF shows the whole reference again; ON clips it; Delete removes the boundary.
        s.execute("xclip", &json!({ "handles": [h.hex()], "option": "off" })).unwrap();
        assert!((extents(&s).max.x - 210.0).abs() < 1e-6);
        s.execute("xclip", &json!({ "handles": [h.hex()], "option": "on" })).unwrap();
        assert!((extents(&s).max.x - 60.0).abs() < 1e-6);
        s.execute("xclip", &json!({ "handles": [h.hex()], "option": "clipdepth", "front": 5, "back": -2 })).unwrap();
        let x = XClip::of(&s.doc().unwrap().entity(h).unwrap().common).unwrap();
        assert_eq!((x.front, x.back), (Some(5.0), Some(-2.0)));
        // Generate Polyline traces the boundary in world coordinates.
        let r = s.execute("xclip", &json!({ "handles": [h.hex()], "option": "polyline" })).unwrap();
        let ph = Handle::parse_hex(r["polylines"][0].as_str().unwrap()).unwrap();
        let pb = cadcraft_doc::entity_bounds(s.doc().unwrap(), s.doc().unwrap().entity(ph).unwrap(), 0);
        assert!(pb.min.near(Vec2::ZERO, 1e-9) && pb.max.near(Vec2::new(60.0, 60.0), 1e-9), "{pb:?}");
        s.execute("xclip", &json!({ "handles": [h.hex()], "option": "delete" })).unwrap();
        assert_eq!(XClip::of(&s.doc().unwrap().entity(h).unwrap().common), None);
        // Undo brings the clip back.
        s.execute("undo", &json!({})).unwrap();
        assert!(XClip::of(&s.doc().unwrap().entity(h).unwrap().common).is_some());
    }

    #[test]
    fn a_reference_on_a_locked_layer_is_not_clipped() {
        let (mut s, h) = session();
        let layer = s.doc().unwrap().entity(h).unwrap().common.layer.clone();
        s.execute("layer.set", &json!({ "name": layer, "locked": true })).unwrap();
        assert!(s.execute("xclip", &json!({ "handles": [h.hex()], "points": [[0, 0], [60, 60]] })).is_err());
        assert!(XClip::of(&s.doc().unwrap().entity(h).unwrap().common).is_none());
    }

    #[test]
    fn the_clip_moves_and_copies_with_the_reference() {
        let (mut s, h) = session();
        s.execute("xclip", &json!({ "handles": [h.hex()], "points": [[0, 0], [60, 0], [60, 60]] })).unwrap();
        s.execute("move", &json!({ "handles": [h.hex()], "from": [0, 0], "to": [100, 0] })).unwrap();
        assert!((extents(&s).max.x - 160.0).abs() < 1e-6, "{:?}", extents(&s));
        let r = s.execute("copy", &json!({ "handles": [h.hex()], "from": [0, 0], "to": [0, 500] })).unwrap();
        let c = Handle::parse_hex(r["handles"][0].as_str().unwrap()).unwrap();
        assert!(XClip::of(&s.doc().unwrap().entity(c).unwrap().common).is_some());
    }

    #[test]
    fn typed_xclip_asks_like_autocad() {
        let (mut s, h) = session();
        s.set_selection(vec![h]);
        for line in ["xclip", "", "r", "0,0", "60,60"] {
            s.cmdline(line).unwrap();
        }
        assert!(s.running.is_none());
        assert!(XClip::of(&s.doc().unwrap().entity(h).unwrap().common).is_some());
        // A new boundary over an old one asks first; No keeps it.
        s.set_selection(vec![h]);
        for line in ["xclip", "n", "n"] {
            s.cmdline(line).unwrap();
        }
        assert!(s.running.is_none());
        s.set_selection(vec![h]);
        for line in ["xclip", "", "", "p", "0,0", "80,0", "80,80", ""] {
            s.cmdline(line).unwrap();
        }
        assert_eq!(XClip::of(&s.doc().unwrap().entity(h).unwrap().common).unwrap().boundary.len(), 3);
        s.set_selection(vec![h]);
        for line in ["xclip", "c", "4", "r"] {
            s.cmdline(line).unwrap();
        }
        let x = XClip::of(&s.doc().unwrap().entity(h).unwrap().common).unwrap();
        assert_eq!((x.front, x.back), (None, None));
        s.set_selection(vec![h]);
        for line in ["xclip", "off"] {
            s.cmdline(line).unwrap();
        }
        assert!(!XClip::of(&s.doc().unwrap().entity(h).unwrap().common).unwrap().on);
    }

    #[test]
    fn hostile_xclip_params_are_refused() {
        let (mut s, h) = session();
        for p in [
            json!({ "handles": [h.hex()], "points": [[0, 0], [0, 50]] }),
            json!({ "handles": [h.hex()], "points": [[0, 0]] }),
            json!({ "handles": [h.hex()], "points": "junk" }),
            json!({ "handles": [h.hex()], "boundary": "FFFFFF" }),
            json!({ "handles": [h.hex()], "option": "explode" }),
            json!({ "handles": [h.hex()], "option": "on" }),
            json!({ "handles": ["zz"] }),
            json!({ "handles": [] }),
        ] {
            assert!(s.execute("xclip", &p).is_err(), "{p}");
        }
        s.execute("xclip", &json!({ "handles": [h.hex()], "points": [[0, 0], [60, 60]] })).unwrap();
        assert!(s.execute("xclip", &json!({ "handles": [h.hex()], "option": "clipdepth", "front": "x" })).is_err());
        assert!(s.execute("xclip", &json!({ "handles": [h.hex()], "option": "clipdepth" })).is_err());
    }
}
