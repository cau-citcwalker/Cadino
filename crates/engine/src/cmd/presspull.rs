//! PRESSPULL, THICKEN and XEDGES (CAD-rhino). PRESSPULL pushes or pulls what is clicked: a face of
//! a 3D solid, a closed boundary drawn on a face (a boss out of it or a pocket into it, its holes
//! left), or a closed boundary on its own (extruded into a solid). THICKEN makes surfaces solids;
//! XEDGES draws a solid's edges as lines, arcs, circles, ellipses and splines on its layer.

use cadcraft_doc::{Entity, Handle};
use cadcraft_geom::{Mat4, Vec2, Vec3, point_in_polygon};
use cadcraft_kernel::{Curve, Region};
use serde_json::{Value, json};

use super::curves::{is_locked, pick_at};
use super::machines::{SelOutcome, SelectPhase, SelectRun};
use super::solids::{curve_of, distance, kernel, locked_err, normal_of, p3, placed, reshape, solids};
use super::subobj::{Aim, Sub, aim, edge_entity, named, seg_points, solid_of, target, topology, world};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("presspull", "Presspull", run_presspull)
            .menu(&["Modify", "Solid Editing", "Presspull"])
            .params(
                "{at: [x,y,z] (a point inside the area: on a face of a solid, or inside a closed curve), distance} | {handle, face: index | [x,y,z], distance} | \
                 {handle, profile: handle (a closed curve on a face), holes?: [handles], distance} | {handles: [curves], distance}: + pulls out of the solid, - pushes in",
            )
            .interactive(|_| Ok(Box::new(PressPullM::default()))),
        CommandSpec::new("thicken", "Thicken", run_thicken)
            .menu(&["Modify", "3D Operations", "Thicken"])
            .params("{handles?, thickness (+ along the surface's normals), bothSides?: bool}: surfaces become solids")
            .interactive(|_| Ok(Box::new(ThickenM::default()))),
        CommandSpec::new("xedges", "Extract Edges", run_xedges)
            .menu(&["Modify", "3D Operations", "Extract Edges"])
            .params("{handles?}: each 3D solid's or surface's edges as lines, arcs, circles, ellipses and splines on its layer")
            .interactive(|_| Ok(Box::new(SelectRun::new("xedges", "XEDGES")))),
    ]
}

// ---------------- PRESSPULL ----------------

/// A closed flat curve object: its plane (a point on it and the unit normal) and its outline in
/// that plane's coordinates (`to` takes world points there).
struct Loop {
    h: Handle,
    o: Vec3,
    n: Vec3,
    /// Points round it in the world, and in its plane's coordinates.
    pts: Vec<Vec3>,
    poly: Vec<Vec2>,
}

impl Loop {
    fn of(e: &Entity) -> Option<Loop> {
        let c = curve_of(e).ok()??;
        if !c.is_closed() {
            return None;
        }
        let pts: Vec<Vec3> = match &c {
            Curve::Segs(segs) => segs.iter().flat_map(seg_points).collect(),
            Curve::Spline { points, .. } => points.clone(),
        };
        let o = *pts.first()?;
        let n = pts.iter().zip(pts.iter().cycle().skip(1)).fold(Vec3::ZERO, |n, (p, q)| n + (*p - o).cross(*q - o));
        let size = pts.iter().map(|p| (*p - o).len()).fold(0.0, f64::max);
        // NaN (points that aren't finite) is no plane either.
        let plane = n.len() > 1e-12 * size * size;
        if !plane {
            return None;
        }
        let n = n.normalized();
        if pts.iter().any(|p| n.dot(*p - o).abs() > 1e-6 * (1.0 + size)) {
            return None;
        }
        let mut l = Loop { h: e.handle, o, n, pts, poly: Vec::new() };
        l.poly = l.pts.iter().map(|p| l.to(*p)).collect();
        Some(l)
    }
    /// A world point in the loop's plane coordinates.
    fn to(&self, p: Vec3) -> Vec2 {
        let m = Mat4::ocs(self.n);
        Vec2::new(m.col(0).dot(p - self.o), m.col(1).dot(p - self.o))
    }
    fn contains(&self, p: Vec3) -> bool {
        point_in_polygon(&self.poly, self.to(p))
    }
    fn area(&self) -> f64 {
        self.poly.iter().zip(self.poly.iter().cycle().skip(1)).map(|(a, b)| a.x * b.y - b.x * a.y).sum::<f64>().abs() / 2.0
    }
    /// Where the aim meets the loop's plane.
    fn meet(&self, aim: Aim, tol: f64) -> Option<Vec3> {
        match aim {
            Aim::At(p) => (self.n.dot(p - self.o).abs() <= tol).then_some(p),
            Aim::Ray(o, d) => {
                let k = self.n.dot(d);
                (k.abs() > 1e-9 * d.len()).then(|| o + d * (self.n.dot(self.o - o) / k))
            }
        }
    }
}

/// The closed flat curves in the space being edited (not on locked layers).
fn loops(s: &Session) -> Result<Vec<Loop>> {
    let d = s.doc()?;
    Ok(d.space(&s.space())
        .into_iter()
        .flat_map(|st| st.iter())
        .filter(|e| d.is_visible(e) && !is_locked(s, e.handle))
        .filter_map(|e| Loop::of(e))
        .collect())
}

/// What a click pulls (see the module).
#[derive(Clone, Debug, PartialEq)]
enum Pull {
    /// A face of solid `h`, named by the point clicked on it, along its outward normal `n`.
    Face { h: Handle, at: Vec3, n: Vec3 },
    /// Curves on a face of solid `h`: the outline round the click and the holes inside it.
    Profile { h: Handle, outer: Handle, holes: Vec<Handle>, at: Vec3, n: Vec3 },
    /// A curve on its own, extruded along its plane's normal.
    Curve { h: Handle, at: Vec3, n: Vec3 },
}

impl Pull {
    fn base(&self) -> (Vec3, Vec3) {
        match self {
            Pull::Face { at, n, .. } | Pull::Profile { at, n, .. } | Pull::Curve { at, n, .. } => (*at, *n),
        }
    }
    fn json(&self, d: f64) -> Value {
        let v = |p: Vec3| json!([p.x, p.y, p.z]);
        match self {
            Pull::Face { h, at, .. } => json!({ "handle": h.hex(), "face": v(*at), "distance": d }),
            Pull::Profile { h, outer, holes, .. } => {
                json!({ "handle": h.hex(), "profile": outer.hex(), "holes": holes.iter().map(|h| h.hex()).collect::<Vec<_>>(), "distance": d })
            }
            Pull::Curve { h, .. } => json!({ "handles": [h.hex()], "distance": d }),
        }
    }
}

/// What an input clicks: a face of a solid (or the boundary drawn round the click on it), a
/// closed boundary on its own, or a curve picked. `None` when nothing is there.
fn pull_at(s: &Session, i: &Input) -> Result<Option<Pull>> {
    let Some(a) = aim(s, i, Sub::Face) else { return Ok(None) };
    let tol = super::curves::pick_aperture(s).max(1e-9) * 1e-3;
    let ls = loops(s)?;
    if let Some((h, f, at)) = super::subobj::pick(s, i, Sub::Face, None)? {
        let x = solid_of(s, "presspull", h)?;
        let t = topology(&x)?;
        let face = t.faces.get(f as usize).ok_or_else(|| EngineError::Other("The face isn't on the solid.".into()))?;
        let n = face.normal;
        let flat = face.kind == cadcraft_kernel::SurfaceKind::Plane;
        let tol = 1e-6 * (1.0 + super::subobj::size(&x));
        let on: Vec<&Loop> = ls.iter().filter(|l| flat && l.n.cross(n).len() < 1e-6 && n.dot(l.o - at).abs() <= tol).collect();
        let round = on.iter().filter(|l| l.contains(at)).min_by(|a, b| a.area().total_cmp(&b.area()));
        return Ok(Some(match round {
            Some(outer) => {
                let holes =
                    on.iter().filter(|l| l.h != outer.h && !l.contains(at) && l.pts.iter().all(|q| outer.contains(*q))).map(|l| l.h).collect();
                Pull::Profile { h, outer: outer.h, holes, at, n }
            }
            None => Pull::Face { h, at, n },
        }));
    }
    // A boundary on its own: the smallest round the point.
    let round = ls.iter().filter_map(|l| l.meet(a, tol).filter(|p| l.contains(*p)).map(|p| (l, p))).min_by(|a, b| a.0.area().total_cmp(&b.0.area()));
    if let Some((l, at)) = round {
        let n = s.doc()?.entity(l.h).map_or(l.n, |e| normal_of(e));
        return Ok(Some(Pull::Curve { h: l.h, at, n }));
    }
    // A curve picked on its line.
    let picked = match i {
        Input::Pick(hs) => hs.first().copied(),
        Input::Point(p) => pick_at(s, *p),
        _ => None,
    };
    let d = s.doc()?;
    Ok(picked.and_then(|h| d.entity(h)).filter(|e| curve_of(e).ok().flatten().is_some()).map(|e| {
        let at = match a {
            Aim::At(p) | Aim::Ray(p, _) => p,
        };
        Pull::Curve { h: e.handle, at, n: normal_of(e) }
    }))
}

fn run_presspull(s: &mut Session, p: &Value) -> Result<Value> {
    let dist = f64_req("presspull", p, "distance")?;
    if let Some(at) = p3(p, "at") {
        // Typed as a point with its height, the way the prompt reads it.
        let c = s.cplane();
        let local = c.inverse().map(|ci| ci.apply(at)).ok_or_else(|| bad("presspull", "the construction plane can't be inverted"))?;
        let (z, view) = (s.point_z, s.view_at);
        s.point_z = Some(local.z);
        s.view_at = None;
        let r = pull_at(s, &Input::Point(local.xy()));
        (s.point_z, s.view_at) = (z, view);
        let pull = r?.ok_or_else(|| EngineError::Other("There is no face or closed boundary there.".into()))?;
        return run_presspull(s, &pull.json(dist));
    }
    if p.get("profile").is_some_and(|v| !v.is_null()) {
        let (h, x) = target(s, "presspull", p)?;
        let outer = super::curves::handle_param(p, "profile").ok_or_else(|| bad("presspull", "`profile` must be a curve's handle"))?;
        let holes: Vec<Handle> = match p.get("holes") {
            Some(Value::Array(a)) => a.iter().filter_map(super::curves::handle_value).collect(),
            _ => Vec::new(),
        };
        let d = s.doc()?;
        let curve = |h: Handle| -> Result<Curve> {
            d.entity(h).and_then(|e| curve_of(e).ok().flatten()).ok_or_else(|| bad("presspull", format!("{} is not a closed curve", h.hex())))
        };
        let region = Region::Profile { outer: curve(outer)?, holes: holes.iter().map(|h| curve(*h)).collect::<Result<_>>()? };
        let b = cadcraft_kernel::press_pull(placed(&x), &region, dist).map_err(kernel)?;
        s.doc_mut()?.modify_entity(h, |e| reshape(e, b))?;
        // The boundary is used up, as EXTRUDE's closed profiles are.
        for c in std::iter::once(outer).chain(holes) {
            if !is_locked(s, c) {
                s.doc_mut()?.remove_entity(c);
            }
        }
        return Ok(json!({ "handle": h.hex() }));
    }
    if p.get("face").is_some_and(|v| !v.is_null()) {
        let (h, x) = target(s, "presspull", p)?;
        let face = named("presspull", &x, &json!({ "faces": [p.get("face")] }), "faces", Sub::Face)?;
        let Some(f) = face.first() else { return Err(bad("presspull", "`face` is required")) };
        let b = cadcraft_kernel::press_pull(placed(&x), &Region::Face(*f), dist).map_err(kernel)?;
        s.doc_mut()?.modify_entity(h, |e| reshape(e, b))?;
        return Ok(json!({ "handle": h.hex() }));
    }
    if p.get("handles").is_some_and(|v| !v.is_null()) {
        return run_body(s, "extrude", &json!({ "handles": p.get("handles"), "height": dist }));
    }
    Err(bad("presspull", "give `at`, `face`, `profile` or `handles`"))
}

/// PRESSPULL at the command line: click inside an area (or on a face), then drag or type the
/// height; again until Enter. Multiple takes several areas for one height.
/// ponytail: no live shape while dragging (a kernel call per mouse move), and a bounded area is
/// one closed object (not one HATCH-style boundary of several curves); upgrade: preview the area's
/// outline moved along its normal, and boundary detection from the hatch code.
#[derive(Default)]
struct PressPullM {
    pulls: Vec<Pull>,
    /// Multiple: more areas before the height.
    multiple: bool,
}

impl PressPullM {
    /// How far along the first pull's normal a point picked for the height is: where the eye's
    /// line through the cursor passes nearest the normal's line in a 3D view, else the point's
    /// height above the area.
    fn height(&self, s: &Session, p: Vec2) -> Option<f64> {
        let (b, n) = self.pulls.first()?.base();
        if let Some((dir, v)) = s.view_at {
            let (x, y, d) = dir.axes();
            let o = x * v.x + y * v.y;
            // Closest points of the lines b + n t and o + d u.
            let (w, nd) = (b - o, n.dot(d));
            let den = 1.0 - nd * nd;
            if den > 1e-9 {
                return Some((nd * d.dot(w) - n.dot(w)) / den);
            }
        }
        Some(n.dot(world(s, p) - b))
    }
    fn run(&mut self, s: &mut Session, d: f64) -> Result<Step> {
        if !(d.is_finite() && d.abs() > 1e-9) {
            return Err(EngineError::Other("Requires a height other than zero.".into()));
        }
        let pulls = std::mem::take(&mut self.pulls);
        self.multiple = false;
        for pull in pulls {
            run_body(s, "presspull", &pull.json(d))?;
        }
        s.set_selection(Vec::new());
        Ok(Step::Continue)
    }
}

impl Interactive for PressPullM {
    fn name(&self) -> &'static str {
        "PRESSPULL"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.pulls.is_empty() || self.multiple {
            Prompt::new("Select object or bounded area", Accept::POINT)
        } else {
            Prompt::new("Specify extrusion height", Accept::POINT_OR_NUMBER).kw(&["Multiple"]).base_opt(None)
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        let picking = self.pulls.is_empty() || self.multiple;
        match (&i, picking) {
            (Input::Enter, true) if self.pulls.is_empty() => Ok(Step::Done),
            (Input::Enter, true) => {
                self.multiple = false;
                Ok(Step::Continue)
            }
            (Input::Point(_) | Input::Pick(_), true) => {
                match pull_at(s, &i)? {
                    Some(p) => {
                        if !self.pulls.contains(&p) {
                            self.pulls.push(p);
                        }
                    }
                    None => s.echo("No face or closed boundary found there."),
                }
                Ok(Step::Continue)
            }
            (Input::Text(t) | Input::Keyword(t), false) if self.prompt(s).match_keyword(t).is_some() => {
                self.multiple = true;
                Ok(Step::Continue)
            }
            (Input::Text(t), false) => {
                let d = distance(t)?;
                self.run(s, d)
            }
            (Input::Point(p), false) => {
                let d = self.height(s, *p).ok_or_else(|| EngineError::Other("Requires a height.".into()))?;
                self.run(s, d)
            }
            _ => Ok(Step::Continue),
        }
    }
}

// ---------------- THICKEN ----------------

fn run_thicken(s: &mut Session, p: &Value) -> Result<Value> {
    let t = f64_req("thicken", p, "thickness")?;
    let both = bool_or(p, "bothSides", false);
    let hs = targets(s, p)?;
    let mut xs = solids(s, &hs)?;
    xs.retain(|(_, x)| x.mesh.open && !x.brep.is_empty());
    let all = xs.len();
    xs.retain(|(h, _)| !is_locked(s, *h));
    if xs.is_empty() {
        return Err(if all > 0 { locked_err() } else { bad("thicken", "select surfaces") });
    }
    let built = xs.iter().map(|(h, x)| Ok((*h, cadcraft_kernel::thicken(placed(x), t, both).map_err(kernel)?))).collect::<Result<Vec<_>>>()?;
    let n = built.len();
    let d = s.doc_mut()?;
    for (h, b) in built {
        d.modify_entity(h, |e| reshape(e, b))?;
    }
    Ok(json!({ "thickened": n, "locked": all - n }))
}

/// THICKEN at the command line: the surfaces, then the thickness.
#[derive(Default)]
struct ThickenM {
    sel: SelectPhase,
    hs: Vec<Handle>,
}

impl Interactive for ThickenM {
    fn name(&self) -> &'static str {
        "THICKEN"
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
            (false, _) => Prompt::new("Specify thickness", Accept::NUMBER),
            (true, true) => self.sel.prompt(),
            (true, false) => Prompt::new("Select surfaces to thicken", Accept::SELECT),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_empty() {
            return Ok(match self.sel.feed(s, &i)? {
                SelOutcome::More => Step::Continue,
                SelOutcome::Empty => Step::Done,
                SelOutcome::Done(hs) => {
                    self.hs = hs;
                    Step::Continue
                }
            });
        }
        let Input::Text(t) = i else { return Ok(Step::Continue) };
        let hs: Vec<String> = self.hs.iter().map(|h| h.hex()).collect();
        run_body(s, "thicken", &json!({ "handles": hs, "thickness": distance(&t)? }))?;
        s.set_selection(Vec::new());
        Ok(Step::Done)
    }
}

// ---------------- XEDGES ----------------

/// ponytail: 3D solids and surfaces only; regions (EXPLODE draws their loops) and single faces
/// picked with Ctrl when someone needs them.
fn run_xedges(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let xs = solids(s, &hs)?;
    let mut made = Vec::new();
    for (h, x) in xs.iter().filter(|(_, x)| !x.brep.is_empty()) {
        let t = topology(x)?;
        let d = s.doc()?;
        let (Some(common), Some(space)) = (d.entity(*h).map(|e| e.common.clone()), d.space_of(*h)) else { continue };
        let ents: Vec<Entity> = t.edges.iter().filter(|e| !e.seam && !e.degenerate).filter_map(|e| edge_entity(&e.curve, &common)).collect();
        let d = s.doc_mut()?;
        for e in ents {
            made.push(d.add(&space, e.common, e.kind)?.hex());
        }
    }
    if made.is_empty() && xs.is_empty() {
        return Err(bad("xedges", "select 3D solids or surfaces"));
    }
    let n = made.len();
    Ok(json!({ "handles": made, "message": format!("{n} object(s) created.") }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_and_refusals() {
        let mut s = Session::new();
        s.start("presspull").unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Select object or bounded area:");
        // Nothing there.
        s.input(Input::Point(Vec2::new(5.0, 5.0))).unwrap();
        assert!(s.log.last().unwrap().contains("No face or closed boundary"), "{:?}", s.log);
        s.input(Input::Enter).unwrap();
        assert!(s.running.is_none());
        let err = |s: &mut Session, id: &str, p: Value| s.execute(id, &p).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err(&mut s, "presspull", json!({ "distance": 5 })).contains("give `at`"));
        assert!(err(&mut s, "presspull", json!({ "at": [0, 0, 0] })).contains("distance"));
        assert!(err(&mut s, "thicken", json!({ "handles": [], "thickness": 2 })).contains("select surfaces"));
        assert!(err(&mut s, "xedges", json!({ "handles": [] })).contains("select 3D solids"));
    }

    /// A rectangle on its own: a click inside it finds it, and the closed polyline round the
    /// point (the inner of two) wins.
    #[test]
    fn a_click_inside_a_boundary_finds_it() {
        let mut s = Session::new();
        let pv = |x: f64, y: f64| cadcraft_geom::PolyVertex::new(Vec2::new(x, y));
        let big = s.add_entity(super::super::helpers::lwpoly(vec![pv(0.0, 0.0), pv(100.0, 0.0), pv(100.0, 50.0), pv(0.0, 50.0)], true)).unwrap();
        let small = s.add_entity(super::super::helpers::lwpoly(vec![pv(10.0, 10.0), pv(20.0, 10.0), pv(20.0, 20.0), pv(10.0, 20.0)], true)).unwrap();
        let got = pull_at(&s, &Input::Point(Vec2::new(15.0, 15.0))).unwrap();
        assert!(matches!(got, Some(Pull::Curve { h, n, .. }) if h == small && n == Vec3::Z), "{got:?}");
        let got = pull_at(&s, &Input::Point(Vec2::new(50.0, 40.0))).unwrap();
        assert!(matches!(got, Some(Pull::Curve { h, .. }) if h == big), "{got:?}");
        assert!(pull_at(&s, &Input::Point(Vec2::new(500.0, 40.0))).unwrap().is_none());
    }
}

#[cfg(test)]
#[cfg(feature = "occt")]
mod occt_tests {
    use cadcraft_doc::{EntityKind, Part, Solid3d};
    use cadcraft_geom::ViewDir;

    use super::*;

    fn solid_x(s: &Session, h: Handle) -> Solid3d {
        let EntityKind::Solid3d(x) = &s.doc().unwrap().entity(h).unwrap().kind else { panic!() };
        x.clone()
    }

    fn zmax(x: &Solid3d) -> f64 {
        x.mesh.verts.iter().map(|p| x.world(*p).z).fold(f64::MIN, f64::max)
    }

    /// A worktop: its top pulled up in the iso view, a circle on it pushed in as a hole, then a
    /// rectangle on the floor pulled into a plinth.
    #[test]
    fn faces_boundaries_and_curves() {
        let mut s = Session::new();
        let r = s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [600, 400], "height": 18 })).unwrap();
        let h = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
        s.doc_mut()
            .unwrap()
            .modify_entity(h, |e| {
                if let EntityKind::Solid3d(x) = &mut e.kind {
                    x.part = Part { name: "Top".into(), material: "Oak".into() }
                }
            })
            .unwrap();
        s.start("presspull").unwrap();
        let iso = ViewDir::named("iso").unwrap();
        s.view_at = Some((iso, iso.project(Vec3::new(300.0, 200.0, 18.0))));
        s.input(Input::Pick(vec![h])).unwrap();
        s.view_at = None;
        assert_eq!(s.current_prompt().unwrap().display(), "Specify extrusion height or [Multiple]:");
        s.input(Input::Text("12".into())).unwrap();
        assert!((zmax(&solid_x(&s, h)) - 30.0).abs() < 1e-6, "{:?}", s.log);
        s.input(Input::Enter).unwrap();
        assert!(s.running.is_none());
        // A circle on the top, pushed through: a hole; the circle is used up.
        let c = s.add_entity(EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::new(100.0, 100.0, 30.0), radius: 20.0 })).unwrap();
        let v0 = cadcraft_kernel::topology(placed(&solid_x(&s, h))).unwrap().faces.len();
        s.execute("presspull", &json!({ "at": [100, 100, 30], "distance": -30 })).unwrap();
        assert!(s.doc().unwrap().entity(c).is_none());
        assert_eq!(cadcraft_kernel::topology(placed(&solid_x(&s, h))).unwrap().faces.len(), v0 + 1, "a hole's wall");
        assert_eq!(solid_x(&s, h).part.name, "Top");
        // A rectangle on its own pulls into a solid.
        let pv = |x: f64, y: f64| cadcraft_geom::PolyVertex::new(Vec2::new(x, y));
        s.add_entity(super::super::helpers::lwpoly(vec![pv(0.0, 500.0), pv(600.0, 500.0), pv(600.0, 580.0), pv(0.0, 580.0)], true)).unwrap();
        let r = s.execute("presspull", &json!({ "at": [300, 540, 0], "distance": -100 })).unwrap();
        assert_eq!(r["handles"].as_array().map(Vec::len), Some(1), "{r}");
    }

    #[test]
    fn thicken_and_xedges() {
        let mut s = Session::new();
        let r = s.execute("planesurf", &json!({ "corner1": [0, 0, 0], "corner2": [600, 400] })).unwrap();
        let h = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
        s.execute("thicken", &json!({ "handles": [h.hex()], "thickness": 18 })).unwrap();
        let x = solid_x(&s, h);
        assert!(!x.mesh.open && (zmax(&x) - 18.0).abs() < 1e-6);
        // A sound solid: its top pulls up.
        let r = s.execute("solidedit", &json!({ "handle": h.hex(), "op": "check" })).unwrap();
        assert_eq!(r["valid"], true, "{r}");
        s.execute("presspull", &json!({ "handle": h.hex(), "face": [300, 200, 18], "distance": 10 })).unwrap();
        assert!((zmax(&solid_x(&s, h)) - 28.0).abs() < 1e-6, "{}", zmax(&solid_x(&s, h)));
        s.undo().unwrap();
        let r = s.execute("xedges", &json!({ "handles": [h.hex()] })).unwrap();
        assert_eq!(r["handles"].as_array().map(Vec::len), Some(12), "{r}");
        let c = s.execute("cylinder", &json!({ "center": [0, 0, 0], "radius": 5, "height": 10 })).unwrap();
        let r = s.execute("xedges", &json!({ "handles": [c["handle"]] })).unwrap();
        let hs: Vec<Handle> = r["handles"].as_array().unwrap().iter().map(|v| Handle::parse_hex(v.as_str().unwrap()).unwrap()).collect();
        assert_eq!(hs.len(), 2, "two circles, no seam");
        assert!(hs.iter().all(|h| matches!(s.doc().unwrap().entity(*h).unwrap().kind, EntityKind::Circle(_))));
    }

    /// A planar surface pressed or pulled becomes a solid (it once failed, "joining the solids
    /// failed"); CHECK calls a surface a surface.
    #[test]
    fn a_surface_pulls_into_a_solid() {
        let mut s = Session::new();
        let r = s.execute("planesurf", &json!({ "corner1": [0, 0, 0], "corner2": [100, 50] })).unwrap();
        let h = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
        let r = s.execute("solidedit", &json!({ "handle": h.hex(), "op": "check" })).unwrap();
        assert_eq!(r["valid"], false, "{r}");
        assert!(r.to_string().contains("surface"), "{r}");
        s.execute("presspull", &json!({ "at": [50, 25, 0], "distance": 10 })).unwrap();
        let x = solid_x(&s, h);
        assert!(!x.mesh.open && (zmax(&x) - 10.0).abs() < 1e-6, "{}", zmax(&x));
    }

    /// IMPRINT two circles on a worktop, then PRESSPULL each: the second is still there to pull
    /// (once, the first PRESSPULL wiped every imprint and the second took the whole top).
    #[test]
    fn imprints_stay_for_the_next_presspull() {
        let mut s = Session::new();
        let r = s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [600, 400], "height": 18 })).unwrap();
        let h = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
        for x in [150.0, 450.0] {
            let c = s.add_entity(EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::new(x, 200.0, 18.0), radius: 50.0 })).unwrap();
            s.execute("imprint", &json!({ "handle": h.hex(), "objects": [c.hex()], "delete": true })).unwrap();
        }
        s.execute("presspull", &json!({ "at": [150, 200, 18], "distance": -18 })).unwrap();
        s.execute("presspull", &json!({ "at": [450, 200, 18], "distance": -10 })).unwrap();
        let x = solid_x(&s, h);
        assert!((zmax(&x) - 18.0).abs() < 1e-6, "the top stays put: {}", zmax(&x));
    }
}
