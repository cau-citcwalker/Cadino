//! 3D solids in DXF. A 3DSOLID carries an ACIS body for AutoCAD and other programs
//! ([`crate::acis`], made from the solid's mesh): SAT text in the modeler data up to 2010 (a group
//! 1 per line, continued in 3s, encoded as AutoCAD does), SAB in the data store section (ACDSDATA,
//! [`data_store`]) from 2013. CAD-rhino's own model is beside it: `CADCRAFT` xdata `1000 SOLID3D`
//! holding the model → world transform (16 reals, row by row), and the OpenCASCADE B-rep text in an
//! XRECORD of the named object dictionary, `CADCRAFT_BREP_<solid handle>`: a group 1 per line,
//! continued in groups 3. (Files written before it kept those lines in the 3DSOLID's own modeler
//! data; they are still read.) The mesh is rebuilt from the B-rep when the file is read. Other
//! 3DSOLIDs stay unknown objects (kept as read).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cadcraft_doc::{Drawing, Entity, EntityKind, Handle, Solid3d};
use cadcraft_dxf::{Tag, Value, Version};
use cadcraft_geom::{Mat4, Mesh3};

use crate::dxf_ext::{APP, MAX_PAYLOAD, xdata};

const MARK: &str = "SOLID3D";
/// Before a solid's part name and material in its xdata.
const PART: &str = "PART";
/// Named-object-dictionary key of a solid's B-rep XRECORD, before the solid's handle (hex).
pub(crate) const KEY: &str = "CADCRAFT_BREP_";
/// Longest string written in one group.
const CHUNK: usize = 250;

/// The groups after the common entity groups, xdata last. `target` is the version the file ends
/// up in: `AcDb3dSolid` and its history handle came with 2007; from 2013 the ACIS body is in the
/// data store ([`data_store`]) and the solid has a flag (290) and an id (2) instead of version 1
/// modeler data.
pub(crate) fn tags(s: &Solid3d, handle: Handle, target: Version) -> Vec<Tag> {
    let mut out = Vec::new();
    if target >= Version::R2013 {
        out.extend([Tag::i(290, 1), Tag::s(2, format!("{{CADC0000-0000-4000-8000-{:012X}}}", handle.0 & 0xFFFF_FFFF_FFFF))]);
    } else {
        out.push(Tag::i(70, 1));
        if let Some(body) = acis_body(s, handle) {
            out.extend(sat_groups(&body.sat()));
        }
    }
    if target >= Version::R2007 {
        out.extend([Tag::s(100, "AcDb3dSolid"), Tag::s(350, "0")]);
    }
    out.extend([Tag::s(1001, APP), Tag::s(1000, MARK)]);
    out.extend(s.xform.m.iter().flatten().map(|v| Tag::f(1040, *v)));
    if !s.part.is_empty() {
        // xdata strings hold 255 bytes at most.
        let cut = |t: &str| {
            t.chars()
                .scan(0usize, |n, c| {
                    *n += c.len_utf8();
                    (*n <= 250).then_some(c)
                })
                .collect::<String>()
        };
        out.extend([Tag::s(1000, PART), Tag::s(1000, cut(&s.part.name)), Tag::s(1000, cut(&s.part.material))]);
    }
    out
}

/// B-rep text as groups: a 1 per line, continued in 3s.
pub(crate) fn lines(brep: &str) -> Vec<Tag> {
    let mut out = Vec::new();
    for line in brep.lines() {
        let chars: Vec<char> = line.chars().collect();
        let mut chunks = chars.chunks(CHUNK);
        out.push(Tag::s(1, chunks.next().map(|c| c.iter().collect::<String>()).unwrap_or_default()));
        out.extend(chunks.map(|c| Tag::s(3, c.iter().collect::<String>())));
    }
    out
}

/// The solid's ACIS body. A mesh that makes none (not a closed, consistently oriented surface) is
/// logged: other programs get the solid without one.
fn acis_body(s: &Solid3d, h: Handle) -> Option<crate::acis::Body> {
    let body = crate::acis::body(&s.mesh, &s.xform);
    if body.is_none() && !s.mesh.tris.is_empty() {
        log::warn!("3D solid {}: its mesh makes no ACIS body; other programs get the solid without one", h.hex());
    }
    body
}

/// SAT text as modeler data: each line encoded (a printable character `c` becomes `159 - c`,
/// AutoCAD's cipher) in a group 1, continued in 3s. The cipher turns `A` into `^`, DXF's escape
/// character, written `^ ` (an escaped caret); a group never ends inside one.
fn sat_groups(sat: &str) -> Vec<Tag> {
    let mut out = Vec::new();
    for line in sat.lines() {
        let mut group = String::new();
        let mut code = 1;
        for c in line.chars() {
            let unit = match c {
                'A' => "^ ".to_string(),
                '!'..='~' => char::from(159 - c as u8).to_string(),
                c => c.to_string(),
            };
            if group.len() + unit.len() > CHUNK {
                out.push(Tag::s(code, std::mem::take(&mut group)));
                code = 3;
            }
            group.push_str(&unit);
        }
        out.push(Tag::s(code, group));
    }
    out
}

/// Whether modeler data text is an ACIS body (SAT, encoded or not): its first line is the four
/// header numbers. Ours is beside it, so it is passed over; anything else is a B-rep written there
/// by an earlier version.
fn is_acis(text: &str) -> bool {
    let first = text.lines().next().unwrap_or_default();
    let decoded: String = first.replace("^ ", "^").chars().map(|c| if ('!'..='~').contains(&c) { char::from(159 - c as u8) } else { c }).collect();
    let header = |l: &str| l.split_whitespace().take(4).filter(|t| t.parse::<i64>().is_ok()).count() == 4;
    header(first) || header(&decoded)
}

/// The text of [`lines`] groups (others are passed over); `None` when it is too long.
fn text(tags: &[Tag]) -> Option<String> {
    let mut brep = String::new();
    let mut first = true;
    for t in tags {
        match t.code {
            1 => {
                if !first {
                    brep.push('\n');
                }
                first = false;
                brep.push_str(&t.str());
            }
            3 => brep.push_str(&t.str()),
            _ => {}
        }
        if brep.len() > MAX_PAYLOAD {
            return None;
        }
    }
    if !first {
        brep.push('\n');
    }
    Some(brep)
}

/// Mesh every solid read from a file, in one kernel worker ([`cadcraft_kernel::tessellate_all`]).
/// A solid the kernel can't mesh keeps an empty mesh (nothing to draw), which is logged; without
/// the kernel (the web build) that is every solid.
pub(crate) fn mesh_all<'a>(ents: impl Iterator<Item = &'a mut Entity>) {
    let mut solids: Vec<(Handle, &mut Solid3d)> = ents
        .filter_map(|e| match &mut e.kind {
            EntityKind::Solid3d(s) if !s.brep.trim().is_empty() && s.mesh.tris.is_empty() => Some((e.handle, s)),
            _ => None,
        })
        .collect();
    if solids.is_empty() {
        return;
    }
    let breps: Vec<Arc<str>> = solids.iter().map(|(_, s)| s.brep.clone()).collect();
    let refs: Vec<&str> = breps.iter().map(|b| &**b).collect();
    for ((h, s), m) in solids.iter_mut().zip(cadcraft_kernel::tessellate_all(&refs)) {
        match m {
            Ok(m) => s.mesh = Arc::new(m),
            Err(e) => log::warn!("3D solid {}: {e}; it is kept but has nothing to draw", h.hex()),
        }
    }
}

/// A 3DSOLID CAD-rhino wrote; `None` for others. Its B-rep comes from its XRECORD
/// ([`attach`]) unless the record has it (files written before the XRECORD, `(entget)` lists).
pub(crate) fn read(tags: &[Tag]) -> Option<EntityKind> {
    let x = xdata(tags, APP);
    let at = x.iter().position(|t| t.code == 1000 && t.str().trim() == MARK)?;
    let vals: Vec<f64> = x.get(at + 1..).unwrap_or(&[]).iter().take_while(|t| t.code == 1040).map(Tag::f64).collect();
    let mut xform = Mat4::IDENTITY;
    if vals.len() == 16 && vals.iter().all(|v| v.is_finite()) {
        for (i, v) in vals.iter().enumerate() {
            if let Some(cell) = xform.m.get_mut(i / 4).and_then(|r| r.get_mut(i % 4)) {
                *cell = *v;
            }
        }
    }
    let start = tags.iter().position(|t| t.code == 100 && t.str() == "AcDbModelerGeometry").map_or(0, |i| i + 1);
    let body = tags.get(start..).unwrap_or(&[]);
    let brep = text(body.get(..body.iter().position(|t| t.code == 1001).unwrap_or(body.len())).unwrap_or(&[]))?;
    // A DWG converted back to DXF has an empty line of modeler data, a file written for AutoCAD
    // the solid's ACIS body: the B-rep is in the XRECORD then ([`attach`]). The mesh comes later
    // ([`mesh_all`]).
    let brep = if brep.trim().is_empty() || is_acis(&brep) { String::new() } else { brep };
    let part = x
        .iter()
        .position(|t| t.code == 1000 && t.str().trim() == PART)
        .map(|i| {
            let s = |k: usize| x.get(i + k).filter(|t| t.code == 1000).map(Tag::str).unwrap_or_default();
            cadcraft_doc::Part { name: s(1), material: s(2) }
        })
        .unwrap_or_default();
    Some(EntityKind::Solid3d(Solid3d { brep: Arc::from(brep), mesh: Arc::new(Mesh3::default()), xform, part }))
}

/// Give a solid read without its B-rep the one in its XRECORD (`breps`: solid handle → the
/// record's groups); [`mesh_all`] makes its mesh.
pub(crate) fn attach(e: &mut Entity, breps: &HashMap<Handle, &[Tag]>) {
    let EntityKind::Solid3d(s) = &mut e.kind else { return };
    if !s.brep.is_empty() {
        return;
    }
    match breps.get(&e.handle).and_then(|t| text(t)).filter(|b| !b.trim().is_empty()) {
        Some(brep) => s.brep = Arc::from(brep),
        None => log::warn!("3D solid {}: the file has no B-rep for it; it is kept but has nothing to draw", e.handle.hex()),
    }
}

/// A new data store section's groups before its records, as AutoCAD writes them (ASCII DXF): the
/// thumbnail and ASM data schemas with their properties, then the property schemas they use.
const SCHEMAS: &str = "70\n2\n71\n2\n\
0\nACDSSCHEMA\n90\n0\n1\nAcDb_Thumbnail_Schema\n2\nAcDbDs::ID\n280\n10\n91\n8\n2\nThumbnail_Data\n280\n15\n91\n0\n\
101\nACDSRECORD\n95\n0\n90\n2\n2\nAcDbDs::TreatedAsObjectData\n280\n1\n291\n1\n\
101\nACDSRECORD\n95\n0\n90\n3\n2\nAcDbDs::Legacy\n280\n1\n291\n1\n\
101\nACDSRECORD\n1\nAcDbDs::ID\n90\n4\n2\nAcDs:Indexable\n280\n1\n291\n1\n\
101\nACDSRECORD\n1\nAcDbDs::ID\n90\n5\n2\nAcDbDs::HandleAttribute\n280\n7\n282\n1\n\
0\nACDSSCHEMA\n90\n1\n1\nAcDb3DSolid_ASM_Data\n2\nAcDbDs::ID\n280\n10\n91\n8\n2\nASM_Data\n280\n15\n91\n0\n\
101\nACDSRECORD\n95\n1\n90\n2\n2\nAcDbDs::TreatedAsObjectData\n280\n1\n291\n1\n\
101\nACDSRECORD\n95\n1\n90\n3\n2\nAcDbDs::Legacy\n280\n1\n291\n1\n\
101\nACDSRECORD\n1\nAcDbDs::ID\n90\n4\n2\nAcDs:Indexable\n280\n1\n291\n1\n\
101\nACDSRECORD\n1\nAcDbDs::ID\n90\n5\n2\nAcDbDs::HandleAttribute\n280\n7\n282\n1\n\
0\nACDSSCHEMA\n90\n2\n1\nAcDbDs::TreatedAsObjectDataSchema\n2\nAcDbDs::TreatedAsObjectData\n280\n1\n91\n0\n\
0\nACDSSCHEMA\n90\n3\n1\nAcDbDs::LegacySchema\n2\nAcDbDs::Legacy\n280\n1\n91\n0\n\
0\nACDSSCHEMA\n90\n4\n1\nAcDbDs::IndexedPropertySchema\n2\nAcDs:Indexable\n280\n1\n91\n0\n\
0\nACDSSCHEMA\n90\n5\n1\nAcDbDs::HandleAttributeSchema\n2\nAcDbDs::HandleAttribute\n280\n7\n91\n1\n284\n1\n";

/// Bytes of SAB data per 310 group.
const SAB_CHUNK: usize = 127;

/// Put the ACIS bodies (SAB) of the solids written in `tags` into the data store section, where
/// files of 2013 and later (`target`) keep them; the DWG bridge reads them there too. Each is an
/// `ACDSRECORD` bound to its solid by handle (320). A data store kept from the file read
/// ([`crate::dxf_raw::data_store`], already in `tags`) gets them in place of its records of these
/// solids, which are out of date; otherwise a new section goes before the end of the file. A solid
/// without a mesh (read where the kernel isn't available) has no new body and keeps its record.
pub(crate) fn data_store(tags: &mut Vec<Tag>, d: &Drawing, target: Version) {
    if target < Version::R2013 {
        return;
    }
    let norm = |s: &str| s.trim().to_ascii_uppercase();
    // The handles of the 3DSOLID records written.
    let mut written: HashSet<String> = HashSet::new();
    let mut in_solid = false;
    for t in tags.iter() {
        match t.code {
            0 => in_solid = t.str() == "3DSOLID",
            5 if in_solid => {
                written.insert(norm(&t.str()));
                in_solid = false;
            }
            _ => {}
        }
    }
    let bodies: Vec<(String, Vec<u8>)> = std::iter::once(&d.model)
        .chain(d.layouts.iter().map(|l| &l.entities))
        .chain(d.blocks.values().map(|b| &b.entities))
        .flat_map(|s| s.iter())
        .filter_map(|e| match &e.kind {
            EntityKind::Solid3d(s) if written.contains(&norm(&e.handle.hex())) => Some((e.handle.hex(), acis_body(s, e.handle)?.sab())),
            _ => None,
        })
        .collect();
    let ours: HashSet<String> = bodies.iter().map(|(h, _)| norm(h)).collect();
    // The kept section, taken out of `tags` (its groups after the name), or a new one's.
    let start = tags.windows(2).position(|w| w[0].code == 0 && w[0].str() == "SECTION" && w[1].code == 2 && w[1].str() == "ACDSDATA");
    let end = start.and_then(|s| tags.iter().skip(s).position(|t| t.code == 0 && t.str() == "ENDSEC").map(|k| s + k));
    let section: Vec<Tag> = match (start, end) {
        (Some(s), Some(e)) => tags.drain(s..=e).skip(2).take(e.saturating_sub(s + 2)).collect(),
        _ => cadcraft_dxf::parse_ascii(SCHEMAS).unwrap_or_default(),
    };
    // Its records of these solids go; the ASM data schema's number binds ours.
    let mut out: Vec<Tag> = Vec::with_capacity(section.len());
    let mut schema = 1;
    let mut i = 0;
    while i < section.len() {
        let next = section.iter().skip(i + 1).position(|t| t.code == 0).map_or(section.len(), |k| i + 1 + k);
        let rec = section.get(i..next).unwrap_or_default();
        let head = rec.first().filter(|t| t.code == 0).map(Tag::str).unwrap_or_default();
        if head == "ACDSSCHEMA" && rec.iter().any(|t| t.code == 1 && t.str() == "AcDb3DSolid_ASM_Data") {
            schema = rec.iter().find(|t| t.code == 90).map_or(1, Tag::i64);
        }
        if !(head == "ACDSRECORD" && rec.iter().any(|t| t.code == 320 && ours.contains(&norm(&t.str())))) {
            out.extend_from_slice(rec);
        }
        i = next;
    }
    for (h, sab) in bodies {
        out.extend([
            Tag::s(0, "ACDSRECORD"),
            Tag::i(90, schema),
            Tag::s(2, "AcDbDs::ID"),
            Tag::i(280, 10),
            Tag::s(320, h),
            Tag::s(2, "ASM_Data"),
            Tag::i(280, 15),
            Tag::i(94, i64::try_from(sab.len()).unwrap_or(i64::MAX)),
        ]);
        out.extend(sab.chunks(SAB_CHUNK).map(|c| Tag::new(310, Value::Hex(c.iter().map(|b| format!("{b:02X}")).collect()))));
    }
    if !out.iter().any(|t| t.code == 0 && t.str() == "ACDSRECORD") {
        return;
    }
    let at = start.unwrap_or_else(|| tags.iter().rposition(|t| t.code == 0 && t.str() == "EOF").unwrap_or(tags.len()));
    let section = [Tag::s(0, "SECTION"), Tag::s(2, "ACDSDATA")].into_iter().chain(out).chain([Tag::s(0, "ENDSEC")]);
    tags.splice(at..at, section);
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::{Drawing, Space};
    use cadcraft_geom::{Mat3, Vec2};

    use super::*;

    /// B-rep-like text: a long line, an empty one, leading and trailing spaces. It lacks the
    /// B-rep header, so the kernel (when built in) refuses it before parsing and the mesh stays
    /// empty: garbage behind a real header would reach OpenCASCADE's reader in-process here.
    fn brep() -> String {
        format!("B-rep-like text\n{}\n\n  0 0 \nLocations 0\n", "x".repeat(600))
    }

    fn drawing(brep: &str) -> (Drawing, Solid3d) {
        let part = cadcraft_doc::Part { name: "측판 Side".into(), material: "Birch plywood 18".into() };
        let mut s = Solid3d { brep: Arc::from(brep), part, ..Default::default() };
        s.transform(&Mat3::rotate_about(Vec2::new(1.0, 2.0), 0.5));
        let mut d = Drawing::default();
        d.add(&Space::Model, Default::default(), EntityKind::Solid3d(s.clone())).unwrap();
        (d, s)
    }

    fn solid(d: &Drawing) -> Solid3d {
        let got = d.model.iter().find_map(|e| match &e.kind {
            EntityKind::Solid3d(x) => Some(x.clone()),
            _ => None,
        });
        got.expect("a 3D solid")
    }

    #[test]
    fn solid_round_trips_through_dxf() {
        let (d, s) = drawing(&brep());
        let text = crate::write_dxf(&d);
        // No modeler data an ACIS reader would decode; one CADCRAFT xdata group with the marker.
        let rec = text.split("\r\n  0\r\n").find(|r| r.starts_with("3DSOLID")).unwrap();
        assert!(!rec.contains("Locations") && !rec.contains("\r\n  1\r\n"));
        assert_eq!(rec.matches("1001\r\nCADCRAFT\r\n").count(), 1);
        assert!(text.contains(&format!("{KEY}{}", d.model.iter().next().unwrap().handle.hex())));
        let back = crate::read_dxf(text.as_bytes()).unwrap();
        let got = solid(&back);
        assert_eq!(&*got.brep, brep().as_str());
        assert_eq!(got.xform, s.xform);
        assert_eq!(got.part, s.part, "the part's name and material");
        // Saved again, the record is written once (not also kept as another program's).
        assert_eq!(crate::write_dxf(&back).matches(KEY).count(), 1);
    }

    /// Files written before the XRECORD: the B-rep in the 3DSOLID's modeler data.
    #[test]
    fn legacy_solid_reads() {
        let (d, s) = drawing("");
        let mut tags = crate::dxf_write::tags(&d, Version::R2000);
        let rec = tags.iter().position(|t| t.code == 0 && t.str() == "3DSOLID").unwrap();
        let at = rec + tags.iter().skip(rec).position(|t| t.code == 70).unwrap();
        let old = brep();
        tags.splice(at + 1..at + 1, lines(&old));
        let got = solid(&crate::read_dxf(cadcraft_dxf::write_ascii(&tags).as_bytes()).unwrap());
        assert_eq!(&*got.brep, old.as_str());
        assert_eq!(got.xform, s.xform);
    }

    /// The B-rep and transform survive a DWG save (AutoCAD 2004 and 2018) and come back.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn solid_round_trips_through_dwg() {
        let (d, s) = drawing(&brep());
        for version in [Version::R2004, Version::R2018] {
            let dwg = crate::write_with(&d, "x.dwg", &crate::SaveOptions { version: Some(version), binary: false }).unwrap();
            let got = solid(&crate::read(&dwg, "x.dwg").unwrap());
            assert_eq!(&*got.brep, brep().as_str(), "{version:?}");
            assert_eq!(got.xform, s.xform, "{version:?}");
        }
    }

    #[test]
    fn entget_list_keeps_the_brep() {
        let (d, _) = drawing(&brep());
        let e = d.model.iter().next().unwrap();
        let (kind, tags) = crate::entity_groups(&d, e);
        let Some((_, EntityKind::Solid3d(got))) = crate::entity_from_groups(&kind, &tags) else { panic!("a 3D solid") };
        assert_eq!(&*got.brep, brep().as_str());
    }

    #[test]
    fn foreign_3dsolid_stays_unknown() {
        let tags = vec![Tag::s(100, "AcDbModelerGeometry"), Tag::i(70, 1), Tag::s(1, "acis data")];
        assert!(read(&tags).is_none());
    }

    /// A solid with a mesh (a 600 × 400 × 18 panel), turned and moved like [`drawing`]'s.
    fn panel() -> (Drawing, Solid3d) {
        let (_, mut s) = drawing(&brep());
        s.mesh = Arc::new(crate::acis::tests::cube(cadcraft_geom::Vec3::ZERO, cadcraft_geom::Vec3::new(600.0, 400.0, 18.0)));
        let mut d = Drawing::default();
        d.add(&Space::Model, Default::default(), EntityKind::Solid3d(s.clone())).unwrap();
        (d, s)
    }

    /// The SAT text of a 3DSOLID record's modeler data (groups 1 and 3), decoded.
    fn sat_of(rec: &[Tag]) -> String {
        let mut text = String::new();
        for t in rec.iter().skip_while(|t| t.code != 70).take_while(|t| t.code != 100 && t.code != 1001) {
            match t.code {
                1 if !text.is_empty() => text.push('\n'),
                1 | 3 => {}
                _ => continue,
            }
            text.extend(t.str().replace("^ ", "^").chars().map(|c| if ('!'..='~').contains(&c) { char::from(159 - c as u8) } else { c }));
        }
        text + "\n"
    }

    /// Each data store record's handle and SAB bytes.
    fn asm_data(tags: &[Tag]) -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> = Vec::new();
        let (mut asm, mut in_store) = (false, false);
        for t in tags {
            match (t.code, t.str().as_str()) {
                (2, "ACDSDATA") => in_store = true,
                (0, "ENDSEC") => in_store = false,
                (0, "ACDSRECORD") if in_store => out.push((String::new(), Vec::new())),
                (320, h) if in_store => out.last_mut().unwrap().0 = h.to_string(),
                (2, name) if in_store => asm = name == "ASM_Data",
                (310, hex) if in_store && asm => {
                    let bytes = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap());
                    out.last_mut().unwrap().1.extend(bytes);
                }
                _ => {}
            }
        }
        out
    }

    fn records(text: &[u8], kind: &str) -> Vec<Vec<Tag>> {
        cadcraft_dxf::records(&cadcraft_dxf::parse(text).unwrap()).into_iter().filter(|(k, _)| k == kind).map(|(_, t)| t).collect()
    }

    fn save(d: &Drawing, name: &str, version: Version) -> Vec<u8> {
        let bytes = crate::write_with(d, name, &crate::SaveOptions { version: Some(version), binary: false }).unwrap();
        // For black-box checks with other readers (ezdxf, a DWG validator).
        if let Ok(dir) = std::env::var("CADRHINO_ACIS_OUT") {
            let _ = std::fs::write(format!("{dir}/panel_{}.{}", version.year(), name.rsplit('.').next().unwrap()), &bytes);
        }
        bytes
    }

    /// Up to 2010 a 3DSOLID carries its ACIS body as SAT text in its modeler data; the B-rep read
    /// back is still ours, from the XRECORD.
    #[test]
    fn sat_body_in_the_solid_up_to_2010() {
        let (d, s) = panel();
        for version in [Version::R2000, Version::R2010] {
            let text = save(&d, "x.dxf", version);
            let rec = records(&text, "3DSOLID").pop().unwrap();
            assert!(rec.iter().any(|t| t.code == 1 && t.str().contains("^ ")), "'A' written as an escaped caret");
            assert!(rec.iter().filter(|t| matches!(t.code, 1 | 3)).all(|t| !t.str().ends_with('^')));
            let m = crate::acis::parse::sat(&sat_of(&rec));
            assert_eq!((m.version, m.count("face"), m.count("edge"), m.count("vertex")), (700, 6, 12, 8), "{version:?}");
            m.check();
            assert!(!String::from_utf8_lossy(&text).contains("ACDSDATA"));
            let got = solid(&crate::read_dxf(&text).unwrap());
            assert_eq!((&*got.brep, got.xform), (brep().as_str(), s.xform), "{version:?}");
        }
    }

    /// From 2013 the body is SAB in the data store, bound to the solid; saving the file read back
    /// replaces its record instead of adding another, and another program's records stay.
    #[test]
    fn sab_body_in_the_data_store_from_2013() {
        let (d, s) = panel();
        let text = save(&d, "x.dxf", Version::R2018);
        let rec = records(&text, "3DSOLID").pop().unwrap();
        let h = rec.iter().find(|t| t.code == 5).unwrap().str();
        assert!(rec.iter().any(|t| t.code == 290 && t.i64() == 1) && !rec.iter().any(|t| matches!(t.code, 1 | 3 | 70)));
        assert!(rec.iter().any(|t| t.code == 2 && t.str().starts_with("{CADC0000-")));
        let tags = cadcraft_dxf::parse(&text).unwrap();
        let store = asm_data(&tags);
        assert_eq!(store.len(), 1);
        assert_eq!(store[0].0, h);
        let m = crate::acis::parse::sab(&store[0].1);
        assert_eq!((m.version, m.count("face"), m.count("edge"), m.count("vertex")), (21800, 6, 12, 8));
        m.check();
        // Read back without the kernel, the solid has no mesh: its record is kept as read.
        let mut back = crate::read_dxf(&text).unwrap();
        assert_eq!(solid(&back).brep, s.brep);
        let resave = |d: &Drawing| cadcraft_dxf::parse(&crate::write_with(d, "x.dxf", &crate::SaveOptions::default()).unwrap()).unwrap();
        assert_eq!(asm_data(&resave(&back)), store, "kept");
        // Meshed (as the kernel does) and moved, its record is replaced by one where it now is.
        let handle = back.model.iter().next().unwrap().handle;
        back.modify_entity(handle, |e| {
            if let EntityKind::Solid3d(x) = &mut e.kind {
                x.mesh = s.mesh.clone();
                x.xform.m[2][3] += 100.0;
            }
        })
        .unwrap();
        let again = resave(&back);
        let moved = asm_data(&again);
        assert_eq!((moved.len(), &moved[0].0), (1, &h), "one record, rewritten");
        assert_ne!(moved[0].1, store[0].1);
        crate::acis::parse::sab(&moved[0].1).check();
        assert_eq!(again.iter().filter(|t| t.code == 2 && t.str() == "ACDSDATA").count(), 1);

        // Another program's solid with its record: both records in one section.
        let foreign = "0\nSECTION\n2\nHEADER\n9\n$ACADVER\n1\nAC1032\n9\n$HANDSEED\n5\n100\n0\nENDSEC\n\
             0\nSECTION\n2\nENTITIES\n0\n3DSOLID\n5\n2A\n330\n1F\n100\nAcDbEntity\n8\n0\n100\nAcDbModelerGeometry\n290\n0\n\
             2\n{00000000-0000-0000-0000-000000000000}\n100\nAcDb3dSolid\n350\n0\n0\nENDSEC\n\
             0\nSECTION\n2\nOBJECTS\n0\nDICTIONARY\n5\nC\n330\n0\n100\nAcDbDictionary\n0\nENDSEC\n\
             0\nSECTION\n2\nACDSDATA\n70\n2\n71\n2\n0\nACDSSCHEMA\n90\n1\n1\nAcDb3DSolid_ASM_Data\n2\nAcDbDs::ID\n280\n10\n91\n8\n\
             2\nASM_Data\n280\n15\n91\n0\n0\nACDSRECORD\n90\n1\n2\nAcDbDs::ID\n280\n10\n320\n2A\n2\nASM_Data\n280\n15\n94\n2\n310\nABCD\n\
             0\nENDSEC\n0\nEOF\n";
        let mut both = crate::read_dxf(foreign.as_bytes()).unwrap();
        both.add(&Space::Model, Default::default(), EntityKind::Solid3d(s)).unwrap();
        let tags = cadcraft_dxf::parse(&crate::write_with(&both, "x.dxf", &crate::SaveOptions::default()).unwrap()).unwrap();
        let store = asm_data(&tags);
        assert_eq!(store.len(), 2);
        assert_eq!(store[0], ("2A".to_string(), vec![0xAB, 0xCD]));
        assert_eq!(crate::acis::parse::sab(&store[1].1).count("face"), 6);
    }

    /// Through a DWG (the bridge, then its writer) the body arrives as SAT (2004) or SAB (2018)
    /// and comes back as it was written.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn acis_body_survives_a_dwg() {
        let (d, s) = panel();
        for version in [Version::R2004, Version::R2018] {
            let dwg = save(&d, "x.dwg", version);
            let dxf = cadcraft_dwg::dwg_to_dxf(&dwg).unwrap();
            let m = if version >= Version::R2013 {
                let store = asm_data(&cadcraft_dxf::parse(&dxf).unwrap());
                assert_eq!(store.len(), 1, "{version:?}");
                crate::acis::parse::sab(&store[0].1)
            } else {
                crate::acis::parse::sat(&sat_of(&records(&dxf, "3DSOLID").pop().unwrap()))
            };
            assert_eq!((m.count("face"), m.count("edge"), m.count("vertex")), (6, 12, 8), "{version:?}");
            m.check();
            let got = solid(&crate::read(&dwg, "x.dwg").unwrap());
            assert_eq!((&*got.brep, got.xform), (&*s.brep, s.xform), "{version:?}");
        }
    }
}
