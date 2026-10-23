//! XCLIP: the clip boundary of a block or xref reference (the DXF `SPATIAL_FILTER` under the
//! reference's `ACAD_FILTER` extension dictionary entry).
//!
//! It lives in the reference's own extended data under [`APP`], so every copy of the reference
//! (COPY, ARRAY, MIRROR, the clipboard, a block made from it) keeps its clip, as in AutoCAD. The
//! DXF writer turns it into a `SPATIAL_FILTER` and drops the xdata; the reader turns the filter
//! back into it.

use cadcraft_geom::{Bounds2, Vec2};

use crate::{Common, RawTag};

/// The extended-data application holding a reference's clip.
pub const APP: &str = "CADCRAFT_XCLIP";

/// Most boundary vertices kept (hostile files and command lines).
pub const MAX_VERTICES: usize = 10_000;

/// A block reference's clip boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct XClip {
    /// In the block's own coordinates (those of its definition's objects): two opposite corners
    /// of a rectangle, or three or more polygon vertices.
    pub boundary: Vec<Vec2>,
    /// Clipping is on (XCLIP ON/OFF).
    pub on: bool,
    /// Front and back clipping planes (Clipdepth): distances from the boundary's plane along the
    /// reference's normal, in block units.
    pub front: Option<f64>,
    pub back: Option<f64>,
}

impl XClip {
    /// The boundary as a closed polygon (a rectangle's four corners).
    pub fn polygon(&self) -> Vec<Vec2> {
        match self.boundary.as_slice() {
            [a, b] => vec![*a, Vec2::new(b.x, a.y), *b, Vec2::new(a.x, b.y)],
            vs => vs.to_vec(),
        }
    }

    /// The boundary's box in block coordinates.
    pub fn bounds(&self) -> Bounds2 {
        Bounds2::from_points(self.boundary.iter().copied())
    }

    /// The clip of a reference, from its extended data. `None` when it has none (or a damaged
    /// one: fewer than two finite vertices).
    pub fn of(c: &Common) -> Option<XClip> {
        if c.xdata.is_empty() {
            return None;
        }
        let start = c.xdata.iter().position(|t| t.code == 1001 && t.value.trim().eq_ignore_ascii_case(APP))?;
        let tags = c.xdata.get(start + 1..)?;
        let tags = tags.get(..tags.iter().position(|t| t.code == 1001).unwrap_or(tags.len()))?;
        let num = |t: &RawTag| t.value.trim().parse::<f64>().ok().filter(|v| v.is_finite());
        let flags = tags.iter().find(|t| t.code == 1070).and_then(|t| t.value.trim().parse::<i64>().ok()).unwrap_or(1);
        let reals: Vec<f64> = tags.iter().filter(|t| t.code == 1040).filter_map(num).collect();
        let mut boundary = Vec::new();
        let mut x = None;
        for t in tags {
            match t.code {
                1010 => x = num(t),
                1020 => {
                    if let (Some(px), Some(py)) = (x.take(), num(t))
                        && boundary.len() < MAX_VERTICES
                    {
                        boundary.push(Vec2::new(px, py));
                    }
                }
                _ => {}
            }
        }
        (boundary.len() >= 2).then(|| XClip {
            boundary,
            on: flags & 1 != 0,
            front: (flags & 2 != 0).then(|| reals.first().copied()).flatten(),
            back: (flags & 4 != 0).then(|| reals.get(1).copied()).flatten(),
        })
    }

    /// Give `c` this clip (`None`: remove it), keeping other applications' extended data.
    pub fn put(c: &mut Common, clip: Option<&XClip>) {
        if let Some(start) = c.xdata.iter().position(|t| t.code == 1001 && t.value.trim().eq_ignore_ascii_case(APP)) {
            let end = c.xdata.iter().skip(start + 1).position(|t| t.code == 1001).map_or(c.xdata.len(), |n| start + 1 + n);
            c.xdata.drain(start..end);
        }
        let Some(x) = clip else { return };
        let tag = |code: i32, value: String| RawTag { code, value };
        let flags = i64::from(x.on) | if x.front.is_some() { 2 } else { 0 } | if x.back.is_some() { 4 } else { 0 };
        c.xdata.push(tag(1001, APP.into()));
        c.xdata.push(tag(1070, flags.to_string()));
        c.xdata.push(tag(1040, x.front.unwrap_or(0.0).to_string()));
        c.xdata.push(tag(1040, x.back.unwrap_or(0.0).to_string()));
        for p in x.boundary.iter().take(MAX_VERTICES) {
            c.xdata.push(tag(1010, p.x.to_string()));
            c.xdata.push(tag(1020, p.y.to_string()));
            c.xdata.push(tag(1030, "0".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clip_round_trips_through_xdata_beside_other_apps() {
        let mut c = Common::default();
        c.xdata.push(RawTag { code: 1001, value: "OTHER".into() });
        c.xdata.push(RawTag { code: 1000, value: "kept".into() });
        let x = XClip { boundary: vec![Vec2::new(-1.5, 2.0), Vec2::new(4.0, 7.25)], on: true, front: None, back: Some(3.0) };
        XClip::put(&mut c, Some(&x));
        assert_eq!(XClip::of(&c), Some(x.clone()));
        assert_eq!(x.polygon().len(), 4);
        // Replacing it keeps one copy; removing it keeps the other application's data.
        let off = XClip { on: false, ..x };
        XClip::put(&mut c, Some(&off));
        assert_eq!(XClip::of(&c), Some(off));
        XClip::put(&mut c, None);
        assert_eq!(XClip::of(&c), None);
        assert_eq!(c.xdata.len(), 2);
        // A damaged clip (one vertex, junk numbers) reads as none.
        c.xdata.extend([(1001, APP), (1010, "nan"), (1020, "1"), (1010, "1"), (1020, "2")].map(|(code, v)| RawTag { code, value: v.into() }));
        assert_eq!(XClip::of(&c), None);
    }
}
