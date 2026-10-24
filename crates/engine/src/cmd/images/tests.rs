use super::*;

fn encoded(w: u32, h: u32, format: image::ImageFormat) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 40) as u8, (y * 60) as u8, 128, if x == 0 { 0 } else { 255 }]));
    let mut out = std::io::Cursor::new(Vec::new());
    match format {
        // JPEG has no alpha.
        image::ImageFormat::Jpeg => image::DynamicImage::ImageRgba8(img).into_rgb8().write_to(&mut out, format).unwrap(),
        _ => img.write_to(&mut out, format).unwrap(),
    }
    out.into_inner()
}

#[test]
fn png_jpeg_and_bmp_decode() {
    for f in [image::ImageFormat::Png, image::ImageFormat::Jpeg, image::ImageFormat::Bmp] {
        let (r, size) = decode(&encoded(6, 4, f)).unwrap_or_else(|e| panic!("{f:?}: {e}"));
        assert_eq!(size, (6, 4), "{f:?}");
        assert_eq!((r.width, r.height, r.rgba.len()), (6, 4, 96), "{f:?}");
    }
    let (r, _) = decode(&encoded(6, 4, image::ImageFormat::Png)).unwrap();
    assert!(r.alpha && r.rgba[3] == 0 && r.rgba[7] == 255, "PNG alpha is kept");
}

#[test]
fn large_pictures_are_kept_smaller_with_their_size() {
    let (r, size) = decode(&encoded(KEEP_SIDE + 100, 3, image::ImageFormat::Png)).unwrap();
    assert_eq!(size, (KEEP_SIDE + 100, 3));
    assert!(r.width <= KEEP_SIDE && r.height >= 1);
}

/// A PNG whose header claims `w` × `h` pixels, with a valid CRC and no data.
fn png_claiming(w: u32, h: u32) -> Vec<u8> {
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    let crc = {
        let mut c = !0u32;
        for b in &ihdr {
            c ^= u32::from(*b);
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    };
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend(13u32.to_be_bytes());
    v.extend(ihdr);
    v.extend(crc.to_be_bytes());
    v
}

#[test]
fn hostile_image_files_never_panic() {
    let good = encoded(5, 5, image::ImageFormat::Png);
    let bmp = encoded(5, 5, image::ImageFormat::Bmp);
    let mut huge_bmp = bmp.clone();
    // Width and height fields of the BMP info header.
    huge_bmp[18..22].copy_from_slice(&i32::MAX.to_le_bytes());
    huge_bmp[22..26].copy_from_slice(&(-i32::MAX).to_le_bytes());
    let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00;".to_vec();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("junk", vec![0xAB; 1000]),
        ("png signature only", b"\x89PNG\r\n\x1a\n".to_vec()),
        ("truncated png", good[..good.len() / 2].to_vec()),
        ("truncated bmp", bmp[..40].to_vec()),
        ("huge png", png_claiming(100_000, 100_000)),
        ("wide png", png_claiming(u32::MAX, 1)),
        ("zero png", png_claiming(0, 0)),
        ("huge bmp", huge_bmp),
        ("jpeg header only", vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0]),
        ("gif", gif),
    ];
    for (what, bytes) in cases {
        assert!(decode(&bytes).is_err(), "{what} must fail");
    }
    // Every prefix of a valid file fails or decodes; none panics.
    for n in (0..good.len()).step_by(7) {
        let _ = decode(&good[..n]);
    }
}

#[test]
fn names_paths_and_stems() {
    assert_eq!(extension("C:\\pics\\Site.JPG"), "jpg");
    assert_eq!(extension("noext"), "");
    assert_eq!(stem("a/b/kitchen.photo.png"), "kitchen.photo");
    assert_eq!(def_name(&Image { path: "x/plan.png".into(), ..Image::default() }), "plan");
    assert_eq!(def_name(&Image { path: "x/plan.png".into(), name: "Site".into(), ..Image::default() }), "Site");
}

#[test]
fn one_file_under_several_spellings_is_decoded_once_within_a_budget() {
    use cadcraft_doc::{Common, Space};
    let dir = std::env::temp_dir().join(format!("cadcraft-images-budget-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("photo.png"), encoded(8, 8, image::ImageFormat::Png)).unwrap();
    std::fs::write(dir.join("other.png"), encoded(8, 8, image::ImageFormat::Png)).unwrap();
    let host = dir.join("plan.dxf").to_string_lossy().into_owned();
    let abs = dir.join("photo.png").to_string_lossy().into_owned();
    let mut spellings = vec!["photo.png".to_string(), "./photo.png".into(), r".\photo.png".into(), abs];
    if cfg!(windows) {
        spellings.push("PHOTO.PNG".into());
    }
    let mut d = Drawing::new_metric();
    for p in &spellings {
        d.add(&Space::Model, Common::default(), EntityKind::Image(Image { path: p.clone(), ..Image::default() })).unwrap();
    }
    assert!(load_pictures(&mut d, Some(&host), None).is_empty());
    let pics: Vec<Arc<Raster>> = all_images(&d).map(|i| i.pixels.clone().unwrap()).collect();
    assert_eq!(pics.len(), spellings.len());
    assert!(pics.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])), "decoded once");
    // Past the budget, further files wait (their frames show) with a note.
    let mut d = Drawing::new_metric();
    for p in ["photo.png", "other.png"] {
        d.add(&Space::Model, Common::default(), EntityKind::Image(Image { path: p.into(), ..Image::default() })).unwrap();
    }
    let notes = load_within(&mut d, Some(&host), None, 1);
    assert_eq!(all_images(&d).filter(|i| i.pixels.is_some()).count(), 1);
    assert!(notes.iter().any(|n| n.starts_with("1 image file(s) not read")), "{notes:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
