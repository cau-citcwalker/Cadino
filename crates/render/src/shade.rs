//! RENDER: the current view of the model's solids and meshes as a shaded picture, through the
//! view's camera (perspective or parallel). Not photoreal: each solid takes its part
//! material's colour (else its own), lit by a key light from above and to the left of the
//! viewer, a fill light and ambient light, with a little sheen; curved faces are smooth
//! (their vertex normals are interpolated) and creases stay sharp. There are no shadows,
//! reflections, textures or transparency. Edges can be drawn over the faces.
//!
//! The builder collects the triangles ([`crate::Options::render`], [`collect`]), placed by block
//! references as everything else is; this module depth-buffers them at twice the size and
//! scales the picture down, so its edges are smooth.

use cadcraft_color::Rgb;
use cadcraft_doc::{Drawing, Handle, Space};
use cadcraft_geom::{Mat4, Mesh3, Persp, Vec2, Vec3, ViewDir};
use tiny_skia::Pixmap;

use crate::{Builder, Ctx};

/// A triangle to render: its corners in view coordinates (x right, y up, z toward the viewer;
/// before perspective), their unit normals, its colour and the object it belongs to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadeTri {
    pub p: [Vec3; 3],
    pub n: [Vec3; 3],
    pub color: Rgb,
    pub handle: Handle,
}

/// What to render and how big.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderOptions {
    /// The picture's size in pixels (each 16 to [`MAX_SIDE`]).
    pub width: u32,
    pub height: u32,
    /// The solids' edges drawn dark over their faces.
    pub edges: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { width: 1280, height: 720, edges: false }
    }
}

/// The largest side of a picture.
pub const MAX_SIDE: u32 = 4096;

/// Each side is rendered this many times larger and scaled down, for pictures up to
/// [`SUPER_PIXELS`] (larger ones are fine enough as they are, and would take too much memory).
const SUPER: u32 = 2;
const SUPER_PIXELS: u64 = 4_000_000;

/// Faces meeting at an angle sharper than this keep a crease (cosine of 40°).
const CREASE: f64 = 0.766;

/// A vertex shared by more triangles than this takes its triangles' own normals (hostile meshes).
const MAX_SHARED: usize = 64;

/// The colour a part material is shown in: its name read for a wood, board, metal, glass or
/// colour (English or Korean, any case: "Birch plywood 18", "오크 18T"), else `None`.
pub fn material_rgb(name: &str) -> Option<Rgb> {
    let n = name.trim().to_lowercase();
    if n.is_empty() {
        return None;
    }
    // Most specific first: "white oak" is oak-coloured, "black walnut" walnut.
    const TABLE: &[(&[&str], Rgb)] = &[
        (&["walnut", "월넛", "호두"], Rgb(110, 76, 52)),
        (&["wenge", "웬지"], Rgb(76, 58, 46)),
        (&["mahogany", "마호가니"], Rgb(128, 64, 46)),
        (&["cherry", "체리"], Rgb(160, 92, 62)),
        (&["teak", "티크"], Rgb(165, 120, 72)),
        (&["oak", "오크", "참나무"], Rgb(194, 154, 104)),
        (&["ash", "애쉬", "물푸레"], Rgb(212, 188, 150)),
        (&["beech", "너도밤"], Rgb(214, 172, 128)),
        (&["maple", "메이플", "단풍"], Rgb(226, 200, 160)),
        (&["birch", "자작"], Rgb(228, 206, 170)),
        (&["pine", "소나무", "파인", "spruce", "삼나무"], Rgb(222, 190, 136)),
        (&["bamboo", "대나무"], Rgb(214, 190, 130)),
        (&["plywood", "합판", "ply"], Rgb(220, 190, 146)),
        (&["mdf", "hdf"], Rgb(186, 156, 118)),
        (&["particle", "chipboard", "파티클", "pb"], Rgb(196, 170, 132)),
        (&["glass", "유리"], Rgb(170, 200, 210)),
        (&["mirror", "거울"], Rgb(200, 210, 216)),
        (&["stainless", "스테인리스", "스텐", "aluminium", "aluminum", "알루미늄"], Rgb(186, 190, 196)),
        (&["steel", "스틸", "철", "iron", "metal", "금속"], Rgb(132, 136, 142)),
        (&["brass", "황동", "gold", "골드"], Rgb(196, 160, 82)),
        (&["copper", "구리"], Rgb(184, 112, 72)),
        (&["marble", "대리석", "stone", "석재", "quartz"], Rgb(226, 224, 220)),
        (&["concrete", "콘크리트", "cement"], Rgb(160, 160, 156)),
        (&["leather", "가죽"], Rgb(120, 72, 46)),
        (&["fabric", "패브릭", "천", "linen"], Rgb(170, 160, 146)),
        (&["black", "블랙", "검정"], Rgb(48, 48, 50)),
        (&["white", "화이트", "흰", "melamine", "멜라민", "lpm", "hpl"], Rgb(236, 236, 232)),
        (&["grey", "gray", "그레이", "회색"], Rgb(150, 152, 154)),
        (&["red", "레드", "빨강"], Rgb(186, 52, 46)),
        (&["blue", "블루", "파랑"], Rgb(58, 92, 160)),
        (&["green", "그린", "초록"], Rgb(76, 132, 82)),
        (&["yellow", "옐로", "노랑"], Rgb(222, 190, 70)),
    ];
    let found = TABLE.iter().find(|(names, _)| names.iter().any(|k| n.contains(k))).map(|(_, c)| *c);
    // Another name: a muted colour of its own, the same every time.
    found.or_else(|| {
        let h = n.bytes().fold(2166136261u32, |h, b| (h ^ u32::from(b)).wrapping_mul(16777619));
        let c = |shift: u32| 120 + ((h >> shift) & 0x5f) as u8;
        Some(Rgb(c(0), c(8), c(16)))
    })
}

/// Collect `mesh` placed by `xform` for a render ([`crate::Options::render`]): its triangles with
/// smooth normals within each face (creases kept) and colour `color`, and its edges.
pub(crate) fn collect(b: &mut Builder, ctx: &Ctx, mesh: &Mesh3, xform: &Mat4, color: Rgb) {
    let xf = ctx.xf3.mul(xform);
    let out = if xf.det3() < 0.0 { -1.0 } else { 1.0 };
    let world: Vec<Vec3> = mesh.verts.iter().map(|p| xf.apply(*p)).collect();
    // Each triangle's corners (by index) and unit normal.
    let faces: Vec<([u32; 3], [Vec3; 3], Vec3)> = mesh
        .tris
        .iter()
        .filter_map(|t| {
            let w = |i: u32| world.get(i as usize).copied();
            let (p, q, r) = (w(t[0])?, w(t[1])?, w(t[2])?);
            let n = (q - p).cross(r - p) * out;
            let l = n.len();
            (l > 0.0 && l.is_finite()).then(|| (*t, [p, q, r], n * (1.0 / l)))
        })
        .collect();
    // The triangles at each vertex.
    let mut at: Vec<Vec<u32>> = vec![Vec::new(); world.len()];
    for (k, (t, ..)) in faces.iter().enumerate() {
        for i in t {
            if let (Some(list), Ok(k)) = (at.get_mut(*i as usize), u32::try_from(k)) {
                list.push(k);
            }
        }
    }
    for (t, p, n) in &faces {
        let smooth = |i: u32| {
            let around = at.get(i as usize).filter(|a| a.len() <= MAX_SHARED);
            let sum = around
                .into_iter()
                .flatten()
                .filter_map(|k| faces.get(*k as usize))
                .map(|f| f.2)
                .filter(|m| m.dot(*n) >= CREASE)
                .fold(Vec3::ZERO, |a, m| a + m);
            let s = sum.normalized();
            if s == Vec3::ZERO || !s.is_finite() { *n } else { s }
        };
        b.list.shade.push(ShadeTri { p: *p, n: [smooth(t[0]), smooth(t[1]), smooth(t[2])], color, handle: ctx.top });
    }
    let dark = Rgb((color.0 as f32 * 0.3) as u8, (color.1 as f32 * 0.3) as u8, (color.2 as f32 * 0.3) as u8);
    for e in &mesh.edges {
        b.list.shade_edges.push((e.iter().map(|p| xf.apply(*p)).collect(), dark));
    }
}

/// A vertex on its way to the screen: view coordinates and normal.
#[derive(Clone, Copy)]
struct V {
    p: Vec3,
    n: Vec3,
}

/// Render `space`'s solids and meshes of `d` as projection `dir` shows them in a view centred on
/// `center` (view plane, the target plane in perspective) `view_height` high: a picture
/// `o.width` × `o.height`, as wide as that shape makes the view. `None` for a size out of range.
pub fn render(d: &Drawing, dir: ViewDir, center: Vec2, view_height: f64, o: &RenderOptions) -> Option<Pixmap> {
    let ok = |s: u32| (16..=MAX_SIDE).contains(&s);
    if !(ok(o.width) && ok(o.height) && view_height.is_finite() && view_height > 0.0 && center.is_finite()) {
        return None;
    }
    let opts = crate::Options { view_dir: dir, render: true, tolerance: view_height * 1e-3, ..Default::default() };
    let list = crate::build(d, &Space::Model, &opts);
    let sup = if u64::from(o.width) * u64::from(o.height) <= SUPER_PIXELS { SUPER } else { 1 };
    let (w, h) = (o.width * sup, o.height * sup);
    let mut img = Canvas::new(w, h, sup, dir.persp, center, f64::from(h) / view_height)?;
    // Depths relative to one of the model's own, so f32 keeps them apart far from the origin.
    img.zref = list.shade.first().and_then(|t| img.screen_raw(t.p[0])).map_or(0.0, |s| s.2);
    for t in &list.shade {
        img.tri(t);
    }
    if o.edges {
        for (pts, c) in &list.shade_edges {
            img.polyline(pts, *c);
        }
    }
    img.finish(o.width, o.height)
}

/// The picture being rendered: colours and depths, `sup` times the final size.
struct Canvas {
    w: u32,
    h: u32,
    sup: u32,
    rgb: Vec<[f32; 3]>,
    depth: Vec<f32>,
    persp: Option<Persp>,
    center: Vec2,
    /// Pixels per view unit.
    scale: f64,
    /// Depth of the nearest and farthest of everything, for the edges' depth bias.
    span: f64,
    /// Depths are kept relative to this one.
    zref: f64,
}

/// The key and fill lights (toward the light, in view coordinates: x right, y up, z toward the
/// viewer).
fn lights() -> (Vec3, Vec3) {
    (Vec3::new(-0.45, 0.6, 0.66).normalized(), Vec3::new(0.6, -0.1, 0.8).normalized())
}

impl Canvas {
    fn new(w: u32, h: u32, sup: u32, persp: Option<Persp>, center: Vec2, scale: f64) -> Option<Canvas> {
        let n = usize::try_from(u64::from(w) * u64::from(h)).ok()?;
        let mut depth = Vec::new();
        depth.try_reserve_exact(n).ok()?;
        depth.resize(n, f32::NEG_INFINITY);
        // The background: a soft studio gradient, lighter at the bottom.
        let mut rgb = Vec::new();
        rgb.try_reserve_exact(n).ok()?;
        for y in 0..h {
            let t = f64::from(y) / f64::from(h.max(1));
            let c = [0.80 + 0.15 * t, 0.83 + 0.13 * t, 0.87 + 0.10 * t].map(|v| v as f32);
            rgb.extend(std::iter::repeat_n(c, w as usize));
        }
        Some(Canvas { w, h, sup, rgb, depth, persp, center, scale, span: 0.0, zref: 0.0 })
    }

    /// Screen position (pixels, y down) and depth (larger nearer, linear across the screen over a
    /// plane) of view-coordinates point `p`; `None` behind the near plane.
    fn screen(&self, p: Vec3) -> Option<(f64, f64, f64)> {
        self.screen_raw(p).map(|(x, y, z)| (x, y, z - self.zref))
    }

    /// [`Self::screen`] with the depth as it is.
    fn screen_raw(&self, p: Vec3) -> Option<(f64, f64, f64)> {
        let (q, z) = match &self.persp {
            Some(c) => c.map(p)?,
            None => (p.xy(), p.z),
        };
        let x = (q.x - self.center.x) * self.scale + f64::from(self.w) / 2.0;
        let y = f64::from(self.h) / 2.0 - (q.y - self.center.y) * self.scale;
        (x.is_finite() && y.is_finite() && z.is_finite()).then_some((x, y, z))
    }

    /// The triangle, clipped at the near plane in perspective.
    fn tri(&mut self, t: &ShadeTri) {
        let v = [0, 1, 2].map(|i| V { p: t.p[i], n: t.n[i] });
        let Some(c) = self.persp else { return self.fill(v, t.color) };
        let lim = crate::persp::clip_depth(&c);
        let inside = |a: &V| a.p.z <= lim;
        if v.iter().all(inside) {
            return self.fill(v, t.color);
        }
        let mut poly: Vec<V> = Vec::with_capacity(4);
        for (a, b) in [(v[0], v[1]), (v[1], v[2]), (v[2], v[0])] {
            if inside(&a) {
                poly.push(a);
            }
            if inside(&a) != inside(&b) {
                let k = (lim - a.p.z) / (b.p.z - a.p.z);
                let p = a.p + (b.p - a.p) * k;
                poly.push(V { p: Vec3::new(p.x, p.y, lim), n: (a.n + (b.n - a.n) * k).normalized() });
            }
        }
        if let [first, rest @ ..] = poly.as_slice() {
            for pair in rest.windows(2) {
                if let [b, d] = pair {
                    self.fill([*first, *b, *d], t.color);
                }
            }
        }
    }

    /// Fill a triangle in front of the eye, lighting each pixel.
    fn fill(&mut self, v: [V; 3], color: Rgb) {
        let (Some(a), Some(b), Some(c)) = (self.screen(v[0].p), self.screen(v[1].p), self.screen(v[2].p)) else { return };
        let area = (b.0 - a.0) * (c.1 - a.1) - (c.0 - a.0) * (b.1 - a.1);
        if !(area.abs() > 1e-12 && area.is_finite()) {
            return;
        }
        for z in [a.2, b.2, c.2] {
            self.span = self.span.max(z.abs());
        }
        let (minx, maxx) = (a.0.min(b.0).min(c.0).floor().max(0.0), a.0.max(b.0).max(c.0).ceil().min(f64::from(self.w) - 1.0));
        let (miny, maxy) = (a.1.min(b.1).min(c.1).floor().max(0.0), a.1.max(b.1).max(c.1).ceil().min(f64::from(self.h) - 1.0));
        if minx > maxx || miny > maxy {
            return;
        }
        // Perspective-correct weights: 1 / (distance in front of the eye) at each corner.
        let inv_w = |p: Vec3| self.persp.map_or(1.0, |c| 1.0 / (c.eye.z - p.z).max(1e-300));
        let iw = [inv_w(v[0].p), inv_w(v[1].p), inv_w(v[2].p)];
        let base = [color.0, color.1, color.2].map(|c| (f64::from(c) / 255.0).powf(2.2));
        let (key, fill) = lights();
        let (x0, x1, y0, y1) = (minx as u32, maxx as u32, miny as u32, maxy as u32);
        for y in y0..=y1 {
            let py = f64::from(y) + 0.5;
            for x in x0..=x1 {
                let px = f64::from(x) + 0.5;
                let w0 = ((b.0 - px) * (c.1 - py) - (c.0 - px) * (b.1 - py)) / area;
                let w1 = ((c.0 - px) * (a.1 - py) - (a.0 - px) * (c.1 - py)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let z = w0 * a.2 + w1 * b.2 + w2 * c.2;
                let i = (y * self.w + x) as usize;
                let Some(slot) = self.depth.get_mut(i) else { continue };
                if (z as f32) <= *slot {
                    continue;
                }
                *slot = z as f32;
                let (k0, k1, k2) = (w0 * iw[0], w1 * iw[1], w2 * iw[2]);
                let s = k0 + k1 + k2;
                let mut n = ((v[0].n * k0 + v[1].n * k1 + v[2].n * k2) * (1.0 / s)).normalized();
                let at = (v[0].p * k0 + v[1].p * k1 + v[2].p * k2) * (1.0 / s);
                // Toward the viewer from this point; a face seen from behind is lit as its front.
                let eye = match &self.persp {
                    Some(c) => (c.eye - at).normalized(),
                    None => Vec3::Z,
                };
                if n.dot(eye) < 0.0 {
                    n = -n;
                }
                let diffuse = 0.62 * n.dot(key).max(0.0) + 0.18 * n.dot(fill).max(0.0) + 0.28;
                let half = (key + eye).normalized();
                let spec = 0.12 * n.dot(half).max(0.0).powi(24);
                if let Some(px) = self.rgb.get_mut(i) {
                    *px = base.map(|c| (c * diffuse + spec) as f32);
                }
            }
        }
    }

    /// An edge drawn over the faces where they don't hide it.
    fn polyline(&mut self, pts: &[Vec3], color: Rgb) {
        let lim = self.persp.map(|c| crate::persp::clip_depth(&c));
        let ink = [color.0, color.1, color.2].map(|c| (f64::from(c) / 255.0).powf(2.2) as f32);
        // Lines lie on their faces: let them through a little in front of the depths there.
        let bias = (self.span * 1e-4).max(1e-9);
        for s in pts.windows(2) {
            let &[mut p, mut q] = s else { continue };
            if let Some(lim) = lim {
                if p.z > lim && q.z > lim {
                    continue;
                }
                let cut = |a: Vec3, b: Vec3| {
                    let p = a + (b - a) * ((lim - a.z) / (b.z - a.z));
                    Vec3::new(p.x, p.y, lim)
                };
                if p.z > lim {
                    p = cut(p, q);
                } else if q.z > lim {
                    q = cut(q, p);
                }
            }
            let (Some(a), Some(b)) = (self.screen(p), self.screen(q)) else { continue };
            let n = ((b.0 - a.0).abs().max((b.1 - a.1).abs()).ceil() as usize).clamp(1, 1 << 16);
            for k in 0..=n {
                let t = k as f64 / n as f64;
                let (x, y, z) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t);
                // Lines are `sup` pixels wide here: one pixel wide once scaled down.
                let wide = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)];
                for &(dx, dy) in wide.iter().take(if self.sup > 1 { 4 } else { 1 }) {
                    let (px, py) = ((x + dx - 0.5).floor(), (y + dy - 0.5).floor());
                    if px < 0.0 || py < 0.0 || px >= f64::from(self.w) || py >= f64::from(self.h) {
                        continue;
                    }
                    let i = (py as u32 * self.w + px as u32) as usize;
                    if self.depth.get(i).is_some_and(|d| (z + bias) as f32 >= *d)
                        && let Some(c) = self.rgb.get_mut(i)
                    {
                        *c = ink;
                    }
                }
            }
        }
    }

    /// Scale down to `w` × `h` (each pixel the mean of its `sup` × `sup`), back to sRGB.
    fn finish(self, w: u32, h: u32) -> Option<Pixmap> {
        let mut pm = Pixmap::new(w, h)?;
        let sup = self.sup;
        let k = (sup * sup) as f32;
        let back = |v: f32| ((v / k).clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8;
        let data = pm.data_mut();
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0.0f32; 3];
                for sy in 0..sup {
                    for sx in 0..sup {
                        let i = ((y * sup + sy) * self.w + x * sup + sx) as usize;
                        if let Some(c) = self.rgb.get(i) {
                            for (s, v) in sum.iter_mut().zip(c) {
                                *s += v;
                            }
                        }
                    }
                }
                let o = ((y * w + x) * 4) as usize;
                if let Some(px) = data.get_mut(o..o + 4) {
                    px.copy_from_slice(&[back(sum[0]), back(sum[1]), back(sum[2]), 255]);
                }
            }
        }
        Some(pm)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cadcraft_doc::{Common, Drawing, EntityKind, Part, Solid3d, Space};
    use cadcraft_geom::{Camera, Mesh3};

    use super::*;

    /// A box `w` × `d` × `h` at `o` with its faces' own corners, as the kernel tessellates it.
    fn block(o: Vec3, s: Vec3, material: &str) -> Solid3d {
        let v = |i: u32| o + Vec3::new(s.x * f64::from(i & 1), s.y * f64::from((i >> 1) & 1), s.z * f64::from((i >> 2) & 1));
        let corners =
            [[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
        let verts = corners.iter().flatten().map(|&i| v(i)).collect();
        let tris = (0..12).map(|t| [3 * t, 3 * t + 1, 3 * t + 2]).collect();
        let e = |a: u32, b: u32| vec![v(a), v(b)];
        let edges = vec![e(0, 1), e(1, 3), e(3, 2), e(2, 0), e(4, 5), e(5, 7), e(7, 6), e(6, 4), e(0, 4), e(1, 5), e(2, 6), e(3, 7)];
        Solid3d {
            brep: Arc::from(""),
            mesh: Arc::new(Mesh3 { verts, tris, edges, open: false }),
            part: Part { name: "Side".into(), material: material.into() },
            ..Default::default()
        }
    }

    fn cabinet() -> Drawing {
        let mut d = Drawing::default();
        for (o, s, m) in [
            (Vec3::new(0.0, 0.0, 0.0), Vec3::new(18.0, 400.0, 700.0), "Walnut 18"),
            (Vec3::new(582.0, 0.0, 0.0), Vec3::new(18.0, 400.0, 700.0), "Walnut 18"),
            (Vec3::new(18.0, 0.0, 682.0), Vec3::new(564.0, 400.0, 18.0), "White melamine"),
        ] {
            d.add(&Space::Model, Common::default(), EntityKind::Solid3d(block(o, s, m))).unwrap();
        }
        d
    }

    fn pixel(pm: &Pixmap, x: u32, y: u32) -> [u8; 3] {
        let c = pm.pixel(x, y).unwrap();
        [c.red(), c.green(), c.blue()]
    }

    #[test]
    fn materials_have_their_colours() {
        assert_eq!(material_rgb("Birch plywood 18"), material_rgb("birch"));
        assert_eq!(material_rgb("월넛 18T"), material_rgb("Walnut"));
        assert_ne!(material_rgb("Oak"), material_rgb("White"));
        assert!(material_rgb("").is_none());
        // Any other name has a colour of its own, the same every time.
        assert_eq!(material_rgb("Zebrano"), material_rgb("ZEBRANO"));
    }

    #[test]
    fn a_perspective_render_shows_the_parts_in_their_materials() {
        let d = cabinet();
        let dir = ViewDir::named("iso").unwrap();
        let cam = Camera::new(50.0, dir.depth(Vec3::new(300.0, 200.0, 350.0)));
        let center = dir.project(Vec3::new(300.0, 200.0, 350.0));
        let height = cam.height_at(3000.0);
        let persp = dir.with_camera(Some(cam), center, height);
        let o = RenderOptions { width: 320, height: 240, edges: true };
        let pm = render(&d, persp, center, height, &o).unwrap();
        assert_eq!((pm.width(), pm.height()), (320, 240));
        // The corner is background, the middle of the top is white, the sides walnut brown.
        let corner = pixel(&pm, 2, 2);
        assert!(corner.iter().all(|c| *c > 180), "{corner:?}");
        let (mut white, mut brown) = (0, 0);
        for y in 0..240 {
            for x in 0..320 {
                let [r, g, b] = pixel(&pm, x, y);
                if r > 200 && g > 200 && b > 190 && (i32::from(r) - i32::from(b)).abs() < 20 && (x, y) != (2, 2) {
                    white += 1;
                }
                if r > g && g > b && i32::from(r) - i32::from(b) > 30 && r < 170 {
                    brown += 1;
                }
            }
        }
        assert!(brown > 200, "walnut sides: {brown}");
        assert!(white > 0, "the white top");
        // PNG out.
        assert!(pm.encode_png().unwrap().len() > 1000);
    }

    /// Block "B" holding `e`, referenced once at the origin with XCLIP boundary (2,2)-(8,8)
    /// shown (XCLIPFRAME 1).
    fn clipped_reference(e: EntityKind) -> Drawing {
        use cadcraft_doc::{Block, Insert, xclip::XClip};
        let mut d = Drawing::default();
        let mut blk = Block::new("B");
        blk.entities.push(cadcraft_doc::Entity { handle: d.new_handle(), common: Common::default(), kind: e });
        d.blocks.insert("B".into(), Arc::new(blk));
        let mut common = Common::default();
        let clip = XClip { boundary: vec![Vec2::new(2.0, 2.0), Vec2::new(8.0, 8.0)], on: true, front: None, back: None };
        XClip::put(&mut common, Some(&clip));
        let ins = Insert {
            block: "B".into(),
            insert: Vec3::ZERO,
            scale: Vec3::new(1.0, 1.0, 1.0),
            rotation: 0.0,
            attribs: vec![],
            cols: 1,
            rows: 1,
            col_spacing: 0.0,
            row_spacing: 0.0,
        };
        d.add(&Space::Model, common, EntityKind::Insert(ins)).unwrap();
        d.header.set("XCLIPFRAME", cadcraft_doc::HVal::Int(1));
        d
    }

    #[test]
    fn an_xclip_frame_in_perspective_is_drawn_where_its_boundary_is() {
        let d = clipped_reference(EntityKind::Line(cadcraft_doc::Line { a: Vec3::new(3.0, 3.0, 0.0), b: Vec3::new(7.0, 7.0, 0.0) }));
        // The target plane 30 above the boundary's.
        let dir = ViewDir::named("iso").unwrap();
        let target = Vec3::new(5.0, 5.0, 30.0);
        let cam = Camera::new(50.0, dir.depth(target));
        let persp = dir.with_camera(Some(cam), dir.project(target), cam.height_at(60.0));
        let list = crate::build(&d, &Space::Model, &crate::Options { view_dir: persp, ..Default::default() });
        let frame = list.prims.iter().rev().find(|p| list.points(p).len() >= 5).map(|p| list.points(p).to_vec()).unwrap();
        for c in [(2.0, 2.0), (8.0, 2.0), (8.0, 8.0), (2.0, 8.0)] {
            let want = persp.project(Vec3::new(c.0, c.1, 0.0));
            assert!(frame.iter().any(|q| q.dist(want) < 1e-6), "{want:?} not on the frame {frame:?}");
        }
    }

    #[test]
    fn render_shows_the_solids_of_an_xclipped_reference() {
        let d = clipped_reference(EntityKind::Solid3d(block(Vec3::new(3.0, 3.0, 0.0), Vec3::new(4.0, 4.0, 4.0), "Walnut")));
        let dir = ViewDir::named("iso").unwrap();
        let center = dir.project(Vec3::new(5.0, 5.0, 2.0));
        let pm = render(&d, dir, center, 20.0, &RenderOptions { width: 64, height: 48, edges: false }).unwrap();
        let background = pixel(&pm, 1, 1);
        assert_ne!(pixel(&pm, 32, 24), background, "the solid is rendered");
    }

    #[test]
    fn a_camera_inside_the_model_clips_and_unusable_sizes_fail() {
        let d = cabinet();
        let dir = ViewDir::named("front").unwrap();
        // The eye between the sides, looking at the back: the near plane cuts the sides.
        let cam = Camera::new(24.0, dir.depth(Vec3::new(300.0, 380.0, 350.0)));
        let center = dir.project(Vec3::new(300.0, 380.0, 350.0));
        let persp = dir.with_camera(Some(cam), center, cam.height_at(350.0));
        assert!(render(&d, persp, center, cam.height_at(350.0), &RenderOptions { width: 64, height: 48, edges: true }).is_some());
        assert!(render(&d, dir, center, 1000.0, &RenderOptions { width: 0, height: 48, edges: false }).is_none());
        assert!(render(&d, dir, center, f64::NAN, &RenderOptions::default()).is_none());
        assert!(render(&d, dir, center, 1000.0, &RenderOptions { width: MAX_SIDE + 1, height: 48, edges: false }).is_none());
    }
}
