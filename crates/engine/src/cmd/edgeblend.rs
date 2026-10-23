//! FILLETEDGE and CHAMFEREDGE (CAD-rhino): round or bevel chosen edges of a 3D solid, picked one
//! by one, as a smooth chain or as a face's loop; or every edge of each solid (the JSON form
//! without `edges`). Rhino's FilletEdge and ChamferEdge reach them.

use cadcraft_kernel::Bevel;
use serde_json::{Value, json};

use super::solids::{distance, kernel, per_solid, placed, reshape};
use super::subobj::{Picks, Sub, chain, face_loop, highlight, named, size, target};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("filletedge", "Fillet Edge", run_filletedge)
            .menu(&["Modify", "Solid Editing", "Fillet Edges"])
            .params(
                "{handle, edges: [index | [x,y,z] (a point on the edge)], radius}: rounds those edges of the solid | {handle | handles?, radius}: every edge of each solid",
            )
            .interactive(|_| Ok(Box::new(BlendM::new(false)))),
        CommandSpec::new("chamferedge", "Chamfer Edge", run_chamferedge)
            .menu(&["Modify", "Solid Editing", "Chamfer Edges"])
            .params(
                "{handle, edges: [index | [x,y,z]], distance1, distance2? (default distance1), face?: index | [x,y,z] (the base face distance1 runs along)} | {handle | handles?, distance}: every edge of each solid",
            )
            .interactive(|_| Ok(Box::new(BlendM::new(true)))),
    ]
}

/// One face or edge named by `key` (an index or a point), as [`named`] reads a list.
fn named_one(cmd: &str, x: &cadcraft_doc::Solid3d, p: &Value, key: &str, kind: Sub) -> Result<Option<u32>> {
    match p.get(key).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => Ok(named(cmd, x, &json!({ key: [v] }), key, kind)?.first().copied()),
    }
}

fn has(p: &Value, key: &str) -> bool {
    p.get(key).is_some_and(|v| !v.is_null())
}

fn run_filletedge(s: &mut Session, p: &Value) -> Result<Value> {
    let r = f64_req("filletedge", p, "radius")?;
    if has(p, "edges") {
        let (h, x) = target(s, "filletedge", p)?;
        let edges = named("filletedge", &x, p, "edges", Sub::Edge)?;
        let b = cadcraft_kernel::fillet_edges(placed(&x), &edges.iter().map(|e| (*e, r)).collect::<Vec<_>>()).map_err(kernel)?;
        s.doc_mut()?.modify_entity(h, |e| reshape(e, b))?;
        return Ok(json!({ "filleted": 1, "edges": edges.len(), "handle": h.hex(), "locked": 0 }));
    }
    let hs = targets(s, p)?;
    let (n, locked) = per_solid(s, "filletedge", &hs, |x| cadcraft_kernel::fillet(x, r))?;
    Ok(json!({ "filleted": n, "locked": locked }))
}

fn run_chamferedge(s: &mut Session, p: &Value) -> Result<Value> {
    let d1 = match p.get("distance1").filter(|v| !v.is_null()) {
        Some(_) => f64_req("chamferedge", p, "distance1")?,
        None => f64_req("chamferedge", p, "distance")?,
    };
    if has(p, "edges") {
        let d2 = f64_or(p, "distance2", d1);
        let (h, x) = target(s, "chamferedge", p)?;
        let edges = named("chamferedge", &x, p, "edges", Sub::Edge)?;
        let face = named_one("chamferedge", &x, p, "face", Sub::Face)?;
        let bevels: Vec<Bevel> = edges.iter().map(|e| Bevel { edge: *e, base: d1, other: d2, face }).collect();
        let b = cadcraft_kernel::chamfer_edges(placed(&x), &bevels).map_err(kernel)?;
        s.doc_mut()?.modify_entity(h, |e| reshape(e, b))?;
        return Ok(json!({ "chamfered": 1, "edges": edges.len(), "handle": h.hex(), "locked": 0 }));
    }
    let hs = targets(s, p)?;
    let (n, locked) = per_solid(s, "chamferedge", &hs, |x| cadcraft_kernel::chamfer(x, d1))?;
    Ok(json!({ "chamfered": n, "locked": locked }))
}

/// How a pick adds edges.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Edge,
    Chain,
    Loop,
}

/// What is being asked besides edges.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ask {
    Pick,
    Radius,
    D1,
    D2,
    Accept,
    /// A face's loop is shown: take it, or show the other face's.
    Next,
}

/// FILLETEDGE and CHAMFEREDGE at the command line, as AutoCAD's: edges picked on one solid (an
/// edge, its smooth chain, or a face's loop), Enter, then Enter again to apply; the radius or the
/// two distances can be changed on the way.
/// ponytail: no Expression option, and the radius is one for every edge.
struct BlendM {
    chamfer: bool,
    picks: Picks,
    mode: Mode,
    ask: Ask,
    /// Where a value prompt goes back to (picking, or the final Enter).
    back: Ask,
    radius: f64,
    d1: f64,
    d2: f64,
    /// The loop on offer: the edge picked, its faces, which one's loop is shown.
    looping: Option<(u32, Vec<u32>, usize)>,
}

impl BlendM {
    fn new(chamfer: bool) -> Self {
        BlendM {
            chamfer,
            picks: Picks::new(Sub::Edge),
            mode: Mode::Edge,
            ask: Ask::Pick,
            back: Ask::Pick,
            radius: 1.0,
            d1: 1.0,
            d2: 1.0,
            looping: None,
        }
    }
    fn tol(&self) -> f64 {
        1e-6 * (1.0 + self.picks.solid.as_ref().map_or(0.0, |(_, x)| size(x)))
    }
    /// The loop on offer, if any.
    fn offered(&self) -> Vec<u32> {
        let (Some((e, faces, i)), Some(t)) = (&self.looping, &self.picks.topo) else { return Vec::new() };
        faces.get(*i).map(|f| face_loop(t, *f, *e, self.tol())).unwrap_or_default()
    }
    fn apply(&mut self, s: &mut Session) -> Result<Step> {
        let Some(h) = self.picks.handle() else { return Ok(Step::Done) };
        let edges = self.picks.picked.clone();
        let r = if self.chamfer {
            // The base face is the one every edge is on, when there is one.
            let t = self.picks.topo()?;
            let on = |e: &u32| t.edges.get(*e as usize).map(|x| x.faces.clone()).unwrap_or_default();
            let face = edges.first().map(on).unwrap_or_default().into_iter().find(|f| edges.iter().all(|e| on(e).contains(f)));
            run_body(s, "chamferedge", &json!({ "handle": h.hex(), "edges": edges, "distance1": self.d1, "distance2": self.d2, "face": face }))
        } else {
            run_body(s, "filletedge", &json!({ "handle": h.hex(), "edges": edges, "radius": self.radius }))
        };
        r?;
        s.set_selection(Vec::new());
        Ok(Step::Done)
    }
    fn picked(&mut self, s: &mut Session, i: &Input) -> Result<()> {
        let id = if self.chamfer { "chamferedge" } else { "filletedge" };
        let Some((e, _)) = self.picks.hit(s, id, i)? else { return Ok(()) };
        let tol = self.tol();
        let t = self.picks.topo()?;
        match self.mode {
            Mode::Edge => {
                self.picks.take(&[e]);
            }
            Mode::Chain => {
                let c = chain(t, e, tol);
                self.picks.take(&c);
            }
            Mode::Loop => {
                let faces = t.edges.get(e as usize).map(|x| x.faces.clone()).unwrap_or_default();
                self.looping = Some((e, faces, 0));
                self.ask = Ask::Next;
            }
        }
        Ok(())
    }
}

impl Interactive for BlendM {
    fn name(&self) -> &'static str {
        if self.chamfer { "CHAMFEREDGE" } else { "FILLETEDGE" }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        // The radius is remembered in the drawing (FILLETRAD3D, 1 to start).
        self.radius = s.doc()?.header.f64("FILLETRAD3D", 1.0);
        if !(self.radius.is_finite() && self.radius > 0.0) {
            self.radius = 1.0;
        }
        s.echo(if self.chamfer { format!("Distance1 = {:.4}, Distance2 = {:.4}", self.d1, self.d2) } else { format!("Radius = {:.4}", self.radius) });
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        let none = self.picks.picked.is_empty();
        match (self.ask, self.chamfer, self.mode) {
            (Ask::Radius, ..) => Prompt::new("Enter fillet radius", Accept::NUMBER).default(format!("{:.4}", self.radius)),
            (Ask::D1, ..) => Prompt::new("Specify Distance1", Accept::NUMBER).default(format!("{:.4}", self.d1)),
            (Ask::D2, ..) => Prompt::new("Specify Distance2", Accept::NUMBER).default(format!("{:.4}", self.d2)),
            (Ask::Accept, true, _) => Prompt::new("Press Enter to accept the chamfer", super::curves::KW).kw(&["Distance"]),
            (Ask::Accept, false, _) => Prompt::new("Press Enter to accept the fillet", super::curves::KW).kw(&["Radius"]),
            (Ask::Next, ..) => Prompt::new("Enter an option", super::curves::KW).kw(&["Accept", "Next"]).default("Accept"),
            (Ask::Pick, false, Mode::Edge) => Prompt::new("Select an edge", Accept::POINT).kw(&["Chain", "Loop", "Radius"]),
            (Ask::Pick, false, Mode::Chain) => Prompt::new("Select edge chain", Accept::POINT).kw(&["Edge", "Radius"]),
            (Ask::Pick, false, Mode::Loop) => Prompt::new("Select edge loop", Accept::POINT).kw(&["Edge", "Chain", "Radius"]),
            (Ask::Pick, true, Mode::Loop) => Prompt::new("Select edge loop", Accept::POINT).kw(&["Edge", "Distance"]),
            (Ask::Pick, true, _) if none => Prompt::new("Select an edge", Accept::POINT).kw(&["Loop", "Distance"]),
            (Ask::Pick, true, _) => Prompt::new("Select another edge on the same face", Accept::POINT).kw(&["Loop", "Distance"]),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        let kw = match &i {
            Input::Text(t) | Input::Keyword(t) => self.prompt(s).match_keyword(t),
            _ => None,
        };
        // A value: typed, or Enter for the one shown.
        if matches!(self.ask, Ask::Radius | Ask::D1 | Ask::D2) {
            let v = match &i {
                Input::Enter => None,
                Input::Text(t) => {
                    Some(distance(t).ok().filter(|v| *v > 0.0).ok_or_else(|| EngineError::Other("Requires a positive distance.".into()))?)
                }
                _ => return Ok(Step::Continue),
            };
            match self.ask {
                Ask::Radius => {
                    if let Some(r) = v {
                        self.radius = r;
                        s.doc_mut()?.header.set_f64("FILLETRAD3D", r);
                    }
                    self.ask = self.back;
                }
                Ask::D1 => {
                    if let Some(d) = v {
                        self.d1 = d;
                    }
                    self.ask = Ask::D2;
                }
                _ => {
                    if let Some(d) = v {
                        self.d2 = d;
                    }
                    self.ask = self.back;
                }
            }
            return Ok(Step::Continue);
        }
        let value = if self.chamfer { Ask::D1 } else { Ask::Radius };
        match (self.ask, kw.as_deref(), &i) {
            (Ask::Next, Some("Next"), _) => {
                if let Some((_, faces, k)) = &mut self.looping {
                    *k = (*k + 1) % faces.len().max(1);
                }
            }
            (Ask::Next, Some("Accept"), _) | (Ask::Next, _, Input::Enter) => {
                let l = self.offered();
                self.picks.take(&l);
                self.looping = None;
                self.ask = Ask::Pick;
            }
            (Ask::Next, ..) => {}
            (Ask::Accept, None, Input::Enter) => return self.apply(s),
            (Ask::Accept | Ask::Pick, Some("Radius" | "Distance"), _) => {
                self.back = self.ask;
                self.ask = value;
            }
            (Ask::Pick, Some("Edge"), _) => self.mode = Mode::Edge,
            (Ask::Pick, Some("Chain"), _) => self.mode = Mode::Chain,
            (Ask::Pick, Some("Loop"), _) => self.mode = Mode::Loop,
            (Ask::Pick, None, Input::Enter) => {
                let n = self.picks.picked.len();
                if n == 0 {
                    return Ok(Step::Done);
                }
                s.echo(if self.chamfer { format!("{n} edge(s) selected for chamfer.") } else { format!("{n} edge(s) selected for fillet.") });
                self.ask = Ask::Accept;
            }
            (Ask::Pick, None, Input::Point(_) | Input::Pick(_)) => self.picked(s, &i)?,
            (_, None, Input::Text(_) | Input::Keyword(_)) => return Err(EngineError::Other("Invalid option keyword.".into())),
            _ => {}
        }
        Ok(Step::Continue)
    }
    fn preview(&self, _s: &Session, _cursor: cadcraft_geom::Vec2) -> Vec<cadcraft_doc::EntityKind> {
        let mut out = self.picks.preview();
        if let Some(t) = &self.picks.topo {
            out.extend(highlight(t, Sub::Edge, &self.offered()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prompts read as AutoCAD's, and change with the options.
    #[test]
    fn prompts_follow_the_options() {
        let mut s = Session::new();
        s.start("filletedge").unwrap();
        assert!(s.log.iter().any(|l| l == "Radius = 1.0000"), "{:?}", s.log);
        assert_eq!(s.current_prompt().unwrap().display(), "Select an edge or [Chain/Loop/Radius]:");
        s.input(Input::Text("C".into())).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Select edge chain or [Edge/Radius]:");
        s.input(Input::Text("r".into())).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Enter fillet radius <1.0000>:");
        s.input(Input::Text("6".into())).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Select edge chain or [Edge/Radius]:");
        assert_eq!(s.doc().unwrap().header.f64("FILLETRAD3D", 0.0), 6.0);
        // Nothing picked: Enter ends it.
        s.input(Input::Enter).unwrap();
        assert!(s.running.is_none());

        s.start("chamferedge").unwrap();
        assert!(s.log.iter().any(|l| l == "Distance1 = 1.0000, Distance2 = 1.0000"), "{:?}", s.log);
        assert_eq!(s.current_prompt().unwrap().display(), "Select an edge or [Loop/Distance]:");
        s.input(Input::Text("d".into())).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Specify Distance1 <1.0000>:");
        s.input(Input::Text("3".into())).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Specify Distance2 <1.0000>:");
        let _ = s.input(Input::Text("-1".into()));
        assert_eq!(s.current_prompt().unwrap().display(), "Specify Distance2 <1.0000>:", "a bad distance is asked again");
        s.input(Input::Enter).unwrap();
        assert_eq!(s.current_prompt().unwrap().display(), "Select an edge or [Loop/Distance]:");
        s.input(Input::Cancel).unwrap();
    }

    #[test]
    fn json_refuses_bad_edges_before_the_kernel() {
        let mut s = Session::new();
        let h = s.add_entity(cadcraft_doc::EntityKind::Solid3d(cadcraft_doc::Solid3d::default())).unwrap();
        let err = |s: &mut Session, p: Value| s.execute("filletedge", &p).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err(&mut s, json!({ "handle": h.hex(), "edges": [1], "radius": 1 })).contains("mesh only"));
        assert!(err(&mut s, json!({ "handle": "zz", "edges": [1], "radius": 1 })).contains("not an object handle"));
        assert!(err(&mut s, json!({ "edges": [1], "radius": 1 })).contains("`handle`"));
        assert!(err(&mut s, json!({ "handle": h.hex(), "edges": [1] })).contains("radius"));
    }
}

#[cfg(test)]
#[cfg(feature = "occt")]
mod occt_tests {
    use cadcraft_doc::{EntityKind, Handle, Part};
    use cadcraft_geom::{Vec2, ViewDir};

    use super::*;

    fn panel(s: &mut Session) -> Handle {
        let r = s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [600, 400], "height": 18 })).unwrap();
        let h = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
        s.doc_mut()
            .unwrap()
            .modify_entity(h, |e| {
                if let EntityKind::Solid3d(x) = &mut e.kind {
                    x.part = Part { name: "Top".into(), material: "Oak 18".into() }
                }
            })
            .unwrap();
        h
    }

    fn faces_of(s: &Session, h: Handle) -> (usize, cadcraft_doc::Solid3d) {
        let EntityKind::Solid3d(x) = &s.doc().unwrap().entity(h).unwrap().kind else { panic!() };
        (super::super::subobj::topology(x).unwrap().faces.len(), x.clone())
    }

    /// Edges named by a point on them: a box with one top edge rounded has 7 faces, keeps its part,
    /// and undoes in one step.
    #[test]
    fn fillet_and_chamfer_chosen_edges() {
        let mut s = Session::new();
        let h = panel(&mut s);
        s.execute("filletedge", &json!({ "handle": h.hex(), "edges": [[300, 0, 18]], "radius": 5 })).unwrap();
        let (n, x) = faces_of(&s, h);
        assert_eq!(n, 7);
        assert_eq!(x.part.name, "Top");
        s.undo().unwrap();
        assert_eq!(faces_of(&s, h).0, 6);
        s.execute("chamferedge", &json!({ "handle": h.hex(), "edges": [[300, 0, 18], [0, 200, 18]], "distance1": 3, "distance2": 6 })).unwrap();
        assert_eq!(faces_of(&s, h).0, 8);
        // Too large a radius is refused, and the solid stays.
        assert!(s.execute("filletedge", &json!({ "handle": h.hex(), "edges": [[300, 400, 0]], "radius": 500 })).is_err());
        assert_eq!(faces_of(&s, h).0, 8);
    }

    /// Picked at the prompts: clicks in a 3D view (the eye's line through the cursor) and typed
    /// points; Loop takes a face's outline.
    #[test]
    fn prompts_pick_edges_in_views() {
        let mut s = Session::new();
        let h = panel(&mut s);
        s.start("filletedge").unwrap();
        // A typed point on the front top edge.
        s.point_z = Some(18.0);
        s.input(Input::Point(Vec2::new(300.0, 0.0))).unwrap();
        s.point_z = None;
        assert_eq!(s.current_prompt().unwrap().message, "Select an edge");
        // The SW iso view, the cursor over the left top edge's middle.
        let iso = ViewDir::named("iso").unwrap();
        s.view_at = Some((iso, iso.project(cadcraft_geom::Vec3::new(0.0, 200.0, 18.0))));
        s.input(Input::Pick(vec![h])).unwrap();
        s.view_at = None;
        s.input(Input::Enter).unwrap();
        assert!(s.log.iter().any(|l| l == "2 edge(s) selected for fillet."), "{:?}", s.log);
        s.input(Input::Text("r".into())).unwrap();
        s.input(Input::Text("4".into())).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(s.running.is_none(), "{:?}", s.log);
        assert_eq!(faces_of(&s, h).0, 8);
        s.undo().unwrap();
        // Loop: the top face's four edges.
        s.start("chamferedge").unwrap();
        s.input(Input::Text("L".into())).unwrap();
        s.point_z = Some(18.0);
        s.input(Input::Point(Vec2::new(600.0, 100.0))).unwrap();
        s.point_z = None;
        assert_eq!(s.current_prompt().unwrap().display(), "Enter an option [Accept/Next] <Accept>:");
        // The edge's faces come in kernel order; show each loop till the top face's (4 edges, z 18).
        for _ in 0..2 {
            let t = s.preview(Vec2::ZERO);
            if t.len() == 4 && t.iter().all(|e| matches!(&e.kind, EntityKind::Polyline3d(p) if p.points.iter().all(|q| (q.z - 18.0).abs() < 1e-9))) {
                break;
            }
            s.input(Input::Text("n".into())).unwrap();
        }
        s.input(Input::Enter).unwrap();
        s.input(Input::Enter).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(s.running.is_none(), "{:?}", s.log);
        assert_eq!(faces_of(&s, h).0, 10, "{:?}", s.log);
    }
}
