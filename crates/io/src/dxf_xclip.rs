//! XCLIP boundaries of block references in DXF (DXF Reference: SPATIAL_FILTER object). A clipped
//! INSERT has an extension dictionary whose `ACAD_FILTER` entry is a dictionary whose `SPATIAL`
//! entry is the SPATIAL_FILTER: the boundary vertices (two = a rectangle's corners), the
//! clipping flag, the front and back clipping planes, and two 4x3 matrices (the inverse of the
//! reference's transform when it was clipped, and the transform from block coordinates into the
//! boundary's own). CADCraft keeps the clip in the reference ([`cadcraft_doc::xclip`]) in block
//! coordinates and writes the second matrix as the identity.

use std::collections::HashMap;

use cadcraft_doc::xclip::{MAX_VERTICES, XClip};
use cadcraft_doc::{Drawing, Entity, EntityKind, Handle};
use cadcraft_dxf::Tag;
use cadcraft_geom::{Mat3, Vec2};

/// The objects written for one clipped reference: its extension dictionary, the `ACAD_FILTER`
/// dictionary and the SPATIAL_FILTER.
pub(crate) struct Obj {
    pub xdict: String,
    dict: String,
    filter: String,
    clip: XClip,
    /// The reference's block-to-world transform (2D) and z scale.
    xf: Mat3,
    sz: f64,
}

/// The clipped references among `ents`, each with fresh handles from `h`.
pub(crate) fn plan<'a>(d: &Drawing, ents: impl IntoIterator<Item = &'a Entity>, mut h: impl FnMut() -> String) -> HashMap<Handle, Obj> {
    let mut out = HashMap::new();
    for e in ents {
        let EntityKind::Insert(ins) = &e.kind else { continue };
        let Some(clip) = XClip::of(&e.common) else { continue };
        let base = d.block(&ins.block).map(|b| b.base.xy()).unwrap_or(Vec2::ZERO);
        let sz = if ins.scale.z.is_finite() && ins.scale.z != 0.0 { ins.scale.z } else { 1.0 };
        out.insert(e.handle, Obj { xdict: h(), dict: h(), filter: h(), clip, xf: ins.transform(base), sz });
    }
    out
}

/// A 4x3 matrix in column-major order (twelve 40 groups) from a 2D transform and a z scale.
fn matrix(m: &Mat3, sz: f64, out: &mut Vec<Tag>) {
    for v in [m.a, m.b, 0.0, m.c, m.d, 0.0, 0.0, 0.0, sz, m.e, m.f, 0.0] {
        out.push(Tag::f(40, v));
    }
}

/// The OBJECTS section records of the planned clips, in handle order.
pub(crate) fn objects(plan: &HashMap<Handle, Obj>) -> Vec<Tag> {
    let mut list: Vec<(&Handle, &Obj)> = plan.iter().collect();
    list.sort_by_key(|(h, _)| **h);
    let mut t = Vec::new();
    let head = |t: &mut Vec<Tag>, kind: &str, h: &str, owner: &str| {
        t.extend([Tag::s(0, kind), Tag::s(5, h), Tag::s(102, "{ACAD_REACTORS"), Tag::s(330, owner), Tag::s(102, "}"), Tag::s(330, owner)]);
    };
    for (insert, o) in list {
        head(&mut t, "DICTIONARY", &o.xdict, &insert.hex());
        t.extend([Tag::s(100, "AcDbDictionary"), Tag::i(280, 1), Tag::i(281, 1), Tag::s(3, "ACAD_FILTER"), Tag::s(360, o.dict.clone())]);
        head(&mut t, "DICTIONARY", &o.dict, &o.xdict);
        t.extend([Tag::s(100, "AcDbDictionary"), Tag::i(280, 1), Tag::i(281, 1), Tag::s(3, "SPATIAL"), Tag::s(360, o.filter.clone())]);
        head(&mut t, "SPATIAL_FILTER", &o.filter, &o.dict);
        t.extend([Tag::s(100, "AcDbFilter"), Tag::s(100, "AcDbSpatialFilter")]);
        let vs: Vec<Vec2> = o.clip.boundary.iter().take(MAX_VERTICES).copied().collect();
        t.push(Tag::i(70, vs.len() as i64));
        for v in &vs {
            t.extend([Tag::f(10, v.x), Tag::f(20, v.y)]);
        }
        t.extend([Tag::f(210, 0.0), Tag::f(220, 0.0), Tag::f(230, 1.0), Tag::f(11, 0.0), Tag::f(21, 0.0), Tag::f(31, 0.0)]);
        t.push(Tag::i(71, i64::from(o.clip.on)));
        t.push(Tag::i(72, i64::from(o.clip.front.is_some())));
        if let Some(f) = o.clip.front {
            t.push(Tag::f(40, f));
        }
        t.push(Tag::i(73, i64::from(o.clip.back.is_some())));
        if let Some(b) = o.clip.back {
            t.push(Tag::f(41, b));
        }
        let inv = o.xf.inverse().unwrap_or(Mat3::IDENTITY);
        matrix(&inv, 1.0 / o.sz, &mut t);
        matrix(&Mat3::IDENTITY, 1.0, &mut t);
    }
    t
}

/// The owner (330 outside the `102` groups) of an object's groups.
pub(crate) fn owner(tags: &[Tag]) -> Option<String> {
    let mut in_group = false;
    for t in tags {
        match t.code {
            100 => return None,
            102 => in_group = t.str().trim_start().starts_with('{'),
            330 if !in_group => return Some(t.str().trim().to_ascii_uppercase()),
            _ => {}
        }
    }
    None
}

/// A SPATIAL_FILTER's clip, in block coordinates.
pub(crate) fn read(tags: &[Tag]) -> Option<XClip> {
    let start = tags.iter().position(|t| t.code == 100 && t.str().trim() == "AcDbSpatialFilter")?;
    let num = |t: &Tag| Some(t.f64()).filter(|v| v.is_finite());
    let mut boundary = Vec::new();
    let mut x = None;
    let (mut on, mut front, mut back) = (true, None, None);
    // After the 73 flag (and 41) come the two matrices' 40 groups.
    let (mut front_next, mut matrices) = (false, false);
    let mut m: Vec<f64> = Vec::new();
    for t in tags.get(start + 1..)? {
        match t.code {
            10 => x = num(t),
            20 => {
                if let (Some(px), Some(py)) = (x.take(), num(t))
                    && boundary.len() < MAX_VERTICES
                {
                    boundary.push(Vec2::new(px, py));
                }
            }
            71 => on = t.i64() != 0,
            72 => front_next = t.i64() != 0,
            40 if front_next => {
                front = num(t);
                front_next = false;
            }
            73 => {
                matrices = true;
                if t.i64() == 0 {
                    back = None;
                }
            }
            41 => back = num(t),
            40 if matrices && m.len() < 24 => m.push(t.f64()),
            _ => {}
        }
    }
    // Boundary coordinates → block coordinates: the inverse of the second matrix.
    if let [a, b, _, c, d_, _, _, _, _, e, f, _] = m.get(12..24).unwrap_or(&[]) {
        let to_boundary = Mat3 { a: *a, b: *b, c: *c, d: *d_, e: *e, f: *f };
        if to_boundary != Mat3::IDENTITY
            && let Some(inv) = to_boundary.inverse()
        {
            boundary = boundary.iter().map(|p| inv.apply(*p)).collect();
        }
    }
    (boundary.len() >= 2 && boundary.iter().all(|p| p.is_finite())).then_some(XClip { boundary, on, front, back })
}

/// Give each clipped reference its clip: `filters` are the SPATIAL_FILTERs (owner, groups),
/// `owners` the dictionaries' owners and `names` the dictionary entry names, by handle.
pub(crate) fn apply(d: &mut Drawing, filters: &[(String, Vec<Tag>)], owners: &HashMap<String, String>, names: &HashMap<String, String>) {
    for (dict, tags) in filters {
        // SPATIAL_FILTER → its ACAD_FILTER dictionary → the extension dictionary → the reference.
        if !names.get(dict).is_some_and(|n| n.eq_ignore_ascii_case("ACAD_FILTER")) {
            continue;
        }
        let Some(insert) = owners.get(dict).and_then(|x| owners.get(x)).and_then(|h| Handle::parse_hex(h)) else { continue };
        let Some(clip) = read(tags) else { continue };
        let put = |e: &mut Entity| {
            if matches!(e.kind, EntityKind::Insert(_)) {
                XClip::put(&mut e.common, Some(&clip));
            }
        };
        if d.modify_entity(insert, put).is_err() {
            for b in d.blocks.values_mut() {
                if b.entities.contains(insert) {
                    std::sync::Arc::make_mut(b).entities.modify(insert, put);
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::{Block, Common, Insert, Line, Space};
    use cadcraft_geom::Vec3;

    use super::*;

    fn clipped(clip: &XClip) -> (Drawing, Handle) {
        let mut d = Drawing::new_metric();
        let mut b = Block::new("Plan");
        b.entities.push(Entity::new(Handle(0x50), EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(100.0, 0.0, 0.0) })));
        d.blocks.insert("Plan".into(), std::sync::Arc::new(b));
        let mut c = Common::default();
        XClip::put(&mut c, Some(clip));
        let ins = Insert {
            block: "Plan".into(),
            insert: Vec3::new(10.0, 5.0, 0.0),
            scale: Vec3::new(2.0, 2.0, 1.0),
            rotation: 0.5,
            attribs: vec![],
            cols: 1,
            rows: 1,
            col_spacing: 0.0,
            row_spacing: 0.0,
        };
        let h = d.add(&Space::Model, c, EntityKind::Insert(ins)).unwrap();
        (d, h)
    }

    #[test]
    fn an_xclip_round_trips_as_a_spatial_filter() {
        for clip in [
            XClip { boundary: vec![Vec2::new(10.0, -5.0), Vec2::new(40.0, 5.0)], on: true, front: None, back: None },
            XClip {
                boundary: vec![Vec2::new(0.0, -1.0), Vec2::new(30.0, -1.0), Vec2::new(15.0, 9.0)],
                on: false,
                front: Some(2.5),
                back: Some(-1.0),
            },
        ] {
            let (d, h) = clipped(&clip);
            let text = crate::write_dxf(&d);
            assert!(text.contains("SPATIAL_FILTER") && text.contains("ACAD_FILTER") && text.contains("AcDbSpatialFilter"), "{text}");
            // The clip travels as the standard object only, not as CADCraft xdata.
            assert!(!text.contains(cadcraft_doc::xclip::APP));
            let back = crate::read_dxf(text.as_bytes()).unwrap();
            let e = back.entity(h).unwrap();
            assert_eq!(XClip::of(&e.common), Some(clip.clone()));
            // Written again, the reference keeps one extension dictionary.
            let again = crate::write_dxf(&back);
            assert_eq!(again.matches("ACAD_FILTER").count(), 1, "{again}");
        }
    }

    /// A clipped reference's other extension dictionary entries (an application's XRECORD, a
    /// dynamic block's link) stay beside the clip when the file is saved again, in the one
    /// extension dictionary; a filter that can't be read is kept as it was.
    #[test]
    fn other_extension_dictionary_entries_of_a_clipped_reference_survive() {
        let clip = XClip { boundary: vec![Vec2::new(10.0, -5.0), Vec2::new(40.0, 5.0)], on: true, front: None, back: None };
        let (d, h) = clipped(&clip);
        let mut tags = cadcraft_dxf::parse_ascii(&crate::write_dxf(&d)).unwrap();
        let at = tags.iter().position(|t| t.code == 3 && t.str() == "ACAD_FILTER").unwrap();
        let xd = tags[..at].iter().rev().find(|t| t.code == 5).unwrap().str();
        tags.splice(at + 2..at + 2, [Tag::s(3, "MYAPP"), Tag::s(360, "FFF0")]);
        let end = tags.iter().rposition(|t| t.code == 0 && t.str() == "ENDSEC").unwrap();
        let xrecord = [
            Tag::s(0, "XRECORD"),
            Tag::s(5, "FFF0"),
            Tag::s(102, "{ACAD_REACTORS"),
            Tag::s(330, xd.clone()),
            Tag::s(102, "}"),
            Tag::s(330, xd),
            Tag::s(100, "AcDbXrecord"),
            Tag::i(280, 1),
            Tag::s(1, "KEEPME"),
        ];
        tags.splice(end..end, xrecord);
        let back = crate::read_dxf(cadcraft_dxf::write_ascii(&tags).as_bytes()).unwrap();
        assert_eq!(XClip::of(&back.entity(h).unwrap().common), Some(clip));
        let out = cadcraft_dxf::parse_ascii(&crate::write_dxf(&back)).unwrap();
        let recs = cadcraft_dxf::records(&out);
        let handle = |t: &[Tag]| t.iter().find(|t| t.code == 5).map(Tag::str).unwrap_or_default();
        let (_, ins) = recs.iter().find(|(k, t)| k == "INSERT" && handle(t) == h.hex()).unwrap();
        let xdict = ins.iter().skip_while(|t| t.str() != "{ACAD_XDICTIONARY").nth(1).unwrap().str();
        let (_, dict) = recs.iter().find(|(k, t)| k == "DICTIONARY" && handle(t) == xdict).unwrap();
        let keys: Vec<String> = dict.iter().filter(|t| t.code == 3).map(Tag::str).collect();
        assert_eq!(keys, ["ACAD_FILTER", "MYAPP"]);
        let (_, xr) = recs.iter().find(|(k, t)| k == "XRECORD" && t.iter().any(|t| t.str() == "KEEPME")).unwrap();
        assert_eq!(owner(xr), Some(xdict.clone()));
        assert_eq!(out.iter().filter(|t| t.code == 3 && t.str() == "ACAD_FILTER").count(), 1);

        // A SPATIAL_FILTER without a boundary isn't a clip CADCraft reads: kept raw.
        let sf = tags.iter().position(|t| t.code == 100 && t.str() == "AcDbSpatialFilter").unwrap();
        let sf_end = sf + tags[sf..].iter().position(|t| t.code == 0).unwrap();
        let bad: Vec<Tag> =
            tags.iter().enumerate().filter(|(i, t)| !((sf..sf_end).contains(i) && matches!(t.code, 10 | 20))).map(|(_, t)| t.clone()).collect();
        let back = crate::read_dxf(cadcraft_dxf::write_ascii(&bad).as_bytes()).unwrap();
        assert_eq!(XClip::of(&back.entity(h).unwrap().common), None);
        let text = crate::write_dxf(&back);
        assert!(text.contains("SPATIAL_FILTER") && text.contains("ACAD_FILTER") && text.contains("KEEPME"), "{text}");
    }

    /// Through the DWG bridge too.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn an_xclip_survives_a_dwg_save() {
        let clip = XClip { boundary: vec![Vec2::new(10.0, -5.0), Vec2::new(40.0, 5.0)], on: true, front: None, back: None };
        let (d, _) = clipped(&clip);
        let back = crate::read(&crate::write(&d, "x.dwg").unwrap(), "x.dwg").unwrap();
        let clips: Vec<XClip> = back.model.iter().filter_map(|e| XClip::of(&e.common)).collect();
        assert_eq!(clips, vec![clip]);
    }

    #[test]
    fn a_boundary_in_its_own_coordinates_comes_back_in_block_coordinates() {
        // The second matrix moves block coordinates by (5, 0): boundary (5,0)-(15,10) is block (0,0)-(10,10).
        let mut tags = vec![Tag::s(100, "AcDbFilter"), Tag::s(100, "AcDbSpatialFilter"), Tag::i(70, 2)];
        tags.extend([Tag::f(10, 5.0), Tag::f(20, 0.0), Tag::f(10, 15.0), Tag::f(20, 10.0), Tag::i(71, 1), Tag::i(72, 0), Tag::i(73, 0)]);
        let id = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        let shift = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 5.0, 0.0, 0.0];
        tags.extend(id.iter().chain(&shift).map(|v| Tag::f(40, *v)));
        let x = read(&tags).unwrap();
        assert_eq!(x.boundary, vec![Vec2::new(0.0, 0.0), Vec2::new(10.0, 10.0)]);
        assert!(x.on && x.front.is_none() && x.back.is_none());
        // Hostile: no vertices, NaN, a missing marker.
        assert!(read(&[Tag::s(100, "AcDbSpatialFilter"), Tag::i(70, 99), Tag::f(10, f64::NAN), Tag::f(20, 1.0)]).is_none());
        assert!(read(&[Tag::i(70, 2)]).is_none());
    }
}
