//! CPU rasteriser: draws a display list with tiny-skia (PNG export, plot preview, tests).
//!
//! Primitives are drawn in list order (painter's), except that solids' opaque shaded faces
//! ([`DisplayList::faces`]) are depth-tested against each other, and the solids' edges of Shaded
//! with Edges ([`DisplayList::edges`]) go on top where no face hides them, as on the GPU canvas.

use cadcraft_color::Rgb;
use cadcraft_geom::{Bounds2, Vec2};
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::{DisplayList, Kind};

/// A 2D view: world point at the image centre and pixels per drawing unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub center: Vec2,
    pub scale: f64,
    pub width: u32,
    pub height: u32,
}

impl View {
    pub fn to_screen(&self, p: Vec2) -> (f32, f32) {
        let x = (p.x - self.center.x) * self.scale + f64::from(self.width) / 2.0;
        let y = f64::from(self.height) / 2.0 - (p.y - self.center.y) * self.scale;
        (x as f32, y as f32)
    }
    /// Fit `b` with a margin fraction.
    pub fn fit(b: &Bounds2, width: u32, height: u32, margin: f64) -> View {
        if b.is_empty() {
            return View { center: Vec2::ZERO, scale: 1.0, width, height };
        }
        let w = b.width().max(1e-9);
        let h = b.height().max(1e-9);
        let s = (f64::from(width) / w).min(f64::from(height) / h) * (1.0 - margin);
        View { center: b.center(), scale: if s.is_finite() && s > 0.0 { s } else { 1.0 }, width, height }
    }
    /// The view of `window` (drawing units), filling the image: `width` × `height` pixels when both
    /// are given; otherwise the side given (default 1600 wide) with the other matching the window's
    /// shape, each side 16..=8192.
    pub fn of_window(window: &Bounds2, width: Option<u32>, height: Option<u32>) -> View {
        let aspect = if window.width() > 1e-12 && window.height() > 1e-12 { window.width() / window.height() } else { 1.0 };
        let side = |v: f64| v.round().clamp(16.0, 8192.0) as u32;
        let (w, h) = match (width, height) {
            (Some(w), Some(h)) => (side(f64::from(w)), side(f64::from(h))),
            (None, Some(h)) => (side(f64::from(h) * aspect), side(f64::from(h))),
            (w, None) => {
                let w = f64::from(w.unwrap_or(1600));
                (side(w), side(w / aspect))
            }
        };
        View::fit(window, w, h, 0.0)
    }
    pub fn world_bounds(&self) -> Bounds2 {
        let hw = f64::from(self.width) / 2.0 / self.scale;
        let hh = f64::from(self.height) / 2.0 / self.scale;
        Bounds2::new(self.center - Vec2::new(hw, hh), self.center + Vec2::new(hw, hh))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RasterOptions {
    pub background: Rgb,
    /// Line width in pixels for zero-lineweight lines.
    pub hairline: f32,
    /// Pixels per mm for lineweights (0 = ignore lineweights).
    pub px_per_mm: f32,
    pub antialias: bool,
}

impl Default for RasterOptions {
    fn default() -> Self {
        RasterOptions { background: Rgb(33, 40, 48), hairline: 1.0, px_per_mm: 0.0, antialias: true }
    }
}

impl RasterOptions {
    /// On white, as on paper: for images read rather than worked in (a named view rendered for the
    /// AI assistant), where the solids' dark edges would vanish into the canvas's dark background.
    pub fn paper() -> Self {
        RasterOptions { background: Rgb(255, 255, 255), ..Default::default() }
    }
}

/// Render to a pixmap. Returns `None` for zero or absurd sizes.
pub fn render(list: &DisplayList, view: &View, o: &RasterOptions) -> Option<Pixmap> {
    if view.width == 0 || view.height == 0 || view.width > 16384 || view.height > 16384 {
        return None;
    }
    let mut pm = Pixmap::new(view.width, view.height)?;
    pm.fill(tiny_skia::Color::from_rgba8(o.background.0, o.background.1, o.background.2, 255));
    let vis = view.world_bounds();
    let mut faces = list.faces.iter().peekable();
    // Depths relative to one of the list's own, so f32 keeps them precise far from the origin.
    let zref = list.tri_depth.first().copied().unwrap_or(0.0);
    let mut zbuf = if list.faces.is_empty() { None } else { depth_buffer(view) };
    for (i, p) in list.prims.iter().enumerate() {
        let c = p.display_rgb(o.background);
        let mut paint = Paint::default();
        // A transparent object blends with what is drawn under it.
        paint.set_color_rgba8(c.0, c.1, c.2, p.alpha());
        // Solids' shaded faces are filled aliased, as on the GPU canvas: a face's triangles are
        // spread over several primitives, and anti-aliased neighbours leave seams between them.
        while faces.next_if(|r| r.end as usize <= i).is_some() {}
        let face = faces.peek().is_some_and(|r| r.start as usize <= i);
        paint.anti_alias = o.antialias && !face;
        if face
            && p.kind == Kind::Tris
            && p.alpha() == u8::MAX
            && let Some(z) = zbuf.as_mut()
        {
            fill_face(&mut pm, z, list, p, view, zref, c);
            continue;
        }
        let pts = list.points(p);
        match p.kind {
            Kind::Polyline => {
                let bb = Bounds2::from_points(pts.iter().copied());
                if !bb.intersects(&vis) {
                    continue;
                }
                let mut pb = PathBuilder::new();
                for (i, q) in pts.iter().enumerate() {
                    let (x, y) = view.to_screen(*q);
                    if i == 0 {
                        pb.move_to(x, y);
                    } else {
                        pb.line_to(x, y);
                    }
                }
                if let Some(path) = pb.finish() {
                    let w = if o.px_per_mm > 0.0 && p.lw > 0.0 { (p.lw * o.px_per_mm).max(o.hairline) } else { o.hairline };
                    let stroke = Stroke { width: w, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
                    pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                }
            }
            Kind::Tris | Kind::Mask => {
                if p.kind == Kind::Mask {
                    paint.set_color_rgba8(o.background.0, o.background.1, o.background.2, 255);
                }
                let mut pb = PathBuilder::new();
                for t in pts.chunks(3) {
                    if let [a, b, c] = t {
                        let (ax, ay) = view.to_screen(*a);
                        let (bx, by) = view.to_screen(*b);
                        let (cx, cy) = view.to_screen(*c);
                        pb.move_to(ax, ay);
                        pb.line_to(bx, by);
                        pb.line_to(cx, cy);
                        pb.close();
                    }
                }
                if let Some(path) = pb.finish() {
                    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
                }
            }
            Kind::Point => {
                if let Some(q) = pts.first() {
                    let (x, y) = view.to_screen(*q);
                    // Filled as a path: tiny-skia's anti-aliased `fill_rect` trips a debug
                    // assertion on rects under two pixels wide whose sides both fall mid-pixel.
                    if let Some(r) = tiny_skia::Rect::from_xywh(x - 0.5, y - 0.5, 1.5, 1.5) {
                        pm.fill_path(&PathBuilder::from_rect(r), &paint, FillRule::Winding, Transform::identity(), None);
                    }
                }
            }
            Kind::Infinite { ray } => {
                let (Some(base), Some(dir)) = (pts.first(), pts.get(1)) else { continue };
                let far = (vis.width() + vis.height()) * 2.0 + base.dist(vis.center());
                let a = if ray { *base } else { *base - *dir * far };
                let b = *base + *dir * far;
                let mut pb = PathBuilder::new();
                let (ax, ay) = view.to_screen(a);
                let (bx, by) = view.to_screen(b);
                pb.move_to(ax, ay);
                pb.line_to(bx, by);
                if let Some(path) = pb.finish() {
                    pm.stroke_path(&path, &paint, &Stroke { width: o.hairline, ..Default::default() }, Transform::identity(), None);
                }
            }
        }
    }
    if let Some(z) = &zbuf {
        draw_edges(&mut pm, z, list, view, o, zref);
    }
    Some(pm)
}

/// Stroke the solids' edges ([`DisplayList::edges`]) where no shaded face hides them: each edge
/// is walked a pixel at a time against the faces' depths `zbuf` ([`fill_face`]) and its visible
/// runs are stroked as hairlines.
fn draw_edges(pm: &mut Pixmap, zbuf: &[f32], list: &DisplayList, view: &View, o: &RasterOptions, zref: f64) {
    let (w, h) = (view.width as usize, view.height as usize);
    let face_at = |x: f64, y: f64| {
        let inside = x >= 0.0 && y >= 0.0 && (x as usize) < w && (y as usize) < h;
        let z = if inside { zbuf.get(y as usize * w + x as usize) } else { None };
        z.map_or(f64::NEG_INFINITY, |z| f64::from(*z))
    };
    let px = |q: Vec2| {
        let (x, y) = view.to_screen(q);
        (f64::from(x), f64::from(y))
    };
    for e in &list.edges {
        let pts = list.points(e);
        let Some(z) = list.vert_depth.get(e.start as usize..e.start as usize + e.len as usize) else { continue };
        let mut pb = PathBuilder::new();
        for (s, d) in pts.windows(2).zip(z.windows(2)) {
            let ([a, b], [za, zb]) = (s, d) else { continue };
            let (a, b) = (px(*a), px(*b));
            let Some((t0, t1)) = clip(a, b, f64::from(view.width), f64::from(view.height)) else { continue };
            let n = ((t1 - t0) * (b.0 - a.0).hypot(b.1 - a.1)).ceil().clamp(1.0, 65536.0) as u32;
            let mut down = false;
            for k in 0..=n {
                let t = t0 + (t1 - t0) * f64::from(k) / f64::from(n);
                let (x, y) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                let seen = za + (zb - za) * t - zref >= face_at(x, y);
                match (seen, down) {
                    (true, true) => pb.line_to(x as f32, y as f32),
                    (true, false) => pb.move_to(x as f32, y as f32),
                    _ => {}
                }
                down = seen;
            }
        }
        if let Some(path) = pb.finish() {
            let c = e.display_rgb(o.background);
            let mut paint = Paint::default();
            paint.set_color_rgba8(c.0, c.1, c.2, e.alpha());
            paint.anti_alias = o.antialias;
            let stroke = Stroke { width: o.hairline, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
            pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }
}

/// A depth buffer for the view, every pixel at -inf; `None` when it can't be allocated.
fn depth_buffer(view: &View) -> Option<Vec<f32>> {
    let n = (view.width as usize).checked_mul(view.height as usize)?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(n).ok()?;
    buf.resize(n, f32::NEG_INFINITY);
    Some(buf)
}

/// Fill the triangles of shaded face `p` in `color` where they are nearer than what `zbuf`
/// holds (depths less `zref`, row by row), aliased, sampling pixel centres. Like the GPU canvas,
/// a face's depth is written pushed back by two pixels of its depth slope plus a pixel's worth
/// of depth, so an edge lying on it passes [`draw_edges`]' test.
// ponytail: each triangle scans its bounding box, so long diagonal slivers (fans) test many
// pixels outside themselves; scan row spans if that ever shows in render times.
fn fill_face(pm: &mut Pixmap, zbuf: &mut [f32], list: &DisplayList, p: &crate::DPrim, view: &View, zref: f64, color: Rgb) {
    let (w, h) = (view.width as usize, view.height as usize);
    let Some(ink) = tiny_skia::PremultipliedColorU8::from_rgba(color.0, color.1, color.2, 255) else { return };
    let pixels = pm.pixels_mut();
    let px = 1.0 / view.scale;
    let zs = list.tri_depth.get(p.start as usize..p.start as usize + p.len as usize).unwrap_or(&[]);
    for (t, z) in list.points(p).chunks_exact(3).zip(zs.chunks_exact(3)) {
        let ([a, b, c], [za, zb, zc]) = (t, z) else { continue };
        let s = |q: &Vec2| {
            let (x, y) = view.to_screen(*q);
            (f64::from(x), f64::from(y))
        };
        let ((x0, y0), (x1, y1), (x2, y2)) = (s(a), s(b), s(c));
        let area = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
        if !area.is_finite() || area.abs() < 1e-12 {
            continue;
        }
        let (dz1, dz2) = (zb - za, zc - za);
        let slope = ((dz1 * (y2 - y0) - dz2 * (y1 - y0)) / area).abs().max(((dz2 * (x1 - x0) - dz1 * (x2 - x0)) / area).abs());
        let z0 = za - zref - 2.0 * slope - px;
        let (lx, hx) = (x0.min(x1).min(x2).max(0.0), x0.max(x1).max(x2).min(w as f64 - 1.0));
        let (ly, hy) = (y0.min(y1).min(y2).max(0.0), y0.max(y1).max(y2).min(h as f64 - 1.0));
        if lx > hx || ly > hy {
            continue;
        }
        for y in ly as usize..=hy as usize {
            let cy = y as f64 + 0.5 - y0;
            for x in lx as usize..=hx as usize {
                let cx = x as f64 + 0.5 - x0;
                // Barycentric weights of corners b and c.
                let u = (cx * (y2 - y0) - (x2 - x0) * cy) / area;
                let v = ((x1 - x0) * cy - cx * (y1 - y0)) / area;
                let z = (z0 + u * dz1 + v * dz2) as f32;
                if u >= 0.0
                    && v >= 0.0
                    && u + v <= 1.0
                    && let (Some(d), Some(c)) = (zbuf.get_mut(y * w + x), pixels.get_mut(y * w + x))
                    && z >= *d
                {
                    *d = z;
                    *c = ink;
                }
            }
        }
    }
}

/// The part `t0..=t1` of segment `a`-`b` (pixels) inside the image (`w` x `h`, a pixel beyond its
/// sides), or `None` when it misses.
fn clip(a: (f64, f64), b: (f64, f64), w: f64, h: f64) -> Option<(f64, f64)> {
    let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
    for (p, d, hi) in [(a.0, b.0 - a.0, w), (a.1, b.1 - a.1, h)] {
        if d.abs() < 1e-12 {
            if !(-1.0..=hi + 1.0).contains(&p) {
                return None;
            }
            continue;
        }
        let (u, v) = ((-1.0 - p) / d, (hi + 1.0 - p) / d);
        t0 = t0.max(u.min(v));
        t1 = t1.min(u.max(v));
    }
    (t0 <= t1).then_some((t0, t1))
}

/// Render and encode as PNG.
pub fn render_png(list: &DisplayList, view: &View, o: &RasterOptions) -> Option<Vec<u8>> {
    render(list, view, o)?.encode_png().ok()
}
