//! Shared geometry helpers for commands.

use cadcraft_doc::{Entity, EntityKind, Line, Prim};
use cadcraft_geom::{Mat3, PolyVertex, Segment, Vec2, Vec3};

pub fn v3(p: Vec2) -> Vec3 {
    p.to3(0.0)
}

pub fn line(a: Vec2, b: Vec2) -> EntityKind {
    EntityKind::Line(Line { a: v3(a), b: v3(b) })
}

pub fn circle(c: Vec2, r: f64) -> EntityKind {
    EntityKind::Circle(cadcraft_doc::Circle { center: v3(c), radius: r.abs() })
}

pub fn arc(a: &cadcraft_geom::Arc) -> EntityKind {
    EntityKind::Arc(cadcraft_doc::Arc { center: v3(a.center), radius: a.radius, start: a.start, end: a.end })
}

pub fn lwpoly(vs: Vec<PolyVertex>, closed: bool) -> EntityKind {
    EntityKind::LwPolyline(cadcraft_doc::LwPolyline { vertices: vs, closed, const_width: 0.0, elevation: 0.0, plinegen: false })
}

pub fn rect_vertices(a: Vec2, b: Vec2) -> Vec<PolyVertex> {
    [a, Vec2::new(b.x, a.y), b, Vec2::new(a.x, b.y)].into_iter().map(PolyVertex::new).collect()
}

/// Regular polygon vertices: inscribed (vertex at `radius`) or circumscribed (edge midpoint at `radius`).
pub fn polygon_vertices(center: Vec2, sides: usize, radius_pt: Vec2, inscribed: bool) -> Vec<PolyVertex> {
    let n = sides.clamp(3, 1024);
    let d = radius_pt - center;
    let r = d.len();
    let a0 = d.angle();
    let step = cadcraft_geom::TAU / n as f64;
    let (r, a0) = if inscribed { (r, a0) } else { (r / (step / 2.0).cos(), a0 - step / 2.0) };
    (0..n).map(|i| PolyVertex::new(Vec2::polar(center, r, a0 + step * i as f64))).collect()
}

/// The radius point for a polygon whose radius is typed: the bottom edge is horizontal in both
/// modes (inscribed: a vertex half a side's angle past straight down; circumscribed: the bottom
/// edge's midpoint straight down).
pub fn polygon_typed_radius_point(center: Vec2, sides: usize, radius: f64, inscribed: bool) -> Vec2 {
    let down = -std::f64::consts::FRAC_PI_2;
    let a = if inscribed { down + std::f64::consts::PI / sides.clamp(3, 1024) as f64 } else { down };
    Vec2::polar(center, radius, a)
}

/// Polygon from an edge (first two vertices).
pub fn polygon_from_edge(p1: Vec2, p2: Vec2, sides: usize) -> Vec<PolyVertex> {
    let n = sides.clamp(3, 1024);
    let step = cadcraft_geom::TAU / n as f64;
    let mut pts = vec![p1, p2];
    let len = p1.dist(p2);
    let mut ang = p1.angle_to(p2);
    for _ in 2..n {
        ang += step;
        let last = pts.last().copied().unwrap_or(p2);
        pts.push(Vec2::polar(last, len, ang));
    }
    pts.into_iter().map(PolyVertex::new).collect()
}

/// Rectangle vertices with fillet radius or chamfer distances at every corner.
pub fn rect_with_corners(a: Vec2, b: Vec2, fillet: f64, chamfer: (f64, f64)) -> Vec<PolyVertex> {
    let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
    let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
    let w = x1 - x0;
    let h = y1 - y0;
    if fillet > 0.0 && fillet * 2.0 <= w.min(h) + 1e-12 {
        let r = fillet;
        let bul = cadcraft_geom::arc_to_bulge(std::f64::consts::FRAC_PI_2);
        return vec![
            PolyVertex::new(Vec2::new(x0 + r, y0)),
            PolyVertex::with_bulge(Vec2::new(x1 - r, y0), bul),
            PolyVertex::new(Vec2::new(x1, y0 + r)),
            PolyVertex::with_bulge(Vec2::new(x1, y1 - r), bul),
            PolyVertex::new(Vec2::new(x1 - r, y1)),
            PolyVertex::with_bulge(Vec2::new(x0 + r, y1), bul),
            PolyVertex::new(Vec2::new(x0, y1 - r)),
            PolyVertex::with_bulge(Vec2::new(x0, y0 + r), bul),
        ];
    }
    let (c1, c2) = chamfer;
    if c1 > 0.0 && c2 > 0.0 && c1 + c1 <= w + 1e-12 && c2 + c2 <= h + 1e-12 {
        return [
            Vec2::new(x0 + c1, y0),
            Vec2::new(x1 - c1, y0),
            Vec2::new(x1, y0 + c2),
            Vec2::new(x1, y1 - c2),
            Vec2::new(x1 - c1, y1),
            Vec2::new(x0 + c1, y1),
            Vec2::new(x0, y1 - c2),
            Vec2::new(x0, y0 + c2),
        ]
        .into_iter()
        .map(PolyVertex::new)
        .collect();
    }
    rect_vertices(Vec2::new(x0, y0), Vec2::new(x1, y1))
}

// ---------- the user's support files ----------

/// Largest support file (linetype, hatch pattern or alias file) read.
pub const MAX_SUPPORT_FILE_BYTES: u64 = 8 << 20;

/// The text of a support file the user gives: a path in `{path_key}` (read from disk; the web has
/// no file system) or the contents in `{text_key}`. Returns the text and a name for messages, or
/// `None` when neither is given. Files over [`MAX_SUPPORT_FILE_BYTES`] are refused; bytes that
/// aren't UTF-8 are read as replacement characters.
pub fn support_text(cmd: &str, p: &serde_json::Value, path_key: &str, text_key: &str) -> crate::Result<Option<(String, String)>> {
    if let Some(text) = p.get(text_key).and_then(serde_json::Value::as_str) {
        if text.len() as u64 > MAX_SUPPORT_FILE_BYTES {
            return Err(super::bad(cmd, format!("`{text_key}` is larger than {} MB", MAX_SUPPORT_FILE_BYTES >> 20)));
        }
        return Ok(Some((text.to_string(), "the given text".into())));
    }
    match p.get(path_key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(path)) => Ok(Some((read_support_file(cmd, path)?, path.clone()))),
        Some(_) => Err(super::bad(cmd, format!("`{path_key}` must be a file path"))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn read_support_file(cmd: &str, path: &str) -> crate::Result<String> {
    use std::io::Read;
    crate::limits::files(cmd)?;
    let f = std::fs::File::open(path).map_err(|e| super::bad(cmd, format!("{path}: {e}")))?;
    let mut bytes = Vec::new();
    f.take(MAX_SUPPORT_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(|e| super::bad(cmd, format!("{path}: {e}")))?;
    if bytes.len() as u64 > MAX_SUPPORT_FILE_BYTES {
        return Err(super::bad(cmd, format!("{path}: larger than {} MB", MAX_SUPPORT_FILE_BYTES >> 20)));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(target_arch = "wasm32")]
fn read_support_file(cmd: &str, _path: &str) -> crate::Result<String> {
    Err(super::bad(cmd, "paths are not available on the web; pass the file's contents"))
}

/// `e` after the plan transform `m` ([`Entity::transform`]: an object in a tilted plane moves in
/// 3D). An error when that can't be represented (a non-uniform scale of a circle, arc, text or
/// block in a tilted plane), for the command to leave the drawing as it was.
pub fn transformed(e: &Entity, m: &Mat3) -> crate::Result<Entity> {
    let mut ne = e.clone();
    if ne.transform(m) {
        Ok(ne)
    } else {
        Err(crate::EngineError::Other(format!("A {} in a tilted plane can't be scaled unevenly.", e.kind.type_name().to_ascii_lowercase())))
    }
}

/// How `e` shows in plan, as kinds for a rubber-band preview (a preview carries no plane): its
/// own kind, or for an object in a tilted plane what it looks like from above.
pub fn plan_kinds(e: &Entity) -> Vec<EntityKind> {
    if e.plan_xf().is_none() {
        return vec![e.kind.clone()];
    }
    e.prims()
        .into_iter()
        .map(|p| match p {
            Prim::Seg(Segment::Line(l)) => line(l.a, l.b),
            Prim::Seg(Segment::Arc { arc: a, .. }) => arc(&a),
            Prim::Circle(c) => circle(c.center, c.radius),
            Prim::Ellipse(el) => EntityKind::Ellipse(cadcraft_doc::Ellipse {
                center: v3(el.center),
                major: v3(el.major),
                ratio: el.ratio,
                start: el.start,
                end: el.end,
            }),
            Prim::Spline(sp) => EntityKind::Spline(sp),
            Prim::Point(q) => EntityKind::Point(cadcraft_doc::Point { p: v3(q), angle: 0.0 }),
            Prim::Infinite { base, dir, ray } => {
                let r = cadcraft_doc::RayLine { base: v3(base), dir: v3(dir) };
                if ray { EntityKind::Ray(r) } else { EntityKind::XLine(r) }
            }
            Prim::Fill(pts) => lwpoly(pts.into_iter().map(PolyVertex::new).collect(), true),
        })
        .collect()
}

/// [`plan_kinds`] of `e` after the plan transform `m` (nothing when it can't take it).
pub fn plan_preview(e: &Entity, m: &Mat3) -> Vec<EntityKind> {
    transformed(e, m).map(|t| plan_kinds(&t)).unwrap_or_default()
}

/// Refuse a 2D shape edit (TRIM, EXTEND, BREAK, FILLET, CHAMFER, JOIN, PEDIT Join and Edit
/// vertex) of an object in a tilted plane, whose own coordinates aren't what plan view shows.
/// ponytail: refused, not edited (they combine it with other objects or picked points in plan);
/// upgrade: take the picked and cutting geometry into its plane ([`to_local`]) and edit there,
/// as OFFSET, LENGTHEN and DIVIDE/MEASURE do.
pub fn level_only(e: &Entity) -> crate::Result<()> {
    match e.plan_xf() {
        Some(_) => Err(crate::EngineError::Other(format!(
            "The {} is in a tilted plane, which this command can't edit: it can be moved, rotated, scaled, mirrored, grip-edited, offset or lengthened in plan.",
            e.kind.type_name().to_ascii_lowercase()
        ))),
        None => Ok(()),
    }
}

/// Plan point `p` in `e`'s own plane (its 2D coordinates): [`Entity::plan_to_local`] from the
/// grip plan view shows nearest `p` (a plane seen edge-on keeps that grip's depth). `p` itself
/// for objects seen flat in plan.
pub fn to_local(e: &Entity, p: Vec2) -> Vec2 {
    if e.plan_xf().is_none() {
        return p;
    }
    let near = e.grips().into_iter().zip(e.kind.grips()).min_by(|a, b| a.0.dist(p).total_cmp(&b.0.dist(p))).map_or(Vec2::ZERO, |(_, l)| l);
    e.plan_to_local(p, near)
}

/// Block object `child` placed where block reference `parent` (`ins`, block base point `base`)
/// shows it, as EXPLODE leaves it: through the reference's plane in 3D (elevation, z scale and
/// base point height included), or, for a reference in the XY plane's orientation, by its 2D
/// transform where 3D can't represent the result (a circle under a non-uniform scale becomes a
/// circle of the mean radius, as 2D EXPLODE always did). `None` when neither can.
pub fn placed_in(parent: &Entity, ins: &cadcraft_doc::Insert, base: Vec3, child: &Entity) -> Option<Entity> {
    let m3 = parent.plane().unwrap_or(cadcraft_geom::Mat4::IDENTITY).mul(&ins.transform3(base));
    let mut ne = child.clone();
    (ne.transform3(&m3) || (parent.plan_xf().is_none() && ne.transform(&ins.transform(base.xy())))).then_some(ne)
}
