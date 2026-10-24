//! UCSMAN (UC, DDUCS; Tools ▸ Named UCS...): the named UCSs of the drawing. JSON
//! `{option: list|save|restore|delete|rename|previous|orthographic, …}` saves the current UCS
//! under a name, restores one (World included), deletes or renames one, goes back to the previous
//! UCS, or sets an orthographic UCS (Top, Bottom, Front, Back, Left, Right) relative to World or
//! a named UCS, `depth` along its normal. Typed or from the menu: the UCS dialog. Each change is
//! an undo step; the UCS itself is the current tile's construction plane (UCS, CPLANE).
// ponytail: Previous is the last different UCS in the drawing's undo history (a second Previous
// returns to where the first started), not AutoCAD's ten-deep UCS history.

use cadcraft_doc::{Drawing, Ucs};
use cadcraft_geom::{Mat4, Vec3};
use serde_json::{Value, json};

use super::*;
use crate::{Result, Session};

const CMD: &str = "ucsman";
/// The orthographic UCSs, in the dialog's order.
pub const ORTHOGRAPHIC: [&str; 6] = ["Top", "Bottom", "Front", "Back", "Left", "Right"];
/// Undo steps looked through for the previous UCS.
const MAX_PREVIOUS: usize = 1000;

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::new(CMD, "Named UCS...", run).menu(&["Tools", "Named UCS..."]).alias(&["uc", "dducs"]).params(
        "{option?: list (default)|save|restore|delete|rename|previous|orthographic, name? (save, restore: also \"World\", delete, \
                 rename), to? (rename), ortho?: top|bottom|front|back|left|right, relativeTo?: \"World\" | a named UCS, depth? (along the \
                 orthographic UCS's normal)} → {current, ucs: [{name, origin, xaxis, yaxis, zaxis}], previous: bool} (typed or from the \
                 menu: the UCS dialog)",
    )]
}

fn v3(v: Vec3) -> Value {
    json!([v.x, v.y, v.z])
}

/// The UCS stored as `u`: World when its axes make no plane.
fn frame(u: &Ucs) -> Mat4 {
    Mat4::frame(u.origin, u.x_axis, u.y_axis).unwrap_or(Mat4::IDENTITY)
}

fn near(a: &Mat4, b: &Mat4) -> bool {
    (0..4).all(|i| {
        let d = a.col(i) - b.col(i);
        d.x.abs() < 1e-9 && d.y.abs() < 1e-9 && d.z.abs() < 1e-9
    })
}

/// The named UCS called `name` (any case).
fn named<'a>(d: &'a Drawing, name: &str) -> Option<&'a Ucs> {
    d.ucss.iter().find(|u| u.name.eq_ignore_ascii_case(name.trim()))
}

/// The UCS called `name`: World or a named one.
pub fn plane(d: &Drawing, name: &str) -> Result<Mat4> {
    if name.trim().eq_ignore_ascii_case("world") {
        return Ok(Mat4::IDENTITY);
    }
    named(d, name).map(frame).ok_or_else(|| bad(CMD, format!("no UCS named \"{}\"", name.trim())))
}

/// The construction plane `d` keeps for tile `active` (as `Session::set_cplane` stores it).
fn plane_of(d: &Drawing, active: usize) -> Mat4 {
    d.vports.iter().find(|k| k.is_active() && k.tiles.len() > 1).and_then(|c| c.tiles.get(active)).map_or_else(|| d.header.cplane(), |t| t.cplane)
}

/// The last UCS before the current one in the undo history.
fn previous(s: &Session) -> Option<Mat4> {
    let st = s.state().ok()?;
    let cur = s.cplane();
    st.undo.iter().rev().take(MAX_PREVIOUS).map(|snap| plane_of(&snap.doc, st.tiles.active)).find(|c| !near(c, &cur))
}

/// The orthographic UCS `ortho` of `base`, its origin moved `depth` along its normal.
pub fn orthographic(base: &Mat4, ortho: &str, depth: f64) -> Option<Mat4> {
    let m = base.mul(&super::view::cplane::preset(ortho)?);
    Mat4::frame(base.col(3) + m.col(2) * depth, m.col(0), m.col(1))
}

/// The current UCS's name: World, a named UCS it matches, or Unnamed.
fn current_name(d: &Drawing, c: &Mat4) -> String {
    if near(c, &Mat4::IDENTITY) {
        return "World".into();
    }
    d.ucss.iter().find(|u| near(&frame(u), c)).map_or_else(|| "Unnamed".into(), |u| u.name.clone())
}

pub fn list(s: &Session) -> Result<Value> {
    let d = s.doc()?;
    let ucs: Vec<Value> = d
        .ucss
        .iter()
        .map(|u| {
            let f = frame(u);
            json!({ "name": u.name, "origin": v3(f.col(3)), "xaxis": v3(f.col(0)), "yaxis": v3(f.col(1)), "zaxis": v3(f.col(2)) })
        })
        .collect();
    Ok(json!({ "current": current_name(d, &s.cplane()), "ucs": ucs, "previous": previous(s).is_some() }))
}

fn name_param(p: &Value) -> Result<&str> {
    str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(CMD, "`name` is required"))
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    if !(p.is_object() || p.is_null()) {
        return Err(bad(CMD, "parameters must be an object"));
    }
    let option = str_param(p, "option").unwrap_or("list").to_ascii_lowercase();
    match option.as_str() {
        "list" | "?" => return list(s),
        "save" => {
            let name = name_param(p)?;
            if name.eq_ignore_ascii_case("world") || name.len() > 255 || name.chars().any(|c| "<>/\\\":;?*|,=`".contains(c)) {
                return Err(bad(CMD, format!("\"{name}\" is not a valid UCS name")));
            }
            let c = s.cplane();
            let u = Ucs { name: name.to_string(), origin: c.col(3), x_axis: c.col(0), y_axis: c.col(1) };
            let d = s.doc_mut()?;
            match d.ucss.iter_mut().find(|x| x.name.eq_ignore_ascii_case(name)) {
                Some(x) => *x = Ucs { name: x.name.clone(), ..u },
                None => d.ucss.push(u),
            }
        }
        "restore" | "set" => {
            let c = plane(s.doc()?, name_param(p)?)?;
            s.set_cplane(c)?;
        }
        "delete" => {
            let name = name_param(p)?;
            let d = s.doc_mut()?;
            let before = d.ucss.len();
            d.ucss.retain(|u| !u.name.eq_ignore_ascii_case(name));
            if d.ucss.len() == before {
                return Err(bad(CMD, format!("no UCS named \"{name}\"")));
            }
        }
        "rename" => {
            let to = str_param(p, "to").ok_or_else(|| bad(CMD, "`to` is required"))?;
            run_body(s, "rename", &json!({ "table": "ucs", "from": name_param(p)?, "to": to }))?;
        }
        "previous" | "p" => {
            let c = previous(s).ok_or_else(|| bad(CMD, "there is no previous UCS"))?;
            s.set_cplane(c)?;
        }
        "orthographic" | "ortho" => {
            let ortho = str_param(p, "ortho").ok_or_else(|| bad(CMD, "`ortho` is required: top, bottom, front, back, left or right"))?;
            let base = plane(s.doc()?, str_param(p, "relativeTo").unwrap_or("World"))?;
            let depth = f64_or(p, "depth", 0.0);
            let c = orthographic(&base, ortho, depth).ok_or_else(|| bad(CMD, format!("unknown orthographic UCS `{ortho}`")))?;
            s.set_cplane(c)?;
        }
        _ => return Err(bad(CMD, format!("unknown option `{option}`: list, save, restore, delete, rename, previous or orthographic"))),
    }
    list(s)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn save_restore_rename_delete_and_previous() {
        let mut s = Session::new();
        assert_eq!(s.execute(CMD, &json!({})).unwrap()["current"], json!("World"));
        s.execute("ucs", &json!({ "origin": [10, 5, 0] })).unwrap();
        assert_eq!(s.execute(CMD, &json!({})).unwrap()["current"], json!("Unnamed"));
        let r = s.execute(CMD, &json!({ "option": "save", "name": "Bench" })).unwrap();
        assert_eq!(r["current"], json!("Bench"));
        assert_eq!(r["ucs"][0]["origin"], json!([10.0, 5.0, 0.0]));
        assert!(s.execute(CMD, &json!({ "option": "save", "name": "a|b" })).is_err());

        s.execute(CMD, &json!({ "option": "restore", "name": "world" })).unwrap();
        assert_eq!(s.cplane(), Mat4::IDENTITY);
        // Previous goes back to the UCS before World.
        let r = s.execute(CMD, &json!({ "option": "previous" })).unwrap();
        assert_eq!(r["current"], json!("Bench"));
        s.execute(CMD, &json!({ "option": "restore", "name": "WORLD" })).unwrap();
        s.execute(CMD, &json!({ "option": "restore", "name": "bench" })).unwrap();
        assert_eq!(s.cplane().col(3), Vec3::new(10.0, 5.0, 0.0));

        let r = s.execute(CMD, &json!({ "option": "rename", "name": "Bench", "to": "Workbench" })).unwrap();
        assert_eq!(r["current"], json!("Workbench"));
        let undo = s.state().unwrap().undo.len();
        s.execute(CMD, &json!({ "option": "delete", "name": "workbench" })).unwrap();
        assert!(s.doc().unwrap().ucss.is_empty());
        assert_eq!(s.state().unwrap().undo.len(), undo + 1, "an undo step");
        for p in [json!({ "option": "delete", "name": "Nope" }), json!({ "option": "restore", "name": "Nope" }), json!({ "option": "spin" })] {
            assert!(s.execute(CMD, &p).is_err(), "{p}");
        }
    }

    #[test]
    fn orthographic_ucs_relative_to_a_named_one_with_depth() {
        let mut s = Session::new();
        let r = s.execute(CMD, &json!({ "option": "orthographic", "ortho": "front", "depth": 2 })).unwrap();
        assert_eq!(r["current"], json!("Unnamed"));
        let c = s.cplane();
        // Front: x = X, y = Z, normal -Y; two units along it.
        assert!(near(&c, &Mat4::from_cols(Vec3::X, Vec3::Z, -Vec3::Y, Vec3::new(0.0, -2.0, 0.0))), "{c:?}");
        // Relative to a UCS turned 90° about Z and moved: Top of it is that UCS itself.
        s.execute(CMD, &json!({ "option": "restore", "name": "World" })).unwrap();
        s.execute("ucs", &json!({ "origin": [5, 0, 0], "xaxis": [0, 1, 0] })).unwrap();
        s.execute(CMD, &json!({ "option": "save", "name": "Turned" })).unwrap();
        s.execute(CMD, &json!({ "option": "restore", "name": "World" })).unwrap();
        let r = s.execute(CMD, &json!({ "option": "orthographic", "ortho": "top", "relativeTo": "Turned" })).unwrap();
        assert_eq!(r["current"], json!("Turned"));
        s.execute(CMD, &json!({ "option": "orthographic", "ortho": "right", "relativeTo": "Turned" })).unwrap();
        let c = s.cplane();
        // Turned has x = Y, y = -X; its Right has x = its y (-X), y = Z and normal = its x (Y).
        assert!(near(&c, &Mat4::from_cols(-Vec3::X, Vec3::Z, Vec3::Y, Vec3::new(5.0, 0.0, 0.0))), "{c:?}");
        assert!(s.execute(CMD, &json!({ "option": "orthographic", "ortho": "sideways" })).is_err());
    }
}
