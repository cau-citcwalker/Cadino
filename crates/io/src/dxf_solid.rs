//! 3D solids in DXF. AutoCAD's 3DSOLID carries ACIS modeler data, which CAD-rhino doesn't
//! write. Ours is a 3DSOLID whose modeler-data lines (groups 1, continued in 3) are the
//! OpenCASCADE B-rep text, with `CADCRAFT` xdata `1000 SOLID3D` saying so and holding the
//! model → world transform (16 reals, row by row). The mesh is rebuilt from the B-rep when the
//! file is read. Other 3DSOLIDs stay unknown objects (kept as read).

use std::sync::Arc;

use cadcraft_doc::{EntityKind, Solid3d};
use cadcraft_dxf::Tag;
use cadcraft_geom::Mat4;

use crate::dxf_ext::{APP, MAX_PAYLOAD, xdata};

const MARK: &str = "SOLID3D";
/// Longest string written in one group.
const CHUNK: usize = 250;

/// The groups after the common entity groups, xdata last.
pub(crate) fn tags(s: &Solid3d) -> Vec<Tag> {
    let mut out = vec![Tag::i(70, 1)];
    for line in s.brep.lines() {
        let chars: Vec<char> = line.chars().collect();
        let mut chunks = chars.chunks(CHUNK);
        out.push(Tag::s(1, chunks.next().map(|c| c.iter().collect::<String>()).unwrap_or_default()));
        out.extend(chunks.map(|c| Tag::s(3, c.iter().collect::<String>())));
    }
    out.push(Tag::s(100, "AcDb3dSolid"));
    out.extend([Tag::s(1001, APP), Tag::s(1000, MARK)]);
    out.extend(s.xform.m.iter().flatten().map(|v| Tag::f(1040, *v)));
    out
}

/// A 3DSOLID CAD-rhino wrote; `None` for others.
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
    let mut brep = String::new();
    for t in tags.get(start..).unwrap_or(&[]).iter().take_while(|t| t.code != 1001) {
        match t.code {
            1 => {
                if !brep.is_empty() {
                    brep.push('\n');
                }
                brep.push_str(&t.str());
            }
            3 => brep.push_str(&t.str()),
            _ => {}
        }
        if brep.len() > MAX_PAYLOAD {
            return None;
        }
    }
    brep.push('\n');
    // Without the kernel (the web build) the solid is kept but has nothing to draw.
    let mesh = cadcraft_kernel::tessellate(&brep).unwrap_or_default();
    Some(EntityKind::Solid3d(Solid3d { brep: Arc::from(brep), mesh: Arc::new(mesh), xform }))
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::{Drawing, Space};
    use cadcraft_geom::{Mat3, Vec2};

    use super::*;

    #[test]
    fn solid_round_trips_through_dxf() {
        // A long line and short ones: groups 1 and 3 must rebuild them exactly. (Not real B-rep
        // text, so a build with the kernel refuses to mesh it and the mesh stays empty.)
        let brep = format!("first line\n{}\nLocations 0\n", "x".repeat(600));
        let mut s = Solid3d { brep: Arc::from(brep.as_str()), ..Default::default() };
        s.transform(&Mat3::rotate_about(Vec2::new(1.0, 2.0), 0.5));
        let mut d = Drawing::default();
        d.add(&Space::Model, Default::default(), EntityKind::Solid3d(s.clone())).unwrap();
        let back = crate::read_dxf(crate::write_dxf(&d).as_bytes()).unwrap();
        let got = back.model.iter().find_map(|e| match &e.kind {
            EntityKind::Solid3d(x) => Some(x.clone()),
            _ => None,
        });
        let got = got.expect("a 3D solid");
        assert_eq!(&*got.brep, brep.as_str());
        for (r, q) in got.xform.m.iter().zip(s.xform.m.iter()) {
            for (a, b) in r.iter().zip(q) {
                assert!((a - b).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn foreign_3dsolid_stays_unknown() {
        let tags = vec![Tag::s(100, "AcDbModelerGeometry"), Tag::i(70, 1), Tag::s(1, "acis data")];
        assert!(read(&tags).is_none());
    }
}
