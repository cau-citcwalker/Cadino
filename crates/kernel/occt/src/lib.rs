//! The OpenCASCADE operations `cadcraft-kernel` needs that the `opencascade` crate doesn't bind:
//! our own C++ (`src/ops.cpp`) behind a cxx bridge. Shapes cross as OpenCASCADE B-rep text with a
//! placement, curves and results as plain data, so no C++ object ever reaches Rust.
//!
//! Every function catches what OpenCASCADE throws (see `include/cadrhino_occt.hxx`) and returns
//! it as an `Err`; a crash inside OpenCASCADE still ends the process, which is why
//! `cadcraft-kernel` runs these in its worker process. Indices of faces and edges count from 0 in
//! `TopExp::MapShapes` order of the shape read from its B-rep text.
//!
//! Empty on wasm (OpenCASCADE is native only).
#![cfg(not(target_arch = "wasm32"))]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub use cxx::Exception;
pub use ffi::*;

// The only unsafe code: cxx needs the C++ functions declared in an `unsafe extern "C++"` block.
// They take and return plain data and catch every exception, so calling them is safe.
#[allow(unsafe_code)]
#[cxx::bridge(namespace = "cadrhino")]
mod ffi {
    /// One piece of a curve: `kind` 0 a line `[a, b]`, 1 an arc `[a, mid, b]`, 2 a circle
    /// `[centre, normal, radius]`, 3 a spline `[tangents given (0/1), start tangent, end tangent,
    /// points…]` (x, y, z each).
    #[derive(Clone, Debug, Default)]
    struct SegIn {
        kind: u8,
        v: Vec<f64>,
    }

    /// Pieces joined end to end into one wire.
    #[derive(Clone, Debug, Default)]
    struct WireIn {
        segs: Vec<SegIn>,
    }

    /// A stored shape: its B-rep text and model → world placement (a similarity, rows of 3 × 4).
    #[derive(Clone, Debug)]
    struct ShapeIn {
        brep: String,
        xf: [f64; 12],
    }

    /// A curve handed back: `kind` 0 line, 1 arc, 2 circle, 3 ellipse, 4 spline (a NURBS curve:
    /// `degree`, `poles` (xyz), `weights` (empty: not rational), `knots` written out). `a`, `mid`
    /// and `b` are its start, its middle (by parameter) and its end; `points` a polyline along it.
    #[derive(Clone, Debug, Default)]
    struct CurveOut {
        kind: u8,
        a: [f64; 3],
        mid: [f64; 3],
        b: [f64; 3],
        center: [f64; 3],
        normal: [f64; 3],
        major: [f64; 3],
        radius: f64,
        ratio: f64,
        start: f64,
        end: f64,
        degree: u32,
        poles: Vec<f64>,
        weights: Vec<f64>,
        knots: Vec<f64>,
        points: Vec<f64>,
    }

    /// A face: `kind` is OpenCASCADE's GeomAbs_SurfaceType (0 plane … 10 other); `origin` and
    /// `axis` the plane's point and normal or the round surface's axis; `edges` its edges' indices.
    #[derive(Clone, Debug, Default)]
    struct FaceOut {
        kind: u8,
        area: f64,
        centroid: [f64; 3],
        normal: [f64; 3],
        lo: [f64; 3],
        hi: [f64; 3],
        origin: [f64; 3],
        axis: [f64; 3],
        radius: f64,
        edges: Vec<u32>,
    }

    #[derive(Clone, Debug, Default)]
    struct EdgeOut {
        curve: CurveOut,
        length: f64,
        faces: Vec<u32>,
        seam: bool,
        degenerate: bool,
    }

    #[derive(Clone, Debug, Default)]
    struct TopoOut {
        faces: Vec<FaceOut>,
        edges: Vec<EdgeOut>,
    }

    #[derive(Clone, Debug, Default)]
    struct HitOut {
        index: u32,
        point: [f64; 3],
        along: f64,
        distance: f64,
    }

    #[derive(Clone, Debug, Default)]
    struct PickOut {
        faces: Vec<HitOut>,
        edges: Vec<HitOut>,
    }

    #[derive(Clone, Debug, Default)]
    struct WireOut {
        curves: Vec<CurveOut>,
        closed: bool,
    }

    /// A planar region: its face as B-rep text, and its outer wire then its holes.
    #[derive(Clone, Debug, Default)]
    struct RegionOut {
        brep: String,
        wires: Vec<WireOut>,
    }

    #[derive(Clone, Debug, Default)]
    struct SectionOut {
        regions: Vec<RegionOut>,
        wires: Vec<WireOut>,
    }

    #[derive(Clone, Debug, Default)]
    struct FlatOut {
        visible: Vec<CurveOut>,
        hidden: Vec<CurveOut>,
    }

    #[derive(Clone, Debug, Default)]
    struct ClashOut {
        a: u32,
        b: u32,
        brep: String,
        volume: f64,
    }

    #[derive(Clone, Debug, Default)]
    struct CheckOut {
        valid: bool,
        messages: Vec<String>,
    }

    unsafe extern "C++" {
        include!("cadrhino_occt.hxx");

        fn topology(s: &ShapeIn) -> Result<TopoOut>;
        /// `dir` all zero: the faces and edges within `tol` of the point `origin`.
        fn pick(s: &ShapeIn, origin: &[f64], dir: &[f64], tol: f64) -> Result<PickOut>;
        fn fillet_edges(s: &ShapeIn, edges: &[u32], radii: &[f64]) -> Result<String>;
        /// `faces[i]` < 0: the first face on the edge.
        fn chamfer_edges(s: &ShapeIn, edges: &[u32], d1: &[f64], d2: &[f64], faces: &[i32]) -> Result<String>;
        fn extrude_faces(s: &ShapeIn, faces: &[u32], distance: f64, taper: f64) -> Result<String>;
        fn offset_faces(s: &ShapeIn, faces: &[u32], distance: f64) -> Result<String>;
        /// `xf`: a rigid motion, rows of 3 × 4.
        fn transform_faces(s: &ShapeIn, faces: &[u32], xf: &[f64]) -> Result<String>;
        fn taper_faces(s: &ShapeIn, faces: &[u32], base: &[f64], dir: &[f64], angle: f64) -> Result<String>;
        fn delete_faces(s: &ShapeIn, faces: &[u32]) -> Result<String>;
        fn copy_faces(s: &ShapeIn, faces: &[u32]) -> Result<Vec<String>>;
        fn imprint(s: &ShapeIn, tools: &[ShapeIn], curves: &[WireIn]) -> Result<String>;
        fn separate(s: &ShapeIn) -> Result<Vec<String>>;
        fn shell_faces(s: &ShapeIn, faces: &[u32], thickness: f64) -> Result<String>;
        fn clean(s: &ShapeIn) -> Result<String>;
        fn check(s: &ShapeIn) -> Result<CheckOut>;
        fn press_pull(s: &ShapeIn, outer: &WireIn, holes: &[WireIn], distance: f64) -> Result<String>;
        fn thicken(s: &ShapeIn, thickness: f64, both: bool) -> Result<String>;
        fn section(s: &ShapeIn, origin: &[f64], normal: &[f64]) -> Result<SectionOut>;
        fn flatshot(shapes: &[ShapeIn], dir: &[f64], xdir: &[f64]) -> Result<FlatOut>;
        /// `second` empty: every pair within `first`.
        fn interfere(first: &[ShapeIn], second: &[ShapeIn]) -> Result<Vec<ClashOut>>;
        fn sew(shapes: &[ShapeIn], solid: bool) -> Result<String>;
        fn mesh_shape(verts: &[f64], tris: &[u32], solid: bool, merge: bool) -> Result<String>;
        fn to_surface(s: &ShapeIn) -> Result<String>;
        fn split(s: &ShapeIn, tools: &[ShapeIn]) -> Result<Vec<String>>;
        fn ruled(a: &WireIn, b: &WireIn) -> Result<String>;
        fn tabulated(w: &WireIn, dir: &[f64]) -> Result<String>;
        fn revolved(w: &WireIn, origin: &[f64], axis: &[f64], start: f64, angle: f64) -> Result<String>;
        fn coons(edges: &[WireIn]) -> Result<String>;
        fn planar_face(outer: &WireIn, holes: &[WireIn]) -> Result<String>;
        /// `guides`: none (the spine is a path through the sections) or one (kept in contact).
        fn pipe_loft(sections: &[WireIn], spine: &WireIn, solid: bool) -> Result<String>;
        fn guided_loft(sections: &[WireIn], guides: &[WireIn], solid: bool) -> Result<String>;
    }
}
