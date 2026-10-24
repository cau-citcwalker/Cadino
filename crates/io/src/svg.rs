//! SVG export of a space (vector, for the web and documents). Raster images' pictures go in as
//! PNG data URIs, clipped as they show.

use std::fmt::Write as _;

use cadcraft_color::Rgb;
use cadcraft_doc::{Drawing, Space};
use std::sync::Arc;

use cadcraft_geom::{Bounds2, Mat3, Vec2};
use cadcraft_render::Kind;
use cadcraft_render::image::ImagePrim;

/// Export with a white background (colour 7 prints black), fitted to the extents.
pub fn export(d: &Drawing, space: &Space) -> String {
    export_window(d, space, None)
}

/// Export showing exactly `window` (the `viewBox`; geometry outside it is clipped by the viewer),
/// or the extents with a 2% margin when `window` is `None`. Objects are drawn with their
/// transparency, as on screen.
pub fn export_window(d: &Drawing, space: &Space, window: Option<Bounds2>) -> String {
    export_window_with(d, space, window, true)
}

/// [`export_window`], drawing transparency only when `transparency` is on (plots).
pub fn export_window_with(d: &Drawing, space: &Space, window: Option<Bounds2>, transparency: bool) -> String {
    let list = cadcraft_render::build(d, space, &cadcraft_render::Options { transparency, ..cadcraft_render::Options::default() });
    export_list(&list, window)
}

/// ` opacity="…"` for a transparent primitive (none for an opaque one or a wipeout's mask).
fn opacity(p: &cadcraft_render::DPrim) -> String {
    if p.kind == Kind::Mask || p.transparency == 0 {
        return String::new();
    }
    format!(r#" opacity="{:.2}""#, f64::from(p.alpha()) / 255.0)
}

/// Write a display list (as [`export_window`] does for a space's own).
pub fn export_list(list: &cadcraft_render::DisplayList, window: Option<Bounds2>) -> String {
    let window = window.filter(|w| !w.is_empty());
    let b = window.unwrap_or(list.bounds);
    let (w, h) = if b.is_empty() { (1.0, 1.0) } else { (b.width().max(1e-9), b.height().max(1e-9)) };
    let m = if window.is_some() { 0.0 } else { (w.max(h)) * 0.02 };
    let (x0, y1) = if b.is_empty() { (0.0, 1.0) } else { (b.min.x - m, b.max.y + m) };
    let vw = w + 2.0 * m;
    let vh = h + 2.0 * m;
    let stroke = vw.max(vh) / 1500.0;
    let mut s = String::new();
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 {vw:.6} {vh:.6}" width="{:.0}" height="{:.0}"><rect width="100%" height="100%" fill="white"/>"#,
        1600.0,
        1600.0 * vh / vw
    );
    let white = Rgb(255, 255, 255);
    let tx = |x: f64| x - x0;
    let ty = |y: f64| y1 - y;
    let mut pictures = Pictures::default();
    let mut images = list.images.iter().peekable();
    for (i, p) in list.prims.iter().enumerate() {
        while let Some(im) = images.next_if(|im| im.at as usize <= i) {
            pictures.put(&mut s, im, x0, y1, stroke * 0.75);
        }
        let c = p.display_rgb(white).hex();
        let op = opacity(p);
        let pts = list.points(p);
        match p.kind {
            Kind::Polyline => {
                let _ = write!(
                    s,
                    r#"<polyline fill="none" stroke="{c}"{op} stroke-width="{stroke:.6}" stroke-linecap="round" stroke-linejoin="round" points=""#
                );
                for q in pts {
                    let _ = write!(s, "{:.6},{:.6} ", tx(q.x), ty(q.y));
                }
                s.push_str(r#""/>"#);
            }
            Kind::Tris | Kind::Mask => {
                // A wipeout's mask hides what lies under it with the white background.
                let c = if p.kind == Kind::Mask { white.hex() } else { c };
                let _ = write!(s, r#"<path fill="{c}"{op} d=""#);
                for t in pts.chunks(3) {
                    if let [a, b2, cc] = t {
                        let _ = write!(s, "M{:.6} {:.6}L{:.6} {:.6}L{:.6} {:.6}Z", tx(a.x), ty(a.y), tx(b2.x), ty(b2.y), tx(cc.x), ty(cc.y));
                    }
                }
                s.push_str(r#""/>"#);
            }
            Kind::Point => {
                if let Some(q) = pts.first() {
                    let _ = write!(s, r#"<circle cx="{:.6}" cy="{:.6}" r="{:.6}" fill="{c}"{op}/>"#, tx(q.x), ty(q.y), stroke);
                }
            }
            Kind::Infinite { ray } => {
                if let (Some(bp), Some(dir)) = (pts.first(), pts.get(1)) {
                    // A window can lie far from the base point: reach past it.
                    let far = (vw + vh) * 2.0 + if window.is_some() { bp.dist(b.center()) } else { 0.0 };
                    let a = if ray { *bp } else { *bp - *dir * far };
                    let e = *bp + *dir * far;
                    let _ = write!(
                        s,
                        r#"<line x1="{:.6}" y1="{:.6}" x2="{:.6}" y2="{:.6}" stroke="{c}"{op} stroke-width="{stroke:.6}"/>"#,
                        tx(a.x),
                        ty(a.y),
                        tx(e.x),
                        ty(e.y)
                    );
                }
            }
        }
    }
    for im in images {
        pictures.put(&mut s, im, x0, y1, stroke * 0.75);
    }
    s.push_str("</svg>");
    s
}

/// The pictures written so far (each once, as `<image id="im<k>">` in `<defs>`) and the clip
/// paths made.
#[derive(Default)]
struct Pictures<'a> {
    written: Vec<&'a ImagePrim>,
    clips: usize,
}

impl<'a> Pictures<'a> {
    /// Write `im` where the SVG's origin is list point (`x0`, `y1`), y down. In perspective each
    /// triangle shows its piece of the picture, grown by `seam` so neighbours leave no gap.
    fn put(&mut self, s: &mut String, im: &'a ImagePrim, x0: f64, y1: f64, seam: f64) {
        let flip = Mat3 { a: 1.0, b: 0.0, c: 0.0, d: -1.0, e: -x0, f: y1 };
        let k = match self.written.iter().position(|w| Arc::ptr_eq(&w.pixels, &im.pixels) && w.look == im.look) {
            Some(k) => k,
            None => {
                let Some(png) = cadcraft_render::image::png(im) else { return };
                let k = self.written.len();
                let draft = if im.look.draft { r#" image-rendering="optimizeSpeed""# } else { "" };
                let _ = write!(
                    s,
                    r#"<defs><image id="im{k}" width="1" height="1" preserveAspectRatio="none"{draft} xlink:href="data:image/png;base64,{}"/></defs>"#,
                    base64(&png)
                );
                self.written.push(im);
                k
            }
        };
        let mut clip = |s: &mut String, pts: &[Vec2]| {
            let n = self.clips;
            self.clips += 1;
            let _ = write!(s, r#"<clipPath id="ic{n}"><path d=""#);
            for (j, q) in pts.iter().enumerate() {
                let q = flip.apply(*q);
                let _ = write!(s, "{}{:.6} {:.6}", if j == 0 { 'M' } else { 'L' }, q.x, q.y);
            }
            let _ = write!(s, r#"Z"/></clipPath><g clip-path="url(#ic{n})">"#);
        };
        let use_ = |s: &mut String, m: Mat3| {
            let m = flip.then_before(m);
            let _ =
                write!(s, r##"<use xlink:href="#im{k}" transform="matrix({:.9} {:.9} {:.9} {:.9} {:.6} {:.6})"/>"##, m.a, m.b, m.c, m.d, m.e, m.f);
        };
        if im.mesh.is_empty() {
            let clips: Vec<&Vec<Vec2>> = im.clips.iter().filter(|c| c.len() >= 3).collect();
            for c in &clips {
                clip(s, c);
            }
            use_(s, im.texture_xf());
            s.push_str(&"</g>".repeat(clips.len()));
            return;
        }
        for (t, m) in im.pieces() {
            clip(s, &cadcraft_render::image::grown(t, seam));
            use_(s, m);
            s.push_str("</g>");
        }
    }
}

/// Standard base64 (RFC 4648, padded).
fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = c.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            let ch = if i <= c.len() { ABC.get(((n >> (18 - 6 * i)) & 63) as usize).copied().unwrap_or(b'=') } else { b'=' };
            out.push(char::from(ch));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_pads_as_rfc_4648_says() {
        let cases = [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy"), ("\u{ff}", "w78=")];
        for (raw, want) in cases {
            assert_eq!(super::base64(raw.as_bytes()), want);
        }
    }
}
