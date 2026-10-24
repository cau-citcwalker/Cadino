//! 3D object snaps: 3DOSNAP (F4 turns them on and off; typed, the UI opens Drafting Settings on
//! its 3D Object Snap tab) and -3DOSNAP at the command line. The modes are 3DOSMODE's bits; the
//! canvas finds the points in 3D views (`canvas/snap3d.rs`).

use serde_json::{Value, json};

use super::*;
use crate::{Accept, Input, Interactive, Prompt, Result, Session, Step};

/// 3DOSMODE bits, as AutoCAD documents them.
pub mod mode3d {
    /// The 3D object snaps are off (F4); the other bits are kept.
    pub const OFF: u32 = 1;
    pub const VERTEX: u32 = 2;
    pub const MIDPOINT: u32 = 4;
    pub const CENTER: u32 = 8;
    pub const KNOT: u32 = 16;
    pub const PERPENDICULAR: u32 = 32;
    pub const NEAREST: u32 = 64;
    use crate::snap::mode;
    /// Each mode with its name in Drafting Settings, its -3DOSNAP keyword and the object snap
    /// whose marker it shows.
    pub const ALL: [(u32, &str, &str, u32); 6] = [
        (VERTEX, "Vertex", "ZVERtex", mode::END),
        (MIDPOINT, "Midpoint on edge", "ZMIDpoint", mode::MID),
        (CENTER, "Center of face", "ZCENter", mode::CEN),
        (KNOT, "Knot", "ZKNOt", mode::NOD),
        (PERPENDICULAR, "Perpendicular", "ZPERpendicular", mode::PER),
        (NEAREST, "Nearest to face", "ZNEARest", mode::NEA),
    ];
    /// Every mode.
    pub const MODES: u32 = VERTEX | MIDPOINT | CENTER | KNOT | PERPENDICULAR | NEAREST;
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("3dosnap", "3D Object Snap", run)
            .key("F4")
            .params("{on?: bool, modes?: [\"vertex\",\"midpoint\",\"center\",\"knot\",\"perpendicular\",\"nearest\"] | osmode3d?: n} (none: toggle) → {on, osmode3d, modes}")
            .enabled(always)
            .noundo()
            .transparent(),
        CommandSpec::new("-3dosnap", "3D Object Snap", run).params("as 3DOSNAP").enabled(always).noundo().transparent().interactive(|_| Ok(Box::new(Osnap3dM))),
    ]
}

/// The 3DOSMODE bit of a mode name: its keyword or name, with or without the Z, at least three
/// letters of it.
fn bit(name: &str) -> Option<u32> {
    let l = name.trim().to_ascii_lowercase();
    let l = l.strip_prefix('z').filter(|r| r.len() >= 3).unwrap_or(&l);
    if l.len() < 3 {
        return None;
    }
    mode3d::ALL.iter().find(|(_, n, ..)| n.to_ascii_lowercase().starts_with(l)).map(|(b, ..)| *b)
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let st = &mut s.settings;
    if let Some(n) = p.get("osmode3d").and_then(Value::as_u64) {
        st.osmode3d = u32::try_from(n).unwrap_or(0) & (mode3d::OFF | mode3d::MODES);
    }
    if let Some(ms) = p.get("modes").and_then(Value::as_array) {
        let mut bits = 0;
        for m in ms.iter().take(64) {
            let name = m.as_str().unwrap_or("");
            bits |= bit(name).ok_or_else(|| bad("3dosnap", format!("unknown 3D object snap mode `{name}`")))?;
        }
        st.osmode3d = (st.osmode3d & mode3d::OFF) | bits;
    }
    if p.get("osmode3d").is_none() && p.get("modes").is_none() {
        let on = p.get("on").and_then(Value::as_bool).unwrap_or(st.osmode3d & mode3d::OFF != 0);
        st.osmode3d = if on { st.osmode3d & !mode3d::OFF } else { st.osmode3d | mode3d::OFF };
    }
    let (bits, on) = (st.osmode3d, st.osmode3d & mode3d::OFF == 0);
    s.touch();
    let modes: Vec<&str> = mode3d::ALL.iter().filter(|(b, ..)| bits & b != 0).map(|(_, n, ..)| *n).collect();
    Ok(json!({ "on": on, "osmode3d": bits, "modes": modes, "message": format!("<3D Osnap {}>", if on { "on" } else { "off" }) }))
}

/// -3DOSNAP: "Enter list of 3D object snap modes" (ZVERtex, ZMIDpoint, ...; ZNONe clears them).
/// Enter keeps the current modes; a list turns the 3D object snaps on.
struct Osnap3dM;

impl Interactive for Osnap3dM {
    fn name(&self) -> &'static str {
        "-3DOSNAP"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let bits = s.settings.osmode3d;
        let names: Vec<&str> = mode3d::ALL.iter().filter(|(b, ..)| bits & mode3d::OFF == 0 && bits & b != 0).map(|(_, _, k, _)| *k).collect();
        s.echo(format!("Current 3D osnap modes: {}", if names.is_empty() { "ZNONe".to_string() } else { names.join(",") }));
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter list of 3D object snap modes", Accept::WORD)
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
            let l = name.to_ascii_lowercase();
            if l.len() >= 3 && ["znone", "none"].iter().any(|n| n.starts_with(&l)) {
                continue;
            }
            bits |= bit(name).ok_or_else(|| EngineError::Other(format!("Unknown 3D object snap mode \"{name}\".")))?;
        }
        s.settings.osmode3d = bits;
        s.touch();
        Ok(Step::Done)
    }
}

#[cfg(test)]
mod tests {
    use super::mode3d::*;
    use crate::Session;
    use serde_json::json;

    /// 3DOSNAP's JSON form (F4) toggles the 3D object snaps, keeping the modes; `modes` and
    /// `osmode3d` set them; -3DOSNAP takes AutoCAD's keywords; 3DOSMODE and OSNAPZ are system
    /// variables.
    #[test]
    fn f4_toggles_and_the_modes_are_set_by_name() {
        let mut s = Session::new();
        assert_eq!(s.settings.osmode3d, OFF | VERTEX | CENTER);
        let r = s.execute("3dosnap", &json!({})).unwrap();
        assert_eq!((r["on"].as_bool(), s.settings.osmode3d), (Some(true), VERTEX | CENTER));
        assert_eq!(r["message"], "<3D Osnap on>");
        s.execute("3dosnap", &json!({})).unwrap();
        assert_eq!(s.settings.osmode3d, OFF | VERTEX | CENTER);
        s.execute("3dosnap", &json!({ "modes": ["zmid", "Nearest to face", "perp"] })).unwrap();
        assert_eq!(s.settings.osmode3d, OFF | MIDPOINT | NEAREST | PERPENDICULAR);
        assert!(s.execute("3dosnap", &json!({ "modes": ["zz"] })).is_err());
        s.execute("3dosnap", &json!({ "on": true })).unwrap();
        s.cmdline("-3dosnap").unwrap();
        assert!(s.prompt_text().contains("Enter list of 3D object snap modes"), "{}", s.prompt_text());
        s.cmdline("ZVERtex,zkno").unwrap();
        assert_eq!((s.running.is_none(), s.settings.osmode3d), (true, VERTEX | KNOT));
        s.cmdline("-3dosnap").unwrap();
        s.cmdline("znon").unwrap();
        assert_eq!(s.settings.osmode3d, 0);
        assert!(s.execute("-3dosnap", &json!({ "osmode3d": 255 })).is_ok());
        assert_eq!(s.settings.osmode3d, OFF | MODES);
        crate::sysvars::set(&mut s, "3DOSMODE", &json!(2)).unwrap();
        assert_eq!(crate::sysvars::get(&s, "3dosmode"), Some(json!(2)));
        assert!(crate::sysvars::set(&mut s, "3DOSMODE", &json!(200)).is_err());
        crate::sysvars::set(&mut s, "OSNAPZ", &json!(1)).unwrap();
        assert!(s.settings.osnapz);
        assert_eq!(crate::sysvars::get(&s, "OSNAPZ"), Some(json!(1)));
    }
}
