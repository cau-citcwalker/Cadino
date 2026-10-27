//! Drafting settings and system variables: ORTHO, OSNAP, GRID, SNAP, POLAR, SETVAR/GETVAR.

use serde_json::{Value, json};

use super::*;
use crate::{Accept, Input, Interactive, Prompt, Result, Session, Step};

pub(super) mod iso;

/// GRID (F7): the current viewport's grid on or off (its own grid, else the drawing's).
fn run_grid(s: &mut Session, p: &Value) -> Result<Value> {
    let on = p.get("on").and_then(Value::as_bool).unwrap_or(!s.grid().on);
    crate::sysvars::set(s, "GRIDMODE", &json!(on))?;
    s.touch();
    Ok(json!({ "on": on, "message": format!("<Grid {}>", if on { "on" } else { "off" }) }))
}

fn toggle(s: &mut Session, p: &Value, get: fn(&mut crate::Settings) -> &mut bool, name: &str) -> Result<Value> {
    let v = get(&mut s.settings);
    *v = p.get("on").and_then(Value::as_bool).unwrap_or(!*v);
    let on = *v;
    s.touch();
    Ok(json!({ "on": on, "message": format!("<{name} {}>", if on { "on" } else { "off" }) }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("ortho", "Ortho Mode", |s, p| toggle(s, p, |st| &mut st.orthomode, "Ortho"))
            .key("F8")
            .params("{on?: bool}")
            .enabled(always)
            .noundo()
            .transparent(),
        CommandSpec::new("grid", "Grid Display", run_grid).key("F7").params("{on?: bool}").enabled(always).noundo().transparent(),
        CommandSpec::new("snap", "Snap Mode", |s, p| toggle(s, p, |st| &mut st.snapmode, "Snap"))
            .key("F9")
            .params("{on?: bool}")
            .enabled(always)
            .noundo()
            .transparent(),
        CommandSpec::new("polar", "Polar Tracking", |s, p| toggle(s, p, |st| &mut st.polarmode, "Polar"))
            .key("F10")
            .params("{on?: bool}")
            .enabled(always)
            .noundo()
            .transparent(),
        CommandSpec::new("otrack", "Object Snap Tracking", |s, p| toggle(s, p, |st| &mut st.otrack, "Object Snap Tracking"))
            .key("F11")
            .params("{on?: bool}")
            .enabled(always)
            .noundo()
            .transparent(),
        CommandSpec::new("dynmode", "Dynamic Input", |s, p| toggle(s, p, |st| &mut st.dynmode, "Dynamic Input"))
            .key("F12")
            .params("{on?: bool}")
            .enabled(always)
            .noundo()
            .transparent(),
        CommandSpec::new("lwdisplay", "Show/Hide Lineweight", |s, p| toggle(s, p, |st| &mut st.lwdisplay, "Lineweight"))
            .params("{on?: bool}")
            .enabled(always)
            .noundo()
            .transparent(),
        CommandSpec::new("isodraft", "Isometric Drafting", iso::run_isodraft)
            .params("{on?: bool, plane?: \"left\"|\"top\"|\"right\"|\"orthographic\"} (neither: toggle)")
            .enabled(always)
            .noundo()
            .transparent()
            .interactive(|_| Ok(Box::new(iso::IsodraftM))),
        CommandSpec::new("isoplane", "Isometric Plane", iso::run_isoplane)
            .key("F5")
            .params("{plane?: \"left\"|\"top\"|\"right\"} (none: the next plane)")
            .enabled(always)
            .noundo()
            .transparent()
            .interactive(|_| Ok(Box::new(iso::IsoplaneM))),
        CommandSpec::new("transparencydisplay", "Show/Hide Transparency", |s, p| toggle(s, p, |st| &mut st.transparency_display, "Transparency"))
            .params("{on?: bool}")
            .enabled(always)
            .noundo(),
        CommandSpec::new("qpmode", "Quick Properties", |s, p| toggle(s, p, |st| &mut st.qpmode, "Quick Properties"))
            .params("{on?: bool}")
            .enabled(always)
            .noundo()
            .transparent(),
        CommandSpec::new("selectioncycling", "Selection Cycling", |s, p| toggle(s, p, |st| &mut st.selection_cycling, "Selection Cycling"))
            .params("{on?: bool}")
            .enabled(always)
            .noundo(),
        CommandSpec::new("osnap", "Object Snap", run_osnap)
            .key("F3")
            .alias(&["os"])
            .params("{on?: bool, modes?: [\"end\",\"mid\",...] | osmode?: n}")
            .enabled(always)
            .noundo()
            .transparent(),
        CommandSpec::new("-osnap", "Object Snap", run_osnap)
            .params("as OSNAP")
            .enabled(always)
            .noundo()
            .transparent()
            .interactive(|_| Ok(Box::new(OsnapM))),
        CommandSpec::new("fill", "Fill", |s, p| run_mode(s, p, "FILLMODE"))
            .params("{on?: bool} (FILLMODE) → {on}")
            .interactive(|_| Ok(Box::new(ModeM { var: "FILLMODE", name: "FILL" }))),
        CommandSpec::new("qtext", "Quick Text", |s, p| run_mode(s, p, "QTEXTMODE"))
            .params("{on?: bool} (QTEXTMODE) → {on}")
            .interactive(|_| Ok(Box::new(ModeM { var: "QTEXTMODE", name: "QTEXT" }))),
        CommandSpec::new("ucsicon", "UCS Icon", |s, p| run_mode(s, p, "UCSICON"))
            .params("{on?: bool} (UCSICON) → {on}")
            .enabled(always)
            .noundo()
            .interactive(|_| Ok(Box::new(ModeM { var: "UCSICON", name: "UCSICON" }))),
        CommandSpec::new("dsettings", "Drafting Settings...", run_dsettings)
            .menu(&["Tools", "Drafting Settings..."])
            .alias(&["ds", "se"])
            .params("{snapunit?: [x,y], gridunit?: [x,y], polarang? (degrees), gridmajor?}")
            .enabled(always),
        CommandSpec::new("setvar", "Set Variable", run_setvar)
            .menu(&["Tools", "Inquiry", "Set Variable"])
            .alias(&["set"])
            .params("{name, value}")
            .transparent()
            .interactive(|_| Ok(Box::new(SetvarM::default()))),
        CommandSpec::new("getvar", "Get Variable", run_getvar)
            .params("{name}")
            .enabled(always)
            .noundo()
            .interactive(|_| Ok(Box::new(SetvarM { name: None, get: true }))),
        CommandSpec::new("sysvars", "List System Variables", |s, _| Ok(crate::sysvars::list(s))).enabled(always).noundo(),
    ]
}

/// Object snap modes by the short names typed at -OSNAP (the first three letters are enough).
const SNAP_NAMES: [(u32, &str); 14] = {
    use crate::snap::mode::*;
    [
        (END, "End"),
        (MID, "Mid"),
        (CEN, "Cen"),
        (GCEN, "Gcen"),
        (NOD, "Nod"),
        (QUA, "Qua"),
        (INT, "Int"),
        (EXT, "Ext"),
        (INS, "Ins"),
        (PER, "Per"),
        (TAN, "Tan"),
        (NEA, "Nea"),
        (APP, "App"),
        (PAR, "Par"),
    ]
};

/// The OSMODE bit of an object snap mode name (END, MID, CEN...).
fn snap_bit(name: &str) -> Option<u32> {
    let l = name.trim().to_ascii_lowercase();
    let key = l.get(..3)?;
    SNAP_NAMES.iter().find(|(_, n)| n.to_ascii_lowercase().starts_with(key)).map(|(b, _)| *b)
}

fn run_osnap(s: &mut Session, p: &Value) -> Result<Value> {
    use crate::snap::mode;
    if let Some(n) = p.get("osmode").and_then(Value::as_u64) {
        s.settings.osmode = (n as u32) & 0x7fff;
    }
    if let Some(ms) = p.get("modes").and_then(Value::as_array) {
        s.settings.osmode = ms.iter().filter_map(Value::as_str).filter_map(snap_bit).fold(0, |a, b| a | b);
    }
    if p.get("osmode").is_none() && p.get("modes").is_none() {
        let on = p.get("on").and_then(Value::as_bool).unwrap_or(s.settings.osmode & mode::OFF != 0);
        if on {
            s.settings.osmode &= !mode::OFF;
        } else {
            s.settings.osmode |= mode::OFF;
        }
    }
    s.touch();
    let on = s.settings.osmode & mode::OFF == 0;
    let active: Vec<&str> = mode::ALL.iter().filter(|(b, _)| s.settings.osmode & b != 0).map(|(_, n)| *n).collect();
    Ok(json!({ "on": on, "osmode": s.settings.osmode, "modes": active, "message": format!("<Osnap {}>", if on { "on" } else { "off" }) }))
}

/// -OSNAP: "Enter list of object snap modes" (END,MID,CEN...; NONe turns them all off). Enter keeps
/// the current modes.
struct OsnapM;

impl Interactive for OsnapM {
    fn name(&self) -> &'static str {
        "-OSNAP"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let osmode = s.settings.osmode;
        let on = osmode & crate::snap::mode::OFF == 0;
        let names: Vec<&str> = SNAP_NAMES.iter().filter(|(b, _)| on && osmode & b != 0).map(|(_, n)| *n).collect();
        s.echo(format!("Current osnap modes: {}", if names.is_empty() { "None".to_string() } else { names.join(",") }));
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter list of object snap modes", Accept::WORD)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let t = match i {
            Input::Text(t) | Input::Keyword(t) => t,
            Input::Cancel => return Ok(Step::Cancel),
            Input::Enter => return Ok(Step::Done),
            _ => return Ok(Step::Continue),
        };
        let mut bits = 0;
        for name in t.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            if name.len() >= 3 && "none".starts_with(&name.to_ascii_lowercase()) {
                continue;
            }
            bits |= snap_bit(name).ok_or_else(|| EngineError::Other(format!("Unknown object snap mode \"{name}\".")))?;
        }
        s.settings.osmode = bits;
        s.touch();
        Ok(Step::Done)
    }
}

/// FILL, QTEXT and UCSICON: an on/off system variable, `{on}` sets it.
fn run_mode(s: &mut Session, p: &Value, var: &str) -> Result<Value> {
    if let Some(on) = p.get("on").and_then(Value::as_bool) {
        crate::sysvars::set(s, var, &json!(i32::from(on)))?;
        s.touch();
    }
    Ok(json!({ "on": crate::sysvars::get(s, var).and_then(|v| v.as_i64()).unwrap_or(0) != 0 }))
}

/// FILL, QTEXT and UCSICON at the command line: "Enter mode [ON/OFF] <current>".
// ponytail: UCSICON offers ON/OFF only (no Noorigin/ORigin/Properties).
struct ModeM {
    var: &'static str,
    name: &'static str,
}

impl Interactive for ModeM {
    fn name(&self) -> &'static str {
        self.name
    }
    fn prompt(&self, s: &Session) -> Prompt {
        const KW: Accept = Accept { point: false, number: false, text: false, select: false, enter: true };
        let on = crate::sysvars::get(s, self.var).and_then(|v| v.as_i64()).unwrap_or(0) != 0;
        let message = if self.var == "UCSICON" { "Enter an option" } else { "Enter mode" };
        Prompt::new(message, KW).kw(&["ON", "OFF"]).default(if on { "ON" } else { "OFF" })
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Keyword(k) => {
                run_mode(s, &json!({ "on": k == "ON" }), self.var)?;
                Ok(Step::Done)
            }
            Input::Enter => Ok(Step::Done),
            Input::Cancel => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
}

fn run_dsettings(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(v) = point_param(p, "snapunit").filter(|v| v.x > 0.0 && v.y > 0.0) {
        crate::sysvars::set(s, "SNAPUNIT", &json!([v.x, v.y]))?;
    }
    if let Some(v) = point_param(p, "gridunit").filter(|v| v.x >= 0.0 && v.y >= 0.0) {
        crate::sysvars::set(s, "GRIDUNIT", &json!([v.x, v.y]))?;
    }
    if let Some(a) = p.get("polarang").and_then(Value::as_f64).filter(|a| *a > 0.0) {
        s.settings.polarang = a.to_radians();
    }
    if let Some(m) = p.get("gridmajor").and_then(Value::as_u64) {
        crate::sysvars::set(s, "GRIDMAJOR", &json!(m.min(100)))?;
    }
    s.touch();
    Ok(serde_json::to_value(s.drafting_settings()).unwrap_or(Value::Null))
}

fn run_setvar(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_param(p, "name").ok_or_else(|| bad("setvar", "`name` is required"))?;
    let v = p.get("value").ok_or_else(|| bad("setvar", "`value` is required"))?;
    crate::sysvars::set(s, name, v)?;
    s.touch();
    Ok(json!({ "name": name.to_ascii_uppercase(), "value": crate::sysvars::get(s, name) }))
}

/// A system variable value as the command line shows it.
fn show_value(v: &Value) -> String {
    match v {
        Value::String(t) => t.clone(),
        Value::Array(a) => a.iter().map(show_value).collect::<Vec<_>>().join(","),
        other => other.to_string(),
    }
}

/// SETVAR at the command line: "Enter variable name or [?]", then "Enter new value for NAME <current>".
/// GETVAR asks only for the name and shows "NAME = value".
#[derive(Default)]
struct SetvarM {
    name: Option<String>,
    get: bool,
}

impl Interactive for SetvarM {
    fn name(&self) -> &'static str {
        if self.get { "GETVAR" } else { "SETVAR" }
    }
    fn prompt(&self, s: &Session) -> Prompt {
        // Not a free-text prompt: Space ends the input, as it does for names and numbers.
        let accept = Accept { number: true, ..Accept::TEXT };
        match &self.name {
            None => Prompt::new("Enter variable name", accept).kw(&["?"]),
            Some(n) => {
                let p = Prompt::new(format!("Enter new value for {n}"), accept);
                match crate::sysvars::get(s, n) {
                    // A text variable's value is quoted, as AutoCAD shows it: <"0">.
                    Some(Value::String(t)) => p.default(format!("\"{t}\"")),
                    Some(v) => p.default(show_value(&v)),
                    None => p,
                }
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match (self.name.clone(), i) {
            (_, Input::Enter | Input::Cancel) => Ok(Step::Done),
            (None, Input::Keyword(_)) => {
                if let Value::Object(m) = crate::sysvars::list(s) {
                    for (k, v) in m {
                        s.echo(format!("{k:<12} {}", show_value(&v)));
                    }
                }
                Ok(Step::Done)
            }
            (None, Input::Text(t)) => {
                let n = t.trim().to_ascii_uppercase();
                match crate::sysvars::get(s, &n) {
                    None => {
                        s.echo(format!("Unknown variable {n}. Type SETVAR ? to list variables."));
                        Ok(Step::Done)
                    }
                    Some(v) if self.get => {
                        s.echo(format!("{n} = {}", show_value(&v)));
                        Ok(Step::Done)
                    }
                    Some(v) if crate::sysvars::is_read_only(&n) => {
                        s.echo(format!("{n} = {} (read only)", show_value(&v)));
                        Ok(Step::Done)
                    }
                    Some(_) => {
                        self.name = Some(n);
                        Ok(Step::Continue)
                    }
                }
            }
            (Some(n), Input::Text(t)) => match crate::sysvars::set(s, &n, &Value::String(t)) {
                Ok(()) => {
                    s.touch();
                    Ok(Step::Done)
                }
                Err(e) => {
                    s.echo(e.to_string());
                    Ok(Step::Continue)
                }
            },
            _ => Ok(Step::Continue),
        }
    }
}

fn run_getvar(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_param(p, "name").ok_or_else(|| bad("getvar", "`name` is required"))?;
    let v = crate::sysvars::get(s, name).ok_or_else(|| bad("getvar", format!("unknown variable `{name}`")))?;
    Ok(json!({ "name": name.to_ascii_uppercase(), "value": v }))
}
