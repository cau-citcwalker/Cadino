//! Raster images: an IMAGE entity's picture goes into the display list as an [`ImagePrim`]
//! (drawn here by the CPU raster, by the canvas as a texture), with its frame per IMAGEFRAME (0
//! none, 1 shown and plotted, 2 shown but not plotted). An image whose file isn't loaded shows its
//! frame and file name instead.

use std::sync::Arc;

use cadcraft_doc::{Handle, Image, Raster, Text};
use cadcraft_geom::{Bounds2, Mat3, Vec2};
use tiny_skia::{FillRule, FilterQuality, Mask, Paint, PathBuilder, Pattern, Pixmap, SpreadMode, Transform};

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
    /// The picture's unit square in list coordinates: (0, 0) its lower-left corner, (1, 1) the
    /// upper-right one.
    pub placement: Mat3,
    /// Polygons in list coordinates the picture is clipped to, every one of them: its clip
    /// boundary, the outline of the viewport it is seen through.
    pub clips: Vec<Vec<Vec2>>,
    pub pixels: Arc<Raster>,
    pub look: Look,
}

impl ImagePrim {
    /// The picture's corners in list coordinates.
    pub fn corners(&self) -> [Vec2; 4] {
        [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|(x, y)| self.placement.apply(Vec2::new(x, y)))
    }

    /// What shows of the picture, as triangles in list coordinates (three points each).
    pub fn triangles(&self) -> Vec<Vec2> {
        let [a, b, c, d] = self.corners();
        let mut tris = vec![a, b, c, a, c, d];
        for clip in &self.clips {
            tris = match crate::clip::PolyClip::new(clip) {
                Some(pc) => pc.clip_triangles(&tris),
                None => Vec::new(),
            };
        }
        tris
    }

    /// The texture coordinates of list point `p`: 0..1 across, 0 at the top row to 1 at the
    /// bottom. `None` for a picture with no area.
    pub fn uv(&self, p: Vec2) -> Option<[f32; 2]> {
        let q = self.placement.inverse()?.apply(p);
        q.is_finite().then_some([q.x as f32, (1.0 - q.y) as f32])
    }
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

/// Put entity `e`'s image into the list: its picture, its frame per IMAGEFRAME, or its frame and
/// file name while the picture isn't there.
pub(crate) fn build(b: &mut Builder, ctx: &Ctx, i: &Image, rgb: Ink, lw: f32) {
    let pic = i.pixels.as_ref().filter(|_| i.shown());
    let placement = ctx.xf.then_before(i.placement());
    let ok = [placement.a, placement.b, placement.c, placement.d, placement.e, placement.f].iter().all(|v| v.is_finite());
    // ponytail: a perspective view shows the frame only (the picture is placed affinely); map
    // the picture through the camera (a textured, near-clipped mesh) if that matters.
    if let Some(px) = pic
        && ok
        && placement.det().abs() > 1e-300
        && b.opts.view_dir.persp.is_none()
    {
        let clips = i.clip_outline().map(|o| o.iter().map(|p| ctx.xf.apply(*p)).collect()).into_iter().collect();
        let look = Look {
            brightness: i.brightness,
            contrast: i.contrast,
            fade: i.fade,
            transparent: i.transparent(),
            alpha: crate::alpha(b.transparency),
            draft: ctx.d.header.i64("IMAGEQUALITY", 1) == 0,
        };
        let im = ImagePrim { handle: ctx.top, at: b.list.prims.len() as u32, placement, clips, pixels: px.clone(), look };
        for c in im.corners() {
            b.list.bounds.add(c);
        }
        b.list.images.push(im);
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
    let mut rgba = adjusted(px, &im.look);
    for p in rgba.as_chunks_mut::<4>().0 {
        let a = u32::from(p[3]);
        for c in p.iter_mut().take(3) {
            *c = ((u32::from(*c) * a + 127) / 255) as u8;
        }
    }
    let Some(size) = tiny_skia::IntSize::from_wh(px.width, px.height) else { return };
    let Some(src) = Pixmap::from_vec(rgba, size) else { return };
    // Pixel (column, row from the top) → the unit square → list → screen.
    let (w, h) = (f64::from(px.width), f64::from(px.height));
    let unit = Mat3 { a: 1.0 / w, b: 0.0, c: 0.0, d: -1.0 / h, e: 0.0, f: 1.0 };
    let (cx, cy) = (f64::from(view.width) / 2.0 - view.center.x * view.scale, f64::from(view.height) / 2.0 + view.center.y * view.scale);
    let to_screen = Mat3 { a: view.scale, b: 0.0, c: 0.0, d: -view.scale, e: cx, f: cy };
    let m = to_screen.then_before(im.placement).then_before(unit);
    let ts = Transform::from_row(m.a as f32, m.b as f32, m.c as f32, m.d as f32, m.e as f32, m.f as f32);
    let quality = if im.look.draft { FilterQuality::Nearest } else { FilterQuality::Bilinear };
    let paint = Paint { shader: Pattern::new(src.as_ref(), SpreadMode::Pad, quality, 1.0, ts), anti_alias: true, ..Default::default() };
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
    fn clip_cuts_the_triangles_and_uv_maps_back() {
        let im = ImagePrim {
            handle: Handle(1),
            at: 0,
            placement: Mat3 { a: 4.0, b: 0.0, c: 0.0, d: 2.0, e: 10.0, f: 0.0 },
            clips: Vec::new(),
            pixels: raster(vec![0; 16], 2, 2, false),
            look: PLAIN,
        };
        assert_eq!(im.triangles().len(), 6);
        assert_eq!(im.uv(Vec2::new(10.0, 2.0)), Some([0.0, 0.0]));
        assert_eq!(im.uv(Vec2::new(14.0, 0.0)), Some([1.0, 1.0]));
        let left =
            ImagePrim { clips: vec![vec![Vec2::new(9.0, -1.0), Vec2::new(12.0, -1.0), Vec2::new(12.0, 3.0), Vec2::new(9.0, 3.0)]], ..im.clone() };
        let area: f64 = left.triangles().chunks(3).map(|t| ((t[1] - t[0]).cross(t[2] - t[0]) / 2.0).abs()).sum();
        assert!((area - 4.0).abs() < 1e-9, "{area}");
        let degenerate = ImagePrim { clips: vec![vec![Vec2::ZERO]], ..im.clone() };
        assert!(degenerate.triangles().is_empty());
        assert!(ImagePrim { placement: Mat3 { a: 0.0, d: 0.0, ..im.placement }, ..im }.uv(Vec2::ZERO).is_none());
    }
}
