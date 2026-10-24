//! Raster images: where an IMAGE entity's pixels and clip boundary land in the drawing, and the
//! decoded pixels it shows ([`Raster`]: read by the engine from the image file, never saved).

use std::sync::Arc;

use cadcraft_geom::{Mat3, Vec2};

use super::Image;

/// Decoded pixels of an attached image: RGBA rows from the top, straight (not premultiplied)
/// alpha.
#[derive(Clone, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// The file has an alpha channel (shown with TRANSPARENCY on).
    pub alpha: bool,
}

impl std::fmt::Debug for Raster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Raster({}x{})", self.width, self.height)
    }
}

impl Raster {
    /// A raster of `width` × `height` pixels; `None` unless `rgba` holds exactly that many.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>, alpha: bool) -> Option<Arc<Raster>> {
        let n = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
        (width > 0 && height > 0 && rgba.len() == n).then(|| Arc::new(Raster { width, height, rgba, alpha }))
    }
}

/// [`Image::display`] bit: the picture is shown.
pub const IMAGE_SHOW: u16 = 1;
/// [`Image::display`] bit: transparent pixels show what is under them (TRANSPARENCY).
pub const IMAGE_TRANSPARENT: u16 = 8;

impl Image {
    /// The image's unit square in the drawing: (0, 0) is its lower-left corner (the insertion
    /// point), (1, 1) its upper-right one.
    pub fn placement(&self) -> Mat3 {
        let (o, u, v) = (self.insert.xy(), self.u.xy() * self.size.x, self.v.xy() * self.size.y);
        Mat3 { a: u.x, b: u.y, c: v.x, d: v.y, e: o.x, f: o.y }
    }

    /// The corners, counter-clockwise from the insertion point.
    pub fn corners(&self) -> [Vec2; 4] {
        let m = self.placement();
        [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|(x, y)| m.apply(Vec2::new(x, y)))
    }

    /// A point in pixel coordinates (as [`Image::clip`]: from the top-left pixel's centre, y down)
    /// in the drawing.
    pub fn pixel_to_world(&self, q: Vec2) -> Vec2 {
        self.insert.xy() + self.u.xy() * (q.x + 0.5) + self.v.xy() * (self.size.y - 0.5 - q.y)
    }

    /// A drawing point in pixel coordinates; `None` when the image has no area.
    pub fn world_to_pixel(&self, p: Vec2) -> Option<Vec2> {
        let (o, u, v) = (self.insert.xy(), self.u.xy(), self.v.xy());
        let r = Mat3 { a: u.x, b: u.y, c: v.x, d: v.y, e: o.x, f: o.y }.inverse()?.apply(p);
        let q = Vec2::new(r.x - 0.5, self.size.y - 0.5 - r.y);
        q.is_finite().then_some(q)
    }

    /// The clip boundary in the drawing while clipping is on (a rectangle as its four corners);
    /// `None`: the whole image shows.
    pub fn clip_outline(&self) -> Option<Vec<Vec2>> {
        if !self.clipping || self.clip.len() < 2 {
            return None;
        }
        let pts = match self.clip.as_slice() {
            [a, b] => vec![*a, Vec2::new(b.x, a.y), *b, Vec2::new(a.x, b.y)],
            vs => vs.to_vec(),
        };
        Some(pts.into_iter().map(|q| self.pixel_to_world(q)).collect())
    }

    /// The image's frame (not closed): its clip boundary, else its edges.
    pub fn outline(&self) -> Vec<Vec2> {
        self.clip_outline().unwrap_or_else(|| self.corners().to_vec())
    }

    /// The picture is drawn (shown and not unloaded; it still needs its pixels).
    pub fn shown(&self) -> bool {
        self.display & IMAGE_SHOW != 0 && !self.unloaded
    }

    /// TRANSPARENCY is on.
    pub fn transparent(&self) -> bool {
        self.display & IMAGE_TRANSPARENT != 0
    }

    /// The image file's name without its folders.
    pub fn file_name(&self) -> &str {
        self.path.rsplit(['/', '\\']).next().unwrap_or(&self.path)
    }

    /// The image with its corner grip `index` (counter-clockwise from the insertion point)
    /// dragged to `to`: scaled about the opposite corner, keeping its shape. `None` when that
    /// leaves no size.
    pub fn stretched(&self, index: usize, to: Vec2) -> Option<Image> {
        let c = self.corners();
        let (from, fixed) = (*c.get(index % 4)?, *c.get((index + 2) % 4)?);
        let d = from - fixed;
        let s = (to - fixed).dot(d) / d.len2();
        if !(s.is_finite() && s > 1e-9) {
            return None;
        }
        let o = fixed + (self.insert.xy() - fixed) * s;
        Some(Image { insert: o.to3(self.insert.z), u: self.u * s, v: self.v * s, ..self.clone() })
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_geom::Vec3;

    use super::*;

    fn photo() -> Image {
        // 40 x 20 pixels of 0.5 units, rotated 90 degrees, at (10, 5).
        Image {
            insert: Vec3::new(10.0, 5.0, 0.0),
            u: Vec3::new(0.0, 0.5, 0.0),
            v: Vec3::new(-0.5, 0.0, 0.0),
            size: Vec2::new(40.0, 20.0),
            ..Image::default()
        }
    }

    #[test]
    fn pixels_and_corners_land_in_the_drawing() {
        let i = photo();
        let c = i.corners();
        assert!(c[0].near(Vec2::new(10.0, 5.0), 1e-12) && c[1].near(Vec2::new(10.0, 25.0), 1e-12) && c[2].near(Vec2::new(0.0, 25.0), 1e-12));
        // The top-left pixel corner is the upper-left corner of the image.
        assert!(i.pixel_to_world(Vec2::new(-0.5, -0.5)).near(c[3], 1e-12));
        assert!(i.pixel_to_world(Vec2::new(39.5, 19.5)).near(c[1], 1e-12));
        let q = Vec2::new(7.0, 3.0);
        assert!(i.world_to_pixel(i.pixel_to_world(q)).is_some_and(|r| r.near(q, 1e-9)));
        assert!(Image { size: Vec2::ZERO, u: Vec3::ZERO, ..photo() }.world_to_pixel(q).is_none());
    }

    #[test]
    fn clip_outline_and_frame() {
        let mut i = photo();
        assert_eq!(i.outline().len(), 4);
        i.clip = vec![Vec2::new(-0.5, -0.5), Vec2::new(19.5, 9.5)];
        assert!(i.clip_outline().is_none(), "clipping off");
        i.clipping = true;
        let o = i.clip_outline().unwrap();
        assert_eq!(o.len(), 4);
        assert!(o[0].near(i.corners()[3], 1e-12));
    }

    #[test]
    fn corner_grip_scales_about_the_opposite_corner() {
        let i = Image { u: Vec3::new(1.0, 0.0, 0.0), v: Vec3::new(0.0, 1.0, 0.0), size: Vec2::new(4.0, 2.0), ..Image::default() };
        let s = i.stretched(2, Vec2::new(8.0, 4.0)).unwrap();
        assert!(s.corners()[2].near(Vec2::new(8.0, 4.0), 1e-12) && s.corners()[0].near(Vec2::ZERO, 1e-12));
        let s = i.stretched(0, Vec2::new(-4.0, -2.0)).unwrap();
        assert!(s.corners()[2].near(Vec2::new(4.0, 2.0), 1e-12) && s.insert.xy().near(Vec2::new(-4.0, -2.0), 1e-12));
        assert!(i.stretched(2, Vec2::new(-1.0, -1.0)).is_none());
        assert!(i.stretched(1, Vec2::new(f64::NAN, 0.0)).is_none());
    }

    #[test]
    fn raster_checks_its_size() {
        assert!(Raster::new(2, 1, vec![0; 8], false).is_some());
        assert!(Raster::new(2, 1, vec![0; 7], false).is_none());
        assert!(Raster::new(0, 0, Vec::new(), false).is_none());
        assert!(Raster::new(u32::MAX, u32::MAX, Vec::new(), false).is_none());
        assert_eq!(format!("{:?}", Raster::new(2, 1, vec![0; 8], true).unwrap()), "Raster(2x1)");
    }
}
