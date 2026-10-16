//! CPLANE (Rhino) and UCS (AutoCAD): the current tile's construction plane ([`Session::cplane`]).
//!
//! JSON: `{preset}` (World/Top, Front, Right, Back, Left, Bottom: Rhino's and AutoCAD's
//! orthographic planes, at the world origin), or `{origin?, xaxis?, ypoint?}` in world
//! coordinates (what is left out stays as the current plane has it), `{}` reports the current
//! plane. At the prompt: "Specify origin of UCS or [World/Top/Front/Right/Back/Left/Bottom/3point]
//! <World>", then a point on the X axis and one on the XY plane (Enter keeps the current axes);
//! points given there are in the current plane's coordinates, as every point is. Setting a plane
//! is an undo step.

use cadcraft_geom::{Mat4, Vec3};
use serde_json::{Value, json};

use super::super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

/// The preset keywords, as the prompt shows them (capitals: the shortcut).
const PRESETS: [&str; 7] = ["World", "Top", "Front", "Right", "BAck", "Left", "BOttom"];

pub fn specs() -> Vec<CommandSpec> {
    const PARAMS: &str = "{preset: world|top|front|right|back|left|bottom} | {origin?: [x,y,z], xaxis?: [x,y,z] (direction), ypoint?: [x,y,z] (a point on the plane's positive y side)} in world coordinates; {} reports the current plane. Acts on the current tile (with one view, the drawing's UCS)";
    vec![
        CommandSpec::new("cplane", "Construction Plane", run_cplane)
            .menu(&["Tools", "Construction Plane"])
            .params(PARAMS)
            .interactive(|_| Ok(Box::new(CplaneM { name: "CPLANE", ..Default::default() }))),
        CommandSpec::new("ucs", "UCS", |s, p| plane(s, p, "ucs"))
            .params(PARAMS)
            .interactive(|_| Ok(Box::new(CplaneM { name: "UCS", ..Default::default() }))),
    ]
}

/// A preset plane by name (any case): World and Top are the identity; the others stand on the
/// world origin as AutoCAD's orthographic UCSs do (Front: x = X, y = Z, normal -Y).
pub(crate) fn preset(name: &str) -> Option<Mat4> {
    let o = Vec3::ZERO;
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "world" | "w" | "top" | "t" => Mat4::IDENTITY,
        "front" | "f" => Mat4::from_cols(Vec3::X, Vec3::Z, -Vec3::Y, o),
        "back" | "ba" => Mat4::from_cols(-Vec3::X, Vec3::Z, Vec3::Y, o),
        "right" | "r" => Mat4::from_cols(Vec3::Y, Vec3::Z, Vec3::X, o),
        "left" | "l" => Mat4::from_cols(-Vec3::Y, Vec3::Z, -Vec3::X, o),
        "bottom" | "bo" => Mat4::from_cols(Vec3::X, -Vec3::Y, -Vec3::Z, o),
        _ => return None,
    })
}

/// A point or direction `[x, y]` / `[x, y, z]`: `None` when absent, an error when unusable.
fn point3(cmd: &str, p: &Value, key: &str) -> Result<Option<Vec3>> {
    let v = match p.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(v) => v,
    };
    let n: Option<Vec<f64>> = v.as_array().filter(|a| (2..=3).contains(&a.len())).and_then(|a| a.iter().map(Value::as_f64).collect());
    match n.as_deref() {
        Some(&[x, y]) if x.is_finite() && y.is_finite() => Ok(Some(Vec3::new(x, y, 0.0))),
        Some(&[x, y, z]) if Vec3::new(x, y, z).is_finite() => Ok(Some(Vec3::new(x, y, z))),
        _ => Err(bad(cmd, format!("`{key}` must be [x, y, z] (finite numbers)"))),
    }
}

/// The frame at `o` with x axis `x` and y toward `y`; without `y`, the one nearest the current
/// plane (an x axis in it turns the plane about its normal).
fn frame_from(cur: &Mat4, o: Vec3, x: Vec3, y: Option<Vec3>) -> Option<Mat4> {
    y.into_iter().chain([cur.col(2).cross(x), cur.col(1), cur.col(0)]).find_map(|y| Mat4::frame(o, x, y))
}

fn describe(c: &Mat4) -> Value {
    let v = |a: Vec3| json!([a.x, a.y, a.z]);
    let world = *c == Mat4::IDENTITY;
    let named = ["Front", "Right", "Back", "Left", "Bottom"].into_iter().find(|n| preset(n) == Some(*c));
    let name = named.unwrap_or(if world { "World" } else { "Custom" });
    json!({ "origin": v(c.col(3)), "xaxis": v(c.col(0)), "yaxis": v(c.col(1)), "normal": v(c.col(2)), "world": world, "name": name })
}

fn run_cplane(s: &mut Session, p: &Value) -> Result<Value> {
    plane(s, p, "cplane")
}

/// CPLANE and UCS: errors name the command given.
fn plane(s: &mut Session, p: &Value, cmd: &str) -> Result<Value> {
    if !(p.is_object() || p.is_null()) {
        return Err(bad(cmd, "parameters must be an object"));
    }
    let cur = s.cplane();
    let new = match p.get("preset") {
        Some(Value::String(t)) => {
            preset(t).ok_or_else(|| bad(cmd, format!("unknown preset `{t}`: world, top, front, right, back, left or bottom")))?
        }
        None | Some(Value::Null) => {
            let (origin, xaxis, ypoint) = (point3(cmd, p, "origin")?, point3(cmd, p, "xaxis")?, point3(cmd, p, "ypoint")?);
            if origin.is_none() && xaxis.is_none() && ypoint.is_none() {
                return Ok(describe(&cur));
            }
            let o = origin.unwrap_or(cur.col(3));
            let x = xaxis.unwrap_or(cur.col(0));
            if x.normalized() == Vec3::ZERO {
                return Err(bad(cmd, "`xaxis` must not be zero"));
            }
            let f = match ypoint {
                Some(q) => Mat4::frame(o, x, q - o),
                None => frame_from(&cur, o, x, None),
            };
            f.ok_or_else(|| bad(cmd, "`ypoint` must be off the x axis through the origin"))?
        }
        Some(_) => return Err(bad(cmd, "`preset` must be a name")),
    };
    s.set_cplane(new)?;
    Ok(describe(&s.cplane()))
}

#[derive(Clone, Copy, Default, PartialEq)]
enum Stage {
    #[default]
    Origin,
    /// 3point: the origin prompt of its own.
    NewOrigin,
    XPoint,
    YPoint,
}

/// UCS / CPLANE at the prompt.
#[derive(Default)]
struct CplaneM {
    name: &'static str,
    stage: Stage,
    origin: Option<Vec3>,
    x: Option<Vec3>,
}

impl CplaneM {
    fn set(s: &mut Session, c: Mat4) -> Result<Step> {
        s.set_cplane(c)?;
        Ok(Step::Done)
    }
    /// A given point (current plane coordinates and [`Session::point_z`]) in the world.
    fn world(s: &Session, p: cadcraft_geom::Vec2) -> Vec3 {
        s.cplane().apply(p.to3(s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0)))
    }
}

impl Interactive for CplaneM {
    fn name(&self) -> &'static str {
        if self.name.is_empty() { "CPLANE" } else { self.name }
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.stage {
            Stage::Origin => {
                let msg = if self.name == "UCS" { "Specify origin of UCS" } else { "CPlane origin" };
                let mut kws = PRESETS.to_vec();
                kws.push("3point");
                Prompt::new(msg, Accept::POINT).kw(&kws).default("World")
            }
            Stage::NewOrigin => Prompt::new("Specify new origin point", Accept::POINT).default("current origin"),
            Stage::XPoint => Prompt::new("Specify point on X-axis", Accept::POINT).default("Accept"),
            Stage::YPoint => Prompt::new("Specify point on the XY plane", Accept::POINT).default("Accept"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let cur = s.cplane();
        match (self.stage, i) {
            (Stage::Origin, Input::Enter) => Self::set(s, Mat4::IDENTITY),
            (Stage::Origin, Input::Keyword(t) | Input::Text(t)) => {
                let t = t.trim().to_ascii_lowercase();
                if matches!(t.as_str(), "3point" | "3p" | "3") {
                    self.stage = Stage::NewOrigin;
                    return Ok(Step::Continue);
                }
                let c = preset(&t).ok_or_else(|| EngineError::Other("Invalid option keyword.".into()))?;
                Self::set(s, c)
            }
            (Stage::Origin | Stage::NewOrigin, Input::Point(p)) => {
                self.origin = Some(Self::world(s, p));
                self.stage = Stage::XPoint;
                Ok(Step::Continue)
            }
            (Stage::NewOrigin, Input::Enter) => {
                self.origin = Some(cur.col(3));
                self.stage = Stage::XPoint;
                Ok(Step::Continue)
            }
            (Stage::XPoint, Input::Point(p)) => {
                let o = self.origin.unwrap_or(cur.col(3));
                let x = Self::world(s, p) - o;
                if x.normalized() == Vec3::ZERO {
                    return Err(EngineError::Other("The X axis point must differ from the origin.".into()));
                }
                self.x = Some(x);
                self.stage = Stage::YPoint;
                Ok(Step::Continue)
            }
            (Stage::XPoint, Input::Enter) => {
                let c = frame_from(&cur, self.origin.unwrap_or(cur.col(3)), cur.col(0), None).unwrap_or(cur);
                Self::set(s, c)
            }
            (Stage::YPoint, Input::Point(p)) => {
                let o = self.origin.unwrap_or(cur.col(3));
                let c = Mat4::frame(o, self.x.unwrap_or(cur.col(0)), Self::world(s, p) - o)
                    .ok_or_else(|| EngineError::Other("The point must be off the X axis.".into()))?;
                Self::set(s, c)
            }
            (Stage::YPoint, Input::Enter) => {
                let o = self.origin.unwrap_or(cur.col(3));
                let c = frame_from(&cur, o, self.x.unwrap_or(cur.col(0)), None).ok_or_else(|| EngineError::Other("Invalid X axis.".into()))?;
                Self::set(s, c)
            }
            (_, Input::Cancel) => Ok(Step::Cancel),
            _ => Err(EngineError::Other("Requires a point or an option keyword.".into())),
        }
    }
}

#[cfg(test)]
#[path = "cplane_tests.rs"]
mod tests;
