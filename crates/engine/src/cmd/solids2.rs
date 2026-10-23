//! More 3D modelling (CAD-rhino): WEDGE, PYRAMID, LOFT, SWEEP, PLANESURF, SLICE, and the 3D transforms 3DMOVE, 3DROTATE, MIRROR3D and 3DSCALE. Shapes come from [`cadcraft_kernel`] as in `solids`; a surface is a [`Solid3d`] whose
//! mesh is open. As AutoCAD's default DELOBJ does, the curves a solid is made from are erased and
//! those a surface is made from stay.

use std::collections::HashSet;

use cadcraft_doc::{Entity, EntityKind, Handle, Solid3d};
use cadcraft_geom::{EPS, Mat4, Vec3};
use cadcraft_kernel::{Built, Curve, FaceInfo, MAX_PYRAMID_SIDES, Placed, Primitive, Seg, SurfaceKind};
use serde_json::{Value, json};

use super::curves::{KW, handle_param, is_locked, pick_at};
use super::machines::{SelOutcome, SelectPhase};
use super::solids::{
    Ask, BOX, Q, add, all_or_nothing, box_corners, circle_path, curve_of, in_plane, kernel, locked_err, or_skip, p3, p3_req, p3_value, placed,
    reshape, solid, solids,
};
use super::solids3::{Loop, closed_loops, nest, newell};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

// ponytail: base centre, radius and height only; the JSON form takes `sides` and `topRadius`.
// AutoCAD's Edge, Sides, Inscribed and Top radius keywords when people draw pyramids by hand.
const PYRAMID: &[Q] =
    &[Q::Point("center", "Specify center point of base"), Q::Dist("radius", "Specify base radius"), Q::Dist("height", "Specify height")];

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("wedge", "Wedge", run_wedge)
            .menu(&["Draw", "Modeling", "Wedge"])
            .alias(&["we"])
            .params("{corner1: [x,y,z?], corner2: [x,y,z?], height?} | {corner1, length, width, height}: a box cut diagonally, falling along +X")
            .interactive(|_| Ok(Box::new(Ask::new("wedge", "WEDGE", BOX)))),
        CommandSpec::new("pyramid", "Pyramid", run_pyramid)
            .menu(&["Draw", "Modeling", "Pyramid"])
            .alias(&["pyr"])
            .params("{center: [x,y,z?], radius (base, to a vertex), height, sides? (3 to 32, default 4), topRadius? (0 = pointed)}")
            .interactive(|_| Ok(Box::new(Ask::new("pyramid", "PYRAMID", PYRAMID)))),
        CommandSpec::new("loft", "Loft", run_loft)
            .menu(&["Draw", "Modeling", "Loft"])
            .params(
                "{handles: cross sections in order, ruled?: bool, guides?: [handles] (curves through a corner of every section, first to last; the sections need as many corners each) | path?: handle, mode?: \"solid\" | \"surface\"}: \
                 closed sections (circles, closed polylines) make a solid (a surface open at its ends in surface mode), open ones (lines, arcs, open polylines) a surface",
            )
            .interactive(|_| Ok(Box::new(LoftM::default()))),
        CommandSpec::new("sweep", "Sweep", run_sweep)
            .menu(&["Draw", "Modeling", "Sweep"])
            .params("{handles?: profiles, path: handle (line, arc, circle, polyline, spline or helix)}: closed profiles make solids, open ones surfaces")
            .interactive(|_| Ok(Box::new(SweepM::default()))),
        CommandSpec::new("planesurf", "Planar Surface", run_planesurf)
            .menu(&["Draw", "Modeling", "Surfaces", "Planar"])
            .params("{handles?: closed planar curves (one inside another in its plane is a hole)} | {corner1: [x,y,z?], corner2: [x,y]}")
            .interactive(|_| Ok(Box::new(PlaneSurfM::default()))),
        CommandSpec::new("slice", "Slice", run_slice)
            .menu(&["Modify", "3D Operations", "Slice"])
            .alias(&["sl"])
            .params(
                "{handles?, the cut: point1: [x,y,z?], point2: [x,y,z?], up?: [x,y,z] (the plane holds the line between the points and this direction; default Z: a vertical plane) \
                 | points: [p1,p2,p3] | origin: [x,y,z], normal: [x,y,z] | plane: \"xy\" | \"yz\" | \"zx\", point? | zaxis: [on the plane, along its normal] \
                 | object: handle (a circle, ellipse, arc, 2D spline or 2D polyline: its plane) | view: true, point? | surface: handle, \
                 keep?: \"both\" | \"side\", side?: [x,y,z?] (on the side to keep)}",
            )
            .interactive(|_| Ok(Box::new(SliceM::default()))),
        CommandSpec::new("3dmove", "3D Move", run_3dmove)
            .menu(&["Modify", "3D Operations", "3D Move"])
            .alias(&["3m"])
            .params("{handles?, delta: [dx,dy,dz]}")
            .interactive(|_| Ok(Box::new(Xform3M::new(Op3::Move)))),
        CommandSpec::new("3drotate", "3D Rotate", run_3drotate)
            .menu(&["Modify", "3D Operations", "3D Rotate"])
            .alias(&["3r"])
            .params("{handles?, axis: [[x,y,z],[x,y,z]], angle (degrees, counterclockwise looking from the second point toward the first)}")
            .interactive(|_| Ok(Box::new(Xform3M::new(Op3::Rotate)))),
        CommandSpec::new("mirror3d", "3D Mirror", run_mirror3d)
            .menu(&["Modify", "3D Operations", "3D Mirror"])
            .params(
                "{handles?, plane: \"xy\" | \"yz\" | \"zx\", point?: [x,y,z] (on the plane; default the origin) | points: [p1,p2,p3], erase?: bool (default false: keep the source)}",
            )
            .interactive(|_| Ok(Box::new(Xform3M::new(Op3::Mirror)))),
        CommandSpec::new("3dscale", "3D Scale", run_3dscale)
            .menu(&["Modify", "3D Operations", "3D Scale"])
            .params("{handles?, base: [x,y,z], factor}")
            .interactive(|_| Ok(Box::new(Xform3M::new(Op3::Scale)))),
    ]
}

fn run_wedge(s: &mut Session, p: &Value) -> Result<Value> {
    let (a, b) = box_corners("wedge", p)?;
    add(s, cadcraft_kernel::primitive(&Primitive::Wedge { a, b }).map_err(kernel)?)
}

fn run_pyramid(s: &mut Session, p: &Value) -> Result<Value> {
    let base = p3_req("pyramid", p, "center")?;
    let sides = match p.get("sides") {
        None | Some(Value::Null) => 4,
        Some(v) => v
            .as_f64()
            .and_then(|x| (3..=MAX_PYRAMID_SIDES).find(|n| f64::from(*n) == x))
            .ok_or_else(|| bad("pyramid", format!("`sides` must be a whole number from 3 to {MAX_PYRAMID_SIDES}")))?,
    };
    let (radius, height) = (f64_req("pyramid", p, "radius")?, f64_req("pyramid", p, "height")?);
    let top_radius = f64_or(p, "topRadius", 0.0);
    add(s, cadcraft_kernel::primitive(&Primitive::Pyramid { base, sides, radius, top_radius, height }).map_err(kernel)?)
}

/// `hs` without repeats, in order.
pub(super) fn once(hs: Vec<Handle>) -> Vec<Handle> {
    let mut seen = HashSet::with_capacity(hs.len());
    hs.into_iter().filter(|h| seen.insert(*h)).collect()
}

pub(super) fn hexes(hs: &[Handle]) -> Vec<String> {
    hs.iter().map(|h| h.hex()).collect()
}

/// The shape made from `from`, added in its space with its layer and other properties (and its
/// part, when `from` is a solid).
pub(super) fn add_like(s: &mut Session, from: Handle, b: Built) -> Result<Handle> {
    let d = s.doc_mut()?;
    let (Some(mut e), Some(space)) = (d.entity(from).map(|e| (**e).clone()), d.space_of(from)) else {
        return Err(EngineError::Other("no such object".into()));
    };
    reshape(&mut e, b);
    Ok(d.add(&space, e.common, e.kind)?)
}

pub(super) fn type_name(s: &Session, h: Handle) -> String {
    s.doc().ok().and_then(|d| d.entity(h).map(|e| e.kind.type_name().to_lowercase())).unwrap_or_else(|| "object".into())
}

/// `key`'s object handles (none when it is left out).
pub(super) fn handle_list(cmd: &str, p: &Value, key: &str) -> Result<Vec<Handle>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .and_then(Handle::parse_hex)
                    .ok_or_else(|| bad(cmd, format!("`{key}` holds object handles (hex strings such as \"1A2\"), not {v}")))
            })
            .collect(),
        Some(v) => Err(bad(cmd, format!("`{key}` must be an array of object handles, not {v}"))),
    }
}

/// An optional handle `key`: `None` when left out, an error when it isn't a handle.
pub(super) fn handle_opt(cmd: &str, p: &Value, key: &str) -> Result<Option<Handle>> {
    match p.get(key).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => handle_param(p, key).map(Some).ok_or_else(|| bad(cmd, format!("`{key}` must be an object handle, not {v}"))),
    }
}

/// LOFT: every section is used; those on locked layers are only read, never erased. One or two
/// guide curves or a path steer it. In surface mode closed sections make a surface open at its
/// ends; otherwise a solid, and then the sections, guides and path are erased (AutoCAD's default
/// DELOBJ).
/// ponytail: no POint or Join multiple edges options, and the Settings are Ruled or Smooth fit
/// only (no Normal to or Draft angles: the kernel's lofts take no end conditions).
fn run_loft(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "loft";
    let hs = once(targets(s, p)?);
    let ruled = bool_or(p, "ruled", false);
    let surface_mode = match p.get("mode") {
        None | Some(Value::Null) => false,
        Some(Value::String(m)) if m.eq_ignore_ascii_case("solid") => false,
        Some(Value::String(m)) if m.eq_ignore_ascii_case("surface") => true,
        Some(m) => return Err(bad(CMD, format!("`mode` is \"solid\" or \"surface\", not {m}"))),
    };
    let guides = once(handle_list(CMD, p, "guides")?);
    let path = handle_opt(CMD, p, "path")?;
    if path.is_some() && !guides.is_empty() {
        return Err(bad(CMD, "give `guides` or `path`, not both"));
    }
    let steer: Vec<Handle> = guides.iter().copied().chain(path).collect();
    let curve = |s: &Session, h: Handle, what: &str| -> Result<Curve> {
        let e = s.doc()?.entity(h).ok_or_else(|| bad(CMD, format!("the {what} object does not exist")))?;
        path_of(e)?.ok_or_else(|| bad(CMD, format!("a {} can't be a {what}: use a line, arc, circle, polyline, spline or helix", type_name(s, h))))
    };
    let (mut sections, mut used) = (Vec::new(), Vec::new());
    for h in hs.into_iter().filter(|h| !steer.contains(h)) {
        let Some(e) = s.doc()?.entity(h) else { continue };
        let Some(c) = curve_of(e)? else {
            return Err(bad(CMD, format!("a {} can't be a cross section: use lines, arcs, circles, polylines, splines or helices", type_name(s, h))));
        };
        sections.push(c);
        used.push(h);
    }
    if used.len() < 2 {
        return Err(bad(CMD, "select at least two cross sections"));
    }
    let b = match path {
        Some(ph) => cadcraft_kernel::loft_along(&sections, &curve(s, ph, "path")?),
        None if !guides.is_empty() => {
            let gs = guides.iter().map(|g| curve(s, *g, "guide")).collect::<Result<Vec<_>>>()?;
            cadcraft_kernel::loft_guided(&sections, &gs)
        }
        None => cadcraft_kernel::loft(&sections, ruled),
    }
    .map_err(kernel)?;
    let b = if surface_mode && !b.mesh.open { uncap(&b, &sections)? } else { b };
    let surface = b.mesh.open;
    let (locked, free): (Vec<Handle>, Vec<Handle>) = used.iter().chain(&steer).partition(|u| is_locked(s, **u));
    let locked = locked.iter().filter(|h| used.contains(h)).count();
    // On the current layer, as AutoCAD's LOFT: the sections' layers may be locked. Made where
    // the sections are in the world, not placed in the construction plane again.
    let h = s.add_entity_world(solid(b))?;
    if !surface {
        let d = s.doc_mut()?;
        for u in &free {
            d.remove_entity(*u);
        }
    }
    Ok(json!({ "handle": h.hex(), "surface": surface, "sections": used.len(), "locked": locked }))
}

/// LOFT's surface mode: the faces of a loft solid round its sides as a surface, without the flat
/// end caps lying in the first and last sections' planes.
fn uncap(b: &Built, sections: &[Curve]) -> Result<Built> {
    let ends: Vec<(Vec3, Vec3)> = [sections.first(), sections.last()].into_iter().flatten().filter_map(section_plane).collect();
    let id = Mat4::IDENTITY;
    let at = Placed { brep: &b.brep, xform: &id };
    let topo = cadcraft_kernel::topology(at).map_err(kernel)?;
    let sides: Vec<u32> = (0u32..).zip(&topo.faces).filter(|(_, f)| !is_cap(f, &ends)).map(|(i, _)| i).collect();
    let faces = cadcraft_kernel::copy_faces(at, &sides).map_err(kernel)?;
    let shapes: Vec<Placed> = faces.iter().map(|f| Placed { brep: &f.brep, xform: &id }).collect();
    cadcraft_kernel::sew(&shapes, false).map_err(kernel)
}

/// A flat face in one of the `ends` planes (point, unit normal).
fn is_cap(f: &FaceInfo, ends: &[(Vec3, Vec3)]) -> bool {
    let (o, n) = f.axis;
    let tol = 1e-6 * (1.0 + (f.bbox.1 - f.bbox.0).len());
    f.kind == SurfaceKind::Plane && ends.iter().any(|(p, m)| n.normalized().cross(*m).len() < 1e-6 && (o - *p).dot(*m).abs() < tol)
}

/// A flat section's plane: a point on it and its unit normal.
fn section_plane(c: &Curve) -> Option<(Vec3, Vec3)> {
    if let Curve::Segs(v) = c
        && let [Seg::Circle { center, normal, .. }] = v.as_slice()
    {
        return Some((*center, normal.normalized()));
    }
    let pts = c.points();
    let n = newell(&pts);
    (n.len() > 1e-12).then(|| (pts.first().copied().unwrap_or_default(), n.normalized()))
}

/// A curve object as a sweep path, where it is: a circle runs from its rightmost point (in its
/// own plane).
pub(super) fn path_of(e: &Entity) -> Result<Option<Curve>> {
    match &e.kind {
        EntityKind::Circle(c) => Ok(Some(in_plane(e, circle_path(c.center, c.radius)))),
        _ => curve_of(e),
    }
}

/// ponytail: the profile goes square to the path at its box centre; AutoCAD's Alignment, Base
/// point, Scale and Twist options when sweeps need them.
fn run_sweep(s: &mut Session, p: &Value) -> Result<Value> {
    let ph =
        handle_param(p, "path").ok_or_else(|| bad("sweep", "`path` (the handle of a line, arc, circle, polyline, spline or helix) is required"))?;
    let path_e = s.doc()?.entity(ph).ok_or_else(|| bad("sweep", "the path object does not exist"))?;
    let path = path_of(path_e)?
        .ok_or_else(|| bad("sweep", format!("a {} can't be a path: use a line, arc, circle, polyline, spline or helix", type_name(s, ph))))?;
    let hs: Vec<Handle> = once(targets(s, p)?).into_iter().filter(|h| *h != ph).collect();
    // A degenerate or unsuitable profile is skipped; with nothing made, its reason is the error.
    let (mut built, mut skipped, mut locked, mut why) = (Vec::new(), 0usize, 0usize, None);
    for h in hs {
        let Some(e) = s.doc()?.entity(h) else { continue };
        if is_locked(s, h) {
            locked += 1;
            continue;
        }
        let shape = match curve_of(e) {
            Ok(Some(c)) => or_skip(cadcraft_kernel::sweep(&c, &path), &mut why)?,
            Ok(None) => None,
            Err(e) => {
                why.get_or_insert(e);
                None
            }
        };
        match shape {
            Some(b) => built.push((h, b)),
            None => skipped += 1,
        }
    }
    if built.is_empty() {
        return Err(why.unwrap_or_else(|| {
            if locked > 0 { locked_err() } else { bad("sweep", "select curves to sweep (lines, arcs, circles, polylines, splines, helices)") }
        }));
    }
    let all_solid = built.iter().all(|(_, b)| !b.mesh.open);
    let mut made = Vec::with_capacity(built.len());
    for (h, b) in built {
        let surface = b.mesh.open;
        made.push(add_like(s, h, b)?.hex());
        if !surface {
            s.doc_mut()?.remove_entity(h);
        }
    }
    // The path goes too when only solids were made from it.
    if all_solid && !is_locked(s, ph) {
        s.doc_mut()?.remove_entity(ph);
    }
    Ok(json!({ "handles": made, "skipped": skipped, "locked": locked }))
}

/// One surface for each closed curve (a region: for each outer loop); a curve inside another in
/// its plane is a hole in it, and one inside a hole an island again ([`nest`]).
fn run_planesurf(s: &mut Session, p: &Value) -> Result<Value> {
    // Corners given (even malformed ones) are the corners form: never the selection instead.
    if p.get("corner1").is_some_and(|v| !v.is_null()) {
        let a = p3_req("planesurf", p, "corner1")?;
        let c = point_req("planesurf", p, "corner2")?;
        let at = |x: f64, y: f64| Vec3::new(x, y, a.z);
        let pts = [at(a.x, a.y), at(c.x, a.y), at(c.x, c.y), at(a.x, c.y)];
        let segs: Vec<Seg> = (0..4).filter_map(|i| Some(Seg::Line { a: *pts.get(i)?, b: *pts.get((i + 1) % 4)? })).collect();
        return add(s, cadcraft_kernel::plane_surface(&segs).map_err(kernel)?);
    }
    let hs = once(targets(s, p)?);
    let (mut loops, mut skipped, mut locked, mut why) = (Vec::new(), 0usize, 0usize, None);
    for h in hs {
        let Some(e) = s.doc()?.entity(h) else { continue };
        if is_locked(s, h) {
            locked += 1;
            continue;
        }
        match closed_loops(e) {
            Ok(ls) if !ls.is_empty() => loops.extend(ls.into_iter().map(|(pts, curve)| Loop { owner: h, pts, curve })),
            Ok(_) => skipped += 1,
            Err(e) => {
                why.get_or_insert(e);
                skipped += 1;
            }
        }
    }
    let mut built = Vec::new();
    for (outer, holes) in nest(&loops) {
        let Some(o) = loops.get(outer) else { continue };
        let holes: Vec<Curve> = holes.iter().filter_map(|i| loops.get(*i)).map(|l| l.curve.clone()).collect();
        match or_skip(cadcraft_kernel::planar_surface(&o.curve, &holes), &mut why)? {
            Some(b) => built.push((o.owner, b)),
            None => skipped += 1,
        }
    }
    if built.is_empty() {
        return Err(why.unwrap_or_else(|| {
            if locked > 0 {
                locked_err()
            } else {
                bad("planesurf", "select closed planar curves (circles, closed polylines, single-loop regions) or give `corner1` and `corner2`")
            }
        }));
    }
    let mut made = Vec::with_capacity(built.len());
    for (h, b) in built {
        made.push(add_like(s, h, b)?.hex());
    }
    Ok(json!({ "handles": made, "skipped": skipped, "locked": locked }))
}

/// What SLICE cuts with: a plane (a point on it, its unit normal) or a surface.
enum Cutter {
    Plane(Vec3, Vec3),
    Surface(Handle, Box<Solid3d>),
}

/// The plane of a circle, ellipse, arc, 2D spline or 2D polyline (SLICE's planar Object).
pub(super) fn planar(e: &Entity) -> Option<Mat4> {
    let flat =
        matches!(e.kind, EntityKind::Arc(_) | EntityKind::Circle(_) | EntityKind::Ellipse(_) | EntityKind::LwPolyline(_) | EntityKind::Spline(_));
    e.plane().filter(|_| flat)
}

/// SLICE's cut from its JSON form.
fn cutter(s: &Session, p: &Value) -> Result<Cutter> {
    const CMD: &str = "slice";
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    let plane = |o: Vec3, n: Vec3| {
        let len = n.len();
        if o.is_finite() && n.is_finite() && len > 1e-12 {
            Ok(Cutter::Plane(o, n * (1.0 / len)))
        } else {
            Err(bad(CMD, "the cutting plane's normal has no length"))
        }
    };
    let point_or_origin = || if given("point") { p3_req(CMD, p, "point") } else { Ok(Vec3::ZERO) };
    if let Some(h) = handle_opt(CMD, p, "surface")? {
        let Some((_, x)) = solids(s, &[h])?.into_iter().next() else { return Err(bad(CMD, "`surface` must be a surface or a 3D solid")) };
        return Ok(Cutter::Surface(h, Box::new(x)));
    }
    if let Some(h) = handle_opt(CMD, p, "object")? {
        let e = s.doc()?.entity(h).ok_or_else(|| bad(CMD, "the planar object does not exist"))?;
        let pl =
            planar(e).ok_or_else(|| bad(CMD, format!("a {} has no plane: use a circle, ellipse, arc, 2D spline or 2D polyline", type_name(s, h))))?;
        return plane(pl.apply(Vec3::ZERO), pl.col(2));
    }
    if given("points") {
        let pts: Option<Vec<Vec3>> =
            p.get("points").and_then(Value::as_array).filter(|a| a.len() == 3).and_then(|a| a.iter().map(p3_value).collect());
        let Some([a, b, c]) = pts.as_deref() else { return Err(bad(CMD, "`points` must be three points [[x,y,z],[x,y,z],[x,y,z]]")) };
        return plane(*a, plane_normal(*a, *b, *c).ok_or_else(|| bad(CMD, "the three points are on one line"))?);
    }
    if given("origin") || given("normal") {
        let n = p3(p, "normal").ok_or_else(|| bad(CMD, "`normal` ([x, y, z]) is required with `origin`"))?;
        return plane(p3_req(CMD, p, "origin")?, n);
    }
    if let Some(v) = p.get("plane").filter(|v| !v.is_null()) {
        let n = match v.as_str().map(str::to_ascii_lowercase).as_deref() {
            Some("xy") => Vec3::Z,
            Some("yz") => Vec3::X,
            Some("zx" | "xz") => Vec3::Y,
            _ => return Err(bad(CMD, format!("`plane` is \"xy\", \"yz\" or \"zx\", not {v}"))),
        };
        return plane(point_or_origin()?, n);
    }
    if given("zaxis") {
        let pts: Option<Vec<Vec3>> = p.get("zaxis").and_then(Value::as_array).filter(|a| a.len() == 2).and_then(|a| a.iter().map(p3_value).collect());
        let Some([a, b]) = pts.as_deref() else { return Err(bad(CMD, "`zaxis` must be two points [[x,y,z],[x,y,z]]")) };
        return plane(*a, *b - *a);
    }
    if given("view") {
        if p.get("view") != Some(&Value::Bool(true)) {
            return Err(bad(CMD, "`view` is true (the plane square to the view direction)"));
        }
        return plane(point_or_origin()?, s.state()?.screen_dir().dir);
    }
    let (a, b) = (p3_req(CMD, p, "point1")?, p3_req(CMD, p, "point2")?);
    if (b - a).len() <= EPS * (1.0 + a.len()) {
        return Err(bad(CMD, "`point1` and `point2` must be different points"));
    }
    let up = match p.get("up").filter(|v| !v.is_null()) {
        Some(v) => p3_value(v).ok_or_else(|| bad(CMD, "`up` must be a direction [x, y, z]"))?,
        None => Vec3::Z,
    };
    // The plane holds the line through the two points and `up`; its normal points left of the
    // line seen from `up`'s end.
    let normal = up.normalized().cross((b - a).normalized());
    if !normal.is_finite() || normal.len() <= 1e-9 {
        return Err(bad(CMD, "`up` must not run along the line between the points"));
    }
    plane(a, normal)
}

/// Which side of the surface `tool` the point `q` is on: along or against the normal of its
/// mesh triangle nearest `q`.
/// ponytail: the nearest triangle by its centre, and a piece's side by its points' average; a
/// point projected onto the surface (the kernel's pick) if a deeply curved cut needs it.
fn surface_side(tool: &Solid3d, q: Vec3) -> Option<bool> {
    let m = &tool.mesh;
    let (_, c, n) = m
        .tris
        .iter()
        .filter_map(|t| m.tri(t))
        .map(|[a, b, c]| {
            let (a, b, c) = (tool.world(a), tool.world(b), tool.world(c));
            let mid = (a + b + c) * (1.0 / 3.0);
            ((mid - q).len(), mid, (b - a).cross(c - a))
        })
        .min_by(|x, y| x.0.total_cmp(&y.0))?;
    Some(n.dot(q - c) > 0.0)
}

/// Where a piece of a solid is, for [`surface_side`]: the average of its mesh points.
fn middle(b: &Built) -> Vec3 {
    let n = b.mesh.verts.len().max(1) as f64;
    b.mesh.verts.iter().fold(Vec3::ZERO, |a, p| a + *p) * (1.0 / n)
}

/// SLICE by a plane or a surface, keeping both sides or the side a point is on (the solid keeps
/// the piece on the plane normal's side, or the surface's first piece; the others are new).
fn run_slice(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let cut_by = cutter(s, p)?;
    let keep = match p.get("keep") {
        None | Some(Value::Null) => "both".to_string(),
        Some(Value::String(k)) => k.to_ascii_lowercase(),
        Some(k) => return Err(bad("slice", format!("`keep` is \"both\" or \"side\", not {k}"))),
    };
    // The side to keep: along the plane's normal or not (or the surface's normal there).
    let keep_side = match keep.as_str() {
        "both" => None,
        "side" => {
            let side = p3_req("slice", p, "side")?;
            Some(match &cut_by {
                Cutter::Plane(a, normal) => {
                    let off = normal.dot(side - *a);
                    if off.abs() <= EPS * (1.0 + (side - *a).len()) {
                        return Err(bad("slice", "the `side` point is on the cutting plane"));
                    }
                    off > 0.0
                }
                Cutter::Surface(_, tool) => surface_side(tool, side).ok_or_else(|| bad("slice", "the surface has no faces"))?,
            })
        }
        k => return Err(bad("slice", format!("`keep` is \"both\" or \"side\", not \"{k}\""))),
    };
    let mut xs = solids(s, &hs)?;
    if let Cutter::Surface(t, _) = &cut_by {
        xs.retain(|(h, _)| h != t);
    }
    let all = xs.len();
    xs.retain(|(h, _)| !is_locked(s, *h));
    if xs.is_empty() {
        return Err(if all > 0 { locked_err() } else { bad("slice", "select 3D solids") });
    }
    // Every solid first, as the pieces to keep; one the cut misses stays as it is.
    let mut cut = Vec::new();
    for (h, x) in &xs {
        let pieces = match &cut_by {
            Cutter::Plane(a, normal) => match cadcraft_kernel::slice(placed(x), *a, *normal).map_err(kernel)? {
                [Some(plus), Some(minus)] => match keep_side {
                    Some(true) => vec![plus],
                    Some(false) => vec![minus],
                    None => vec![plus, minus],
                },
                _ => continue,
            },
            Cutter::Surface(_, tool) => {
                let all = cadcraft_kernel::split(placed(x), &[placed(tool)]).map_err(kernel)?;
                if all.len() < 2 {
                    continue;
                }
                match keep_side {
                    Some(want) => all.into_iter().filter(|b| surface_side(tool, middle(b)) == Some(want)).collect(),
                    None => all,
                }
            }
        };
        if !pieces.is_empty() {
            cut.push((*h, pieces));
        }
    }
    if cut.is_empty() {
        return Err(EngineError::Other(
            if matches!(cut_by, Cutter::Plane(..)) {
                "The plane does not cut the selected solids."
            } else {
                "The surface does not cut the selected solids."
            }
            .into(),
        ));
    }
    let n = cut.len();
    let mut made = Vec::new();
    for (h, pieces) in cut {
        let mut pieces = pieces.into_iter();
        let Some(first) = pieces.next() else { continue };
        for o in pieces {
            made.push(add_like(s, h, o)?.hex());
        }
        s.doc_mut()?.modify_entity(h, |e| reshape(e, first))?;
        made.push(h.hex());
    }
    Ok(json!({ "sliced": n, "missed": xs.len() - n, "handles": made, "locked": all - xs.len() }))
}

/// Runs a command's JSON form from its prompts, and clears what was picked for it.
pub(super) fn run_json(s: &mut Session, id: &str, p: Value) -> Result<Step> {
    let spec = find_command(id).ok_or_else(|| EngineError::UnknownCommand(id.into()))?;
    let r = (spec.run)(s, &p)?;
    if let Some(m) = r.get("message").and_then(Value::as_str).filter(|m| !m.is_empty()) {
        s.echo(m.to_string());
    }
    s.set_selection(Vec::new());
    Ok(Step::Done)
}

/// A selection phase's input; the objects land in `hs` when it finishes.
pub(super) fn feed(sel: &mut SelectPhase, hs: &mut Vec<Handle>, s: &mut Session, i: &Input) -> Result<Step> {
    Ok(match sel.feed(s, i)? {
        SelOutcome::More => Step::Continue,
        SelOutcome::Empty => Step::Done,
        SelOutcome::Done(picked) => {
            *hs = picked;
            Step::Continue
        }
    })
}

/// Typed text that starts `word` (a keyword's shortcut or more of it).
pub(super) fn is_kw(t: &str, word: &str) -> bool {
    let t = t.trim().to_ascii_lowercase();
    !t.is_empty() && word.starts_with(&t)
}

/// What LOFT asks after the cross sections.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LoftNext {
    Guides,
    Path,
    Settings,
}

/// LOFT: cross sections in lofting order (MOde: closed sections make solids or surfaces), then
/// Guides, Path, Cross sections only or Settings (Ruled or Smooth fit).
#[derive(Default)]
struct LoftM {
    sel: SelectPhase,
    hs: Vec<Handle>,
    surface: bool,
    asking_mode: bool,
    next: Option<LoftNext>,
    guides: SelectPhase,
}

impl LoftM {
    fn run(&self, s: &mut Session, mut p: Value) -> Result<Step> {
        p["handles"] = json!(hexes(&self.hs));
        p["mode"] = json!(if self.surface { "surface" } else { "solid" });
        run_json(s, "loft", p)
    }
}

impl Interactive for LoftM {
    fn name(&self) -> &'static str {
        "LOFT"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        s.echo(if self.surface { "Closed profiles creation mode = Surface" } else { "Closed profiles creation mode = Solid" });
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.hs = self.sel.picked.clone();
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.asking_mode {
            return Prompt::new("Closed surfaces or solids mode", KW).kw(&["SOlid", "SUrface"]).default(if self.surface {
                "Surface"
            } else {
                "Solid"
            });
        }
        if self.hs.is_empty() {
            return if self.sel.removing {
                self.sel.prompt()
            } else {
                Prompt::new("Select cross sections in lofting order", Accept::SELECT).kw(&["MOde"])
            };
        }
        match self.next {
            None => Prompt::new("Enter an option", KW).kw(&["Guides", "Path", "Cross sections only", "Settings"]).default("Cross sections only"),
            Some(LoftNext::Guides) if self.guides.removing => self.guides.prompt(),
            Some(LoftNext::Guides) => Prompt::new("Select guide curves", Accept::SELECT),
            Some(LoftNext::Path) => Prompt::new("Select path profile", Accept::POINT),
            Some(LoftNext::Settings) => Prompt::new("Enter an option", KW).kw(&["Ruled", "Smooth"]).default("Smooth"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        let word = match &i {
            Input::Text(t) | Input::Keyword(t) => self.prompt(s).match_keyword(t),
            _ => None,
        };
        if self.asking_mode {
            match (word.as_deref(), &i) {
                (Some("SOlid"), _) => self.surface = false,
                (Some("SUrface"), _) => self.surface = true,
                (_, Input::Enter) => {}
                _ => return Ok(Step::Continue),
            }
            self.asking_mode = false;
            return Ok(Step::Continue);
        }
        if self.hs.is_empty() {
            if word.as_deref() == Some("MOde") {
                self.asking_mode = true;
                return Ok(Step::Continue);
            }
            return feed(&mut self.sel, &mut self.hs, s, &i);
        }
        match (self.next, word.as_deref(), &i) {
            (None, Some("Cross sections only"), _) | (None, _, Input::Enter) => self.run(s, json!({})),
            (None, Some("Guides"), _) => {
                self.next = Some(LoftNext::Guides);
                Ok(Step::Continue)
            }
            (None, Some("Path"), _) => {
                self.next = Some(LoftNext::Path);
                Ok(Step::Continue)
            }
            (None, Some("Settings"), _) => {
                self.next = Some(LoftNext::Settings);
                Ok(Step::Continue)
            }
            (None, ..) => {
                s.echo("Requires Guides, Path, Cross sections only or Settings.");
                Ok(Step::Continue)
            }
            (Some(LoftNext::Guides), ..) => match self.guides.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Done),
                SelOutcome::Done(gs) => self.run(s, json!({ "guides": hexes(&gs) })),
            },
            (Some(LoftNext::Path), ..) => {
                let path = match &i {
                    Input::Point(p) => pick_at(s, *p),
                    Input::Pick(hs) => hs.first().copied(),
                    _ => return Ok(Step::Continue),
                };
                let Some(path) = path else {
                    s.echo("No object found.");
                    return Ok(Step::Continue);
                };
                self.run(s, json!({ "path": path.hex() }))
            }
            (Some(LoftNext::Settings), Some("Ruled"), _) => self.run(s, json!({ "ruled": true })),
            (Some(LoftNext::Settings), Some("Smooth"), _) | (Some(LoftNext::Settings), _, Input::Enter) => self.run(s, json!({ "ruled": false })),
            (Some(LoftNext::Settings), ..) => {
                s.echo("Requires Ruled or Smooth.");
                Ok(Step::Continue)
            }
        }
    }
}

/// SWEEP: the objects to sweep, then the path.
#[derive(Default)]
struct SweepM {
    sel: SelectPhase,
    hs: Vec<Handle>,
}

impl Interactive for SweepM {
    fn name(&self) -> &'static str {
        "SWEEP"
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
            (true, false) => Prompt::new("Select objects to sweep", Accept::SELECT),
            (true, true) => self.sel.prompt(),
            (false, _) => Prompt::new("Select sweep path", Accept::POINT),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_empty() {
            return feed(&mut self.sel, &mut self.hs, s, &i);
        }
        let path = match &i {
            Input::Point(p) => pick_at(s, *p),
            Input::Pick(hs) => hs.first().copied(),
            _ => return Ok(Step::Continue),
        };
        let Some(path) = path else {
            s.echo("No object found.");
            return Ok(Step::Continue);
        };
        run_json(s, "sweep", json!({ "handles": hexes(&self.hs), "path": path.hex() }))
    }
}

/// PLANESURF: two corners, or Object and closed curves.
#[derive(Default)]
struct PlaneSurfM {
    corner: Option<Vec3>,
    objects: Option<SelectPhase>,
}

impl PlaneSurfM {
    fn objects(&mut self, s: &mut Session, hs: Vec<Handle>) -> Result<Step> {
        run_json(s, "planesurf", json!({ "handles": hexes(&hs) }))
    }
    fn object_option(&mut self, s: &mut Session) -> Result<Step> {
        let sel = SelectPhase::begin(s);
        if sel.done {
            return self.objects(s, sel.picked);
        }
        self.objects = Some(sel);
        Ok(Step::Continue)
    }
}

impl Interactive for PlaneSurfM {
    fn name(&self) -> &'static str {
        "PLANESURF"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match (&self.objects, self.corner) {
            (Some(sel), _) => sel.prompt(),
            (None, Some(c)) => Prompt::new("Specify other corner", Accept::POINT).corner(c.xy()),
            (None, None) => Prompt::new("Specify first corner", Accept::POINT).kw(&["Object"]).default("Object"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if let Some(sel) = self.objects.as_mut() {
            return match sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Done),
                SelOutcome::Done(hs) => self.objects(s, hs),
            };
        }
        match (self.corner, i) {
            // A first corner snapped to a 3D object sets the surface's height.
            (None, Input::Point(p)) => {
                self.corner = Some(p.to3(s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0)));
                Ok(Step::Continue)
            }
            (Some(c), Input::Point(p)) => run_json(s, "planesurf", json!({ "corner1": [c.x, c.y, c.z], "corner2": [p.x, p.y] })),
            (None, Input::Enter) => self.object_option(s),
            (None, Input::Text(t) | Input::Keyword(t)) if is_kw(&t, "object") => self.object_option(s),
            _ => Ok(Step::Continue),
        }
    }
}

/// How SLICE's plane is being given.
#[derive(Clone, Copy, PartialEq)]
enum SliceBy {
    /// A start point picked: a second point, the plane standing square to the construction plane.
    TwoPoints,
    ThreePoints,
    Object,
    Surface,
    Zaxis,
    View,
    /// The construction plane's XY, YZ or ZX plane through a point: its name and normal.
    Named(&'static str, Vec3),
}

/// SLICE: the solids; the cutting plane (two or three points, a planar object, a surface, a point
/// and the normal's direction, the view plane, or the XY/YZ/ZX plane through a point); then the
/// side to keep or both. Points are in the construction plane, at the height of the 3D object
/// they snap to or typed as `x,y,z`.
#[derive(Default)]
struct SliceM {
    sel: SelectPhase,
    hs: Vec<Handle>,
    by: Option<SliceBy>,
    pts: Vec<Vec3>,
    /// The cut, once given, as the JSON form's keys (in the world).
    cut: Option<Value>,
}

impl SliceM {
    fn run(&self, s: &mut Session, keep: Value) -> Result<Step> {
        let mut p = json!({ "handles": hexes(&self.hs), "keep": "both" });
        for v in [self.cut.as_ref(), Some(&keep)].into_iter().flatten() {
            if let (Some(o), Some(k)) = (p.as_object_mut(), v.as_object()) {
                o.extend(k.clone());
            }
        }
        run_json(s, "slice", p)
    }
    /// The cut from the points given, once there are enough of them.
    fn cut_from_points(&self, s: &Session) -> Result<Option<Value>> {
        let c = s.cplane();
        let w = |p: &Vec3| json3(c.apply(*p));
        Ok(match (self.by, self.pts.as_slice()) {
            (Some(SliceBy::TwoPoints), [a, b]) => Some(json!({ "point1": w(a), "point2": w(b), "up": json3(c.apply_vec(Vec3::Z)) })),
            (Some(SliceBy::ThreePoints), [a, b, d]) => Some(json!({ "points": [w(a), w(b), w(d)] })),
            (Some(SliceBy::Zaxis), [a, b]) => Some(json!({ "origin": w(a), "normal": json3(c.apply_vec(*b - *a)) })),
            (Some(SliceBy::View), [a]) => Some(json!({ "origin": w(a), "normal": json3(s.state()?.screen_dir().dir) })),
            (Some(SliceBy::Named(_, n)), [a]) => Some(json!({ "origin": w(a), "normal": json3(c.apply_vec(n)) })),
            _ => None,
        })
    }
}

impl Interactive for SliceM {
    fn name(&self) -> &'static str {
        "SLICE"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.hs = self.sel.picked.clone();
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.hs.is_empty() {
            return if self.sel.removing { self.sel.prompt() } else { Prompt::new("Select objects to slice", Accept::SELECT) };
        }
        if self.cut.is_some() {
            return Prompt::new("Specify a point on desired side", Accept::POINT).kw(&["keep Both sides"]).default("Both");
        }
        let base = self.pts.first().map(|p| p.xy());
        match (self.by, self.pts.len()) {
            (None, _) => Prompt::new("Specify start point of slicing plane", Accept::POINT)
                .kw(&["planar Object", "Surface", "Zaxis", "View", "XY", "YZ", "ZX", "3points"])
                .default("3points"),
            (Some(SliceBy::TwoPoints), _) => Prompt::new("Specify second point on plane", Accept::POINT).base_opt(base),
            (Some(SliceBy::ThreePoints), 0) => Prompt::new("Specify first point on plane", Accept::POINT),
            (Some(SliceBy::ThreePoints), 1) => Prompt::new("Specify second point on plane", Accept::POINT).base_opt(base),
            (Some(SliceBy::ThreePoints), _) => Prompt::new("Specify third point on plane", Accept::POINT).base_opt(base),
            (Some(SliceBy::Object), _) => {
                Prompt::new("Select a circle, ellipse, arc, 2D-spline, 2D-polyline to define the slicing plane", Accept::POINT)
            }
            (Some(SliceBy::Surface), _) => Prompt::new("Select a surface", Accept::POINT),
            (Some(SliceBy::Zaxis), 0) => Prompt::new("Specify a point on the section plane", Accept::POINT),
            (Some(SliceBy::Zaxis), _) => Prompt::new("Specify a point on the Z-axis (normal) of the plane", Accept::POINT).base_opt(base),
            (Some(SliceBy::View), _) => Prompt::new("Specify a point on the current view plane", Accept::POINT).default("0,0,0"),
            (Some(SliceBy::Named(n, _)), _) => Prompt::new(format!("Specify a point on the {n}-plane"), Accept::POINT).default("0,0,0"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_empty() {
            return feed(&mut self.sel, &mut self.hs, s, &i);
        }
        let word = match &i {
            Input::Text(t) | Input::Keyword(t) => self.prompt(s).match_keyword(t),
            _ => None,
        };
        let z = s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0);
        if self.cut.is_some() {
            return match (&i, word.as_deref()) {
                (Input::Point(p), _) => {
                    let side = s.cplane().apply(p.to3(z));
                    self.run(s, json!({ "keep": "side", "side": json3(side) }))
                }
                (Input::Enter, _) | (_, Some("keep Both sides")) => self.run(s, json!({})),
                _ => Ok(Step::Continue),
            };
        }
        match (self.by, &i, word.as_deref()) {
            (None, Input::Point(p), _) => {
                self.by = Some(SliceBy::TwoPoints);
                self.pts.push(p.to3(z));
            }
            (None, Input::Enter, _) | (None, _, Some("3points")) => self.by = Some(SliceBy::ThreePoints),
            (None, _, Some(k)) => {
                self.by = Some(match k {
                    "planar Object" => SliceBy::Object,
                    "Surface" => SliceBy::Surface,
                    "Zaxis" => SliceBy::Zaxis,
                    "View" => SliceBy::View,
                    "XY" => SliceBy::Named("XY", Vec3::Z),
                    "YZ" => SliceBy::Named("YZ", Vec3::X),
                    _ => SliceBy::Named("ZX", Vec3::Y),
                })
            }
            (Some(SliceBy::Object | SliceBy::Surface), Input::Point(_) | Input::Pick(_), _) => {
                let h = match &i {
                    Input::Point(p) => pick_at(s, *p),
                    Input::Pick(hs) => hs.first().copied(),
                    _ => None,
                };
                let Some(h) = h else {
                    s.echo("No object found.");
                    return Ok(Step::Continue);
                };
                let d = s.doc()?;
                self.cut = Some(if self.by == Some(SliceBy::Object) {
                    if d.entity(h).and_then(|e| planar(e)).is_none() {
                        return Err(EngineError::Other("Select a circle, ellipse, arc, 2D spline or 2D polyline.".into()));
                    }
                    json!({ "object": h.hex() })
                } else {
                    if self.hs.contains(&h) || !matches!(d.entity(h).map(|e| &e.kind), Some(EntityKind::Solid3d(_))) {
                        return Err(EngineError::Other("Select a surface that is not being sliced.".into()));
                    }
                    json!({ "surface": h.hex() })
                });
            }
            (Some(SliceBy::View | SliceBy::Named(..)), Input::Enter, _) => self.pts.push(Vec3::ZERO),
            (Some(_), Input::Point(p), _) => {
                let p = p.to3(z);
                if self.pts.iter().any(|q| (p - *q).len() <= EPS) {
                    return Err(EngineError::Other("The plane needs two different points.".into()));
                }
                if let (Some(SliceBy::ThreePoints), [a, b]) = (self.by, self.pts.as_slice())
                    && plane_normal(*a, *b, p).is_none()
                {
                    return Err(EngineError::Other("The three points are on one line: specify a third point off it.".into()));
                }
                self.pts.push(p);
            }
            _ => {}
        }
        if self.cut.is_none() {
            self.cut = self.cut_from_points(s)?;
        }
        Ok(Step::Continue)
    }
}

// ---------------- 3D transforms ----------------

/// Applies `m` to the objects among `hs` (to copies of them with `copy`) through
/// [`cadcraft_doc::Entity::transform3`]: a 3D solid composes it with its transform, lines, 3D
/// polylines and points move their points, planar objects get the moved plane. Objects on locked
/// layers are left alone; one that can't take the transform (a dimension turned out of the
/// horizontal …) refuses the whole command.
pub(super) fn transform3(s: &mut Session, cmd: &str, hs: &[Handle], m: &Mat4, copy: bool) -> Result<Value> {
    if !m.m.iter().flatten().all(|v| v.is_finite()) {
        return Err(bad(cmd, "the transform is out of range"));
    }
    let mut changed = Vec::new();
    let mut locked = 0usize;
    for h in once(hs.to_vec()) {
        let Some(mut e) = s.doc()?.entity(h).map(|e| (**e).clone()) else { continue };
        if is_locked(s, h) {
            locked += 1;
            continue;
        }
        if !e.transform3(m) {
            return Err(EngineError::Other(format!(
                "{}: a {} can't take this transform (it would leave the plane it must stay in, or lose its shape).",
                cmd.to_ascii_uppercase(),
                e.kind.type_name().to_lowercase()
            )));
        }
        changed.push((h, e));
    }
    if changed.is_empty() {
        return Err(if locked > 0 { locked_err() } else { bad(cmd, "select objects") });
    }
    let out = all_or_nothing(s, |s| {
        let mut out = Vec::with_capacity(changed.len());
        let d = s.doc_mut()?;
        for (h, e) in changed {
            if copy {
                let Some(space) = d.space_of(h) else { continue };
                out.push(d.add(&space, e.common, e.kind)?.hex());
            } else {
                d.modify_entity(h, |x| *x = e)?;
                out.push(h.hex());
            }
        }
        Ok(out)
    })?;
    Ok(json!({ "handles": out, "locked": locked }))
}

fn run_3dmove(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let d = p3(p, "delta").ok_or_else(|| bad("3dmove", "`delta` ([dx, dy] or [dx, dy, dz]) is required"))?;
    transform3(s, "3dmove", &hs, &Mat4::translate(d), false)
}

/// Two 3D points from `[[x,y,z],[x,y,z]]`.
pub(super) fn axis_param(cmd: &str, p: &Value) -> Result<(Vec3, Vec3)> {
    let pts: Option<Vec<Vec3>> = p.get("axis").and_then(Value::as_array).filter(|a| a.len() == 2).and_then(|a| a.iter().map(p3_value).collect());
    match pts.as_deref() {
        Some([a, b]) if (*b - *a).len() > EPS * (1.0 + a.len()) => Ok((*a, *b)),
        Some(_) => Err(bad(cmd, "the axis needs two different points")),
        None => Err(bad(cmd, "`axis` ([[x,y,z],[x,y,z]]) is required")),
    }
}

fn run_3drotate(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let (a, b) = axis_param("3drotate", p)?;
    let angle = f64_req("3drotate", p, "angle")?;
    transform3(s, "3drotate", &hs, &Mat4::rotate_about(a, b - a, angle.to_radians()), false)
}

fn run_mirror3d(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "mirror3d";
    let hs = targets(s, p)?;
    let (o, n) = match (p.get("plane").filter(|v| !v.is_null()), p.get("points").filter(|v| !v.is_null())) {
        (Some(v), _) => {
            let n = match v.as_str().map(str::to_ascii_lowercase).as_deref() {
                Some("xy") => Vec3::Z,
                Some("yz") => Vec3::new(1.0, 0.0, 0.0),
                Some("zx" | "xz") => Vec3::new(0.0, 1.0, 0.0),
                _ => return Err(bad(CMD, format!("`plane` is \"xy\", \"yz\" or \"zx\", not {v}"))),
            };
            let o = match p.get("point").filter(|v| !v.is_null()) {
                Some(v) => p3_value(v).ok_or_else(|| bad(CMD, "`point` must be [x, y] or [x, y, z]"))?,
                None => Vec3::ZERO,
            };
            (o, n)
        }
        (None, Some(v)) => {
            let pts: Option<Vec<Vec3>> = v.as_array().filter(|a| a.len() == 3).and_then(|a| a.iter().map(p3_value).collect());
            let Some([a, b, c]) = pts.as_deref() else { return Err(bad(CMD, "`points` must be three points [[x,y,z],[x,y,z],[x,y,z]]")) };
            (*a, plane_normal(*a, *b, *c).ok_or_else(|| bad(CMD, "the three points of the mirror plane are on one line"))?)
        }
        (None, None) => return Err(bad(CMD, "give `plane` (\"xy\", \"yz\" or \"zx\") or `points` (three points)")),
    };
    transform3(s, CMD, &hs, &Mat4::mirror(o, n), !bool_or(p, "erase", false))
}

/// The unit normal of the plane through three points; `None` when they are on one line.
pub(super) fn plane_normal(a: Vec3, b: Vec3, c: Vec3) -> Option<Vec3> {
    let n = (b - a).cross(c - a);
    let reach = (b - a).len().max((c - a).len());
    (n.len() > 1e-12 * reach * reach).then(|| n.normalized())
}

fn run_3dscale(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    let base = p3_req("3dscale", p, "base")?;
    let f = size_param("3dscale", p, "factor", false)?.ok_or_else(|| bad("3dscale", "`factor` (number) is required"))?;
    transform3(s, "3dscale", &hs, &Mat4::translate(base).mul(&Mat4::scale(Vec3::new(f, f, f))).mul(&Mat4::translate(-base)), false)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op3 {
    Move,
    Rotate,
    Mirror,
    Scale,
}

/// 3DMOVE, 3DROTATE, MIRROR3D and 3DSCALE at the command line. Points, typed displacements and
/// the X/Y/Z axes and XY/YZ/ZX planes are in the construction plane (as AutoCAD's are in the
/// UCS); picked points take the height of the 3D object they snap to (`point_z`). The JSON form
/// they run is in the world. A bad value is asked for again at its prompt; when the command fails
/// at the end (an object can't take the transform), it starts again from the selection.
/// ponytail: typed points are on the construction plane (the command line reads `x,y,z` as
/// `x,y`); 3DMOVE's Displacement option takes `dx,dy,dz`, and the axis and plane keywords cover
/// the rest until typed points carry z.
struct Xform3M {
    op: Op3,
    sel: SelectPhase,
    hs: Vec<Handle>,
    pts: Vec<Vec3>,
    /// An axis (3DROTATE) or a plane (MIRROR3D) chosen by keyword: its name and direction.
    named: Option<(&'static str, Vec3)>,
    displacement: bool,
}

impl Xform3M {
    fn new(op: Op3) -> Self {
        Xform3M { op, sel: SelectPhase::default(), hs: Vec::new(), pts: Vec::new(), named: None, displacement: false }
    }
    fn run(&mut self, s: &mut Session, id: &str, mut p: Value) -> Result<Step> {
        p["handles"] = json!(hexes(&self.hs));
        let r = run_json(s, id, p);
        if r.is_err() {
            *self = Xform3M::new(self.op);
        }
        r
    }
    /// The rotation axis once known: two points, or a point and a named axis.
    fn axis(&self) -> Option<(Vec3, Vec3)> {
        match (self.pts.as_slice(), self.named) {
            ([a], Some((_, d))) => Some((*a, *a + d)),
            ([a, b, ..], None) => Some((*a, *b)),
            _ => None,
        }
    }
    /// Whether the mirror plane is known.
    fn plane_known(&self) -> bool {
        (self.named.is_some() && self.pts.len() == 1) || self.pts.len() == 3
    }
}

pub(super) fn json3(p: Vec3) -> Value {
    json!([p.x, p.y, p.z])
}

/// `dx,dy[,dz]` typed.
fn parse3(t: &str) -> Option<Vec3> {
    let parts: Vec<&str> = t.trim().trim_start_matches('@').split(',').collect();
    let n = |i: usize| parts.get(i).and_then(|v| crate::units::parse_distance(v.trim()));
    let v = match parts.len() {
        2 => Vec3::new(n(0)?, n(1)?, 0.0),
        3 => Vec3::new(n(0)?, n(1)?, n(2)?),
        _ => return None,
    };
    v.is_finite().then_some(v)
}

impl Interactive for Xform3M {
    fn name(&self) -> &'static str {
        match self.op {
            Op3::Move => "3DMOVE",
            Op3::Rotate => "3DROTATE",
            Op3::Mirror => "MIRROR3D",
            Op3::Scale => "3DSCALE",
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.hs = self.sel.picked.clone();
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.hs.is_empty() {
            return self.sel.prompt();
        }
        let base = self.pts.first().map(|p| p.xy());
        let name = self.named.map_or("", |n| n.0);
        match (self.op, self.pts.len()) {
            (Op3::Move, _) if self.displacement => Prompt::new("Specify displacement (dx,dy,dz)", Accept::TEXT).default("0,0,0"),
            (Op3::Move, 0) => Prompt::new("Specify base point", Accept::POINT).kw(&["Displacement"]).default("Displacement"),
            (Op3::Move, _) => Prompt::new("Specify second point", Accept::POINT).base_opt(base),
            (Op3::Rotate, _) if self.axis().is_some() => Prompt::new("Specify rotation angle", Accept::NUMBER),
            (Op3::Rotate, 0) if self.named.is_some() => Prompt::new(format!("Specify a point on the {name} axis"), Accept::POINT).default("0,0,0"),
            (Op3::Rotate, 0) => Prompt::new("Specify first point on axis", Accept::POINT).kw(&["Xaxis", "Yaxis", "Zaxis"]),
            (Op3::Rotate, _) => Prompt::new("Specify second point on axis", Accept::POINT).base_opt(base),
            (Op3::Mirror, _) if self.plane_known() => Prompt::new("Delete source objects?", Accept::TEXT).kw(&["Yes", "No"]).default("No"),
            (Op3::Mirror, 0) if self.named.is_some() => Prompt::new(format!("Specify point on {name} plane"), Accept::POINT).default("0,0,0"),
            (Op3::Mirror, 0) => Prompt::new("Specify first point of mirror plane (3 points)", Accept::POINT).kw(&["XY", "YZ", "ZX"]),
            (Op3::Mirror, 1) => Prompt::new("Specify second point on mirror plane", Accept::POINT).base_opt(base),
            (Op3::Mirror, _) => Prompt::new("Specify third point on mirror plane", Accept::POINT).base_opt(base),
            (Op3::Scale, 0) => Prompt::new("Specify base point", Accept::POINT),
            (Op3::Scale, _) => Prompt::new("Specify scale factor", Accept::NUMBER).base_opt(base),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.hs.is_empty() {
            return feed(&mut self.sel, &mut self.hs, s, &i);
        }
        let z = s.point_z.filter(|z| z.is_finite()).unwrap_or(0.0);
        // Construction plane → world, for what the JSON form takes.
        let c = s.cplane();
        let word = match &i {
            Input::Text(t) | Input::Keyword(t) => t.trim().to_ascii_lowercase(),
            _ => String::new(),
        };
        let typed = matches!(i, Input::Text(_) | Input::Keyword(_));
        match (self.op, i) {
            (Op3::Move, i) if self.displacement => match i {
                Input::Enter => Ok(Step::Done),
                Input::Text(t) => {
                    let d = parse3(&t).ok_or_else(|| EngineError::Other("Requires dx,dy or dx,dy,dz.".into()))?;
                    self.run(s, "3dmove", json!({ "delta": json3(c.apply_vec(d)) }))
                }
                _ => Ok(Step::Continue),
            },
            (Op3::Move, Input::Enter) if self.pts.is_empty() => {
                self.displacement = true;
                Ok(Step::Continue)
            }
            (Op3::Move, _) if typed && self.pts.is_empty() && is_kw(&word, "displacement") => {
                self.displacement = true;
                Ok(Step::Continue)
            }
            (Op3::Move, Input::Point(p)) => {
                let p = p.to3(z);
                match self.pts.first() {
                    Some(a) => self.run(s, "3dmove", json!({ "delta": json3(c.apply_vec(p - *a)) })),
                    None => {
                        self.pts.push(p);
                        Ok(Step::Continue)
                    }
                }
            }
            (Op3::Rotate, i) if self.axis().is_some() => {
                let Input::Text(t) = i else { return Ok(Step::Continue) };
                let angle = super::machines::number(&t).ok_or_else(|| EngineError::Other("Requires an angle.".into()))?;
                let (a, b) = self.axis().unwrap_or_default();
                self.run(s, "3drotate", json!({ "axis": [json3(c.apply(a)), json3(c.apply(b))], "angle": angle }))
            }
            (Op3::Rotate | Op3::Mirror, Input::Enter) if self.pts.is_empty() && self.named.is_some() => {
                self.pts.push(Vec3::ZERO);
                Ok(Step::Continue)
            }
            (Op3::Rotate, _) if typed && self.pts.is_empty() => {
                self.named = match word.chars().next() {
                    Some('x') => Some(("X", Vec3::new(1.0, 0.0, 0.0))),
                    Some('y') => Some(("Y", Vec3::new(0.0, 1.0, 0.0))),
                    Some('z') => Some(("Z", Vec3::Z)),
                    _ => self.named,
                };
                Ok(Step::Continue)
            }
            (Op3::Mirror, i) if self.plane_known() => {
                let erase = match i {
                    Input::Enter => false,
                    _ if typed && is_kw(&word, "yes") => true,
                    _ if typed && is_kw(&word, "no") => false,
                    _ => return Ok(Step::Continue),
                };
                let p = match self.named {
                    Some((n, _)) if c == Mat4::IDENTITY => {
                        json!({ "plane": n.to_ascii_lowercase(), "point": json3(self.pts.first().copied().unwrap_or_default()), "erase": erase })
                    }
                    // The construction plane's XY, YZ or ZX plane, by three of its points.
                    Some((_, d)) => {
                        let o = self.pts.first().copied().unwrap_or_default();
                        let pts = [o, o + Vec3::new(d.y, d.z, d.x), o + Vec3::new(d.z, d.x, d.y)];
                        json!({ "points": pts.iter().map(|p| json3(c.apply(*p))).collect::<Vec<_>>(), "erase": erase })
                    }
                    None => json!({ "points": self.pts.iter().map(|p| json3(c.apply(*p))).collect::<Vec<_>>(), "erase": erase }),
                };
                self.run(s, "mirror3d", p)
            }
            (Op3::Mirror, _) if typed && self.pts.is_empty() => {
                self.named = match word.as_str() {
                    "xy" => Some(("XY", Vec3::Z)),
                    "yz" => Some(("YZ", Vec3::new(1.0, 0.0, 0.0))),
                    "zx" | "xz" => Some(("ZX", Vec3::new(0.0, 1.0, 0.0))),
                    _ => self.named,
                };
                Ok(Step::Continue)
            }
            (Op3::Scale, Input::Text(t)) if !self.pts.is_empty() => {
                let f = super::machines::number(&t)
                    .filter(|f| f.is_finite() && *f > 0.0)
                    .ok_or_else(|| EngineError::Other("Requires a positive number.".into()))?;
                let base = self.pts.first().copied().unwrap_or_default();
                self.run(s, "3dscale", json!({ "base": json3(c.apply(base)), "factor": f }))
            }
            (Op3::Scale, Input::Point(_)) if !self.pts.is_empty() => Ok(Step::Continue),
            (_, Input::Point(p)) => {
                let p = p.to3(z);
                if self.pts.iter().any(|q| (p - *q).len() <= EPS) {
                    return Err(EngineError::Other("The points must be different.".into()));
                }
                if let (Op3::Mirror, [a, b]) = (self.op, self.pts.as_slice())
                    && plane_normal(*a, *b, p).is_none()
                {
                    return Err(EngineError::Other("The three points are on one line: specify a third point off it.".into()));
                }
                self.pts.push(p);
                Ok(Step::Continue)
            }
            _ => Ok(Step::Continue),
        }
    }
}

/// Kernel-free checks: parameters, prompts and refusals before any shape is made.
#[cfg(test)]
mod plain_tests {
    use cadcraft_doc::Solid3d;
    use cadcraft_geom::{PolyVertex, Vec2};

    use super::*;

    fn err(r: Result<Value>) -> String {
        r.err().map(|e| e.to_string()).unwrap_or_default()
    }

    fn circle(s: &mut Session, x: f64) -> Handle {
        s.add_entity(super::super::helpers::circle(Vec2::new(x, 0.0), 1.0)).unwrap()
    }

    fn line(s: &mut Session) -> Value {
        s.execute("line", &json!({ "points": [[0, 0], [5, 0]] })).unwrap()["handles"][0].clone()
    }

    #[test]
    fn pyramid_sides_are_whole_numbers_from_3_to_32() {
        let mut s = Session::new();
        for sides in [json!(2), json!(33), json!(4.5), json!("4"), json!(-4), json!(1e300), json!(f64::MAX)] {
            let r = s.execute("pyramid", &json!({ "center": [0, 0], "radius": 5, "height": 3, "sides": sides }));
            assert!(err(r).contains("sides"), "{sides}");
        }
        assert!(s.doc().unwrap().model.iter().next().is_none());
        // Without the kernel the right parameters reach it and come back unavailable.
        let r = s.execute("pyramid", &json!({ "center": [0, 0, 2], "radius": 5, "height": 3, "sides": 32.0, "topRadius": 1 }));
        assert!(r.is_ok() || !err(r).contains("sides"));
    }

    #[test]
    fn hostile_parameters_are_refused() {
        let mut s = Session::new();
        let c = circle(&mut s, 0.0);
        for (id, p) in [
            ("wedge", json!({ "corner1": [0, 0], "corner2": [1e308, -1e308], "height": "x" })),
            ("pyramid", json!({ "center": ["a"], "radius": 5, "height": 1 })),
            ("loft", json!({ "handles": "zz", "ruled": 7 })),
            ("sweep", json!({ "handles": [c.hex()], "path": [1, 2] })),
            ("sweep", json!({ "handles": [c.hex()], "path": "FFFFFFFFFFFFFFFFFF" })),
            ("chamferedge", json!({ "handles": [c.hex()], "distance": null })),
            ("shell", json!({ "handle": -1, "thickness": 1 })),
            ("planesurf", json!({ "corner1": [0, 0], "corner2": "x" })),
            ("slice", json!({ "point1": [0, 0], "point2": [0, 0] })),
            ("slice", json!({ "point1": [0, 0], "point2": [1, 0], "keep": "side" })),
            ("slice", json!({ "point1": [0, 0], "point2": [1, 0], "keep": 3 })),
        ] {
            assert!(s.execute(id, &p).is_err(), "{id} {p}");
        }
        assert_eq!(s.doc().unwrap().model.iter().count(), 1, "the drawing is as it was");
        // The bad value is the error: not the selection, nor a default in its place.
        let x = s.add_entity(EntityKind::Solid3d(Solid3d::default())).unwrap();
        for k in [json!(3), json!(true), json!(["side"]), json!("left")] {
            let r = s.execute("slice", &json!({ "handles": [x.hex()], "point1": [0, 0], "point2": [1, 0], "keep": k }));
            assert!(err(r).contains("`keep`"), "{k}");
        }
        for corner in [json!([0, "y"]), json!([0, 0, "z"]), json!(true)] {
            let r = s.execute("planesurf", &json!({ "corner1": corner, "corner2": [1, 1], "handles": [c.hex()] }));
            assert!(err(r).contains("corner1"), "{corner}");
        }
        assert_eq!(s.doc().unwrap().model.iter().count(), 2);
    }

    #[test]
    fn loft_needs_two_curves_at_different_elevations() {
        let mut s = Session::new();
        let (a, b) = (circle(&mut s, 0.0), circle(&mut s, 10.0));
        assert!(err(s.execute("loft", &json!({ "handles": [a.hex()] }))).contains("two cross sections"));
        assert!(err(s.execute("loft", &json!({ "handles": [a.hex(), a.hex()] }))).contains("two cross sections"), "one section twice");
        // Both on the XY plane: refused by the kernel's checks, which run without the kernel too.
        assert!(err(s.execute("loft", &json!({ "handles": [a.hex(), b.hex()] }))).contains("different elevations"));
        let l = line(&mut s);
        assert!(err(s.execute("loft", &json!({ "handles": [a.hex(), l] }))).contains("all open or all closed"));
        let pt = s.add_entity(EntityKind::Point(cadcraft_doc::Point { p: Vec3::ZERO, angle: 0.0 })).unwrap();
        assert!(err(s.execute("loft", &json!({ "handles": [a.hex(), pt.hex()] }))).contains("can't be a cross section"));
        assert_eq!(s.doc().unwrap().model.iter().count(), 4);
    }

    #[test]
    fn sweep_needs_a_curve_path_and_profiles() {
        let mut s = Session::new();
        let c = circle(&mut s, 0.0);
        let solid = s.add_entity(EntityKind::Solid3d(Solid3d::default())).unwrap();
        assert!(err(s.execute("sweep", &json!({ "handles": [c.hex()] }))).contains("`path`"));
        assert!(err(s.execute("sweep", &json!({ "handles": [c.hex()], "path": solid.hex() }))).contains("can't be a path"));
        let l = line(&mut s);
        // The path alone is not something to sweep.
        assert!(err(s.execute("sweep", &json!({ "handles": [l], "path": l }))).contains("select curves"));
        assert!(err(s.execute("sweep", &json!({ "handles": [solid.hex()], "path": l }))).contains("select curves"));
    }

    #[test]
    fn locked_objects_are_left_alone() {
        let mut s = Session::new();
        let (a, b) = (circle(&mut s, 0.0), circle(&mut s, 5.0));
        let x = s.add_entity(EntityKind::Solid3d(Solid3d::default())).unwrap();
        s.doc_mut().unwrap().layer_mut("0").unwrap().locked = true;
        let hs = json!([a.hex(), b.hex()]);
        // LOFT reads locked sections (never erasing them): here both are at one elevation.
        assert!(err(s.execute("loft", &json!({ "handles": hs }))).contains("different elevations"));
        assert!(err(s.execute("sweep", &json!({ "handles": [a.hex()], "path": b.hex() }))).contains("locked"));
        assert!(err(s.execute("planesurf", &json!({ "handles": hs }))).contains("locked"));
        for (id, p) in [
            ("chamferedge", json!({ "handles": [x.hex()], "distance": 1 })),
            ("shell", json!({ "handles": [x.hex()], "thickness": 1 })),
            ("slice", json!({ "handles": [x.hex()], "point1": [0, 0], "point2": [0, 1] })),
        ] {
            assert!(err(s.execute(id, &p)).contains("locked"), "{id}");
        }
        assert_eq!(s.doc().unwrap().model.iter().count(), 3);
    }

    #[test]
    fn planesurf_takes_closed_curves_only() {
        let mut s = Session::new();
        let l = line(&mut s);
        assert!(err(s.execute("planesurf", &json!({ "handles": [l] }))).contains("closed planar curves"));
        let vs = vec![PolyVertex::new(Vec2::ZERO), PolyVertex::new(Vec2::new(1.0, 0.0)), PolyVertex::new(Vec2::new(1.0, 1.0))];
        let open = s.add_entity(super::super::helpers::lwpoly(vs, false)).unwrap();
        assert!(err(s.execute("planesurf", &json!({ "handles": [open.hex()] }))).contains("closed planar curves"));
    }

    #[test]
    fn surfaces_are_not_chamfered_or_shelled() {
        let mut s = Session::new();
        let mesh = cadcraft_geom::Mesh3 { open: true, ..Default::default() };
        let surf = s.add_entity(EntityKind::Solid3d(Solid3d { mesh: std::sync::Arc::new(mesh), ..Default::default() })).unwrap();
        assert!(err(s.execute("chamferedge", &json!({ "handles": [surf.hex()], "distance": 1 }))).contains("select 3D solids"));
        assert!(err(s.execute("shell", &json!({ "handles": [surf.hex()], "thickness": 1 }))).contains("select 3D solids"));
    }

    #[test]
    fn prompts_follow_autocad() {
        let mut s = Session::new();
        let msg = |s: &Session| s.current_prompt().map(|p| p.message).unwrap_or_default();
        s.start("wedge").unwrap();
        assert!(msg(&s).contains("first corner"));
        s.input(Input::Point(Vec2::ZERO)).unwrap();
        assert!(msg(&s).contains("other corner"));
        s.cancel();
        s.start("pyramid").unwrap();
        assert!(msg(&s).contains("center point of base"));
        s.cancel();
        // LOFT: cross sections, then Guides, Path, Cross sections only or Settings.
        let (a, b) = (circle(&mut s, 0.0), circle(&mut s, 5.0));
        s.start("loft").unwrap();
        assert!(msg(&s).contains("lofting order"));
        s.input(Input::Pick(vec![a, b])).unwrap();
        s.input(Input::Enter).unwrap();
        let p = s.current_prompt().unwrap();
        assert_eq!(p.keywords, vec!["Guides", "Path", "Cross sections only", "Settings"]);
        s.cancel();
        // SWEEP: objects, then the path (nothing picked first: no pickfirst selection).
        s.set_selection(Vec::new());
        s.start("sweep").unwrap();
        s.input(Input::Pick(vec![a])).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(msg(&s).contains("sweep path"));
        s.cancel();
        // SLICE: objects, two points, then the side.
        s.set_selection(Vec::new());
        s.start("slice").unwrap();
        s.input(Input::Pick(vec![a])).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(msg(&s).contains("start point of slicing plane"));
        s.input(Input::Point(Vec2::ZERO)).unwrap();
        let _ = s.input(Input::Point(Vec2::ZERO));
        assert!(msg(&s).contains("second point on plane"), "the same point twice is asked again");
        s.input(Input::Point(Vec2::new(0.0, 1.0))).unwrap();
        assert!(msg(&s).contains("desired side"));
        s.cancel();
        // PLANESURF: a corner, or Object.
        s.set_selection(Vec::new());
        s.start("planesurf").unwrap();
        assert_eq!(s.current_prompt().unwrap().keywords, vec!["Object"]);
        s.input(Input::Text("o".into())).unwrap();
        assert!(msg(&s).contains("Select objects"));
        s.cancel();
    }

    #[test]
    fn a_first_point_on_a_3d_object_sets_the_height() {
        let mut ask = Ask::new("box", "BOX", BOX);
        let mut s = Session::new();
        s.point_z = Some(5.0);
        ask.input(&mut s, Input::Point(Vec2::new(1.0, 2.0))).unwrap();
        // Only the first point: the other corner stays on its plane.
        ask.input(&mut s, Input::Point(Vec2::new(4.0, 6.0))).unwrap();
        assert_eq!(ask.values().get("corner1"), Some(&json!([1.0, 2.0, 5.0])));
        assert_eq!(ask.values().get("corner2"), Some(&json!([4.0, 6.0])));
        let mut plain = Ask::new("sphere", "SPHERE", PYRAMID);
        s.point_z = None;
        plain.input(&mut s, Input::Point(Vec2::new(1.0, 2.0))).unwrap();
        assert_eq!(plain.values().get("center"), Some(&json!([1.0, 2.0])));
    }
}

#[cfg(test)]
#[cfg(feature = "occt")]
mod tests {
    use cadcraft_geom::{Bounds2, PolyVertex, Vec2};

    use super::*;

    fn shapes(s: &Session) -> Vec<Solid3d> {
        s.doc()
            .unwrap()
            .model
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Solid3d(x) => Some(x.clone()),
                _ => None,
            })
            .collect()
    }

    /// World-space height range of a shape's mesh.
    fn heights(x: &Solid3d) -> (f64, f64) {
        x.mesh.verts.iter().map(|p| x.world(*p).z).fold((f64::MAX, f64::MIN), |(lo, hi), z| (lo.min(z), hi.max(z)))
    }

    fn spans(x: &Solid3d, lo: f64, hi: f64) -> bool {
        let (a, b) = heights(x);
        (a - lo).abs() < 1e-6 && (b - hi).abs() < 1e-6
    }

    fn handle(v: &Value) -> Handle {
        Handle::parse_hex(v.as_str().unwrap()).unwrap()
    }

    fn near(b: Bounds2, lo: (f64, f64), hi: (f64, f64)) -> bool {
        near_by(b, lo, hi, 1e-6)
    }

    fn near_by(b: Bounds2, lo: (f64, f64), hi: (f64, f64), tol: f64) -> bool {
        (b.min.x - lo.0).abs() < tol && (b.min.y - lo.1).abs() < tol && (b.max.x - hi.0).abs() < tol && (b.max.y - hi.1).abs() < tol
    }

    fn poly(s: &mut Session, pts: &[(f64, f64, f64)], closed: bool, z: f64) -> Handle {
        let vs = pts.iter().map(|(x, y, b)| PolyVertex::with_bulge(Vec2::new(*x, *y), *b)).collect();
        let EntityKind::LwPolyline(mut p) = super::super::helpers::lwpoly(vs, closed) else { panic!("not a polyline") };
        p.elevation = z;
        s.add_entity(EntityKind::LwPolyline(p)).unwrap()
    }

    fn circle_at(s: &mut Session, x: f64, z: f64, r: f64) -> Handle {
        s.add_entity(EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::new(x, 0.0, z), radius: r })).unwrap()
    }

    fn box3(x: &Solid3d) -> (Vec3, Vec3) {
        x.mesh
            .verts
            .iter()
            .map(|p| x.world(*p))
            .fold((Vec3::new(f64::MAX, f64::MAX, f64::MAX), Vec3::new(f64::MIN, f64::MIN, f64::MIN)), |(lo, hi), p| {
                (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
            })
    }

    fn near3(a: Vec3, b: [f64; 3], tol: f64) -> bool {
        (a.x - b[0]).abs() < tol && (a.y - b[1]).abs() < tol && (a.z - b[2]).abs() < tol
    }

    /// LOFT, PLANESURF and SWEEP take curves standing in their own planes where they are: circles
    /// facing +X (extrusion +X) at elevations 0, 10 and 20, and a sweep path standing in the XZ
    /// plane.
    #[test]
    fn standing_curves_loft_surface_and_sweep_where_they_are() {
        let mut s = Session::new();
        let facing = |s: &mut Session, z: f64, r: f64, n: Vec3| {
            let h = s.add_entity(EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::new(0.0, 0.0, z), radius: r })).unwrap();
            s.doc_mut().unwrap().modify_entity(h, |e| e.common.extrusion = n).unwrap();
            h
        };
        let x = Vec3::new(1.0, 0.0, 0.0);
        let (a, b) = (facing(&mut s, 0.0, 2.0, x), facing(&mut s, 10.0, 1.0, x));
        let r = s.execute("loft", &json!({ "handles": [a.hex(), b.hex()] })).unwrap();
        let (lo, hi) = box3(&shapes(&s)[0]);
        assert!(near3(lo, [0.0, -2.0, -2.0], 0.1) && near3(hi, [10.0, 2.0, 2.0], 0.1), "{r} {lo:?} {hi:?}");
        s.undo().unwrap();
        let c = facing(&mut s, 20.0, 1.0, x);
        let r = s.execute("planesurf", &json!({ "handles": [c.hex()] })).unwrap();
        let surf = shapes(&s).into_iter().next().unwrap();
        assert!(surf.mesh.verts.iter().all(|p| (surf.world(*p).x - 20.0).abs() < 1e-6), "{r}: in the plane x = 20");
        s.undo().unwrap();
        // A path circle of radius 10 standing in the XZ plane, a small profile anywhere: a ring
        // in that plane.
        let path = facing(&mut s, 0.0, 10.0, Vec3::new(0.0, -1.0, 0.0));
        let prof = s.add_entity(EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::new(50.0, 50.0, 0.0), radius: 1.0 })).unwrap();
        s.execute("sweep", &json!({ "handles": [prof.hex()], "path": path.hex() })).unwrap();
        let (lo, hi) = box3(&shapes(&s)[0]);
        assert!(near3(lo, [-11.0, -1.0, -11.0], 0.1) && near3(hi, [11.0, 1.0, 11.0], 0.1), "{lo:?} {hi:?}");
    }

    /// The volume a closed mesh encloses.
    fn volume(x: &Solid3d) -> f64 {
        let m = &x.mesh;
        m.tris.iter().filter_map(|t| m.tri(t)).map(|[a, b, c]| a.dot(b.cross(c)) / 6.0).sum::<f64>().abs()
    }

    #[test]
    fn loft_reads_locked_sections_and_keeps_them() {
        let mut s = Session::new();
        let (a, m, b) = (circle_at(&mut s, 0.0, 0.0, 5.0), circle_at(&mut s, 0.0, 5.0, 8.0), circle_at(&mut s, 0.0, 10.0, 5.0));
        s.doc_mut().unwrap().modify_entity(m, |e| e.common.layer = "Defpoints".into()).unwrap();
        s.doc_mut().unwrap().layer_mut("Defpoints").unwrap().locked = true;
        let r = s.execute("loft", &json!({ "handles": [a.hex(), m.hex(), b.hex()], "ruled": true })).unwrap();
        assert_eq!((r["sections"].as_u64(), r["locked"].as_u64()), (Some(3), Some(1)), "{r}");
        let d = s.doc().unwrap();
        assert!(d.entity(m).is_some() && d.entity(a).is_none() && d.entity(b).is_none(), "only the locked section stays");
        // The locked middle section shapes the loft: its radius is 8 at z = 5.
        let x = &shapes(&s)[0];
        let r5 = x.mesh.verts.iter().filter(|p| (p.z - 5.0).abs() < 1e-6).map(|p| p.xy().len()).fold(0.0, f64::max);
        assert!((r5 - 8.0).abs() < 1e-3, "{r5}");
        // The loft is on the current layer, not a section's (which may be locked).
        let h = r["handle"].as_str().and_then(cadcraft_doc::Handle::parse_hex).unwrap();
        assert_eq!(s.doc().unwrap().entity(h).unwrap().common.layer, "0");
    }

    #[test]
    fn a_degenerate_profile_is_skipped_by_planesurf() {
        let mut s = Session::new();
        let good = circle_at(&mut s, 0.0, 0.0, 5.0);
        let flat = s.execute("line", &json!({ "points": [[20, 0], [20, 0]] })).ok().and_then(|r| r["handles"][0].as_str().map(str::to_owned));
        let mut hs = vec![json!(good.hex())];
        hs.extend(flat.map(Value::from));
        let r = s.execute("planesurf", &json!({ "handles": hs })).unwrap();
        assert_eq!(r["handles"].as_array().map(Vec::len), Some(1), "{r}");
    }

    #[test]
    fn wedge_and_pyramid() {
        let mut s = Session::new();
        s.execute("wedge", &json!({ "corner1": [0, 0, 1], "corner2": [10, 5], "height": 3 })).unwrap();
        s.execute("pyramid", &json!({ "center": [20, 0], "radius": 4, "height": 6, "sides": 6, "topRadius": 1 })).unwrap();
        let xs = shapes(&s);
        assert_eq!(xs.len(), 2);
        assert!(near(xs[0].bounds(), (0.0, 0.0), (10.0, 5.0)) && spans(&xs[0], 1.0, 4.0));
        assert!(!xs[1].mesh.open && (heights(&xs[1]).1 - 6.0).abs() < 1e-9);
        s.undo().unwrap();
        assert_eq!(shapes(&s).len(), 1);
    }

    #[test]
    fn a_box_started_on_a_3d_object_starts_at_its_height() {
        let mut s = Session::new();
        s.start("box").unwrap();
        s.point_z = Some(5.0);
        s.input(Input::Point(Vec2::ZERO)).unwrap();
        s.point_z = None;
        s.input(Input::Point(Vec2::new(4.0, 4.0))).unwrap();
        s.input(Input::Text("2".into())).unwrap();
        let xs = shapes(&s);
        assert_eq!(xs.len(), 1);
        assert!(spans(&xs[0], 5.0, 7.0), "{:?}", heights(&xs[0]));
    }

    #[test]
    fn loft_closed_sections_into_a_solid_and_open_ones_into_a_surface() {
        let mut s = Session::new();
        let (a, b) =
            (circle_at(&mut s, 0.0, 0.0, 5.0), poly(&mut s, &[(-2.0, -2.0, 0.0), (2.0, -2.0, 0.0), (2.0, 2.0, 0.0), (-2.0, 2.0, 0.0)], true, 8.0));
        let r = s.execute("loft", &json!({ "handles": [a.hex(), b.hex()], "ruled": true })).unwrap();
        assert_eq!(r["surface"], false);
        let xs = shapes(&s);
        assert_eq!(xs.len(), 1);
        assert_eq!(s.doc().unwrap().model.iter().count(), 1, "the sections are erased");
        assert!(spans(&xs[0], 0.0, 8.0), "{:?}", heights(&xs[0]));
        s.undo().unwrap();
        // Open sections: a surface, and the curves stay.
        let mut s = Session::new();
        let l1 = s.add_entity(EntityKind::Line(cadcraft_doc::Line { a: Vec3::ZERO, b: Vec3::new(10.0, 0.0, 0.0) })).unwrap();
        let l2 = s.add_entity(EntityKind::Line(cadcraft_doc::Line { a: Vec3::new(0.0, 5.0, 4.0), b: Vec3::new(10.0, 5.0, 4.0) })).unwrap();
        let r = s.execute("loft", &json!({ "handles": [l1.hex(), l2.hex()] })).unwrap();
        assert_eq!(r["surface"], true);
        assert!(shapes(&s)[0].mesh.open);
        assert_eq!(s.doc().unwrap().model.iter().count(), 3);
    }

    #[test]
    fn sweep_a_circle_along_a_polyline_and_a_line_along_a_helix() {
        let mut s = Session::new();
        let prof = circle_at(&mut s, 50.0, 0.0, 0.5);
        // Along X, a quarter-circle bend (bulge tan(22.5°)), then a corner up Y.
        let path =
            poly(&mut s, &[(0.0, 0.0, 0.0), (10.0, 0.0, 0.41421356237309503), (15.0, 5.0, 0.0), (15.0, 15.0, 0.0), (5.0, 15.0, 0.0)], false, 0.0);
        let r = s.execute("sweep", &json!({ "handles": [prof.hex()], "path": path.hex() })).unwrap();
        assert_eq!(r["handles"].as_array().map(Vec::len), Some(1));
        let xs = shapes(&s);
        assert!(!xs[0].mesh.open);
        // The mesh's polygons sit just inside the round tube.
        assert!(near_by(xs[0].bounds(), (0.0, -0.5), (15.5, 15.5), 0.01), "{:?}", xs[0].bounds());
        assert_eq!(s.doc().unwrap().model.iter().count(), 1, "profile and path are erased");
        // The start cap is whole: the last corner's mitre plane (y = x) passes through it, but
        // only trims near that corner.
        let top = xs[0].mesh.verts.iter().filter(|p| p.x.abs() < 1e-6).map(|p| p.y).fold(f64::MIN, f64::max);
        assert!(top > 0.45, "{top}");
        // A unit square along the same path holds the path's length (30 + 2.5π) in volume.
        let mut s = Session::new();
        let sq = poly(&mut s, &[(40.0, 0.0, 0.0), (41.0, 0.0, 0.0), (41.0, 1.0, 0.0), (40.0, 1.0, 0.0)], true, 0.0);
        let path =
            poly(&mut s, &[(0.0, 0.0, 0.0), (10.0, 0.0, 0.41421356237309503), (15.0, 5.0, 0.0), (15.0, 15.0, 0.0), (5.0, 15.0, 0.0)], false, 0.0);
        s.execute("sweep", &json!({ "handles": [sq.hex()], "path": path.hex() })).unwrap();
        let v = volume(&shapes(&s)[0]);
        assert!((v / (30.0 + 2.5 * std::f64::consts::PI) - 1.0).abs() < 0.01, "{v}");
        // An open profile along a helix: a surface; the curves stay.
        let mut s = Session::new();
        let helix = cadcraft_doc::Helix {
            center: Vec3::ZERO,
            base_radius: 10.0,
            top_radius: 10.0,
            height: 20.0,
            turns: 2.0,
            start_angle: 0.0,
            ccw: true,
            constrain: Default::default(),
        };
        let path = s.add_entity(EntityKind::Helix(helix)).unwrap();
        let l = s.add_entity(EntityKind::Line(cadcraft_doc::Line { a: Vec3::ZERO, b: Vec3::new(2.0, 0.0, 0.0) })).unwrap();
        s.execute("sweep", &json!({ "handles": [l.hex()], "path": path.hex() })).unwrap();
        assert!(shapes(&s)[0].mesh.open);
        assert_eq!(s.doc().unwrap().model.iter().count(), 3);
        // A circle path runs round once.
        let mut s = Session::new();
        let ring = circle_at(&mut s, 0.0, 0.0, 10.0);
        let tube = circle_at(&mut s, 30.0, 0.0, 1.0);
        s.execute("sweep", &json!({ "handles": [tube.hex()], "path": ring.hex() })).unwrap();
        let xs = shapes(&s);
        assert!(!xs[0].mesh.open && near_by(xs[0].bounds(), (-11.0, -11.0), (11.0, 11.0), 0.05), "{:?}", xs[0].bounds());
    }

    #[test]
    fn chamfer_shell_and_slice_a_box() {
        let mut s = Session::new();
        let b = handle(&s.execute("box", &json!({ "corner1": [0, 0, 0], "corner2": [10, 10], "height": 10 })).unwrap()["handle"]);
        let before = shapes(&s)[0].mesh.tris.len();
        s.execute("chamferedge", &json!({ "handle": b.hex(), "distance": 1 })).unwrap();
        assert!(shapes(&s)[0].mesh.tris.len() > before);
        s.undo().unwrap();
        s.execute("shell", &json!({ "handles": [b.hex()], "thickness": 1 })).unwrap();
        assert!(!shapes(&s)[0].mesh.open && spans(&shapes(&s)[0], 0.0, 10.0));
        s.undo().unwrap();
        // Both halves stay: the solid keeps the one left of point1 → point2 (here -X), the other is new.
        let r = s.execute("slice", &json!({ "handles": [b.hex()], "point1": [4, 0], "point2": [4, 1] })).unwrap();
        assert_eq!(r["sliced"], 1);
        let xs = shapes(&s);
        assert_eq!(xs.len(), 2);
        assert!(
            near(xs[0].bounds(), (0.0, 0.0), (4.0, 10.0)) && near(xs[1].bounds(), (4.0, 0.0), (10.0, 10.0)),
            "{:?}",
            xs.iter().map(|x| x.bounds()).collect::<Vec<_>>()
        );
        s.undo().unwrap();
        // Keep the side a point is on.
        s.execute("slice", &json!({ "handles": [b.hex()], "point1": [4, 0], "point2": [4, 1], "keep": "side", "side": [9, 5] })).unwrap();
        let xs = shapes(&s);
        assert_eq!(xs.len(), 1);
        assert!(near(xs[0].bounds(), (4.0, 0.0), (10.0, 10.0)));
        // A plane that misses: an error, nothing changes.
        let before = s.doc().unwrap().clone();
        assert!(s.execute("slice", &json!({ "handles": [b.hex()], "point1": [40, 0], "point2": [40, 1] })).is_err());
        assert_eq!(*s.doc().unwrap(), before);
    }

    #[test]
    fn planar_surfaces_from_corners_and_curves() {
        let mut s = Session::new();
        s.execute("planesurf", &json!({ "corner1": [0, 0, 3], "corner2": [4, 2] })).unwrap();
        let c = circle_at(&mut s, 10.0, 0.0, 2.0);
        s.execute("planesurf", &json!({ "handles": [c.hex()] })).unwrap();
        let xs = shapes(&s);
        assert_eq!(xs.len(), 2);
        assert!(xs.iter().all(|x| x.mesh.open));
        assert!(spans(&xs[0], 3.0, 3.0), "{:?}", heights(&xs[0]));
        assert!(s.doc().unwrap().entity(c).is_some(), "the circle stays");
    }

    #[test]
    fn extruding_an_open_curve_makes_a_surface_and_keeps_it() {
        let mut s = Session::new();
        let l = s.execute("line", &json!({ "points": [[0, 0], [5, 0]] })).unwrap()["handles"][0].clone();
        let c = s.add_entity(super::super::helpers::circle(Vec2::new(20.0, 0.0), 1.0)).unwrap();
        s.execute("extrude", &json!({ "handles": [l, c.hex()], "height": 4 })).unwrap();
        let xs = shapes(&s);
        assert_eq!(xs.len(), 2);
        assert!(xs[0].mesh.open && !xs[1].mesh.open);
        assert!(s.doc().unwrap().entity(handle(&l)).is_some() && s.doc().unwrap().entity(c).is_none());
    }
}
