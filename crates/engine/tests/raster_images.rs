//! Raster image references (IMAGEATTACH, ATTACH, -IMAGE, IMAGECLIP, IMAGEADJUST, IMAGEFRAME,
//! IMAGEQUALITY, TRANSPARENCY), ETRANSMIT and PARTIALOPEN on real files in a temporary folder:
//! pictures drawn by the CPU raster, DXF round trips with relative paths, missing files, and
//! hostile pictures.

use std::path::{Path, PathBuf};

use cadcraft_engine::Session;
use cadcraft_engine::cmd::file::{IoHooks, set_io};
use cadcraft_engine::doc::geom::{Vec2, Vec3};
use cadcraft_engine::doc::{Drawing, Entity, EntityKind, Handle, Image, Layer, Line, Space};
use cadcraft_engine::render::raster::{RasterOptions, View};
use serde_json::json;

fn hooks() {
    set_io(IoHooks {
        read: |b, name| cadcraft_io::read(b, name).map_err(|e| e.to_string()),
        write: |d, name| cadcraft_io::write(d, name).map_err(|e| e.to_string()),
        plot: None,
    });
}

/// A fresh folder for one test.
fn folder(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cadcraft-images-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A 20 x 10 picture: red left half, blue right half; the top-left pixel see-through.
fn photo(path: &Path) {
    let img = image::RgbaImage::from_fn(20, 10, |x, y| {
        if x == 0 && y == 0 {
            image::Rgba([0, 0, 0, 0])
        } else if x < 10 {
            image::Rgba([220, 20, 20, 255])
        } else {
            image::Rgba([20, 20, 220, 255])
        }
    });
    img.save(path).unwrap();
}

fn save(d: &Drawing, path: &Path) {
    std::fs::write(path, cadcraft_io::write(d, &path.to_string_lossy()).unwrap()).unwrap();
}

fn open(s: &mut Session, path: &Path) {
    s.execute("open", &json!({ "path": path.to_string_lossy() })).unwrap();
}

/// A floor plan: a wall line on layer Walls and a table on layer Furniture.
fn plan() -> Drawing {
    let mut d = Drawing::new_metric();
    for name in ["Walls", "Furniture"] {
        d.layers.push(Layer { name: name.into(), ..Layer::default() });
    }
    let mut add = |layer: &str, a: (f64, f64), b: (f64, f64)| {
        let h = d.new_handle();
        let mut e = Entity::new(h, EntityKind::Line(Line { a: Vec3::new(a.0, a.1, 0.0), b: Vec3::new(b.0, b.1, 0.0) }));
        e.common.layer = layer.into();
        d.model.push(e);
    };
    add("Walls", (0.0, 0.0), (20.0, 0.0));
    add("Furniture", (100.0, 100.0), (110.0, 100.0));
    d
}

fn images(s: &Session) -> Vec<Image> {
    s.doc()
        .unwrap()
        .model
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Image(i) => Some(i.clone()),
            _ => None,
        })
        .collect()
}

/// The model space drawn on a white 200 x 100 image of the area 0,0 – 20,10.
fn shot(s: &Session) -> tiny_skia_pixels::Pixels {
    let list = cadcraft_engine::render::build(s.doc().unwrap(), &Space::Model, &cadcraft_engine::render::Options::default());
    let view = View { center: Vec2::new(10.0, 5.0), scale: 10.0, width: 200, height: 100 };
    let pm = cadcraft_engine::render::raster::render(
        &list,
        &view,
        &RasterOptions { background: cadcraft_engine::doc::color::Rgb(255, 255, 255), ..RasterOptions::default() },
    )
    .unwrap();
    tiny_skia_pixels::Pixels { w: 200, data: pm.data().to_vec() }
}

mod tiny_skia_pixels {
    pub struct Pixels {
        pub w: usize,
        pub data: Vec<u8>,
    }
    impl Pixels {
        /// The (premultiplied) colour at pixel `x`, `y`.
        pub fn at(&self, x: usize, y: usize) -> [u8; 3] {
            let o = (y * self.w + x) * 4;
            [self.data[o], self.data[o + 1], self.data[o + 2]]
        }
    }
}

fn reddish(c: [u8; 3]) -> bool {
    c[0] > 150 && c[2] < 100
}
fn bluish(c: [u8; 3]) -> bool {
    c[2] > 150 && c[0] < 100
}

#[test]
fn attach_draws_the_picture_and_round_trips_through_dxf() {
    hooks();
    let dir = folder("attach");
    photo(&dir.join("kitchen.png"));
    let host = dir.join("plan.dxf");
    save(&plan(), &host);
    let mut s = Session::new();
    open(&mut s, &host);

    // 20 units wide at 0,0: one unit per pixel, saved relative to the drawing's folder.
    let r = s.execute("imageattach", &json!({ "path": dir.join("kitchen.png").to_string_lossy(), "scale": 20 })).unwrap();
    assert_eq!(
        (r["name"].as_str(), r["path"].as_str(), r["width"].as_u64(), r["height"].as_u64()),
        (Some("kitchen"), Some("kitchen.png"), Some(20), Some(10))
    );
    let i = &images(&s)[0];
    assert!(i.pixels.is_some() && (i.u.x - 1.0).abs() < 1e-12 && i.corners()[2].near(Vec2::new(20.0, 10.0), 1e-9));
    let px = shot(&s);
    assert!(reddish(px.at(50, 50)) && bluish(px.at(150, 50)), "{:?} {:?}", px.at(50, 50), px.at(150, 50));
    // TRANSPARENCY off: the see-through pixel is white; on: what is under it (white paper here too).
    assert_eq!(px.at(3, 3), [255, 255, 255]);

    // Saved and reopened: the path, the definition and the picture again.
    s.execute("qsave", &json!({})).unwrap();
    let text = std::fs::read_to_string(&host).unwrap();
    assert!(text.contains("IMAGEDEF") && text.contains("ACAD_IMAGE_DICT") && text.contains("IMAGEDEF_REACTOR") && text.contains("kitchen.png"));
    let mut t = Session::new();
    open(&mut t, &host);
    let i = &images(&t)[0];
    assert_eq!((i.name.as_str(), i.path.as_str()), ("kitchen", "kitchen.png"));
    assert!(i.pixels.is_some(), "the picture is read again, relative to the drawing");
    assert!(!t.state().unwrap().is_dirty(), "reading pictures isn't a change");
    assert!(reddish(shot(&t).at(50, 50)));

    // The same file attached again shares its definition.
    let r = t.execute("imageattach", &json!({ "path": "kitchen.png", "at": [30, 0] })).unwrap();
    assert_eq!(r["name"], "kitchen");
    let defs = t.execute("image", &json!({})).unwrap();
    assert_eq!(defs["images"][0]["references"], 2);
    assert_eq!(defs["images"][0]["status"], "Loaded");
}

#[test]
fn a_missing_file_shows_the_frame_and_its_name() {
    hooks();
    let dir = folder("missing");
    photo(&dir.join("site.png"));
    let host = dir.join("plan.dxf");
    save(&plan(), &host);
    let mut s = Session::new();
    open(&mut s, &host);
    s.execute("imageattach", &json!({ "path": "site.png", "scale": 20 })).unwrap();
    s.execute("qsave", &json!({})).unwrap();
    std::fs::remove_file(dir.join("site.png")).unwrap();

    let mut t = Session::new();
    open(&mut t, &host);
    assert!(t.log.iter().any(|l| l.contains("site.png") && l.contains("not found")), "{:?}", t.log);
    let i = &images(&t)[0];
    assert!(i.pixels.is_none());
    let defs = t.execute("image", &json!({})).unwrap();
    assert_eq!(defs["images"][0]["status"], "Not Found");
    let list = cadcraft_engine::render::build(t.doc().unwrap(), &Space::Model, &cadcraft_engine::render::Options::default());
    assert!(list.images.is_empty());
    // The frame, and the name's strokes inside it.
    assert!(list.prims.len() > 2, "frame and file name: {}", list.prims.len());
    // Without the file, ATTACH says why.
    let e = t.execute("imageattach", &json!({ "path": "gone.png" })).unwrap_err().to_string();
    assert!(e.contains("not found"), "{e}");
}

#[test]
fn hostile_pictures_are_refused_without_harm() {
    hooks();
    let dir = folder("hostile");
    std::fs::write(dir.join("junk.png"), b"\x89PNG\r\n\x1a\nnot really").unwrap();
    std::fs::write(dir.join("empty.jpg"), b"").unwrap();
    std::fs::write(dir.join("text.bmp"), b"BMhello").unwrap();
    let mut s = Session::new();
    for f in ["junk.png", "empty.jpg", "text.bmp"] {
        let r = s.execute("imageattach", &json!({ "path": dir.join(f).to_string_lossy() }));
        assert!(r.is_err(), "{f}");
    }
    assert!(images(&s).is_empty());
    for p in [
        json!({}),
        json!({ "path": 5 }),
        json!({ "path": "x.png", "scale": -1 }),
        json!({ "path": "x.png", "scale": "big" }),
        json!({ "path": "x.png", "rotation": f64::MAX }),
    ] {
        assert!(s.execute("imageattach", &p).is_err(), "{p}");
    }
    // Underlays aren't available yet; drawings go to XATTACH.
    let e = s.execute("attach", &json!({ "path": "plan.pdf" })).unwrap_err().to_string();
    assert!(e.contains("not available yet"), "{e}");
    let e = s.execute("attach", &json!({ "path": dir.join("nothing.dwg").to_string_lossy() })).unwrap_err().to_string();
    assert!(!e.contains("not available"), "{e}");
}

#[test]
fn file_access_denied_keeps_pictures_unread() {
    hooks();
    let dir = folder("deny");
    photo(&dir.join("a.png"));
    let mut s = Session::new();
    let _deny = cadcraft_engine::limits::deny_files("files are off");
    let e = s.execute("imageattach", &json!({ "path": dir.join("a.png").to_string_lossy() })).unwrap_err().to_string();
    assert!(e.contains("files are off"), "{e}");
    let e = s.execute("etransmit", &json!({})).unwrap_err().to_string();
    assert!(e.contains("files are off"), "{e}");
}

#[test]
fn clip_adjust_frame_quality_and_transparency() {
    hooks();
    let dir = folder("edit");
    photo(&dir.join("p.png"));
    let mut s = Session::new();
    let r = s.execute("imageattach", &json!({ "path": dir.join("p.png").to_string_lossy(), "scale": 20 })).unwrap();
    let h = r["handle"].as_str().unwrap().to_string();
    let hs = json!([h]);

    // Clip to the left half: the blue half is gone, the frame follows the boundary.
    s.execute("imageclip", &json!({ "handles": hs, "points": [[0, 0], [10, 10]] })).unwrap();
    let i = &images(&s)[0];
    assert!(i.clipping && i.clip.len() == 2);
    let px = shot(&s);
    assert!(reddish(px.at(50, 50)) && px.at(150, 50) == [255, 255, 255], "{:?}", px.at(150, 50));
    s.execute("imageclip", &json!({ "handles": hs, "option": "off" })).unwrap();
    assert!(bluish(shot(&s).at(150, 50)));
    s.execute("imageclip", &json!({ "handles": hs, "option": "on" })).unwrap();
    // A polygon: the lower-left triangle.
    s.execute("imageclip", &json!({ "handles": hs, "points": [[0, 0], [20, 0], [0, 10]] })).unwrap();
    let px = shot(&s);
    assert!(reddish(px.at(20, 80)) && px.at(180, 10) == [255, 255, 255]);
    s.execute("imageclip", &json!({ "handles": hs, "option": "delete" })).unwrap();
    assert!(!images(&s)[0].clipping);
    assert!(s.execute("imageclip", &json!({ "handles": hs, "option": "on" })).is_err(), "no boundary left");
    assert!(s.execute("imageclip", &json!({ "handles": hs, "points": [[0, 0], [0, 10]] })).is_err(), "no area");

    // Brightness 100 washes it out; fade 100 lets the paper show.
    s.execute("imageadjust", &json!({ "handles": hs, "brightness": 100 })).unwrap();
    assert_eq!(shot(&s).at(50, 50), [255, 255, 255]);
    s.execute("imageadjust", &json!({ "handles": hs, "brightness": 50, "fade": 100 })).unwrap();
    assert_eq!(shot(&s).at(50, 50), [255, 255, 255]);
    s.execute("imageadjust", &json!({ "handles": hs, "fade": 0, "contrast": 0 })).unwrap();
    assert_eq!(shot(&s).at(50, 50), [128, 128, 128]);
    for bad in [
        json!({ "handles": hs }),
        json!({ "handles": hs, "fade": 101 }),
        json!({ "handles": hs, "contrast": "x" }),
        json!({ "handles": [], "fade": 1 }),
    ] {
        assert!(s.execute("imageadjust", &bad).is_err(), "{bad}");
    }

    // Frames: 0 none (and plots none), 2 on screen only.
    let frames = |s: &Session, plot: bool| {
        let o = cadcraft_engine::render::Options::default();
        let d = s.doc().unwrap();
        let l = if plot { cadcraft_engine::render::build_plot(d, &Space::Model, &o) } else { cadcraft_engine::render::build(d, &Space::Model, &o) };
        l.prims.len()
    };
    assert_eq!(frames(&s, false), 1);
    s.execute("imageframe", &json!({ "value": 0 })).unwrap();
    assert_eq!(frames(&s, false), 0);
    s.execute("imageframe", &json!({ "value": 2 })).unwrap();
    assert_eq!((frames(&s, false), frames(&s, true)), (1, 0));
    assert!(s.execute("imageframe", &json!({ "value": 3 })).is_err());
    s.execute("imagequality", &json!({ "quality": "draft" })).unwrap();
    assert_eq!(s.doc().unwrap().header.i64("IMAGEQUALITY", 1), 0);

    s.execute("transparency", &json!({ "handles": hs, "on": true })).unwrap();
    assert!(images(&s)[0].transparent());
    s.execute("transparency", &json!({ "handles": hs, "on": false })).unwrap();
    assert!(!images(&s)[0].transparent());

    // The settings survive a DXF round trip.
    let path = dir.join("edit.dxf");
    s.execute("saveas", &json!({ "path": path.to_string_lossy() })).unwrap();
    let mut t = Session::new();
    open(&mut t, &path);
    let d = t.doc().unwrap();
    assert_eq!((d.header.i64("IMAGEFRAME", 1), d.header.i64("IMAGEQUALITY", 1)), (2, 0));
    assert_eq!(images(&t)[0].contrast, 0);
}

#[test]
fn grips_pick_and_extents_follow_the_frame() {
    hooks();
    let dir = folder("grips");
    photo(&dir.join("p.png"));
    let mut s = Session::new();
    let r = s.execute("imageattach", &json!({ "path": dir.join("p.png").to_string_lossy(), "scale": 20, "rotation": 90, "at": [5, 5] })).unwrap();
    let h = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
    let d = s.doc().unwrap();
    let e = d.entity(h).unwrap();
    let ext = d.extents(&Space::Model);
    assert!(ext.min.near(Vec2::new(-5.0, 5.0), 1e-9) && ext.max.near(Vec2::new(5.0, 25.0), 1e-9), "{ext:?}");
    // Picked on its frame, not in the middle of the picture.
    assert_eq!(cadcraft_engine::select::pick(d, &Space::Model, Vec2::new(5.0, 15.0), 0.1), Some(h));
    assert_eq!(cadcraft_engine::select::pick(d, &Space::Model, Vec2::new(0.0, 15.0), 0.1), None);
    // Its corner grips scale it about the opposite corner; the insertion grip moves it.
    let grips = e.kind.grips();
    assert_eq!(grips.len(), 4);
    let big = cadcraft_engine::grips::stretch_grip(&e.kind, 2, Vec2::new(-15.0, 45.0)).unwrap();
    let EntityKind::Image(b) = &big else { panic!() };
    assert!(b.insert.xy().near(Vec2::new(5.0, 5.0), 1e-9) && b.corners()[2].near(Vec2::new(-15.0, 45.0), 1e-9));
    let moved = cadcraft_engine::grips::stretch_grip(&e.kind, 0, Vec2::new(6.0, 5.0)).unwrap();
    let EntityKind::Image(m) = &moved else { panic!() };
    assert!(m.insert.xy().near(Vec2::new(6.0, 5.0), 1e-9) && (m.u.len() - b.u.len() / 2.0).abs() < 1e-9);
}

#[test]
fn dash_image_lists_unloads_reloads_repaths_and_detaches() {
    hooks();
    let dir = folder("dash");
    photo(&dir.join("a.png"));
    std::fs::create_dir_all(dir.join("moved")).unwrap();
    photo(&dir.join("moved").join("a.png"));
    let host = dir.join("plan.dxf");
    save(&plan(), &host);
    let mut s = Session::new();
    open(&mut s, &host);
    s.execute("imageattach", &json!({ "path": "a.png" })).unwrap();

    let r = s.execute("-image", &json!({ "option": "unload", "names": "A" })).unwrap();
    assert_eq!(r["unloaded"], json!(["a"]));
    assert!(images(&s)[0].unloaded && images(&s)[0].pixels.is_none());
    assert_eq!(s.execute("image", &json!({})).unwrap()["images"][0]["status"], "Unloaded");
    s.execute("-image", &json!({ "option": "reload" })).unwrap();
    assert!(!images(&s)[0].unloaded && images(&s)[0].pixels.is_some());
    s.execute("-image", &json!({ "option": "path", "names": "a", "path": "moved/a.png" })).unwrap();
    assert_eq!(images(&s)[0].path, "moved/a.png");
    assert!(images(&s)[0].pixels.is_some());
    assert!(s.execute("-image", &json!({ "option": "unload", "names": "zz*" })).is_err());
    assert!(s.execute("-image", &json!({ "option": "detach" })).is_err(), "detach names its images");

    // Typed: ? lists; Detach removes every reference.
    s.cmdline("-image").unwrap();
    s.cmdline("?").unwrap();
    s.cmdline("").unwrap();
    assert!(s.log.iter().any(|l| l.starts_with("Total Image Definition(s): 1")), "{:?}", s.log);
    s.cmdline("-image").unwrap();
    s.cmdline("d").unwrap();
    s.cmdline("a").unwrap();
    assert!(images(&s).is_empty());
    s.cmdline("u").unwrap();
    assert_eq!(images(&s).len(), 1, "UNDO brings it back");

    // Typed attach: file, insertion point, scale, rotation.
    s.cmdline("-image").unwrap();
    s.cmdline("a").unwrap();
    s.cmdline(&format!("\"{}\"", dir.join("a.png").to_string_lossy())).unwrap();
    s.cmdline("10,0").unwrap();
    s.cmdline("40").unwrap();
    s.cmdline("0").unwrap();
    let placed: Vec<Image> = images(&s).into_iter().filter(|i| i.insert.x == 10.0).collect();
    assert_eq!(placed.len(), 1);
    assert!((placed[0].u.x * 20.0 - 40.0).abs() < 1e-9);
}

#[test]
fn typed_imageattach_clip_adjust_and_settings() {
    hooks();
    let dir = folder("typed");
    photo(&dir.join("p.png"));
    let mut s = Session::new();
    s.cmdline("imageattach").unwrap();
    let prompt = s.current_prompt().unwrap();
    assert!(matches!(prompt.picker, Some(cadcraft_engine::Picker::File { save: false, .. })), "FILEDIA's file dialog");
    s.cmdline(&dir.join("p.png").to_string_lossy()).unwrap();
    s.cmdline("").unwrap();
    s.cmdline("20").unwrap();
    s.cmdline("").unwrap();
    assert_eq!(images(&s).len(), 1);

    s.cmdline("imageclip").unwrap();
    s.cmdline("0,5").unwrap();
    s.cmdline("").unwrap();
    s.cmdline("").unwrap();
    s.cmdline("0,0").unwrap();
    s.cmdline("10,10").unwrap();
    assert!(images(&s)[0].clipping);

    s.cmdline("imageadjust").unwrap();
    s.cmdline("0,5").unwrap();
    s.cmdline("").unwrap();
    s.cmdline("c").unwrap();
    s.cmdline("70").unwrap();
    assert_eq!(images(&s)[0].contrast, 70);

    s.cmdline("transparency").unwrap();
    s.cmdline("0,5").unwrap();
    s.cmdline("").unwrap();
    s.cmdline("on").unwrap();
    assert!(images(&s)[0].transparent());

    s.cmdline("imageframe").unwrap();
    s.cmdline("0").unwrap();
    s.cmdline("imagequality").unwrap();
    s.cmdline("d").unwrap();
    let d = s.doc().unwrap();
    assert_eq!((d.header.i64("IMAGEFRAME", 1), d.header.i64("IMAGEQUALITY", 1)), (0, 0));
    // A PDF is refused at the file prompt.
    s.cmdline("attach").unwrap();
    assert!(s.cmdline("plan.pdf").is_err() || s.log.iter().any(|l| l.contains("not available yet")));
}

/// The entries of a stored ZIP, by name.
fn unzip(z: &[u8]) -> Vec<(String, Vec<u8>)> {
    let u16_at = |o: usize| u16::from_le_bytes([z[o], z[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes([z[o], z[o + 1], z[o + 2], z[o + 3]]) as usize;
    let end = z.len() - 22;
    let (n, mut at) = (u16_at(end + 10), u32_at(end + 16));
    let mut out = Vec::new();
    for _ in 0..n {
        let (size, nlen, local) = (u32_at(at + 24), u16_at(at + 28), u32_at(at + 42));
        let name = String::from_utf8(z[at + 46..at + 46 + nlen].to_vec()).unwrap();
        out.push((name, z[local + 30 + nlen..local + 30 + nlen + size].to_vec()));
        at += 46 + nlen;
    }
    out
}

#[test]
fn etransmit_packs_the_drawing_with_its_files_and_a_report() {
    hooks();
    let dir = folder("etransmit");
    std::fs::create_dir_all(dir.join("pics")).unwrap();
    photo(&dir.join("pics").join("site.png"));
    let mut d = plan();
    d.text_styles.push(cadcraft_engine::doc::TextStyle {
        name: "Notes".into(),
        font: "handwriting.shx".into(),
        ..cadcraft_engine::doc::TextStyle::default()
    });
    let host = dir.join("plan.dxf");
    save(&d, &host);
    let mut s = Session::new();
    open(&mut s, &host);
    s.execute("imageattach", &json!({ "path": "pics/site.png" })).unwrap();
    s.execute("qsave", &json!({})).unwrap();

    let r = s.execute("etransmit", &json!({ "reportOnly": true })).unwrap();
    assert!(r["report"].as_str().unwrap().contains("handwriting.shx"), "missing files are reported");
    let r = s.execute("etransmit", &json!({})).unwrap();
    let zip = dir.join("plan.zip");
    assert_eq!(r["path"].as_str(), Some(zip.to_string_lossy().as_ref()));
    let entries = unzip(&std::fs::read(&zip).unwrap());
    let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["plan.dxf", "pics/site.png", "plan - Transmittal Report.txt"]);
    assert_eq!(entries[1].1, std::fs::read(dir.join("pics").join("site.png")).unwrap());
    let report = String::from_utf8(entries[2].1.clone()).unwrap();
    assert!(report.contains("pics/site.png (image)") && report.contains("handwriting.shx (font)"), "{report}");

    // Unpacked elsewhere, the drawing finds its picture.
    let out = folder("etransmit-out");
    for (n, b) in &entries {
        let p = out.join(n);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b).unwrap();
    }
    let mut t = Session::new();
    open(&mut t, &out.join("plan.dxf"));
    assert!(images(&t)[0].pixels.is_some());

    // Without a folder of its own, the package comes back as data.
    let mut u = Session::new();
    let r = u.execute("etransmit", &json!({})).unwrap();
    assert!(r["data"].as_str().is_some_and(|d| !d.is_empty()));
}

#[test]
fn partialopen_loads_chosen_layers_and_partiaload_more() {
    hooks();
    let dir = folder("partial");
    let mut d = plan();
    d.views.push(cadcraft_engine::doc::NamedView {
        name: "Corner".into(),
        center: Vec2::new(105.0, 100.0),
        height: 10.0,
        width: 20.0,
        ..Default::default()
    });
    let host = dir.join("plan.dxf");
    save(&d, &host);
    let mut s = Session::new();
    let r = s.execute("partialopen", &json!({ "path": host.to_string_lossy(), "layers": ["walls"] })).unwrap();
    assert_eq!((r["loaded"].as_u64(), r["notLoaded"].as_u64()), (Some(1), Some(1)));
    let doc = s.doc().unwrap();
    assert_eq!(doc.header.i64("FULLOPEN", 1), 0);
    assert!(!s.state().unwrap().is_dirty());
    let list = cadcraft_engine::render::build(doc, &Space::Model, &cadcraft_engine::render::Options::default());
    assert_eq!(list.prims.len(), 1, "only the wall is drawn");
    // Saving keeps what wasn't loaded.
    s.execute("qsave", &json!({})).unwrap();
    assert_eq!(cadcraft_io::read(&std::fs::read(&host).unwrap(), "plan.dxf").unwrap().model.len(), 2);

    let r = s.execute("partiaload", &json!({ "view": "corner" })).unwrap();
    assert_eq!(r["notLoaded"].as_u64(), Some(0));
    assert_eq!(s.doc().unwrap().header.i64("FULLOPEN", 0), 1);
    assert!(s.execute("partiaload", &json!({})).is_err(), "fully open now");

    // A view that isn't there leaves nothing half open.
    let n = s.docs.len();
    assert!(s.execute("partialopen", &json!({ "path": host.to_string_lossy(), "view": "nope" })).is_err());
    assert_eq!(s.docs.len(), n);

    // Typed: file, view, layers (Enter ends), xrefs.
    s.cmdline("-partialopen").unwrap();
    s.cmdline(&format!("\"{}\"", host.to_string_lossy())).unwrap();
    s.cmdline("").unwrap();
    s.cmdline("Furn*").unwrap();
    s.cmdline("").unwrap();
    s.cmdline("n").unwrap();
    let doc = s.doc().unwrap();
    assert_eq!(doc.hidden_objects.len(), 1);
    assert!(s.log.iter().any(|l| l == "Loaded 1 object(s); 1 not loaded."), "{:?}", s.log);
}

/// A 30 x 30 blue picture.
fn blue(path: &Path) {
    image::RgbaImage::from_pixel(30, 30, image::Rgba([20, 20, 220, 255])).save(path).unwrap();
}

/// The colour of an image's picture a quarter of the way across its middle row (red in
/// [`photo`], whose right half is blue).
fn middle(i: &Image) -> [u8; 3] {
    let r = i.pixels.as_ref().unwrap();
    let o = ((r.height / 2 * r.width + r.width / 4) * 4) as usize;
    [r.rgba[o], r.rgba[o + 1], r.rgba[o + 2]]
}

#[test]
fn etransmit_keeps_same_named_files_apart_and_packs_one_file_once() {
    hooks();
    let dir = folder("etransmit-names");
    for sub in ["siteA", "siteB"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    photo(&dir.join("siteA").join("photo.png"));
    blue(&dir.join("siteB").join("photo.png"));
    let host = dir.join("plan.dxf");
    save(&plan(), &host);
    let mut s = Session::new();
    open(&mut s, &host);
    let abs = |sub: &str| dir.join(sub).join("photo.png").to_string_lossy().into_owned();
    s.execute("imageattach", &json!({ "path": abs("siteA"), "name": "red", "pathType": "full" })).unwrap();
    s.execute("imageattach", &json!({ "path": abs("siteB"), "name": "blue", "pathType": "full", "at": [40, 0] })).unwrap();
    // The red picture again, by a relative path: the same file.
    s.execute("imageattach", &json!({ "path": "siteA/photo.png", "name": "red again", "at": [80, 0] })).unwrap();
    s.execute("qsave", &json!({})).unwrap();
    let r = s.execute("etransmit", &json!({})).unwrap();
    let entries = unzip(&std::fs::read(r["path"].as_str().unwrap()).unwrap());
    let pictures = entries.iter().filter(|(n, _)| n.ends_with(".png")).count();
    assert_eq!(pictures, 2, "two files, one of them under two spellings: {:?}", entries.iter().map(|e| &e.0).collect::<Vec<_>>());
    // Unpacked elsewhere (the original folders gone), each image shows its own picture.
    let out = folder("etransmit-names-out");
    for (n, b) in &entries {
        let p = out.join(n);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b).unwrap();
    }
    std::fs::remove_dir_all(&dir).unwrap();
    let mut t = Session::new();
    open(&mut t, &out.join("plan.dxf"));
    let by = |n: &str| images(&t).into_iter().find(|i| i.name == n).unwrap();
    assert!(reddish(middle(&by("red"))) && reddish(middle(&by("red again"))));
    assert!(bluish(middle(&by("blue"))));
    // A new drawing's fonts are the app's own: nothing is reported missing.
    let mut u = Session::new();
    let r = u.execute("etransmit", &json!({ "reportOnly": true })).unwrap();
    assert_eq!(r["missing"], json!([]), "{}", r["report"]);
}

#[test]
fn image_names_with_commas_and_hashes_are_named_exactly() {
    hooks();
    let dir = folder("names");
    photo(&dir.join("a.png"));
    let host = dir.join("plan.dxf");
    save(&plan(), &host);
    let mut s = Session::new();
    open(&mut s, &host);
    for (i, n) in ["North", "Elevation, North", "Plan #2"].into_iter().enumerate() {
        s.execute("imageattach", &json!({ "path": "a.png", "name": n, "at": [i as f64 * 30.0, 0] })).unwrap();
    }
    // What the External References palette sends: an array names the image exactly.
    let r = s.execute("-image", &json!({ "option": "detach", "names": ["Elevation, North"] })).unwrap();
    assert_eq!(r["detached"], json!(["Elevation, North"]));
    let mut left: Vec<String> = images(&s).into_iter().map(|i| i.name).collect();
    left.sort();
    assert_eq!(left, ["North", "Plan #2"]);
    let r = s.execute("-image", &json!({ "option": "unload", "names": ["Plan #2"] })).unwrap();
    assert_eq!(r["unloaded"], json!(["Plan #2"]));
    // Typed lists keep their wildcards.
    let r = s.execute("-image", &json!({ "option": "unload", "names": "N*" })).unwrap();
    assert_eq!(r["unloaded"], json!(["North"]));
}

#[test]
fn an_xrefs_pictures_are_found_from_its_own_folder() {
    hooks();
    let dir = folder("xref-pictures");
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    photo(&dir.join("lib").join("logo.png"));
    let mut site = Session::new();
    let site_path = dir.join("lib").join("site.dxf");
    save(&plan(), &site_path);
    open(&mut site, &site_path);
    site.execute("imageattach", &json!({ "path": "logo.png" })).unwrap();
    site.execute("qsave", &json!({})).unwrap();

    let host = dir.join("plan.dxf");
    save(&plan(), &host);
    let mut s = Session::new();
    open(&mut s, &host);
    s.execute("xattach", &json!({ "path": "lib/site.dxf" })).unwrap();
    let xref_images = |s: &Session| -> Vec<Image> {
        s.doc()
            .unwrap()
            .blocks
            .values()
            .flat_map(|b| b.entities.iter())
            .filter_map(|e| match &e.kind {
                EntityKind::Image(i) => Some(i.clone()),
                _ => None,
            })
            .collect()
    };
    assert!(xref_images(&s).iter().all(|i| i.pixels.is_some()) && !xref_images(&s).is_empty(), "loaded on attach");
    // The xref's images are its own: -IMAGE neither lists nor detaches them.
    assert_eq!(s.execute("image", &json!({})).unwrap()["images"], json!([]));
    assert!(s.execute("-image", &json!({ "option": "detach", "names": "logo" })).is_err());
    s.execute("qsave", &json!({})).unwrap();
    let mut t = Session::new();
    open(&mut t, &host);
    assert!(xref_images(&t).iter().all(|i| i.pixels.is_some()) && !xref_images(&t).is_empty(), "loaded on reopening");
}
