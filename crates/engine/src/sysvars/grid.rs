//! The grid variables: GRIDMODE, GRIDUNIT, SNAPUNIT, GRIDMAJOR, GRIDLINECOUNT and GRIDAXES (Rhino's
//! grid options). They are the drawing's grid ([`Session::drawing_grid`]: GRIDUNIT and SNAPUNIT
//! in its header, the others session settings kept in the profile), or while the current model
//! tile has a grid of its own ([`Session::tile_grid`]) that tile's, as AutoCAD keeps them per
//! viewport and Rhino per view.

use cadcraft_doc::{HVal, TileGrid};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

/// The grid variables (upper case).
pub const NAMES: [&str; 6] = ["GRIDMODE", "GRIDUNIT", "SNAPUNIT", "GRIDMAJOR", "GRIDLINECOUNT", "GRIDAXES"];

/// Whether `name` (upper case) is a grid variable.
pub(super) fn is_grid_var(name: &str) -> bool {
    NAMES.contains(&name)
}

/// Grid variable `name`'s value in `g`.
pub fn value(g: &TileGrid, name: &str) -> Option<Value> {
    Some(match name {
        "GRIDMODE" => json!(i32::from(g.on)),
        "GRIDUNIT" => json!([g.unit.x, g.unit.y]),
        "SNAPUNIT" => json!([g.snap.x, g.snap.y]),
        "GRIDMAJOR" => json!(g.major),
        "GRIDLINECOUNT" => json!(g.count),
        "GRIDAXES" => json!(i32::from(g.axes)),
        _ => return None,
    })
}

/// A whole number from 0 to `max` (true and false are 1 and 0).
fn whole(v: &Value, max: u32) -> Option<u32> {
    let f = v.as_bool().map(f64::from).or_else(|| super::as_f64(v))?;
    (f.fract() == 0.0 && (0.0..=f64::from(max)).contains(&f)).then_some(f as u32)
}

/// Set grid variable `name` to `v` in `g`, checked as SETVAR checks it.
fn set_in(g: &mut TileGrid, name: &str, v: &Value) -> Option<()> {
    match name {
        "GRIDMODE" => g.on = super::as_bool(v)?,
        // GRIDUNIT 0 follows the snap spacing; SNAPUNIT must be positive.
        "GRIDUNIT" => g.unit = super::as_pt(v).filter(|p| p.is_finite() && p.x >= 0.0 && p.y >= 0.0)?,
        "SNAPUNIT" => g.snap = super::as_pt(v).filter(|p| p.is_finite() && p.x > 0.0 && p.y > 0.0)?,
        "GRIDMAJOR" => g.major = super::as_f64(v)?.clamp(1.0, 100.0) as u32,
        "GRIDLINECOUNT" => g.count = whole(v, 1000)?,
        "GRIDAXES" => g.axes = whole(v, 1)? == 1,
        _ => return None,
    }
    Some(())
}

/// Set grid variable `name` (upper case): in the current tile's own grid when it has one, else
/// in the drawing's.
pub(super) fn set(s: &mut Session, name: &str, v: &Value) -> Result<()> {
    match s.tile_grid() {
        Some(mut g) => {
            set_in(&mut g, name, v).ok_or_else(|| bad(name))?;
            s.put_tile_grid(Some(g))
        }
        None => set_drawing(s, name, v),
    }
}

/// Set grid variable `name` (upper case) in the drawing's grid, whatever the current tile shows
/// (a saved profile is the drawing's grid).
pub fn set_drawing(s: &mut Session, name: &str, v: &Value) -> Result<()> {
    // The spacings are the drawing's own: there must be one.
    if matches!(name, "GRIDUNIT" | "SNAPUNIT") {
        s.doc()?;
    }
    let mut g = s.drawing_grid();
    set_in(&mut g, name, v).ok_or_else(|| bad(name))?;
    put_drawing_grid(s, &g)
}

fn bad(name: &str) -> EngineError {
    let allowed = match name {
        "GRIDLINECOUNT" => " (0 to 1000)",
        "GRIDAXES" => " (0 or 1)",
        _ => "",
    };
    EngineError::BadParams { cmd: "setvar".into(), msg: format!("invalid value for {name}{allowed}") }
}

/// Make `g` the drawing's grid: the session settings, and the header's spacings where they
/// differ (an unchanged spacing leaves the drawing as it is).
pub(crate) fn put_drawing_grid(s: &mut Session, g: &TileGrid) -> Result<()> {
    let st = &mut s.settings;
    (st.gridmode, st.gridmajor, st.gridlinecount, st.gridaxes) = (g.on, g.major, g.count, g.axes);
    let Ok(d) = s.doc() else { return Ok(()) };
    let (unit, snap) = (d.header.grid_unit() != g.unit, d.header.snap_unit() != g.snap);
    if unit || snap {
        let h = &mut s.doc_mut()?.header;
        if unit {
            h.set("GRIDUNIT", HVal::Point(g.unit.to3(0.0)));
        }
        if snap {
            h.set("SNAPUNIT", HVal::Point(g.snap.to3(0.0)));
        }
    }
    Ok(())
}
