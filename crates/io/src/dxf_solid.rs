//! 3D solids in DXF. AutoCAD's 3DSOLID carries ACIS modeler data, which CAD-rhino doesn't
//! write. Ours is a 3DSOLID with no modeler data (format `70 1`, no lines) and `CADCRAFT` xdata
//! `1000 SOLID3D` holding the model → world transform (16 reals, row by row). The OpenCASCADE
//! B-rep text is in an XRECORD of the named object dictionary, `CADCRAFT_BREP_<solid handle>`:
//! a group 1 per line, continued in groups 3. (Files written before it kept those lines in the
//! 3DSOLID's own modeler data; they are still read.) The mesh is rebuilt from the B-rep when the
//! file is read. Other 3DSOLIDs stay unknown objects (kept as read).

use std::collections::HashMap;
use std::sync::Arc;

use cadcraft_doc::{Entity, EntityKind, Handle, Solid3d};
use cadcraft_dxf::{Tag, Version};
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
/// up in: `AcDb3dSolid` and its history handle came with 2007.
pub(crate) fn tags(s: &Solid3d, target: Version) -> Vec<Tag> {
    let mut out = vec![Tag::i(70, 1)];
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
    // A DWG converted back to DXF has an empty line of modeler data. The mesh comes later
    // ([`mesh_all`]).
    let brep = if brep.trim().is_empty() { String::new() } else { brep };
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
}
