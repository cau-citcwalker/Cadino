//! `cargo xtask cadino-icon`: draw Cadino's app icon (original work, MIT OR Apache-2.0) and write
//! `assets/app-icon/cadino-1024.png`, `cadino-256.png` and `cadino.ico`.
//!
//! The mark is an isometric amber block (3D modelling) over a dimension line (2D drafting) on a
//! dark slate tile. Every size is drawn from the same vector geometry, not downscaled, so the small
//! ICO entries stay sharp.

use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, Path, PathBuilder, Pixmap, Point, SpreadMode, Stroke, Transform,
};

/// Sizes packed into the Windows `.ico` (Explorer, taskbar and title bar at 100–200 % scaling).
const ICO_SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];

fn rgb(hex: u32) -> Color {
    Color::from_rgba8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, 255)
}

fn paint(color: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color);
    p.anti_alias = true;
    p
}

fn poly(points: &[(f32, f32)], close: bool) -> Result<Path, String> {
    let mut pb = PathBuilder::new();
    for (i, &(x, y)) in points.iter().enumerate() {
        if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
    }
    if close {
        pb.close();
    }
    pb.finish().ok_or_else(|| "empty path".to_string())
}

fn rounded_square(min: f32, max: f32, r: f32) -> Result<Path, String> {
    let k = r * 0.552_284_8; // cubic approximation of a quarter circle
    let mut pb = PathBuilder::new();
    pb.move_to(min + r, min);
    pb.line_to(max - r, min);
    pb.cubic_to(max - r + k, min, max, min + r - k, max, min + r);
    pb.line_to(max, max - r);
    pb.cubic_to(max, max - r + k, max - r + k, max, max - r, max);
    pb.line_to(min + r, max);
    pb.cubic_to(min + r - k, max, min, max - r + k, min, max - r);
    pb.line_to(min, min + r);
    pb.cubic_to(min, min + r - k, min + r - k, min, min + r, min);
    pb.close();
    pb.finish().ok_or_else(|| "empty path".to_string())
}

/// The icon at `size` px, as PNG bytes. Geometry is in a 1024-unit square.
pub fn render(size: u32) -> Result<Vec<u8>, String> {
    let mut pm = Pixmap::new(size, size).ok_or_else(|| format!("bad icon size {size}"))?;
    let scale = size as f32 / 1024.0;
    let ts = Transform::from_scale(scale, scale);
    // Lines thinner than a pixel vanish at 16–32 px; keep them at least ~0.9 px wide.
    let width = |w: f32| (w * scale).max(0.9) / scale;

    let mut tile = paint(rgb(0x2B3646));
    tile.shader = LinearGradient::new(
        Point::from_xy(0.0, 48.0),
        Point::from_xy(0.0, 976.0),
        vec![GradientStop::new(0.0, rgb(0x34425A)), GradientStop::new(1.0, rgb(0x161C26))],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .ok_or("tile gradient")?;
    pm.fill_path(&rounded_square(48.0, 976.0, 212.0)?, &tile, FillRule::Winding, ts, None);

    // Isometric block: top, left and right faces around the front vertex C.
    let (cx, cy, s) = (512.0f32, 444.0f32, 292.0f32);
    let dx = s * 0.866_025_4;
    let t = (cx, cy - s);
    let ul = (cx - dx, cy - s / 2.0);
    let ur = (cx + dx, cy - s / 2.0);
    let c = (cx, cy);
    let ll = (cx - dx, cy + s / 2.0);
    let lr = (cx + dx, cy + s / 2.0);
    let b = (cx, cy + s);
    for (face, color) in [(&[t, ur, c, ul][..], 0xFFC857), (&[ul, c, b, ll][..], 0xF2902B), (&[c, ur, lr, b][..], 0xC4601A)] {
        pm.fill_path(&poly(face, true)?, &paint(rgb(color)), FillRule::Winding, ts, None);
    }
    let edge = Stroke { width: width(16.0), line_join: LineJoin::Round, line_cap: LineCap::Round, ..Stroke::default() };
    let edge_paint = paint(rgb(0xFFF6E6));
    pm.stroke_path(&poly(&[t, ur, lr, b, ll, ul], true)?, &edge_paint, &edge, ts, None);
    for p in [ul, ur, b] {
        pm.stroke_path(&poly(&[c, p], false)?, &edge_paint, &edge, ts, None);
    }

    // Dimension below the block: extension lines, dimension line, filled arrowheads.
    let blue = paint(rgb(0x8EC5FF));
    let thin = Stroke { width: width(14.0), line_cap: LineCap::Butt, ..Stroke::default() };
    let (y, gap, over) = (842.0, 34.0, 36.0);
    for x in [ll.0, lr.0] {
        pm.stroke_path(&poly(&[(x, ll.1 + gap), (x, y + over)], false)?, &blue, &thin, ts, None);
    }
    let (al, aw) = (64.0, 26.0);
    pm.stroke_path(&poly(&[(ll.0 + al - 2.0, y), (lr.0 - al + 2.0, y)], false)?, &blue, &thin, ts, None);
    pm.fill_path(&poly(&[(ll.0, y), (ll.0 + al, y - aw), (ll.0 + al, y + aw)], true)?, &blue, FillRule::Winding, ts, None);
    pm.fill_path(&poly(&[(lr.0, y), (lr.0 - al, y - aw), (lr.0 - al, y + aw)], true)?, &blue, FillRule::Winding, ts, None);

    pm.encode_png().map_err(|e| format!("encode {size} px PNG: {e}"))
}

pub fn run(root: &std::path::Path) -> Result<(), String> {
    let dir = root.join("assets/app-icon");
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.join(name);
        std::fs::write(&path, bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
        println!("{}: {} bytes", path.display(), bytes.len());
        Ok::<(), String>(())
    };
    write("cadino-1024.png", &render(1024)?)?;
    write("cadino-256.png", &render(256)?)?;
    let pngs = ICO_SIZES.iter().map(|&s| render(s)).collect::<Result<Vec<_>, _>>()?;
    write("cadino.ico", &crate::ico::pack(&pngs)?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders_every_ico_size() {
        for s in super::ICO_SIZES {
            let png = super::render(s).unwrap();
            assert_eq!(crate::ico::png_size(&png).unwrap(), (s, s));
        }
    }
}
