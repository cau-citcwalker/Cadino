//! Furniture design: panels, part names and materials, cut lists, and drawings made from the
//! 3D model (VIEWBASE).
//!
//! A PANEL is a board whose face is a profile drawn in plan (laid on the top, front or right
//! plane) or one already standing in a plane of its own (used where it is), and whose thickness
//! runs along the plane's normal; the profile stays (it is the drawing). PART names a solid and gives its
//! material; CUTLIST lists the parts by name, material and size; VIEWBASE projects the solids
//! into front, top, right and isometric views with hidden lines dashed ([`hlr`]), on a layout's
//! sheet (as AutoCAD's drawing views) or in model space.

mod hlr;

use std::collections::{BTreeMap, HashMap, HashSet};

use cadcraft_doc::{Common, Drawing, Entity, EntityKind, Handle, Layer, MAX_BLOCK_DEPTH, Part, Solid3d, Space};
use cadcraft_geom::{Bounds2, EPS, Mat3, Mat4, Mesh3, PolyVertex, Vec2, Vec3, ViewDir};
use cadcraft_kernel::{Placed, Seg};
use serde_json::{Value, json};

use super::curves::KW;
use super::machines::{SelOutcome, SelectPhase, number};
use super::solids::{all_or_nothing, distance, kernel, locked_err, normal_of, p3, profile, solid, solids};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

/// Board thicknesses offered at the prompt (millimetres).
const THICKNESSES: &[&str] = &["9", "12", "15", "18", "24"];
const DEFAULT_THICKNESS: f64 = 18.0;
/// The views VIEWBASE draws, by name, with the direction toward the viewer.
const VIEWS: &[(&str, [f64; 3])] = &[("front", [0.0, -1.0, 0.0]), ("top", [0.0, 0.0, 1.0]), ("right", [1.0, 0.0, 0.0]), ("iso", [1.0, -1.0, 1.0])];

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("panel", "Panel", run_panel)
            .menu(&["Draw", "Modeling", "Panel"])
            .params(
                "{handle (closed profile) | corner1: [x,y], corner2: [x,y], thickness? (default 18), orientation?: \"top\" | \"front\" | \"right\", \
                 base?: [x,y] (profile point at the plane's origin; default its lower-left), at?: [x,y,z] (where that point goes), name?, material?}: \
                 a profile in a tilted or standing plane is used where it is, its thickness along the plane's normal (no orientation, base or at); \
                 a profile's base and at are world points, corners and their at the construction plane's",
            )
            .interactive(|_| Ok(Box::new(PanelM::default()))),
        CommandSpec::new("part", "Part", run_part)
            .menu(&["Modify", "Part"])
            .params("{handles?, name?, material?}: names 3D solids as furniture parts (cut lists)")
            .interactive(|_| Ok(Box::new(PartM::default()))),
        CommandSpec::new("cutlist", "Cut List", run_cutlist)
            .menu(&["Tools", "Cut List"])
            .params(
                "{handles? (default every 3D solid in model space, on any layer; a block reference counts the solids in its block, each copy), \
                 at?: [x,y,z?] (insert a table there in the world; a long list goes on in more tables to the right), csv?: path (UTF-8 with BOM), group?: bool (default true)} \
                 → {rows: [{name, material, length, width, thickness, quantity}], total, handles?}: sizes along each part's own axes",
            )
            .interactive(|_| Ok(Box::new(PickThenPoint::new(PickFor::CutList)))),
        CommandSpec::new("viewbase", "Base View", run_viewbase)
            .menu(&["Draw", "Drawing Views", "Base View"])
            .alias(&["make2d"])
            .params(
                "{handles? (default every 3D solid on a layer that is on and thawed; block references draw the solids in their blocks), views?: [\"front\",\"top\",\"right\",\"iso\"] (default front, top, right), \
                 layout?: name (default \"Drawing views\", made when missing; the views go on its sheet), space?: \"paper\" | \"model\" (default paper; model draws them in model space), \
                 at?: [x,y] (front view's lower-left: on the sheet, default the views centred in its printable area; in model space default beside the model), \
                 scale?: model units per paper unit or \"1:10\" (default on a sheet the largest drawing of 1, 2 or 5 times a power of ten that fits; in model space 1), \
                 gap? (between views, drawn units; default a tenth of the drawn model's size, at least 15 mm on a sheet), hidden?: bool (default true; the iso view shows none), \
                 dims?: bool (default true: overall sizes reading model units, 2.5 mm text on a sheet), projection?: \"third\" | \"first\" (default third)} → {views, capped, layout, scale}",
            )
            .interactive(|_| Ok(Box::new(PickThenPoint::new(PickFor::Views)))),
    ]
}

// ---------------- PANEL ----------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plane {
    Top,
    Front,
    Right,
}

impl Plane {
    fn parse(t: &str) -> Option<Plane> {
        match t.trim().to_ascii_lowercase().as_str() {
            "top" | "t" => Some(Plane::Top),
            "front" | "f" => Some(Plane::Front),
            "right" | "r" => Some(Plane::Right),
            _ => None,
        }
    }
    /// A profile point on the plane.
    fn map(self, p: Vec2) -> Vec3 {
        match self {
            Plane::Top => Vec3::new(p.x, p.y, 0.0),
            Plane::Front => Vec3::new(p.x, 0.0, p.y),
            Plane::Right => Vec3::new(0.0, p.x, p.y),
        }
    }
    /// The direction the thickness runs.
    fn thickness_dir(self) -> Vec3 {
        match self {
            Plane::Top => Vec3::Z,
            Plane::Front => Vec3::new(0.0, 1.0, 0.0),
            Plane::Right => Vec3::new(-1.0, 0.0, 0.0),
        }
    }
}

/// An optional text parameter; anything but a string (or null) is an error.
fn text_param(cmd: &str, p: &Value, key: &str) -> Result<Option<String>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) => Ok(Some(t.trim().to_string())),
        Some(v) => Err(bad(cmd, format!("`{key}` must be text, not {v}"))),
    }
}

fn part_params(cmd: &str, p: &Value) -> Result<(Option<String>, Option<String>)> {
    Ok((text_param(cmd, p, "name")?, text_param(cmd, p, "material")?))
}

fn run_panel(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "panel";
    let thickness = size_param(CMD, p, "thickness", false)?.unwrap_or(DEFAULT_THICKNESS);
    let plane = match p.get("orientation") {
        None | Some(Value::Null) => Plane::Top,
        Some(v) => v.as_str().and_then(Plane::parse).ok_or_else(|| bad(CMD, format!("`orientation` is \"top\", \"front\" or \"right\", not {v}")))?,
    };
    let (name, material) = part_params(CMD, p)?;
    // The profile: an object's (in the world: its base and insertion point are world points too),
    // or a rectangle's (in the construction plane, as BOX's corners); with its lower-left corner.
    let world = p.get("handle").is_some_and(|v| !v.is_null());
    let (segs, lower_left, standing) = if world {
        object_profile(s, targets(s, p)?.first().copied().ok_or_else(|| bad(CMD, "`handle` is required"))?)?
    } else {
        let (segs, lo) = rect_profile(point_req(CMD, p, "corner1")?, point_req(CMD, p, "corner2")?)?;
        (segs, lo, None)
    };
    // A profile standing in a plane of its own is the panel's face where it is.
    if let Some(n) = standing {
        if ["orientation", "base", "at"].iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null())) {
            return Err(bad(CMD, "the profile stands in a plane of its own and is used where it is: leave out orientation, base and at"));
        }
        let built = cadcraft_kernel::extrude(&segs, n * thickness).map_err(kernel)?;
        return add_panel(s, built, name, material, true);
    }
    let base = match p.get("base").filter(|v| !v.is_null()) {
        Some(_) => point_req(CMD, p, "base")?,
        None => lower_left,
    };
    let at = match p.get("at").filter(|v| !v.is_null()) {
        Some(_) => p3(p, "at").ok_or_else(|| bad(CMD, "`at` must be [x, y] or [x, y, z]"))?,
        None => Vec3::ZERO,
    };
    if !base.is_finite() {
        return Err(bad(CMD, "the profile has no finite corner"));
    }
    // ponytail: the profile's x and y only (an elevation or a 3D polyline's heights are dropped):
    // it is drawn in plan.
    let to = |q: Vec3| plane.map(q.xy() - base) + at;
    let normal = plane.map(Vec2::new(1.0, 0.0)).cross(plane.map(Vec2::new(0.0, 1.0)));
    let placed: Vec<Seg> = segs
        .iter()
        .map(|g| match *g {
            Seg::Line { a, b } => Seg::Line { a: to(a), b: to(b) },
            Seg::Arc { a, mid, b } => Seg::Arc { a: to(a), mid: to(mid), b: to(b) },
            Seg::Circle { center, radius, .. } => Seg::Circle { center: to(center), radius, normal },
        })
        .collect();
    let finite = |q: &Vec3| q.is_finite();
    if !placed.iter().all(|g| match g {
        Seg::Line { a, b } => finite(a) && finite(b),
        Seg::Arc { a, mid, b } => finite(a) && finite(mid) && finite(b),
        Seg::Circle { center, .. } => finite(center),
    }) {
        return Err(bad(CMD, "the panel's corners are out of range"));
    }
    let built = cadcraft_kernel::extrude(&placed, plane.thickness_dir() * thickness).map_err(kernel)?;
    add_panel(s, built, name, material, world)
}

/// The panel added: made from corners, in the construction plane's coordinates (placed in it, as
/// a BOX is); made from a profile object, where that profile is in the world (`world`).
fn add_panel(s: &mut Session, built: cadcraft_kernel::Built, name: Option<String>, material: Option<String>, world: bool) -> Result<Value> {
    let mut k = solid(built);
    if let EntityKind::Solid3d(x) = &mut k {
        x.part = Part { name: name.unwrap_or_default(), material: material.unwrap_or_default() };
    }
    let h = if world { s.add_entity_world(k)? } else { s.add_entity(k)? };
    Ok(json!({ "handle": h.hex() }))
}

/// A closed profile object: its pieces where they are, its lower-left corner in plan, and the
/// normal of its plane when it stands in a plane of its own (tilted, standing or upside down, or a
/// 3D polyline that isn't level): the panel's thickness then runs along that normal from where
/// the profile is.
fn object_profile(s: &Session, h: Handle) -> Result<(Vec<Seg>, Vec2, Option<Vec3>)> {
    const CMD: &str = "panel";
    let d = s.doc()?;
    let e = d.entity(h).ok_or_else(|| bad(CMD, format!("no object with handle {}", h.hex())))?;
    let segs = profile(e, true)?.ok_or_else(|| bad(CMD, "the profile must be a closed polyline, circle or single-loop region"))?;
    let standing = match &e.kind {
        EntityKind::Polyline3d(pl) if pl.points.iter().any(|q| pl.points.first().is_some_and(|f| (q.z - f.z).abs() > EPS)) => {
            // Its winding's normal (Newell's method).
            let n = segs
                .iter()
                .fold(Vec3::ZERO, |n, g| match *g {
                    Seg::Line { a, b } | Seg::Arc { a, b, .. } => n + a.cross(b),
                    Seg::Circle { normal, .. } => n + normal,
                })
                .normalized();
            if n == Vec3::ZERO {
                return Err(bad(CMD, "the profile encloses no area"));
            }
            Some(n)
        }
        _ if e.plan_xf().is_some() => Some(normal_of(e)),
        _ => None,
    };
    Ok((segs, cadcraft_doc::entity_bounds(d, e, 0).min, standing))
}

/// The rectangle with corners `a` and `b`: its sides and its lower-left corner.
fn rect_profile(a: Vec2, b: Vec2) -> Result<(Vec<Seg>, Vec2)> {
    if (a.x - b.x).abs() < 1e-6 || (a.y - b.y).abs() < 1e-6 {
        return Err(bad("panel", "the corners must make a rectangle with some area"));
    }
    let (lo, hi) = (a.min(b), a.max(b));
    let c = [lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)];
    Ok(((0..4).filter_map(|i| Some(Seg::Line { a: c.get(i)?.to3(0.0), b: c.get((i + 1) % 4)?.to3(0.0) })).collect(), lo))
}

/// PANEL: a closed profile (Object) or two corners, the thickness, the orientation, the base point
/// and where it goes. A bad profile or corner is asked for again at once; when the panel can't be
/// made at the end, it starts again from the profile.
#[derive(Default)]
struct PanelM {
    corner: Option<Vec2>,
    picking: bool,
    vals: serde_json::Map<String, Value>,
}

impl PanelM {
    fn profile_given(&self) -> bool {
        self.vals.contains_key("handle") || self.vals.contains_key("corner2")
    }
    fn finish(&mut self, s: &mut Session) -> Result<Step> {
        let r = run_panel(s, &Value::Object(self.vals.clone()));
        if r.is_err() {
            // Start again: any value given may be the one that failed, so none is kept.
            *self = PanelM::default();
        }
        r.map(|_| Step::Done)
    }
}

impl Interactive for PanelM {
    fn name(&self) -> &'static str {
        "PANEL"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.picking {
            return Prompt::new("Select closed profile", Accept::POINT);
        }
        if !self.profile_given() {
            return match self.corner {
                Some(c) => Prompt::new("Specify other corner", Accept::POINT).corner(c),
                None => Prompt::new("Specify first corner", Accept::POINT).kw(&["Object"]).default("Object"),
            };
        }
        if !self.vals.contains_key("thickness") {
            return Prompt::new("Specify thickness", Accept::NUMBER).kw(THICKNESSES).default("18");
        }
        if !self.vals.contains_key("orientation") {
            return Prompt::new("Specify orientation", KW).kw(&["Top", "Front", "Right"]).default("Top");
        }
        if !self.vals.contains_key("base") {
            return Prompt::new("Specify base point on the profile", Accept::POINT).default("lower-left");
        }
        Prompt::new("Specify insertion point", Accept::POINT).default("0,0,0")
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.picking {
            let h = match &i {
                Input::Point(p) => super::curves::pick_at(s, *p),
                Input::Pick(hs) => hs.first().copied(),
                _ => return Ok(Step::Continue),
            };
            match h {
                Some(h) => {
                    if object_profile(s, h)?.2.is_some() {
                        // Standing in its own plane: used where it is, so only the thickness is asked.
                        for k in ["orientation", "base", "at"] {
                            self.vals.insert(k.into(), Value::Null);
                        }
                    }
                    self.vals.insert("handle".into(), json!(h.hex()));
                    self.picking = false;
                }
                None => s.echo("No object found."),
            }
            return Ok(Step::Continue);
        }
        if !self.profile_given() {
            match (self.corner, i) {
                (None, Input::Point(p)) => self.corner = Some(p),
                (Some(c), Input::Point(p)) => {
                    rect_profile(c, p)?;
                    self.vals.insert("corner1".into(), json!([c.x, c.y]));
                    self.vals.insert("corner2".into(), json!([p.x, p.y]));
                }
                (None, Input::Enter) => self.picking = true,
                (None, Input::Keyword(t) | Input::Text(t)) if t.trim().eq_ignore_ascii_case("object") || t.trim().eq_ignore_ascii_case("o") => {
                    self.picking = true
                }
                _ => {}
            }
            return Ok(Step::Continue);
        }
        if !self.vals.contains_key("thickness") {
            let t = match i {
                Input::Enter => DEFAULT_THICKNESS,
                Input::Keyword(t) | Input::Text(t) => distance(&t)?,
                _ => return Ok(Step::Continue),
            };
            if t <= 0.0 {
                return Err(EngineError::Other("The thickness must be positive.".into()));
            }
            self.vals.insert("thickness".into(), json!(t));
            if self.vals.contains_key("at") {
                return self.finish(s);
            }
            return Ok(Step::Continue);
        }
        if !self.vals.contains_key("orientation") {
            let o = match i {
                Input::Enter => Plane::Top,
                Input::Keyword(t) | Input::Text(t) => match Plane::parse(&t) {
                    Some(o) => o,
                    None => {
                        s.echo("Requires Top, Front or Right.");
                        return Ok(Step::Continue);
                    }
                },
                _ => return Ok(Step::Continue),
            };
            let name = match o {
                Plane::Top => "top",
                Plane::Front => "front",
                Plane::Right => "right",
            };
            self.vals.insert("orientation".into(), json!(name));
            return Ok(Step::Continue);
        }
        // A profile object's base and insertion point are world points (as the profile is):
        // points picked in a construction plane are taken there; a rectangle's stay in the plane.
        let z = s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0);
        let c = if self.vals.contains_key("handle") { s.cplane() } else { Mat4::IDENTITY };
        if !self.vals.contains_key("base") {
            match i {
                Input::Point(p) => {
                    let q = if c == Mat4::IDENTITY { p } else { c.apply(p.to3(z)).xy() };
                    self.vals.insert("base".into(), json!([q.x, q.y]))
                }
                Input::Enter => self.vals.insert("base".into(), Value::Null),
                _ => return Ok(Step::Continue),
            };
            return Ok(Step::Continue);
        }
        let at = match i {
            Input::Point(p) => {
                let q = c.apply(p.to3(z));
                json!([q.x, q.y, q.z])
            }
            Input::Enter => {
                let o = c.apply(Vec3::ZERO);
                json!([o.x, o.y, o.z])
            }
            _ => return Ok(Step::Continue),
        };
        self.vals.insert("at".into(), at);
        self.finish(s)
    }
    fn preview(&self, _s: &Session, cursor: Vec2) -> Vec<EntityKind> {
        match self.corner {
            Some(c) if !self.profile_given() => vec![super::helpers::lwpoly(super::helpers::rect_vertices(c, cursor), true)],
            _ => Vec::new(),
        }
    }
}

// ---------------- PART ----------------

fn run_part(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let (name, material) = part_params("part", p)?;
    if name.is_none() && material.is_none() {
        return Err(bad("part", "give `name`, `material` or both"));
    }
    let mut xs = solids(s, &hs)?;
    let all = xs.len();
    xs.retain(|(h, _)| !super::curves::is_locked(s, *h));
    if xs.is_empty() {
        return Err(if all > 0 { locked_err() } else { bad("part", "select 3D solids") });
    }
    let d = s.doc_mut()?;
    for (h, _) in &xs {
        d.modify_entity(*h, |e| {
            if let EntityKind::Solid3d(x) = &mut e.kind {
                if let Some(n) = &name {
                    x.part.name.clone_from(n);
                }
                if let Some(m) = &material {
                    x.part.material.clone_from(m);
                }
            }
        })?;
    }
    Ok(json!({ "changed": xs.len(), "locked": all - xs.len() }))
}

/// PART's objects: some 3D solids among them, not all on locked layers.
fn editable_solids(s: &Session, hs: &[Handle]) -> Result<()> {
    let xs = solids(s, hs)?;
    if xs.is_empty() {
        return Err(bad("part", "select 3D solids"));
    }
    if xs.iter().all(|(h, _)| super::curves::is_locked(s, *h)) {
        return Err(locked_err());
    }
    Ok(())
}

/// PART: objects, then the name and the material (Enter keeps what they have). Objects with no
/// solid to change are asked for again; a failure at the end starts again from the selection.
#[derive(Default)]
struct PartM {
    sel: SelectPhase,
    hs: Vec<Handle>,
    name: Option<Option<String>>,
}

impl Interactive for PartM {
    fn name(&self) -> &'static str {
        "PART"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            let hs = self.sel.picked.clone();
            return self.take(s, hs);
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        if self.hs.is_empty() {
            return self.sel.prompt();
        }
        // The first solid's values are the defaults.
        let first = solids(s, &self.hs).ok().and_then(|xs| xs.into_iter().next()).map(|(_, x)| x.part).unwrap_or_default();
        let (msg, cur) = if self.name.is_none() { ("Enter part name", first.name) } else { ("Enter material", first.material) };
        let p = Prompt::new(msg, Accept::TEXT);
        if cur.is_empty() { p } else { p.default(cur) }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_empty() {
            return match self.sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Done),
                SelOutcome::Done(hs) => self.take(s, hs),
            };
        }
        let given = match i {
            Input::Text(t) | Input::Keyword(t) => Some(t),
            Input::Enter => None,
            _ => return Ok(Step::Continue),
        };
        let Some(name) = self.name.clone() else {
            self.name = Some(given);
            return Ok(Step::Continue);
        };
        if name.is_none() && given.is_none() {
            return Ok(Step::Done);
        }
        let hs: Vec<String> = self.hs.iter().map(|h| h.hex()).collect();
        if let Err(e) = run_part(s, &json!({ "handles": hs, "name": name, "material": given })) {
            *self = PartM::default();
            return Err(e);
        }
        s.set_selection(Vec::new());
        Ok(Step::Done)
    }
}

impl PartM {
    /// The objects picked, when there is a solid to change among them; else select again.
    fn take(&mut self, s: &mut Session, hs: Vec<Handle>) -> Result<Step> {
        if let Err(e) = editable_solids(s, &hs) {
            self.sel = SelectPhase::default();
            return Err(e);
        }
        self.hs = hs;
        Ok(Step::Continue)
    }
}

// ---------------- CUTLIST ----------------

/// Most solids one cut list or drawing gathers, most block reference copies it opens on the way
/// and most objects in them it looks at (hostile arrays and nesting).
const MAX_PARTS: usize = 100_000;
const MAX_COPIES: usize = 1_000_000;
const MAX_VISITS: usize = 10_000_000;

/// The solids a command works on, placed in the world: those given, else every one in model
/// space (`seen`: only those on a layer that is on and thawed). A block reference stands for the
/// solids in its block, once for each of its copies and where it puts them (nested references
/// too, as the canvas draws them).
fn chosen(s: &Session, p: &Value, cmd: &str, seen: bool) -> Result<Vec<Solid3d>> {
    // An empty list (as MCP clients send optional arrays) counts as none given.
    let given = ["handle", "handles"].iter().any(|k| p.get(k).is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty())));
    let d = s.doc()?;
    let hs: Vec<Handle> = if given { targets(s, p)? } else { d.model.iter().map(|e| e.handle).collect() };
    let mut g = Gather { d, seen, out: Vec::new(), copies: 0, visits: 0, solid_blocks: HashMap::new() };
    let mut once = HashSet::with_capacity(hs.len());
    for h in hs.into_iter().filter(|h| once.insert(*h)) {
        let Some(e) = d.entity(h) else { continue };
        if given || g.shown(&e.common.layer) {
            g.add(e, &Mat4::IDENTITY, &e.common.layer, 0);
        }
    }
    if g.out.len() >= MAX_PARTS || g.copies >= MAX_COPIES || g.visits >= MAX_VISITS {
        log::warn!("{cmd}: past {MAX_PARTS} solids, {MAX_COPIES} block copies or {MAX_VISITS} objects in blocks, the rest were left out");
    }
    if g.out.is_empty() { Err(bad(cmd, "there are no 3D solids")) } else { Ok(g.out) }
}

/// [`chosen`]'s walk through objects and the blocks they reference.
struct Gather<'a> {
    d: &'a Drawing,
    seen: bool,
    out: Vec<Solid3d>,
    copies: usize,
    visits: usize,
    /// Whether each block holds a solid, itself or in a reference it holds (worked out once, so
    /// arrays of linework are passed by).
    solid_blocks: HashMap<&'a str, bool>,
}

impl<'a> Gather<'a> {
    fn shown(&self, layer: &str) -> bool {
        !self.seen || self.d.layer(layer).is_none_or(Layer::visible)
    }

    /// Whether block `name` holds a 3D solid, in itself or in a reference it holds.
    /// ponytail: a block in a reference cycle (which AutoCAD refuses) may be judged by the part of
    /// the cycle seen first.
    fn has_solids(&mut self, name: &'a str, depth: usize) -> bool {
        if let Some(has) = self.solid_blocks.get(name) {
            return *has;
        }
        // Past the nesting the walk opens: looked at by the walk itself (and its caps).
        if depth >= MAX_BLOCK_DEPTH {
            return true;
        }
        // Meanwhile none, so a block holding itself ends.
        self.solid_blocks.insert(name, false);
        let d = self.d;
        let has = d.block(name).is_some_and(|b| {
            b.entities.iter().any(|e| match &e.kind {
                EntityKind::Solid3d(_) => true,
                EntityKind::Insert(i) => self.has_solids(&i.block, depth + 1),
                _ => false,
            })
        });
        self.solid_blocks.insert(name, has);
        has
    }

    /// `e`, on `layer` (a block's layer 0 objects take their reference's), placed by `at`.
    fn add(&mut self, e: &'a Entity, at: &Mat4, layer: &str, depth: usize) {
        if self.out.len() >= MAX_PARTS {
            return;
        }
        match &e.kind {
            EntityKind::Solid3d(x) => self.out.push(Solid3d { xform: at.mul(&x.xform), ..x.clone() }),
            EntityKind::Insert(ins) if depth < MAX_BLOCK_DEPTH && self.has_solids(&ins.block, 0) => {
                let d = self.d;
                let Some(blk) = d.block(&ins.block) else { return };
                // The reference's own plane takes the block's XY plane on to the world.
                let plane = at.mul(&e.plane().unwrap_or(Mat4::IDENTITY));
                for r in 0..ins.rows.clamp(1, 10_000) {
                    for c in 0..ins.cols.clamp(1, 10_000) {
                        self.copies += 1;
                        if self.copies > MAX_COPIES || self.out.len() >= MAX_PARTS || self.visits >= MAX_VISITS {
                            return;
                        }
                        let off = Vec2::new(ins.col_spacing * f64::from(c), ins.row_spacing * f64::from(r)).rotate(ins.rotation);
                        let m = Mat3::translate(off).then_before(ins.transform(blk.base.xy()));
                        let w = plane.mul(&ins.lift(&m, blk.base));
                        for be in blk.entities.iter() {
                            self.visits += 1;
                            if self.visits >= MAX_VISITS {
                                return;
                            }
                            let l = if be.common.layer == "0" { layer } else { be.common.layer.as_str() };
                            if self.shown(l) {
                                self.add(be, &w, l, depth + 1);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// A size in tenths of a millimetre (drawing unit).
fn tenths(v: f64) -> Option<i64> {
    let t = (v * 10.0).round();
    (t.is_finite() && t.abs() < 9.0e15).then_some(t as i64)
}

/// Most outline points [`part_axes`] tries directions for (each one against every point).
const MAX_HULL_TRIES: usize = 512;

/// A part's own axes in its model coordinates, as a rotation onto them (rows: length, width and
/// thickness directions, longest first not implied): the thickness along the normal of its
/// largest flat faces (a board's two faces count together), length and width along the smallest
/// rectangle round its outline seen along that normal. So a board measures the same however it
/// was made or turned: drawn at an angle in plan, standing in a tilted plane, after a boolean.
/// ponytail: the smallest rectangle tries at most [`MAX_HULL_TRIES`] outline edges; every one
/// (rotating calipers) when parts with huge round outlines need the last tenth.
fn part_axes(m: &Mesh3) -> Mat4 {
    // Facet area by direction, opposite directions together (quantised unit normals).
    let mut area: BTreeMap<(i64, i64, i64), (f64, Vec3)> = BTreeMap::new();
    for t in &m.tris {
        let Some([a, b, c]) = m.tri(t) else { continue };
        let n = (b - a).cross(c - a);
        let l = n.len();
        if !(l > 0.0 && l.is_finite()) {
            continue;
        }
        let u = n * (1.0 / l);
        let q = |v: f64| (v * 1e6).round() as i64;
        let k = (q(u.x), q(u.y), q(u.z));
        let (k, u) = if k < (0, 0, 0) { ((-k.0, -k.1, -k.2), -u) } else { (k, u) };
        area.entry(k).or_insert((0.0, u)).0 += l;
    }
    let n =
        area.values().fold(None::<(f64, Vec3)>, |best, x| if best.is_some_and(|b| b.0 >= x.0) { best } else { Some(*x) }).map_or(Vec3::Z, |b| b.1);
    // Two axes across it: X and Y for a part lying flat.
    let e1 = if n.x.abs() < 0.9 { Vec3::new(1.0, 0.0, 0.0) - n * n.x } else { Vec3::new(0.0, 1.0, 0.0) - n * n.y }.normalized();
    let e2 = n.cross(e1);
    let flat: Vec<Vec2> = m.points().filter(|p| p.is_finite()).map(|p| Vec2::new(p.dot(e1), p.dot(e2))).collect();
    let hull = hull(flat);
    // The outline edge whose direction gives the smallest rectangle (an outline with no area
    // keeps the first axis).
    let step = hull.len().div_ceil(MAX_HULL_TRIES).max(1);
    let mut best: Option<(f64, Vec2)> = None;
    for i in (0..hull.len()).step_by(step).filter(|_| hull.len() >= 3) {
        let (Some(a), Some(b)) = (hull.get(i), hull.get((i + 1) % hull.len())) else { continue };
        let d = (*b - *a).normalized();
        if d == Vec2::ZERO {
            continue;
        }
        let w = d.perp();
        let span = |f: &dyn Fn(Vec2) -> f64| hull.iter().map(|p| f(*p)).fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let ((a0, a1), (b0, b1)) = (span(&|p| p.dot(d)), span(&|p| p.dot(w)));
        let size = (a1 - a0) * (b1 - b0);
        if size.is_finite() && best.is_none_or(|(s, _)| size < s - 1e-9 * s.abs()) {
            best = Some((size, d));
        }
    }
    let d = best.map_or(Vec2::new(1.0, 0.0), |b| b.1);
    let u = e1 * d.x + e2 * d.y;
    let v = n.cross(u);
    Mat4 { m: [[u.x, u.y, u.z, 0.0], [v.x, v.y, v.z, 0.0], [n.x, n.y, n.z, 0.0], [0.0, 0.0, 0.0, 1.0]] }
}

/// The convex hull of `pts`, counterclockwise (Andrew's monotone chain); fewer than three points
/// when they don't span an area.
fn hull(mut pts: Vec<Vec2>) -> Vec<Vec2> {
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let mut out: Vec<Vec2> = Vec::with_capacity(pts.len() + 1);
    // Whether `p` keeps the turn at the end of `out` counterclockwise (else the end goes).
    let left = |out: &[Vec2], p: Vec2| match out {
        [.., o, a] => (*a - *o).cross(p - *o) > 0.0,
        _ => true,
    };
    // The lower chain left to right, then the upper one back.
    for p in &pts {
        while out.len() >= 2 && !left(&out, *p) {
            out.pop();
        }
        out.push(*p);
    }
    let lower = out.len() + 1;
    for p in pts.iter().rev().skip(1) {
        while out.len() >= lower && !left(&out, *p) {
            out.pop();
        }
        out.push(*p);
    }
    out.pop();
    out
}

/// Each part's length, width and thickness in tenths, longest first: the box of its B-rep along
/// its own axes ([`part_axes`] of the part as placed, so a part in a block reference scaled
/// unevenly measures stretched), from the kernel (round and free-form faces measure their true
/// size); from its mesh where the kernel can't measure it (no B-rep, a build without it).
fn part_sizes(xs: &[&Solid3d]) -> Vec<Option<[i64; 3]>> {
    // World → along the part's axes; worked out once per mesh and turn (block copies repeat both).
    let mut axes: HashMap<(*const Mesh3, [u64; 9]), Mat4> = HashMap::new();
    let frames: Vec<Mat4> = xs
        .iter()
        .map(|x| {
            let m = &x.xform.m;
            let turn: [u64; 9] = std::array::from_fn(|i| m.get(i / 3).and_then(|r| r.get(i % 3)).map_or(0, |v| v.to_bits()));
            axes.entry((std::sync::Arc::as_ptr(&x.mesh), turn)).or_insert_with(|| part_axes(&placed_mesh(x))).mul(&x.xform)
        })
        .collect();
    let exact = if xs.iter().any(|x| !x.brep.is_empty()) {
        let placed: Vec<Placed> = xs.iter().zip(&frames).map(|(x, t)| Placed { brep: &x.brep, xform: t }).collect();
        cadcraft_kernel::boxes(&placed).unwrap_or_else(|e| {
            log::warn!("cut list: sizes from the meshes ({e})");
            Vec::new()
        })
    } else {
        Vec::new()
    };
    xs.iter()
        .zip(&frames)
        .enumerate()
        .map(|(i, (x, t))| {
            let (lo, hi) = exact.get(i).copied().flatten().or_else(|| point_box(x.mesh.points().map(|p| t.apply(p))))?;
            let mut s = [tenths(hi.x - lo.x)?, tenths(hi.y - lo.y)?, tenths(hi.z - lo.z)?];
            s.sort_unstable_by(|a, b| b.cmp(a));
            Some(s)
        })
        .collect()
}

/// The part's mesh where it is in the world.
fn placed_mesh(x: &Solid3d) -> Mesh3 {
    let at = |p: &Vec3| x.xform.apply(*p);
    Mesh3 {
        verts: x.mesh.verts.iter().map(at).collect(),
        tris: x.mesh.tris.clone(),
        edges: x.mesh.edges.iter().map(|e| e.iter().map(at).collect()).collect(),
        open: x.mesh.open,
    }
}

/// The box of the finite points among `pts`.
fn point_box(pts: impl Iterator<Item = Vec3>) -> Option<(Vec3, Vec3)> {
    let mut pts = pts.filter(|p| p.is_finite());
    let first = pts.next()?;
    Some(pts.fold((first, first), |(lo, hi), p| {
        (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
    }))
}

#[derive(Clone, Debug, PartialEq)]
struct Row {
    name: String,
    material: String,
    size: [i64; 3],
    qty: usize,
}

/// One row per part (identical name, material and size grouped when `group`), by material, name
/// and size, longest first.
fn cut_rows(xs: &[Solid3d], group: bool) -> Vec<Row> {
    let closed: Vec<&Solid3d> = xs.iter().filter(|x| !x.mesh.open).collect();
    let mut rows: Vec<Row> = Vec::new();
    for (x, size) in closed.iter().zip(part_sizes(&closed)) {
        let Some(size) = size else { continue };
        let row = Row { name: x.part.name.clone(), material: x.part.material.clone(), size, qty: 1 };
        match rows.iter_mut().find(|r| group && r.name == row.name && r.material == row.material && r.size == row.size) {
            Some(r) => r.qty += 1,
            None => rows.push(row),
        }
    }
    rows.sort_by(|a, b| a.material.cmp(&b.material).then_with(|| a.name.cmp(&b.name)).then_with(|| b.size.cmp(&a.size)));
    rows
}

fn mm(t: i64) -> String {
    format!("{:.1}", t as f64 / 10.0)
}

/// A CSV field: quoted when it holds a separator, quote or line break; a leading `=`, `+`, `-` or
/// `@` is kept from being read as a spreadsheet formula.
fn csv_field(t: &str) -> String {
    let t = if t.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{t}") } else { t.to_string() };
    if t.contains([',', '"', '\n', '\r']) { format!("\"{}\"", t.replace('"', "\"\"")) } else { t }
}

const HEADER: [&str; 6] = ["Name", "Material", "Length", "Width", "Thickness", "Qty"];

fn table_cells(rows: &[Row]) -> Vec<Vec<String>> {
    rows.iter().map(|r| vec![r.name.clone(), r.material.clone(), mm(r.size[0]), mm(r.size[1]), mm(r.size[2]), r.qty.to_string()]).collect()
}

fn run_cutlist(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "cutlist";
    let xs = chosen(s, p, CMD, false)?;
    let rows = cut_rows(&xs, bool_or(p, "group", true));
    if rows.is_empty() {
        return Err(bad(CMD, "there are no closed 3D solids to list"));
    }
    let total: usize = rows.iter().map(|r| r.qty).sum();
    let mut cells = table_cells(&rows);
    cells.push(vec!["Total".into(), String::new(), String::new(), String::new(), String::new(), total.to_string()]);
    let mut out = json!({
        "rows": rows.iter().map(|r| json!({
            "name": r.name, "material": r.material,
            "length": r.size[0] as f64 / 10.0, "width": r.size[1] as f64 / 10.0, "thickness": r.size[2] as f64 / 10.0,
            "quantity": r.qty,
        })).collect::<Vec<_>>(),
        "total": total,
    });
    // Where the table goes in the world (a table lies level, whatever the construction plane).
    let at = match p.get("at").filter(|v| !v.is_null()) {
        Some(_) => Some(p3(p, "at").ok_or_else(|| bad(CMD, "`at` must be [x, y] or [x, y, z]"))?),
        None => None,
    };
    // The file first: when it can't be written the drawing stays as it was.
    if let Some(path) = text_param(CMD, p, "csv")? {
        if path.is_empty() {
            return Err(bad(CMD, "`csv` must be a file path"));
        }
        let mut text = String::from("\u{feff}");
        for r in std::iter::once(HEADER.iter().map(|h| h.to_string()).collect::<Vec<_>>()).chain(cells.iter().cloned()) {
            text.push_str(&r.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
            text.push_str("\r\n");
        }
        std::fs::write(&path, text).map_err(|e| EngineError::Other(format!("Can't write {path}: {e}")))?;
        out["csv"] = json!(path);
    }
    if let Some(at) = at {
        let hs = all_or_nothing(s, |s| cut_tables(s, at, &cells))?;
        out["handle"] = json!(hs.first().map(|h| h.hex()));
        out["handles"] = json!(hs.iter().map(|h| h.hex()).collect::<Vec<_>>());
    }
    Ok(out)
}

/// Most rows one cut-list table holds (well within a table's limit); a longer list goes on in
/// more tables to the right, each with the header, the last with the total.
const ROWS_PER_TABLE: usize = 1000;

/// The cut list's tables (title, header, `cells`) from `at` on, in the world.
fn cut_tables(s: &mut Session, at: Vec3, cells: &[Vec<String>]) -> Result<Vec<Handle>> {
    let d = s.doc()?;
    let cur = d.header.str("CTABLESTYLE", "Standard");
    let st = d.table_styles.iter().find(|t| t.name.eq_ignore_ascii_case(&cur)).or(d.table_styles.first()).cloned().unwrap_or_default();
    let header: Vec<String> = HEADER.iter().map(|h| h.to_string()).collect();
    // Columns as wide as their longest text, alike in every table.
    let th = st.text_height;
    let widths: Vec<f64> = (0..HEADER.len())
        .map(|c| {
            let chars = std::iter::once(HEADER.get(c).map_or(0, |h| h.chars().count()))
                .chain(cells.iter().map(|r| r.get(c).map_or(0, |x| x.chars().count())))
                .max()
                .unwrap_or(0);
            (chars.max(3) as f64 + 2.0) * th
        })
        .collect();
    let parts = cells.len().div_ceil(ROWS_PER_TABLE);
    let mut x = 0.0;
    let mut out = Vec::with_capacity(parts);
    for (i, chunk) in cells.chunks(ROWS_PER_TABLE).enumerate() {
        let title = if parts > 1 { format!("Cut list ({}/{parts})", i + 1) } else { "Cut list".to_string() };
        let mut t = super::table::make_table(
            &st,
            at.xy() + Vec2::new(x, 0.0),
            chunk.len(),
            HEADER.len(),
            None,
            None,
            Some(&title),
            Some(header.clone()),
            chunk,
        );
        // Every row or none: a table that took fewer rows than it was given is an error.
        if t.cells.len() != chunk.len() + 2 {
            return Err(EngineError::Other("The cut list does not fit its table.".into()));
        }
        t.insert.z = at.z;
        t.col_widths.clone_from(&widths);
        x += widths.iter().sum::<f64>() + 2.0 * th;
        out.push(s.add_entity_world(EntityKind::Table(t))?);
    }
    Ok(out)
}

// ---------------- VIEWBASE ----------------

fn view_dir(name: &str) -> ViewDir {
    let d = VIEWS.iter().find(|(n, _)| *n == name).map_or([0.0, 0.0, 1.0], |(_, d)| *d);
    ViewDir::new(Vec3::new(d[0], d[1], d[2]))
}

/// A run of points as a line, or a polyline (closed when it ends where it starts).
fn line_entity(pts: &[Vec2]) -> Option<EntityKind> {
    match pts {
        [] | [_] => None,
        [a, b] => Some(super::helpers::line(*a, *b)),
        [first, .., last] => {
            let closed = first.near(*last, EPS);
            let n = if closed { pts.len() - 1 } else { pts.len() };
            Some(super::helpers::lwpoly(pts.iter().take(n).map(|p| PolyVertex::new(*p)).collect(), closed))
        }
    }
}

/// A group name not taken yet: `VIEW front`, `VIEW front 2` …
fn free_group_name(s: &Session, base: &str) -> String {
    let taken = |n: &str| s.doc().is_ok_and(|d| super::groups::find(d, n).is_some());
    std::iter::once(base.to_string()).chain((2..).map(|i| format!("{base} {i}"))).take(10_000).find(|n| !taken(n)).unwrap_or_else(|| base.to_string())
}

/// The VISIBLE and HIDDEN layers, made when missing, and the dashed HIDDEN linetype (the drawing's,
/// or the library's loaded): the linetype hidden lines take, ByLayer when the HIDDEN layer has it
/// (a HIDDEN layer the drawing already had may be continuous).
fn ensure_view_layers(s: &mut Session) -> Result<String> {
    let d = s.doc_mut()?;
    if d.linetype("HIDDEN").is_none()
        && let Some(lt) = cadcraft_doc::library::standard_linetypes_for(d).into_iter().find(|l| l.name == "HIDDEN")
    {
        d.linetypes.push(lt);
    }
    let dashed = d.linetype("HIDDEN").is_some();
    if d.layer("VISIBLE").is_none() {
        d.layers.push(Layer::new("VISIBLE"));
    }
    if d.layer("HIDDEN").is_none() {
        let mut l = Layer::new("HIDDEN");
        if dashed {
            l.linetype = "HIDDEN".into();
        }
        d.layers.push(l);
    }
    let by_layer = d.layer("HIDDEN").is_some_and(|l| l.linetype.eq_ignore_ascii_case("HIDDEN"));
    Ok(if dashed && !by_layer { "HIDDEN".into() } else { Common::default().linetype })
}

/// Text height of the view dimensions on a sheet (millimetres).
const PAPER_TEXT_MM: f64 = 2.5;
/// The layout VIEWBASE draws on when none is named.
const VIEWS_LAYOUT: &str = "Drawing views";

/// A scale as model units per drawn unit: a positive number, or a ratio such as `"1:10"`
/// (drawn : model).
fn scale_value(v: &Value) -> Option<f64> {
    let k = match v {
        Value::Number(n) => n.as_f64()?,
        Value::String(t) => match t.split_once(':') {
            Some((a, b)) => number(b.trim())? / number(a.trim())?,
            None => number(t.trim())?,
        },
        _ => return None,
    };
    (k.is_finite() && k > 0.0).then_some(k)
}

/// The scales tried when fitting views on a sheet, largest drawing first: 1, 2 and 5 times the
/// powers of ten from 1000:1 to 1:50 000 000 (model units per paper unit).
fn standard_scales() -> impl Iterator<Item = f64> {
    (-3..=7).flat_map(|e| [1.0, 2.0, 5.0].map(move |m| m * 10f64.powi(e)))
}

/// Each view's rectangle at scale `k` (model units per drawn unit) with `gap` between views, the
/// front view's lower-left corner at the origin; `ext` gives a view's extent in model units.
fn arrange(names: &[&'static str], ext: &dyn Fn(&str) -> Bounds2, k: f64, gap: f64, first_angle: bool) -> Vec<(&'static str, Bounds2)> {
    let size = |n: &str| {
        let b = ext(n);
        (b.max - b.min) * (1.0 / k)
    };
    let f = size("front");
    names
        .iter()
        .map(|n| {
            let s = size(n);
            let at = match (*n, first_angle) {
                ("top", false) => Vec2::new(0.0, f.y + gap),
                ("top", true) => Vec2::new(0.0, -gap - s.y),
                ("right", false) => Vec2::new(f.x + gap, 0.0),
                ("right", true) => Vec2::new(-gap - s.x, 0.0),
                ("iso", _) => Vec2::new(f.x + gap, f.y + gap),
                _ => Vec2::ZERO,
            };
            (*n, Bounds2 { min: at, max: at + s })
        })
        .collect()
}

/// The views' rectangles with `margin` for the dimensions below and left of the orthographic ones.
fn footprint(rects: &[(&str, Bounds2)], margin: f64) -> Bounds2 {
    rects.iter().fold(Bounds2::EMPTY, |a, (n, r)| {
        let m = if *n == "iso" { 0.0 } else { margin };
        a.union(&Bounds2 { min: r.min - Vec2::new(m, m), max: r.max })
    })
}

/// The layout `name` (made, with no viewport, when missing) and its sheet.
fn sheet_of(s: &mut Session, name: &str) -> Result<(String, cadcraft_render::Sheet)> {
    let name = match s.doc()?.layout(name) {
        Some(l) => l.name.clone(),
        None => {
            let spec = find_command("layout.new").ok_or_else(|| EngineError::UnknownCommand("layout.new".into()))?;
            let r = (spec.run)(s, &json!({ "name": name, "viewport": false }))?;
            r.get("name").and_then(Value::as_str).unwrap_or(name).to_string()
        }
    };
    let sheet = cadcraft_render::sheet(s.doc()?, &name).ok_or_else(|| bad("viewbase", format!("no layout `{name}`")))?;
    Ok((name, sheet))
}

/// Runs `f` with `space` as the space objects are added to (the views' layout, or model space),
/// then puts back the space being edited.
fn in_space<T>(s: &mut Session, space: &Space, f: impl FnOnce(&mut Session) -> Result<T>) -> Result<T> {
    let st = s.state_mut()?;
    let was = (std::mem::replace(&mut st.space, space.clone()), st.mspace.take());
    let r = f(s);
    if let Ok(st) = s.state_mut() {
        (st.space, st.mspace) = was;
    }
    r
}

/// Runs `f` with the construction plane World, so the objects commands add for it (VIEWBASE's
/// dimensions, computed in the world) are not placed in the plane again; the plane is put back.
fn in_world<T>(s: &mut Session, f: impl FnOnce(&mut Session) -> Result<T>) -> Result<T> {
    let c = s.cplane();
    if c == Mat4::IDENTITY {
        return f(s);
    }
    s.set_cplane(Mat4::IDENTITY)?;
    let r = f(s);
    let back = s.set_cplane(c);
    r.and_then(|v| back.map(|()| v))
}

/// Each named view's extent in model units: the solids' B-rep boxes seen along it, from the
/// kernel (round faces reach their full size); a solid it can't measure by its mesh.
fn view_extents(xs: &[&Solid3d], names: &[&str]) -> Vec<Bounds2> {
    let turns: Vec<Mat4> = names.iter().map(|n| view_dir(n).rotation()).collect();
    let placed: Vec<(&Solid3d, Mat4)> = turns.iter().flat_map(|t| xs.iter().map(move |x| (*x, t.mul(&x.xform)))).collect();
    let exact = if xs.iter().any(|x| !x.brep.is_empty()) {
        let all: Vec<Placed> = placed.iter().map(|(x, m)| Placed { brep: &x.brep, xform: m }).collect();
        cadcraft_kernel::boxes(&all).unwrap_or_else(|e| {
            log::warn!("VIEWBASE: view sizes from the meshes ({e})");
            Vec::new()
        })
    } else {
        Vec::new()
    };
    names
        .iter()
        .enumerate()
        .map(|(v, n)| {
            xs.iter().enumerate().fold(Bounds2::EMPTY, |b, (i, x)| match exact.get(v * xs.len() + i).copied().flatten() {
                Some((lo, hi)) => b.union(&Bounds2 { min: lo.xy(), max: hi.xy() }),
                None => b.union(&hlr::extent(&[*x], view_dir(n))),
            })
        })
        .collect()
}

/// How the views are dimensioned: at all, the dimension lines' offset from a view, the model
/// units per drawn unit (the dimensions read model sizes), and on a sheet the text height.
struct Dims {
    on: bool,
    off: f64,
    k: f64,
    text: Option<f64>,
}

fn dimension(s: &mut Session, a: Vec2, b: Vec2, at: Vec2, rotation: f64, dims: &Dims) -> Result<Handle> {
    let spec = find_command("dimlinear").ok_or_else(|| EngineError::UnknownCommand("dimlinear".into()))?;
    let r = (spec.run)(s, &json!({ "p1": [a.x, a.y], "p2": [b.x, b.y], "at": [at.x, at.y], "rotation": rotation }))?;
    let h = r.get("handle").and_then(Value::as_str).and_then(Handle::parse_hex).ok_or_else(|| EngineError::Other("no dimension was made".into()))?;
    // On a sheet the whole style is scaled (DIMSCALE) to the paper's text height.
    let d = s.doc()?;
    let style_text = match d.entity(h).map(|e| &e.kind) {
        Some(EntityKind::Dimension(dm)) => d.dim_style(&dm.style).cloned().unwrap_or_default().with_overrides(&dm.overrides).text_height,
        _ => 0.0,
    };
    let dimscale = dims.text.map(|t| t / style_text).filter(|v| v.is_finite() && *v > 0.0);
    let k = dims.k;
    s.doc_mut()?.modify_entity(h, |e| {
        if let EntityKind::Dimension(d) = &mut e.kind {
            // A scaled view measures true sizes (DIMLFAC).
            if (k - 1.0).abs() > 1e-12 {
                d.overrides.insert("linearFactor".into(), json!(k));
            }
            if let Some(v) = dimscale {
                d.overrides.insert("scale".into(), json!(v));
            }
        }
    })?;
    Ok(h)
}

fn run_viewbase(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "viewbase";
    let xs = chosen(s, p, CMD, true)?;
    let names: Vec<&'static str> = match p.get("views") {
        None | Some(Value::Null) => vec!["front", "top", "right"],
        Some(Value::Array(a)) => {
            let mut v: Vec<&'static str> = Vec::new();
            for x in a.iter().take(VIEWS.len() * 4) {
                let n = x.as_str().map(str::to_ascii_lowercase).unwrap_or_default();
                let known = VIEWS
                    .iter()
                    .find(|(k, _)| *k == n)
                    .map(|(k, _)| *k)
                    .ok_or_else(|| bad(CMD, format!("`views` takes front, top, right and iso, not {x}")))?;
                if !v.contains(&known) {
                    v.push(known);
                }
            }
            if v.is_empty() {
                return Err(bad(CMD, "`views` names no view"));
            }
            v
        }
        Some(v) => return Err(bad(CMD, format!("`views` must be an array of view names, not {v}"))),
    };
    let scale = match p.get("scale") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            scale_value(v)
                .ok_or_else(|| bad(CMD, format!("`scale` is model units per paper unit (a positive number) or a ratio such as \"1:10\", not {v}")))?,
        ),
    };
    let model = match p.get("space") {
        None | Some(Value::Null) => false,
        Some(v) => match v.as_str().map(str::to_ascii_lowercase).as_deref() {
            Some("paper") => false,
            Some("model") => true,
            _ => return Err(bad(CMD, format!("`space` is \"paper\" or \"model\", not {v}"))),
        },
    };
    let layout = match text_param(CMD, p, "layout")? {
        _ if model => None,
        Some(n) if n.eq_ignore_ascii_case("model") => None,
        Some(n) if !n.is_empty() => Some(n),
        _ => Some(VIEWS_LAYOUT.to_string()),
    };
    let hidden = bool_or(p, "hidden", true);
    let dims = bool_or(p, "dims", true);
    let first_angle = match p.get("projection") {
        None | Some(Value::Null) => false,
        Some(v) => match v.as_str().map(str::to_ascii_lowercase).as_deref() {
            Some("third") => false,
            Some("first") => true,
            _ => return Err(bad(CMD, format!("`projection` is \"third\" or \"first\", not {v}"))),
        },
    };
    let gap = size_param(CMD, p, "gap", true)?;
    let at = match p.get("at").filter(|v| !v.is_null()) {
        Some(_) => Some(point_req(CMD, p, "at")?),
        None => None,
    };
    let refs: Vec<&Solid3d> = xs.iter().collect();
    // Each view's extent in model units, once.
    let wanted: Vec<&str> = VIEWS.iter().filter(|(n, _)| *n != "iso" || names.contains(n)).map(|(n, _)| *n).collect();
    let exts: Vec<(&str, Bounds2)> = wanted.iter().copied().zip(view_extents(&refs, &wanted)).collect();
    let ext = |n: &str| exts.iter().find(|(m, _)| *m == n).map_or(Bounds2::EMPTY, |(_, b)| *b);
    let (front, top) = (ext("front"), ext("top"));
    // The model's size: its extent along X, Y and Z.
    let size = front.width().max(front.height()).max(top.height());
    if !size.is_finite() || size <= EPS {
        return Err(bad(CMD, "the solids have no size"));
    }
    // The solids as drawn: meshed again by the kernel, which tells their smooth edges (a seam, a
    // fillet's tangent edge) from real ones however shallow; as stored where it can't. Each B-rep
    // once, however many block copies share it.
    let mut slot: HashMap<&str, usize> = HashMap::new();
    let mut breps: Vec<&str> = Vec::new();
    let which: Vec<usize> = refs
        .iter()
        .map(|x| {
            *slot.entry(&*x.brep).or_insert_with(|| {
                breps.push(&x.brep);
                breps.len() - 1
            })
        })
        .collect();
    let outlined = if breps.iter().any(|b| !b.is_empty()) {
        cadcraft_kernel::outlines(&breps).unwrap_or_else(|e| {
            log::warn!("VIEWBASE: edges judged from the stored meshes ({e})");
            Vec::new()
        })
    } else {
        Vec::new()
    };
    let meshes: Vec<Option<(std::sync::Arc<Mesh3>, &[bool])>> =
        outlined.iter().map(|o| o.as_ref().map(|o| (std::sync::Arc::new(o.mesh.clone()), o.smooth.as_slice()))).collect();
    let remeshed: Vec<(Solid3d, Option<&[bool]>)> = refs
        .iter()
        .zip(&which)
        .map(|(x, i)| match meshes.get(*i).and_then(Option::as_ref) {
            Some((m, smooth)) => (Solid3d { mesh: m.clone(), ..(*x).clone() }, Some(*smooth)),
            None => ((*x).clone(), None),
        })
        .collect();
    let drawn: Vec<hlr::Drawn> = remeshed.iter().map(|(x, smooth)| hlr::Drawn { solid: x, smooth: *smooth }).collect();
    // Every view's lines (in model units) before the drawing changes.
    let mut lines = Vec::with_capacity(names.len());
    let mut capped: Vec<String> = Vec::new();
    for n in &names {
        let l = hlr::lines(&drawn, view_dir(n));
        for c in &l.capped {
            log::warn!("VIEWBASE {n} view: {c}");
            capped.push(format!("{n}: {c}"));
        }
        // ponytail: the iso view shows visible lines only, as pictorial views do; a per-view
        // choice when someone wants dashes there.
        let hid = if hidden && *n != "iso" { l.hidden } else { Vec::new() };
        lines.push((*n, l.visible, hid));
    }
    if lines.iter().all(|(_, v, h)| v.is_empty() && h.is_empty()) {
        return Err(bad(CMD, "the solids have no edges to draw"));
    }
    all_or_nothing(s, |s| {
        // On a sheet (its layout made when missing), or in model space.
        let sheet = match &layout {
            Some(n) => Some(sheet_of(s, n)?),
            None => None,
        };
        let text = sheet.as_ref().map(|(_, sh)| PAPER_TEXT_MM / sh.unit_mm);
        let gap_at = |k: f64| {
            gap.unwrap_or_else(|| {
                let g = size / k / 10.0;
                text.map_or(g, |t| g.max(6.0 * t))
            })
        };
        let off = |gap: f64| text.map_or(gap * 0.4, |t| 3.0 * t);
        let plan = |k: f64| {
            let gap = gap_at(k);
            let rects = arrange(&names, &ext, k, gap, first_angle);
            let fp = footprint(&rects, if dims { off(gap) + 2.0 * text.unwrap_or(0.0) } else { 0.0 });
            (gap, rects, fp)
        };
        let k = match (scale, &sheet) {
            (Some(k), _) => k,
            (None, Some((_, sh))) => {
                let room = sh.printable;
                let fits = |k: f64| {
                    let fp = plan(k).2;
                    fp.width() <= room.width() && fp.height() <= room.height()
                };
                standard_scales().find(|k| fits(*k)).unwrap_or(5e7)
            }
            (None, None) => 1.0,
        };
        let (gap, rects, fp) = plan(k);
        let at = match (at, &sheet) {
            (Some(a), _) => a,
            (None, Some((_, sh))) => sh.printable.center() - fp.center(),
            // Beside the model in plan.
            (None, None) => Vec2::new(top.max.x + size, top.min.y),
        };
        if !(at + fp.min).is_finite() || !(at + fp.max).is_finite() {
            return Err(bad(CMD, "the views are out of range at that scale and place"));
        }
        let views: Vec<View> = lines
            .into_iter()
            .zip(&rects)
            .map(|((n, vis, hid), (_, r))| {
                let origin = at + r.min - ext(n).min * (1.0 / k);
                let place = |pts: Vec<Vec2>| pts.into_iter().map(|q| q * (1.0 / k) + origin).collect::<Vec<Vec2>>();
                (n, vis.into_iter().map(place).collect(), hid.into_iter().map(place).collect(), Bounds2 { min: at + r.min, max: at + r.max })
            })
            .collect();
        let space = sheet.as_ref().map_or(Space::Model, |(n, _)| Space::Paper(n.clone()));
        let report = in_world(s, |s| in_space(s, &space, |s| draw_views(s, views, &Dims { on: dims, off: off(gap), k, text })))?;
        Ok(json!({ "views": report, "capped": capped, "layout": sheet.map(|(n, _)| n), "scale": k }))
    })
}

type View = (&'static str, Vec<Vec<Vec2>>, Vec<Vec<Vec2>>, Bounds2);

/// VIEWBASE's drawing, in the space being edited: each view's lines, dimensions and group (a
/// failure part way leaves partial views, so the caller puts the drawing back).
fn draw_views(s: &mut Session, views: Vec<View>, dims: &Dims) -> Result<Vec<Value>> {
    let dashes = ensure_view_layers(s)?;
    let space = s.space();
    let mut report = Vec::new();
    for (n, vis, hid, b) in views {
        let mut hs = Vec::new();
        for (layer, runs) in [("VISIBLE", &vis), ("HIDDEN", &hid)] {
            for r in runs {
                let Some(k) = line_entity(r) else { continue };
                let linetype = if layer == "HIDDEN" { dashes.clone() } else { Common::default().linetype };
                let common = Common { layer: layer.into(), linetype, ..Common::default() };
                hs.push(s.doc_mut()?.add(&space, common, k)?);
            }
        }
        // ponytail: overall width and height below and left of each orthographic view; hole
        // positions and part sizes (DIMBASELINE chains) by hand until shop drawings need them.
        let off = dims.off;
        if dims.on && n != "iso" && b.width() > EPS {
            hs.push(dimension(s, b.min, Vec2::new(b.max.x, b.min.y), Vec2::new((b.min.x + b.max.x) / 2.0, b.min.y - off), 0.0, dims)?);
        }
        if dims.on && n != "iso" && b.height() > EPS {
            hs.push(dimension(s, b.min, Vec2::new(b.min.x, b.max.y), Vec2::new(b.min.x - off, (b.min.y + b.max.y) / 2.0), 90.0, dims)?);
        }
        let group = free_group_name(s, &format!("VIEW {n}"));
        if !hs.is_empty() {
            super::groups::create(s, Some(&group), "VIEWBASE", &hs, true)?;
        }
        report.push(
            json!({ "view": n, "group": group, "visible": vis.len(), "hidden": hid.len(), "min": [b.min.x, b.min.y], "max": [b.max.x, b.max.y] }),
        );
    }
    Ok(report)
}

// ---------------- CUTLIST and VIEWBASE prompts ----------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum PickFor {
    CutList,
    Views,
}

/// CUTLIST and VIEWBASE: the solids (Enter: the entire model), then where the table or the front
/// view goes. VIEWBASE started outside a layout first asks for the layout (made when missing and
/// made current, as AutoCAD does; Model keeps the views in model space), and takes a Scale. A
/// selection with no solids is asked for again; a failure at the end starts again from the
/// selection.
struct PickThenPoint {
    what: PickFor,
    sel: SelectPhase,
    hs: Option<Vec<Handle>>,
    /// VIEWBASE's layout: not asked yet, model space (`Some(None)`), or a layout's name.
    layout: Option<Option<String>>,
    scale: Option<f64>,
    asking_scale: bool,
}

impl PickThenPoint {
    fn new(what: PickFor) -> Self {
        PickThenPoint { what, sel: SelectPhase::default(), hs: None, layout: None, scale: None, asking_scale: false }
    }
    fn id(&self) -> &'static str {
        match self.what {
            PickFor::CutList => "cutlist",
            PickFor::Views => "viewbase",
        }
    }
    /// The JSON form's objects: those picked (none: the entire model).
    fn params(hs: &[Handle]) -> Value {
        if hs.is_empty() { json!({}) } else { json!({ "handles": hs.iter().map(|h| h.hex()).collect::<Vec<_>>() }) }
    }
    /// The objects picked, when there are solids among them (or in the model); else select again.
    /// In a layout's paper space VIEWBASE's views go on that layout.
    fn take(&mut self, s: &mut Session, hs: Vec<Handle>) -> Result<Step> {
        if let Err(e) = chosen(s, &Self::params(&hs), self.id(), self.what == PickFor::Views) {
            self.sel = SelectPhase::default();
            return Err(e);
        }
        if self.what == PickFor::Views
            && let Space::Paper(n) = s.space()
        {
            self.layout = Some(Some(n));
        }
        self.hs = Some(hs);
        Ok(Step::Continue)
    }
    /// The layout named at the prompt, made when missing and made current; `model` keeps the
    /// views in model space.
    fn choose_layout(&mut self, s: &mut Session, name: &str) -> Result<Step> {
        let name = if name.is_empty() { VIEWS_LAYOUT } else { name };
        if name.eq_ignore_ascii_case("model") {
            self.layout = Some(None);
            return Ok(Step::Continue);
        }
        let (name, _) = sheet_of(s, name)?;
        let set = find_command("layout.set").ok_or_else(|| EngineError::UnknownCommand("layout.set".into()))?;
        (set.run)(s, &json!({ "name": name }))?;
        self.layout = Some(Some(name));
        Ok(Step::Continue)
    }
    fn run(&mut self, s: &mut Session, at: Option<Vec2>) -> Result<Step> {
        let mut p = Self::params(self.hs.as_deref().unwrap_or_default());
        // The point picked in the construction plane, in the world (where the JSON form takes it).
        if let Some(at) = at {
            let q = s.cplane().apply(at.to3(s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0)));
            p["at"] = if self.what == PickFor::CutList { json!([q.x, q.y, q.z]) } else { json!([q.x, q.y]) };
        }
        if let Some(k) = self.scale {
            p["scale"] = json!(k);
        }
        match &self.layout {
            Some(Some(n)) => p["layout"] = json!(n),
            Some(None) => p["space"] = json!("model"),
            None => {}
        }
        let spec = find_command(self.id()).ok_or_else(|| EngineError::UnknownCommand(self.id().into()))?;
        if let Err(e) = (spec.run)(s, &p) {
            // Start again from the selection: the objects may be what failed.
            *self = PickThenPoint::new(self.what);
            return Err(e);
        }
        s.set_selection(Vec::new());
        Ok(Step::Done)
    }
}

impl Interactive for PickThenPoint {
    fn name(&self) -> &'static str {
        match self.what {
            PickFor::CutList => "CUTLIST",
            PickFor::Views => "VIEWBASE",
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            let hs = self.sel.picked.clone();
            return self.take(s, hs);
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match (&self.hs, self.what) {
            (None, _) if self.sel.removing => self.sel.prompt(),
            (None, _) => Prompt::new("Select solids", Accept::SELECT).kw(&["Entire model"]).default("Entire model"),
            (Some(_), PickFor::Views) if self.layout.is_none() => {
                Prompt::new("Enter new or existing layout name to make current", Accept::TEXT).kw(&["Model"]).default(VIEWS_LAYOUT)
            }
            (Some(_), _) if self.asking_scale => {
                let fit = if matches!(self.layout, Some(Some(_))) { "Fit".to_string() } else { "1".to_string() };
                Prompt::new("Enter scale", Accept::NUMBER).default(self.scale.map_or(fit, |k| format!("{k}")))
            }
            (Some(_), PickFor::CutList) => Prompt::new("Specify insertion point for the table", Accept::POINT),
            (Some(_), PickFor::Views) => Prompt::new("Specify location of base view", Accept::POINT).kw(&["Scale"]).default("Auto"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_none() {
            if let Input::Keyword(t) | Input::Text(t) = &i
                && t.trim().to_ascii_lowercase().starts_with('e')
            {
                return self.take(s, Vec::new());
            }
            return match self.sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => self.take(s, Vec::new()),
                SelOutcome::Done(hs) => self.take(s, hs),
            };
        }
        if self.what == PickFor::Views && self.layout.is_none() {
            return match i {
                Input::Enter => self.choose_layout(s, ""),
                Input::Keyword(t) | Input::Text(t) => self.choose_layout(s, t.trim()),
                _ => Ok(Step::Continue),
            };
        }
        if self.asking_scale {
            match i {
                Input::Enter => {}
                Input::Text(t) | Input::Keyword(t) => match scale_value(&json!(t.trim())) {
                    Some(v) => self.scale = Some(v),
                    None => return Err(EngineError::Other("The scale must be a positive number or a ratio such as 1:10.".into())),
                },
                _ => return Ok(Step::Continue),
            }
            self.asking_scale = false;
            return Ok(Step::Continue);
        }
        match i {
            Input::Point(p) => self.run(s, Some(p)),
            Input::Enter if self.what == PickFor::Views => self.run(s, None),
            Input::Keyword(t) | Input::Text(t) if self.what == PickFor::Views && t.trim().to_ascii_lowercase().starts_with('s') => {
                self.asking_scale = true;
                Ok(Step::Continue)
            }
            _ => Ok(Step::Continue),
        }
    }
}

/// Kernel-free checks: parameters, parts, cut lists and prompts.
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cadcraft_geom::Mesh3;

    use super::*;

    fn err(r: Result<Value>) -> String {
        r.err().map(|e| e.to_string()).unwrap_or_default()
    }

    /// A solid whose mesh is a box from the origin to `size`.
    fn block(s: &mut Session, size: [f64; 3], name: &str, material: &str) -> Handle {
        let mesh = Mesh3 { verts: vec![Vec3::ZERO, Vec3::new(size[0], size[1], size[2])], ..Default::default() };
        let x = Solid3d { mesh: Arc::new(mesh), part: Part { name: name.into(), material: material.into() }, ..Default::default() };
        s.add_entity(EntityKind::Solid3d(x)).unwrap()
    }

    #[test]
    fn hostile_panel_parameters_are_refused() {
        let mut s = Session::new();
        for p in [
            json!({ "corner1": [0, 0], "corner2": [0, 5] }),
            json!({ "corner1": [0, 0], "corner2": [5, 5], "thickness": -1 }),
            json!({ "corner1": [0, 0], "corner2": [5, 5], "thickness": "x" }),
            json!({ "corner1": [0, 0], "corner2": [5, 5], "orientation": "up" }),
            json!({ "corner1": [0, 0], "corner2": [5, 5], "orientation": 3 }),
            json!({ "corner1": [0, 0], "corner2": [5, 5], "at": "x" }),
            json!({ "corner1": [0, 0], "corner2": [5, 5], "base": [1] }),
            json!({ "corner1": [0, 0], "corner2": [5, 5], "name": 7 }),
            json!({ "handle": "zz" }),
            json!({ "handle": "FFFFFF" }),
            json!({ "corner1": [1e308, 0], "corner2": [-1e308, 5] }),
        ] {
            assert!(s.execute("panel", &p).is_err(), "{p}");
        }
        // A line is no profile.
        let l = s.execute("line", &json!({ "points": [[0, 0], [5, 0]] })).unwrap()["handles"][0].clone();
        assert!(err(s.execute("panel", &json!({ "handle": l }))).contains("closed"));
        assert_eq!(s.doc().unwrap().model.iter().count(), 1);
    }

    #[test]
    fn part_names_solids_and_leaves_locked_ones() {
        let mut s = Session::new();
        let a = block(&mut s, [1.0, 1.0, 1.0], "", "");
        let l = s.execute("line", &json!({ "points": [[0, 0], [5, 0]] })).unwrap()["handles"][0].clone();
        assert!(err(s.execute("part", &json!({ "handles": [a.hex()] }))).contains("name"));
        assert!(err(s.execute("part", &json!({ "handles": [l], "name": "x" }))).contains("select 3D solids"));
        s.execute("part", &json!({ "handles": [a.hex()], "name": "측판", "material": "Birch 18" })).unwrap();
        s.execute("part", &json!({ "handles": [a.hex()], "material": "Oak" })).unwrap();
        let part = |s: &Session| match &s.doc().unwrap().entity(a).unwrap().kind {
            EntityKind::Solid3d(x) => x.part.clone(),
            _ => panic!(),
        };
        assert_eq!(part(&s), Part { name: "측판".into(), material: "Oak".into() });
        s.undo().unwrap();
        assert_eq!(part(&s).material, "Birch 18");
        s.doc_mut().unwrap().layer_mut("0").unwrap().locked = true;
        assert!(err(s.execute("part", &json!({ "handles": [a.hex()], "name": "y" }))).contains("locked"));
    }

    #[test]
    fn cut_list_groups_sorts_and_rounds() {
        let mut s = Session::new();
        block(&mut s, [600.0, 18.0, 400.04], "Side", "Birch");
        block(&mut s, [18.0, 600.0, 399.96], "Side", "Birch");
        block(&mut s, [500.0, 300.0, 18.0], "Shelf", "Birch");
        block(&mut s, [800.0, 400.0, 9.0], "Back", "Hardboard");
        let r = s.execute("cutlist", &json!({})).unwrap();
        let rows = r["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 3, "{r}");
        // By material, then name: Birch Shelf, Birch Side (two, rounded alike), Hardboard Back.
        assert_eq!((rows[0]["name"].as_str(), rows[1]["name"].as_str(), rows[2]["name"].as_str()), (Some("Shelf"), Some("Side"), Some("Back")));
        assert_eq!((rows[1]["length"].as_f64(), rows[1]["width"].as_f64(), rows[1]["thickness"].as_f64()), (Some(600.0), Some(400.0), Some(18.0)));
        assert_eq!(rows[1]["quantity"], 2);
        assert_eq!(r["total"], 4);
        // Ungrouped: one row each.
        assert_eq!(s.execute("cutlist", &json!({ "group": false })).unwrap()["rows"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn a_scaled_solid_lists_its_scaled_size() {
        let mut s = Session::new();
        let a = block(&mut s, [100.0, 50.0, 10.0], "P", "M");
        s.execute("scale", &json!({ "handles": [a.hex()], "base": [0, 0], "factor": 2 })).unwrap();
        let r = s.execute("cutlist", &json!({})).unwrap();
        assert_eq!(r["rows"][0]["length"].as_f64(), Some(200.0), "{r}");
        assert_eq!(r["rows"][0]["thickness"].as_f64(), Some(20.0), "{r}");
    }

    #[test]
    fn cut_list_writes_csv_with_a_bom_and_a_table() {
        let mut s = Session::new();
        block(&mut s, [600.0, 400.0, 18.0], "측판, 좌", "자작 \"18\"");
        block(&mut s, [300.0, 200.0, 9.0], "=cmd", "");
        let dir = std::env::temp_dir().join(format!("cadino-cutlist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cut.csv");
        let r = s.execute("cutlist", &json!({ "csv": path.to_str().unwrap(), "at": [0, 0] })).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..3], b"\xEF\xBB\xBF");
        let text = String::from_utf8(bytes[3..].to_vec()).unwrap();
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert_eq!(lines[0], "Name,Material,Length,Width,Thickness,Qty");
        assert!(lines.contains(&"'=cmd,,300.0,200.0,9.0,1"), "{text}");
        assert!(lines.contains(&"\"측판, 좌\",\"자작 \"\"18\"\"\",600.0,400.0,18.0,1"), "{text}");
        assert!(lines.contains(&"Total,,,,,2"), "{text}");
        // The table: title, header, two rows and the total.
        let h = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
        let EntityKind::Table(t) = &s.doc().unwrap().entity(h).unwrap().kind else { panic!() };
        assert_eq!(t.cells.len(), 5);
        assert_eq!(t.cells[2][0].text, "=cmd");
        std::fs::remove_dir_all(&dir).ok();
        // A file that can't be written: an error, and no table.
        let n = s.doc().unwrap().model.iter().count();
        assert!(s.execute("cutlist", &json!({ "csv": dir.join("no/such/dir/x.csv").to_str().unwrap(), "at": [0, 0] })).is_err());
        assert_eq!(s.doc().unwrap().model.iter().count(), n);
    }

    #[test]
    fn cut_list_without_solids_is_an_error() {
        let mut s = Session::new();
        assert!(s.execute("cutlist", &json!({})).is_err());
        assert!(s.execute("viewbase", &json!({})).is_err());
        assert!(s.execute("cutlist", &json!({ "handles": "x" })).is_err());
    }

    #[test]
    fn hostile_viewbase_parameters_are_refused() {
        let mut s = Session::new();
        block(&mut s, [10.0, 10.0, 10.0], "", "");
        for p in [
            json!({ "views": ["side"] }),
            json!({ "views": [] }),
            json!({ "views": "front" }),
            json!({ "scale": 0 }),
            json!({ "scale": -2 }),
            json!({ "gap": -1 }),
            json!({ "projection": "second" }),
            json!({ "at": [1] }),
            json!({ "scale": "1:0" }),
            json!({ "scale": "a:b" }),
            json!({ "scale": [1, 2] }),
            json!({ "space": "sheet" }),
            json!({ "layout": 5 }),
            json!({ "layout": "a/b" }),
            json!({ "at": [1e308, 0], "scale": 1e-300 }),
        ] {
            assert!(s.execute("viewbase", &p).is_err(), "{p}");
        }
        assert_eq!(s.doc().unwrap().model.iter().count(), 1);
        assert!(s.doc().unwrap().layouts.iter().all(|l| l.name != VIEWS_LAYOUT), "no layout is left behind");
    }

    #[test]
    fn hostile_meshes_never_panic() {
        let mut s = Session::new();
        let n = f64::NAN;
        for mesh in [
            Mesh3 { verts: vec![Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0)], tris: vec![[0, 1, 99], [u32::MAX, 0, 1]], ..Default::default() },
            Mesh3 {
                verts: vec![Vec3::new(n, 0.0, 0.0), Vec3::new(1.0, 2.0, 3.0)],
                edges: vec![vec![Vec3::new(f64::INFINITY, 0.0, 0.0)]],
                ..Default::default()
            },
            Mesh3 {
                verts: vec![Vec3::new(1e300, -1e300, 0.0), Vec3::new(-1e300, 1e300, 1.0)],
                tris: vec![[0, 1, 0]],
                edges: vec![vec![Vec3::ZERO; 3]],
                ..Default::default()
            },
            Mesh3 { verts: vec![Vec3::ZERO; 4], tris: vec![[0, 1, 2], [1, 2, 3]], edges: vec![vec![], vec![Vec3::ZERO]], ..Default::default() },
        ] {
            s.add_entity(EntityKind::Solid3d(Solid3d { mesh: Arc::new(mesh), ..Default::default() })).unwrap();
        }
        // `execute` turns a panic into an internal error: none may come back.
        for (id, p) in [
            ("viewbase", json!({ "views": ["front", "top", "right", "iso"] })),
            ("cutlist", json!({})),
            ("viewbase", json!({ "at": [1e308, 1e308], "scale": 1e300 })),
        ] {
            let r = s.execute(id, &p);
            assert!(!matches!(r, Err(EngineError::Internal(..))), "{id} {p}: {r:?}");
        }
    }

    #[test]
    fn prompts() {
        let mut s = Session::new();
        let msg = |s: &Session| s.current_prompt().map(|p| p.display()).unwrap_or_default();
        s.start("panel").unwrap();
        assert!(msg(&s).contains("first corner or [Object]"), "{}", msg(&s));
        s.input(Input::Point(Vec2::ZERO)).unwrap();
        s.input(Input::Point(Vec2::new(600.0, 400.0))).unwrap();
        assert!(msg(&s).contains("thickness or [9/12/15/18/24] <18>"), "{}", msg(&s));
        s.input(Input::Keyword("15".into())).unwrap();
        assert!(msg(&s).contains("[Top/Front/Right] <Top>"), "{}", msg(&s));
        s.cmdline("f").unwrap();
        assert!(msg(&s).contains("base point"), "{}", msg(&s));
        s.input(Input::Enter).unwrap();
        assert!(msg(&s).contains("insertion point"), "{}", msg(&s));
        s.cancel();
        let a = block(&mut s, [1.0, 1.0, 1.0], "Old", "");
        s.set_selection(vec![a]);
        s.start("part").unwrap();
        assert!(msg(&s).contains("part name <Old>"), "{}", msg(&s));
        s.cmdline("Top panel").unwrap();
        assert!(msg(&s).contains("material"), "{}", msg(&s));
        s.cmdline("Birch plywood 18").unwrap();
        assert!(s.current_prompt().is_none());
        let EntityKind::Solid3d(x) = &s.doc().unwrap().entity(a).unwrap().kind else { panic!() };
        assert_eq!(x.part, Part { name: "Top panel".into(), material: "Birch plywood 18".into() });
        s.set_selection(Vec::new());
        s.start("viewbase").unwrap();
        assert!(msg(&s).contains("[Entire model]"), "{}", msg(&s));
        s.input(Input::Enter).unwrap();
        assert!(msg(&s).contains("layout name"), "{}", msg(&s));
        s.input(Input::Enter).unwrap();
        assert!(msg(&s).contains("location of base view or [Scale]"), "{}", msg(&s));
        s.cancel();
        s.execute("layout.set", &json!({ "name": "Model" })).unwrap();
        s.start("cutlist").unwrap();
        s.input(Input::Keyword("Entire model".into())).unwrap();
        s.input(Input::Point(Vec2::new(5000.0, 0.0))).unwrap();
        assert!(s.current_prompt().is_none());
        assert!(s.doc().unwrap().model.iter().any(|e| matches!(e.kind, EntityKind::Table(_))));
    }

    /// A designer turns the doors' layer off to look inside: the cut list still has the doors;
    /// VIEWBASE draws what is shown.
    #[test]
    fn cut_list_takes_solids_on_every_layer() {
        let mut s = Session::new();
        block(&mut s, [600.0, 400.0, 18.0], "Side", "Birch");
        let door = block(&mut s, [700.0, 500.0, 18.0], "Door", "Oak");
        let shelf = block(&mut s, [500.0, 300.0, 18.0], "Shelf", "Birch");
        let d = s.doc_mut().unwrap();
        d.layers.push(Layer { on: false, ..Layer::new("DOORS") });
        d.layers.push(Layer { frozen: true, ..Layer::new("SHELVES") });
        d.modify_entity(door, |e| e.common.layer = "DOORS".into()).unwrap();
        d.modify_entity(shelf, |e| e.common.layer = "SHELVES".into()).unwrap();
        assert_eq!(s.execute("cutlist", &json!({})).unwrap()["total"], 3);
        s.doc_mut().unwrap().layer_mut("0").unwrap().on = false;
        assert!(err(s.execute("viewbase", &json!({}))).contains("no 3D solids"));
    }

    /// Two boxes, one behind the other seen from the front: hidden lines to draw.
    fn boxes(s: &mut Session) {
        for (lo, hi) in [(Vec3::ZERO, Vec3::new(100.0, 60.0, 20.0)), (Vec3::new(20.0, 80.0, 5.0), Vec3::new(40.0, 90.0, 10.0))] {
            s.add_entity(EntityKind::Solid3d(hlr::tests::cube(lo, hi))).unwrap();
        }
    }

    fn hidden_linetypes(s: &Session) -> Vec<String> {
        s.doc().unwrap().model.iter().filter(|e| e.common.layer == "HIDDEN").map(|e| e.common.linetype.clone()).collect()
    }

    #[test]
    fn hidden_lines_are_dashed_on_any_hidden_layer() {
        // A new HIDDEN layer takes the dashed linetype; the lines go ByLayer.
        let mut s = Session::new();
        boxes(&mut s);
        s.execute("viewbase", &json!({ "views": ["front"], "dims": false, "space": "model" })).unwrap();
        let lts = hidden_linetypes(&s);
        assert!(!lts.is_empty() && lts.iter().all(|l| l.eq_ignore_ascii_case("ByLayer")), "{lts:?}");
        assert_eq!(s.doc().unwrap().layer("HIDDEN").unwrap().linetype, "HIDDEN");
        // A HIDDEN layer the drawing already had, continuous: the lines are dashed themselves.
        let mut s = Session::new();
        boxes(&mut s);
        s.doc_mut().unwrap().layers.push(Layer::new("HIDDEN"));
        s.execute("viewbase", &json!({ "views": ["front"], "dims": false, "space": "model" })).unwrap();
        let lts = hidden_linetypes(&s);
        assert!(!lts.is_empty() && lts.iter().all(|l| l == "HIDDEN"), "{lts:?}");
        assert!(s.doc().unwrap().linetype("HIDDEN").is_some_and(|l| !l.pattern.is_empty()));
    }

    /// A view drawn at half size (two model units per drawn unit) measures the model's true size
    /// (DIMLFAC 2).
    #[test]
    fn scaled_views_dimension_true_sizes() {
        let mut s = Session::new();
        boxes(&mut s);
        let r = s.execute("viewbase", &json!({ "views": ["front"], "scale": "1:2", "at": [0, 0], "space": "model" })).unwrap();
        assert_eq!(r["scale"], 2.0);
        assert_eq!(r["views"][0]["max"], json!([50.0, 10.0]), "{r}");
        let d = s.doc().unwrap();
        let read: Vec<(f64, f64)> = d
            .model
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Dimension(dm) => {
                    let drawn = (dm.p14 - dm.p13).len();
                    Some((drawn, drawn * cadcraft_doc::DimStyle::default().with_overrides(&dm.overrides).linear_factor))
                }
                _ => None,
            })
            .collect();
        assert_eq!(read.len(), 2, "{read:?}");
        for want in [100.0, 20.0] {
            assert!(read.iter().any(|(drawn, v)| (drawn - want / 2.0).abs() < 1e-6 && (v - want).abs() < 1e-6), "{want}: {read:?}");
        }
    }

    fn msg(s: &Session) -> String {
        s.current_prompt().map(|p| p.display()).unwrap_or_default()
    }

    /// The bounds of a view group's visible lines.
    fn view_box(s: &Session, group: &str) -> Bounds2 {
        let d = s.doc().unwrap();
        let g = d.groups.iter().find(|g| g.name == group).unwrap();
        Bounds2::from_points(g.members.iter().filter_map(|h| d.entity(*h)).filter(|e| e.common.layer == "VISIBLE").flat_map(|e| e.grips()))
    }

    /// The objects in a layout's paper space.
    fn on_sheet(s: &Session, layout: &str) -> Vec<EntityKind> {
        s.doc().unwrap().space(&Space::Paper(layout.into())).map(|st| st.iter().map(|e| e.kind.clone()).collect()).unwrap_or_default()
    }

    /// VIEWBASE at the prompts from the Model tab: the layout (made and made current), the Scale
    /// option (a bad one asked again; a ratio taken), then the location on the sheet.
    #[test]
    fn viewbase_takes_a_scale_at_the_prompt() {
        let mut s = Session::new();
        boxes(&mut s);
        s.start("viewbase").unwrap();
        s.input(Input::Enter).unwrap();
        assert!(msg(&s).contains("layout name to make current or [Model] <Drawing views>"), "{}", msg(&s));
        s.cmdline("a/b").unwrap();
        assert!(msg(&s).contains("layout name"), "a bad name is asked again: {}", msg(&s));
        s.input(Input::Enter).unwrap();
        assert_eq!(s.layout_space(), Space::Paper(VIEWS_LAYOUT.into()), "the layout is made current");
        s.cmdline("s").unwrap();
        assert!(msg(&s).contains("Enter scale <Fit>"), "{}", msg(&s));
        s.cmdline("-2").unwrap();
        assert!(msg(&s).contains("Enter scale"), "a bad scale is asked again: {}", msg(&s));
        s.cmdline("1:2").unwrap();
        assert!(msg(&s).contains("location of base view or [Scale] <Auto>"), "{}", msg(&s));
        s.input(Input::Point(Vec2::new(50.0, 40.0))).unwrap();
        assert!(s.current_prompt().is_none());
        let b = view_box(&s, "VIEW front");
        assert!(b.min.near(Vec2::new(50.0, 40.0), 1e-6) && b.max.near(Vec2::new(100.0, 50.0), 1e-6), "half size: {b:?}");
        assert!(on_sheet(&s, VIEWS_LAYOUT).iter().any(|k| matches!(k, EntityKind::Dimension(_))));
        assert_eq!(s.doc().unwrap().model.iter().count(), 2, "model space has the solids only");
        // Started in the layout, the views go on it; Model at the layout prompt keeps them in
        // model space, placed beside the model with Enter.
        s.start("viewbase").unwrap();
        s.input(Input::Enter).unwrap();
        assert!(msg(&s).contains("location of base view"), "{}", msg(&s));
        s.cancel();
        s.execute("layout.set", &json!({ "name": "Model" })).unwrap();
        s.start("viewbase").unwrap();
        s.input(Input::Enter).unwrap();
        s.cmdline("m").unwrap();
        assert!(msg(&s).contains("location of base view"), "{}", msg(&s));
        s.input(Input::Enter).unwrap();
        assert!(s.current_prompt().is_none());
        let b = view_box(&s, "VIEW front 2");
        assert!(b.min.near(Vec2::new(200.0, 0.0), 1e-6) && b.max.near(Vec2::new(300.0, 20.0), 1e-6), "beside the model, full size: {b:?}");
        assert!(s.doc().unwrap().model.iter().count() > 2);
    }

    /// A wardrobe on a new A4 layout: 1:20 is the largest standard scale that fits, the views
    /// are centred on the sheet, and the dimensions read model sizes with 2.5 mm text. The
    /// command stays in the Model tab, and one undo takes it all back, the layout too.
    #[test]
    fn views_go_on_a_sheet_sized_for_the_paper() {
        for metric in [true, false] {
            let mut s = Session::new();
            s.new_drawing(metric);
            s.add_entity(EntityKind::Solid3d(hlr::tests::cube(Vec3::ZERO, Vec3::new(2000.0, 600.0, 1800.0)))).unwrap();
            let before = s.doc().unwrap().clone();
            let r = s.execute("viewbase", &json!({})).unwrap();
            assert_eq!(r["layout"], VIEWS_LAYOUT, "{r}");
            assert_eq!(s.layout_space(), Space::Model, "the JSON form switches no tab");
            let d = s.doc().unwrap();
            let sheet = cadcraft_render::sheet(d, VIEWS_LAYOUT).unwrap();
            let k = r["scale"].as_f64().unwrap();
            if metric {
                assert_eq!(k, 20.0, "{r}");
            }
            assert!(standard_scales().any(|x| x == k), "{k}");
            // Everything on the sheet, within its printable area.
            let objs = on_sheet(&s, VIEWS_LAYOUT);
            assert_eq!(d.model.iter().count(), 1);
            let all = Bounds2::from_points(objs.iter().flat_map(|k| match k {
                EntityKind::Dimension(dm) => vec![dm.defpt.xy(), dm.p13.xy(), dm.p14.xy()],
                k => k.grips(),
            }));
            let room = sheet.printable;
            assert!(all.min.x >= room.min.x && all.min.y >= room.min.y && all.max.x <= room.max.x && all.max.y <= room.max.y, "{all:?} in {room:?}");
            let front = view_box(&s, "VIEW front");
            assert!(((front.max.x - front.min.x) * k - 2000.0).abs() < 1e-6, "{front:?}");
            // Dimensions: model sizes, 2.5 mm text on the paper.
            let mut read = Vec::new();
            for k in &objs {
                let EntityKind::Dimension(dm) = k else { continue };
                let st = d.dim_style(&dm.style).cloned().unwrap_or_default().with_overrides(&dm.overrides);
                assert!((st.text_height * st.scale - 2.5 / sheet.unit_mm).abs() < 1e-9, "{} {}", st.text_height, st.scale);
                read.push(((dm.p14 - dm.p13).len() * st.linear_factor).round());
            }
            for want in [2000.0, 1800.0, 600.0] {
                assert!(read.contains(&want), "{want}: {read:?}");
            }
            s.undo().unwrap();
            assert_eq!(*s.doc().unwrap(), before);
        }
    }

    /// Restarts after a failure at the end: PART (the layer locked meanwhile), CUTLIST (only
    /// surfaces to list) and VIEWBASE (solids with no size) go back to the selection; VIEWBASE
    /// asks again for a selection with no solid shown.
    #[test]
    fn failures_at_the_end_start_again_from_the_selection() {
        let mut s = Session::new();
        let a = block(&mut s, [1.0, 1.0, 1.0], "", "");
        s.set_selection(Vec::new());
        s.start("part").unwrap();
        s.input(Input::Pick(vec![a])).unwrap();
        s.input(Input::Enter).unwrap();
        s.cmdline("Side").unwrap();
        s.doc_mut().unwrap().layer_mut("0").unwrap().locked = true;
        let _ = s.cmdline("Oak");
        assert!(msg(&s).contains("Select objects"), "{}", msg(&s));
        s.cancel();
        s.doc_mut().unwrap().layer_mut("0").unwrap().locked = false;
        // An open mesh: a surface, not a part.
        s.doc_mut()
            .unwrap()
            .modify_entity(a, |e| {
                if let EntityKind::Solid3d(x) = &mut e.kind {
                    let mut m = (*x.mesh).clone();
                    m.open = true;
                    x.mesh = Arc::new(m);
                }
            })
            .unwrap();
        s.start("cutlist").unwrap();
        s.input(Input::Enter).unwrap();
        let _ = s.input(Input::Point(Vec2::ZERO));
        assert!(msg(&s).contains("Select solids"), "{}", msg(&s));
        s.cancel();
        // A solid of one point: no size to draw.
        let dot = Solid3d { mesh: Arc::new(Mesh3 { verts: vec![Vec3::new(5.0, 5.0, 5.0)], ..Default::default() }), ..Default::default() };
        let p = s.add_entity(EntityKind::Solid3d(dot)).unwrap();
        s.start("viewbase").unwrap();
        s.input(Input::Pick(vec![p])).unwrap();
        s.input(Input::Enter).unwrap();
        s.cmdline("m").unwrap();
        let _ = s.input(Input::Enter);
        assert!(msg(&s).contains("Select solids"), "{}", msg(&s));
        s.cancel();
        // Every solid hidden: the entire model has none to draw.
        s.doc_mut().unwrap().layer_mut("0").unwrap().on = false;
        s.start("viewbase").unwrap();
        let _ = s.input(Input::Enter);
        assert!(msg(&s).contains("Select solids"), "{}", msg(&s));
        s.cancel();
        assert_eq!(s.doc().unwrap().model.iter().count(), 2);
    }

    /// Views that fail part way through drawing leave the drawing as it was.
    #[test]
    fn a_view_failing_part_way_puts_the_drawing_back() {
        let mut s = Session::new();
        let before = s.doc().unwrap().clone();
        let line = vec![vec![Vec2::ZERO, Vec2::new(10.0, 0.0)]];
        let good = Bounds2 { min: Vec2::ZERO, max: Vec2::new(10.0, 5.0) };
        // The second view's size can't be dimensioned.
        let bad = Bounds2 { min: Vec2::ZERO, max: Vec2::new(f64::INFINITY, 5.0) };
        let views: Vec<View> = vec![("front", line.clone(), Vec::new(), good), ("top", line, Vec::new(), bad)];
        let dims = Dims { on: true, off: 1.0, k: 1.0, text: None };
        assert!(all_or_nothing(&mut s, |s| draw_views(s, views, &dims)).is_err());
        assert_eq!(*s.doc().unwrap(), before);
    }

    /// Bad input is asked for again where it was given; nothing loops on a stale value.
    #[test]
    fn bad_picks_are_asked_for_again() {
        let mut s = Session::new();
        let line = Handle::parse_hex(s.execute("line", &json!({ "points": [[0, 0], [5, 0]] })).unwrap()["handles"][0].as_str().unwrap()).unwrap();
        // PANEL: an open curve is no profile; equal corners make no rectangle.
        s.start("panel").unwrap();
        s.input(Input::Enter).unwrap();
        s.input(Input::Pick(vec![line])).unwrap();
        assert!(msg(&s).contains("Select closed profile"), "{}", msg(&s));
        s.cancel();
        s.start("panel").unwrap();
        s.input(Input::Point(Vec2::ZERO)).unwrap();
        s.input(Input::Point(Vec2::new(0.0, 5.0))).unwrap();
        assert!(msg(&s).contains("other corner"), "{}", msg(&s));
        s.cancel();
        // A panel that can't be made at the end (out of range) starts again from the profile.
        s.start("panel").unwrap();
        s.input(Input::Point(Vec2::new(-1e308, 0.0))).unwrap();
        s.input(Input::Point(Vec2::new(1e308, 5.0))).unwrap();
        for _ in 0..4 {
            s.input(Input::Enter).unwrap();
        }
        assert!(msg(&s).contains("first corner"), "{}", msg(&s));
        s.cancel();
        // PART and CUTLIST: a selection with no solid is asked for again.
        s.set_selection(Vec::new());
        s.start("part").unwrap();
        s.input(Input::Pick(vec![line])).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(msg(&s).contains("Select objects"), "{}", msg(&s));
        let a = block(&mut s, [1.0, 1.0, 1.0], "", "");
        s.input(Input::Pick(vec![a])).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(msg(&s).contains("part name"), "{}", msg(&s));
        s.cancel();
        s.set_selection(Vec::new());
        s.start("cutlist").unwrap();
        s.input(Input::Pick(vec![line])).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(msg(&s).contains("Select solids"), "{}", msg(&s));
        s.input(Input::Pick(vec![a])).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(msg(&s).contains("insertion point for the table"), "{}", msg(&s));
        s.cancel();
        assert_eq!(s.doc().unwrap().model.iter().count(), 2);
    }

    /// A box mesh `size` big, turned by `turn` in its own model coordinates (as PANEL leaves a
    /// board drawn at an angle: the turn is in the mesh, not the transform).
    fn turned_board(s: &mut Session, size: Vec3, turn: Mat4, name: &str) -> Handle {
        let x = hlr::tests::cube(Vec3::ZERO, size);
        let mut m = (*x.mesh).clone();
        m.verts.iter_mut().for_each(|p| *p = turn.apply(*p));
        m.edges.iter_mut().flatten().for_each(|p| *p = turn.apply(*p));
        let x = Solid3d { mesh: Arc::new(m), part: Part { name: name.into(), material: "Birch".into() }, ..Default::default() };
        s.add_entity(EntityKind::Solid3d(x)).unwrap()
    }

    fn sizes(r: &Value) -> Vec<(f64, f64, f64, u64)> {
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["length"].as_f64().unwrap(), r["width"].as_f64().unwrap(), r["thickness"].as_f64().unwrap(), r["quantity"].as_u64().unwrap()))
            .collect()
    }

    /// A board drawn at 30° in plan, one standing at 45° in plan, one tilted 30° about X and
    /// then turned: each lists its board size, not its world box.
    #[test]
    fn cut_lists_measure_turned_boards_along_their_own_axes() {
        let mut s = Session::new();
        let board = Vec3::new(600.0, 400.0, 18.0);
        let z30 = Mat4::rotation(Vec3::Z, 30f64.to_radians());
        turned_board(&mut s, board, z30, "A");
        turned_board(
            &mut s,
            board,
            Mat4::rotation(Vec3::Z, 45f64.to_radians()).mul(&Mat4::rotation(Vec3::new(1.0, 0.0, 0.0), 90f64.to_radians())),
            "B",
        );
        turned_board(&mut s, board, z30.mul(&Mat4::rotation(Vec3::new(1.0, 0.0, 0.0), 30f64.to_radians())), "C");
        let r = s.execute("cutlist", &json!({})).unwrap();
        assert_eq!(sizes(&r), vec![(600.0, 400.0, 18.0, 1); 3], "{r}");
        // Turned again by ROTATE (its transform): the same.
        let h = turned_board(&mut s, board, z30, "A");
        s.execute("rotate", &json!({ "handles": [h.hex()], "base": [0, 0], "angle": 17 })).unwrap();
        let r = s.execute("cutlist", &json!({})).unwrap();
        assert_eq!(sizes(&r).first(), Some(&(600.0, 400.0, 18.0, 2)), "{r}");
    }

    /// A drawer box made into a block and inserted three times (once as a two-copy array), and
    /// inside another block once more: every copy counts, where it is; VIEWBASE draws them, even
    /// when the model holds only references. A reference on a layer that is off is listed, not
    /// drawn.
    #[test]
    fn cut_lists_and_views_take_solids_in_block_references() {
        let mut s = Session::new();
        let part = |s: &mut Session, hi: Vec3, name: &str| {
            let x = Solid3d { part: Part { name: name.into(), material: "Birch".into() }, ..hlr::tests::cube(Vec3::ZERO, hi) };
            s.add_entity(EntityKind::Solid3d(x)).unwrap()
        };
        let f = part(&mut s, Vec3::new(500.0, 18.0, 200.0), "Front");
        let sd = part(&mut s, Vec3::new(18.0, 450.0, 200.0), "Side");
        s.execute("block", &json!({ "name": "DRAWER", "base": [0, 0], "handles": [f.hex(), sd.hex()], "keep": "delete" })).unwrap();
        assert!(err(s.execute("cutlist", &json!({}))).contains("no 3D solids"), "the model has none yet");
        let ins = |s: &mut Session, at: [f64; 2]| {
            Handle::parse_hex(s.execute("insert", &json!({ "name": "DRAWER", "at": at })).unwrap()["handle"].as_str().unwrap()).unwrap()
        };
        let a = ins(&mut s, [0.0, 0.0]);
        ins(&mut s, [0.0, 1000.0]);
        s.doc_mut()
            .unwrap()
            .modify_entity(a, |e| {
                if let EntityKind::Insert(i) = &mut e.kind {
                    i.cols = 2;
                    i.col_spacing = 1000.0;
                }
            })
            .unwrap();
        let r = s.execute("cutlist", &json!({})).unwrap();
        assert_eq!(r["total"], 6, "{r}");
        assert_eq!(sizes(&r), vec![(500.0, 200.0, 18.0, 3), (450.0, 200.0, 18.0, 3)], "{r}");
        // Given by handle: the reference stands for its solids.
        assert_eq!(s.execute("cutlist", &json!({ "handles": [a.hex()] })).unwrap()["total"], 4);
        // Nested: a unit holding a drawer.
        let inner = ins(&mut s, [5000.0, 0.0]);
        s.execute("block", &json!({ "name": "UNIT", "base": [0, 0], "handles": [inner.hex()], "keep": "delete" })).unwrap();
        let unit = Handle::parse_hex(s.execute("insert", &json!({ "name": "UNIT", "at": [0, 3000] })).unwrap()["handle"].as_str().unwrap()).unwrap();
        assert_eq!(s.execute("cutlist", &json!({})).unwrap()["total"], 8);
        // VIEWBASE draws the copies where they are: the top view spans the array and the unit.
        let r = s.execute("viewbase", &json!({ "views": ["top"], "space": "model", "dims": false, "at": [0, -9000] })).unwrap();
        let v = &r["views"][0];
        let (w, h) = (v["max"][0].as_f64().unwrap() - v["min"][0].as_f64().unwrap(), v["max"][1].as_f64().unwrap() - v["min"][1].as_f64().unwrap());
        assert!((w - 5500.0).abs() < 1e-6 && (h - 3450.0).abs() < 1e-6, "{r}");
        // Off: still in the cut list, not in the views.
        s.doc_mut().unwrap().layers.push(Layer { on: false, ..Layer::new("HIDE") });
        s.doc_mut().unwrap().modify_entity(unit, |e| e.common.layer = "HIDE".into()).unwrap();
        assert_eq!(s.execute("cutlist", &json!({})).unwrap()["total"], 8);
        let r = s.execute("viewbase", &json!({ "views": ["top"], "space": "model", "dims": false, "at": [0, -9000] })).unwrap();
        let v = &r["views"][0];
        assert!((v["max"][1].as_f64().unwrap() - v["min"][1].as_f64().unwrap() - 1450.0).abs() < 1e-6, "{r}");
    }

    /// Hostile references: a block holding a reference to itself, and a huge array, end.
    #[test]
    fn hostile_block_references_end() {
        let mut s = Session::new();
        let a = block(&mut s, [10.0, 10.0, 10.0], "P", "");
        s.execute("block", &json!({ "name": "LOOP", "base": [0, 0], "handles": [a.hex()], "keep": "delete" })).unwrap();
        let h = Handle::parse_hex(s.execute("insert", &json!({ "name": "LOOP", "at": [0, 0] })).unwrap()["handle"].as_str().unwrap()).unwrap();
        let me = s.doc().unwrap().entity(h).unwrap().as_ref().clone();
        Arc::make_mut(s.doc_mut().unwrap().blocks.get_mut("LOOP").unwrap()).entities.push(me);
        s.doc_mut()
            .unwrap()
            .modify_entity(h, |e| {
                if let EntityKind::Insert(i) = &mut e.kind {
                    (i.cols, i.rows) = (u32::MAX, u32::MAX);
                }
            })
            .unwrap();
        let r = s.execute("cutlist", &json!({ "group": false }));
        assert!(!matches!(r, Err(EngineError::Internal(..))), "{r:?}");
        let total = r.unwrap()["total"].as_u64().unwrap();
        assert!(total > 0 && total <= MAX_PARTS as u64, "{total}");
    }

    /// A reference `name` at the origin, changed by `f`.
    fn reference(s: &mut Session, name: &str, f: impl FnOnce(&mut cadcraft_doc::Insert)) -> Handle {
        let h = Handle::parse_hex(s.execute("insert", &json!({ "name": name, "at": [0, 0] })).unwrap()["handle"].as_str().unwrap()).unwrap();
        s.doc_mut()
            .unwrap()
            .modify_entity(h, |e| {
                if let EntityKind::Insert(i) = &mut e.kind {
                    f(i);
                }
            })
            .unwrap();
        h
    }

    /// A board in a block inserted with an uneven scale lists stretched, as it is drawn: along Z
    /// it is thicker, along X longer, and turned as well it still measures along its own axes.
    #[test]
    fn cut_lists_measure_unevenly_scaled_references_as_drawn() {
        let mut s = Session::new();
        let x =
            Solid3d { part: Part { name: "Board".into(), material: "Birch".into() }, ..hlr::tests::cube(Vec3::ZERO, Vec3::new(600.0, 400.0, 18.0)) };
        let b = s.add_entity(EntityKind::Solid3d(x)).unwrap();
        s.execute("block", &json!({ "name": "BOARD", "base": [0, 0], "handles": [b.hex()], "keep": "delete" })).unwrap();
        let only = |s: &mut Session, h: Handle| sizes(&s.execute("cutlist", &json!({ "handles": [h.hex()] })).unwrap());
        let h = reference(&mut s, "BOARD", |i| i.scale = Vec3::new(1.0, 1.0, 1.5));
        assert_eq!(only(&mut s, h), vec![(600.0, 400.0, 27.0, 1)]);
        let h = reference(&mut s, "BOARD", |i| i.scale = Vec3::new(2.0, 1.0, 1.0));
        assert_eq!(only(&mut s, h), vec![(1200.0, 400.0, 18.0, 1)]);
        let h = reference(&mut s, "BOARD", |i| {
            i.scale = Vec3::new(2.0, 1.0, 1.0);
            i.rotation = 30f64.to_radians();
        });
        assert_eq!(only(&mut s, h), vec![(1200.0, 400.0, 18.0, 1)]);
        // Mirrored and stretched.
        let h = reference(&mut s, "BOARD", |i| i.scale = Vec3::new(-1.0, 0.5, 1.0));
        assert_eq!(only(&mut s, h), vec![(600.0, 200.0, 18.0, 1)]);
    }

    /// A million copies of a block of linework are passed by without looking into them; a block
    /// holding a solid among much linework is looked into only so far.
    #[test]
    fn block_arrays_without_solids_are_passed_by() {
        let mut s = Session::new();
        let l = s.execute("line", &json!({ "points": [[0, 0], [5, 0]] })).unwrap()["handles"][0].clone();
        s.execute("block", &json!({ "name": "LINES", "base": [0, 0], "handles": [l], "keep": "delete" })).unwrap();
        let mut blk = s.doc().unwrap().block("LINES").unwrap().as_ref().clone();
        let line = blk.entities.iter().next().unwrap().as_ref().clone();
        for i in 1..20_000 {
            blk.entities.push(Entity { handle: Handle(0x10_0000 + i), ..line.clone() });
        }
        s.doc_mut().unwrap().blocks.insert("LINES".into(), Arc::new(blk));
        let grid = |i: &mut cadcraft_doc::Insert| {
            (i.rows, i.cols) = (1000, 1000);
            (i.row_spacing, i.col_spacing) = (10.0, 10.0);
        };
        let h = reference(&mut s, "LINES", grid);
        let d = s.doc().unwrap();
        let mut g = Gather { d, seen: true, out: Vec::new(), copies: 0, visits: 0, solid_blocks: HashMap::new() };
        g.add(d.entity(h).unwrap(), &Mat4::IDENTITY, "0", 0);
        assert_eq!((g.out.len(), g.copies, g.visits), (0, 0, 0));
        // The same with a board in it: looked at until the cap, then left.
        let board = block(&mut s, [600.0, 400.0, 18.0], "Board", "");
        let mut blk = s.doc().unwrap().block("LINES").unwrap().as_ref().clone();
        blk.entities.push(Entity { handle: Handle(0x20_0000), ..s.doc().unwrap().entity(board).unwrap().as_ref().clone() });
        s.doc_mut().unwrap().blocks.insert("LINES".into(), Arc::new(blk));
        s.doc_mut().unwrap().remove_entity(board);
        let d = s.doc().unwrap();
        let mut g = Gather { d, seen: true, out: Vec::new(), copies: 0, visits: 0, solid_blocks: HashMap::new() };
        g.add(d.entity(h).unwrap(), &Mat4::IDENTITY, "0", 0);
        assert!(g.visits <= MAX_VISITS && g.copies <= MAX_VISITS / 20_000 + 1 && !g.out.is_empty(), "{} {} {}", g.visits, g.copies, g.out.len());
        let found = g.out.len();
        let total = s.execute("cutlist", &json!({})).unwrap()["total"].as_u64().unwrap();
        assert!(total > 0 && total as usize == found, "{total}");
    }

    /// A list longer than a table holds goes on in more tables side by side, every row kept and
    /// the total in the last.
    #[test]
    fn long_cut_lists_go_on_in_more_tables() {
        let mut s = Session::new();
        let n = ROWS_PER_TABLE * 2 + 500;
        for i in 0..n {
            block(&mut s, [100.0 + i as f64, 50.0, 18.0], "P", "");
        }
        let r = s.execute("cutlist", &json!({ "group": false, "at": [0, 0, 5] })).unwrap();
        let hs: Vec<Handle> = r["handles"].as_array().unwrap().iter().map(|h| Handle::parse_hex(h.as_str().unwrap()).unwrap()).collect();
        assert_eq!(hs.len(), 3);
        let d = s.doc().unwrap();
        let tables: Vec<&cadcraft_doc::Table> = hs
            .iter()
            .map(|h| match &d.entity(*h).unwrap().kind {
                EntityKind::Table(t) => t,
                _ => panic!("not a table"),
            })
            .collect();
        // Title and header in each, then the rows; the total last.
        assert_eq!(tables.iter().map(|t| t.cells.len() - 2).sum::<usize>(), n + 1);
        assert_eq!(tables[2].cells.last().unwrap()[0].text, "Total");
        assert_eq!(tables[1].cells[0][0].text, "Cut list (2/3)");
        // Side by side, at the height asked.
        assert!(tables.windows(2).all(|w| w[1].insert.x > w[0].insert.x && w[1].insert.y == w[0].insert.y));
        assert!(tables.iter().all(|t| t.insert.z == 5.0));
        // One undo takes every table back.
        s.undo().unwrap();
        assert!(s.doc().unwrap().model.iter().all(|e| !matches!(e.kind, EntityKind::Table(_))));
    }

    #[test]
    fn csv_fields_are_quoted_and_defused() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("+1"), "'+1");
        assert_eq!(csv_field("=1,2"), "\"'=1,2\"");
    }
}
