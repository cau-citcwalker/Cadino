//! Naming the faces and edges of a solid, and the operations built on them: AutoCAD's SOLIDEDIT
//! (faces, edges and bodies), FILLETEDGE/CHAMFEREDGE on chosen edges, PRESSPULL, THICKEN, SECTION,
//! FLATSHOT, INTERFERE, POLYSOLID, CONVTOSOLID/CONVTOSURFACE/XEDGES, the curve surfaces (RULESURF,
//! TABSURF, REVSURF, EDGESURF), PLANESURF with holes, LOFT along a path or guides and SLICE by a
//! surface. Inputs are checked here; the backend runs in the worker like every kernel operation.
//!
//! **Faces and edges are named by index**, counting from 0 in the order OpenCASCADE maps them
//! (`TopExp::MapShapes`) in the shape read from its B-rep text. An index is valid for that exact
//! B-rep text only: every edit makes new text with new indices, so ask [`topology`] (or [`pick`])
//! again after one. Placing a solid (its transform) doesn't change its indices.
//!
//! Geometry going in and coming out is in world coordinates (the solid's transform applied); a
//! shape that comes out ([`Built`]) is in world coordinates too, to be stored with the identity
//! transform.

use cadcraft_geom::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

use crate::curve::{self, Run, unit};
use crate::{Built, Curve, KernelError, MIN_LEN, Placed, Result, Seg, Solid, check_brep, check_curve, check_flat, run};

/// Most faces or edges one operation may name.
pub const MAX_PICKS: usize = 10_000;
/// Most shapes one operation may take (INTERFERE, FLATSHOT, CONVTOSOLID).
pub const MAX_SHAPES: usize = 10_000;
/// Most triangles a mesh converted to a solid or surface may have.
pub const MAX_MESH_TRIS: usize = 50_000;

/// The kind of surface a face lies on (OpenCASCADE's surface types).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SurfaceKind {
    Plane,
    Cylinder,
    Cone,
    Sphere,
    Torus,
    Bezier,
    BSpline,
    Revolution,
    Extrusion,
    Offset,
    Other,
}

/// A curve the kernel hands back, in world coordinates (FLATSHOT's: in the view, see [`flatshot`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EdgeCurve {
    Line {
        a: Vec3,
        b: Vec3,
    },
    /// A circular arc from `a` through `mid` to `b`.
    Arc {
        a: Vec3,
        mid: Vec3,
        b: Vec3,
    },
    Circle {
        center: Vec3,
        radius: f64,
        normal: Vec3,
    },
    /// An ellipse or part of one: `major` is the half major axis, `ratio` the minor axis over the
    /// major; it runs from `start` to `end` (radians from `major`, counterclockwise about
    /// `normal`), a whole one from 0 to 2π.
    Ellipse {
        center: Vec3,
        major: Vec3,
        ratio: f64,
        normal: Vec3,
        start: f64,
        end: f64,
    },
    /// Any other curve (B-spline, Bézier, a projected or intersection curve) as a NURBS curve:
    /// `knots` written out with their multiplicities (`poles + degree + 1` of them), `weights`
    /// empty when it isn't rational; `points` is a polyline along it, for drawing.
    Spline {
        degree: u32,
        poles: Vec<Vec3>,
        weights: Vec<f64>,
        knots: Vec<f64>,
        points: Vec<Vec3>,
    },
}

/// One face of a solid (see [`topology`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FaceInfo {
    pub kind: SurfaceKind,
    pub area: f64,
    /// The centre of its area: on a flat face's plane; off a curved face (a cylinder's is on its
    /// axis).
    pub centroid: Vec3,
    /// The unit normal pointing out of the solid at the face's point nearest the centroid.
    pub normal: Vec3,
    /// Its bounding box (min, max).
    pub bbox: (Vec3, Vec3),
    /// A plane's point and outward normal; a cylinder's, cone's or torus's axis (a point on it and
    /// its direction); a sphere's centre and polar axis. Zero for free-form faces.
    pub axis: (Vec3, Vec3),
    /// A cylinder's or sphere's radius, a cone's at its axis point, a torus's major radius; else 0.
    pub radius: f64,
    /// The indices of the edges round it.
    pub edges: Vec<u32>,
}

/// One edge of a solid (see [`topology`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeInfo {
    /// The curve as the edge runs (XEDGES draws these).
    pub curve: EdgeCurve,
    pub length: f64,
    /// The point halfway along its parameter.
    pub mid: Vec3,
    /// The faces it bounds: two for most edges, one along a surface's border or a seam.
    pub faces: Vec<u32>,
    /// A face meets itself along it (a cylinder's or a revolved face's seam), not a real edge.
    pub seam: bool,
    /// Shrunk to a point (a cone's apex, a sphere's pole): `curve` is a line of no length there.
    pub degenerate: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Topology {
    pub faces: Vec<FaceInfo>,
    pub edges: Vec<EdgeInfo>,
}

/// What [`pick`] looks along or around.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Probe {
    /// The whole line through `origin` along `dir` (a click in a view: the eye's line through the
    /// cursor, looking along `dir`).
    Ray { origin: Vec3, dir: Vec3 },
    /// A point.
    Point(Vec3),
}

/// A face or edge [`pick`] found.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub index: u32,
    /// The point on the face or edge (for a ray: where it meets the face, or the edge's point
    /// nearest the line).
    pub point: Vec3,
    /// How far along the ray `point` lies from `origin` (in world units: only `dir`'s direction
    /// counts); 0 for a point.
    pub along: f64,
    /// How far the face or edge is from the ray or point (0 where a ray meets a face).
    pub distance: f64,
}

/// [`pick`]'s findings: for a ray, the faces it meets and the edges within the tolerance, front
/// first (smallest `along`: the viewer looks along `dir`); for a point, the faces and edges
/// within the tolerance, nearest first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Picked {
    pub faces: Vec<Hit>,
    pub edges: Vec<Hit>,
}

/// One edge to bevel: `base` along the face `face` (or, `None`, the edge's first face), `other`
/// along the other face (AutoCAD's Distance1 on the base surface and Distance2).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bevel {
    pub edge: u32,
    pub base: f64,
    pub other: f64,
    pub face: Option<u32>,
}

/// What PRESSPULL pushes or pulls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Region {
    /// A flat face of the solid, along its outward normal.
    Face(u32),
    /// A closed flat curve (with holes) lying on a face of the solid, along that face's outward
    /// normal: out of the solid as a boss, or into it as a pocket or hole.
    Profile { outer: Curve, holes: Vec<Curve> },
}

/// [`check`]'s verdict.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub valid: bool,
    /// What is wrong, one line each ("face 3: …"), at most 200.
    pub problems: Vec<String>,
}

/// A closed or open chain of curves, in order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Wire3 {
    pub curves: Vec<EdgeCurve>,
    pub closed: bool,
}

/// Where the cutting plane goes through a solid's material.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SectionRegion {
    /// The region as a flat surface.
    pub face: Built,
    pub outer: Wire3,
    pub holes: Vec<Wire3>,
}

/// [`section`]'s result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Section {
    /// The regions where the plane cuts solid material (none for a surface).
    pub regions: Vec<SectionRegion>,
    /// Every curve the plane cuts, joined into chains (a solid's are its regions' outlines).
    pub wires: Vec<Wire3>,
}

/// [`flatshot`]'s drawing, in the view plane: `x` along the view's right, `y` up, `z` 0.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Flat {
    pub visible: Vec<EdgeCurve>,
    pub hidden: Vec<EdgeCurve>,
}

/// Two solids that overlap (see [`interference`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clash {
    pub a: usize,
    pub b: usize,
    /// The shared volume as a solid.
    pub solid: Built,
    pub volume: f64,
}

/// Which side of its path a polysolid lies on, looking down `up` as the path runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Justify {
    Left,
    Center,
    Right,
}

/// One operation of this module, as the worker receives it.
#[derive(Serialize, Deserialize)]
pub(crate) enum Op {
    Topology(Solid),
    Pick(Solid, Probe, f64),
    /// [`Op::Pick`] on each shape.
    Picks(Vec<Solid>, Probe, f64),
    FilletEdges(Solid, Vec<(u32, f64)>),
    ChamferEdges(Solid, Vec<Bevel>),
    ExtrudeFaces(Solid, Vec<u32>, f64, f64),
    OffsetFaces(Solid, Vec<u32>, f64),
    /// A rigid motion of the faces.
    TransformFaces(Solid, Vec<u32>, Mat4),
    TaperFaces(Solid, Vec<u32>, Vec3, Vec3, f64),
    DeleteFaces(Solid, Vec<u32>),
    CopyFaces(Solid, Vec<u32>),
    Imprint(Solid, Vec<Solid>, Vec<Curve>),
    Separate(Solid),
    ShellFaces(Solid, Vec<u32>, f64),
    Clean(Solid),
    Check(Solid),
    PressPull(Solid, Region, f64),
    Thicken(Solid, f64, bool),
    Section(Solid, Vec3, Vec3),
    /// [`Op::Section`] of each shape and, with the flag, the piece of it behind the plane.
    Sections(Vec<Solid>, Vec3, Vec3, bool),
    Flatshot(Vec<Solid>, Vec3, Vec3),
    Interfere(Vec<Solid>, Vec<Solid>),
    Polysolid(Vec<Run>),
    Sew(Vec<Solid>, bool),
    MeshShape(Vec<Vec3>, Vec<[u32; 3]>, bool, bool),
    ToSurface(Solid),
    Split(Solid, Vec<Solid>),
    Ruled(Curve, Curve),
    Tabulated(Curve, Vec3),
    Revolved(Curve, Vec3, Vec3, f64, f64),
    Coons(Vec<Curve>),
    PlanarFace(Curve, Vec<Curve>),
    /// Sections, the path they are swept along, solid.
    PipeLoft(Vec<Curve>, Curve, bool),
    /// Sections, the guides through their corners, solid.
    GuidedLoft(Vec<Curve>, Vec<Curve>, bool),
}

/// What an [`Op`] gives back.
#[derive(Serialize, Deserialize)]
pub(crate) enum Out {
    Built(Built),
    Builts(Vec<Built>),
    Topology(Topology),
    Picked(Picked),
    Pickeds(Vec<Result<Picked>>),
    Sections(Vec<Result<(Section, Option<Built>)>>),
    Check(Check),
    Section(Section),
    Flat(Flat),
    Clashes(Vec<Clash>),
}

impl Op {
    /// What the operation does, for "it stopped while …".
    #[cfg_attr(not(feature = "occt"), allow(dead_code))]
    pub(crate) fn what(&self) -> &'static str {
        match self {
            Op::Topology(_) => "listing faces and edges",
            Op::Pick(..) | Op::Picks(..) => "picking a face or edge",
            Op::FilletEdges(..) => "rounding edges",
            Op::ChamferEdges(..) => "bevelling edges",
            Op::ExtrudeFaces(..) => "extruding faces",
            Op::OffsetFaces(..) => "offsetting faces",
            Op::TransformFaces(..) => "moving faces",
            Op::TaperFaces(..) => "tapering faces",
            Op::DeleteFaces(..) => "removing faces",
            Op::CopyFaces(..) => "copying faces",
            Op::Imprint(..) => "imprinting",
            Op::Separate(_) => "separating a solid",
            Op::ShellFaces(..) => "hollowing a solid",
            Op::Clean(_) => "cleaning a solid",
            Op::Check(_) => "checking a solid",
            Op::PressPull(..) => "pressing or pulling",
            Op::Thicken(..) => "thickening",
            Op::Section(..) | Op::Sections(..) => "sectioning",
            Op::Flatshot(..) => "drawing hidden lines",
            Op::Interfere(..) => "checking interference",
            Op::Polysolid(_) => "making a polysolid",
            Op::Sew(..) => "joining surfaces",
            Op::MeshShape(..) => "converting a mesh",
            Op::ToSurface(_) => "making a surface",
            Op::Split(..) => "slicing",
            Op::Ruled(..) | Op::Tabulated(..) | Op::Revolved(..) | Op::Coons(_) | Op::PlanarFace(..) => "making a surface",
            Op::PipeLoft(..) | Op::GuidedLoft(..) => "lofting",
        }
    }
}

macro_rules! out {
    ($name:ident, $variant:ident, $t:ty) => {
        fn $name(o: Out) -> Result<$t> {
            if let Out::$variant(x) = o { Ok(x) } else { Err(crate::mismatch()) }
        }
    };
}
out!(built, Built, Built);
out!(builts, Builts, Vec<Built>);
out!(topo, Topology, Topology);
out!(picked, Picked, Picked);
out!(pickeds, Pickeds, Vec<Result<Picked>>);
out!(sectioneds, Sections, Vec<Result<(Section, Option<Built>)>>);
out!(checked, Check, Check);
out!(sectioned, Section, Section);
out!(flat, Flat, Flat);
out!(clashes, Clashes, Vec<Clash>);

fn op(o: Op) -> Result<Out> {
    run(crate::Request::Op(o))?.op()
}

fn bad(m: impl Into<String>) -> KernelError {
    KernelError::Invalid(m.into())
}

/// The stored solid, refused when it isn't B-rep text or is placed by a transform the kernel
/// can't apply.
fn stored(p: Placed) -> Result<Solid> {
    check_brep(p.brep)?;
    if crate::similarity(p.xform).is_none() {
        return Err(bad("the solid has a transform the kernel can't apply (only move, rotate, scale and mirror)"));
    }
    Ok(Solid::of(p))
}

fn stored_all(ps: &[Placed]) -> Result<Vec<Solid>> {
    if ps.len() > MAX_SHAPES {
        return Err(bad(format!("at most {MAX_SHAPES} shapes at once")));
    }
    ps.iter().map(|p| stored(*p)).collect()
}

/// The indices, once each.
fn chosen(at: &[u32], what: &str) -> Result<Vec<u32>> {
    if at.is_empty() || at.len() > MAX_PICKS {
        return Err(bad(format!("choose 1 to {MAX_PICKS} {what}")));
    }
    let mut v = at.to_vec();
    v.sort_unstable();
    v.dedup();
    Ok(v)
}

fn finite(v: Vec3, what: &str) -> Result<Vec3> {
    if v.is_finite() { Ok(v) } else { Err(bad(format!("{what} is not finite"))) }
}

fn direction(v: Vec3, what: &str) -> Result<Vec3> {
    unit(v).filter(|_| v.is_finite() && v.len() >= 1e-12).ok_or_else(|| bad(format!("{what} needs a direction")))
}

/// A length the kernel can model: finite and at least [`MIN_LEN`] either way.
fn length(v: f64, what: &str) -> Result<f64> {
    if v.is_finite() && v.abs() >= MIN_LEN { Ok(v) } else { Err(bad(format!("{what} must be at least {MIN_LEN}"))) }
}

/// An angle short of a right angle either way (a taper, a draft).
fn slant(a: f64) -> Result<f64> {
    if a.is_finite() && a.abs() < 89f64.to_radians() { Ok(a) } else { Err(bad("the angle must be between -89 and 89 degrees")) }
}

/// The faces and edges of the solid, by index, with what they are and where. An index counts
/// from 0 in OpenCASCADE's `TopExp::MapShapes` order of the shape read from this exact B-rep text:
/// the same text always names the same faces and edges (whatever the solid's transform), and any
/// edit makes new text with new indices, so list them again after one.
pub fn topology(s: Placed) -> Result<Topology> {
    topo(op(Op::Topology(stored(s)?))?)
}

/// The faces and edges of the solid on a ray or near a point (within `tol`, at least 0); see
/// [`Picked`] for the order.
/// ponytail: one worker call per pick (about 0.1 s): fine for a click, too slow to follow the
/// cursor; a face index per mesh triangle in [`Built`] if hover highlighting needs it.
pub fn pick(s: Placed, probe: Probe, tol: f64) -> Result<Picked> {
    check_probe(probe, tol)?;
    picked(op(Op::Pick(stored(s)?, probe, tol))?)
}

/// [`pick`] on each of the shapes in one worker call (a click among many parts): each one's
/// findings, or why there are none.
pub fn picks(shapes: &[Placed], probe: Probe, tol: f64) -> Result<Vec<Result<Picked>>> {
    check_probe(probe, tol)?;
    if shapes.is_empty() {
        return Ok(Vec::new());
    }
    pickeds(op(Op::Picks(stored_all(shapes)?, probe, tol))?)
}

fn check_probe(probe: Probe, tol: f64) -> Result<()> {
    match probe {
        Probe::Ray { origin, dir } => {
            finite(origin, "the ray's start")?;
            direction(dir, "the ray")?;
        }
        Probe::Point(p) => {
            finite(p, "the point")?;
        }
    }
    if tol.is_finite() && tol >= 0.0 { Ok(()) } else { Err(bad("the tolerance must be 0 or more")) }
}

/// Round the chosen edges, each with its own radius (FILLETEDGE). OpenCASCADE refusing (a radius
/// too large for the faces round it) is an error, never a broken solid.
pub fn fillet_edges(s: Placed, edges: &[(u32, f64)]) -> Result<Built> {
    if edges.is_empty() || edges.len() > MAX_PICKS {
        return Err(bad(format!("choose 1 to {MAX_PICKS} edges")));
    }
    if edges.iter().any(|(_, r)| !(r.is_finite() && *r >= MIN_LEN)) {
        return Err(bad(format!("a radius must be at least {MIN_LEN}")));
    }
    let mut e = edges.to_vec();
    e.sort_by_key(|(i, _)| *i);
    e.dedup_by_key(|(i, _)| *i);
    built(op(Op::FilletEdges(stored(s)?, e))?)
}

/// Bevel the chosen edges (CHAMFEREDGE), each by its own two distances.
pub fn chamfer_edges(s: Placed, bevels: &[Bevel]) -> Result<Built> {
    if bevels.is_empty() || bevels.len() > MAX_PICKS {
        return Err(bad(format!("choose 1 to {MAX_PICKS} edges")));
    }
    if bevels.iter().any(|b| !(b.base.is_finite() && b.base >= MIN_LEN && b.other.is_finite() && b.other >= MIN_LEN)) {
        return Err(bad(format!("a distance must be at least {MIN_LEN}")));
    }
    let mut b = bevels.to_vec();
    b.sort_by_key(|b| b.edge);
    b.dedup_by_key(|b| b.edge);
    built(op(Op::ChamferEdges(stored(s)?, b))?)
}

/// SOLIDEDIT Face Extrude: each flat face pushed out along its outward normal by `distance` (into
/// the solid when negative), its new sides leaning in by `taper` (radians; negative leans out).
/// Only flat faces.
pub fn extrude_faces(s: Placed, faces: &[u32], distance: f64, taper: f64) -> Result<Built> {
    let f = chosen(faces, "faces")?;
    built(op(Op::ExtrudeFaces(stored(s)?, f, length(distance, "the height")?, slant(taper)?))?)
}

/// SOLIDEDIT Face Offset: the faces moved square to themselves by `distance`, their neighbours
/// following; positive grows the solid (a hole's faces offset positive make the hole smaller).
pub fn offset_faces(s: Placed, faces: &[u32], distance: f64) -> Result<Built> {
    let f = chosen(faces, "faces")?;
    built(op(Op::OffsetFaces(stored(s)?, f, length(distance, "the distance")?))?)
}

/// SOLIDEDIT Face Move. Flat faces go to their moved planes with their neighbours following (a
/// flat face moved within its own plane stays put); curved faces (a hole, a boss) move the
/// feature they bound. Flat and curved faces are moved separately.
pub fn move_faces(s: Placed, faces: &[u32], by: Vec3) -> Result<Built> {
    let f = chosen(faces, "faces")?;
    if finite(by, "the move")?.len() < MIN_LEN {
        return Err(bad("the move has no length"));
    }
    built(op(Op::TransformFaces(stored(s)?, f, Mat4::translate(by)))?)
}

/// SOLIDEDIT Face Rotate about the axis through `origin` along `axis`, by `angle` (radians,
/// counterclockwise looking down `axis`). As [`move_faces`]: flat faces tilt to their turned
/// planes (one turned right over is refused), curved faces turn their feature.
pub fn rotate_faces(s: Placed, faces: &[u32], origin: Vec3, axis: Vec3, angle: f64) -> Result<Built> {
    let f = chosen(faces, "faces")?;
    let k = direction(axis, "the axis")?;
    if !angle.is_finite() || angle.abs() < 1e-9 {
        return Err(bad("the angle must not be zero"));
    }
    built(op(Op::TransformFaces(stored(s)?, f, Mat4::rotate_about(finite(origin, "the axis point")?, k, angle)))?)
}

/// SOLIDEDIT Face Taper: each face tilted about where it crosses the plane through `base` square
/// to `dir` (the pull direction, from the base point to the second point), by `angle` (radians;
/// positive tapers it in, towards the solid, as it runs along `dir`).
pub fn taper_faces(s: Placed, faces: &[u32], base: Vec3, dir: Vec3, angle: f64) -> Result<Built> {
    let f = chosen(faces, "faces")?;
    let d = direction(dir, "the taper")?;
    if angle == 0.0 {
        return Err(bad("the angle must not be zero"));
    }
    built(op(Op::TaperFaces(stored(s)?, f, finite(base, "the base point")?, d, slant(angle)?))?)
}

/// SOLIDEDIT Face Delete: the faces removed and the gap closed by extending the faces round it
/// (fillets, chamfers, holes, bosses). An error when the neighbours can't close it.
pub fn delete_faces(s: Placed, faces: &[u32]) -> Result<Built> {
    let f = chosen(faces, "faces")?;
    built(op(Op::DeleteFaces(stored(s)?, f))?)
}

/// SOLIDEDIT Face Copy: each face as a surface of its own (in place; move it after).
pub fn copy_faces(s: Placed, faces: &[u32]) -> Result<Vec<Built>> {
    let f = chosen(faces, "faces")?;
    builts(op(Op::CopyFaces(stored(s)?, f))?)
}

/// SOLIDEDIT Body Imprint (IMPRINT): the curves, and where the `tools` (solids or surfaces) cut its
/// faces, become edges of the solid's faces. A curve off every face leaves no mark.
pub fn imprint(s: Placed, tools: &[Placed], curves: &[Curve]) -> Result<Built> {
    if tools.is_empty() && curves.is_empty() {
        return Err(bad("nothing to imprint"));
    }
    if curves.len() > MAX_SHAPES {
        return Err(bad(format!("at most {MAX_SHAPES} curves at once")));
    }
    curves.iter().try_for_each(check_curve)?;
    built(op(Op::Imprint(stored(s)?, stored_all(tools)?, curves.to_vec()))?)
}

/// SOLIDEDIT Body Separate: each lump (disjoint volume) of the solid as a solid of its own; one
/// lump comes back alone.
pub fn separate(s: Placed) -> Result<Vec<Built>> {
    builts(op(Op::Separate(stored(s)?))?)
}

/// SOLIDEDIT Body Shell: hollowed inward to walls `thickness` thick (negative: outward), open
/// where the `remove` faces were; closed round an empty inside when none are given.
pub fn shell_faces(s: Placed, thickness: f64, remove: &[u32]) -> Result<Built> {
    let f = if remove.is_empty() { Vec::new() } else { chosen(remove, "faces")? };
    built(op(Op::ShellFaces(stored(s)?, f, length(thickness, "the thickness")?))?)
}

/// SOLIDEDIT Body Clean: faces on one surface merged, edges on one curve joined (imprinted edges
/// on a flat face go too).
pub fn clean(s: Placed) -> Result<Built> {
    built(op(Op::Clean(stored(s)?))?)
}

/// SOLIDEDIT Body Check: whether OpenCASCADE finds the shape valid, and what is wrong.
pub fn check(s: Placed) -> Result<Check> {
    checked(op(Op::Check(stored(s)?))?)
}

/// PRESSPULL: the region pushed or pulled `distance` along the outward normal of the face it is
/// on (positive out of the solid, adding a boss; negative into it, cutting a pocket or hole).
pub fn press_pull(s: Placed, region: &Region, distance: f64) -> Result<Built> {
    let d = length(distance, "the distance")?;
    if let Region::Profile { outer, holes } = region {
        check_region(outer, holes)?;
    }
    built(op(Op::PressPull(stored(s)?, region.clone(), d))?)
}

/// A closed flat outer curve, and closed curves in its plane for holes.
fn check_region(outer: &Curve, holes: &[Curve]) -> Result<()> {
    check_curve(outer)?;
    if !outer.is_closed() {
        return Err(bad("the region's boundary must be closed"));
    }
    let n = check_flat(outer)?;
    if holes.len() > MAX_SHAPES {
        return Err(bad(format!("at most {MAX_SHAPES} holes")));
    }
    let pts = outer.points();
    let o = pts.first().copied().unwrap_or(Vec3::ZERO);
    let tol = 1e-7_f64.max(pts.iter().map(|p| (*p - o).len()).fold(0.0, f64::max) * 1e-12);
    for h in holes {
        check_curve(h)?;
        if !h.is_closed() {
            return Err(bad("a hole must be closed"));
        }
        let off = h.points().iter().map(|p| n.dot(*p - o).abs()).fold(0.0, f64::max);
        if check_flat(h)?.cross(n).len() > 1e-9 || off > tol {
            return Err(bad("a hole must lie in the region's plane"));
        }
    }
    Ok(())
}

/// THICKEN: a surface made a solid `thickness` thick, along its faces' normals (negative: the
/// other way), or half each way when `both_sides`.
pub fn thicken(s: Placed, thickness: f64, both_sides: bool) -> Result<Built> {
    built(op(Op::Thicken(stored(s)?, length(thickness, "the thickness")?, both_sides))?)
}

/// SECTION: the shape cut by the plane through `origin` square to `normal`.
pub fn section(s: Placed, origin: Vec3, normal: Vec3) -> Result<Section> {
    let n = direction(normal, "the cutting plane")?;
    sectioned(op(Op::Section(stored(s)?, finite(origin, "the plane's point")?, n))?)
}

/// [`section`] of each of the shapes in one worker call (SECTION and SECTIONPLANETOBLOCK cut many
/// parts at once) and, when `behind`, the piece of each behind the plane (as [`crate::slice`]'s
/// second piece; `None` when nothing is behind): each one's, or why it failed.
pub fn sections(shapes: &[Placed], origin: Vec3, normal: Vec3, behind: bool) -> Result<Vec<Result<(Section, Option<Built>)>>> {
    let n = direction(normal, "the cutting plane")?;
    let o = finite(origin, "the plane's point")?;
    if shapes.is_empty() {
        return Ok(Vec::new());
    }
    sectioneds(op(Op::Sections(stored_all(shapes)?, o, n, behind))?)
}

/// FLATSHOT: the solids' outlines as seen looking against `toward` (the direction from the
/// target to the viewer, as VIEWDIR), with hidden-line removal: visible edges and silhouettes,
/// and the ones they hide. Drawn in the view plane through the origin, `x` along `right` (made
/// square to the view), `y` up the view, `z` 0.
pub fn flatshot(solids: &[Placed], toward: Vec3, right: Vec3) -> Result<Flat> {
    if solids.is_empty() {
        return Err(bad("no solids to draw"));
    }
    let (d, x) = (direction(toward, "the view")?, direction(right, "the view's right")?);
    if d.cross(x).len() < 1e-9 {
        return Err(bad("the view's right must not run along the view"));
    }
    flat(op(Op::Flatshot(stored_all(solids)?, d, x))?)
}

/// INTERFERE: where the solids of `first` overlap those of `second` (every pair within `first`
/// when `second` is empty), with the indices of each pair (`a` into `first`, `b` into `second`, or
/// both into `first`, `a < b`). Pairs whose boxes don't meet aren't tried; touching faces aren't an
/// overlap (no volume above a billionth of the smaller solid's).
pub fn interference(first: &[Placed], second: &[Placed]) -> Result<Vec<Clash>> {
    if first.is_empty() {
        return Err(bad("no solids to check"));
    }
    clashes(op(Op::Interfere(stored_all(first)?, stored_all(second)?))?)
}

/// POLYSOLID: a wall `width` × `height` along the path (lines and arcs end to end, a closed one
/// round), standing up along `up` (the drawing plane's normal), on the `justify` side of the path
/// looking down `up`; corners mitred.
pub fn polysolid(path: &[Seg], width: f64, height: f64, justify: Justify, up: Vec3) -> Result<Built> {
    crate::check_profile(path)?;
    let (w, h) = (length(width, "the width")?.abs(), length(height, "the height")?.abs());
    let up = direction(up, "the up direction")?;
    let path = Curve::Segs(path.to_vec());
    let (p0, t0) = path.start().ok_or_else(|| bad("a polysolid can't run round a circle (draw it as two arcs)"))?;
    // The wall's cross section at the start, square to the path: across it to the left, and up.
    let left = unit(up.cross(t0)).ok_or_else(|| bad("the path must not start along the up direction"))?;
    let rise = t0.cross(left);
    let (a0, a1) = match justify {
        Justify::Left => (0.0, w),
        Justify::Center => (-w / 2.0, w / 2.0),
        Justify::Right => (-w, 0.0),
    };
    let corner = |a: f64, b: f64| p0 + left * a + rise * b;
    let c = [corner(a0, 0.0), corner(a1, 0.0), corner(a1, h), corner(a0, h)];
    let profile = Curve::Segs((0..4).map(|i| Seg::Line { a: c[i], b: c[(i + 1) % 4] }).collect());
    let reach = c.iter().map(|q| (*q - p0).len()).fold(0.0, f64::max);
    built(op(Op::Polysolid(curve::runs(profile, reach, &path)?))?)
}

/// CONVTOSOLID / CONVTOSURFACE: the surfaces' faces joined along the edges they share; a solid
/// (each closed piece a solid facing outward) when `solid`, refused when they don't close up.
pub fn sew(shapes: &[Placed], solid: bool) -> Result<Built> {
    if shapes.is_empty() {
        return Err(bad("nothing to join"));
    }
    built(op(Op::Sew(stored_all(shapes)?, solid))?)
}

/// CONVTOSOLID / CONVTOSURFACE of a mesh (world coordinates): its triangles as flat faces joined
/// along their edges, a solid when `solid` (refused when the mesh isn't watertight), with flat
/// neighbours merged into one face when `merge` (AutoCAD's optimised faceted conversion).
pub fn mesh_shape(verts: &[Vec3], tris: &[[u32; 3]], solid: bool, merge: bool) -> Result<Built> {
    if tris.is_empty() || tris.len() > MAX_MESH_TRIS {
        return Err(bad(format!("a mesh to convert has 1 to {MAX_MESH_TRIS} triangles")));
    }
    if !verts.iter().all(|v| v.is_finite()) {
        return Err(bad("the mesh has a point that isn't finite"));
    }
    if tris.iter().flatten().any(|i| *i as usize >= verts.len()) {
        return Err(bad("a triangle has a corner that isn't in the mesh"));
    }
    built(op(Op::MeshShape(verts.to_vec(), tris.to_vec(), solid, merge))?)
}

/// CONVTOSURFACE of a solid: its faces as a surface (shells, no longer a solid).
pub fn to_surface(s: Placed) -> Result<Built> {
    built(op(Op::ToSurface(stored(s)?))?)
}

/// SLICE by a surface: the shape split where the `tools` (surfaces or solids) cut it, each piece
/// a shape of its own (solids, or faces of a surface).
pub fn split(s: Placed, tools: &[Placed]) -> Result<Vec<Built>> {
    if tools.is_empty() {
        return Err(bad("nothing to slice with"));
    }
    builts(op(Op::Split(stored(s)?, stored_all(tools)?))?)
}

/// RULESURF: the surface of straight lines between two curves, from start to start (reverse one
/// to cross them).
pub fn ruled_surface(a: &Curve, b: &Curve) -> Result<Built> {
    check_curve(a)?;
    check_curve(b)?;
    if a.is_closed() != b.is_closed() {
        return Err(bad("both curves must be open, or both closed"));
    }
    built(op(Op::Ruled(a.clone(), b.clone()))?)
}

/// TABSURF: the curve swept straight along `dir` (its length the distance).
pub fn tabulated_surface(c: &Curve, dir: Vec3) -> Result<Built> {
    check_curve(c)?;
    if !dir.is_finite() || dir.len() < MIN_LEN {
        return Err(bad("the direction vector has no length"));
    }
    built(op(Op::Tabulated(c.clone(), dir))?)
}

/// REVSURF: the curve turned about the axis through `origin` along `axis`, from `start` by
/// `angle` (radians, counterclockwise looking down `axis`; a whole turn or more: all round).
pub fn revolved_surface(c: &Curve, origin: Vec3, axis: Vec3, start: f64, angle: f64) -> Result<Built> {
    check_curve(c)?;
    let k = direction(axis, "the axis")?;
    if !start.is_finite() || !angle.is_finite() || angle.abs() < 1e-9 {
        return Err(bad("the angle must not be zero"));
    }
    let a = angle.clamp(-std::f64::consts::TAU, std::f64::consts::TAU);
    built(op(Op::Revolved(c.clone(), finite(origin, "the axis point")?, k, start % std::f64::consts::TAU, a))?)
}

/// EDGESURF: the Coons patch bounded by four curves that meet end to end in a loop (in any order
/// and direction).
pub fn edge_surface(edges: &[Curve; 4]) -> Result<Built> {
    edges.iter().try_for_each(check_curve)?;
    if edges.iter().any(Curve::is_closed) {
        return Err(bad("an edge surface's edges must be open curves"));
    }
    built(op(Op::Coons(edges.to_vec()))?)
}

/// PLANESURF / CONVTOSURFACE of a region: the flat surface inside `outer`, less the `holes`.
pub fn planar_surface(outer: &Curve, holes: &[Curve]) -> Result<Built> {
    check_region(outer, holes)?;
    built(op(Op::PlanarFace(outer.clone(), holes.to_vec()))?)
}

/// LOFT Path: closed sections lofted along `path` into a solid (open ones into a surface); the
/// path should pass through each section's plane.
pub fn loft_along(sections: &[Curve], path: &Curve) -> Result<Built> {
    let solid = check_sections(sections)?;
    check_curve(path)?;
    built(op(Op::PipeLoft(sections.to_vec(), path.clone(), solid))?)
}

/// LOFT Guides: the sections lofted following guide curves that run from the first section to
/// the last, each touching every section (at a corner, or where one is made). A guide shapes
/// the loft's faces either side of it only, as AutoCAD's do: the rest runs straight on between
/// the sections (two sections) or smoothly through their corners. With guides the sections need
/// as many corners each.
pub fn loft_guided(sections: &[Curve], guides: &[Curve]) -> Result<Built> {
    let solid = check_sections(sections)?;
    if sections.len() < 2 {
        return Err(bad("a guided loft needs two cross sections or more"));
    }
    if guides.is_empty() {
        return Err(bad("a guided loft needs a guide curve"));
    }
    if guides.len() > crate::MAX_SECTIONS {
        return Err(bad(format!("a loft follows at most {} guide curves", crate::MAX_SECTIONS)));
    }
    guides.iter().try_for_each(check_curve)?;
    built(op(Op::GuidedLoft(sections.to_vec(), guides.to_vec(), solid))?)
}

/// Cross sections for a loft: checked, all closed (a solid, `true`) or all open.
fn check_sections(sections: &[Curve]) -> Result<bool> {
    if sections.is_empty() || sections.len() > crate::MAX_SECTIONS {
        return Err(bad(format!("a loft needs 1 to {} cross sections", crate::MAX_SECTIONS)));
    }
    sections.iter().try_for_each(check_curve)?;
    let shut = sections.iter().filter(|c| c.is_closed()).count();
    if shut != 0 && shut != sections.len() {
        return Err(bad("cross sections must be all open or all closed"));
    }
    if shut > 0 {
        sections.iter().try_for_each(|c| check_flat(c).map(|_| ()))?;
    }
    Ok(shut > 0)
}

#[cfg(test)]
mod tests {
    use cadcraft_geom::{Mat4, Vec3};

    use super::*;

    /// Hostile parameters are refused before the kernel, with or without it.
    #[test]
    fn bad_parameters_are_refused_before_the_kernel() {
        let invalid = |r: Result<Built>| matches!(r, Err(KernelError::Invalid(_)));
        let id = Mat4::IDENTITY;
        let s = Placed { brep: "CASCADE Topology", xform: &id };
        let v = Vec3::new;
        assert!(invalid(fillet_edges(s, &[])) && invalid(fillet_edges(s, &[(0, f64::NAN)])) && invalid(fillet_edges(s, &[(0, -1.0)])));
        assert!(invalid(chamfer_edges(s, &[Bevel { edge: 0, base: 1.0, other: 0.0, face: None }])));
        assert!(invalid(extrude_faces(s, &[], 1.0, 0.0)) && invalid(extrude_faces(s, &[0], f64::INFINITY, 0.0)));
        assert!(invalid(extrude_faces(s, &[0], 1.0, f64::NAN)) && invalid(extrude_faces(s, &vec![0; MAX_PICKS + 1], 1.0, 0.0)));
        assert!(invalid(move_faces(s, &[0], Vec3::ZERO)) && invalid(move_faces(s, &[0], v(f64::NAN, 0.0, 0.0))));
        assert!(invalid(rotate_faces(s, &[0], Vec3::ZERO, Vec3::ZERO, 1.0)) && invalid(rotate_faces(s, &[0], Vec3::ZERO, Vec3::Z, 0.0)));
        assert!(invalid(taper_faces(s, &[0], Vec3::ZERO, Vec3::Z, 0.0)) && invalid(taper_faces(s, &[0], Vec3::ZERO, Vec3::Z, 2.0)));
        assert!(invalid(offset_faces(s, &[0], 0.0)) && invalid(shell_faces(s, f64::NAN, &[])));
        assert!(invalid(thicken(s, 0.0, false)) && invalid(press_pull(s, &Region::Face(0), 0.0)));
        assert!(matches!(pick(s, Probe::Point(Vec3::ZERO), -1.0), Err(KernelError::Invalid(_))));
        assert!(matches!(section(s, Vec3::ZERO, Vec3::ZERO), Err(KernelError::Invalid(_))));
        assert!(matches!(flatshot(&[], Vec3::Z, v(1.0, 0.0, 0.0)), Err(KernelError::Invalid(_))));
        assert!(matches!(interference(&[], &[]), Err(KernelError::Invalid(_))));
        assert!(invalid(mesh_shape(&[Vec3::ZERO], &[[0, 0, 7]], true, false)) && invalid(mesh_shape(&[], &[], true, false)));
        assert!(invalid(mesh_shape(&[v(f64::NAN, 0.0, 0.0)], &[[0, 0, 0]], true, false)));
        assert!(invalid(sew(&[], true)) && invalid(imprint(s, &[], &[])));
        // Not B-rep text; a transform the kernel can't apply.
        assert!(invalid(clean(Placed { brep: "garbage", xform: &id })));
        let mut shear = Mat4::IDENTITY;
        shear.m[0][1] = 0.5;
        assert!(matches!(separate(Placed { brep: "CASCADE Topology", xform: &shear }), Err(KernelError::Invalid(_))));
        // Curves: open regions, holes off the plane, a polysolid without width or path.
        let sq = |z: f64| {
            let c = [v(0.0, 0.0, z), v(1.0, 0.0, z), v(1.0, 1.0, z), v(0.0, 1.0, z)];
            Curve::Segs((0..4).map(|i| Seg::Line { a: c[i], b: c[(i + 1) % 4] }).collect())
        };
        let open = Curve::Segs(vec![Seg::Line { a: Vec3::ZERO, b: v(1.0, 0.0, 0.0) }]);
        assert!(invalid(planar_surface(&open, &[])) && invalid(planar_surface(&sq(0.0), &[sq(1.0)])));
        assert!(invalid(press_pull(s, &Region::Profile { outer: open.clone(), holes: Vec::new() }, 1.0)));
        let line = [Seg::Line { a: Vec3::ZERO, b: v(1.0, 0.0, 0.0) }];
        assert!(invalid(polysolid(&line, 0.0, 1.0, Justify::Left, Vec3::Z)) && invalid(polysolid(&[], 1.0, 1.0, Justify::Left, Vec3::Z)));
        assert!(invalid(polysolid(&line, 1.0, 1.0, Justify::Left, v(1.0, 0.0, 0.0))), "up along the path");
        assert!(invalid(ruled_surface(&sq(0.0), &open)) && invalid(tabulated_surface(&open, Vec3::ZERO)));
        assert!(
            invalid(revolved_surface(&open, Vec3::ZERO, Vec3::Z, 0.0, 0.0))
                && invalid(edge_surface(&[sq(0.0), open.clone(), open.clone(), open.clone()]))
        );
        assert!(
            invalid(loft_along(&[sq(0.0), open.clone()], &open))
                && invalid(loft_guided(&[sq(0.0)], std::slice::from_ref(&open)))
                && invalid(loft_guided(&[sq(0.0), sq(1.0)], &[]))
        );
    }
}
