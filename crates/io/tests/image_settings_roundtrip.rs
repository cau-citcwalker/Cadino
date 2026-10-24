//! What IMAGEATTACH and its editing commands set survives DXF save and reopen: an unloaded
//! definition (IMAGEDEF group 280), a polygonal clip boundary, brightness/contrast/fade and
//! transparency, and the drawing's IMAGEFRAME and IMAGEQUALITY (the RASTERVARIABLES object).
//! Pictures themselves are never written.

use cadcraft_doc::{Drawing, Entity, EntityKind, IMAGE_TRANSPARENT, Image, Raster};
use cadcraft_geom::{Vec2, Vec3};

fn image(name: &str, path: &str, unloaded: bool) -> Image {
    Image {
        insert: Vec3::new(1.0, 2.0, 0.0),
        u: Vec3::new(0.5, 0.0, 0.0),
        v: Vec3::new(0.0, 0.5, 0.0),
        size: Vec2::new(40.0, 20.0),
        path: path.into(),
        name: name.into(),
        clip: vec![Vec2::new(-0.5, -0.5), Vec2::new(39.5, -0.5), Vec2::new(10.0, 19.5)],
        clipping: true,
        display: 7 | IMAGE_TRANSPARENT,
        brightness: 70,
        contrast: 20,
        fade: 30,
        unloaded,
        pixels: Raster::new(1, 1, vec![1, 2, 3, 4], false),
        ..Image::default()
    }
}

fn images(d: &Drawing) -> Vec<Image> {
    d.model
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Image(i) => Some(i.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn image_settings_round_trip() {
    let mut d = Drawing::new_metric();
    for i in [image("site", "photos\\site.jpg", false), image("logo", "logo.png", true)] {
        let h = d.new_handle();
        d.model.push(Entity::new(h, EntityKind::Image(i)));
    }
    d.header.set_i64("IMAGEFRAME", 2);
    d.header.set_i64("IMAGEQUALITY", 0);
    let text = String::from_utf8(cadcraft_io::write(&d, "x.dxf").unwrap()).unwrap();
    let back = cadcraft_io::read(text.as_bytes(), "x.dxf").unwrap();
    let (a, b) = (images(&d), images(&back));
    assert_eq!(b.len(), 2);
    for (want, got) in a.iter().zip(&b) {
        assert!(got.pixels.is_none(), "pictures are never saved");
        assert_eq!(&Image { pixels: None, ..want.clone() }, got);
    }
    assert_eq!((back.header.i64("IMAGEFRAME", 1), back.header.i64("IMAGEQUALITY", 1)), (2, 0));
    // The defaults are written as such and read as nothing set.
    d.header.set_i64("IMAGEFRAME", 1);
    d.header.set_i64("IMAGEQUALITY", 1);
    let back = cadcraft_io::read(&cadcraft_io::write(&d, "x.dxf").unwrap(), "x.dxf").unwrap();
    assert!(back.header.get("IMAGEFRAME").is_none() && back.header.get("IMAGEQUALITY").is_none());
}

#[test]
fn hostile_raster_variables_and_definitions_are_ignored() {
    let mut d = Drawing::new_metric();
    let h = d.new_handle();
    d.model.push(Entity::new(h, EntityKind::Image(image("site", "site.jpg", false))));
    let text = String::from_utf8(cadcraft_io::write(&d, "x.dxf").unwrap()).unwrap().replace("\r\n", "\n");
    // A frame setting out of range and a loaded flag that isn't a number.
    let bad = text.replacen("AcDbRasterVariables\n 90\n0\n 70\n1\n", "AcDbRasterVariables\n 90\n0\n 70\n99\n", 1).replacen(
        "280\n1\n281\n0\n",
        "280\nyes\n281\n0\n",
        1,
    );
    assert_eq!(bad.len(), text.len() + 3, "both groups were changed");
    let back = cadcraft_io::read(bad.as_bytes(), "x.dxf").unwrap();
    assert!(back.header.get("IMAGEFRAME").is_none());
    assert_eq!(images(&back).len(), 1);
}
