//! Exchanging 3D models with Rhino and other modellers: IGESOUT and IGESIN (solids, surfaces and
//! curves in one IGES file), and RHINOOUT (an IGES file and a STEP file side by side).
//!
//! Curves go out where they are: a planar object (arc, circle, polyline, spline) through its own
//! plane ([`cadcraft_doc::Entity::plane`]: extrusion direction and elevation).
//! ponytail: objects inside block references are not exported; IGES is written in millimetres
//! (OpenCASCADE's default).

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use cadcraft_doc::{EntityKind, Polyline3d, Solid3d};
use cadcraft_geom::Vec3;
use cadcraft_kernel::Curve;
use serde_json::{Value, json};

use super::solids::{all_or_nothing, curve_of, kernel, placed, solid};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("igesout", "Export IGES", run_igesout)
            .menu(&["File", "Export", "IGES"])
            .alias(&["igesexport"])
            .params("{path, handles?}: 3D solids, surfaces and curves (lines, arcs, circles, polylines, splines, helices); those given or selected, else all of model space")
            .noundo()
            .interactive(|_| Ok(Box::new(AskPath { id: "igesout", name: "IGESOUT", msg: "Enter name of IGES file to write" }))),
        CommandSpec::new("igesin", "Import IGES", run_igesin)
            .menu(&["File", "Import", "IGES"])
            .alias(&["igesimport"])
            .params("{path}: surfaces and solids become 3D solids, free curves 3D polylines, on the current layer")
            .interactive(|_| Ok(Box::new(AskPath { id: "igesin", name: "IGESIN", msg: "Enter name of IGES file to import" }))),
        CommandSpec::new("rhinoout", "Export for Rhino", run_rhinoout)
            .menu(&["File", "Export", "Rhino (IGES + STEP)"])
            .params("{path, handles?}: <path>.igs (solids, surfaces and curves) and <path>.stp (solids), as IGESOUT chooses objects")
            .noundo()
            .interactive(|_| Ok(Box::new(AskPath { id: "rhinoout", name: "RHINOOUT", msg: "Enter file name for Rhino (.igs and .stp)" }))),
    ]
}

fn path_req<'a>(cmd: &str, p: &'a Value) -> Result<&'a str> {
    str_param(p, "path").map(str::trim).filter(|x| !x.is_empty()).ok_or_else(|| bad(cmd, "`path` is required"))
}

/// What to export: the objects among `handle`/`handles`, else the selected ones, else all of model
/// space; as solids, curves, and how many of the curves the kernel can't make (degenerate).
fn export_set(s: &Session, p: &Value) -> Result<(Vec<Solid3d>, Vec<Curve>, usize)> {
    // An empty list (as MCP clients send optional arrays) counts as none given.
    let given = ["handle", "handles"].iter().any(|k| p.get(k).is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty())));
    let d = s.doc()?;
    let pick = |hs: &[cadcraft_doc::Handle]| {
        let (mut xs, mut cs, mut bad) = (Vec::new(), Vec::new(), 0usize);
        let mut seen = HashSet::with_capacity(hs.len());
        for e in hs.iter().filter(|h| seen.insert(**h)).filter_map(|h| d.entity(*h)) {
            match &e.kind {
                EntityKind::Solid3d(x) => xs.push(x.clone()),
                _ => match curve_of(e) {
                    Ok(Some(c)) => cs.push(c),
                    Ok(None) => {}
                    Err(_) => bad += 1,
                },
            }
        }
        (xs, cs, bad)
    };
    let mut got = pick(&targets(s, p)?);
    if !given && got.0.is_empty() && got.1.is_empty() {
        got = pick(&d.model.iter().map(|e| e.handle).collect::<Vec<_>>());
    }
    if got.0.is_empty() && got.1.is_empty() {
        return Err(EngineError::Other("There are no 3D solids or curves to export.".into()));
    }
    Ok(got)
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// Writes the IGES file: how many curves went in and how many were left out.
fn write_iges(xs: &[Solid3d], cs: &[Curve], bad: usize, path: &Path) -> Result<(usize, usize)> {
    let written = cadcraft_kernel::export_iges(&xs.iter().map(placed).collect::<Vec<_>>(), cs, path).map_err(kernel)?;
    Ok((written, bad + cs.len().saturating_sub(written)))
}

fn left_out(n: usize) -> String {
    if n == 0 { String::new() } else { format!(" ({} too small to export left out)", plural(n, "curve")) }
}

fn run_igesout(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_req("igesout", p)?;
    let (xs, cs, bad) = export_set(s, p)?;
    let (curves, skipped) = write_iges(&xs, &cs, bad, Path::new(path))?;
    let message = format!("Exported {} and {} to {path}{}.", plural(xs.len(), "solid"), plural(curves, "curve"), left_out(skipped));
    Ok(json!({ "solids": xs.len(), "curves": curves, "skipped": skipped, "path": path, "message": message }))
}

/// `<path>.ext`, with an exchange extension the path already has replaced.
fn with_ext(path: &str, ext: &str) -> PathBuf {
    let p = Path::new(path);
    let known =
        p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["igs", "iges", "stp", "step", "3dm"].iter().any(|k| e.eq_ignore_ascii_case(k)));
    let mut out: OsString = if known { p.with_extension("").into_os_string() } else { p.as_os_str().to_owned() };
    out.push(".");
    out.push(ext);
    PathBuf::from(out)
}

fn run_rhinoout(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_req("rhinoout", p)?;
    let (igs, stp) = (with_ext(path, "igs"), with_ext(path, "stp"));
    let (xs, cs, bad) = export_set(s, p)?;
    let (curves, skipped) = write_iges(&xs, &cs, bad, &igs)?;
    // STEP keeps solids as solids (IGES has them as trimmed surfaces); it has no curves.
    let step = !xs.is_empty();
    if step {
        cadcraft_kernel::export_step(&xs.iter().map(placed).collect::<Vec<_>>(), &stp).map_err(kernel)?;
    }
    let (igs, stp) = (igs.display().to_string(), stp.display().to_string());
    let what = format!("{} and {}", plural(xs.len(), "solid"), plural(curves, "curve"));
    let message = if step {
        format!("Wrote {igs} ({what}) and {stp} ({}) for Rhino{}.", plural(xs.len(), "solid"), left_out(skipped))
    } else {
        format!("Wrote {igs} ({what}) for Rhino{}; no solids, so no STEP file.", left_out(skipped))
    };
    let stp = if step { json!(stp) } else { Value::Null };
    Ok(json!({ "iges": igs, "step": stp, "solids": xs.len(), "curves": curves, "skipped": skipped, "message": message }))
}

/// A polyline the kernel read as a 3D polyline (closed when it ends where it starts); `None` when
/// it has no length or a non-finite point.
fn polyline(mut pts: Vec<Vec3>) -> Option<EntityKind> {
    if pts.len() < 2 || !pts.iter().all(|p| p.is_finite()) {
        return None;
    }
    let closed = pts.len() > 3 && pts.first() == pts.last();
    if closed {
        pts.pop();
    }
    Some(EntityKind::Polyline3d(Polyline3d { points: pts, closed }))
}

fn run_igesin(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_req("igesin", p)?;
    let got = cadcraft_kernel::import_iges(Path::new(path)).map_err(kernel)?;
    let shapes = got.shapes.len();
    let surfaces = got.shapes.iter().filter(|b| b.mesh.open).count();
    let mut kinds: Vec<EntityKind> = got.shapes.into_iter().map(solid).collect();
    kinds.extend(got.curves.into_iter().filter_map(polyline));
    let curves = kinds.len().saturating_sub(shapes);
    // Everything is read before the drawing changes.
    let handles = all_or_nothing(s, |s| kinds.into_iter().map(|k| s.add_entity(k).map(|h| h.hex())).collect::<Result<Vec<_>>>())?;
    let message = format!(
        "Imported {}, {} and {} from {path}.",
        plural(shapes.saturating_sub(surfaces), "solid"),
        plural(surfaces, "surface"),
        plural(curves, "curve")
    );
    Ok(json!({ "handles": handles, "solids": shapes, "surfaces": surfaces, "curves": curves, "message": message }))
}

/// The interactive form: the file name is typed (no dialog), then the JSON form runs.
struct AskPath {
    id: &'static str,
    name: &'static str,
    msg: &'static str,
}

impl Interactive for AskPath {
    fn name(&self) -> &'static str {
        self.name
    }

    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new(self.msg, Accept::TEXT)
    }

    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Cancel | Input::Enter => Ok(Step::Cancel),
            Input::Text(t) | Input::Keyword(t) => {
                let path = t.trim().trim_matches('"');
                if path.is_empty() {
                    return Ok(Step::Cancel);
                }
                let spec = find_command(self.id).ok_or_else(|| EngineError::UnknownCommand(self.id.into()))?;
                let r = (spec.run)(s, &json!({ "path": path }))?;
                if let Some(m) = r.get("message").and_then(Value::as_str) {
                    s.echo(m);
                }
                Ok(Step::Done)
            }
            _ => Ok(Step::Continue),
        }
    }
}

/// Kernel-free: paths, object choice, prompts.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rhino_files_sit_side_by_side() {
        assert_eq!(with_ext("C:/work/cabinet", "igs"), PathBuf::from("C:/work/cabinet.igs"));
        assert_eq!(with_ext("C:/work/cabinet.IGS", "stp"), PathBuf::from("C:/work/cabinet.stp"));
        assert_eq!(with_ext("cabinet.3dm", "igs"), PathBuf::from("cabinet.igs"));
        assert_eq!(with_ext("v1.2/cab.v2", "stp"), PathBuf::from("v1.2/cab.v2.stp"));
    }

    #[test]
    fn nothing_to_export_and_bad_params_are_errors() {
        let mut s = Session::new();
        for id in ["igesout", "rhinoout", "igesin"] {
            for p in [json!({}), json!({ "path": "  " }), json!({ "path": 5 }), json!(null)] {
                assert!(s.execute(id, &p).is_err(), "{id} {p}");
            }
        }
        // Only text: nothing IGES can carry.
        s.execute("text", &json!({ "insert": [0, 0], "height": 2.5, "text": "x" })).ok();
        assert!(matches!(s.execute("igesout", &json!({ "path": "never.igs" })), Err(EngineError::Other(m)) if m.contains("no 3D solids or curves")));
        assert!(s.execute("igesout", &json!({ "path": "never.igs", "handles": ["zz"] })).is_err());
    }

    #[test]
    fn curves_are_chosen_as_given_selected_or_all() {
        let mut s = Session::new();
        let l = s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
        s.execute("circle", &json!({ "center": [0, 0], "radius": 5 })).unwrap();
        let all = export_set(&s, &json!({})).unwrap();
        assert!(all.0.is_empty() && all.1.len() >= 2, "{:?}", all.1);
        let h = l["handle"].as_str().or_else(|| l["handles"][0].as_str()).unwrap().to_string();
        let one = export_set(&s, &json!({ "handles": [h, h] })).unwrap();
        assert_eq!(one.1.len(), 1, "given once, even twice");
        // An empty list is none given.
        assert_eq!(export_set(&s, &json!({ "handles": [] })).unwrap().1.len(), all.1.len());
    }

    /// Arcs, circles, polylines and splines go out in their own plane: raised, upside down or
    /// standing; lines keep their world points.
    #[test]
    fn planar_curves_go_out_where_they_are() {
        let mut s = Session::new();
        let h = |r: Value| cadcraft_doc::Handle::parse_hex(r["handle"].as_str().or_else(|| r["handles"][0].as_str()).unwrap()).unwrap();
        let c = h(s.execute("circle", &json!({ "center": [5, 2], "radius": 1 })).unwrap());
        let a = h(s.execute("arc", &json!({ "center": [0, 0], "radius": 4, "start": 0, "end": 90 })).unwrap());
        let sp = h(s.execute("spline", &json!({ "fit": [[0, 0], [5, 3], [10, 0]] })).unwrap());
        let l = h(s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap());
        let d = s.doc_mut().unwrap();
        // Upside down (extrusion -Z) at elevation 3: OCS x runs along world -X, z = -3.
        d.modify_entity(c, |e| {
            e.common.extrusion = Vec3::new(0.0, 0.0, -1.0);
            e.kind.set_elevation(3.0);
        })
        .unwrap();
        // Standing in the XZ plane (extrusion -Y).
        d.modify_entity(a, |e| e.common.extrusion = Vec3::new(0.0, -1.0, 0.0)).unwrap();
        d.modify_entity(sp, |e| e.kind.set_elevation(7.0)).unwrap();
        let curve = |h: cadcraft_doc::Handle| export_set(&s, &json!({ "handles": [h.hex()] })).unwrap().1.remove(0);
        match curve(c) {
            Curve::Segs(g) => match g.as_slice() {
                [cadcraft_kernel::Seg::Circle { center, normal, .. }] => {
                    assert!((*center - Vec3::new(-5.0, 2.0, -3.0)).len() < 1e-9, "{center:?}");
                    assert!((*normal - Vec3::new(0.0, 0.0, -1.0)).len() < 1e-9, "{normal:?}");
                }
                g => panic!("{g:?}"),
            },
            c => panic!("{c:?}"),
        }
        let Curve::Segs(g) = curve(a) else { panic!() };
        let [cadcraft_kernel::Seg::Arc { a: p, mid, b: q }] = g.as_slice() else { panic!("{g:?}") };
        for v in [p, mid, q] {
            assert!(v.y.abs() < 1e-9 && (v.len() - 4.0).abs() < 1e-9, "in the XZ plane: {v:?}");
        }
        assert!(mid.z.abs() > 1.0, "the arc stands up: {mid:?}");
        let Curve::Spline { points, .. } = curve(sp) else { panic!() };
        assert!(points.iter().all(|p| (p.z - 7.0).abs() < 1e-9), "at its elevation");
        let Curve::Segs(g) = curve(l) else { panic!() };
        assert_eq!(g, vec![cadcraft_kernel::Seg::Line { a: Vec3::ZERO, b: Vec3::new(10.0, 0.0, 0.0) }]);
    }

    #[test]
    fn closed_polylines_come_in_closed() {
        let v = Vec3::new;
        let there_and_back = vec![v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 0.0, 0.0)];
        assert!(matches!(polyline(there_and_back), Some(EntityKind::Polyline3d(p)) if !p.closed && p.points.len() == 3), "too few to close");
        let tri = vec![v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(1.0, 1.0, 0.0), v(0.0, 0.0, 0.0)];
        assert!(matches!(polyline(tri), Some(EntityKind::Polyline3d(p)) if p.closed && p.points.len() == 3));
        let sq = vec![v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(1.0, 1.0, 0.0), v(0.0, 1.0, 0.0), v(0.0, 0.0, 0.0)];
        assert!(matches!(polyline(sq), Some(EntityKind::Polyline3d(p)) if p.closed && p.points.len() == 4));
        assert!(polyline(vec![v(0.0, 0.0, 0.0)]).is_none());
        assert!(polyline(vec![v(0.0, 0.0, 0.0), v(f64::NAN, 0.0, 0.0)]).is_none());
    }

    #[test]
    fn the_file_name_is_asked_for() {
        let mut s = Session::new();
        s.start("igesin").unwrap();
        assert!(s.prompt_text().contains("IGES file"), "{}", s.prompt_text());
        s.input(Input::Point(cadcraft_geom::Vec2::new(1.0, 2.0))).unwrap();
        assert!(s.running.is_some(), "a point is not a file name");
        s.input(Input::Enter).unwrap();
        assert!(s.running.is_none());
    }
}
