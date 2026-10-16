//! Furniture design: panels, part names and materials, cut lists, and drawings made from the
//! 3D model (VIEWBASE).
//!
//! A PANEL is a board whose face is a profile drawn in plan and whose thickness runs along the
//! plane's normal; the profile stays (it is the drawing). PART names a solid and gives its
//! material; CUTLIST lists the parts by name, material and size; VIEWBASE projects the solids
//! into front, top, right and isometric views with hidden lines dashed ([`hlr`]).

mod hlr;

use cadcraft_doc::{Common, EntityKind, Handle, Layer, Part, Solid3d};
use cadcraft_geom::{Bounds2, EPS, Mat4, PolyVertex, Vec2, Vec3, ViewDir};
use cadcraft_kernel::Seg;
use serde_json::{Value, json};

use super::curves::KW;
use super::machines::{SelOutcome, SelectPhase, number};
use super::solids::{all_or_nothing, distance, kernel, locked_err, p3, profile, solid, solids};
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
                 base?: [x,y] (profile point at the plane's origin; default its lower-left), at?: [x,y,z] (where that point goes), name?, material?}",
            )
            .interactive(|_| Ok(Box::new(PanelM::default()))),
        CommandSpec::new("part", "Part", run_part)
            .menu(&["Modify", "Part"])
            .params("{handles?, name?, material?}: names 3D solids as furniture parts (cut lists)")
            .interactive(|_| Ok(Box::new(PartM::default()))),
        CommandSpec::new("cutlist", "Cut List", run_cutlist)
            .menu(&["Tools", "Cut List"])
            .params(
                "{handles? (default every 3D solid in model space, on any layer), at?: [x,y] (insert a table), csv?: path (UTF-8 with BOM), group?: bool (default true)} \
                 → {rows: [{name, material, length, width, thickness, quantity}], total}",
            )
            .interactive(|_| Ok(Box::new(PickThenPoint::new(PickFor::CutList)))),
        CommandSpec::new("viewbase", "Base View", run_viewbase)
            .menu(&["Draw", "Drawing Views", "Base View"])
            .alias(&["make2d"])
            .params(
                "{handles? (default every 3D solid on a layer that is on and thawed), views?: [\"front\",\"top\",\"right\",\"iso\"] (default front, top, right), at?: [x,y] (front view's lower-left), \
                 scale? (default 1), gap? (default a tenth of the model's size), hidden?: bool (default true; the iso view shows none), \
                 dims?: bool (default true), projection?: \"third\" | \"first\" (default third)}",
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
    // The profile: an object's, or a rectangle's; with its lower-left corner.
    let (segs, lower_left) = match p.get("handle").filter(|v| !v.is_null()) {
        Some(_) => object_profile(s, targets(s, p)?.first().copied().ok_or_else(|| bad(CMD, "`handle` is required"))?)?,
        None => rect_profile(point_req(CMD, p, "corner1")?, point_req(CMD, p, "corner2")?)?,
    };
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
    let mut k = solid(built);
    if let EntityKind::Solid3d(x) = &mut k {
        x.part = Part { name: name.unwrap_or_default(), material: material.unwrap_or_default() };
    }
    let h = s.add_entity(k)?;
    Ok(json!({ "handle": h.hex() }))
}

/// A closed profile object drawn in plan: its pieces and its lower-left corner.
fn object_profile(s: &Session, h: Handle) -> Result<(Vec<Seg>, Vec2)> {
    const CMD: &str = "panel";
    let d = s.doc()?;
    let e = d.entity(h).ok_or_else(|| bad(CMD, format!("no object with handle {}", h.hex())))?;
    let n = e.common.extrusion;
    if n.x.abs() > 1e-9 || n.y.abs() > 1e-9 || n.z < 0.0 {
        return Err(bad(CMD, "the profile must be drawn in plan (on the XY plane)"));
    }
    let segs = profile(&e.kind, true)?.ok_or_else(|| bad(CMD, "the profile must be a closed polyline, circle or single-loop region"))?;
    Ok((segs, cadcraft_doc::entity_bounds(d, e, 0).min))
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
                    object_profile(s, h)?;
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
        if !self.vals.contains_key("base") {
            match i {
                Input::Point(p) => self.vals.insert("base".into(), json!([p.x, p.y])),
                Input::Enter => self.vals.insert("base".into(), Value::Null),
                _ => return Ok(Step::Continue),
            };
            return Ok(Step::Continue);
        }
        let at = match i {
            Input::Point(p) => json!([p.x, p.y, s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0)]),
            Input::Enter => json!([0, 0, 0]),
            _ => return Ok(Step::Continue),
        };
        self.vals.insert("at".into(), at);
        let r = run_panel(s, &Value::Object(self.vals.clone()));
        if r.is_err() {
            // Start again: any value given may be the one that failed, so none is kept.
            *self = PanelM::default();
        }
        r.map(|_| Step::Done)
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

/// The solids a command works on: those given, else every one in model space (`seen`: only those
/// on a layer that is on and thawed).
fn chosen(s: &Session, p: &Value, cmd: &str, seen: bool) -> Result<Vec<(Handle, Solid3d)>> {
    // An empty list (as MCP clients send optional arrays) counts as none given.
    let given = ["handle", "handles"].iter().any(|k| p.get(k).is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty())));
    let xs = if given {
        solids(s, &targets(s, p)?)?
    } else {
        let d = s.doc()?;
        let hs: Vec<Handle> = d.model.iter().filter(|e| !seen || d.layer(&e.common.layer).is_none_or(Layer::visible)).map(|e| e.handle).collect();
        solids(s, &hs)?
    };
    if xs.is_empty() { Err(bad(cmd, "there are no 3D solids")) } else { Ok(xs) }
}

/// A size in tenths of a millimetre (drawing unit).
fn tenths(v: f64) -> Option<i64> {
    let t = (v * 10.0).round();
    (t.is_finite() && t.abs() < 9.0e15).then_some(t as i64)
}

/// A transform's uniform scale.
fn scale_of(m: &Mat4) -> f64 {
    let r = &m.m;
    let det = r[0][0] * (r[1][1] * r[2][2] - r[1][2] * r[2][1]) - r[0][1] * (r[1][0] * r[2][2] - r[1][2] * r[2][0])
        + r[0][2] * (r[1][0] * r[2][1] - r[1][1] * r[2][0]);
    det.abs().cbrt()
}

/// A part's length, width and thickness in tenths, longest first: its mesh's box in its own frame.
/// ponytail: the box along the solid's own axes; a part that a boolean left turned in its frame
/// (the kernel returns world shapes) measures by its axis-aligned box, an oriented minimal box
/// when that shows in cut lists.
fn part_size(x: &Solid3d) -> Option<[i64; 3]> {
    let k = scale_of(&x.xform);
    let mut pts = x.mesh.points().filter(|p| p.is_finite());
    let first = pts.next()?;
    let (lo, hi) = pts.fold((first, first), |(lo, hi), p| {
        (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
    });
    let mut s = [tenths((hi.x - lo.x) * k)?, tenths((hi.y - lo.y) * k)?, tenths((hi.z - lo.z) * k)?];
    s.sort_unstable_by(|a, b| b.cmp(a));
    Some(s)
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
fn cut_rows(xs: &[(Handle, Solid3d)], group: bool) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for (_, x) in xs.iter().filter(|(_, x)| !x.mesh.open) {
        let Some(size) = part_size(x) else { continue };
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
    let at = match p.get("at").filter(|v| !v.is_null()) {
        Some(_) => Some(point_req(CMD, p, "at")?),
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
        let d = s.doc()?;
        let cur = d.header.str("CTABLESTYLE", "Standard");
        let st = d.table_styles.iter().find(|t| t.name.eq_ignore_ascii_case(&cur)).or(d.table_styles.first()).cloned().unwrap_or_default();
        let header = HEADER.iter().map(|h| h.to_string()).collect();
        let mut t = super::table::make_table(&st, at, cells.len(), HEADER.len(), None, None, Some("Cut list"), Some(header), &cells);
        // Columns as wide as their longest text.
        let th = t.text_height;
        for (c, w) in t.col_widths.iter_mut().enumerate() {
            let chars = std::iter::once(HEADER.get(c).map_or(0, |h| h.chars().count()))
                .chain(cells.iter().map(|r| r.get(c).map_or(0, |x| x.chars().count())))
                .max()
                .unwrap_or(0);
            *w = (chars.max(3) as f64 + 2.0) * th;
        }
        let h = s.add_entity(EntityKind::Table(t))?;
        out["handle"] = json!(h.hex());
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

fn dimension(s: &mut Session, a: Vec2, b: Vec2, at: Vec2, rotation: f64, scale: f64) -> Result<Handle> {
    let spec = find_command("dimlinear").ok_or_else(|| EngineError::UnknownCommand("dimlinear".into()))?;
    let r = (spec.run)(s, &json!({ "p1": [a.x, a.y], "p2": [b.x, b.y], "at": [at.x, at.y], "rotation": rotation }))?;
    let h = r.get("handle").and_then(Value::as_str).and_then(Handle::parse_hex).ok_or_else(|| EngineError::Other("no dimension was made".into()))?;
    // A scaled view measures true sizes.
    if (scale - 1.0).abs() > 1e-12 {
        s.doc_mut()?.modify_entity(h, |e| {
            if let EntityKind::Dimension(d) = &mut e.kind {
                d.overrides.insert("linearFactor".into(), json!(1.0 / scale));
            }
        })?;
    }
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
    let scale = size_param(CMD, p, "scale", false)?.unwrap_or(1.0);
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
    let refs: Vec<&Solid3d> = xs.iter().map(|(_, x)| x).collect();
    let ext = |n: &str| hlr::extent(&refs, view_dir(n));
    let (front, top, right) = (ext("front"), ext("top"), ext("right"));
    // The model's size: its extent along X, Y and Z.
    let size = front.width().max(front.height()).max(top.height());
    if !size.is_finite() || size <= EPS {
        return Err(bad(CMD, "the solids have no size"));
    }
    let gap = size_param(CMD, p, "gap", true)?.unwrap_or(size * scale / 10.0);
    let at = match p.get("at").filter(|v| !v.is_null()) {
        Some(_) => point_req(CMD, p, "at")?,
        // Beside the model in plan.
        None => Vec2::new(top.max.x + size, top.min.y),
    };
    let (fw, fh) = (front.width() * scale, front.height() * scale);
    // Each view's lower-left corner.
    let corner = |n: &str| -> Vec2 {
        let (w, h) = match n {
            "top" => (top.width() * scale, top.height() * scale),
            "right" => (right.width() * scale, right.height() * scale),
            _ => (fw, fh),
        };
        at + match (n, first_angle) {
            ("top", false) => Vec2::new(0.0, fh + gap),
            ("top", true) => Vec2::new(0.0, -gap - h),
            ("right", false) => Vec2::new(fw + gap, 0.0),
            ("right", true) => Vec2::new(-gap - w, 0.0),
            ("iso", _) => Vec2::new(fw + gap, fh + gap),
            _ => Vec2::ZERO,
        }
    };
    // Every view's lines before the drawing changes.
    let mut views = Vec::with_capacity(names.len());
    let mut capped: Vec<String> = Vec::new();
    for n in &names {
        let l = hlr::lines(&refs, view_dir(n));
        for c in &l.capped {
            log::warn!("VIEWBASE {n} view: {c}");
            capped.push(format!("{n}: {c}"));
        }
        let b = ext(n);
        let origin = corner(n) - b.min * scale;
        let place = |pts: &Vec<Vec2>| pts.iter().map(|q| *q * scale + origin).collect::<Vec<Vec2>>();
        let vis: Vec<Vec<Vec2>> = l.visible.iter().map(place).collect();
        // ponytail: the iso view shows visible lines only, as pictorial views do; a per-view
        // choice when someone wants dashes there.
        let hid: Vec<Vec<Vec2>> = if hidden && *n != "iso" { l.hidden.iter().map(place).collect() } else { Vec::new() };
        let placed = Bounds2 { min: corner(n), max: corner(n) + (b.max - b.min) * scale };
        views.push((*n, vis, hid, placed));
    }
    if views.iter().all(|(_, v, h, _)| v.is_empty() && h.is_empty()) {
        return Err(bad(CMD, "the solids have no edges to draw"));
    }
    all_or_nothing(s, |s| draw_views(s, views, gap, dims, scale)).map(|report| json!({ "views": report, "capped": capped }))
}

type View = (&'static str, Vec<Vec<Vec2>>, Vec<Vec<Vec2>>, Bounds2);

/// VIEWBASE's drawing: each view's lines, dimensions and group (a failure part way leaves
/// partial views, so the caller puts the drawing back).
fn draw_views(s: &mut Session, views: Vec<View>, gap: f64, dims: bool, scale: f64) -> Result<Vec<Value>> {
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
        let off = gap * 0.4;
        if dims && n != "iso" && b.width() > EPS {
            hs.push(dimension(s, b.min, Vec2::new(b.max.x, b.min.y), Vec2::new((b.min.x + b.max.x) / 2.0, b.min.y - off), 0.0, scale)?);
        }
        if dims && n != "iso" && b.height() > EPS {
            hs.push(dimension(s, b.min, Vec2::new(b.min.x, b.max.y), Vec2::new(b.min.x - off, (b.min.y + b.max.y) / 2.0), 90.0, scale)?);
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
/// view goes (VIEWBASE also takes a Scale). A selection with no solids is asked for again; a
/// failure at the end starts again from the selection.
struct PickThenPoint {
    what: PickFor,
    sel: SelectPhase,
    hs: Option<Vec<Handle>>,
    scale: Option<f64>,
    asking_scale: bool,
}

impl PickThenPoint {
    fn new(what: PickFor) -> Self {
        PickThenPoint { what, sel: SelectPhase::default(), hs: None, scale: None, asking_scale: false }
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
    fn take(&mut self, s: &mut Session, hs: Vec<Handle>) -> Result<Step> {
        if let Err(e) = chosen(s, &Self::params(&hs), self.id(), self.what == PickFor::Views) {
            self.sel = SelectPhase::default();
            return Err(e);
        }
        self.hs = Some(hs);
        Ok(Step::Continue)
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
            (Some(_), _) if self.asking_scale => Prompt::new("Enter scale", Accept::NUMBER).default(format!("{}", self.scale.unwrap_or(1.0))),
            (Some(_), PickFor::CutList) => Prompt::new("Specify insertion point for the table", Accept::POINT),
            (Some(_), PickFor::Views) => Prompt::new("Specify location of base view", Accept::POINT).kw(&["Scale"]),
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
        if self.asking_scale {
            match i {
                Input::Enter => {}
                Input::Text(t) | Input::Keyword(t) => match number(&t).filter(|v| v.is_finite() && *v > 0.0) {
                    Some(v) => self.scale = Some(v),
                    None => return Err(EngineError::Other("The scale must be a positive number.".into())),
                },
                _ => return Ok(Step::Continue),
            }
            self.asking_scale = false;
            return Ok(Step::Continue);
        }
        let at = match i {
            Input::Point(p) => p,
            Input::Keyword(t) | Input::Text(t) if self.what == PickFor::Views && t.trim().to_ascii_lowercase().starts_with('s') => {
                self.asking_scale = true;
                return Ok(Step::Continue);
            }
            _ => return Ok(Step::Continue),
        };
        let mut p = Self::params(self.hs.as_deref().unwrap_or_default());
        p["at"] = json!([at.x, at.y]);
        if let Some(k) = self.scale {
            p["scale"] = json!(k);
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
        ] {
            assert!(s.execute("viewbase", &p).is_err(), "{p}");
        }
        assert_eq!(s.doc().unwrap().model.iter().count(), 1);
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
        assert!(msg(&s).contains("location of base view or [Scale]"), "{}", msg(&s));
        s.cancel();
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
        s.execute("viewbase", &json!({ "views": ["front"], "dims": false })).unwrap();
        let lts = hidden_linetypes(&s);
        assert!(!lts.is_empty() && lts.iter().all(|l| l.eq_ignore_ascii_case("ByLayer")), "{lts:?}");
        assert_eq!(s.doc().unwrap().layer("HIDDEN").unwrap().linetype, "HIDDEN");
        // A HIDDEN layer the drawing already had, continuous: the lines are dashed themselves.
        let mut s = Session::new();
        boxes(&mut s);
        s.doc_mut().unwrap().layers.push(Layer::new("HIDDEN"));
        s.execute("viewbase", &json!({ "views": ["front"], "dims": false })).unwrap();
        let lts = hidden_linetypes(&s);
        assert!(!lts.is_empty() && lts.iter().all(|l| l == "HIDDEN"), "{lts:?}");
        assert!(s.doc().unwrap().linetype("HIDDEN").is_some_and(|l| !l.pattern.is_empty()));
    }

    /// A view drawn at half size measures the model's true size (DIMLFAC 2).
    #[test]
    fn scaled_views_dimension_true_sizes() {
        let mut s = Session::new();
        boxes(&mut s);
        let r = s.execute("viewbase", &json!({ "views": ["front"], "scale": 0.5, "at": [0, 0] })).unwrap();
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

    /// VIEWBASE at the prompts: the Scale option (a bad one asked again), then the location.
    #[test]
    fn viewbase_takes_a_scale_at_the_prompt() {
        let mut s = Session::new();
        boxes(&mut s);
        s.start("viewbase").unwrap();
        s.input(Input::Enter).unwrap();
        s.cmdline("s").unwrap();
        assert!(msg(&s).contains("Enter scale <1>"), "{}", msg(&s));
        s.cmdline("-2").unwrap();
        assert!(msg(&s).contains("Enter scale"), "a bad scale is asked again: {}", msg(&s));
        s.cmdline("0.5").unwrap();
        assert!(msg(&s).contains("location of base view"), "{}", msg(&s));
        s.input(Input::Point(Vec2::new(500.0, 0.0))).unwrap();
        assert!(s.current_prompt().is_none());
        let d = s.doc().unwrap();
        let g = d.groups.iter().find(|g| g.name == "VIEW front").unwrap();
        let b =
            Bounds2::from_points(g.members.iter().filter_map(|h| d.entity(*h)).filter(|e| e.common.layer == "VISIBLE").flat_map(|e| e.kind.grips()));
        assert!(b.min.near(Vec2::new(500.0, 0.0), 1e-6) && b.max.near(Vec2::new(550.0, 10.0), 1e-6), "half size: {b:?}");
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

    #[test]
    fn csv_fields_are_quoted_and_defused() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("+1"), "'+1");
        assert_eq!(csv_field("=1,2"), "\"'=1,2\"");
    }
}
