//! More 3D modelling (CAD-rhino): WEDGE, PYRAMID, LOFT, SWEEP, CHAMFEREDGE, SHELL, PLANESURF and
//! SLICE. Shapes come from [`cadcraft_kernel`] as in `solids`; a surface is a [`Solid3d`] whose
//! mesh is open. As AutoCAD's default DELOBJ does, the curves a solid is made from are erased and
//! those a surface is made from stay.

use cadcraft_doc::{EntityKind, Handle};
use cadcraft_geom::{EPS, Vec2, Vec3};
use cadcraft_kernel::{Built, Curve, MAX_PYRAMID_SIDES, Primitive, Seg};
use serde_json::{Value, json};

use super::curves::{KW, handle_param, is_locked, pick_at};
use super::machines::{SelOutcome, SelectPhase};
use super::solids::{
    Ask, BOX, ProfileM, ProfileOp, Q, add, box_corners, circle_path, curve_of, kernel, locked_err, p3, p3_req, per_solid, placed, solid, solids,
};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

const PYRAMID: &[Q] =
    &[Q::Point("center", "Specify center point of base"), Q::Dist("radius", "Specify base radius"), Q::Dist("height", "Specify height")];

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("wedge", "Wedge", run_wedge)
            .menu(&["Draw", "Modeling", "Wedge"])
            .alias(&["we"])
            .params("{corner1: [x,y,z?], corner2: [x,y,z?], height?} | {corner1, length, width, height}: a box cut diagonally, falling along +X")
            .interactive(|_| Ok(Box::new(Ask::new("wedge", "WEDGE", BOX)))),
        CommandSpec::new("pyramid", "Pyramid", run_pyramid)
            .menu(&["Draw", "Modeling", "Pyramid"])
            .alias(&["pyr"])
            .params("{center: [x,y,z?], radius (base, to a vertex), height, sides? (3 to 32, default 4), topRadius? (0 = pointed)}")
            .interactive(|_| Ok(Box::new(Ask::new("pyramid", "PYRAMID", PYRAMID)))),
        CommandSpec::new("loft", "Loft", run_loft)
            .menu(&["Draw", "Modeling", "Loft"])
            .params("{handles: cross sections in order, ruled?: bool}: closed ones (circles, closed polylines) make a solid, open ones (lines, arcs, open polylines) a surface")
            .interactive(|_| Ok(Box::new(LoftM::default()))),
        CommandSpec::new("sweep", "Sweep", run_sweep)
            .menu(&["Draw", "Modeling", "Sweep"])
            .params("{handles?: profiles, path: handle (line, arc, circle, polyline, spline or helix)}: closed profiles make solids, open ones surfaces")
            .interactive(|_| Ok(Box::new(SweepM::default()))),
        CommandSpec::new("chamferedge", "Chamfer Edge", run_chamferedge)
            .menu(&["Modify", "Solid Editing", "Chamfer Edges"])
            .params("{handle | handles?, distance}: bevels every edge of each solid")
            .interactive(|_| Ok(Box::new(ProfileM::new(ProfileOp::Chamfer)))),
        CommandSpec::new("shell", "Shell", run_shell)
            .menu(&["Modify", "Solid Editing", "Shell"])
            .params("{handle | handles?, thickness, removeTop?: bool (default true)}: hollows each solid inward, open at its top faces")
            .interactive(|_| Ok(Box::new(ProfileM::new(ProfileOp::Shell)))),
        CommandSpec::new("planesurf", "Planar Surface", run_planesurf)
            .menu(&["Draw", "Modeling", "Surfaces", "Planar"])
            .params("{handles?: closed planar curves} | {corner1: [x,y,z?], corner2: [x,y]}")
            .interactive(|_| Ok(Box::new(PlaneSurfM::default()))),
        CommandSpec::new("slice", "Slice", run_slice)
            .menu(&["Modify", "3D Operations", "Slice"])
            .alias(&["sl"])
            .params("{handles?, point1: [x,y], point2: [x,y] (a vertical cutting plane), keep?: \"both\" | \"side\", side?: [x,y] (on the side to keep)}")
            .interactive(|_| Ok(Box::new(SliceM::default()))),
    ]
}

fn run_wedge(s: &mut Session, p: &Value) -> Result<Value> {
    let (a, b) = box_corners("wedge", p)?;
    add(s, cadcraft_kernel::primitive(&Primitive::Wedge { a, b }).map_err(kernel)?)
}

fn run_pyramid(s: &mut Session, p: &Value) -> Result<Value> {
    let base = p3_req("pyramid", p, "center")?;
    let sides = match p.get("sides") {
        None | Some(Value::Null) => 4,
        Some(v) => v
            .as_f64()
            .and_then(|x| (3..=MAX_PYRAMID_SIDES).find(|n| f64::from(*n) == x))
            .ok_or_else(|| bad("pyramid", format!("`sides` must be a whole number from 3 to {MAX_PYRAMID_SIDES}")))?,
    };
    let (radius, height) = (f64_req("pyramid", p, "radius")?, f64_req("pyramid", p, "height")?);
    let top_radius = f64_or(p, "topRadius", 0.0);
    add(s, cadcraft_kernel::primitive(&Primitive::Pyramid { base, sides, radius, top_radius, height }).map_err(kernel)?)
}

/// `hs` without repeats, in order.
fn once(hs: Vec<Handle>) -> Vec<Handle> {
    let mut out: Vec<Handle> = Vec::with_capacity(hs.len());
    for h in hs {
        if !out.contains(&h) {
            out.push(h);
        }
    }
    out
}

fn hexes(hs: &[Handle]) -> Vec<String> {
    hs.iter().map(|h| h.hex()).collect()
}

/// The shape made from `from`, added in its space with its layer and other properties.
fn add_like(s: &mut Session, from: Handle, b: Built) -> Result<Handle> {
    let d = s.doc_mut()?;
    let (Some(common), Some(space)) = (d.entity(from).map(|e| e.common.clone()), d.space_of(from)) else {
        return Err(EngineError::Other("no such object".into()));
    };
    Ok(d.add(&space, common, solid(b))?)
}

fn type_name(s: &Session, h: Handle) -> String {
    s.doc().ok().and_then(|d| d.entity(h).map(|e| e.kind.type_name().to_lowercase())).unwrap_or_else(|| "object".into())
}

fn run_loft(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = once(targets(s, p)?);
    let ruled = bool_or(p, "ruled", false);
    let (mut sections, mut used, mut locked) = (Vec::new(), Vec::new(), 0usize);
    for h in hs {
        let Some(kind) = s.doc()?.entity(h).map(|e| e.kind.clone()) else { continue };
        if is_locked(s, h) {
            locked += 1;
            continue;
        }
        let Some(c) = curve_of(&kind)? else {
            return Err(bad(
                "loft",
                format!("a {} can't be a cross section: use lines, arcs, circles, polylines, splines or helices", type_name(s, h)),
            ));
        };
        sections.push(c);
        used.push(h);
    }
    let Some(first) = used.first().copied().filter(|_| used.len() >= 2) else {
        return Err(if locked > 0 { locked_err() } else { bad("loft", "select at least two cross sections") });
    };
    let b = cadcraft_kernel::loft(&sections, ruled).map_err(kernel)?;
    let surface = b.mesh.open;
    let h = add_like(s, first, b)?;
    if !surface {
        let d = s.doc_mut()?;
        for u in &used {
            d.remove_entity(*u);
        }
    }
    Ok(json!({ "handle": h.hex(), "surface": surface, "sections": used.len(), "locked": locked }))
}

/// A curve object as a sweep path: a circle runs from its rightmost point.
fn path_of(kind: &EntityKind) -> Result<Option<Curve>> {
    match kind {
        EntityKind::Circle(c) => Ok(Some(circle_path(c.center, c.radius))),
        k => curve_of(k),
    }
}

fn run_sweep(s: &mut Session, p: &Value) -> Result<Value> {
    let ph =
        handle_param(p, "path").ok_or_else(|| bad("sweep", "`path` (the handle of a line, arc, circle, polyline, spline or helix) is required"))?;
    let path_kind = s.doc()?.entity(ph).map(|e| e.kind.clone()).ok_or_else(|| bad("sweep", "the path object does not exist"))?;
    let path = path_of(&path_kind)?
        .ok_or_else(|| bad("sweep", format!("a {} can't be a path: use a line, arc, circle, polyline, spline or helix", type_name(s, ph))))?;
    let hs: Vec<Handle> = once(targets(s, p)?).into_iter().filter(|h| *h != ph).collect();
    let (mut built, mut skipped, mut locked) = (Vec::new(), 0usize, 0usize);
    for h in hs {
        let Some(kind) = s.doc()?.entity(h).map(|e| e.kind.clone()) else { continue };
        if is_locked(s, h) {
            locked += 1;
            continue;
        }
        let Some(c) = curve_of(&kind)? else {
            skipped += 1;
            continue;
        };
        built.push((h, cadcraft_kernel::sweep(&c, &path).map_err(kernel)?));
    }
    if built.is_empty() {
        return Err(if locked > 0 {
            locked_err()
        } else {
            bad("sweep", "select curves to sweep (lines, arcs, circles, polylines, splines, helices)")
        });
    }
    let all_solid = built.iter().all(|(_, b)| !b.mesh.open);
    let mut made = Vec::with_capacity(built.len());
    for (h, b) in built {
        let surface = b.mesh.open;
        made.push(add_like(s, h, b)?.hex());
        if !surface {
            s.doc_mut()?.remove_entity(h);
        }
    }
    // The path goes too when only solids were made from it.
    if all_solid && !is_locked(s, ph) {
        s.doc_mut()?.remove_entity(ph);
    }
    Ok(json!({ "handles": made, "skipped": skipped, "locked": locked }))
}

fn run_chamferedge(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let distance = f64_req("chamferedge", p, "distance")?;
    let (n, locked) = per_solid(s, "chamferedge", &hs, |x| cadcraft_kernel::chamfer(x, distance))?;
    Ok(json!({ "chamfered": n, "locked": locked }))
}

fn run_shell(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let thickness = f64_req("shell", p, "thickness")?;
    let remove_top = bool_or(p, "removeTop", true);
    let (n, locked) = per_solid(s, "shell", &hs, |x| cadcraft_kernel::shell(x, thickness, remove_top))?;
    Ok(json!({ "shelled": n, "locked": locked }))
}

fn run_planesurf(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(a) = p3(p, "corner1") {
        let c = point_req("planesurf", p, "corner2")?;
        let at = |x: f64, y: f64| Vec3::new(x, y, a.z);
        let pts = [at(a.x, a.y), at(c.x, a.y), at(c.x, c.y), at(a.x, c.y)];
        let segs: Vec<Seg> = (0..4).filter_map(|i| Some(Seg::Line { a: *pts.get(i)?, b: *pts.get((i + 1) % 4)? })).collect();
        return add(s, cadcraft_kernel::plane_surface(&segs).map_err(kernel)?);
    }
    let hs = once(targets(s, p)?);
    let (mut built, mut skipped, mut locked) = (Vec::new(), 0usize, 0usize);
    for h in hs {
        let Some(kind) = s.doc()?.entity(h).map(|e| e.kind.clone()) else { continue };
        if is_locked(s, h) {
            locked += 1;
            continue;
        }
        match curve_of(&kind)?.filter(Curve::is_closed) {
            Some(Curve::Segs(segs)) => built.push((h, cadcraft_kernel::plane_surface(&segs).map_err(kernel)?)),
            _ => skipped += 1,
        }
    }
    if built.is_empty() {
        return Err(if locked > 0 {
            locked_err()
        } else {
            bad("planesurf", "select closed planar curves (circles, closed polylines, single-loop regions) or give `corner1` and `corner2`")
        });
    }
    let mut made = Vec::with_capacity(built.len());
    for (h, b) in built {
        made.push(add_like(s, h, b)?.hex());
    }
    Ok(json!({ "handles": made, "skipped": skipped, "locked": locked }))
}

fn run_slice(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let (a, b) = (point_req("slice", p, "point1")?, point_req("slice", p, "point2")?);
    if a.near(b, EPS) {
        return Err(bad("slice", "`point1` and `point2` must be different points"));
    }
    // The plane stands on the line through the two points; its normal points left of it.
    let d = (b - a).normalized();
    let normal = Vec3::new(-d.y, d.x, 0.0);
    let keep_side = match str_param(p, "keep").map(str::to_ascii_lowercase).as_deref() {
        None | Some("both") => None,
        Some("side") => {
            let side = point_req("slice", p, "side")?;
            let off = normal.xy().dot(side - a);
            if off.abs() <= EPS * (1.0 + (side - a).len()) {
                return Err(bad("slice", "the `side` point is on the cutting plane"));
            }
            Some(off > 0.0)
        }
        Some(k) => return Err(bad("slice", format!("`keep` is \"both\" or \"side\", not \"{k}\""))),
    };
    let mut xs = solids(s, &hs)?;
    let all = xs.len();
    xs.retain(|(h, _)| !is_locked(s, *h));
    if xs.is_empty() {
        return Err(if all > 0 { locked_err() } else { bad("slice", "select 3D solids") });
    }
    // Every solid first; one the plane misses stays as it is.
    let mut cut = Vec::new();
    for (h, x) in &xs {
        if let [Some(plus), Some(minus)] = cadcraft_kernel::slice(placed(x), a.to3(0.0), normal).map_err(kernel)? {
            cut.push((*h, plus, minus));
        }
    }
    if cut.is_empty() {
        return Err(EngineError::Other("The plane does not cut the selected solids.".into()));
    }
    let n = cut.len();
    let mut made = Vec::new();
    for (h, plus, minus) in cut {
        let (keep, other) = match keep_side {
            Some(true) => (plus, None),
            Some(false) => (minus, None),
            None => (plus, Some(minus)),
        };
        if let Some(o) = other {
            made.push(add_like(s, h, o)?.hex());
        }
        s.doc_mut()?.modify_entity(h, |e| e.kind = solid(keep))?;
        made.push(h.hex());
    }
    Ok(json!({ "sliced": n, "missed": xs.len() - n, "handles": made, "locked": all - xs.len() }))
}

/// Runs a command's JSON form from its prompts, and clears what was picked for it.
fn run_json(s: &mut Session, id: &str, p: Value) -> Result<Step> {
    let spec = find_command(id).ok_or_else(|| EngineError::UnknownCommand(id.into()))?;
    let r = (spec.run)(s, &p)?;
    if let Some(m) = r.get("message").and_then(Value::as_str).filter(|m| !m.is_empty()) {
        s.echo(m.to_string());
    }
    s.set_selection(Vec::new());
    Ok(Step::Done)
}

/// A selection phase's input; the objects land in `hs` when it finishes.
fn feed(sel: &mut SelectPhase, hs: &mut Vec<Handle>, s: &mut Session, i: &Input) -> Result<Step> {
    Ok(match sel.feed(s, i)? {
        SelOutcome::More => Step::Continue,
        SelOutcome::Empty => Step::Done,
        SelOutcome::Done(picked) => {
            *hs = picked;
            Step::Continue
        }
    })
}

/// Typed text that starts `word` (a keyword's shortcut or more of it).
fn is_kw(t: &str, word: &str) -> bool {
    let t = t.trim().to_ascii_lowercase();
    !t.is_empty() && word.starts_with(&t)
}

/// LOFT: cross sections in lofting order, then smooth or ruled.
#[derive(Default)]
struct LoftM {
    sel: SelectPhase,
    hs: Vec<Handle>,
}

impl Interactive for LoftM {
    fn name(&self) -> &'static str {
        "LOFT"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.hs = self.sel.picked.clone();
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match (self.hs.is_empty(), self.sel.removing) {
            (true, false) => Prompt::new("Select cross sections in lofting order", Accept::SELECT),
            (true, true) => self.sel.prompt(),
            (false, _) => Prompt::new("Enter an option", KW).kw(&["Ruled", "Smooth"]).default("Smooth"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_empty() {
            return feed(&mut self.sel, &mut self.hs, s, &i);
        }
        let ruled = match &i {
            Input::Enter => false,
            Input::Text(t) | Input::Keyword(t) if is_kw(t, "ruled") => true,
            Input::Text(t) | Input::Keyword(t) if is_kw(t, "smooth") => false,
            Input::Text(_) | Input::Keyword(_) => {
                s.echo("Requires Ruled or Smooth.");
                return Ok(Step::Continue);
            }
            _ => return Ok(Step::Continue),
        };
        run_json(s, "loft", json!({ "handles": hexes(&self.hs), "ruled": ruled }))
    }
}

/// SWEEP: the objects to sweep, then the path.
#[derive(Default)]
struct SweepM {
    sel: SelectPhase,
    hs: Vec<Handle>,
}

impl Interactive for SweepM {
    fn name(&self) -> &'static str {
        "SWEEP"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.hs = self.sel.picked.clone();
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match (self.hs.is_empty(), self.sel.removing) {
            (true, false) => Prompt::new("Select objects to sweep", Accept::SELECT),
            (true, true) => self.sel.prompt(),
            (false, _) => Prompt::new("Select sweep path", Accept::POINT),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_empty() {
            return feed(&mut self.sel, &mut self.hs, s, &i);
        }
        let path = match &i {
            Input::Point(p) => pick_at(s, *p),
            Input::Pick(hs) => hs.first().copied(),
            _ => return Ok(Step::Continue),
        };
        let Some(path) = path else {
            s.echo("No object found.");
            return Ok(Step::Continue);
        };
        run_json(s, "sweep", json!({ "handles": hexes(&self.hs), "path": path.hex() }))
    }
}

/// PLANESURF: two corners, or Object and closed curves.
#[derive(Default)]
struct PlaneSurfM {
    corner: Option<Vec3>,
    objects: Option<SelectPhase>,
}

impl PlaneSurfM {
    fn objects(&mut self, s: &mut Session, hs: Vec<Handle>) -> Result<Step> {
        run_json(s, "planesurf", json!({ "handles": hexes(&hs) }))
    }
    fn object_option(&mut self, s: &mut Session) -> Result<Step> {
        let sel = SelectPhase::begin(s);
        if sel.done {
            return self.objects(s, sel.picked);
        }
        self.objects = Some(sel);
        Ok(Step::Continue)
    }
}

impl Interactive for PlaneSurfM {
    fn name(&self) -> &'static str {
        "PLANESURF"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match (&self.objects, self.corner) {
            (Some(sel), _) => sel.prompt(),
            (None, Some(c)) => Prompt::new("Specify other corner", Accept::POINT).corner(c.xy()),
            (None, None) => Prompt::new("Specify first corner", Accept::POINT).kw(&["Object"]).default("Object"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if let Some(sel) = self.objects.as_mut() {
            return match sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Done),
                SelOutcome::Done(hs) => self.objects(s, hs),
            };
        }
        match (self.corner, i) {
            // A first corner snapped to a 3D object sets the surface's height.
            (None, Input::Point(p)) => {
                self.corner = Some(p.to3(s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0)));
                Ok(Step::Continue)
            }
            (Some(c), Input::Point(p)) => run_json(s, "planesurf", json!({ "corner1": [c.x, c.y, c.z], "corner2": [p.x, p.y] })),
            (None, Input::Enter) => self.object_option(s),
            (None, Input::Text(t) | Input::Keyword(t)) if is_kw(&t, "object") => self.object_option(s),
            _ => Ok(Step::Continue),
        }
    }
}

/// SLICE: the solids, two points on a vertical cutting plane, then the side to keep or both.
#[derive(Default)]
struct SliceM {
    sel: SelectPhase,
    hs: Vec<Handle>,
    pts: Vec<Vec2>,
}

impl SliceM {
    fn run(&self, s: &mut Session, keep: Value) -> Result<Step> {
        let (Some(a), Some(b)) = (self.pts.first(), self.pts.get(1)) else { return Ok(Step::Continue) };
        let mut p = json!({ "handles": hexes(&self.hs), "point1": [a.x, a.y], "point2": [b.x, b.y], "keep": "both" });
        if let (Some(o), Some(k)) = (p.as_object_mut(), keep.as_object()) {
            o.extend(k.clone());
        }
        run_json(s, "slice", p)
    }
}

impl Interactive for SliceM {
    fn name(&self) -> &'static str {
        "SLICE"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.hs = self.sel.picked.clone();
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.hs.is_empty() {
            return self.sel.prompt();
        }
        match self.pts.as_slice() {
            [] => Prompt::new("Specify first point on plane", Accept::POINT),
            [a] => Prompt::new("Specify second point on plane", Accept::POINT).base(*a),
            _ => Prompt::new("Specify a point on desired side", Accept::POINT).kw(&["keep Both sides"]).default("Both"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_empty() {
            return feed(&mut self.sel, &mut self.hs, s, &i);
        }
        match (self.pts.len(), i) {
            (n, Input::Point(p)) if n < 2 => {
                if self.pts.first().is_some_and(|a| a.near(p, EPS)) {
                    return Err(EngineError::Other("The plane needs two different points.".into()));
                }
                self.pts.push(p);
                Ok(Step::Continue)
            }
            (_, Input::Point(p)) => self.run(s, json!({ "keep": "side", "side": [p.x, p.y] })),
            (2, Input::Enter) => self.run(s, json!({})),
            (2, Input::Text(t) | Input::Keyword(t)) if is_kw(&t, "both") || is_kw(&t, "keep both sides") => self.run(s, json!({})),
            _ => Ok(Step::Continue),
        }
    }
}

/// Kernel-free checks: parameters, prompts and refusals before any shape is made.
#[cfg(test)]
mod plain_tests {
    use cadcraft_doc::Solid3d;
    use cadcraft_geom::PolyVertex;

    use super::*;

    fn err(r: Result<Value>) -> String {
        r.err().map(|e| e.to_string()).unwrap_or_default()
    }

    fn circle(s: &mut Session, x: f64) -> Handle {
        s.add_entity(super::super::helpers::circle(Vec2::new(x, 0.0), 1.0)).unwrap()
    }

    fn line(s: &mut Session) -> Value {
        s.execute("line", &json!({ "points": [[0, 0], [5, 0]] })).unwrap()["handles"][0].clone()
    }

    #[test]
    fn pyramid_sides_are_whole_numbers_from_3_to_32() {
        let mut s = Session::new();
        for sides in [json!(2), json!(33), json!(4.5), json!("4"), json!(-4), json!(1e300), json!(f64::MAX)] {
            let r = s.execute("pyramid", &json!({ "center": [0, 0], "radius": 5, "height": 3, "sides": sides }));
            assert!(err(r).contains("sides"), "{sides}");
        }
        assert!(s.doc().unwrap().model.iter().next().is_none());
        // Without the kernel the right parameters reach it and come back unavailable.
        let r = s.execute("pyramid", &json!({ "center": [0, 0, 2], "radius": 5, "height": 3, "sides": 32.0, "topRadius": 1 }));
        assert!(r.is_ok() || !err(r).contains("sides"));
    }

    #[test]
    fn hostile_parameters_are_refused() {
        let mut s = Session::new();
        let c = circle(&mut s, 0.0);
        for (id, p) in [
            ("wedge", json!({ "corner1": [0, 0], "corner2": [1e308, -1e308], "height": "x" })),
            ("pyramid", json!({ "center": ["a"], "radius": 5, "height": 1 })),
            ("loft", json!({ "handles": "zz", "ruled": 7 })),
            ("sweep", json!({ "handles": [c.hex()], "path": [1, 2] })),
            ("sweep", json!({ "handles": [c.hex()], "path": "FFFFFFFFFFFFFFFFFF" })),
            ("chamferedge", json!({ "handles": [c.hex()], "distance": null })),
            ("shell", json!({ "handle": -1, "thickness": 1 })),
            ("planesurf", json!({ "corner1": [0, 0], "corner2": "x" })),
            ("slice", json!({ "point1": [0, 0], "point2": [0, 0] })),
            ("slice", json!({ "point1": [0, 0], "point2": [1, 0], "keep": "side" })),
            ("slice", json!({ "point1": [0, 0], "point2": [1, 0], "keep": 3 })),
        ] {
            assert!(s.execute(id, &p).is_err(), "{id} {p}");
        }
        assert_eq!(s.doc().unwrap().model.iter().count(), 1, "the drawing is as it was");
    }

    #[test]
    fn loft_needs_two_curves_at_different_elevations() {
        let mut s = Session::new();
        let (a, b) = (circle(&mut s, 0.0), circle(&mut s, 10.0));
        assert!(err(s.execute("loft", &json!({ "handles": [a.hex()] }))).contains("two cross sections"));
        assert!(err(s.execute("loft", &json!({ "handles": [a.hex(), a.hex()] }))).contains("two cross sections"), "one section twice");
        // Both on the XY plane: refused by the kernel's checks, which run without the kernel too.
        assert!(err(s.execute("loft", &json!({ "handles": [a.hex(), b.hex()] }))).contains("different elevations"));
        let l = line(&mut s);
        assert!(err(s.execute("loft", &json!({ "handles": [a.hex(), l] }))).contains("all open or all closed"));
        let pt = s.add_entity(EntityKind::Point(cadcraft_doc::Point { p: Vec3::ZERO, angle: 0.0 })).unwrap();
        assert!(err(s.execute("loft", &json!({ "handles": [a.hex(), pt.hex()] }))).contains("can't be a cross section"));
        assert_eq!(s.doc().unwrap().model.iter().count(), 4);
    }

    #[test]
    fn sweep_needs_a_curve_path_and_profiles() {
        let mut s = Session::new();
        let c = circle(&mut s, 0.0);
        let solid = s.add_entity(EntityKind::Solid3d(Solid3d::default())).unwrap();
        assert!(err(s.execute("sweep", &json!({ "handles": [c.hex()] }))).contains("`path`"));
        assert!(err(s.execute("sweep", &json!({ "handles": [c.hex()], "path": solid.hex() }))).contains("can't be a path"));
        let l = line(&mut s);
        // The path alone is not something to sweep.
        assert!(err(s.execute("sweep", &json!({ "handles": [l], "path": l }))).contains("select curves"));
        assert!(err(s.execute("sweep", &json!({ "handles": [solid.hex()], "path": l }))).contains("select curves"));
    }

    #[test]
    fn locked_objects_are_left_alone() {
        let mut s = Session::new();
        let (a, b) = (circle(&mut s, 0.0), circle(&mut s, 5.0));
        let x = s.add_entity(EntityKind::Solid3d(Solid3d::default())).unwrap();
        s.doc_mut().unwrap().layer_mut("0").unwrap().locked = true;
        let hs = json!([a.hex(), b.hex()]);
        assert!(err(s.execute("loft", &json!({ "handles": hs }))).contains("locked"));
        assert!(err(s.execute("sweep", &json!({ "handles": [a.hex()], "path": b.hex() }))).contains("locked"));
        assert!(err(s.execute("planesurf", &json!({ "handles": hs }))).contains("locked"));
        for (id, p) in [
            ("chamferedge", json!({ "handles": [x.hex()], "distance": 1 })),
            ("shell", json!({ "handles": [x.hex()], "thickness": 1 })),
            ("slice", json!({ "handles": [x.hex()], "point1": [0, 0], "point2": [0, 1] })),
        ] {
            assert!(err(s.execute(id, &p)).contains("locked"), "{id}");
        }
        assert_eq!(s.doc().unwrap().model.iter().count(), 3);
    }

    #[test]
    fn planesurf_takes_closed_curves_only() {
        let mut s = Session::new();
        let l = line(&mut s);
        assert!(err(s.execute("planesurf", &json!({ "handles": [l] }))).contains("closed planar curves"));
        let vs = vec![PolyVertex::new(Vec2::ZERO), PolyVertex::new(Vec2::new(1.0, 0.0)), PolyVertex::new(Vec2::new(1.0, 1.0))];
        let open = s.add_entity(super::super::helpers::lwpoly(vs, false)).unwrap();
        assert!(err(s.execute("planesurf", &json!({ "handles": [open.hex()] }))).contains("closed planar curves"));
    }

    #[test]
    fn surfaces_are_not_chamfered_or_shelled() {
        let mut s = Session::new();
        let mesh = cadcraft_geom::Mesh3 { open: true, ..Default::default() };
        let surf = s.add_entity(EntityKind::Solid3d(Solid3d { mesh: std::sync::Arc::new(mesh), ..Default::default() })).unwrap();
        assert!(err(s.execute("chamferedge", &json!({ "handles": [surf.hex()], "distance": 1 }))).contains("select 3D solids"));
        assert!(err(s.execute("shell", &json!({ "handles": [surf.hex()], "thickness": 1 }))).contains("select 3D solids"));
    }

    #[test]
    fn prompts_follow_autocad() {
        let mut s = Session::new();
        let msg = |s: &Session| s.current_prompt().map(|p| p.message).unwrap_or_default();
        s.start("wedge").unwrap();
        assert!(msg(&s).contains("first corner"));
        s.input(Input::Point(Vec2::ZERO)).unwrap();
        assert!(msg(&s).contains("other corner"));
        s.cancel();
        s.start("pyramid").unwrap();
        assert!(msg(&s).contains("center point of base"));
        s.cancel();
        // LOFT: cross sections, then Ruled or Smooth.
        let (a, b) = (circle(&mut s, 0.0), circle(&mut s, 5.0));
        s.start("loft").unwrap();
        assert!(msg(&s).contains("lofting order"));
        s.input(Input::Pick(vec![a, b])).unwrap();
        s.input(Input::Enter).unwrap();
        let p = s.current_prompt().unwrap();
        assert_eq!(p.keywords, vec!["Ruled", "Smooth"]);
        s.cancel();
        // SWEEP: objects, then the path (nothing picked first: no pickfirst selection).
        s.set_selection(Vec::new());
        s.start("sweep").unwrap();
        s.input(Input::Pick(vec![a])).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(msg(&s).contains("sweep path"));
        s.cancel();
        // SLICE: objects, two points, then the side.
        s.set_selection(Vec::new());
        s.start("slice").unwrap();
        s.input(Input::Pick(vec![a])).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(msg(&s).contains("first point on plane"));
        s.input(Input::Point(Vec2::ZERO)).unwrap();
        let _ = s.input(Input::Point(Vec2::ZERO));
        assert!(msg(&s).contains("second point on plane"), "the same point twice is asked again");
        s.input(Input::Point(Vec2::new(0.0, 1.0))).unwrap();
        assert!(msg(&s).contains("desired side"));
        s.cancel();
        // PLANESURF: a corner, or Object.
        s.set_selection(Vec::new());
        s.start("planesurf").unwrap();
        assert_eq!(s.current_prompt().unwrap().keywords, vec!["Object"]);
        s.input(Input::Text("o".into())).unwrap();
        assert!(msg(&s).contains("Select objects"));
        s.cancel();
    }

    #[test]
    fn a_first_point_on_a_3d_object_sets_the_height() {
        let mut ask = Ask::new("box", "BOX", BOX);
        let mut s = Session::new();
        s.point_z = Some(5.0);
        ask.input(&mut s, Input::Point(Vec2::new(1.0, 2.0))).unwrap();
        // Only the first point: the other corner stays on its plane.
        ask.input(&mut s, Input::Point(Vec2::new(4.0, 6.0))).unwrap();
        assert_eq!(ask.values().get("corner1"), Some(&json!([1.0, 2.0, 5.0])));
        assert_eq!(ask.values().get("corner2"), Some(&json!([4.0, 6.0])));
        let mut plain = Ask::new("sphere", "SPHERE", PYRAMID);
        s.point_z = None;
        plain.input(&mut s, Input::Point(Vec2::new(1.0, 2.0))).unwrap();
        assert_eq!(plain.values().get("center"), Some(&json!([1.0, 2.0])));
    }
}

#[cfg(test)]
#[cfg(feature = "occt")]
mod tests {
    use cadcraft_doc::Solid3d;
    use cadcraft_geom::{Bounds2, PolyVertex};

    use super::*;

    fn shapes(s: &Session) -> Vec<Solid3d> {
        s.doc()
            .unwrap()
            .model
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Solid3d(x) => Some(x.clone()),
                _ => None,
            })
            .collect()
    }

    /// World-space height range of a shape's mesh.
    fn heights(x: &Solid3d) -> (f64, f64) {
        x.mesh.verts.iter().map(|p| x.world(*p).z).fold((f64::MAX, f64::MIN), |(lo, hi), z| (lo.min(z), hi.max(z)))
    }

    fn spans(x: &Solid3d, lo: f64, hi: f64) -> bool {
        let (a, b) = heights(x);
        (a - lo).abs() < 1e-6 && (b - hi).abs() < 1e-6
    }

    fn handle(v: &Value) -> Handle {
        Handle::parse_hex(v.as_str().unwrap()).unwrap()
    }

    fn near(b: Bounds2, lo: (f64, f64), hi: (f64, f64)) -> bool {
        near_by(b, lo, hi, 1e-6)
    }

    fn near_by(b: Bounds2, lo: (f64, f64), hi: (f64, f64), tol: f64) -> bool {
        (b.min.x - lo.0).abs() < tol && (b.min.y - lo.1).abs() < tol && (b.max.x - hi.0).abs() < tol && (b.max.y - hi.1).abs() < tol
    }

    fn poly(s: &mut Session, pts: &[(f64, f64, f64)], closed: bool, z: f64) -> Handle {
        let vs = pts.iter().map(|(x, y, b)| PolyVertex::with_bulge(Vec2::new(*x, *y), *b)).collect();
        let EntityKind::LwPolyline(mut p) = super::super::helpers::lwpoly(vs, closed) else { panic!("not a polyline") };
        p.elevation = z;
        s.add_entity(EntityKind::LwPolyline(p)).unwrap()
    }

    fn circle_at(s: &mut Session, x: f64, z: f64, r: f64) -> Handle {
        s.add_entity(EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::new(x, 0.0, z), radius: r })).unwrap()
    }

    #[test]
    fn wedge_and_pyramid() {
        let mut s = Session::new();
        s.execute("wedge", &json!({ "corner1": [0, 0, 1], "corner2": [10, 5], "height": 3 })).unwrap();
        s.execute("pyramid", &json!({ "center": [20, 0], "radius": 4, "height": 6, "sides": 6, "topRadius": 1 })).unwrap();
        let xs = shapes(&s);
        assert_eq!(xs.len(), 2);
        assert!(near(xs[0].bounds(), (0.0, 0.0), (10.0, 5.0)) && spans(&xs[0], 1.0, 4.0));
        assert!(!xs[1].mesh.open && (heights(&xs[1]).1 - 6.0).abs() < 1e-9);
        s.undo().unwrap();
        assert_eq!(shapes(&s).len(), 1);
    }

    #[test]
    fn a_box_started_on_a_3d_object_starts_at_its_height() {
        let mut s = Session::new();
        s.start("box").unwrap();
        s.point_z = Some(5.0);
        s.input(Input::Point(Vec2::ZERO)).unwrap();
        s.point_z = None;
        s.input(Input::Point(Vec2::new(4.0, 4.0))).unwrap();
        s.input(Input::Text("2".into())).unwrap();
        let xs = shapes(&s);
        assert_eq!(xs.len(), 1);
        assert!(spans(&xs[0], 5.0, 7.0), "{:?}", heights(&xs[0]));
    }

    #[test]
    fn loft_closed_sections_into_a_solid_and_open_ones_into_a_surface() {
        let mut s = Session::new();
        let (a, b) =
            (circle_at(&mut s, 0.0, 0.0, 5.0), poly(&mut s, &[(-2.0, -2.0, 0.0), (2.0, -2.0, 0.0), (2.0, 2.0, 0.0), (-2.0, 2.0, 0.0)], true, 8.0));
        let r = s.execute("loft", &json!({ "handles": [a.hex(), b.hex()], "ruled": true })).unwrap();
        assert_eq!(r["surface"], false);
        let xs = shapes(&s);
        assert_eq!(xs.len(), 1);
        assert_eq!(s.doc().unwrap().model.iter().count(), 1, "the sections are erased");
        assert!(spans(&xs[0], 0.0, 8.0), "{:?}", heights(&xs[0]));
        s.undo().unwrap();
        // Open sections: a surface, and the curves stay.
        let mut s = Session::new();
        let l1 = s.add_entity(EntityKind::Line(cadcraft_doc::Line { a: Vec3::ZERO, b: Vec3::new(10.0, 0.0, 0.0) })).unwrap();
        let l2 = s.add_entity(EntityKind::Line(cadcraft_doc::Line { a: Vec3::new(0.0, 5.0, 4.0), b: Vec3::new(10.0, 5.0, 4.0) })).unwrap();
        let r = s.execute("loft", &json!({ "handles": [l1.hex(), l2.hex()] })).unwrap();
        assert_eq!(r["surface"], true);
        assert!(shapes(&s)[0].mesh.open);
        assert_eq!(s.doc().unwrap().model.iter().count(), 3);
    }

    #[test]
    fn sweep_a_circle_along_a_polyline_and_a_line_along_a_helix() {
        let mut s = Session::new();
        let prof = circle_at(&mut s, 50.0, 0.0, 0.5);
        // Along X, a quarter-circle bend (bulge tan(22.5°)), then a corner up Y.
        let path =
            poly(&mut s, &[(0.0, 0.0, 0.0), (10.0, 0.0, 0.41421356237309503), (15.0, 5.0, 0.0), (15.0, 15.0, 0.0), (5.0, 15.0, 0.0)], false, 0.0);
        let r = s.execute("sweep", &json!({ "handles": [prof.hex()], "path": path.hex() })).unwrap();
        assert_eq!(r["handles"].as_array().map(Vec::len), Some(1));
        let xs = shapes(&s);
        assert!(!xs[0].mesh.open);
        // The mesh's polygons sit just inside the round tube.
        assert!(near_by(xs[0].bounds(), (0.0, -0.5), (15.5, 15.5), 0.01), "{:?}", xs[0].bounds());
        assert_eq!(s.doc().unwrap().model.iter().count(), 1, "profile and path are erased");
        // An open profile along a helix: a surface; the curves stay.
        let mut s = Session::new();
        let helix = cadcraft_doc::Helix {
            center: Vec3::ZERO,
            base_radius: 10.0,
            top_radius: 10.0,
            height: 20.0,
            turns: 2.0,
            start_angle: 0.0,
            ccw: true,
            constrain: Default::default(),
        };
        let path = s.add_entity(EntityKind::Helix(helix)).unwrap();
        let l = s.add_entity(EntityKind::Line(cadcraft_doc::Line { a: Vec3::ZERO, b: Vec3::new(2.0, 0.0, 0.0) })).unwrap();
        s.execute("sweep", &json!({ "handles": [l.hex()], "path": path.hex() })).unwrap();
        assert!(shapes(&s)[0].mesh.open);
        assert_eq!(s.doc().unwrap().model.iter().count(), 3);
        // A circle path runs round once.
        let mut s = Session::new();
        let ring = circle_at(&mut s, 0.0, 0.0, 10.0);
        let tube = circle_at(&mut s, 30.0, 0.0, 1.0);
        s.execute("sweep", &json!({ "handles": [tube.hex()], "path": ring.hex() })).unwrap();
        let xs = shapes(&s);
        assert!(!xs[0].mesh.open && near_by(xs[0].bounds(), (-11.0, -11.0), (11.0, 11.0), 0.05), "{:?}", xs[0].bounds());
    }

    #[test]
    fn chamfer_shell_and_slice_a_box() {
        let mut s = Session::new();
        let b = handle(&s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [10, 10], "height": 10 })).unwrap()["handle"]);
        let before = shapes(&s)[0].mesh.tris.len();
        s.execute("chamferedge", &json!({ "handle": b.hex(), "distance": 1 })).unwrap();
        assert!(shapes(&s)[0].mesh.tris.len() > before);
        s.undo().unwrap();
        s.execute("shell", &json!({ "handles": [b.hex()], "thickness": 1 })).unwrap();
        assert!(!shapes(&s)[0].mesh.open && spans(&shapes(&s)[0], 0.0, 10.0));
        s.undo().unwrap();
        // Both halves stay: the solid keeps the one left of point1 → point2 (here -X), the other is new.
        let r = s.execute("slice", &json!({ "handles": [b.hex()], "point1": [4, 0], "point2": [4, 1] })).unwrap();
        assert_eq!(r["sliced"], 1);
        let xs = shapes(&s);
        assert_eq!(xs.len(), 2);
        assert!(
            near(xs[0].bounds(), (0.0, 0.0), (4.0, 10.0)) && near(xs[1].bounds(), (4.0, 0.0), (10.0, 10.0)),
            "{:?}",
            xs.iter().map(|x| x.bounds()).collect::<Vec<_>>()
        );
        s.undo().unwrap();
        // Keep the side a point is on.
        s.execute("slice", &json!({ "handles": [b.hex()], "point1": [4, 0], "point2": [4, 1], "keep": "side", "side": [9, 5] })).unwrap();
        let xs = shapes(&s);
        assert_eq!(xs.len(), 1);
        assert!(near(xs[0].bounds(), (4.0, 0.0), (10.0, 10.0)));
        // A plane that misses: an error, nothing changes.
        let before = s.doc().unwrap().clone();
        assert!(s.execute("slice", &json!({ "handles": [b.hex()], "point1": [40, 0], "point2": [40, 1] })).is_err());
        assert_eq!(*s.doc().unwrap(), before);
    }

    #[test]
    fn planar_surfaces_from_corners_and_curves() {
        let mut s = Session::new();
        s.execute("planesurf", &json!({ "corner1": [0, 0, 3], "corner2": [4, 2] })).unwrap();
        let c = circle_at(&mut s, 10.0, 0.0, 2.0);
        s.execute("planesurf", &json!({ "handles": [c.hex()] })).unwrap();
        let xs = shapes(&s);
        assert_eq!(xs.len(), 2);
        assert!(xs.iter().all(|x| x.mesh.open));
        assert!(spans(&xs[0], 3.0, 3.0), "{:?}", heights(&xs[0]));
        assert!(s.doc().unwrap().entity(c).is_some(), "the circle stays");
    }

    #[test]
    fn extruding_an_open_curve_makes_a_surface_and_keeps_it() {
        let mut s = Session::new();
        let l = s.execute("line", &json!({ "points": [[0, 0], [5, 0]] })).unwrap()["handles"][0].clone();
        let c = s.add_entity(super::super::helpers::circle(Vec2::new(20.0, 0.0), 1.0)).unwrap();
        s.execute("extrude", &json!({ "handles": [l, c.hex()], "height": 4 })).unwrap();
        let xs = shapes(&s);
        assert_eq!(xs.len(), 2);
        assert!(xs[0].mesh.open && !xs[1].mesh.open);
        assert!(s.doc().unwrap().entity(handle(&l)).is_some() && s.doc().unwrap().entity(c).is_none());
    }
}
