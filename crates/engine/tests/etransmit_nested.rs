//! ETRANSMIT with nested references: an xref's own xrefs and images go into the package where
//! it looks for them, a circular reference is packed once, and the unpacked package opens with
//! everything found.

use std::path::{Path, PathBuf};

use cadcraft_engine::Session;
use cadcraft_engine::cmd::file::{IoHooks, set_io};
use cadcraft_engine::doc::geom::Vec3;
use cadcraft_engine::doc::{Block, Drawing, EntityKind, Insert, Line, Space, XrefStatus};
use serde_json::json;

fn hooks() {
    set_io(IoHooks {
        read: |b, name| cadcraft_io::read(b, name).map_err(|e| e.to_string()),
        write: |d, name| cadcraft_io::write(d, name).map_err(|e| e.to_string()),
        plot: None,
    });
}

fn folder(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cadcraft-etransmit-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A drawing with a line and an attached xref to each of `xrefs` (saved paths).
fn drawing(xrefs: &[&str]) -> Drawing {
    let mut d = Drawing::new_metric();
    d.add(&Space::Model, Default::default(), EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(10.0, 0.0, 0.0) })).unwrap();
    for (k, x) in xrefs.iter().enumerate() {
        let name = format!("X{k}");
        let mut b = Block::new(&name);
        b.xref_path = Some((*x).to_string());
        b.xref_status = XrefStatus::Unresolved;
        d.blocks.insert(name.clone(), std::sync::Arc::new(b));
        let ins = Insert {
            block: name,
            insert: Vec3::ZERO,
            scale: Vec3::new(1.0, 1.0, 1.0),
            rotation: 0.0,
            attribs: vec![],
            cols: 1,
            rows: 1,
            col_spacing: 0.0,
            row_spacing: 0.0,
        };
        d.add(&Space::Model, Default::default(), EntityKind::Insert(ins)).unwrap();
    }
    d
}

fn save(d: &Drawing, path: &Path) {
    std::fs::write(path, cadcraft_io::write(d, &path.to_string_lossy()).unwrap()).unwrap();
}

/// The entries of a stored ZIP, by its central directory.
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
fn nested_xrefs_and_their_images_are_packed_once() {
    hooks();
    let dir = folder("nested");
    std::fs::create_dir_all(dir.join("xrefs")).unwrap();
    std::fs::create_dir_all(dir.join("pics")).unwrap();
    image::RgbaImage::from_pixel(4, 2, image::Rgba([10, 200, 10, 255])).save(dir.join("pics").join("p.png")).unwrap();
    // plan → xrefs/a → b (beside a) and ../pics/p.png; b → a again (circular) and the plan.
    save(&drawing(&["xrefs/a.dxf"]), &dir.join("plan.dxf"));
    let mut ad = drawing(&["b.dxf"]);
    let img = cadcraft_engine::doc::Image {
        u: Vec3::new(1.0, 0.0, 0.0),
        v: Vec3::new(0.0, 1.0, 0.0),
        size: cadcraft_engine::doc::geom::Vec2::new(4.0, 2.0),
        path: "../pics/p.png".into(),
        name: "p".into(),
        ..Default::default()
    };
    ad.add(&Space::Model, Default::default(), EntityKind::Image(img)).unwrap();
    save(&ad, &dir.join("xrefs").join("a.dxf"));
    save(&drawing(&["a.dxf", "../plan.dxf", "gone.dxf"]), &dir.join("xrefs").join("b.dxf"));

    let mut s = Session::new();
    s.execute("open", &json!({ "path": dir.join("plan.dxf").to_string_lossy() })).unwrap();
    let r = s.execute("etransmit", &json!({})).unwrap();
    let entries = unzip(&std::fs::read(dir.join("plan.zip")).unwrap());
    let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["plan.dxf", "xrefs/a.dxf", "xrefs/b.dxf", "pics/p.png", "plan - Transmittal Report.txt"], "{r}");
    let missing: Vec<&str> = r["missing"].as_array().unwrap().iter().filter_map(|m| m["name"].as_str()).collect();
    assert_eq!(missing, ["xrefs/gone.dxf"]);

    // Unpacked elsewhere, the nested xref and its picture are found.
    let out = folder("nested-out");
    for (n, b) in &entries {
        let p = out.join(n);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b).unwrap();
    }
    let mut t = Session::new();
    t.execute("open", &json!({ "path": out.join("plan.dxf").to_string_lossy() })).unwrap();
    let xr = t.execute("xref", &json!({})).unwrap();
    assert!(xr["xrefs"].as_array().unwrap().iter().any(|x| x["status"] == "Loaded"), "{xr}");
    let d = t.doc().unwrap();
    let nested = d.blocks.values().find(|b| b.name.to_ascii_uppercase().ends_with("|X0")).expect("a's own xref came along");
    assert!(nested.entities.iter().count() > 0, "b loaded inside a");
    let pictures =
        d.blocks.values().flat_map(|b| b.entities.iter()).filter(|e| matches!(&e.kind, EntityKind::Image(i) if i.pixels.is_some())).count();
    assert_eq!(pictures, 1, "a's picture loaded from the package");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&out);
}
