//! The DWG corpus: a drawing with every kind of object Cadino makes, saved as DXF and as DWG at
//! every version (2000–2018) and read back. A DWG keeps what the DXF of its version keeps.
//!
//! With `CADINO_CORPUS_DIR` set, the files are also written there, named `<version>.dxf` and
//! `<version>.dwg`, for the external validators (ODA File Converter, ezdxf; see
//! docs/file-format-parity.md, "Validating DWG").

use std::collections::BTreeMap;

use cadcraft_engine::Session;
use cadcraft_engine::doc::{Drawing, EntityKind, PolyMesh};
use cadcraft_engine::geom::Vec3;
use cadcraft_io::{SaveOptions, Version};
use serde_json::{Value, json};

fn x(s: &mut Session, id: &str, p: Value) -> Value {
    s.execute(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

/// The handle a command made (`handle`, or the first of `handles`).
fn h(r: &Value) -> String {
    r.get("handle")
        .or_else(|| r.get("handles").and_then(|v| v.get(0)))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no handle in {r}"))
        .to_string()
}

fn png(path: &std::path::Path) {
    let mut img = image::RgbImage::new(16, 12);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = image::Rgb([(x * 15) as u8, (y * 20) as u8, 128]);
    }
    img.save(path).unwrap();
}

/// Every kind of object, as a furniture drawing would have them.
fn corpus(s: &mut Session, dir: &std::path::Path) {
    s.new_drawing(true);
    // Layers, linetypes, 2D objects.
    x(s, "layer.new", json!({"name": "Walls", "color": "red", "lineweight": 0.5, "current": true}));
    x(s, "layer.new", json!({"name": "가구", "color": "30,120,200"}));
    x(s, "layer.set", json!({"name": "가구", "transparency": 40, "description": "가구 레이어"}));
    x(s, "line", json!({"points": [[0, 0], [100, 0], [100, 50]]}));
    let circle = h(&x(s, "circle", json!({"center": [20, 20], "radius": 8})));
    let arc = h(&x(s, "arc", json!({"center": [50, 20], "radius": 10, "start": 0, "end": 135})));
    x(s, "pline", json!({"vertices": [[0, 60], {"p": [30, 60], "bulge": 0.5}, {"p": [60, 70], "startWidth": 1, "endWidth": 3}, [80, 60]]}));
    x(s, "ellipse", json!({"center": [130, 20], "major": [20, 5], "ratio": 0.4}));
    x(s, "ellipse", json!({"center": [130, 50], "major": [15, 0], "ratio": 0.5, "start": 30, "end": 250}));
    x(s, "spline", json!({"fit": [[0, 100], [20, 120], [40, 95], [60, 115]]}));
    x(s, "spline", json!({"control": [[70, 100], [80, 130], [100, 90], [120, 120]], "degree": 3}));
    x(s, "point", json!({"points": [[5, 5], [6, 6]]}));
    x(s, "xline", json!({"base": [0, -20], "angle": 30}));
    x(s, "ray", json!({"base": [0, -30], "through": [10, -35]}));
    x(s, "polygon", json!({"sides": 6, "center": [170, 20], "radius": 10}));
    x(s, "rectang", json!({"p1": [160, 40], "p2": [190, 60], "fillet": 3}));
    x(s, "donut", json!({"center": [200, 10], "inside": 4, "outside": 8}));
    x(s, "revcloud", json!({"p1": [210, 30], "p2": [240, 50]}));
    x(s, "solid", json!({"points": [[250, 0], [270, 0], [250, 20], [270, 20]]}));
    x(s, "3dface", json!({"points": [[280, 0, 0], [300, 0, 5], [300, 20, 10], [280, 20, 5]]}));
    x(s, "3dpoly", json!({"points": [[310, 0, 0], [320, 10, 5], [330, 0, 10]]}));
    x(s, "helix", json!({"center": [350, 10], "baseRadius": 5, "topRadius": 3, "height": 20, "turns": 4}));
    x(s, "mline", json!({"points": [[0, 150], [50, 150], [50, 180]]}));
    x(s, "wipeout", json!({"points": [[60, 150], [80, 150], [80, 170], [60, 170]]}));
    let r = h(&x(s, "rectang", json!({"p1": [100, 150], "p2": [130, 170]})));
    x(s, "region", json!({"handles": [r]}));
    x(s, "tolerance", json!({"at": [150, 160], "symbol": "position", "tol1": "%%c0.1(M)", "datums": ["A", "B"]}));
    let (a, b) = (h(&x(s, "line", json!({"points": [[0, 200], [10, 210]]}))), h(&x(s, "circle", json!({"center": [20, 205], "radius": 3}))));
    x(s, "group", json!({"name": "G1", "handles": [a, b]}));
    // Text, Korean included.
    x(s, "style", json!({"name": "Hangul", "font": "malgun.ttf"}));
    x(s, "text", json!({"at": [0, 300], "text": "가구 설계 도면 Furniture 1:10", "height": 5}));
    x(s, "text", json!({"at": [0, 310], "text": "Right %%c50 %%d30 %%p0.1", "height": 3, "rotation": 15, "justify": "R"}));
    x(s, "mtext", json!({"at": [0, 340], "text": "{\\b1;서랍} 상판\\P두께 18mm\\P\\C1;빨강", "height": 3, "width": 60}));
    x(
        s,
        "mtext",
        json!({"at": [80, 340], "text": "첫 번째 단 내용입니다. ".repeat(6), "height": 2.5, "width": 30, "columns": {"type": "static", "count": 2, "gutter": 5, "height": 20}}),
    );
    // Dimensions of every kind, leaders and multileaders.
    x(s, "dimlinear", json!({"p1": [0, 0], "p2": [100, 0], "at": [50, -10]}));
    x(s, "dimcontinue", json!({"points": [[150, 0]]}));
    x(s, "dimlinear", json!({"p1": [0, 0], "p2": [0, 40], "at": [-10, 20]}));
    x(s, "dimbaseline", json!({"points": [[0, 60]]}));
    x(s, "dimaligned", json!({"p1": [0, 0], "p2": [40, 30], "at": [15, 30]}));
    x(s, "dimangular", json!({"vertex": [200, 0], "p1": [240, 0], "p2": [230, 30], "at": [235, 15]}));
    x(s, "dimradius", json!({"handle": circle, "at": [30, 30]}));
    x(s, "dimdiameter", json!({"handle": circle, "at": [10, 10]}));
    x(s, "dimarc", json!({"handle": arc, "at": [50, 40]}));
    let big = h(&x(s, "arc", json!({"center": [400, 150], "radius": 40, "start": 20, "end": 100})));
    x(s, "dimjogged", json!({"handle": big, "centerOverride": [450, 100], "at": [420, 185]}));
    x(s, "dimordinate", json!({"feature": [100, 0], "leader": [100, -30]}));
    x(s, "dimordinate", json!({"feature": [0, 40], "leader": [-30, 40], "xtype": false}));
    x(s, "leader", json!({"points": [[100, 100], [120, 120], [140, 120]], "text": "지시선 note"}));
    x(s, "mleader", json!({"points": [[200, 200], [220, 220]], "text": "도어 경첩"}));
    x(s, "mleader", json!({"points": [[250, 200], [260, 215], [280, 220]], "text": "spline", "leaderType": "spline"}));
    // Hatches and gradients.
    let r = h(&x(s, "rectang", json!({"p1": [0, 400], "p2": [50, 430]})));
    x(s, "hatch", json!({"handles": [r], "pattern": "ANSI31", "scale": 2, "associative": true}));
    let r = h(&x(s, "rectang", json!({"p1": [60, 400], "p2": [110, 430]})));
    x(s, "hatch", json!({"handles": [r], "pattern": "SOLID"}));
    let c = h(&x(s, "circle", json!({"center": [140, 415], "radius": 15})));
    x(s, "gradient", json!({"handles": [c], "name": "LINEAR", "color1": "blue", "color2": "yellow", "angle": 30}));
    // Blocks with attributes, nested blocks, an xclip, a dynamic block.
    let parts = [
        h(&x(s, "rectang", json!({"p1": [0, 500], "p2": [20, 510]}))),
        h(&x(s, "attdef", json!({"tag": "PART", "prompt": "부품명", "default": "선반", "at": [2, 503], "height": 2}))),
        h(&x(s, "attdef", json!({"tag": "QTY", "prompt": "Qty", "default": "1", "at": [2, 506], "height": 2, "invisible": true}))),
    ];
    x(s, "block", json!({"name": "SHELF", "base": [0, 500], "handles": parts, "description": "선반 블록"}));
    x(s, "insert", json!({"name": "SHELF", "at": [50, 500], "attribs": {"PART": "상판", "QTY": "2"}}));
    x(s, "insert", json!({"name": "SHELF", "at": [50, 530], "scale": [2, 1], "rotation": 30}));
    let c = h(&x(s, "circle", json!({"center": [0, 550], "radius": 3})));
    x(s, "block", json!({"name": "BALLOON", "base": [0, 550], "handles": [c]}));
    x(s, "mleader", json!({"points": [[300, 200], [320, 220]], "block": "BALLOON"}));
    let ins = h(&x(s, "insert", json!({"name": "SHELF", "at": [150, 500]})));
    x(s, "xclip", json!({"handles": [ins], "option": "new", "points": [[150, 500], [160, 508]]}));
    x(s, "bedit", json!({"name": "DOOR", "create": true}));
    let leaf = h(&x(s, "rectang", json!({"p1": [0, 0], "p2": [40, 2]})));
    x(s, "bparameter", json!({"type": "linear", "base": [0, 0], "end": [40, 0], "label": "Width", "valueSet": {"list": [30, 40, 50]}}));
    x(s, "baction", json!({"type": "stretch", "parameter": "Width", "frame": [[35, -1], [45, 3]], "handles": [leaf]}));
    x(s, "bparameter", json!({"type": "visibility", "at": [20, 10], "label": "Vis"}));
    x(s, "bvstate", json!({"operation": "new", "name": "Open"}));
    x(s, "bsave", json!({}));
    x(s, "bclose", json!({"save": true}));
    x(s, "insert", json!({"name": "DOOR", "at": [200, 550]}));
    // A table, an image, meshes.
    x(s, "tablestyle", json!({"name": "부품표", "textHeight": 3, "margin": 1, "current": true}));
    x(
        s,
        "table",
        json!({"at": [0, 700], "rows": 3, "cols": 3, "title": "재단 목록", "header": ["부품", "크기", "수량"],
        "cells": [["상판", "600x400", "1"], ["측판", "400x700", "2"], ["선반", "560x380", "3"]]}),
    );
    let pic = dir.join("pic.png");
    png(&pic);
    x(s, "imageattach", json!({"path": pic.to_string_lossy(), "at": [300, 700], "scale": 40, "pathType": "full"}));
    let (l1, l2) = (h(&x(s, "line", json!({"points": [[0, 1000], [100, 1000]]}))), h(&x(s, "line", json!({"points": [[0, 1100], [100, 1150]]}))));
    x(s, "rulesurf", json!({"curves": [l1, l2]}));
    let v = |x: f64, y: f64, z: f64| Vec3::new(x, y, z);
    let pface = PolyMesh {
        vertices: vec![v(500.0, 1000.0, 0.0), v(520.0, 1000.0, 0.0), v(520.0, 1020.0, 0.0), v(500.0, 1020.0, 10.0)],
        m: 0,
        n: 0,
        closed_m: false,
        closed_n: false,
        faces: vec![[1, 2, 3, 0], [1, 3, 4, 0]],
    };
    s.add_entity(EntityKind::PolyMesh(pface)).unwrap();
    // 3D solids (the OpenCASCADE kernel).
    #[cfg(feature = "occt")]
    {
        let b = h(&x(s, "box", json!({"corner1": [0, 2000, 0], "corner2": [600, 2400, 18]})));
        x(s, "cylinder", json!({"center": [800, 2000, 0], "radius": 20, "height": 100}));
        let _ = x(s, "part", json!({"handles": [b], "name": "상판", "material": "PB 18T"}));
    }
    // Layouts and viewports, named views and UCSs, a layer state, drawing properties, tiled
    // views with a perspective one.
    x(s, "layout.new", json!({"name": "도면1"}));
    x(s, "layout.set", json!({"name": "도면1"}));
    x(s, "mview", json!({"p1": [10, 10], "p2": [100, 80], "count": 2, "arrangement": "vertical"}));
    x(s, "mview.polygonal", json!({"points": [[120, 10], [200, 10], [200, 80], [160, 100], [120, 80]]}));
    x(s, "layout.new", json!({"name": "Sheet B"}));
    x(s, "layout.set", json!({"name": "Model"}));
    x(s, "view", json!({"option": "save", "name": "Plan"}));
    x(s, "ucs", json!({"origin": [10, 10, 0], "xaxis": [1, 1, 0]}));
    x(s, "ucsman", json!({"save": "Tilt"}));
    x(s, "ucs", json!({"preset": "world"}));
    x(s, "layerstate.save", json!({"name": "작업상태"}));
    x(s, "dwgprops", json!({"title": "가구 도면", "author": "Cadino", "keywords": "furniture", "custom": {"Project": "Kitchen"}}));
    x(s, "viewports.rhino", json!({}));
    // Rhino's "Apply grid changes to the active viewport only": Top gets a grid of its own.
    x(s, "options", json!({"gridViewport": "active", "GRIDUNIT": [1, 1], "GRIDLINECOUNT": 200, "GRIDAXES": 0}));
    x(s, "dview", json!({"camera": [300, -300, 200], "target": [50, 25, 15], "distance": 500, "lens": 50}));
    x(s, "view", json!({"option": "save", "name": "Persp"}));
}

/// What a file kept: objects by type in model space, each layout and each named block, and the
/// names in the tables.
fn summary(d: &Drawing) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut count = |place: &str, store: &cadcraft_engine::doc::EntityStore| {
        let mut by: BTreeMap<&str, usize> = BTreeMap::new();
        for e in store.iter() {
            // The DWG bridge writes a polygon mesh as the same faces in a polyface mesh.
            let t = match e.kind.type_name() {
                "Polygon Mesh" => "Polyface Mesh",
                t => t,
            };
            *by.entry(t).or_default() += 1;
        }
        out.insert(format!("objects in {place}"), format!("{by:?}"));
    };
    count("Model", &d.model);
    for l in &d.layouts {
        count(&format!("layout {}", l.name), &l.entities);
    }
    for b in d.blocks.values().filter(|b| !b.name.starts_with('*')) {
        count(&format!("block {}", b.name), &b.entities);
    }
    let names = |v: Vec<String>| {
        let mut v = v;
        v.sort();
        v.join(", ")
    };
    out.insert("layers".into(), names(d.layers.iter().map(|l| l.name.clone()).collect()));
    out.insert("text styles".into(), names(d.text_styles.iter().map(|t| t.name.clone()).collect()));
    out.insert("groups".into(), names(d.groups.iter().map(|g| format!("{} {}", g.name, g.members.len())).collect()));
    out.insert("views".into(), names(d.views.iter().map(|v| v.name.clone()).collect()));
    out.insert("ucs".into(), names(d.ucss.iter().map(|u| u.name.clone()).collect()));
    let grids = d.vports.iter().flat_map(|c| c.tiles.iter().filter_map(|t| t.grid).map(|g| format!("{} {:?}", c.name, g)));
    out.insert("tile grids".into(), names(grids.collect()));
    out
}

fn differences(a: &BTreeMap<String, String>, b: &BTreeMap<String, String>) -> Vec<String> {
    let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .map(|k| format!("{k}: {} ≠ {}", a.get(k).map_or("-", String::as_str), b.get(k).map_or("-", String::as_str)))
        .collect()
}

#[test]
fn dwg_at_every_version_keeps_what_dxf_keeps() {
    let tmp = std::env::temp_dir().join(format!("cadrhino-dwg-corpus-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let out = std::env::var_os("CADINO_CORPUS_DIR").map(std::path::PathBuf::from);
    let mut s = Session::new();
    corpus(&mut s, &tmp);
    let d = s.doc().unwrap();
    let mut problems = Vec::new();
    for v in Version::ALL {
        let opts = SaveOptions { version: Some(v), binary: false };
        let dxf = cadcraft_io::write_with(d, "c.dxf", &opts).unwrap();
        let dwg = cadcraft_io::write_with(d, "c.dwg", &opts).unwrap();
        if let Some(dir) = &out {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(format!("{}.dxf", v.year())), &dxf).unwrap();
            std::fs::write(dir.join(format!("{}.dwg", v.year())), &dwg).unwrap();
        }
        assert_eq!(&dwg[..6], v.code().as_bytes());
        let from_dxf = summary(&cadcraft_io::read(&dxf, "c.dxf").unwrap());
        let back = cadcraft_io::read(&dwg, "c.dwg").unwrap_or_else(|e| panic!("{v:?}: {e}"));
        assert!(back.read_notes.is_empty(), "{v:?}: {:?}", back.read_notes);
        problems.extend(differences(&from_dxf, &summary(&back)).into_iter().map(|p| format!("{}: {p}", v.year())));
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(problems.is_empty(), "DWG lost what DXF keeps:\n{}", problems.join("\n"));
}
