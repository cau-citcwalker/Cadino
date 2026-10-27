//! GPU canvas: draws the drawing's display list with an `egui_wgpu` paint callback.
//!
//! The display list is converted once per (document, revision, tessellation band, background,
//! lineweight display, vertex origin) into vertex data ([`MeshData`]) and uploaded to wgpu
//! buffers; each frame only a small view uniform changes. Positions are stored as f32 relative to
//! a per-build origin so large world coordinates keep their precision; at deep zoom the origin
//! follows the view (see [`choose_origin`]).
//!
//! Batches: filled triangles (TriangleList), 1-px hairlines (LineList), and instanced screen-space
//! quads for wide lineweights and point markers. Fills are drawn first, then lines on top. A
//! wipeout starts a new draw-order group (see [`MeshData::breaks`]): its background-coloured mask
//! is drawn after everything before it and under everything after it. The shader only uses
//! vertex buffers and one uniform buffer, so it runs on WebGPU and WebGL2.
//!
//! Depth: with a depth buffer in the render pass ([`GpuTarget::stencil`] a depth-stencil format),
//! everything is depth-tested (less or equal) and only solids' shaded faces and wipeouts write
//! depth ([`MeshData::writes`]), pushed back a little so lines on them stay visible. Shaded model
//! space carries vertex depths ([`DisplayList::vert_depth`]); every other list, the highlight
//! and the drag sit at the nearest depth, so they pass and keep their drawing order as in 2D. A
//! wide line's corners take the XY plane's depth where they are ([`MeshData::slope`]), so the
//! whole width passes over a sloped wipeout. The solids' edges ([`DisplayList::edges`], built
//! for Shaded with Edges) are drawn only with a depth buffer, which hides the hidden ones. The
//! pass clears depth every frame, as it does the stencil.
//!
//! Infinite lines (xline/ray) and all overlays stay egui shapes (see `canvas.rs`), and the
//! canvas falls back to the CPU path when no wgpu render state was handed to the app.

use std::sync::{Arc, Mutex};

use cadcraft_color::Rgb;
use cadcraft_geom::{Bounds2, Vec2};
use cadcraft_render::{DisplayList, Kind};
use egui_wgpu::wgpu;
use egui_wgpu::wgpu::util::DeviceExt;

/// Line/triangle vertex: position (2 × f32, relative to the origin) + sRGB colour (4 × u8,
/// premultiplied by its alpha: transparent objects blend with what is under them) + depth (f32,
/// 0 = nearest, see [`Depth`]).
// ponytail: every drawing pays for depth, 2D ones too: 16 bytes a vertex instead of 12 and 36
// an instance instead of 28 (a third more vertex memory); split depth into its own buffer, bound
// only for lists with depth, if vertex memory on huge 2D drawings matters.
const VERTEX_SIZE: usize = 16;
/// Wide-line / point instance: a, b (2 × f32 each) + colour (4 × u8) + width in points (f32) +
/// length along its polyline before `a`, in world units (f32; dashing) + the depths of a and b
/// (2 × f32).
const INSTANCE_SIZE: usize = 36;
/// Vertices (or instances) per GPU buffer; divisible by 2 and 3 so no primitive straddles two.
const CHUNK: usize = 4_194_300;
/// Point markers are 1.5 × 1.5 points (as on the CPU path).
const POINT_SIZE: f32 = 1.5;
/// Vertex depths of a list span clip-space depth [`DEPTH_NEAR`] (nearest) to `DEPTH_NEAR +
/// DEPTH_SPAN`, leaving room behind the farthest for the faces' depth bias, and in front of the
/// nearest for wide lines' corners, which reach past their ends (see [`MeshData::slope`]).
/// Lists without depth sit at 0, in front of everything.
const DEPTH_NEAR: f64 = 0.25;
const DEPTH_SPAN: f64 = 0.5;
/// How far faces that write depth are pushed back: a few depth steps plus two pixels of their
/// depth slope, so an edge or line lying on a face (rasterised up to a pixel off it) passes the
/// depth test, while what lies behind the face by any real distance stays hidden.
// ponytail: depths are spread over the list's whole depth range and the bias is a constant 4
// steps of a 24-bit buffer, so a drawing about 1e7 units deep resolves about 1 unit per step and
// a line a few units behind a face shows through; spread the visible range only, or use a
// 32-bit float buffer with reversed depth, if huge 3D drawings matter.
const FACE_BIAS: wgpu::DepthBiasState = wgpu::DepthBiasState { constant: 4, slope_scale: 2.0, clamp: 0.0 };

/// How the native window presents frames (`NativeOptions::wgpu_options.surface`).
///
/// The canvas hides the OS pointer and draws its own crosshair, so every frame the swapchain holds
/// back is visible as cursor lag. With FIFO (vsync) wgpu asks for one queued frame, but Vulkan
/// clamps that to the driver's minimum swapchain size, which is three images on X11 (two frames
/// queued behind the one on screen; Windows DX12 honours one). `low_latency` (the
/// app passes it on Linux/BSD) prefers a mode that never queues: `AutoNoVsync` picks Immediate,
/// then Mailbox, then falls back to FIFO, so it is valid on every surface; under a compositor the
/// compositor still syncs to the display. `vsync` is the `CADCRAFT_VSYNC` override: `1`/`on`
/// forces FIFO (tear-free without a compositor), `0`/`off` forces the low-latency mode.
pub fn surface_config(vsync: Option<&str>, low_latency: bool) -> egui_wgpu::SurfaceConfig {
    let no_vsync = match vsync.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("1" | "on" | "true" | "yes") => false,
        Some("0" | "off" | "false" | "no") => true,
        _ => low_latency,
    };
    egui_wgpu::SurfaceConfig {
        present_mode: if no_vsync { wgpu::PresentMode::AutoNoVsync } else { wgpu::PresentMode::AutoVsync },
        desired_maximum_frame_latency: Some(1),
    }
}

/// Frame-rate cap for presenting without vsync. Nothing else paces frames then (on X11; Wayland
/// still throttles to the compositor), so a 1000 Hz mouse made the app render ~900 frames per
/// second and keep a CPU core busy. Waiting out the rest of a short minimum interval adds at most
/// that interval of input age, far less than a vsync queue.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug)]
pub struct FrameCap {
    min: std::time::Duration,
    next: Option<std::time::Instant>,
}

#[cfg(not(target_arch = "wasm32"))]
impl FrameCap {
    /// At most `max_fps` frames per second.
    pub fn new(max_fps: u32) -> Self {
        FrameCap { min: std::time::Duration::from_secs(1) / max_fps.max(1), next: None }
    }

    /// How long a frame starting at `now` waits; records when the following one may start.
    pub fn wait_at(&mut self, now: std::time::Instant) -> std::time::Duration {
        let wait = self.next.map_or(std::time::Duration::ZERO, |n| n.saturating_duration_since(now));
        self.next = now.checked_add(wait.saturating_add(self.min));
        wait
    }

    /// Block until this frame may start (call once per frame, at its start).
    pub fn wait(&mut self) {
        let wait = self.wait_at(std::time::Instant::now());
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
}

/// The wgpu target the app renders into, recorded by [`install`].
#[derive(Clone, Copy, Debug)]
pub struct GpuTarget {
    pub format: wgpu::TextureFormat,
    /// The stencil buffer format of the render pass, when it has one (the native window asks
    /// for one): clipped viewports are clipped with it. A depth-stencil format
    /// (`Depth24PlusStencil8`) also depth-tests shaded 3D views.
    pub stencil: Option<wgpu::TextureFormat>,
}

/// CPU-side vertex data for one display list, waiting to be uploaded.
#[derive(Default)]
pub struct MeshData {
    pub lines: Vec<u8>,
    pub tris: Vec<u8>,
    pub quads: Vec<u8>,
    /// Draw-order breaks, one before each wipeout mask: the fill vertices, line vertices and
    /// quad instances drawn before it. Each group is drawn fills, lines, quads in turn.
    pub breaks: Vec<[usize; 3]>,
    /// Fill vertex ranges (in order) that write depth: opaque shaded faces and wipeout masks of
    /// a list with depths. Other fills, lines and quads only test it.
    pub writes: Vec<std::ops::Range<usize>>,
    /// Line vertices at the end of `lines` drawn only with a depth buffer: the solids' edges
    /// over shaded faces ([`DisplayList::edges`]).
    pub edge_lines: usize,
    /// Clip-space depth per world unit of x and y on the XY plane ([`DisplayList::plane_slope`];
    /// zero for lists without depth). Wide lines and point markers (2D objects, on the plane)
    /// give each quad corner the plane's depth there, not the depth at the line's centre, so a
    /// sloped wipeout or face can't cut off one side of a wide line.
    pub slope: [f32; 2],
    /// The vertex buffers already created from this data (see [`MeshData::upload_now`]); the
    /// paint callback then only swaps them in.
    uploaded: Option<Uploaded>,
}

/// GPU buffers made ahead of the frame that draws them.
struct Uploaded {
    lines: Vec<Batch>,
    tris: Vec<Batch>,
    quads: Vec<Batch>,
}

impl MeshData {
    /// Create the vertex buffers now, on the calling thread, and let the CPU copy go. Making a
    /// buffer zero-fills and copies it: about 0.35 s for a million-primitive drawing, which a
    /// background rebuild does off the UI thread (see `canvas::rebuild`).
    pub fn upload_now(&mut self, device: &wgpu::Device) {
        let usage = wgpu::BufferUsages::VERTEX;
        self.uploaded = Some(Uploaded {
            lines: upload(device, "cad_lines", &self.lines, VERTEX_SIZE, usage),
            tris: upload(device, "cad_fills", &self.tris, VERTEX_SIZE, usage),
            quads: upload(device, "cad_quads", &self.quads, INSTANCE_SIZE, usage),
        });
        self.lines = Vec::new();
        self.tris = Vec::new();
        self.quads = Vec::new();
    }

    pub fn line_vertices(&self) -> usize {
        self.lines.len() / VERTEX_SIZE
    }
    pub fn tri_vertices(&self) -> usize {
        self.tris.len() / VERTEX_SIZE
    }
    pub fn quad_instances(&self) -> usize {
        self.quads.len() / INSTANCE_SIZE
    }
}

fn rel(p: Vec2, origin: Vec2) -> [f32; 2] {
    [(p.x - origin.x) as f32, (p.y - origin.y) as f32]
}

fn push_vertex(out: &mut Vec<u8>, p: [f32; 2], c: [u8; 4], z: f32) {
    out.extend_from_slice(&p[0].to_le_bytes());
    out.extend_from_slice(&p[1].to_le_bytes());
    out.extend_from_slice(&c);
    out.extend_from_slice(&z.to_le_bytes());
}

/// A wide line (or a point marker, `a` = `b`) from `a` at depth `z[0]` to `b` at `z[1]`.
fn push_instance(out: &mut Vec<u8>, a: [f32; 2], b: [f32; 2], c: [u8; 4], width: f32, start: f32, z: [f32; 2]) {
    for v in [a[0], a[1], b[0], b[1]] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&c);
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&z[0].to_le_bytes());
    out.extend_from_slice(&z[1].to_le_bytes());
}

/// A display list's vertex depths ([`DisplayList::vert_depth`], [`DisplayList::tri_depth`]) as
/// clip-space depth: [`DEPTH_NEAR`] for the nearest vertex, `DEPTH_NEAR + DEPTH_SPAN` for the
/// farthest. A list without depths, or whose depths don't match its vertices, is all at 0.
struct Depth<'a> {
    verts: &'a [f64],
    tris: &'a [f64],
    near: f64,
    scale: f64,
}

impl<'a> Depth<'a> {
    fn of(list: &'a DisplayList) -> Self {
        let verts = if list.vert_depth.len() == list.verts.len() { list.vert_depth.as_slice() } else { &[] };
        let tris = if list.tri_depth.len() == list.tris.len() { list.tri_depth.as_slice() } else { &[] };
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for d in verts.iter().chain(tris).filter(|d| d.is_finite()) {
            lo = lo.min(*d);
            hi = hi.max(*d);
        }
        let scale = if hi > lo && (hi - lo).is_finite() { DEPTH_SPAN / (hi - lo) } else { 0.0 };
        Depth { verts, tris, near: hi, scale }
    }
    /// Whether the list has depths (then its faces and masks write depth).
    fn any(&self) -> bool {
        !self.verts.is_empty() || !self.tris.is_empty()
    }
    fn z(&self, d: Option<f64>) -> f32 {
        match d {
            Some(d) if d.is_finite() && self.scale > 0.0 => (DEPTH_NEAR + ((self.near - d) * self.scale).clamp(0.0, DEPTH_SPAN)) as f32,
            _ => 0.0,
        }
    }
    /// [`MeshData::slope`] for the XY plane's depth slope `g` ([`DisplayList::plane_slope`]):
    /// nearer is smaller. Zero without depths, or when it doesn't fit an f32.
    fn slope(&self, g: [f64; 2]) -> [f32; 2] {
        let s = g.map(|v| (-v * self.scale) as f32);
        if s.iter().all(|v| v.is_finite()) { s } else { [0.0; 2] }
    }
    fn vert(&self, i: usize) -> f32 {
        self.z(self.verts.get(i).copied())
    }
    fn tri(&self, i: usize) -> f32 {
        self.z(self.tris.get(i).copied())
    }
}

/// Lineweight in screen points for a primitive (as on the CPU path), or `None` for a hairline.
pub fn wide_width(lw: f32, lwdisplay: bool) -> Option<f32> {
    if !lwdisplay || lw <= 0.0 {
        return None;
    }
    let w = (lw * 3.78).clamp(1.0, 12.0);
    (w > 1.0).then_some(w)
}

/// `c` with alpha `a`, premultiplied (as the pipelines blend).
fn premultiplied(c: Rgb, a: u8) -> [u8; 4] {
    let k = |v: u8| u8::try_from((u32::from(v) * u32::from(a) + 127) / 255).unwrap_or(v);
    [k(c.0), k(c.1), k(c.2), a]
}

/// Convert a display list into vertex data relative to `origin`, colours resolved against `bg`.
pub fn build_mesh(list: &DisplayList, origin: Vec2, bg: Rgb, lwdisplay: bool) -> MeshData {
    let mut m = MeshData {
        lines: Vec::with_capacity(list.segment_count().saturating_mul(2 * VERTEX_SIZE)),
        tris: Vec::with_capacity(list.tris.len().saturating_mul(VERTEX_SIZE)),
        ..MeshData::default()
    };
    let depth = Depth::of(list);
    m.slope = depth.slope(list.plane_slope);
    // The shaded faces among the primitives (ranges in order).
    let mut faces = list.faces.iter().peekable();
    for (i, prim) in list.prims.iter().enumerate() {
        let col = premultiplied(prim.display_rgb(bg), prim.alpha());
        let pts = list.points(prim);
        let at = prim.start as usize;
        match prim.kind {
            Kind::Polyline => match wide_width(prim.lw, lwdisplay) {
                Some(w) => {
                    for (k, s) in pts.windows(2).enumerate() {
                        if let [a, b] = s {
                            let z = [depth.vert(at + k), depth.vert(at + k + 1)];
                            push_instance(&mut m.quads, rel(*a, origin), rel(*b, origin), col, w, 0.0, z);
                        }
                    }
                }
                None => {
                    for (k, s) in pts.windows(2).enumerate() {
                        if let [a, b] = s {
                            push_vertex(&mut m.lines, rel(*a, origin), col, depth.vert(at + k));
                            push_vertex(&mut m.lines, rel(*b, origin), col, depth.vert(at + k + 1));
                        }
                    }
                }
            },
            Kind::Tris | Kind::Mask => {
                let mask = prim.kind == Kind::Mask;
                while faces.next_if(|r| r.end as usize <= i).is_some() {}
                let face = faces.peek().is_some_and(|r| r.start as usize <= i) && prim.alpha() == u8::MAX;
                // A wipeout starts a draw-order group; hidden faces (masks among the faces, the
                // Hidden style) hide by depth instead.
                if mask && !(face && depth.any()) {
                    m.breaks.push([m.tri_vertices(), m.line_vertices(), m.quad_instances()]);
                }
                let col = if mask { [bg.0, bg.1, bg.2, 255] } else { col };
                let from = m.tri_vertices();
                for (k, t) in pts.as_chunks::<3>().0.iter().enumerate() {
                    for (j, q) in t.iter().enumerate() {
                        push_vertex(&mut m.tris, rel(*q, origin), col, depth.tri(at + 3 * k + j));
                    }
                }
                let to = m.tri_vertices();
                // ponytail: other fills (hatches, filled text, solids' transparent faces) only
                // test depth, so they keep their 2D order and never z-fight; a solid drawn later
                // and lying behind one (below z = 0) draws over it. Write their depth too, with
                // a per-run order bias, if that matters.
                if (face || mask) && depth.any() && from < to {
                    match m.writes.last_mut() {
                        Some(w) if w.end == from => w.end = to,
                        _ => m.writes.push(from..to),
                    }
                }
            }
            Kind::Point => {
                if let Some(q) = pts.first() {
                    let p = rel(*q, origin);
                    let z = depth.vert(at);
                    push_instance(&mut m.quads, p, p, col, POINT_SIZE, 0.0, [z, z]);
                }
            }
            // Drawn on the CPU each frame (clipped to the view).
            Kind::Infinite { .. } => {}
        }
    }
    // ponytail: edges are hairlines whatever their lineweight; quads with depth if it matters.
    if depth.any() {
        let before = m.line_vertices();
        for prim in &list.edges {
            let col = premultiplied(prim.display_rgb(bg), prim.alpha());
            let at = prim.start as usize;
            for (k, s) in list.points(prim).windows(2).enumerate() {
                if let [a, b] = s {
                    push_vertex(&mut m.lines, rel(*a, origin), col, depth.vert(at + k));
                    push_vertex(&mut m.lines, rel(*b, origin), col, depth.vert(at + k + 1));
                }
            }
        }
        m.edge_lines = m.line_vertices() - before;
    }
    m
}

/// Vertex data drawing primitives `prims` of `list` in `color` (the selection highlight), as on
/// the CPU path: lines `width` points wide, and fills filled, or with `dashed` (SELECTIONEFFECT 0;
/// the callback's `dash` dashes the lines) outlined. A wipeout is always outlined, since filling it
/// would hide the drawing; infinite lines are left to the CPU (they are clipped per frame).
/// A solid's shaded faces keep their shading, as in Rhino: its outline against the view and its
/// edges light up instead, the edges from `edges` (the selected solids built with them, for a
/// plain Shaded list), else from `list` (Shaded with Edges, Hidden). Everything sits at the list's
/// own depths, so in a depth-buffered 3D view what the faces hide stays hidden; other lists sit at
/// the nearest depth, over everything.
pub fn build_highlight_mesh(
    list: &DisplayList,
    prims: &[usize],
    origin: Vec2,
    color: [u8; 4],
    width: f32,
    dashed: bool,
    edges: Option<&DisplayList>,
) -> MeshData {
    let mut m = MeshData::default();
    let depth = Depth::of(list);
    let mut solids = std::collections::HashSet::new();
    let quads = |m: &mut MeshData, pts: &[Vec2], z: &dyn Fn(usize) -> f32| {
        let mut run = 0.0;
        for (k, s) in pts.windows(2).enumerate() {
            if let [a, b] = s {
                push_instance(&mut m.quads, rel(*a, origin), rel(*b, origin), color, width, run as f32, [z(k), z(k + 1)]);
                run += a.dist(*b);
            }
        }
    };
    for &i in prims {
        let Some(prim) = list.prims.get(i) else { continue };
        let pts = list.points(prim);
        let at = prim.start as usize;
        let face = is_face(list, i);
        match prim.kind {
            Kind::Polyline => quads(&mut m, pts, &|k| depth.vert(at + k)),
            Kind::Point => {
                if let Some(q) = pts.first() {
                    let (p, z) = (rel(*q, origin), depth.vert(at));
                    push_instance(&mut m.quads, p, p, color, 3.0, 0.0, [z, z]);
                }
            }
            Kind::Tris if !dashed && !face => {
                let fill = translucent(color, HIGHLIGHT_FILL_ALPHA);
                for (j, q) in pts.as_chunks::<3>().0.iter().flatten().enumerate() {
                    push_vertex(&mut m.tris, rel(*q, origin), fill, depth.tri(at + j));
                }
            }
            // Outlined with the solid's other faces, below.
            Kind::Tris | Kind::Mask if face => {
                solids.insert(prim.handle);
            }
            Kind::Tris | Kind::Mask => {
                // The outline's points are the triangles' own corners: they keep their depths.
                let z: std::collections::HashMap<(u64, u64), f32> = pts.iter().enumerate().map(|(j, q)| (bits(q), depth.tri(at + j))).collect();
                for line in list.fill_outlines(prim) {
                    quads(&mut m, &line, &|k| line.get(k).and_then(|q| z.get(&bits(q))).copied().unwrap_or(0.0));
                }
            }
            Kind::Infinite { .. } => {}
        }
    }
    for line in face_outlines(list, prims) {
        let pts: Vec<Vec2> = line.iter().map(|p| p.0).collect();
        quads(&mut m, &pts, &|k| line.get(k).and_then(|p| p.1).map_or(0.0, |t| depth.tri(t)));
    }
    let from = edges.unwrap_or(list);
    if depth.any() {
        for e in from.edges.iter().filter(|e| solids.contains(&e.handle)) {
            let at = e.start as usize;
            quads(&mut m, from.points(e), &|k| depth.z(from.vert_depth.get(at + k).copied()));
        }
    }
    m
}

/// A point's key: its bits, -0.0 as 0.0 (as the outlines have them).
fn bits(q: &Vec2) -> (u64, u64) {
    ((q.x + 0.0).to_bits(), (q.y + 0.0).to_bits())
}

/// The outlines against the view of the solids whose shaded faces are among primitives `prims`
/// of `list`, each point with its index in `list.tris` (its depth): a solid's faces are outlined
/// together, so neither the edges between their triangles (drawn in depth order, in many runs) nor
/// a curved face's shading bands show.
pub(crate) fn face_outlines(list: &DisplayList, prims: &[usize]) -> Vec<Vec<(Vec2, Option<usize>)>> {
    let mut groups: std::collections::HashMap<cadcraft_doc::Handle, (Rgb, Vec<usize>)> = std::collections::HashMap::new();
    for &i in prims {
        let Some(p) = list.prims.get(i).filter(|p| is_face(list, i) && matches!(p.kind, Kind::Tris | Kind::Mask)) else { continue };
        let at = p.start as usize;
        groups.entry(p.handle).or_insert((p.color, Vec::new())).1.extend(at..at + list.points(p).len() / 3 * 3);
    }
    let mut out = Vec::new();
    for (handle, (color, idx)) in groups {
        let tris: Vec<Vec2> = idx.iter().filter_map(|&k| list.tris.get(k).copied()).collect();
        let at: std::collections::HashMap<(u64, u64), usize> = tris.iter().zip(&idx).map(|(q, &k)| (bits(q), k)).collect();
        let Ok(len) = u32::try_from(tris.len()) else { continue };
        let one = cadcraft_render::DPrim { handle, color, aci7: false, lw: 0.0, transparency: 0, kind: Kind::Tris, start: 0, len };
        let solid = DisplayList { tris, ..Default::default() };
        out.extend(solid.fill_outlines(&one).into_iter().map(|line| line.into_iter().map(|q| (q, at.get(&bits(&q)).copied())).collect()));
    }
    out
}

/// Whether primitive `i` of `list` is one of a solid's shaded faces ([`DisplayList::faces`]).
pub(crate) fn is_face(list: &DisplayList, i: usize) -> bool {
    let k = list.faces.partition_point(|r| r.end as usize <= i);
    list.faces.get(k).is_some_and(|r| r.start as usize <= i)
}

/// The vertex origin for a display list seen at `view_center` / `view_height`: the list's centre
/// normally; at deep zoom (the view much smaller than the drawing) a point near the view, snapped
/// to a coarse grid so panning re-uploads only every few dozen screens.
pub fn choose_origin(bounds: &Bounds2, view_center: Vec2, view_height: f64) -> Vec2 {
    let size = bounds.width().max(bounds.height());
    if bounds.is_empty()
        || !view_center.is_finite()
        || view_height.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)
        || view_height * 2000.0 >= size
    {
        return if bounds.is_empty() { Vec2::ZERO } else { bounds.center() };
    }
    let step = view_height * 64.0;
    Vec2::new((view_center.x / step).round() * step, (view_center.y / step).round() * step)
}

/// Pending upload shared between the canvas (producer) and the paint callback (consumer).
pub type MeshSlot = Arc<Mutex<Option<MeshData>>>;

/// Per-frame parameters for the paint callback.
pub struct CanvasCallback {
    /// Identifies the mesh in `slot`; the GPU buffers are rebuilt when it changes.
    pub key: u64,
    pub slot: MeshSlot,
    /// (origin − view centre) in world units.
    pub offset: [f32; 2],
    /// Points per world unit.
    pub scale: f32,
    /// Canvas rect centre in points.
    pub center: [f32; 2],
    /// Which buffers to draw: [`LAYER_DRAWING`], [`LAYER_DRAG`], [`LAYER_HIGHLIGHT`] or
    /// [`LAYER_VIEWPORT`].
    pub layer: usize,
    /// Affine transform applied to the origin-relative positions before `offset`: the linear
    /// part (a, b, c, d as in `Mat3`) and the translation. [`IDENTITY_XFORM`] for the drawing.
    pub xform: [f32; 6],
    /// Dash period in points for wide lines (0 = solid): the selection highlight.
    pub dash: f32,
    /// How the draw uses the stencil buffer.
    pub stencil: Stencil,
}

/// How a [`CanvasCallback`] uses the stencil buffer ([`GpuTarget::stencil`]): a clipped
/// viewport's outline is drawn as a triangle fan that flips a stencil bit ([`Stencil::Mask`],
/// even-odd, as the outline clips on the CPU), then its contents draw only where the bit is set
/// ([`Stencil::Inside`]). The stencil is cleared every frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stencil {
    #[default]
    Off,
    Mask,
    Inside,
}

/// The drawing's display list.
pub const LAYER_DRAWING: usize = 0;
/// The objects a MOVE/COPY/ROTATE/SCALE preview drags, uploaded once per drag.
pub const LAYER_DRAG: usize = 1;
/// The selection highlight, rebuilt only when the selection or the drawing changes.
pub const LAYER_HIGHLIGHT: usize = 2;
/// Model space seen through the active layout viewport, moved to its current view each frame.
pub const LAYER_VIEWPORT: usize = 3;
/// The outline of the active viewport when it is clipped (stencil only, see [`Stencil`]).
pub const LAYER_MASK: usize = 4;

/// Opacity of filled areas in the selection and hover highlight: see-through, so the lines and
/// text on (and under) a highlighted hatch stay visible, as the reference app's dot pattern does.
pub const HIGHLIGHT_FILL_ALPHA: f32 = 0.5;

/// `c` at `alpha` opacity, premultiplied (as the pipelines blend).
fn translucent(c: [u8; 4], alpha: f32) -> [u8; 4] {
    let a = alpha.clamp(0.0, 1.0);
    let m = |v: u8| (f32::from(v) * a).round() as u8;
    [m(c[0]), m(c[1]), m(c[2]), m(c[3])]
}
pub const IDENTITY_XFORM: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

struct Batch {
    buffer: wgpu::Buffer,
    count: u32,
}

/// One uploaded mesh with its own view uniform (each callback's transform differs).
struct Layer {
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    key: Option<u64>,
    lines: Vec<Batch>,
    tris: Vec<Batch>,
    quads: Vec<Batch>,
    breaks: Vec<[usize; 3]>,
    writes: Vec<std::ops::Range<usize>>,
    edge_lines: usize,
    slope: [f32; 2],
}

struct Resources {
    line_pipeline: wgpu::RenderPipeline,
    tri_pipeline: wgpu::RenderPipeline,
    quad_pipeline: wgpu::RenderPipeline,
    /// Fills that write depth ([`MeshData::writes`]); the fill pipeline without a depth buffer.
    face_pipeline: wgpu::RenderPipeline,
    /// With a stencil buffer: the pipelines of [`Stencil::Mask`] and [`Stencil::Inside`].
    clipped: Option<ClipPipelines>,
    /// The render pass has a depth buffer.
    depth: bool,
    linear_out: bool,
    layers: Vec<Layer>,
}

struct ClipPipelines {
    /// Flips the stencil bit under each triangle; draws no colour.
    mask: wgpu::RenderPipeline,
    line: wgpu::RenderPipeline,
    tri: wgpu::RenderPipeline,
    quad: wgpu::RenderPipeline,
    face: wgpu::RenderPipeline,
}

const UNIFORM_SIZE: u64 = 80;

const SHADER: &str = r#"
struct View {
    a: vec4<f32>, // offset x, offset y (world), scale (device px per unit), 1 = linear output
    b: vec4<f32>, // canvas centre x, y (device px), screen w, h (device px)
    c: vec4<f32>, // pixels per point, dash period in points (0 = solid), XY-plane depth per world x, y
    d: vec4<f32>, // transform linear part: a, b, c, d (x' = a x + c y, y' = b x + d y)
    e: vec4<f32>, // transform translation x, y (world, origin-relative)
};
@group(0) @binding(0) var<uniform> view: View;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Device pixels along a wide line from its start (dashing).
    @location(1) along: f32,
};

fn to_px(p: vec2<f32>) -> vec2<f32> {
    let q = vec2<f32>(view.d.x * p.x + view.d.z * p.y, view.d.y * p.x + view.d.w * p.y) + view.e.xy;
    let w = (q + view.a.xy) * view.a.z;
    return vec2<f32>(view.b.x + w.x, view.b.y - w.y);
}

fn px_to_clip(px: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(px.x / view.b.z * 2.0 - 1.0, 1.0 - px.y / view.b.w * 2.0, 0.0, 1.0);
}

// `px_to_clip` at depth `z` (0 = nearest).
fn px_to_clip_at(px: vec2<f32>, z: f32) -> vec4<f32> {
    var c = px_to_clip(px);
    c.z = z;
    return c;
}

// `c` is premultiplied sRGB; a linear target gets it linearised, still premultiplied.
fn out_color(c: vec4<f32>) -> vec4<f32> {
    if (view.a.w > 0.5) {
        let s = c.rgb / max(c.a, 0.0001);
        let lo = s / 12.92;
        let hi = pow((s + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
        return vec4<f32>(select(hi, lo, s <= vec3<f32>(0.04045)) * c.a, c.a);
    }
    return c;
}

@vertex
fn vs_line(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>, @location(2) z: f32) -> VOut {
    var o: VOut;
    o.pos = px_to_clip_at(to_px(pos), z);
    o.color = out_color(color);
    o.along = 0.0;
    return o;
}

@vertex
fn vs_quad(
    @builtin(vertex_index) vi: u32,
    @location(0) a: vec2<f32>,
    @location(1) b: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) width: f32,
    @location(4) start: f32,
    @location(5) za: f32,
    @location(6) zb: f32,
) -> VOut {
    let pa = to_px(a);
    let pb = to_px(b);
    let d = pb - pa;
    let len = length(d);
    var dir = vec2<f32>(1.0, 0.0);
    if (len > 0.0001) {
        dir = d / len;
    }
    let n = vec2<f32>(-dir.y, dir.x);
    // Width arrives in points.
    let hw = 0.5 * width * view.c.x;
    let t = select(0.0, 1.0, vi == 1u || vi == 4u || vi == 5u);
    let s = select(-1.0, 1.0, vi == 2u || vi == 3u || vi == 5u);
    let along = mix(pa - dir * hw, pb + dir * hw, t);
    let corner = along + n * hw * s;
    // The corner at the XY plane's depth there (wide lines are 2D objects): its offset from the
    // line's end in world units (y up) times the plane's depth slope. Lists without depth have
    // no slope. ponytail: the offset ignores the layer's transform; only the untransformed
    // drawing carries depth, undo the transform here if a transformed layer ever does.
    let off = corner - mix(pa, pb, t);
    let z = mix(za, zb, t) + dot(view.c.zw, vec2<f32>(off.x, -off.y) / max(view.a.z, 1e-20));
    var o: VOut;
    o.pos = px_to_clip_at(corner, clamp(z, 0.0, 1.0));
    o.color = out_color(color);
    o.along = start * view.a.z + mix(-hw, len + hw, t);
    return o;
}

@fragment
fn fs_main(v: VOut) -> @location(0) vec4<f32> {
    if (view.c.y > 0.0) {
        // 6 on, 4 off (as the CPU selection highlight's dashes).
        if (fract(v.along / (view.c.y * view.c.x)) > 0.6) {
            discard;
        }
    }
    return v.color;
}
"#;

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![0 => Float32x2, 1 => Unorm8x4, 2 => Float32];
    wgpu::VertexBufferLayout { array_stride: VERTEX_SIZE as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &ATTRS }
}

fn instance_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 7] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Unorm8x4, 3 => Float32, 4 => Float32, 5 => Float32, 6 => Float32];
    wgpu::VertexBufferLayout { array_stride: INSTANCE_SIZE as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: &ATTRS }
}

impl Resources {
    fn new(device: &wgpu::Device, target: GpuTarget) -> Self {
        let module =
            device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("cad_canvas"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("cad_canvas"), entries: &[entry(0)] });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cad_canvas"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let layer = || {
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cad_canvas_view"),
                size: UNIFORM_SIZE,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cad_canvas"),
                layout: &bgl,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
            });
            Layer {
                uniform,
                bind_group,
                key: None,
                lines: Vec::new(),
                tris: Vec::new(),
                quads: Vec::new(),
                breaks: Vec::new(),
                writes: Vec::new(),
                edge_lines: 0,
                slope: [0.0; 2],
            }
        };
        // A render pass with a stencil buffer needs every pipeline to name its format; only the
        // clipped-viewport pipelines read or write it. With a depth buffer every pipeline tests
        // depth and only `write` ones (faces) write it.
        let depth = target.stencil.is_some_and(|f| f.has_depth_aspect());
        let face = |compare, pass_op| wgpu::StencilFaceState {
            compare,
            fail_op: wgpu::StencilOperation::Keep,
            depth_fail_op: wgpu::StencilOperation::Keep,
            pass_op,
        };
        let stencil_state = |s: Stencil, write: bool| {
            target.stencil.map(|format| {
                let stencil = match s {
                    Stencil::Off => wgpu::StencilState::default(),
                    Stencil::Mask => {
                        let f = face(wgpu::CompareFunction::Always, wgpu::StencilOperation::Invert);
                        wgpu::StencilState { front: f, back: f, read_mask: 1, write_mask: 1 }
                    }
                    Stencil::Inside => {
                        let f = face(wgpu::CompareFunction::Equal, wgpu::StencilOperation::Keep);
                        wgpu::StencilState { front: f, back: f, read_mask: 1, write_mask: 0 }
                    }
                };
                let (depth_write_enabled, depth_compare, bias) = match (depth, write) {
                    (true, true) => (Some(true), Some(wgpu::CompareFunction::LessEqual), FACE_BIAS),
                    (true, false) => (Some(false), Some(wgpu::CompareFunction::LessEqual), wgpu::DepthBiasState::default()),
                    (false, _) => (None, None, wgpu::DepthBiasState::default()),
                };
                wgpu::DepthStencilState { format, depth_write_enabled, depth_compare, stencil, bias }
            })
        };
        let pipeline =
            |label: &str, vs: &str, topology: wgpu::PrimitiveTopology, buffer: wgpu::VertexBufferLayout<'static>, s: Stencil, write: bool| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some(vs),
                        buffers: &[Some(buffer)],
                        compilation_options: Default::default(),
                    },
                    primitive: wgpu::PrimitiveState { topology, ..Default::default() },
                    depth_stencil: stencil_state(s, write),
                    multisample: wgpu::MultisampleState { count: 1, mask: !0, alpha_to_coverage_enabled: false },
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some("fs_main"),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: target.format,
                            blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                            write_mask: if s == Stencil::Mask { wgpu::ColorWrites::empty() } else { wgpu::ColorWrites::ALL },
                        })],
                        compilation_options: Default::default(),
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
        use wgpu::PrimitiveTopology::{LineList, TriangleList};
        Resources {
            line_pipeline: pipeline("cad_lines", "vs_line", LineList, vertex_layout(), Stencil::Off, false),
            tri_pipeline: pipeline("cad_fills", "vs_line", TriangleList, vertex_layout(), Stencil::Off, false),
            quad_pipeline: pipeline("cad_quads", "vs_quad", TriangleList, instance_layout(), Stencil::Off, false),
            face_pipeline: pipeline("cad_faces", "vs_line", TriangleList, vertex_layout(), Stencil::Off, true),
            clipped: target.stencil.map(|_| ClipPipelines {
                mask: pipeline("cad_clip_mask", "vs_line", TriangleList, vertex_layout(), Stencil::Mask, false),
                line: pipeline("cad_clip_lines", "vs_line", LineList, vertex_layout(), Stencil::Inside, false),
                tri: pipeline("cad_clip_fills", "vs_line", TriangleList, vertex_layout(), Stencil::Inside, false),
                quad: pipeline("cad_clip_quads", "vs_quad", TriangleList, instance_layout(), Stencil::Inside, false),
                face: pipeline("cad_clip_faces", "vs_line", TriangleList, vertex_layout(), Stencil::Inside, true),
            }),
            depth,
            linear_out: target.format.is_srgb(),
            layers: (0..=LAYER_MASK).map(|_| layer()).collect(),
        }
    }
}

fn upload(device: &wgpu::Device, label: &str, bytes: &[u8], stride: usize, usage: wgpu::BufferUsages) -> Vec<Batch> {
    bytes
        .chunks(CHUNK * stride)
        .filter(|c| c.len() >= stride)
        .map(|c| Batch {
            buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: c, usage }),
            count: u32::try_from(c.len() / stride).unwrap_or(0),
        })
        .collect()
}

/// GPU trouble wgpu reported: uncaptured errors (validation, out of memory) and the device being
/// lost (a driver reset or a hung GPU). wgpu's default handler panics on the first uncaptured
/// error, closing the app with any unsaved work (#602); the handlers [`install`] registers record
/// them here instead, and the app autosaves and tells the user (see `CadApp::gpu_fault_tick`).
#[derive(Debug, Default)]
pub struct GpuFaults {
    state: Mutex<FaultState>,
}

#[derive(Debug, Default)]
struct FaultState {
    errors: u64,
    errors_seen: u64,
    lost: bool,
    lost_seen: bool,
    last: String,
}

/// What [`GpuFaults::take`] reports.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuFault {
    /// Uncaptured errors so far (an invalid resource can fail again every frame).
    pub errors: u64,
    /// The device was lost: nothing more can be drawn until the app restarts.
    pub lost: bool,
    /// The latest error or device-lost message.
    pub message: String,
}

impl GpuFaults {
    /// Errors logged in full before only every 1000th is (a repeating error would flood the log).
    const LOGGED: u64 = 10;

    pub fn error(&self, message: String) {
        let Ok(mut s) = self.state.lock() else { return };
        s.errors += 1;
        if s.errors <= Self::LOGGED || s.errors % 1000 == 0 {
            log::error!("wgpu error #{}: {message}", s.errors);
        }
        s.last = message;
    }

    pub fn device_lost(&self, reason: wgpu::DeviceLostReason, message: String) {
        // `Destroyed` is the device being dropped on purpose (the app closing), not a failure.
        if reason == wgpu::DeviceLostReason::Destroyed {
            return;
        }
        log::error!("wgpu device lost ({reason:?}): {message}");
        let Ok(mut s) = self.state.lock() else { return };
        s.lost = true;
        s.last = message;
    }

    /// Faults reported since the last call, if any.
    pub fn take(&self) -> Option<GpuFault> {
        let Ok(mut s) = self.state.lock() else { return None };
        if s.errors == s.errors_seen && s.lost == s.lost_seen {
            return None;
        }
        s.errors_seen = s.errors;
        s.lost_seen = s.lost;
        Some(GpuFault { errors: s.errors, lost: s.lost, message: s.last.clone() })
    }
}

/// Create the canvas pipelines in the renderer's callback resources, and report wgpu errors and
/// device loss to `faults` instead of panicking. Call once with the app's render state (eframe
/// `CreationContext::wgpu_render_state`) and the format of its render pass's stencil buffer,
/// if it has one (eframe `NativeOptions::stencil_buffer`; with `depth_buffer` too, the combined
/// depth-stencil format, which depth-tests shaded 3D views).
pub fn install(rs: &egui_wgpu::RenderState, faults: &Arc<GpuFaults>, stencil: Option<wgpu::TextureFormat>) -> GpuTarget {
    let f = Arc::clone(faults);
    rs.device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| f.error(e.to_string())));
    let f = Arc::clone(faults);
    rs.device.set_device_lost_callback(move |reason, message| f.device_lost(reason, message));
    let target = GpuTarget { format: rs.target_format, stencil: stencil.filter(|s| s.has_stencil_aspect()) };
    let res = Resources::new(&rs.device, target);
    rs.renderer.write().callback_resources.insert(res);
    log::info!("gpu canvas: target {:?}, depth/stencil {:?}", rs.target_format, target.stencil);
    target
}

impl egui_wgpu::CallbackTrait for CanvasCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(res) = resources.get_mut::<Resources>() else { return Vec::new() };
        let linear_out = res.linear_out;
        let Some(res) = res.layers.get_mut(self.layer) else { return Vec::new() };
        if res.key != Some(self.key) {
            let pending = self.slot.lock().ok().and_then(|mut s| s.take());
            if let Some(mut m) = pending {
                if m.uploaded.is_none() {
                    m.upload_now(device);
                }
                if let Some(u) = m.uploaded {
                    (res.lines, res.tris, res.quads) = (u.lines, u.tris, u.quads);
                }
                res.breaks = m.breaks;
                res.writes = m.writes;
                res.edge_lines = m.edge_lines;
                res.slope = m.slope;
                res.key = Some(self.key);
            }
        }
        let ppp = screen.pixels_per_point;
        let [w, h] = screen.size_in_pixels;
        let a = [self.offset[0], self.offset[1], self.scale * ppp, if linear_out { 1.0 } else { 0.0 }];
        let b = [self.center[0] * ppp, self.center[1] * ppp, w.max(1) as f32, h.max(1) as f32];
        let c = [ppp, self.dash, res.slope[0], res.slope[1]];
        let [ma, mb, mc, md, tx, ty] = self.xform;
        let d = [ma, mb, mc, md];
        let e = [tx, ty, 0.0, 0.0];
        let bytes: Vec<u8> = a.iter().chain(&b).chain(&c).chain(&d).chain(&e).flat_map(|v| v.to_le_bytes()).collect();
        queue.write_buffer(&res.uniform, 0, &bytes);
        Vec::new()
    }

    fn paint(&self, info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        let Some(shared) = resources.get::<Resources>() else { return };
        let Some(res) = shared.layers.get(self.layer) else { return };
        if res.key != Some(self.key) {
            return;
        }
        let [sw, sh] = info.screen_size_px;
        if sw == 0 || sh == 0 {
            return;
        }
        // Whole-screen device pixels; egui already set the scissor to the canvas clip rect.
        pass.set_viewport(0.0, 0.0, sw as f32, sh as f32, 0.0, 1.0);
        pass.set_bind_group(0, &res.bind_group, &[]);
        let (tri, line, quad, face) = match (self.stencil, &shared.clipped) {
            (Stencil::Off, _) => (&shared.tri_pipeline, &shared.line_pipeline, &shared.quad_pipeline, &shared.face_pipeline),
            // The outline is all triangles.
            (Stencil::Mask, Some(c)) => (&c.mask, &c.mask, &c.mask, &c.mask),
            (Stencil::Inside, Some(c)) => (&c.tri, &c.line, &c.quad, &c.face),
            // No stencil buffer: the canvas never asks for these.
            (_, None) => return,
        };
        if self.stencil != Stencil::Off {
            pass.set_stencil_reference(1);
        }
        let mask = self.stencil == Stencil::Mask;
        // Draw-order groups split at each wipeout; without wipeouts there is one group.
        let total = |bs: &[Batch]| bs.iter().map(|b| b.count as usize).sum::<usize>();
        // Without a depth buffer the solids' edges at the end of the lines would all show.
        let lines = total(&res.lines).saturating_sub(if shared.depth { 0 } else { res.edge_lines });
        let all = [total(&res.tris), if mask { 0 } else { lines }, if mask { 0 } else { total(&res.quads) }];
        let mut from = [0usize; 3];
        for to in res.breaks.iter().copied().chain(std::iter::once(all)) {
            // Fills in runs: faces that write depth and the rest, in drawing order.
            for (r, write) in write_runs(from[0]..to[0], &res.writes) {
                draw_range(pass, if write { face } else { tri }, &res.tris, r, false);
            }
            draw_range(pass, line, &res.lines, from[1]..to[1], false);
            draw_range(pass, quad, &res.quads, from[2]..to[2], true);
            from = to;
        }
    }
}

/// Draw `range` of the vertices (or, `instanced`, quad instances) laid end to end in `batches`.
fn draw_range(
    pass: &mut wgpu::RenderPass<'static>,
    pipeline: &wgpu::RenderPipeline,
    batches: &[Batch],
    range: std::ops::Range<usize>,
    instanced: bool,
) {
    let parts = batch_ranges(batches.iter().map(|b| b.count), range);
    if parts.is_empty() {
        return;
    }
    pass.set_pipeline(pipeline);
    for (j, r) in parts {
        let Some(b) = batches.get(j) else { continue };
        pass.set_vertex_buffer(0, b.buffer.slice(..));
        // Quads are instanced: six vertices per instance.
        if instanced { pass.draw(0..6, r) } else { pass.draw(r, 0..1) }
    }
}

/// `range` of the fill vertices split into runs that write depth (inside one of `writes`, which
/// are in order) and runs that don't, in order.
fn write_runs(range: std::ops::Range<usize>, writes: &[std::ops::Range<usize>]) -> Vec<(std::ops::Range<usize>, bool)> {
    let mut out = Vec::new();
    let mut at = range.start;
    let first = writes.partition_point(|w| w.end <= range.start);
    for w in writes.iter().skip(first).take_while(|w| w.start < range.end) {
        let (lo, hi) = (w.start.max(at), w.end.min(range.end));
        if lo >= hi {
            continue;
        }
        if at < lo {
            out.push((at..lo, false));
        }
        out.push((lo..hi, true));
        at = hi;
    }
    if at < range.end {
        out.push((at..range.end, false));
    }
    out
}

/// For batches holding `counts` vertices (or instances) each: the batch indices and the ranges
/// within them that cover `range` of the batches laid end to end.
fn batch_ranges(counts: impl IntoIterator<Item = u32>, range: std::ops::Range<usize>) -> Vec<(usize, std::ops::Range<u32>)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    for (j, n) in counts.into_iter().enumerate() {
        let n = n as usize;
        let (lo, hi) = (range.start.max(off), range.end.min(off + n));
        if lo < hi {
            out.push((j, (lo - off) as u32..(hi - off) as u32));
        }
        off += n;
    }
    out
}

#[cfg(test)]
// Vertex ranges as data: one range is a list of one, and a backwards one is hostile input.
#[allow(clippy::single_range_in_vec_init, clippy::reversed_empty_ranges)]
mod tests {
    use super::*;
    use cadcraft_doc::Handle;
    use cadcraft_render::DPrim;

    #[test]
    fn surface_config_prefers_low_latency_where_asked_and_honours_override() {
        use wgpu::PresentMode::{AutoNoVsync, AutoVsync};
        assert_eq!(surface_config(None, true).present_mode, AutoNoVsync);
        assert_eq!(surface_config(None, false).present_mode, AutoVsync);
        for on in ["1", "on", " TRUE ", "yes"] {
            assert_eq!(surface_config(Some(on), true).present_mode, AutoVsync, "{on}");
        }
        for off in ["0", "Off", "false", "no"] {
            assert_eq!(surface_config(Some(off), false).present_mode, AutoNoVsync, "{off}");
        }
        assert_eq!(surface_config(Some("maybe"), true).present_mode, AutoNoVsync);
        assert_eq!(surface_config(Some(""), false).present_mode, AutoVsync);
        assert_eq!(surface_config(None, true).desired_maximum_frame_latency, Some(1));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn frame_cap_waits_out_the_minimum_interval_only() {
        use std::time::{Duration, Instant};
        let mut cap = FrameCap::new(250); // 4 ms
        let t0 = Instant::now();
        assert_eq!(cap.wait_at(t0), Duration::ZERO, "first frame never waits");
        assert_eq!(cap.wait_at(t0 + Duration::from_millis(1)), Duration::from_millis(3));
        // That frame started at 4 ms; the next one 10 ms later is long past the interval.
        assert_eq!(cap.wait_at(t0 + Duration::from_millis(14)), Duration::ZERO);
        assert_eq!(cap.wait_at(t0 + Duration::from_millis(18)), Duration::ZERO);
        assert_eq!(FrameCap::new(0).min, Duration::from_secs(1));
    }

    fn list() -> DisplayList {
        let mut l = DisplayList {
            verts: vec![Vec2::new(1e6, 1e6), Vec2::new(1e6 + 1.0, 1e6), Vec2::new(1e6 + 1.0, 1e6 + 1.0), Vec2::new(5.0, 5.0)],
            tris: vec![Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)],
            ..Default::default()
        };
        let p = |kind, start, len, lw| DPrim { handle: Handle(1), color: Rgb(255, 255, 255), aci7: true, lw, transparency: 0, kind, start, len };
        l.prims =
            vec![p(Kind::Polyline, 0, 3, 0.0), p(Kind::Tris, 0, 3, 0.0), p(Kind::Point, 3, 1, 0.0), p(Kind::Infinite { ray: false }, 0, 2, 0.0)];
        for v in l.verts.clone() {
            l.bounds.add(v);
        }
        l
    }

    #[test]
    fn highlight_dashes_run_on_along_each_polyline() {
        let l = list();
        // The 3-point polyline (two unit segments) and the point marker.
        let m = build_highlight_mesh(&l, &[0, 2], Vec2::new(1e6, 1e6), [1, 2, 3, 255], 1.5, true, None);
        assert_eq!(m.quad_instances(), 3);
        let start = |i: usize| {
            let o = i * INSTANCE_SIZE + 24;
            f32::from_le_bytes([m.quads[o], m.quads[o + 1], m.quads[o + 2], m.quads[o + 3]])
        };
        assert_eq!([start(0), start(1), start(2)], [0.0, 1.0, 0.0]);
        assert!(m.lines.is_empty() && m.tris.is_empty());
    }

    #[test]
    fn highlight_fills_fills_and_outlines_wipeouts() {
        let mut l = list();
        // The filled triangle (0,0) (1,0) (0,1) is filled in the highlight colour...
        let m = build_highlight_mesh(&l, &[1], Vec2::ZERO, [1, 2, 3, 255], 1.5, false, None);
        assert_eq!((m.tri_vertices(), m.quad_instances()), (3, 0));
        assert_eq!(&m.tris[8..12], &[1, 1, 2, 128], "in the highlight colour, see-through (premultiplied)");
        // ...or, dashed (SELECTIONEFFECT 0), outlined by its three edges, the dash running on.
        let m = build_highlight_mesh(&l, &[1], Vec2::ZERO, [1, 2, 3, 255], 1.5, true, None);
        assert_eq!((m.tri_vertices(), m.quad_instances()), (0, 3));
        let start = |i: usize| {
            let o = i * INSTANCE_SIZE + 24;
            f32::from_le_bytes([m.quads[o], m.quads[o + 1], m.quads[o + 2], m.quads[o + 3]])
        };
        let mut runs = [start(0), start(1), start(2)];
        runs.sort_by(f32::total_cmp);
        assert!(runs[0] == 0.0 && runs[1] > 0.9 && runs[2] > runs[1] + 0.9, "{runs:?}");
        // A wipeout would hide the drawing if filled: outlined either way.
        if let Some(p) = l.prims.get_mut(1) {
            p.kind = Kind::Mask;
        }
        let m = build_highlight_mesh(&l, &[1], Vec2::ZERO, [1, 2, 3, 255], 1.5, false, None);
        assert_eq!((m.tri_vertices(), m.quad_instances()), (0, 3));
    }

    /// A selected solid keeps its shading (no tint over its faces, as in Rhino): its faces'
    /// outline and its edges light up instead, each point at its own depth.
    #[test]
    fn a_selected_solid_lights_its_outline_and_edges_not_its_faces() {
        let l = shaded_list();
        let m = build_highlight_mesh(&l, &[0], Vec2::ZERO, [255, 255, 0, 255], 2.0, false, None);
        assert_eq!(m.tri_vertices(), 0, "no tint over the shaded face");
        assert_eq!(m.quad_instances(), 4 + 3, "the square face's four sides and the solid's three edges");
        let at = |i: usize, o: usize| f32_at(&m.quads, i * INSTANCE_SIZE + o);
        let corner = (0..m.quad_instances()).find(|&i| (at(i, 0), at(i, 4)) == (16.0, 16.0)).unwrap();
        assert_eq!(at(corner, 28), 0.5, "(16, 16) lies at depth 3 of 0..=6, as the face draws it");
        // The face drawn in two runs (its triangles sort by depth among the solid's others): still
        // its four sides, no diagonal between its triangles.
        let mut split = shaded_list();
        let second = DPrim { start: 3, len: 3, ..split.prims[0] };
        split.prims[0].len = 3;
        split.prims.insert(1, second);
        split.faces = vec![0..2];
        assert_eq!(build_highlight_mesh(&split, &[0, 1], Vec2::ZERO, [255, 255, 0, 255], 2.0, false, None).quad_instances(), 4 + 3);
        // A plain Shaded list has no edges: they come from the solids built with them, at the
        // list's depths.
        let mut plain = shaded_list();
        let built = DisplayList {
            edges: std::mem::take(&mut plain.edges),
            verts: plain.verts.clone(),
            vert_depth: plain.vert_depth.clone(),
            ..Default::default()
        };
        assert_eq!(build_highlight_mesh(&plain, &[0], Vec2::ZERO, [255, 255, 0, 255], 2.0, false, None).quad_instances(), 4, "the outline alone");
        let m2 = build_highlight_mesh(&plain, &[0], Vec2::ZERO, [255, 255, 0, 255], 2.0, false, Some(&built));
        let edges = |m: &MeshData| m.quads.get(4 * INSTANCE_SIZE..).map(<[u8]>::to_vec);
        assert_eq!(edges(&m2), edges(&m), "the same edges (after the outline) at the same depths");
        // A fill that isn't a solid's face (a hatch) is still filled.
        let mut flat = shaded_list();
        flat.faces.clear();
        assert_eq!(build_highlight_mesh(&flat, &[0], Vec2::ZERO, [255, 255, 0, 255], 2.0, false, None).tri_vertices(), 6);
    }

    #[test]
    fn mesh_counts_and_precision() {
        let l = list();
        let origin = Vec2::new(1e6, 1e6);
        let m = build_mesh(&l, origin, Rgb(255, 255, 255), false);
        assert_eq!(m.line_vertices(), 4, "two segments");
        assert_eq!(m.tri_vertices(), 3);
        assert_eq!(m.quad_instances(), 1, "the point marker");
        // First vertex is stored relative to the origin, so it's exactly zero.
        assert_eq!(&m.lines[0..8], &[0u8; 8]);
        // Colour 7 on a white background inverts to black (display_rgb).
        assert_eq!(&m.lines[8..12], &[0, 0, 0, 255]);
        // A list without depths is all at the nearest depth: drawn in order, as in 2D.
        assert_eq!(f32_at(&m.lines, 12), 0.0);
        assert_eq!(f32_at(&m.lines, VERTEX_SIZE), 1.0, "the second vertex's x");
        assert!(m.writes.is_empty() && m.edge_lines == 0);
    }

    fn f32_at(bytes: &[u8], o: usize) -> f32 {
        f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]])
    }

    /// A shaded 3D view's list: a face sloped both ways (depth 1 + (x - 16) / 16 + (48 - y) / 16,
    /// 1 to 5), a line across the view behind it (depth 0), a line in front (depth 6), edges on
    /// the face (its diagonal, and one off the pixel centres, which only the depth bias keeps in
    /// front of it) and an edge behind it, and a wipeout. Grey lines, black face.
    fn shaded_list() -> DisplayList {
        let v = Vec2::new;
        let (grey, black) = (Rgb(128, 128, 128), Rgb(0, 0, 0));
        let p = |kind, color, start, len| DPrim { handle: Handle(1), color, aci7: false, lw: 0.0, transparency: 0, kind, start, len };
        let face = [v(16.0, 16.0), v(48.0, 16.0), v(48.0, 48.0), v(16.0, 16.0), v(48.0, 48.0), v(16.0, 48.0)];
        let mask = [v(56.0, 56.0), v(60.0, 56.0), v(60.0, 60.0)];
        let on = |q: Vec2| 1.0 + (q.x - 16.0) / 16.0 + (48.0 - q.y) / 16.0;
        let verts = [
            (0.0, 31.5),
            (64.0, 31.5),
            (40.5, 0.0),
            (40.5, 64.0),
            (16.0, 16.0),
            (48.0, 48.0),
            (20.0, 39.7),
            (44.0, 39.7),
            (16.0, 27.5),
            (48.0, 27.5),
        ]
        .map(|(x, y)| v(x, y));
        let behind = |q: Vec2| on(q) - 0.5;
        let mut l = DisplayList {
            vert_depth: vec![0.0, 0.0, 6.0, 6.0, on(verts[4]), on(verts[5]), on(verts[6]), on(verts[7]), behind(verts[8]), behind(verts[9])],
            verts: verts.to_vec(),
            tri_depth: face.iter().map(|q| on(*q)).chain([2.0; 3]).collect(),
            tris: face.iter().chain(&mask).copied().collect(),
            prims: vec![p(Kind::Tris, black, 0, 6), p(Kind::Polyline, grey, 0, 2), p(Kind::Polyline, grey, 2, 2), p(Kind::Mask, black, 6, 3)],
            faces: vec![0..1],
            edges: vec![p(Kind::Polyline, grey, 4, 2), p(Kind::Polyline, grey, 6, 2), p(Kind::Polyline, grey, 8, 2)],
            ..Default::default()
        };
        l.bounds = Bounds2::new(v(0.0, 0.0), v(64.0, 64.0));
        l
    }

    #[test]
    fn depths_run_from_the_nearest_and_faces_write() {
        let l = shaded_list();
        let m = build_mesh(&l, Vec2::ZERO, Rgb(255, 255, 255), false);
        // Depths 0..=6 span 0.75..=0.25 (nearest first).
        let z = |bytes: &[u8], i: usize| f32_at(bytes, i * VERTEX_SIZE + 12);
        assert_eq!([z(&m.lines, 0), z(&m.lines, 2)], [0.75, 0.25], "the line behind, the one in front");
        assert_eq!(z(&m.tris, 0), 0.5, "the face's corner at depth 3");
        assert!((z(&m.tris, 1) - (0.25 + 1.0 / 12.0)).abs() < 1e-7, "the one at depth 5");
        assert_eq!(m.slope, [0.0; 2], "no plane slope given");
        // The face and the wipeout write depth (one run: they follow each other); the edges
        // follow the lines.
        assert_eq!(m.writes, vec![0..9]);
        assert_eq!(m.breaks, vec![[6, 4, 0]]);
        assert_eq!((m.line_vertices(), m.edge_lines), (10, 6));
        // A transparent face only tests depth, like a hatch.
        let mut t = shaded_list();
        t.prims[0].transparency = 50;
        assert_eq!(build_mesh(&t, Vec2::ZERO, Rgb(255, 255, 255), false).writes, vec![6..9]);
        // Depths that don't match the vertices are dropped: all nearest, nothing written.
        let mut bad = shaded_list();
        bad.tri_depth.pop();
        bad.vert_depth.clear();
        let m = build_mesh(&bad, Vec2::ZERO, Rgb(255, 255, 255), false);
        assert!(m.writes.is_empty() && m.edge_lines == 0);
        assert!((0..m.tri_vertices()).all(|i| z(&m.tris, i) == 0.0));
        // Wide lines carry the depth of both ends, and the plane's slope for their corners:
        // nearer is smaller, over the 6 units of depth the list spans.
        let mut wide = shaded_list();
        wide.prims[1].lw = 1.0;
        wide.plane_slope = [1.0, -2.0];
        let m = build_mesh(&wide, Vec2::ZERO, Rgb(255, 255, 255), true);
        assert_eq!([f32_at(&m.quads, 28), f32_at(&m.quads, 32)], [0.75, 0.75]);
        assert_eq!(m.slope, [-0.5 / 6.0, 1.0 / 6.0].map(|v: f64| v as f32));
        // A slope too steep for an f32, or a list without depth: no slope.
        wide.plane_slope = [f64::MAX, 0.0];
        assert_eq!(build_mesh(&wide, Vec2::ZERO, Rgb(255, 255, 255), true).slope, [0.0; 2]);
        wide.plane_slope = [f64::NAN, 1.0];
        assert_eq!(build_mesh(&wide, Vec2::ZERO, Rgb(255, 255, 255), true).slope, [0.0; 2]);
        let mut flat = list();
        flat.plane_slope = [1.0, 1.0];
        assert_eq!(build_mesh(&flat, Vec2::ZERO, Rgb(255, 255, 255), true).slope, [0.0; 2]);
    }

    #[test]
    fn write_runs_split_fills_at_the_faces() {
        assert_eq!(write_runs(0..10, &[]), vec![(0..10, false)]);
        assert_eq!(write_runs(0..10, &[2..4, 6..12]), vec![(0..2, false), (2..4, true), (4..6, false), (6..10, true)]);
        assert_eq!(write_runs(3..5, &[0..4, 4..9]), vec![(3..4, true), (4..5, true)]);
        assert!(write_runs(4..4, &[0..9]).is_empty());
        // Hostile ranges (backwards, out of order) draw every vertex once, in order.
        assert_eq!(write_runs(0..6, &[5..2, 4..6, 1..3]), vec![(0..4, false), (4..6, true)]);
    }

    /// Render `list` into a white 64 × 64 target with a `format` depth/stencil buffer, one
    /// world unit per pixel, lineweights shown or not (`lwdisplay`); the sum of each pixel's
    /// colour channels, row by row from the top.
    #[allow(clippy::unwrap_used)]
    fn render(list: &DisplayList, format: wgpu::TextureFormat, lwdisplay: bool) -> Vec<u32> {
        const SIZE: u32 = 64;
        let rs = egui_kittest::wgpu::create_render_state(egui_kittest::wgpu::default_wgpu_setup(), Default::default());
        let faults = Arc::new(GpuFaults::default());
        install(&rs, &faults, Some(format));
        let (device, queue) = (&rs.device, &rs.queue);
        let extent = wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 };
        let texture = |format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let color = texture(rs.target_format, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC);
        let ds = texture(format, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let (color_view, ds_view) = (color.create_view(&Default::default()), ds.create_view(&Default::default()));
        let slot: MeshSlot = Arc::default();
        *slot.lock().unwrap() = Some(build_mesh(list, Vec2::ZERO, Rgb(255, 255, 255), lwdisplay));
        let cb = CanvasCallback {
            key: 1,
            slot,
            offset: [-32.0, -32.0],
            scale: 1.0,
            center: [32.0, 32.0],
            layer: LAYER_DRAWING,
            xform: IDENTITY_XFORM,
            dash: 0.0,
            stencil: Stencil::Off,
        };
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [SIZE, SIZE], pixels_per_point: 1.0 };
        let mut encoder = device.create_command_encoder(&Default::default());
        let mut renderer = rs.renderer.write();
        let _ = egui_wgpu::CallbackTrait::prepare(&cb, device, queue, &screen, &mut encoder, &mut renderer.callback_resources);
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &color_view,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::WHITE), store: wgpu::StoreOp::Store },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &ds_view,
                        depth_ops: format
                            .has_depth_aspect()
                            .then_some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                        stencil_ops: format
                            .has_stencil_aspect()
                            .then_some(wgpu::Operations { load: wgpu::LoadOp::Clear(0), store: wgpu::StoreOp::Discard }),
                    }),
                    ..Default::default()
                })
                .forget_lifetime();
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SIZE as f32, SIZE as f32));
            let info = egui::PaintCallbackInfo { viewport: rect, clip_rect: rect, pixels_per_point: 1.0, screen_size_px: [SIZE, SIZE] };
            egui_wgpu::CallbackTrait::paint(&cb, info, &mut pass, &renderer.callback_resources);
        }
        let row = SIZE * 4;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(row * SIZE),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            color.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(SIZE) },
            },
            extent,
        );
        queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        let px: Vec<u32> =
            buffer.slice(..).get_mapped_range().unwrap().chunks(4).map(|p| u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])).collect();
        assert_eq!(faults.take(), None, "wgpu reported nothing");
        px
    }

    #[test]
    fn depth_buffer_hides_what_is_behind_shaded_faces() {
        // Screen (x, y) is world (x, 64 - y); 0 = black (the face), about 384 = grey (a line),
        // 765 = white.
        let at = |px: &[u32], x: usize, y: usize| px[y * 64 + x];
        let (black, grey, white) = (|v: u32| v < 100, |v: u32| (200..600).contains(&v), |v: u32| v > 700);
        let px = render(&shaded_list(), wgpu::TextureFormat::Depth24PlusStencil8, false);
        assert!(grey(at(&px, 8, 32)) && black(at(&px, 24, 32)), "the line behind the face is hidden by it");
        assert!(grey(at(&px, 40, 8)) && grey(at(&px, 40, 20)), "the line in front shows over the face");
        // The edges on the sloped face show all along them; the one behind it stays hidden.
        for x in [20, 26, 32, 38, 44] {
            let y = 64 - x;
            assert!(grey(at(&px, x, y)) || grey(at(&px, x, y - 1)) || grey(at(&px, x, y + 1)), "diagonal edge at {x}");
        }
        // World y 39.7 is screen row 24, whose pixel centres lie 0.2 units up the slope, where
        // the face is nearer than the edge: shown only thanks to the bias.
        for x in [22, 28, 34] {
            assert!(grey(at(&px, x, 24)), "edge off the pixel centres at {x}: {}", at(&px, x, 24));
        }
        assert!(black(at(&px, 24, 36)) && black(at(&px, 30, 36)), "the hidden edge");
        assert!(white(at(&px, 4, 4)));
        // Without a depth buffer: drawing order as before (lines over fills), no edges.
        let px = render(&shaded_list(), wgpu::TextureFormat::Stencil8, false);
        assert!(grey(at(&px, 24, 32)), "the line draws over the face");
        assert!(black(at(&px, 30, 36)) && black(at(&px, 28, 24)) && black(at(&px, 26, 38)), "no edges");
    }

    /// A shaded 3D view of 2D objects: a wipeout over most of the view and, after it, a grey
    /// line 12 points wide along x at y 32, both on the XY plane, which comes nearer toward the
    /// top (depth y / 16).
    fn wide_line_over_wipeout() -> DisplayList {
        let v = Vec2::new;
        let g = [0.0, 1.0 / 16.0];
        let p = |kind, start, len, lw| DPrim { handle: Handle(1), color: Rgb(128, 128, 128), aci7: false, lw, transparency: 0, kind, start, len };
        let tris = vec![v(4.0, 4.0), v(60.0, 4.0), v(60.0, 60.0), v(4.0, 4.0), v(60.0, 60.0), v(4.0, 60.0)];
        let verts = vec![v(8.0, 32.0), v(56.0, 32.0)];
        let depth = |q: &Vec2| g[0] * q.x + g[1] * q.y;
        let mut l = DisplayList {
            vert_depth: verts.iter().map(depth).collect(),
            tri_depth: tris.iter().map(depth).collect(),
            verts,
            tris,
            prims: vec![p(Kind::Mask, 0, 6, 0.0), p(Kind::Polyline, 0, 2, 3.2)],
            plane_slope: g,
            ..Default::default()
        };
        l.bounds = Bounds2::new(v(0.0, 0.0), v(64.0, 64.0));
        l
    }

    #[test]
    fn wide_lines_keep_their_whole_width_over_sloped_wipeouts() {
        let at = |px: &[u32], x: usize, y: usize| px[y * 64 + x];
        let grey = |v: u32| (200..600).contains(&v);
        let l = wide_line_over_wipeout();
        assert_eq!(wide_width(3.2, true), Some(12.0));
        for format in [wgpu::TextureFormat::Depth24PlusStencil8, wgpu::TextureFormat::Stencil8] {
            let px = render(&l, format, true);
            // Screen rows 26 to 37 (world y 26 to 38): both halves of the line, the nearer one
            // (the top) too, which a depth taken at the line's centre loses to the wipeout.
            for y in 27..=36 {
                for x in [12, 32, 52] {
                    assert!(grey(at(&px, x, y)), "{format:?}: ({x}, {y}) = {}", at(&px, x, y));
                }
            }
            assert!(at(&px, 32, 22) > 700 && at(&px, 32, 41) > 700, "{format:?}: white beside the line");
        }
    }

    #[test]
    fn transparent_objects_get_premultiplied_alpha() {
        let mut l = list();
        for p in &mut l.prims {
            p.transparency = 50;
        }
        // Colour 7 on a black background is white: half of it, premultiplied.
        let m = build_mesh(&l, Vec2::ZERO, Rgb(0, 0, 0), false);
        assert_eq!(&m.lines[8..12], &[128, 128, 128, 128]);
        assert_eq!(&m.tris[8..12], &[128, 128, 128, 128]);
        // A wipeout's mask stays opaque.
        if let Some(p) = l.prims.get_mut(1) {
            p.kind = Kind::Mask;
        }
        let m = build_mesh(&l, Vec2::ZERO, Rgb(10, 20, 30), false);
        assert_eq!(&m.tris[8..12], &[10, 20, 30, 255]);
        assert_eq!(premultiplied(Rgb(255, 0, 0), 255), [255, 0, 0, 255], "opaque colours are unchanged");
    }

    #[test]
    fn lineweights_become_quads() {
        let mut l = list();
        if let Some(p) = l.prims.first_mut() {
            p.lw = 0.7;
        }
        let m = build_mesh(&l, Vec2::ZERO, Rgb(0, 0, 0), true);
        assert_eq!(m.line_vertices(), 0);
        assert_eq!(m.quad_instances(), 3);
        assert_eq!(wide_width(0.25, true), None, "thin lineweights stay hairlines");
        assert_eq!(wide_width(0.7, false), None);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn shader_is_valid_for_webgpu_and_webgl2() {
        let module = naga::front::wgsl::parse_str(SHADER).unwrap();
        let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty()).validate(&module).unwrap();
        for (stage, entry) in
            [(naga::ShaderStage::Vertex, "vs_line"), (naga::ShaderStage::Vertex, "vs_quad"), (naga::ShaderStage::Fragment, "fs_main")]
        {
            let options = naga::back::glsl::Options { version: naga::back::glsl::Version::new_gles(300), ..Default::default() };
            let pipeline = naga::back::glsl::PipelineOptions { shader_stage: stage, entry_point: entry.into(), multiview: None };
            let mut out = String::new();
            let mut w =
                naga::back::glsl::Writer::new(&mut out, &module, &info, &options, &pipeline, naga::proc::BoundsCheckPolicies::default()).unwrap();
            w.write().unwrap();
            assert!(out.contains("#version 300 es"), "{entry}");
        }
    }

    #[test]
    fn origin_follows_deep_zoom() {
        let b = Bounds2::new(Vec2::new(0.0, 0.0), Vec2::new(1e6, 1e6));
        assert_eq!(choose_origin(&b, Vec2::new(10.0, 10.0), 1e5), b.center());
        let o = choose_origin(&b, Vec2::new(123_456.7, 654_321.2), 1.0);
        assert!((o.x - 123_456.7).abs() <= 32.0 && (o.y - 654_321.2).abs() <= 32.0);
        assert_eq!(choose_origin(&Bounds2::EMPTY, Vec2::new(1.0, 1.0), 1.0), Vec2::ZERO);
        assert_eq!(choose_origin(&b, Vec2::new(f64::NAN, 0.0), 1.0), b.center());
    }

    #[test]
    fn wipeout_masks_start_a_draw_order_group() {
        let mut l = list();
        if let Some(p) = l.prims.get_mut(1) {
            p.kind = Kind::Mask;
        }
        let bg = Rgb(10, 20, 30);
        let m = build_mesh(&l, Vec2::ZERO, bg, false);
        // The polyline's two segments come before the mask; the point marker after it.
        assert_eq!(m.breaks, vec![[0, 4, 0]]);
        assert_eq!(m.tri_vertices(), 3);
        assert_eq!(&m.tris[8..12], &[10, 20, 30, 255], "the mask takes the background colour");
        assert_eq!(m.quad_instances(), 1);
    }

    #[test]
    fn batch_ranges_cover_a_range_across_batches() {
        assert_eq!(batch_ranges([4, 4, 4], 2..9), vec![(0, 2..4), (1, 0..4), (2, 0..1)]);
        assert_eq!(batch_ranges([4, 4], 4..8), vec![(1, 0..4)]);
        assert!(batch_ranges([4, 4], 3..3).is_empty());
        assert!(batch_ranges([], 0..5).is_empty());
    }
}
