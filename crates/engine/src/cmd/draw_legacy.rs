//! AutoCAD's older drawing commands: SOLID (2D filled solids), TRACE (wide segments), 3DFACE
//! (three- and four-sided faces with invisible edges) and SKETCH (freehand lines, polylines or
//! splines).

use cadcraft_doc::{EntityKind, Face3d, Solid};
use cadcraft_geom::{FitOptions, PolyVertex, Spline, Vec2, Vec3};
use serde_json::{Value, json};

use super::curves::{KW, MAX_GEN};
use super::helpers::{line, lwpoly};
use super::machines::number;
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("solid", "2D Solid", run_solid)
            .alias(&["so"])
            .params("{points: [[x,y], ...]} (3 or 4 corners in AutoCAD's order: the 3rd and 4th are opposite the 1st and 2nd; each further pair adds a solid on the last two corners, a lone last point a triangle) → {handles}")
            .interactive(|_| Ok(Box::new(FaceM::new(false)))),
        CommandSpec::new("trace", "Trace", run_trace)
            .params("{points: [[x,y], ...] (2 or more), width? (default TRACEWID)} → {handles}: one TRACE per segment, mitred where segments meet")
            .interactive(|s| Ok(Box::new(TraceM { width: trace_width(s), asked: false, pts: Vec::new(), made: 0 }))),
        CommandSpec::new("3dface", "3D Face", run_3dface)
            .menu(&["Draw", "Modeling", "Meshes", "3D Face"])
            .alias(&["3f"])
            .params("{points: [[x,y,z?], ...] (3 or 4 corners in order around the face; each further pair adds a face on the last two corners), invisible?: [point index] (the edge from that point on is invisible, as typing I before it)} → {handles}")
            .interactive(|_| Ok(Box::new(FaceM::new(true)))),
        CommandSpec::new("sketch", "Sketch", run_sketch)
            .params("{points: [[x,y], ...] | strokes: [[[x,y], ...], ...], type?: line|polyline|spline (default SKPOLY), increment? (default SKETCHINC: closer points are dropped), tolerance? (spline fit, default SKTOLERANCE)} → {handles}")
            .interactive(|s| Ok(Box::new(SketchM::new(s)))),
    ]
}

fn other(m: impl Into<String>) -> EngineError {
    EngineError::Other(m.into())
}

fn handles(hs: &[cadcraft_doc::Handle]) -> Value {
    json!({ "handles": hs.iter().map(|h| h.hex()).collect::<Vec<_>>() })
}

// ---------------- SOLID and 3DFACE ----------------

/// The corners of each solid or face `pts` make: the first four, then each further pair with the
/// last two before it (a missing fourth repeats the third). Indices into `pts`.
fn faces(n: usize) -> Vec<[usize; 4]> {
    (0..n.saturating_sub(1) / 2).map(|k| [2 * k, 2 * k + 1, 2 * k + 2, (2 * k + 3).min(n - 1)]).collect()
}

fn points3(cmd: &str, p: &Value) -> Result<Vec<Vec3>> {
    let a = p.get("points").and_then(Value::as_array).ok_or_else(|| bad(cmd, "`points` [[x,y,z?], ...] (3 or more) is required"))?;
    if a.len() < 3 || a.len() > MAX_GEN {
        return Err(bad(cmd, "give 3 or more corners"));
    }
    a.iter().map(|v| super::solids::p3_value(v).ok_or_else(|| bad(cmd, format!("{v} is not a point")))).collect()
}

fn solid_kind(c: [Vec3; 4], face: bool, hidden: u8) -> EntityKind {
    if face {
        EntityKind::Face3d(Face3d { corners: c, hidden_edges: hidden })
    } else {
        EntityKind::Solid(Solid { corners: c.map(|p| Vec3::new(p.x, p.y, 0.0)) })
    }
}

/// Hidden-edge flags of a face on `idx`: an edge is invisible when its first point was marked;
/// a triangle's closing edge starts at its third point.
fn hidden_bits(idx: [usize; 4], invisible: &[bool]) -> u8 {
    let inv = |i: usize| invisible.get(i).copied().unwrap_or(false);
    let mut b = 0u8;
    for (j, i) in idx.iter().enumerate() {
        if inv(*i) {
            b |= 1 << j;
        }
    }
    if idx[3] == idx[2] && inv(idx[2]) {
        b |= 8;
    }
    b
}

fn add_faces(s: &mut Session, pts: &[Vec3], invisible: &[bool], face: bool) -> Result<Vec<cadcraft_doc::Handle>> {
    let mut out = Vec::new();
    for idx in faces(pts.len()) {
        let c = idx.map(|i| pts.get(i).copied().unwrap_or_default());
        out.push(s.add_entity(solid_kind(c, face, if face { hidden_bits(idx, invisible) } else { 0 }))?);
    }
    Ok(out)
}

fn run_solid(s: &mut Session, p: &Value) -> Result<Value> {
    let pts = points3("solid", p)?;
    Ok(handles(&add_faces(s, &pts, &[], false)?))
}

fn run_3dface(s: &mut Session, p: &Value) -> Result<Value> {
    let pts = points3("3dface", p)?;
    let mut invisible = vec![false; pts.len()];
    for v in p.get("invisible").and_then(Value::as_array).into_iter().flatten() {
        let i = v.as_u64().and_then(|i| usize::try_from(i).ok()).filter(|i| *i < pts.len());
        *invisible
            .get_mut(i.ok_or_else(|| bad("3dface", format!("`invisible`: {v} is not a point index")))?)
            .ok_or_else(|| bad("3dface", "bad index"))? = true;
    }
    Ok(handles(&add_faces(s, &pts, &invisible, true)?))
}

/// SOLID and 3DFACE typed: the first two points, then third and fourth points (Enter at the fourth
/// makes a triangle) until Enter at a third point. 3DFACE's `Invisible` hides the edge from the
/// next point on.
struct FaceM {
    face: bool,
    /// Points of the solid or face being made: the last two of the one before, then new ones.
    pts: Vec<Vec3>,
    invisible: Vec<bool>,
    /// `Invisible` was typed for the next point.
    hide_next: bool,
    made: usize,
}

impl FaceM {
    fn new(face: bool) -> Self {
        FaceM { face, pts: Vec::new(), invisible: Vec::new(), hide_next: false, made: 0 }
    }
    /// Make the solid or face from the four points (or three, the fourth repeating the third),
    /// keeping the last two for the next one.
    fn make(&mut self, s: &mut Session) -> Result<()> {
        if self.pts.len() == 3 {
            self.pts.push(self.pts[2]);
            self.invisible.push(false);
        }
        let c = [self.pts[0], self.pts[1], self.pts[2], self.pts[3]];
        let hidden = if self.face { hidden_bits([0, 1, 2, if c[3] == c[2] { 2 } else { 3 }], &self.invisible) } else { 0 };
        s.add_entity(solid_kind(c, self.face, hidden))?;
        self.made += 1;
        self.pts.drain(..2);
        self.invisible.drain(..2);
        Ok(())
    }
}

impl Interactive for FaceM {
    fn name(&self) -> &'static str {
        if self.face { "3DFACE" } else { "SOLID" }
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        let msg =
            ["Specify first point", "Specify second point", "Specify third point", "Specify fourth point"].get(self.pts.len()).copied().unwrap_or("");
        let mut p = Prompt::new(msg, Accept::POINT).base_opt(self.pts.last().map(|q| q.xy()));
        if self.face {
            p = p.kw(&["Invisible"]);
        }
        match self.pts.len() {
            2 if self.face || self.made > 0 => p.default("exit"),
            3 if self.face => p.default("create three-sided face"),
            3 => Prompt { message: "Specify fourth point or <exit>".into(), ..p },
            _ => p,
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Point(q) => {
                let z = if self.face { s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0) } else { 0.0 };
                self.pts.push(q.to3(z));
                self.invisible.push(std::mem::take(&mut self.hide_next));
                if self.pts.len() == 4 {
                    self.make(s)?;
                }
                Ok(Step::Continue)
            }
            Input::Keyword(k) if k == "Invisible" => {
                self.hide_next = true;
                Ok(Step::Continue)
            }
            Input::Enter => match self.pts.len() {
                3 => {
                    self.make(s)?;
                    Ok(Step::Continue)
                }
                0 | 2 => Ok(Step::Done),
                _ => Ok(Step::Continue),
            },
            Input::Cancel => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
    fn preview(&self, _s: &Session, c: Vec2) -> Vec<EntityKind> {
        let mut pts: Vec<Vec2> = self.pts.iter().map(|q| q.xy()).collect();
        if pts.is_empty() {
            return Vec::new();
        }
        pts.push(c);
        if pts.len() == 4 && !self.face {
            pts.swap(2, 3);
        }
        let mut out: Vec<EntityKind> = pts.windows(2).map(|w| line(w[0], w[1])).collect();
        if pts.len() > 2
            && let (Some(a), Some(b)) = (pts.last(), pts.first())
        {
            out.push(line(*a, *b));
        }
        out
    }
}

// ---------------- TRACE ----------------

fn trace_width(s: &Session) -> f64 {
    let metric = s.doc().is_ok_and(|d| d.header.i64("MEASUREMENT", 0) == 1);
    s.doc().map_or(0.05, |d| d.header.f64("TRACEWID", if metric { 1.0 } else { 0.05 })).max(0.0)
}

/// The left and right corners at point `i` of `pts` for a trace `w` wide: square to the segment at
/// the ends, mitred between segments (square when they nearly turn back).
fn corners_at(pts: &[Vec2], i: usize, w: f64) -> Option<(Vec2, Vec2)> {
    let p = *pts.get(i)?;
    let into = i.checked_sub(1).and_then(|j| pts.get(j)).map(|a| (p - *a).normalized());
    let out = pts.get(i + 1).map(|b| (*b - p).normalized());
    let n = |d: Vec2| d.perp() * (w / 2.0);
    let (left, right) = match (into, out) {
        (Some(a), Some(b)) => {
            let m = (a.perp() + b.perp()).normalized();
            let c = m.dot(a.perp());
            if c < 0.1 {
                return Some((p + n(a), p - n(a)));
            }
            let len = (w / 2.0) / c;
            (p + m * len, p - m * len)
        }
        (Some(d), None) | (None, Some(d)) => (p + n(d), p - n(d)),
        (None, None) => return None,
    };
    Some((left, right))
}

/// Segment `k` (from point `k` to `k + 1`) of a trace: its start corners are mitred with segment
/// `k - 1`, its end corners with `k + 1`.
fn trace_segment(pts: &[Vec2], k: usize, w: f64) -> Option<EntityKind> {
    let (sl, sr) = corners_at(pts, k, w)?;
    let (el, er) = corners_at(pts, k + 1, w)?;
    let c = [sl, sr, el, er].map(|p| p.to3(0.0));
    c.iter().all(|p| p.is_finite()).then_some(EntityKind::Trace(Solid { corners: c }))
}

fn run_trace(s: &mut Session, p: &Value) -> Result<Value> {
    let pts = distinct_points(&points_param(p, "points").ok_or_else(|| bad("trace", "`points` [[x,y], ...] (2 or more) is required"))?);
    if pts.len() < 2 || pts.len() > MAX_GEN {
        return Err(bad("trace", "need 2 or more different points"));
    }
    let w = size_param("trace", p, "width", false)?.unwrap_or_else(|| trace_width(s));
    if w <= 0.0 {
        return Err(bad("trace", "the width must be positive"));
    }
    let mut out = Vec::new();
    for k in 0..pts.len() - 1 {
        if let Some(t) = trace_segment(&pts, k, w) {
            out.push(s.add_entity(t)?);
        }
    }
    Ok(handles(&out))
}

/// TRACE typed: the width (kept in TRACEWID), the start point, then next points; each segment is
/// drawn once the next point mitres its end, the last one at Enter.
struct TraceM {
    width: f64,
    asked: bool,
    pts: Vec<Vec2>,
    /// Segments drawn so far.
    made: usize,
}

impl Interactive for TraceM {
    fn name(&self) -> &'static str {
        "TRACE"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if !self.asked {
            return Prompt::new("Specify trace width", Accept::NUMBER).default(format!("{:.4}", self.width));
        }
        match self.pts.last() {
            None => Prompt::new("Specify start point", Accept::POINT),
            Some(b) => Prompt::new("Specify next point", Accept::POINT).base(*b),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if !self.asked {
            match i {
                Input::Text(t) => {
                    self.width = number(&t).filter(|w| *w > 0.0).ok_or_else(|| other("Requires a positive value."))?;
                    s.doc_mut()?.header.set_f64("TRACEWID", self.width);
                }
                Input::Enter => {}
                Input::Cancel => return Ok(Step::Cancel),
                _ => return Ok(Step::Continue),
            }
            if self.width <= 0.0 {
                return Err(other("Requires a positive value."));
            }
            self.asked = true;
            return Ok(Step::Continue);
        }
        match i {
            Input::Point(q) => {
                if self.pts.last().is_some_and(|l| l.near(q, 1e-12)) || self.pts.len() >= MAX_GEN {
                    return Ok(Step::Continue);
                }
                self.pts.push(q);
                // The segment before the last is now mitred at both ends.
                if self.pts.len() >= 3
                    && let Some(t) = trace_segment(&self.pts, self.pts.len() - 3, self.width)
                {
                    s.add_entity(t)?;
                    self.made += 1;
                }
                Ok(Step::Continue)
            }
            Input::Enter => {
                if self.pts.len() >= 2
                    && let Some(t) = trace_segment(&self.pts, self.pts.len() - 2, self.width)
                {
                    s.add_entity(t)?;
                }
                Ok(Step::Done)
            }
            Input::Cancel => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
    fn preview(&self, _s: &Session, c: Vec2) -> Vec<EntityKind> {
        if !self.asked || self.pts.is_empty() || self.pts.last().is_some_and(|l| l.near(c, 1e-12)) {
            return Vec::new();
        }
        let mut pts = self.pts.clone();
        pts.push(c);
        let from = pts.len().saturating_sub(3);
        (from..pts.len() - 1).filter_map(|k| trace_segment(&pts, k, self.width)).collect()
    }
}

// ---------------- SKETCH ----------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SketchType {
    Line,
    Polyline,
    Spline,
}

impl SketchType {
    fn from_skpoly(v: i64) -> Self {
        match v {
            1 => SketchType::Polyline,
            2 => SketchType::Spline,
            _ => SketchType::Line,
        }
    }
    fn parse(t: &str) -> Option<Self> {
        Some(match t.trim().to_ascii_lowercase().as_str() {
            "line" | "l" | "lines" => SketchType::Line,
            "polyline" | "p" => SketchType::Polyline,
            "spline" | "s" => SketchType::Spline,
            _ => return None,
        })
    }
    fn name(self) -> &'static str {
        match self {
            SketchType::Line => "Line",
            SketchType::Polyline => "Polyline",
            SketchType::Spline => "Spline",
        }
    }
    fn skpoly(self) -> i64 {
        match self {
            SketchType::Line => 0,
            SketchType::Polyline => 1,
            SketchType::Spline => 2,
        }
    }
}

/// SKPOLY, SKETCHINC and SKTOLERANCE (AutoCAD's defaults).
fn sketch_settings(s: &Session) -> (SketchType, f64, f64) {
    let Ok(d) = s.doc() else { return (SketchType::Line, 0.1, 0.5) };
    let metric = d.header.i64("MEASUREMENT", 0) == 1;
    (
        SketchType::from_skpoly(d.header.i64("SKPOLY", 0)),
        d.header.f64("SKETCHINC", if metric { 1.0 } else { 0.1 }).max(0.0),
        d.header.f64("SKTOLERANCE", 0.5).max(0.0),
    )
}

/// The points of a stroke that are at least `inc` from the one recorded before (the record
/// increment), and always its last point.
fn recorded(pts: &[Vec2], inc: f64) -> Vec<Vec2> {
    let mut out: Vec<Vec2> = Vec::new();
    for p in pts.iter().filter(|p| p.is_finite()) {
        if out.last().is_none_or(|l| l.dist(*p) >= inc.max(1e-12)) {
            out.push(*p);
        }
    }
    if let (Some(l), Some(p)) = (out.last().copied(), pts.last().filter(|p| p.is_finite()))
        && !l.near(*p, 1e-12)
        && out.len() > 1
    {
        // The end of the stroke counts even when closer than the increment.
        if let Some(last) = out.last_mut() {
            *last = *p;
        }
    }
    out
}

fn sketch_stroke(s: &mut Session, pts: &[Vec2], t: SketchType, inc: f64, tol: f64) -> Result<Vec<cadcraft_doc::Handle>> {
    let pts = recorded(pts, inc);
    if pts.len() < 2 {
        return Ok(Vec::new());
    }
    Ok(match t {
        SketchType::Line => {
            let mut hs = Vec::new();
            for w in pts.windows(2) {
                hs.push(s.add_entity(line(w[0], w[1]))?);
            }
            hs
        }
        SketchType::Polyline => vec![s.add_entity(lwpoly(pts.into_iter().map(PolyVertex::new).collect(), false))?],
        SketchType::Spline => {
            vec![s.add_entity(EntityKind::Spline(Spline::fit_with(&pts, false, FitOptions { tolerance: tol, ..FitOptions::default() })))?]
        }
    })
}

fn run_sketch(s: &mut Session, p: &Value) -> Result<Value> {
    let (t0, inc0, tol0) = sketch_settings(s);
    let t = match str_param(p, "type") {
        Some(v) => SketchType::parse(v).ok_or_else(|| bad("sketch", "`type` must be line, polyline or spline"))?,
        None => t0,
    };
    let inc = size_param("sketch", p, "increment", true)?.unwrap_or(inc0);
    let tol = size_param("sketch", p, "tolerance", true)?.unwrap_or(tol0);
    let strokes: Vec<Vec<Vec2>> = if let Some(a) = p.get("strokes").and_then(Value::as_array) {
        a.iter()
            .map(|v| points_param(&json!({ "p": v }), "p").ok_or_else(|| bad("sketch", "`strokes` must be lists of points")))
            .collect::<Result<_>>()?
    } else {
        vec![points_param(p, "points").ok_or_else(|| bad("sketch", "`points` [[x,y], ...] or `strokes` is required"))?]
    };
    if strokes.iter().map(Vec::len).sum::<usize>() > MAX_GEN {
        return Err(bad("sketch", format!("more than {MAX_GEN} points")));
    }
    let mut out = Vec::new();
    for st in &strokes {
        out.extend(sketch_stroke(s, st, t, inc, tol)?);
    }
    if out.is_empty() {
        return Err(bad("sketch", "nothing recorded: give 2 or more points farther apart than the increment"));
    }
    Ok(handles(&out))
}

enum SketchAsk {
    Sketch,
    Type,
    Increment,
    Tolerance,
}

/// SKETCH typed: Type, Increment and toLerance, then the sketch, one point per pick, until Enter.
// ponytail: the pen follows picks; AutoCAD records the cursor as it moves (the UI sends no
// motion to commands). A stroke per command; add pen up/down when motion is fed.
struct SketchM {
    ask: SketchAsk,
    kind: SketchType,
    inc: f64,
    tol: f64,
    pts: Vec<Vec2>,
}

impl SketchM {
    fn new(s: &Session) -> Self {
        let (kind, inc, tol) = sketch_settings(s);
        SketchM { ask: SketchAsk::Sketch, kind, inc, tol, pts: Vec::new() }
    }
}

impl Interactive for SketchM {
    fn name(&self) -> &'static str {
        "SKETCH"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        s.echo(format!("Type = {}  Increment = {:.4}  Tolerance = {:.4}", self.kind.name(), self.inc, self.tol));
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.ask {
            SketchAsk::Type => Prompt::new("Enter sketch type", KW).kw(&["Line", "Polyline", "Spline"]).default(self.kind.name()),
            SketchAsk::Increment => Prompt::new("Specify sketch record increment", Accept::NUMBER).default(format!("{:.4}", self.inc)),
            SketchAsk::Tolerance => Prompt::new("Specify spline fit tolerance", Accept::NUMBER).default(format!("{:.4}", self.tol)),
            SketchAsk::Sketch if self.pts.is_empty() => Prompt::new("Specify sketch", Accept::POINT).kw(&["Type", "Increment", "toLerance"]),
            SketchAsk::Sketch => Prompt::new("Specify sketch", Accept::POINT).base_opt(self.pts.last().copied()),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        match (&self.ask, i) {
            (SketchAsk::Type, Input::Keyword(k) | Input::Text(k)) => {
                self.kind = SketchType::parse(&k).ok_or_else(|| other("Invalid option keyword."))?;
                s.doc_mut()?.header.set_i64("SKPOLY", self.kind.skpoly());
                self.ask = SketchAsk::Sketch;
            }
            (SketchAsk::Increment, Input::Text(t)) => {
                self.inc = number(&t).filter(|v| *v >= 0.0).ok_or_else(|| other("Requires a non-negative value."))?;
                s.doc_mut()?.header.set_f64("SKETCHINC", self.inc);
                self.ask = SketchAsk::Sketch;
            }
            (SketchAsk::Tolerance, Input::Text(t)) => {
                self.tol = number(&t).filter(|v| *v >= 0.0).ok_or_else(|| other("Requires a non-negative value."))?;
                s.doc_mut()?.header.set_f64("SKTOLERANCE", self.tol);
                self.ask = SketchAsk::Sketch;
            }
            (SketchAsk::Type | SketchAsk::Increment | SketchAsk::Tolerance, Input::Enter) => self.ask = SketchAsk::Sketch,
            (SketchAsk::Sketch, Input::Keyword(k)) => {
                self.ask = match k.as_str() {
                    "Type" => SketchAsk::Type,
                    "Increment" => SketchAsk::Increment,
                    _ => SketchAsk::Tolerance,
                }
            }
            (SketchAsk::Sketch, Input::Point(p)) if self.pts.len() < MAX_GEN => self.pts.push(p),
            (SketchAsk::Sketch, Input::Enter) => {
                let n = sketch_stroke(s, &self.pts, self.kind, self.inc, self.tol)?.len();
                if n > 0 {
                    s.echo(format!("Sketch done: {n} object{} added.", if n == 1 { "" } else { "s" }));
                }
                return Ok(Step::Done);
            }
            _ => {}
        }
        Ok(Step::Continue)
    }
    fn preview(&self, _s: &Session, c: Vec2) -> Vec<EntityKind> {
        if !matches!(self.ask, SketchAsk::Sketch) || self.pts.is_empty() || self.pts.len() > 2000 {
            return Vec::new();
        }
        let mut pts = self.pts.clone();
        pts.push(c);
        vec![lwpoly(pts.into_iter().map(PolyVertex::new).collect(), false)]
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::EntityKind;
    use cadcraft_geom::{Vec2, Vec3};
    use serde_json::json;

    use crate::{Input, Session};

    fn kinds(s: &Session) -> Vec<EntityKind> {
        s.doc().unwrap().model.iter().map(|e| e.kind.clone()).collect()
    }

    #[test]
    fn solid_json_and_typed_make_filled_solids() {
        let mut s = Session::new();
        let r = s.execute("solid", &json!({"points": [[0, 0], [10, 0], [0, 5], [10, 5], [0, 10]]})).unwrap();
        assert_eq!(r["handles"].as_array().unwrap().len(), 2);
        match &kinds(&s)[..] {
            [EntityKind::Solid(a), EntityKind::Solid(b)] => {
                assert_eq!(a.corners[3], Vec3::new(10.0, 5.0, 0.0));
                // The second continues from the first's third and fourth corners; a lone last point is a triangle.
                assert_eq!(b.corners[0], Vec3::new(0.0, 5.0, 0.0));
                assert_eq!(b.corners[2], b.corners[3]);
            }
            other => panic!("{other:?}"),
        }
        assert!(s.execute("solid", &json!({"points": [[0, 0], [1, 1]]})).is_err());

        // Typed (SO): 4 points, then Enter at the fourth of the next makes a triangle, Enter ends.
        let mut s = Session::new();
        s.cmdline("so").unwrap();
        assert!(s.prompt_text().ends_with("Specify first point:"), "{}", s.prompt_text());
        s.cmdline("0,0 10,0 0,5").unwrap();
        assert!(s.prompt_text().ends_with("Specify fourth point or <exit>:"), "{}", s.prompt_text());
        s.cmdline("10,5").unwrap();
        assert!(s.prompt_text().ends_with("Specify third point <exit>:"), "{}", s.prompt_text());
        s.cmdline("5,9").unwrap();
        s.cmdline("").unwrap();
        s.cmdline("").unwrap();
        assert!(s.running.is_none(), "{}", s.prompt_text());
        assert_eq!(kinds(&s).len(), 2);
    }

    #[test]
    fn three_d_face_keeps_heights_and_invisible_edges() {
        let mut s = Session::new();
        s.execute("3dface", &json!({"points": [[0, 0, 0], [10, 0, 0], [10, 10, 5], [0, 10, 5]], "invisible": [1]})).unwrap();
        match &kinds(&s)[..] {
            [EntityKind::Face3d(f)] => {
                assert_eq!(f.corners[2].z, 5.0);
                assert_eq!(f.hidden_edges, 2, "the second edge (from point 1)");
                assert_eq!(EntityKind::Face3d(f.clone()).prims().len(), 3, "an invisible edge isn't drawn");
            }
            other => panic!("{other:?}"),
        }
        assert!(s.execute("3dface", &json!({"points": [[0, 0], [1, 0], [1, 1]], "invisible": [7]})).is_err());

        // A face with every edge invisible keeps its extents and is windowed like any other.
        let mut s = Session::new();
        s.execute("3dface", &json!({"points": [[0, 0], [10, 0], [10, 10], [0, 10]], "invisible": [0, 1, 2, 3]})).unwrap();
        s.execute("3dface", &json!({"points": [[20, 0], [30, 0], [30, 10], [20, 10]]})).unwrap();
        let d = s.doc().unwrap();
        let b = cadcraft_doc::entity_bounds(d, d.model.iter().next().unwrap(), 0);
        assert_eq!((b.min, b.max), (Vec2::ZERO, Vec2::new(10.0, 10.0)));
        let hs = crate::select::select_window(
            s.doc().unwrap(),
            &cadcraft_doc::Space::Model,
            cadcraft_geom::Bounds2::new(Vec2::new(-1.0, -1.0), Vec2::new(40.0, 20.0)),
            false,
        );
        assert_eq!(hs.len(), 2);

        // Typed as 3F: I before a point hides the edge from it; Enter at the fourth makes a triangle.
        let mut s = Session::new();
        s.cmdline("3f").unwrap();
        assert!(s.prompt_text().ends_with("Specify first point or [Invisible]:"), "{}", s.prompt_text());
        s.cmdline("0,0,0 i 10,0,0 10,10,3").unwrap();
        assert!(s.prompt_text().ends_with("Specify fourth point or [Invisible] <create three-sided face>:"), "{}", s.prompt_text());
        s.cmdline("").unwrap();
        assert!(s.prompt_text().ends_with("Specify third point or [Invisible] <exit>:"), "{}", s.prompt_text());
        s.cmdline("").unwrap();
        assert!(s.running.is_none());
        match &kinds(&s)[..] {
            [EntityKind::Face3d(f)] => {
                assert_eq!(f.hidden_edges, 2);
                assert_eq!(f.corners[2], Vec3::new(10.0, 10.0, 3.0));
                assert_eq!(f.corners[3], f.corners[2]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn trace_segments_are_mitred() {
        let mut s = Session::new();
        let r = s.execute("trace", &json!({"points": [[0, 0], [10, 0], [10, 10]], "width": 2})).unwrap();
        assert_eq!(r["handles"].as_array().unwrap().len(), 2);
        let ks = kinds(&s);
        let (EntityKind::Trace(a), EntityKind::Trace(b)) = (&ks[0], &ks[1]) else { panic!("{ks:?}") };
        // Square start, mitred corner at (10,0): outer (11,-1), inner (9,1).
        assert_eq!(a.corners[0].xy(), Vec2::new(0.0, 1.0));
        assert!(a.corners[2].xy().near(Vec2::new(9.0, 1.0), 1e-9), "{:?}", a.corners);
        assert!(a.corners[3].xy().near(Vec2::new(11.0, -1.0), 1e-9), "{:?}", a.corners);
        assert!(b.corners[0].xy().near(Vec2::new(9.0, 1.0), 1e-9), "{:?}", b.corners);
        assert!(s.execute("trace", &json!({"points": [[0, 0], [1, 0]], "width": -1})).is_err());

        // Typed: width (remembered in TRACEWID), points, Enter draws the last segment.
        let mut s = Session::new();
        s.cmdline("trace").unwrap();
        assert!(s.prompt_text().ends_with("Specify trace width <0.0500>:"), "{}", s.prompt_text());
        s.cmdline("0.5 0,0 10,0 10,10").unwrap();
        assert_eq!(kinds(&s).len(), 1, "the first segment once the second mitres it");
        s.cmdline("").unwrap();
        assert!(s.running.is_none());
        assert_eq!(kinds(&s).len(), 2);
        assert_eq!(s.doc().unwrap().header.f64("TRACEWID", 0.0), 0.5);
    }

    #[test]
    fn sketch_records_points_as_lines_polylines_or_splines() {
        let mut s = Session::new();
        // Points closer than the increment are dropped.
        let r = s.execute("sketch", &json!({"points": [[0, 0], [0.01, 0], [1, 0], [2, 1]], "increment": 0.1})).unwrap();
        assert_eq!(r["handles"].as_array().unwrap().len(), 2, "two lines");
        s.execute("sketch", &json!({"strokes": [[[0, 5], [1, 5], [2, 6]], [[5, 5], [6, 6]]], "type": "polyline"})).unwrap();
        s.execute("sketch", &json!({"points": [[0, 9], [1, 10], [2, 9], [3, 10]], "type": "spline", "tolerance": 0})).unwrap();
        let names: Vec<&str> = s.doc().unwrap().model.iter().map(|e| e.kind.type_name()).collect();
        assert_eq!(names, ["Line", "Line", "Polyline", "Polyline", "Spline"]);
        assert!(s.execute("sketch", &json!({"points": [[0, 0], [0.01, 0]], "increment": 1})).is_err());
        assert!(s.execute("sketch", &json!({"points": [[0, 0], [1, 0]], "type": "arc"})).is_err());

        // Typed: Type remembered in SKPOLY, then picks until Enter.
        let mut s = Session::new();
        s.cmdline("sketch").unwrap();
        assert!(s.log.iter().any(|l| l.starts_with("Type = Line")), "{:?}", s.log);
        assert!(s.prompt_text().ends_with("Specify sketch or [Type/Increment/toLerance]:"), "{}", s.prompt_text());
        s.cmdline("t p").unwrap();
        assert_eq!(s.doc().unwrap().header.i64("SKPOLY", 0), 1);
        for p in [(0.0, 0.0), (1.0, 0.0), (2.0, 1.0)] {
            s.input(Input::Point(Vec2::new(p.0, p.1))).unwrap();
        }
        s.input(Input::Enter).unwrap();
        assert!(s.running.is_none());
        assert_eq!(s.doc().unwrap().model.iter().map(|e| e.kind.type_name()).collect::<Vec<_>>(), ["Polyline"]);
    }
}
