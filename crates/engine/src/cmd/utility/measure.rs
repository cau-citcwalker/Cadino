//! MEASUREGEOM at the command line: "Enter an option [Distance/Radius/Angle/ARea/Volume]
//! <Distance>:", then that measurement's prompts (DIST's and AREA's own for Distance and
//! ARea), then the options again with eXit until Enter. Tools ▸ Inquiry ▸ Radius and Angle start
//! it at their measurement.

use cadcraft_doc::{EntityKind, Handle};
use cadcraft_geom::Vec2;
use serde_json::{Value, json};

use super::super::find_command;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

const KEYWORDS: Accept = Accept { point: false, number: false, text: false, select: false, enter: true };

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::cmd) enum Mode {
    Distance,
    Radius,
    Angle,
    Area,
    Volume,
}

pub(in crate::cmd) struct MeasureM {
    first: Option<Mode>,
    inner: Option<Box<dyn Interactive>>,
    /// A measurement was made: the options offer eXit, the default.
    measured: bool,
}

impl MeasureM {
    pub(in crate::cmd) fn new(first: Option<Mode>) -> Self {
        MeasureM { first, inner: None, measured: false }
    }

    fn enter(&mut self, s: &mut Session, m: Mode) -> Result<Step> {
        let factory = |id: &str| find_command(id).and_then(|c| c.interactive).ok_or_else(|| EngineError::UnknownCommand(id.into()));
        let mut inner: Box<dyn Interactive> = match m {
            Mode::Distance => factory("dist")?(s)?,
            Mode::Area => factory("area")?(s)?,
            Mode::Radius => Box::new(PickOneM { msg: "Select arc or circle", measure: super::run_radius }),
            Mode::Volume => Box::new(PickOneM { msg: "Select a 3D solid", measure: super::run_volume }),
            Mode::Angle => Box::<AngleM>::default(),
        };
        let step = inner.begin(s)?;
        self.inner = Some(inner);
        Ok(self.after(step))
    }

    /// Back to the options when the measurement is done; Escape ends MEASUREGEOM.
    fn after(&mut self, step: Step) -> Step {
        match step {
            Step::Continue => Step::Continue,
            Step::Done => {
                self.inner = None;
                self.measured = true;
                Step::Continue
            }
            Step::Cancel => Step::Cancel,
        }
    }
}

impl Interactive for MeasureM {
    fn name(&self) -> &'static str {
        "MEASUREGEOM"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        match self.first {
            Some(m) => self.enter(s, m),
            None => Ok(Step::Continue),
        }
    }
    fn prompt(&self, s: &Session) -> Prompt {
        if let Some(i) = &self.inner {
            return i.prompt(s);
        }
        if self.measured {
            Prompt::new("Enter an option", KEYWORDS).kw(&["Distance", "Radius", "Angle", "ARea", "Volume", "eXit"]).default("eXit")
        } else {
            Prompt::new("Enter an option", KEYWORDS).kw(&["Distance", "Radius", "Angle", "ARea", "Volume"]).default("Distance")
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if let Some(inner) = &mut self.inner {
            let step = inner.input(s, i)?;
            return Ok(self.after(step));
        }
        let m = match i {
            Input::Keyword(k) => match k.as_str() {
                "Distance" => Mode::Distance,
                "Radius" => Mode::Radius,
                "Angle" => Mode::Angle,
                "ARea" => Mode::Area,
                "Volume" => Mode::Volume,
                _ => return Ok(Step::Done),
            },
            Input::Enter if self.measured => return Ok(Step::Done),
            Input::Enter => Mode::Distance,
            Input::Cancel => return Ok(Step::Cancel),
            _ => return Ok(Step::Continue),
        };
        self.enter(s, m)
    }
    fn preview(&self, s: &Session, c: Vec2) -> Vec<EntityKind> {
        self.inner.as_ref().map(|i| i.preview(s, c)).unwrap_or_default()
    }
}

fn say(s: &mut Session, r: &Value) {
    if let Some(m) = r.get("message").and_then(Value::as_str) {
        for l in m.lines() {
            s.echo(l.to_string());
        }
    }
}

/// Pick one object and report a measurement of it (Radius, Volume). A pick it can't measure is
/// reported and the prompt stays.
struct PickOneM {
    msg: &'static str,
    measure: fn(&mut Session, &Value) -> Result<Value>,
}

impl Interactive for PickOneM {
    fn name(&self) -> &'static str {
        "MEASUREGEOM"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new(self.msg, Accept::SELECT)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Pick(hs) => {
                let Some(h) = hs.first() else { return Ok(Step::Continue) };
                let r = (self.measure)(s, &json!({ "handle": h.hex() }))?;
                say(s, &r);
                Ok(Step::Done)
            }
            Input::Enter | Input::Cancel => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
}

/// The Angle measurement: an arc's angle, the angle at a circle's centre between the picked
/// point and another, the angle between two lines, or from a vertex and two points.
#[derive(Default)]
struct AngleM {
    /// The first line picked.
    line: Option<Handle>,
    /// Vertex and endpoints so far.
    pts: Vec<Vec2>,
    /// Picking points (Enter at the object prompt, or a circle picked).
    points: bool,
}

impl AngleM {
    fn finish(&self, s: &mut Session, p: &Value) -> Result<Step> {
        let r = super::run_angle(s, p)?;
        say(s, &r);
        Ok(Step::Done)
    }
}

impl Interactive for AngleM {
    fn name(&self) -> &'static str {
        "MEASUREGEOM"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.points {
            let base = self.pts.first().copied();
            return match self.pts.len() {
                0 => Prompt::new("Specify angle vertex", Accept::POINT),
                1 => Prompt::new("Specify first angle endpoint", Accept::POINT).base_opt(base),
                _ => Prompt::new("Specify second angle endpoint", Accept::POINT).base_opt(base),
            };
        }
        match self.line {
            None => Prompt::new("Select arc, circle, line, or <specify vertex>", Accept::SELECT),
            Some(_) => Prompt::new("Select second line", Accept::SELECT),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if self.points {
            return match i {
                Input::Point(p) => {
                    self.pts.push(p);
                    match self.pts[..] {
                        [v, a, b] => self.finish(s, &json!({ "vertex": [v.x, v.y], "p1": [a.x, a.y], "p2": [b.x, b.y] })),
                        _ => Ok(Step::Continue),
                    }
                }
                Input::Enter | Input::Cancel => Ok(Step::Cancel),
                _ => Ok(Step::Continue),
            };
        }
        let hs = match i {
            Input::Pick(hs) => hs,
            Input::Enter if self.line.is_none() => {
                self.points = true;
                return Ok(Step::Continue);
            }
            Input::Enter | Input::Cancel => return Ok(Step::Cancel),
            _ => return Ok(Step::Continue),
        };
        let Some(h) = hs.first().copied() else { return Ok(Step::Continue) };
        let kind = s.doc()?.entity(h).map(|e| e.kind.clone());
        match (self.line, kind) {
            (Some(h1), Some(EntityKind::Line(_))) => self.finish(s, &json!({ "h1": h1.hex(), "h2": h.hex() })),
            (None, Some(EntityKind::Line(_))) => {
                self.line = Some(h);
                Ok(Step::Continue)
            }
            (None, Some(EntityKind::Arc(a))) => {
                let ang = cadcraft_geom::ccw_sweep(a.start, a.end).to_degrees();
                s.echo(format!("Angle = {ang:.4}°"));
                Ok(Step::Done)
            }
            (None, Some(EntityKind::Circle(c))) => {
                // The angle at the centre from the point the circle was picked at.
                self.pts = vec![c.center.xy(), s.cursor];
                self.points = true;
                Ok(Step::Continue)
            }
            (Some(_), _) => {
                s.echo("Select a line.");
                Ok(Step::Continue)
            }
            (None, _) => {
                s.echo("Select an arc, circle or line.");
                Ok(Step::Continue)
            }
        }
    }
    fn preview(&self, _s: &Session, c: Vec2) -> Vec<EntityKind> {
        match self.pts[..] {
            [v] => vec![super::super::helpers::line(v, c)],
            [v, a] => vec![super::super::helpers::line(v, a), super::super::helpers::line(v, c)],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::Handle;
    use cadcraft_geom::Vec2;
    use serde_json::json;

    use crate::{Input, Session};

    fn add(s: &mut Session, id: &str, p: serde_json::Value) -> Handle {
        s.execute(id, &p).unwrap();
        s.doc().unwrap().model.last().unwrap().handle
    }

    fn said(s: &Session, from: usize, what: &str) -> bool {
        s.log[from..].iter().any(|l| l.contains(what))
    }

    /// MEASUREGEOM typed bare asks for the measurement (it refused "`p1` is required"), loops
    /// back to the options after each one, and Enter ends it.
    #[test]
    fn measuregeom_prompts_and_loops() {
        let mut s = Session::new();
        let circle = add(&mut s, "circle", json!({ "center": [0, 0], "radius": 5 }));
        let arc = add(&mut s, "arc", json!({ "center": [20, 0], "radius": 2, "start": 0, "end": 270 }));
        let l1 = add(&mut s, "line", json!({ "points": [[0, 20], [10, 20]] }));
        let l2 = add(&mut s, "line", json!({ "points": [[0, 20], [0, 30]] }));

        s.cmdline("measuregeom").unwrap();
        assert_eq!(s.prompt_text(), "MEASUREGEOM Enter an option [Distance/Radius/Angle/ARea/Volume] <Distance>:");
        s.cmdline("").unwrap();
        assert!(s.prompt_text().contains("Specify first point"), "{}", s.prompt_text());
        let n = s.log.len();
        s.cmdline("0,0").unwrap();
        s.cmdline("3,4").unwrap();
        assert!(said(&s, n, "Distance = 5"), "{:?}", &s.log[n..]);
        assert_eq!(s.prompt_text(), "MEASUREGEOM Enter an option [Distance/Radius/Angle/ARea/Volume/eXit] <eXit>:");

        s.cmdline("r").unwrap();
        assert_eq!(s.prompt_text(), "MEASUREGEOM Select arc or circle:");
        s.input(Input::Pick(vec![l1])).unwrap();
        assert!(s.running.is_some() && s.prompt_text().contains("Select arc or circle"), "a line is refused, the prompt stays");
        s.input(Input::Pick(vec![circle])).unwrap();
        assert!(said(&s, n, "Radius = 5.0000"));

        s.cmdline("a").unwrap();
        assert!(s.prompt_text().ends_with("Select arc, circle, line, or <specify vertex>:"), "{}", s.prompt_text());
        s.input(Input::Pick(vec![arc])).unwrap();
        assert!(said(&s, n, "Angle = 270.0000°"));
        s.cmdline("a").unwrap();
        s.input(Input::Pick(vec![l1])).unwrap();
        assert!(s.prompt_text().ends_with("Select second line:"));
        s.input(Input::Pick(vec![l2])).unwrap();
        assert!(said(&s, n, "Angle = 90.0000°"));
        s.cmdline("a").unwrap();
        s.cursor = Vec2::new(5.0, 0.0);
        s.input(Input::Pick(vec![circle])).unwrap();
        assert!(s.prompt_text().ends_with("Specify second angle endpoint:"), "{}", s.prompt_text());
        s.input(Input::Point(Vec2::new(-5.0, 0.0))).unwrap();
        assert!(said(&s, n, "Angle = 180.0000°"));
        s.cmdline("a").unwrap();
        s.cmdline("").unwrap();
        s.cmdline("0,0 1,0 1,1").unwrap();
        assert!(said(&s, n, "Angle = 45.0000°"));

        s.cmdline("v").unwrap();
        assert!(s.prompt_text().ends_with("Select a 3D solid:"));
        s.input(Input::Pick(vec![circle])).unwrap();
        assert!(s.running.is_some(), "a circle has no volume");
        s.cancel();
        assert!(s.running.is_none());

        // Tools > Inquiry > Radius and Angle start at their measurement; Enter after it ends.
        s.cmdline("radius.measure").unwrap();
        assert_eq!(s.prompt_text(), "MEASUREGEOM Select arc or circle:");
        s.input(Input::Pick(vec![arc])).unwrap();
        assert!(s.prompt_text().ends_with("<eXit>:"));
        s.cmdline("").unwrap();
        assert!(s.running.is_none());
        s.start("angle.measure").unwrap();
        assert!(s.prompt_text().ends_with("Select arc, circle, line, or <specify vertex>:"));
        s.cancel();

        // The JSON forms are unchanged.
        assert!(s.execute("measuregeom", &json!({})).is_err());
        let r = s.execute("measuregeom", &json!({ "mode": "radius", "handle": circle.hex() })).unwrap();
        assert_eq!(r["radius"], 5.0);
        assert!(s.running.is_none());
    }
}
