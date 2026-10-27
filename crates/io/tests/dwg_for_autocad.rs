//! What the ODA File Converter's audit repaired in Cadino's DWG files, each fixed at its source
//! (the corpus and the validator are described in docs/file-format-parity.md, "Validating DWG").
#![cfg(not(target_arch = "wasm32"))]

use cadcraft_doc::*;
use cadcraft_geom::Vec3;
use cadcraft_io::{SaveOptions, Version, read, write_with};

/// The DWG versions Cadino writes.
const DWG: [Version; 6] = [Version::R2000, Version::R2004, Version::R2007, Version::R2010, Version::R2013, Version::R2018];

fn dwg(d: &Drawing, v: Version) -> Vec<u8> {
    write_with(d, "x.dwg", &SaveOptions { version: Some(v), binary: false }).unwrap_or_else(|e| panic!("{v:?}: {e}"))
}

fn line(d: &mut Drawing, space: &Space, x: f64) {
    d.add(space, Common::default(), EntityKind::Line(Line { a: Vec3::new(x, 0.0, 0.0), b: Vec3::new(x, 10.0, 0.0) })).expect("line");
}

/// The lines' x on each layout, by name.
fn layout_lines(d: &Drawing) -> Vec<(String, Vec<f64>)> {
    let mut out: Vec<(String, Vec<f64>)> = d
        .layouts
        .iter()
        .map(|l| {
            let xs = l.entities.iter().filter_map(|e| match &e.kind {
                EntityKind::Line(ln) => Some(ln.a.x),
                _ => None,
            });
            (l.name.clone(), xs.collect())
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Objects on the second and later layouts stay on their layout: the DWG names that layout's
/// block as their owner. (With acadrust 0.6.4 they were written as owned by the first layout,
/// which ODA's audit and AutoCAD had to repair.)
#[test]
fn objects_on_later_layouts_stay_on_them() {
    let mut d = Drawing::new_metric();
    d.layouts.push(Layout::new("Sheet C", 3));
    let names: Vec<String> = d.layouts.iter().map(|l| l.name.clone()).collect();
    assert!(names.len() >= 3, "{names:?}");
    for (i, n) in names.iter().enumerate() {
        for k in 0..=i {
            line(&mut d, &Space::Paper(n.clone()), (i * 10 + k) as f64);
        }
    }
    let want = layout_lines(&d);
    for v in DWG {
        let back = read(&dwg(&d, v), "x.dwg").unwrap_or_else(|e| panic!("{v:?}: {e}"));
        assert_eq!(layout_lines(&back), want, "{v:?}");
    }
}

/// A table style's rows name their text style in 2004 and 2007 DWG (acadrust writes those rows
/// by handle only; without one the rows pointed at no style).
#[test]
fn table_style_rows_name_their_text_style() {
    let mut d = Drawing::new_metric();
    line(&mut d, &Space::Model, 0.0);
    for v in [Version::R2004, Version::R2007, Version::R2018] {
        let dxf = String::from_utf8_lossy(&cadcraft_dwg::dwg_to_dxf(&dwg(&d, v)).expect("dwg to dxf")).to_string();
        let tags = cadcraft_dxf::parse(dxf.as_bytes()).expect("parse");
        let at = tags.iter().position(|t| t.code == 0 && t.str() == "TABLESTYLE").unwrap_or_else(|| panic!("{v:?}: no TABLESTYLE"));
        let styles: Vec<String> =
            tags[at + 1..].iter().take_while(|t| t.code != 0).filter(|t| t.code == 7).map(|t| t.str().trim().to_string()).collect();
        assert!(!styles.is_empty() && styles.iter().all(|s| s.eq_ignore_ascii_case("Standard")), "{v:?}: {styles:?}");
    }
}
