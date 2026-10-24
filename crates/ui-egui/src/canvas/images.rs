//! Raster images on the canvas: each picture of a display list
//! ([`cadcraft_render::image::ImagePrim`]) is uploaded once as a texture (per picture and look)
//! and painted as a textured mesh clipped to what shows of it.
//!
//! ponytail: pictures paint before the list's GPU callback, so they sit under every line and fill
//! (an image brought to the front with DRAWORDER still draws behind them on screen; plots and
//! PNG exports keep the draw order). Draw them in the wgpu pass if that matters.

use std::sync::Arc;

use cadcraft_doc::Raster;
use cadcraft_render::DisplayList;
use cadcraft_render::image::{Look, adjusted};
use egui::epaint::Vertex;
use egui::{Color32, ColorImage, Mesh, Rect, Shape, TextureHandle, TextureOptions, pos2};

use super::Xf;

/// One uploaded picture.
#[derive(Clone)]
struct Tex {
    pixels: Arc<Raster>,
    look: Look,
    tex: TextureHandle,
    /// The frame (egui pass) it was last painted in.
    used: u64,
}

#[derive(Clone, Default)]
struct Textures(Vec<Tex>);

/// A texture not painted for this many frames is let go.
const KEEP_FRAMES: u64 = 120;

/// Paint the pictures of `list` (in view-plane coordinates, as `xf` shows them) with `p`.
pub(crate) fn paint(p: &egui::Painter, xf: &Xf, list: &DisplayList) {
    let ctx = p.ctx();
    let id = egui::Id::new("cad-image-textures");
    let now = ctx.cumulative_pass_nr();
    let mut texs: Textures = ctx.data_mut(|d| std::mem::take(d.get_temp_mut_or_default::<Textures>(id)));
    let max_side = ctx.input(|i| i.max_texture_side).max(1);
    for im in &list.images {
        let corners = im.corners().map(|c| xf.view_to_screen(c));
        if !Rect::from_points(&corners).intersects(p.clip_rect()) {
            continue;
        }
        let tex = match texs.0.iter_mut().find(|t| Arc::ptr_eq(&t.pixels, &im.pixels) && t.look == im.look) {
            Some(t) => {
                t.used = now;
                t.tex.id()
            }
            None => {
                let Some(img) = color_image(&im.pixels, &im.look, max_side) else { continue };
                let opts = if im.look.draft { TextureOptions::NEAREST } else { TextureOptions::LINEAR };
                let tex = ctx.load_texture("cad-image", img, opts);
                let tid = tex.id();
                texs.0.push(Tex { pixels: im.pixels.clone(), look: im.look, tex, used: now });
                tid
            }
        };
        let mut mesh = Mesh::with_texture(tex);
        for q in im.triangles() {
            let Some([u, v]) = im.uv(q) else { break };
            let i = mesh.vertices.len() as u32;
            mesh.vertices.push(Vertex { pos: xf.view_to_screen(q), uv: pos2(u, v), color: Color32::WHITE });
            mesh.indices.push(i);
        }
        if mesh.indices.len() >= 3 && mesh.indices.len().is_multiple_of(3) {
            p.add(Shape::mesh(mesh));
        }
    }
    texs.0.retain(|t| now.saturating_sub(t.used) <= KEEP_FRAMES);
    ctx.data_mut(|d| d.insert_temp(id, texs));
}

/// The picture as `look` shows it, at most `max_side` pixels on a side (every n-th pixel of a
/// bigger one).
fn color_image(px: &Raster, look: &Look, max_side: usize) -> Option<ColorImage> {
    let rgba = adjusted(px, look);
    let (w, h) = (px.width as usize, px.height as usize);
    if rgba.len() != w.checked_mul(h)?.checked_mul(4)? || w == 0 || h == 0 {
        return None;
    }
    let n = w.max(h).div_ceil(max_side);
    if n <= 1 {
        return Some(ColorImage::from_rgba_unmultiplied([w, h], &rgba));
    }
    let (nw, nh) = (w.div_ceil(n), h.div_ceil(n));
    let mut out = Vec::with_capacity(nw * nh * 4);
    for y in (0..h).step_by(n) {
        for x in (0..w).step_by(n) {
            let o = (y * w + x) * 4;
            out.extend_from_slice(rgba.get(o..o + 4)?);
        }
    }
    (out.len() == nw * nh * 4).then(|| ColorImage::from_rgba_unmultiplied([nw, nh], &out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_fit_the_largest_texture() {
        let px = Raster::new(5, 3, vec![200; 60], false).unwrap();
        let look = Look { brightness: 50, contrast: 50, fade: 0, transparent: true, alpha: 255, draft: false };
        assert_eq!(color_image(&px, &look, 8).unwrap().size, [5, 3]);
        assert_eq!(color_image(&px, &look, 2).unwrap().size, [2, 1]);
        assert_eq!(color_image(&px, &look, 1).unwrap().size, [1, 1]);
    }

    #[test]
    fn a_picture_paints_as_a_textured_mesh() {
        let ctx = egui::Context::default();
        let mut list = DisplayList::default();
        list.images.push(cadcraft_render::image::ImagePrim {
            handle: cadcraft_doc::Handle(1),
            at: 0,
            placement: cadcraft_geom::Mat3 { a: 10.0, b: 0.0, c: 0.0, d: 5.0, e: 0.0, f: 0.0 },
            clips: Vec::new(),
            pixels: Raster::new(2, 2, vec![255; 16], false).unwrap(),
            look: Look { brightness: 50, contrast: 50, fade: 0, transparent: false, alpha: 255, draft: false },
        });
        let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(200.0, 100.0));
        let xf = Xf::new(rect, cadcraft_geom::Vec2::new(5.0, 2.5), 10.0);
        let mut textures = 0;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let ctx = ui.ctx();
            let p = egui::Painter::new(ctx.clone(), egui::LayerId::background(), rect);
            paint(&p, &xf, &list);
            paint(&p, &xf, &list);
            textures = ctx.data(|d| d.get_temp::<Textures>(egui::Id::new("cad-image-textures"))).map_or(0, |t| t.0.len());
        });
        // The font atlas and the picture.
        let uploads = out.textures_delta.set.len();
        out.textures_delta.clear();
        assert_eq!(uploads, 2);
        assert_eq!(textures, 1, "one texture for the picture, painted twice");
    }
}
