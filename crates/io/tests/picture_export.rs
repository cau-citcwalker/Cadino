//! Raster images in SVG and PDF output: the picture goes in (a PNG data URI, an image XObject
//! with an alpha soft mask), clipped by its boundary and adjusted as it shows, in its place in
//! the draw order; a picture seen in perspective goes in as its pieces.

use cadcraft_doc::{Drawing, EntityKind, Image, Raster, Space};
use cadcraft_geom::{Camera, Vec2, Vec3, ViewDir};
use cadcraft_io::{PdfOptions, pdf, svg};

/// A 20 x 10 picture of 2 x 2 pixels: red on the left, blue on the right, the top-left pixel
/// see-through.
fn photo() -> Image {
    let rgba = vec![200, 30, 30, 0, 30, 30, 200, 255, 200, 30, 30, 255, 30, 30, 200, 255];
    Image {
        u: Vec3::new(10.0, 0.0, 0.0),
        v: Vec3::new(0.0, 5.0, 0.0),
        size: Vec2::new(2.0, 2.0),
        path: "photo.png".into(),
        display: 1 | 8,
        pixels: Raster::new(2, 2, rgba, true),
        ..Image::default()
    }
}

fn drawing(i: Image) -> Drawing {
    let mut d = Drawing::new_metric();
    d.add(&Space::Model, Default::default(), EntityKind::Image(i)).unwrap();
    d
}

/// The uncompressed PDF of model space.
fn pdf_text(d: &Drawing) -> String {
    String::from_utf8_lossy(&pdf(d, &Space::Model, &PdfOptions { compress: false, ..PdfOptions::default() }).unwrap()).into_owned()
}

#[test]
fn svg_embeds_the_picture_clipped() {
    let s = svg::export(&drawing(photo()), &Space::Model);
    assert!(s.contains("data:image/png;base64,iVBORw0KGgo"), "a PNG data URI");
    assert_eq!(s.matches("<use ").count(), 1, "{s}");
    assert!(!s.contains("clipPath"), "an unclipped picture needs no clip");
    // The picture's unit square lands on the image: 20 wide, 10 high, the top row at y = 10.
    let at = s.find("transform=\"matrix(").unwrap();
    let nums: Vec<f64> = s[at + 18..].split(')').next().unwrap().split(' ').map(|v| v.parse().unwrap()).collect();
    assert!((nums[0] - 20.0).abs() < 1e-6 && (nums[3] - 10.0).abs() < 1e-6 && nums[1].abs() < 1e-9 && nums[2].abs() < 1e-9, "{nums:?}");
    // Clipped to its left half: a clip path, the picture still whole under it.
    let mut left = photo();
    left.clip = vec![Vec2::new(-0.5, -0.5), Vec2::new(0.5, 1.5)];
    left.clipping = true;
    left.display |= 4;
    let s = svg::export(&drawing(left), &Space::Model);
    assert!(s.contains("<clipPath id=\"ic0\">") && s.contains("clip-path=\"url(#ic0)\""), "{s}");
    // Shown faded: a different PNG.
    let faded = svg::export(&drawing(Image { fade: 60, ..photo() }), &Space::Model);
    let png = |s: &str| s.split("base64,").nth(1).and_then(|r| r.split('"').next()).map(str::to_string);
    assert_ne!(png(&faded), png(&svg::export(&drawing(photo()), &Space::Model)));
}

#[test]
fn pdf_draws_the_picture_with_its_soft_mask() {
    let t = pdf_text(&drawing(photo()));
    assert!(t.contains("/Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Interpolate true /SMask"), "{t}");
    assert!(t.contains("/ColorSpace /DeviceGray"), "the see-through pixel's mask");
    assert!(t.contains("/XObject << /Im0 ") && t.contains("cm /Im0 Do"), "{t}");
    // Transparency off: the see-through pixel prints white, so no mask.
    let opaque = pdf_text(&drawing(Image { display: 1, ..photo() }));
    assert!(opaque.contains("/Subtype /Image") && !opaque.contains("/SMask"), "{opaque}");
    // IMAGEQUALITY Draft: no interpolation.
    let mut d = drawing(photo());
    d.header.set_i64("IMAGEQUALITY", 0);
    assert!(!pdf_text(&d).contains("/Interpolate"));
    // A clipped picture: a clip path before it is drawn.
    let mut left = photo();
    left.clip = vec![Vec2::new(-0.5, -0.5), Vec2::new(0.5, 1.5)];
    left.clipping = true;
    let t = pdf_text(&drawing(left));
    let draw = t.find("cm /Im0 Do").unwrap();
    assert!(t[..draw].rfind("h W n").is_some_and(|c| c > t[..draw].rfind("\nq").unwrap_or(0)), "{t}");
    // Two references to one picture (one definition's pixels) share its XObject.
    let one = photo();
    let mut two = drawing(one.clone());
    two.add(&Space::Model, Default::default(), EntityKind::Image(Image { insert: Vec3::new(30.0, 0.0, 0.0), ..one })).unwrap();
    let t = pdf_text(&two);
    assert_eq!(t.matches("/ColorSpace /DeviceRGB").count(), 1);
    assert_eq!(t.matches("cm /Im0 Do").count(), 2);
}

#[test]
fn a_picture_seen_in_perspective_goes_out_in_pieces() {
    let d = drawing(photo());
    let dir = ViewDir::named("iso").unwrap();
    let target = Vec3::new(10.0, 5.0, 0.0);
    let cam = Camera::new(50.0, dir.depth(target));
    let view_dir = dir.with_camera(Some(cam), dir.project(target), cam.height_at(40.0));
    let list = cadcraft_render::build(&d, &Space::Model, &cadcraft_render::Options { view_dir, ..Default::default() });
    assert!(list.images.first().is_some_and(|im| !im.mesh.is_empty()));
    let s = svg::export_list(&list, None);
    assert_eq!(s.matches("data:image/png").count(), 1, "the picture once");
    assert!(s.matches("<use ").count() >= 256, "one piece per triangle");
    assert_eq!(s.matches("<use ").count(), s.matches("<clipPath").count());
}
