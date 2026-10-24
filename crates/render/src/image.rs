//! Raster images: an IMAGE entity's picture goes into the display list as an [`ImagePrim`]
//! (drawn here by the CPU raster, by the canvas as a texture, and embedded by PDF and SVG
//! output), with its frame per IMAGEFRAME (0 none, 1 shown and plotted, 2 shown but not
//! plotted). An image whose file isn't loaded shows its frame and file name instead. In a
//! perspective view, or one with clipping planes, the picture is a grid of triangles seen
//! through the eye and cut by the planes ([`ImagePrim::mesh`]), each drawn with its own affine
//! piece of the picture.

use std::sync::Arc;

use cadcraft_doc::{Handle, Image, Raster, Text};
use cadcraft_geom::{Bounds2, Mat3, Vec2, Vec3};
use tiny_skia::{FillRule, FilterQuality, Mask, Paint, PathBuilder, Pattern, Pixmap, SpreadMode, Transform};

use crate::clip::PolyClip;
use crate::persp::Lens;
use crate::raster::View;
use crate::{Builder, Ctx, Ink};

/// How an image's pixels look: IMAGEADJUST, TRANSPARENCY, IMAGEQUALITY and the object's own
/// transparency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Look {
    /// 0..=100, 50 unchanged.
    pub brightness: u8,
    pub contrast: u8,
    /// 0..=100: how far the picture fades into what is behind it.
    pub fade: u8,
    /// Transparent pixels (a PNG's alpha) show what is under them; off, they are white, as the
    /// picture would print on paper.
    pub transparent: bool,
    /// Opacity of the whole picture (the object's transparency): 255 opaque.
    pub alpha: u8,
    /// IMAGEQUALITY Draft: the nearest pixel instead of a bilinear blend.
    pub draft: bool,
}

/// An image's picture in a display list.
#[derive(Clone, Debug)]
pub struct ImagePrim {
    pub handle: Handle,
    /// Drawn just before primitive `at` of the list (its place in the draw order).
    pub at: u32,
    /// A parallel view: the picture's unit square in list coordinates, (0, 0) its lower-left
    /// corner, (1, 1) the upper-right one.
    pub placement: Mat3,
    /// A perspective view or one with clipping planes: the picture as triangles (three corners
    /// each) in list coordinates, each corner with its texture coordinates (0..1 across, 0 at the
    /// top row to 1 at the bottom), cut at the near and clipping planes and by the picture's own
    /// clip boundary; `placement` is then unused. Empty in a plain parallel view.
    pub mesh: Vec<(Vec2, [f32; 2])>,
    /// Polygons in list coordinates the picture is clipped to, every one of them: its clip
    /// boundary, the outline of the viewport it is seen through.
    pub clips: Vec<Vec<Vec2>>,
    pub pixels: Arc<Raster>,
    pub look: Look,
}

/// Texture coordinates (v down) → the unit square (y up), and back.
const FLIP: Mat3 = Mat3 { a: 1.0, b: 0.0, c: 0.0, d: -1.0, e: 0.0, f: 1.0 };

impl ImagePrim {
    /// The corners of what the picture covers in list coordinates: its placed unit square, or
    /// the box around its perspective mesh.
    pub fn corners(&self) -> [Vec2; 4] {
        if self.mesh.is_empty() {
            return [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|(x, y)| self.placement.apply(Vec2::new(x, y)));
        }
        Bounds2::from_points(self.mesh.iter().map(|v| v.0)).corners()
    }

    /// A parallel view's picture: texture coordinates → list coordinates.
    pub fn texture_xf(&self) -> Mat3 {
        self.placement.then_before(FLIP)
    }

    /// What shows of the picture, as triangles (three corners each) in list coordinates with
    /// their texture coordinates.
    pub fn textured(&self) -> Vec<(Vec2, [f32; 2])> {
        let mut tris = if self.mesh.is_empty() {
            let [a, b, c, d] = self.corners();
            vec![(a, [0.0, 1.0]), (b, [1.0, 1.0]), (c, [1.0, 0.0]), (a, [0.0, 1.0]), (c, [1.0, 0.0]), (d, [0.0, 0.0])]
        } else {
            self.mesh.clone()
        };
        for clip in &self.clips {
            let Some(pc) = PolyClip::new(clip) else { return Vec::new() };
            tris = clip_textured(&pc, &tris);
        }
        tris
    }

    /// [`ImagePrim::textured`]'s triangles, each with the map from texture coordinates onto it
    /// (list coordinates): the pieces a perspective picture is drawn in.
    pub fn pieces(&self) -> Vec<([Vec2; 3], Mat3)> {
        let uv = |w: [f32; 2]| Vec2::new(f64::from(w[0]), f64::from(w[1]));
        self.textured().as_chunks::<3>().0.iter().filter_map(|t| Some((t.map(|v| v.0), affine(t.map(|v| uv(v.1)), t.map(|v| v.0))?))).collect()
    }
}

/// The affine map taking triangle `from` onto triangle `to`; `None` for a flat one.
pub fn affine(from: [Vec2; 3], to: [Vec2; 3]) -> Option<Mat3> {
    let frame = |t: [Vec2; 3]| Mat3 { a: t[1].x - t[0].x, b: t[1].y - t[0].y, c: t[2].x - t[0].x, d: t[2].y - t[0].y, e: t[0].x, f: t[0].y };
    let m = frame(to).then_before(frame(from).inverse()?);
    [m.a, m.b, m.c, m.d, m.e, m.f].iter().all(|v| v.is_finite()).then_some(m)
}

/// Triangle `t` grown about its centroid so each edge moves out by at least `by` (at most
/// doubled): a piece of a picture clipped to it overlaps its neighbours, so anti-aliased clip
/// edges leave no seam between them.
pub fn grown(t: [Vec2; 3], by: f64) -> [Vec2; 3] {
    let g = (t[0] + t[1] + t[2]) * (1.0 / 3.0);
    let area = (t[1] - t[0]).cross(t[2] - t[0]).abs() / 2.0;
    let longest = (t[1] - t[0]).len().max((t[2] - t[1]).len()).max((t[0] - t[2]).len());
    // The centroid is a third of the lowest height from the longest edge.
    let k = 3.0 * by * longest / (2.0 * area);
    let k = if k.is_finite() { k.clamp(0.0, 1.0) } else { 0.0 };
    t.map(|q| g + (q - g) * (1.0 + k))
}

/// Where `q` is in triangle `t`: its three barycentric weights; `None` for a flat triangle.
fn barycentric(t: [Vec2; 3], q: Vec2) -> Option<[f64; 3]> {
    let (e1, e2, d) = (t[1] - t[0], t[2] - t[0], q - t[0]);
    let det = e1.cross(e2);
    if !(det.is_finite() && det.abs() > 1e-300) {
        return None;
    }
    let (l1, l2) = (d.cross(e2) / det, e1.cross(d) / det);
    Some([1.0 - l1 - l2, l1, l2])
}

/// Textured triangles `tris` clipped to `pc`, the cut corners' texture coordinates blended from
/// their triangle's.
fn clip_textured(pc: &PolyClip, tris: &[(Vec2, [f32; 2])]) -> Vec<(Vec2, [f32; 2])> {
    let mut out = Vec::with_capacity(tris.len());
    for t in tris.as_chunks::<3>().0 {
        let pos = t.map(|v| v.0);
        for q in pc.clip_triangles(&pos) {
            let Some(w) = barycentric(pos, q) else { break };
            let blend = |k: usize| (w[0] * f64::from(t[0].1[k]) + w[1] * f64::from(t[1].1[k]) + w[2] * f64::from(t[2].1[k])) as f32;
            out.push((q, [blend(0), blend(1)]));
        }
    }
    out.truncate(out.len() / 3 * 3);
    out
}

/// The picture's pixels as `look` shows them: RGBA, straight alpha, rows from the top.
pub fn adjusted(px: &Raster, look: &Look) -> Vec<u8> {
    let plain = look.brightness == 50 && look.contrast == 50 && look.fade == 0 && look.alpha == u8::MAX;
    if plain && (look.transparent || !px.alpha) {
        return px.rgba.clone();
    }
    // ponytail: our own curves (contrast about mid-grey, brightness as an offset, fade as
    // opacity), not the reference app's; fade shows what is under the picture, not only the
    // background colour.
    let k = f32::from(look.contrast.min(100)) / 50.0;
    let off = (f32::from(look.brightness.min(100)) - 50.0) * 5.1;
    let lut: [u8; 256] = std::array::from_fn(|c| ((c as f32 - 128.0) * k + 128.0 + off).round().clamp(0.0, 255.0) as u8);
    let keep = f32::from(100 - look.fade.min(100)) / 100.0 * f32::from(look.alpha) / 255.0;
    let mut out = px.rgba.clone();
    for p in out.as_chunks_mut::<4>().0 {
        let [r, g, b, a] = *p;
        let (r, g, b, a) = if look.transparent || a == u8::MAX {
            (r, g, b, a)
        } else {
            // Over white.
            let w = |c: u8| ((u32::from(c) * u32::from(a) + 255 * (255 - u32::from(a)) + 127) / 255).min(255) as u8;
            (w(r), w(g), w(b), u8::MAX)
        };
        let l = |c: u8| lut.get(usize::from(c)).copied().unwrap_or(c);
        *p = [l(r), l(g), l(b), (f32::from(a) * keep).round().clamp(0.0, 255.0) as u8];
    }
    out
}

/// The picture as `look` shows it, premultiplied for tiny-skia.
fn pixmap(px: &Raster, look: &Look) -> Option<Pixmap> {
    let mut rgba = adjusted(px, look);
    for p in rgba.as_chunks_mut::<4>().0 {
        let a = u32::from(p[3]);
        for c in p.iter_mut().take(3) {
            *c = ((u32::from(*c) * a + 127) / 255) as u8;
        }
    }
    Pixmap::from_vec(rgba, tiny_skia::IntSize::from_wh(px.width, px.height)?)
}

/// The picture of `im` as it looks, as a PNG file (SVG output embeds it).
pub fn png(im: &ImagePrim) -> Option<Vec<u8>> {
    pixmap(&im.pixels, &im.look)?.encode_png().ok()
}

/// Put entity `e`'s image into the list: its picture, its frame per IMAGEFRAME, or its frame and
/// file name while the picture isn't there.
pub(crate) fn build(b: &mut Builder, ctx: &Ctx, i: &Image, rgb: Ink, lw: f32) {
    let pic = i.pixels.as_ref().filter(|_| i.shown());
    let placement = ctx.xf.then_before(i.placement());
    let ok = [placement.a, placement.b, placement.c, placement.d, placement.e, placement.f].iter().all(|v| v.is_finite());
    let lens = b.lens();
    // Seen edge-on in a parallel view only its frame shows; through the eye it may not be.
    if let Some(px) = pic
        && ok
        && i.placement().det().abs() > 1e-300
        && (b.opts.view_dir.persp.is_some() || placement.det().abs() > 1e-300)
    {
        let (clips, mesh) = match &lens {
            Some(l) => (Vec::new(), lens_mesh(ctx, i, l)),
            None => (i.clip_outline().map(|o| o.iter().map(|p| ctx.xf.apply(*p)).collect()).into_iter().collect(), Vec::new()),
        };
        let look = Look {
            brightness: i.brightness,
            contrast: i.contrast,
            fade: i.fade,
            transparent: i.transparent(),
            alpha: crate::alpha(b.transparency),
            draft: ctx.d.header.i64("IMAGEQUALITY", 1) == 0,
        };
        let im = ImagePrim { handle: ctx.top, at: b.list.prims.len() as u32, placement, mesh, clips, pixels: px.clone(), look };
        if lens.is_none() || !im.mesh.is_empty() {
            for c in im.corners() {
                b.list.bounds.add(c);
            }
            b.list.images.push(im);
        }
    }
    let frame = match ctx.d.header.i64("IMAGEFRAME", 1) {
        0 => false,
        2 => !b.plotting,
        _ => true,
    };
    let mut o = i.outline();
    if let Some(f) = o.first().copied()
        && (frame || pic.is_none())
    {
        o.push(f);
        b.polyline(ctx, rgb, lw, &o);
    }
    if pic.is_none() && b.opts.text {
        placeholder(b, ctx, i, rgb, lw);
    }
}

/// Cells along each side of the grid a picture is seen through the eye with.
// ponytail: a fixed grid, each cell an affine piece of the picture, so a picture filling a
// wide-angle view bends a little at the cell edges; subdivide by on-screen size if that shows.
const GRID: u32 = 16;

/// Image `i` seen through lens `l` (the eye, the clipping planes): [`ImagePrim::mesh`].
fn lens_mesh(ctx: &Ctx, i: &Image, l: &Lens) -> Vec<(Vec2, [f32; 2])> {
    let unit = i.placement();
    let Some(inv) = unit.inverse() else { return Vec::new() };
    // The grid's triangles in the unit square, cut by the picture's own clip boundary there.
    let n = f64::from(GRID);
    let mut tris = Vec::with_capacity((GRID * GRID * 6) as usize);
    for gy in 0..GRID {
        for gx in 0..GRID {
            let (x0, y0, x1, y1) = (f64::from(gx) / n, f64::from(gy) / n, f64::from(gx + 1) / n, f64::from(gy + 1) / n);
            let (p, q, r, s) = (Vec2::new(x0, y0), Vec2::new(x1, y0), Vec2::new(x1, y1), Vec2::new(x0, y1));
            tris.extend([p, q, r, p, r, s]);
        }
    }
    if let Some(o) = i.clip_outline() {
        let local: Vec<Vec2> = o.iter().map(|p| inv.apply(*p)).collect();
        tris = match PolyClip::new(&local) {
            Some(pc) => pc.clip_triangles(&tris),
            None => return Vec::new(),
        };
    }
    let mut out = Vec::with_capacity(tris.len());
    for t in tris.as_chunks::<3>().0 {
        // Corners in view coordinates with their unit-square points, cut to the depths seen.
        let v = t.map(|q| (ctx.xf3.apply(unit.apply(q).to3(0.0)), q));
        let poly = cut(cut(v.to_vec(), l.hi, true), l.lo, false);
        let seen: Option<Vec<(Vec2, [f32; 2])>> = poly.iter().map(|(p, q)| Some((l.map(*p)?.0, [q.x as f32, (1.0 - q.y) as f32]))).collect();
        if let Some([first, rest @ ..]) = seen.as_deref() {
            for w in rest.windows(2) {
                if let [p1, p2] = w
                    && first.0.is_finite()
                    && p1.0.is_finite()
                    && p2.0.is_finite()
                {
                    out.extend([*first, *p1, *p2]);
                }
            }
        }
    }
    out
}

/// Polygon `poly` (view coordinates, each corner with its unit-square point) cut to the depths
/// on one side of `bound`: at most `bound` (`below`), else at least.
fn cut(poly: Vec<(Vec3, Vec2)>, bound: f64, below: bool) -> Vec<(Vec3, Vec2)> {
    if bound.is_infinite() {
        return poly;
    }
    let inside = |p: &(Vec3, Vec2)| if below { p.0.z <= bound } else { p.0.z >= bound };
    let mut out = Vec::with_capacity(poly.len() + 1);
    for (k, a) in poly.iter().enumerate() {
        let Some(e) = poly.get((k + 1) % poly.len()) else { continue };
        if inside(a) {
            out.push(*a);
        }
        if inside(a) != inside(e) {
            let s = (bound - a.0.z) / (e.0.z - a.0.z);
            let p = a.0 + (e.0 - a.0) * s;
            out.push((Vec3::new(p.x, p.y, bound), a.1.lerp(e.1, s)));
        }
    }
    out
}

/// The file name written inside the frame of an image whose picture isn't shown.
fn placeholder(b: &mut Builder, ctx: &Ctx, i: &Image, rgb: Ink, lw: f32) {
    let name = i.file_name();
    let name = if name.trim().is_empty() { i.name.as_str() } else { name };
    let (u, v) = (i.u.xy() * i.size.x, i.v.xy() * i.size.y);
    let (w, h) = (u.len(), v.len());
    if name.trim().is_empty() || !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return;
    }
    // Fits half the width (about 0.9 em per character) and a tenth of the height.
    let chars = name.chars().count().clamp(1, 200) as f64;
    let size = (h / 10.0).min(w * 0.5 / (chars * 0.9 + 1.0));
    let at = i.insert.xy() + u.normalized() * size * 0.5 + v.normalized() * (h - size) * 0.5;
    let t = Text {
        insert: at.to3(i.insert.z),
        align_pt: None,
        height: size,
        value: name.to_string(),
        rotation: u.angle(),
        width_factor: 1.0,
        oblique: 0.0,
        style: "Standard".into(),
        halign: Default::default(),
        valign: Default::default(),
    };
    let (sh, _) = crate::place_text_entity(ctx.d, &t, &t.value);
    b.shaped(ctx, rgb, lw, &sh);
}

/// Draw `im` on `pm` (the CPU raster), seen through `view`.
pub(crate) fn raster(pm: &mut Pixmap, im: &ImagePrim, view: &View) {
    let px = &im.pixels;
    let screen = |p: Vec2| {
        let (x, y) = view.to_screen(p);
        Vec2::new(f64::from(x), f64::from(y))
    };
    let corners = im.corners().map(screen);
    let vis = Bounds2::new(Vec2::ZERO, Vec2::new(f64::from(view.width), f64::from(view.height)));
    if !Bounds2::from_points(corners).intersects(&vis) {
        return;
    }
    let Some(src) = pixmap(px, &im.look) else { return };
    // Pixel (column, row from the top) → texture coordinates → list → screen.
    let unit = Mat3::scale(1.0 / f64::from(px.width), 1.0 / f64::from(px.height));
    let (cx, cy) = (f64::from(view.width) / 2.0 - view.center.x * view.scale, f64::from(view.height) / 2.0 + view.center.y * view.scale);
    let to_screen = Mat3 { a: view.scale, b: 0.0, c: 0.0, d: -view.scale, e: cx, f: cy };
    let quality = if im.look.draft { FilterQuality::Nearest } else { FilterQuality::Bilinear };
    let shader = |texture: Mat3| {
        let m = to_screen.then_before(texture).then_before(unit);
        Pattern::new(
            src.as_ref(),
            SpreadMode::Pad,
            quality,
            1.0,
            Transform::from_row(m.a as f32, m.b as f32, m.c as f32, m.d as f32, m.e as f32, m.f as f32),
        )
    };
    let path = |pts: &[Vec2]| {
        let mut pb = PathBuilder::new();
        for (k, q) in pts.iter().enumerate() {
            let s = screen(*q);
            if k == 0 {
                pb.move_to(s.x as f32, s.y as f32);
            } else {
                pb.line_to(s.x as f32, s.y as f32);
            }
        }
        pb.close();
        pb.finish()
    };
    if !im.mesh.is_empty() {
        // Each triangle with its own piece of the picture, aliased: anti-aliased neighbours
        // would leave seams between them.
        for (t, texture) in im.pieces() {
            if let Some(p) = path(&t) {
                let paint = Paint { shader: shader(texture), anti_alias: false, ..Default::default() };
                pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
            }
        }
        return;
    }
    let paint = Paint { shader: shader(im.texture_xf()), anti_alias: true, ..Default::default() };
    let Some(quad) = path(&im.corners()) else { return };
    let mut mask: Option<Mask> = None;
    for c in &im.clips {
        let Some(p) = path(c) else { return };
        let m = match mask.take() {
            None => {
                let Some(mut m) = Mask::new(view.width, view.height) else { return };
                m.fill_path(&p, FillRule::EvenOdd, true, Transform::identity());
                m
            }
            Some(mut m) => {
                m.intersect_path(&p, FillRule::EvenOdd, true, Transform::identity());
                m
            }
        };
        mask = Some(m);
    }
    pm.fill_path(&quad, &paint, FillRule::Winding, Transform::identity(), mask.as_ref());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(rgba: Vec<u8>, w: u32, h: u32, alpha: bool) -> Arc<Raster> {
        Raster::new(w, h, rgba, alpha).unwrap()
    }

    const PLAIN: Look = Look { brightness: 50, contrast: 50, fade: 0, transparent: false, alpha: 255, draft: false };

    fn area(tris: &[(Vec2, [f32; 2])]) -> f64 {
        tris.as_chunks::<3>().0.iter().map(|t| ((t[1].0 - t[0].0).cross(t[2].0 - t[0].0) / 2.0).abs()).sum()
    }

    #[test]
    fn adjustments_change_the_pixels() {
        let px = raster(vec![100, 150, 200, 255, 0, 0, 0, 0], 2, 1, true);
        // Transparency off: the see-through pixel is white.
        assert_eq!(adjusted(&px, &PLAIN), vec![100, 150, 200, 255, 255, 255, 255, 255]);
        assert_eq!(adjusted(&px, &Look { transparent: true, ..PLAIN }), px.rgba);
        let bright = adjusted(&px, &Look { brightness: 100, transparent: true, ..PLAIN });
        assert_eq!(&bright[..4], &[255, 255, 255, 255]);
        let flat = adjusted(&px, &Look { contrast: 0, transparent: true, ..PLAIN });
        assert_eq!(&flat[..3], &[128, 128, 128]);
        let faded = adjusted(&px, &Look { fade: 100, transparent: true, ..PLAIN });
        assert_eq!(faded[3], 0);
        let half = adjusted(&px, &Look { alpha: 128, transparent: true, ..PLAIN });
        assert_eq!(half[3], 128);
    }

    #[test]
    fn clip_cuts_the_triangles_and_texture_follows() {
        let im = ImagePrim {
            handle: Handle(1),
            at: 0,
            placement: Mat3 { a: 4.0, b: 0.0, c: 0.0, d: 2.0, e: 10.0, f: 0.0 },
            mesh: Vec::new(),
            clips: Vec::new(),
            pixels: raster(vec![0; 16], 2, 2, false),
            look: PLAIN,
        };
        let whole = im.textured();
        assert_eq!(whole.len(), 6);
        // The upper-left corner is the top row's first texel, the lower-right the last row's last.
        assert!(whole.iter().any(|(p, uv)| p.near(Vec2::new(10.0, 2.0), 1e-12) && *uv == [0.0, 0.0]));
        assert!(whole.iter().any(|(p, uv)| p.near(Vec2::new(14.0, 0.0), 1e-12) && *uv == [1.0, 1.0]));
        let xf = im.texture_xf();
        assert!(xf.apply(Vec2::new(1.0, 1.0)).near(Vec2::new(14.0, 0.0), 1e-12));
        let left =
            ImagePrim { clips: vec![vec![Vec2::new(9.0, -1.0), Vec2::new(12.0, -1.0), Vec2::new(12.0, 3.0), Vec2::new(9.0, 3.0)]], ..im.clone() };
        let cut = left.textured();
        assert!((area(&cut) - 4.0).abs() < 1e-9, "{}", area(&cut));
        // A cut corner takes the texture coordinates of where it lies on the picture.
        for (p, uv) in &cut {
            assert!((f64::from(uv[0]) - (p.x - 10.0) / 4.0).abs() < 1e-6 && (f64::from(uv[1]) - (1.0 - p.y / 2.0)).abs() < 1e-6, "{p:?} {uv:?}");
        }
        // Each piece maps texture coordinates onto its triangle.
        for (t, m) in left.pieces() {
            for q in t {
                let back = xf.inverse().unwrap().apply(q);
                assert!(m.apply(back).near(q, 1e-6));
            }
        }
        // Grown pieces reach past each edge.
        let t = [Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(0.0, 1.0)];
        let g = grown(t, 0.1);
        assert!(g[0].x < -0.1 + 1e-9 && g[0].y < -0.1 + 1e-9 && g[1].x > 10.0, "{g:?}");
        assert_eq!(grown([Vec2::ZERO; 3], 1.0), [Vec2::ZERO; 3]);
        let degenerate = ImagePrim { clips: vec![vec![Vec2::ZERO]], ..im.clone() };
        assert!(degenerate.textured().is_empty());
        assert!(affine([Vec2::ZERO; 3], [Vec2::ZERO, Vec2::X, Vec2::Y]).is_none());
    }

    #[test]
    fn a_picture_in_perspective_is_a_mesh_through_the_eye() {
        use cadcraft_doc::{Drawing, EntityKind, Space};
        use cadcraft_geom::{Camera, Persp, ViewDir};
        // A 20 x 10 picture on the ground, red left half and blue right half, seen from the
        // south-west through a camera.
        let mut d = Drawing::new_metric();
        let px = raster([[200u8, 30, 30, 255], [30, 30, 200, 255]].repeat(2).concat(), 2, 2, false);
        let img =
            Image { u: Vec3::new(10.0, 0.0, 0.0), v: Vec3::new(0.0, 5.0, 0.0), size: Vec2::new(2.0, 2.0), pixels: Some(px), ..Image::default() };
        d.add(&Space::Model, Default::default(), EntityKind::Image(img)).unwrap();
        let dir = ViewDir::named("iso").unwrap();
        let parallel = crate::build(&d, &Space::Model, &crate::Options { view_dir: dir, ..Default::default() });
        assert!(parallel.images.first().is_some_and(|im| im.mesh.is_empty()));
        let target = Vec3::new(10.0, 5.0, 0.0);
        let cam = Camera::new(50.0, dir.depth(target));
        let pdir = dir.with_camera(Some(cam), dir.project(target), cam.height_at(40.0));
        let list = crate::build(&d, &Space::Model, &crate::Options { view_dir: pdir, ..Default::default() });
        let im = list.images.first().expect("the picture in perspective");
        assert!(im.mesh.len() >= (GRID * GRID * 6) as usize, "{}", im.mesh.len());
        // The texture runs over the whole picture and the mesh lies inside the list's bounds.
        let (lo, hi) = im.mesh.iter().fold((f32::MAX, f32::MIN), |(lo, hi), (_, uv)| (lo.min(uv[0]).min(uv[1]), hi.max(uv[0]).max(uv[1])));
        assert!(lo.abs() < 1e-6 && (hi - 1.0).abs() < 1e-6, "{lo} {hi}");
        assert!(im.mesh.iter().all(|(p, _)| list.bounds.contains(*p)));
        // The picture's corners are where the camera sees the image's.
        for c in [Vec3::ZERO, Vec3::new(20.0, 10.0, 0.0)] {
            let want = pdir.project(c);
            assert!(im.mesh.iter().any(|(p, _)| p.dist(want) < 1e-6), "{want:?}");
        }
        // Drawn by the CPU raster: the picture's colours show, not only its frame.
        let view = View::fit(&list.bounds, 200, 200, 0.1);
        let pm = crate::raster::render(&list, &view, &crate::raster::RasterOptions::default()).unwrap();
        let reds = pm.pixels().iter().filter(|p| p.red() > 150 && p.blue() < 80).count();
        let blues = pm.pixels().iter().filter(|p| p.blue() > 150 && p.red() < 80).count();
        assert!(reds > 200 && blues > 200, "{reds} red, {blues} blue pixels");
        // A camera looking away from the picture sees none of it.
        let away = ViewDir { persp: Some(Persp { eye: Vec3::new(0.0, 0.0, -1e6), focal: 1.0 }), ..pdir };
        assert!(crate::build(&d, &Space::Model, &crate::Options { view_dir: away, ..Default::default() }).images.is_empty());
    }

    /// DVIEW's clipping planes cut a picture as they cut its frame, in a parallel view and in
    /// perspective.
    #[test]
    fn clipping_planes_cut_a_picture() {
        use cadcraft_doc::{Drawing, EntityKind, Space};
        use cadcraft_geom::{Camera, Clip, ViewDir};
        let mut d = Drawing::new_metric();
        let px = raster(vec![200; 16], 2, 2, false);
        let img =
            Image { u: Vec3::new(10.0, 0.0, 0.0), v: Vec3::new(0.0, 5.0, 0.0), size: Vec2::new(2.0, 2.0), pixels: Some(px), ..Image::default() };
        d.add(&Space::Model, Default::default(), EntityKind::Image(img)).unwrap();
        let dir = ViewDir::named("iso").unwrap();
        let images = |v: ViewDir| crate::build(&d, &Space::Model, &crate::Options { view_dir: v, ..Default::default() }).images;
        let whole = images(dir).first().map(|im| im.placement.det().abs()).unwrap();
        // A front plane through the middle (x + y = 15) keeps the farther half.
        let mid = dir.depth(Vec3::new(10.0, 5.0, 0.0));
        let cut = images(ViewDir { clip: Clip::new(Some(mid), None), ..dir });
        let im = cut.first().expect("half of the picture");
        assert!((area(&im.mesh) / whole - 0.5).abs() < 1e-6, "{}", area(&im.mesh) / whole);
        // Wholly behind the back plane: none of it.
        assert!(images(ViewDir { clip: Clip::new(None, Some(1.0)), ..dir }).is_empty());
        // In perspective the front plane cuts it too.
        let target = Vec3::new(10.0, 5.0, 0.0);
        let cam = Camera::new(50.0, dir.depth(target));
        let p = dir.with_camera(Some(cam), dir.project(target), cam.height_at(40.0));
        let full = images(p).first().map(|im| area(&im.mesh)).unwrap();
        let front = images(ViewDir { clip: Clip::new(Some(mid), None), ..p }).first().map(|im| area(&im.mesh)).unwrap();
        assert!(front > 0.0 && front < full * 0.9, "{front} of {full}");
        assert!(images(ViewDir { clip: Clip::new(None, Some(1.0)), ..p }).is_empty());
    }
}
