//! Curves for cross sections, profiles and paths, and the geometry SWEEP works out before the
//! kernel: the profile placed on the path, and the path split at its corners into stretches the
//! kernel sweeps one by one and joins with mitres.

use std::f64::consts::{PI, TAU};

use cadcraft_geom::Vec3;
use serde::{Deserialize, Serialize};

use crate::{KernelError, Result, Seg};

/// Points closer than this are one point to the kernel (OpenCASCADE's precision).
pub(crate) const JOIN: f64 = 1e-7;

/// A cross section, profile or path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Curve {
    /// Lines and arcs joined end to end, or one circle.
    Segs(Vec<Seg>),
    /// The smooth curve through `points` (splines, helices), leaving the first along the first
    /// tangent and reaching the last along the second when they are given.
    Spline { points: Vec<Vec3>, tangents: Option<(Vec3, Vec3)> },
}

/// The ends of a piece; `None` for a circle.
pub(crate) fn ends(s: &Seg) -> Option<(Vec3, Vec3)> {
    match *s {
        Seg::Line { a, b } | Seg::Arc { a, b, .. } => Some((a, b)),
        Seg::Circle { .. } => None,
    }
}

/// Whether the pieces close on themselves (a lone circle does).
pub(crate) fn closed(segs: &[Seg]) -> bool {
    match segs {
        [Seg::Circle { .. }] => true,
        [first, .., last] => matches!((ends(first), ends(last)), (Some((a, _)), Some((_, b))) if (a - b).len() <= JOIN),
        _ => false,
    }
}

pub(crate) fn unit(v: Vec3) -> Option<Vec3> {
    let l = v.len();
    (l.is_finite() && l > 0.0).then(|| v * (1.0 / l))
}

/// Newell's normal of the points taken as a loop about `o`: along the loop's normal, twice the
/// area it encloses long.
pub(crate) fn newell(pts: &[Vec3], o: Vec3) -> Vec3 {
    pts.iter().zip(pts.iter().cycle().skip(1)).fold(Vec3::ZERO, |n, (p, q)| n + (*p - o).cross(*q - o))
}

/// The circle through an arc's three points: its centre, and its normal (not unit) about which
/// a → mid → b turns counterclockwise. `None` when the points are in a line.
pub(crate) fn circumcentre(a: Vec3, mid: Vec3, b: Vec3) -> Option<(Vec3, Vec3)> {
    let (u, v) = (a - b, mid - b);
    let k = u.cross(v);
    let kk = k.dot(k);
    if !kk.is_finite() || kk <= 0.0 {
        return None;
    }
    Some((b + (v * u.dot(u) - u * v.dot(v)).cross(k) * (0.5 / kk), k))
}

/// An arc's centre, unit normal (see [`circumcentre`]) and the angle it turns through.
fn arc_turn(a: Vec3, mid: Vec3, b: Vec3) -> Option<(Vec3, Vec3, f64)> {
    let (c, k) = circumcentre(a, mid, b)?;
    let k = unit(k)?;
    let (x, e) = (a - c, b - c);
    Some((c, k, k.cross(x).dot(e).atan2(x.dot(e)).rem_euclid(TAU)))
}

/// The unit directions a piece leaves its start in and reaches its end in; `None` for a circle.
fn tangents(s: &Seg) -> Option<(Vec3, Vec3)> {
    match *s {
        Seg::Line { a, b } => unit(b - a).map(|t| (t, t)),
        Seg::Arc { a, mid, b } => {
            let (c, k, _) = arc_turn(a, mid, b)?;
            Some((unit(k.cross(a - c))?, unit(k.cross(b - c))?))
        }
        Seg::Circle { .. } => None,
    }
}

impl Curve {
    /// The points that define it: piece ends and arc middles, a circle's centre, a spline's points.
    pub fn points(&self) -> Vec<Vec3> {
        match self {
            Curve::Segs(segs) => {
                let mut out = Vec::with_capacity(segs.len() * 2);
                for s in segs {
                    match *s {
                        Seg::Line { a, b } => out.extend([a, b]),
                        Seg::Arc { a, mid, b } => out.extend([a, mid, b]),
                        Seg::Circle { center, .. } => out.push(center),
                    }
                }
                out
            }
            Curve::Spline { points, .. } => points.clone(),
        }
    }

    pub fn is_closed(&self) -> bool {
        match self {
            Curve::Segs(s) => closed(s),
            Curve::Spline { points, .. } => {
                points.len() > 2 && matches!((points.first(), points.last()), (Some(a), Some(b)) if (*a - *b).len() <= JOIN)
            }
        }
    }

    /// The first and last points; `None` for a circle.
    pub(crate) fn ends(&self) -> Option<(Vec3, Vec3)> {
        match self {
            Curve::Segs(s) => Some((ends(s.first()?)?.0, ends(s.last()?)?.1)),
            Curve::Spline { points, .. } => Some((*points.first()?, *points.last()?)),
        }
    }

    /// The same curve run the other way.
    pub(crate) fn reversed(&self) -> Curve {
        match self {
            Curve::Segs(s) => Curve::Segs(
                s.iter()
                    .rev()
                    .map(|s| match *s {
                        Seg::Line { a, b } => Seg::Line { a: b, b: a },
                        Seg::Arc { a, mid, b } => Seg::Arc { a: b, mid, b: a },
                        Seg::Circle { center, radius, normal } => Seg::Circle { center, radius, normal: -normal },
                    })
                    .collect(),
            ),
            Curve::Spline { points, tangents } => {
                Curve::Spline { points: points.iter().rev().copied().collect(), tangents: tangents.map(|(a, b)| (-b, -a)) }
            }
        }
    }

    /// Where it starts and the unit direction it leaves in; `None` for a circle.
    pub(crate) fn start(&self) -> Option<(Vec3, Vec3)> {
        match self {
            Curve::Segs(s) => {
                let f = s.first()?;
                Some((ends(f)?.0, tangents(f)?.0))
            }
            Curve::Spline { points, tangents } => {
                let p = *points.first()?;
                let t = tangents.map(|t| t.0).and_then(unit).or_else(|| unit(*points.get(1)? - p))?;
                Some((p, t))
            }
        }
    }

    /// Moved: `f` takes each point, `r` each direction.
    pub(crate) fn map(&self, f: &impl Fn(Vec3) -> Vec3, r: &impl Fn(Vec3) -> Vec3) -> Curve {
        match self {
            Curve::Segs(s) => Curve::Segs(
                s.iter()
                    .map(|s| match *s {
                        Seg::Line { a, b } => Seg::Line { a: f(a), b: f(b) },
                        Seg::Arc { a, mid, b } => Seg::Arc { a: f(a), mid: f(mid), b: f(b) },
                        Seg::Circle { center, radius, normal } => Seg::Circle { center: f(center), radius, normal: r(normal) },
                    })
                    .collect(),
            ),
            Curve::Spline { points, tangents } => {
                Curve::Spline { points: points.iter().map(|p| f(*p)).collect(), tangents: tangents.map(|(a, b)| (r(a), r(b))) }
            }
        }
    }
}

/// A rotation: a 3×3 matrix by rows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rot {
    x: Vec3,
    y: Vec3,
    z: Vec3,
}

impl Rot {
    pub(crate) const ID: Rot = Rot { x: Vec3::new(1.0, 0.0, 0.0), y: Vec3::new(0.0, 1.0, 0.0), z: Vec3::Z };

    /// About the unit axis `k` by `angle`, counterclockwise looking down `k` (Rodrigues).
    pub(crate) fn about(k: Vec3, angle: f64) -> Rot {
        let (s, c) = angle.sin_cos();
        let t = 1.0 - c;
        Rot {
            x: Vec3::new(t * k.x * k.x + c, t * k.x * k.y - s * k.z, t * k.x * k.z + s * k.y),
            y: Vec3::new(t * k.x * k.y + s * k.z, t * k.y * k.y + c, t * k.y * k.z - s * k.x),
            z: Vec3::new(t * k.x * k.z - s * k.y, t * k.y * k.z + s * k.x, t * k.z * k.z + c),
        }
    }

    /// The smallest rotation taking unit `a` onto unit `b`.
    pub(crate) fn between(a: Vec3, b: Vec3) -> Rot {
        let axis = a.cross(b);
        let s = axis.len();
        if s > 1e-12 {
            return Rot::about(axis * (1.0 / s), s.atan2(a.dot(b)));
        }
        if a.dot(b) > 0.0 {
            return Rot::ID;
        }
        // Opposite: half a turn about any axis square to `a`.
        let p = if a.x.abs() < 0.9 { Vec3::new(1.0, 0.0, 0.0) } else { Vec3::new(0.0, 1.0, 0.0) };
        unit(a.cross(p)).map_or(Rot::ID, |k| Rot::about(k, PI))
    }

    pub(crate) fn apply(&self, v: Vec3) -> Vec3 {
        Vec3::new(self.x.dot(v), self.y.dot(v), self.z.dot(v))
    }

    /// This rotation, then `o`.
    pub(crate) fn then(&self, o: &Rot) -> Rot {
        let row = |r: Vec3| self.x * r.x + self.y * r.y + self.z * r.z;
        Rot { x: row(o.x), y: row(o.y), z: row(o.z) }
    }
}

/// The centre of the points' bounding box.
fn box_centre(pts: &[Vec3]) -> Option<Vec3> {
    let first = *pts.first()?;
    let (lo, hi) = pts.iter().fold((first, first), |(lo, hi), p| {
        (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
    });
    Some((lo + hi) * 0.5)
}

/// The normal of the plane the points lie in; for points in a line, of the plane through them
/// nearest to horizontal (a vertical line's: the one facing X).
fn plane_normal(pts: &[Vec3]) -> Option<Vec3> {
    let o = *pts.first()?;
    let far = pts.iter().copied().fold(o, |f, p| if (p - o).len() > (f - o).len() { p } else { f });
    let reach = (far - o).len();
    let n = newell(pts, o);
    if n.len() > 1e-12 * reach * reach {
        return unit(n);
    }
    let d = unit(far - o)?;
    let up = if d.cross(Vec3::Z).len() > 1e-9 { Vec3::Z } else { Vec3::new(1.0, 0.0, 0.0) };
    unit(d.cross(up.cross(d)))
}

/// The profile moved so the centre of its points' box is on `at`, turned so its plane stands
/// square to the unit direction `dir` (as SWEEP places it by default), and how far it reaches
/// from `at` at most.
/// ponytail: the box centre of the defining points, not the area centroid; a base point option
/// if profiles need placing elsewhere.
pub(crate) fn align(profile: &Curve, at: Vec3, dir: Vec3) -> Option<(Curve, f64)> {
    let pts = profile.points();
    let circle = match profile {
        Curve::Segs(s) => match s.as_slice() {
            [Seg::Circle { center, radius, normal }] => Some((*center, *normal, *radius)),
            _ => None,
        },
        Curve::Spline { .. } => None,
    };
    let (centre, normal, radius) = match circle {
        Some((c, n, r)) => (c, unit(n)?, r),
        None => (box_centre(&pts)?, plane_normal(&pts)?, 0.0),
    };
    let r = Rot::between(if normal.dot(dir) < 0.0 { -normal } else { normal }, dir);
    Some((profile.map(&|p| at + r.apply(p - centre), &|v| r.apply(v)), reach(profile, centre).max(radius)))
}

/// How far the curve reaches from `centre` at most: exact for lines and circles; an arc up to a
/// half circle lies in the circle on its chord, a longer one in its own circle; a spline's fit
/// may bulge between its points, so twice its furthest point.
fn reach(c: &Curve, centre: Vec3) -> f64 {
    let d = |p: Vec3| (p - centre).len();
    match c {
        Curve::Segs(segs) => segs
            .iter()
            .map(|s| match *s {
                Seg::Line { a, b } => d(a).max(d(b)),
                Seg::Arc { a, mid, b } => match arc_turn(a, mid, b) {
                    Some((c, _, turn)) if turn > PI => d(c) + (a - c).len(),
                    _ => d((a + b) * 0.5) + (b - a).len() * 0.5,
                },
                Seg::Circle { center, radius, .. } => d(center) + radius,
            })
            .fold(0.0, f64::max),
        Curve::Spline { points, .. } => points.iter().map(|p| 2.0 * d(*p)).fold(0.0, f64::max),
    }
}

/// One stretch of a sweep path without corners: the profile where it starts, its path, and where
/// it is trimmed to meet its neighbours at corners.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Run {
    pub profile: Curve,
    pub path: Curve,
    pub cuts: Vec<Cut>,
}

/// A stretch's trim at a corner: what lies past the mitre plane where the stretch overshoots
/// the corner (its extension and its inner side just before the corner) is cut away; the rest of
/// the stretch stays, even where it comes back across the plane further away.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Cut {
    /// The corner.
    pub at: Vec3,
    /// The mitre plane's normal, pointing to the side cut away.
    pub normal: Vec3,
    /// The unit direction the stretch runs on in past the corner.
    pub along: Vec3,
    /// How far the profile reaches from the path.
    pub reach: f64,
    /// How far the stretch runs on past the corner.
    pub past: f64,
}

/// A turn between pieces sharper than this is a corner (radians).
const CORNER: f64 = 1e-6;
/// The sharpest corner a path may turn (radians): past it the mitre runs off far away.
const MAX_TURN: f64 = 175.0 * PI / 180.0;

/// The corner where a path arriving along `a` leaves along `b` (unit): the mitre plane's
/// normal (along the path) and the angle turned; `None` when the path runs on smoothly.
fn corner(a: Vec3, b: Vec3) -> Result<Option<(Vec3, f64)>> {
    let turn = a.dot(b).clamp(-1.0, 1.0).acos();
    if turn <= CORNER {
        return Ok(None);
    }
    if turn > MAX_TURN {
        return Err(KernelError::Invalid("the path turns back on itself".into()));
    }
    Ok(unit(a + b).map(|m| (m, turn)))
}

/// SWEEP's plan: the profile placed square to the start of the path, and the path split at its
/// corners into stretches, each carrying the profile as it arrives there and lengthened past
/// the corners to be trimmed by the mitre planes, so neighbours meet face to face. Along a
/// stretch the profile keeps its angle to the path (OpenCASCADE's corrected Frenet frame: no turn
/// along lines, a turn about the axis along arcs).
/// ponytail: mitred corners only (AutoCAD's default); a closed 3D path that twists can leave its
/// closing mitre slightly mismatched. A trim takes in a box round the overshoot (two profile
/// reaches across), so a stretch that comes back inside that box past the mitre loses that part;
/// a cutter swept from the overshoot itself if paths that tight matter.
pub(crate) fn sweep_runs(profile: &Curve, path: &Curve) -> Result<Vec<Run>> {
    let bad = |m: &str| KernelError::Invalid(m.into());
    let (p0, t0) = path.start().ok_or_else(|| bad("the path needs a start (a circle path is two arcs)"))?;
    let (prof, reach) = align(profile, p0, t0).ok_or_else(|| bad("the profile has no plane"))?;
    let Curve::Segs(segs) = path else {
        return Ok(vec![Run { profile: prof, path: path.clone(), cuts: Vec::new() }]);
    };
    let piece = |s: &Seg| ends(s).zip(tangents(s)).ok_or_else(|| bad("the path has a piece with no direction"));
    // Closed at a corner: the first and last stretches meet in a mitre too.
    let closing = match segs.last().map(piece).transpose()? {
        Some((_, (_, t))) if path.is_closed() => corner(t, t0)?.map(|c| (p0, c)),
        _ => None,
    };
    // A stretch as the kernel sweeps it, from its pieces, the profile's turn since the path's
    // start, and the corners (point, mitre) it starts and ends at.
    let finish = |run: Vec<Seg>, frame: Rot, start: Option<(Vec3, (Vec3, f64))>, end: Option<(Vec3, (Vec3, f64))>| -> Result<Run> {
        let (first, last) = (run.first().ok_or_else(|| bad("empty path"))?, run.last().ok_or_else(|| bad("empty path"))?);
        let (((s0, _), (ts, _)), ((_, q), (_, te))) = (piece(first)?, piece(last)?);
        let past = |turn: f64| reach * (turn / 2.0).tan() + reach;
        let (mut path, mut cuts, mut from) = (Vec::with_capacity(run.len() + 2), Vec::new(), s0);
        if let Some((p, (m, turn))) = start {
            from = s0 - ts * past(turn);
            path.push(Seg::Line { a: from, b: s0 });
            cuts.push(Cut { at: p, normal: -m, along: -ts, reach, past: past(turn) });
        }
        path.extend(run);
        if let Some((p, (m, turn))) = end {
            path.push(Seg::Line { a: q, b: q + te * past(turn) });
            cuts.push(Cut { at: p, normal: m, along: te, reach, past: past(turn) });
        }
        Ok(Run { profile: prof.map(&|p| from + frame.apply(p - p0), &|v| frame.apply(v)), path: Curve::Segs(path), cuts })
    };
    let mut runs = Vec::new();
    let (mut run, mut frame, mut start) = (Vec::new(), Rot::ID, closing);
    let mut run_frame = frame;
    let mut prev: Option<Vec3> = None;
    for s in segs {
        let ((a, _), (ta, tb)) = piece(s)?;
        if let Some(p) = prev
            && let Some(c) = corner(p, ta)?
        {
            runs.push(finish(std::mem::take(&mut run), run_frame, start, Some((a, c)))?);
            frame = frame.then(&Rot::between(p, ta));
            (run_frame, start) = (frame, Some((a, c)));
        }
        run.push(*s);
        if let Seg::Arc { a, mid, b } = *s
            && let Some((_, k, turn)) = arc_turn(a, mid, b)
        {
            frame = frame.then(&Rot::about(k, turn));
        }
        prev = Some(tb);
    }
    runs.push(finish(run, run_frame, start, closing)?);
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: Vec3, b: Vec3) -> bool {
        (a - b).len() < 1e-9
    }

    fn line(a: [f64; 3], b: [f64; 3]) -> Seg {
        Seg::Line { a: Vec3::new(a[0], a[1], a[2]), b: Vec3::new(b[0], b[1], b[2]) }
    }

    #[test]
    fn rotations_compose_and_turn_vectors() {
        let x = Vec3::new(1.0, 0.0, 0.0);
        let y = Vec3::new(0.0, 1.0, 0.0);
        assert!(near(Rot::about(Vec3::Z, PI / 2.0).apply(x), y));
        assert!(near(Rot::between(x, Vec3::Z).apply(x), Vec3::Z));
        assert!(near(Rot::between(x, -x).apply(x), -x), "half a turn");
        // A quarter turn about Z, then one about X: x → y → z.
        let r = Rot::about(Vec3::Z, PI / 2.0).then(&Rot::about(x, PI / 2.0));
        assert!(near(r.apply(x), Vec3::Z));
    }

    #[test]
    fn arcs_know_their_turn_and_tangents() {
        // A quarter circle about the origin from (1, 0) to (0, 1), counterclockwise.
        let h = std::f64::consts::FRAC_1_SQRT_2;
        let (c, k, turn) = arc_turn(Vec3::new(1.0, 0.0, 0.0), Vec3::new(h, h, 0.0), Vec3::new(0.0, 1.0, 0.0)).unwrap();
        assert!(near(c, Vec3::ZERO) && near(k, Vec3::Z) && (turn - PI / 2.0).abs() < 1e-12);
        let arc = Seg::Arc { a: Vec3::new(1.0, 0.0, 0.0), mid: Vec3::new(h, h, 0.0), b: Vec3::new(0.0, 1.0, 0.0) };
        let (ta, tb) = tangents(&arc).unwrap();
        assert!(near(ta, Vec3::new(0.0, 1.0, 0.0)) && near(tb, Vec3::new(-1.0, 0.0, 0.0)));
        // Three quarters the other way round.
        let (_, _, turn) = arc_turn(Vec3::new(1.0, 0.0, 0.0), Vec3::new(-h, -h, 0.0), Vec3::new(0.0, 1.0, 0.0)).unwrap();
        assert!((turn - 1.5 * PI).abs() < 1e-12);
    }

    #[test]
    fn a_profile_is_placed_square_to_the_path() {
        // A circle on the XY plane at (5, 5), swept along +X from (0, 0, 0).
        let c = Curve::Segs(vec![Seg::Circle { center: Vec3::new(5.0, 5.0, 0.0), radius: 2.0, normal: Vec3::Z }]);
        let (placed, reach) = align(&c, Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0)).unwrap();
        let Curve::Segs(s) = placed else { panic!() };
        let [Seg::Circle { center, normal, .. }] = s.as_slice() else { panic!("{s:?}") };
        assert!(near(*center, Vec3::ZERO) && near(*normal, Vec3::new(1.0, 0.0, 0.0)));
        assert_eq!(reach, 2.0);
        // A square reaches its corners; a half disc its arc (not twice its points).
        let sq = Curve::Segs(
            (0..4)
                .map(|i| {
                    let c = [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 2.0, 0.0], [0.0, 2.0, 0.0]];
                    line(c[i], c[(i + 1) % 4])
                })
                .collect(),
        );
        assert!((align(&sq, Vec3::ZERO, Vec3::Z).unwrap().1 - 2.0_f64.sqrt()).abs() < 1e-12);
        let half = Curve::Segs(vec![
            Seg::Arc { a: Vec3::new(-1.0, 0.0, 0.0), mid: Vec3::new(0.0, -1.0, 0.0), b: Vec3::new(1.0, 0.0, 0.0) },
            line([1.0, 0.0, 0.0], [-1.0, 0.0, 0.0]),
        ]);
        assert!((super::reach(&half, Vec3::ZERO) - 1.0).abs() < 1e-12);
        let most = Seg::Arc { a: Vec3::new(0.0, 1.0, 0.0), mid: Vec3::new(0.0, -1.0, 0.0), b: Vec3::new(1.0, 0.0, 0.0) };
        assert!((super::reach(&Curve::Segs(vec![most]), Vec3::new(1.0, 0.0, 0.0)) - 2.0).abs() < 1e-12, "three quarters of a circle");
        // A line on the XY plane, swept up: it stays flat, centred on the path.
        let l = Curve::Segs(vec![line([0.0, 0.0, 0.0], [4.0, 0.0, 0.0])]);
        let (placed, _) = align(&l, Vec3::new(10.0, 0.0, 0.0), Vec3::Z).unwrap();
        assert_eq!(placed.ends().map(|(a, b)| (a.z, b.z)), Some((0.0, 0.0)));
        assert!(near(placed.points()[0], Vec3::new(8.0, 0.0, 0.0)), "{placed:?}");
    }

    #[test]
    fn sweep_paths_split_at_corners_with_mitres() {
        let prof = Curve::Segs(vec![Seg::Circle { center: Vec3::ZERO, radius: 1.0, normal: Vec3::Z }]);
        // An L: along +X, then +Y.
        let path = Curve::Segs(vec![line([0.0, 0.0, 0.0], [10.0, 0.0, 0.0]), line([10.0, 0.0, 0.0], [10.0, 10.0, 0.0])]);
        let runs = sweep_runs(&prof, &path).unwrap();
        assert_eq!(runs.len(), 2);
        let h = std::f64::consts::FRAC_1_SQRT_2;
        let m = Vec3::new(h, h, 0.0);
        let corner = Vec3::new(10.0, 0.0, 0.0);
        // The first stretch runs on past the corner and loses what is beyond the mitre.
        let cut = |r: &Run| r.cuts.iter().map(|c| (c.at, c.normal)).collect::<Vec<_>>();
        // Trimmed only near the corner, where it runs on: the profile's reach is its radius (1),
        // and the extension runs past the corner that far beyond the outer edge's mitre (1 + 1).
        assert!(runs.iter().flat_map(|r| &r.cuts).all(|c| c.reach == 1.0 && (c.past - 2.0).abs() < 1e-9), "{runs:?}");
        assert!(near(runs[0].cuts[0].along, Vec3::new(1.0, 0.0, 0.0)) && near(runs[1].cuts[0].along, Vec3::new(0.0, -1.0, 0.0)));
        assert!(matches!(cut(&runs[0]).as_slice(), [(p, n)] if near(*p, corner) && near(*n, m)), "{:?}", runs[0].cuts);
        let Curve::Segs(p) = &runs[0].path else { panic!() };
        assert_eq!(p.len(), 2);
        assert!(ends(&p[1]).is_some_and(|(_, b)| b.x > 10.0 && b.y == 0.0));
        // The second starts before the corner with the profile turned to +Y.
        assert!(matches!(cut(&runs[1]).as_slice(), [(p, n)] if near(*p, corner) && near(*n, -m)), "{:?}", runs[1].cuts);
        let Curve::Segs(p) = &runs[1].path else { panic!() };
        let start = ends(&p[0]).unwrap().0;
        assert!(start.x == 10.0 && start.y < 0.0);
        let Curve::Segs(c) = &runs[1].profile else { panic!() };
        let [Seg::Circle { center, normal, .. }] = c.as_slice() else { panic!() };
        assert!(near(*center, start) && near(*normal, Vec3::new(0.0, 1.0, 0.0)), "{c:?}");
        // A smooth path is one stretch; one that doubles back is refused.
        let h2 = Seg::Arc { a: Vec3::new(10.0, 0.0, 0.0), mid: Vec3::new(10.0 + 5.0 * h, 5.0 - 5.0 * h, 0.0), b: Vec3::new(15.0, 5.0, 0.0) };
        let smooth = Curve::Segs(vec![line([0.0, 0.0, 0.0], [10.0, 0.0, 0.0]), h2]);
        assert_eq!(sweep_runs(&prof, &smooth).unwrap().len(), 1);
        let back = Curve::Segs(vec![line([0.0, 0.0, 0.0], [10.0, 0.0, 0.0]), line([10.0, 0.0, 0.0], [0.0, 0.01, 0.0])]);
        assert!(sweep_runs(&prof, &back).is_err());
    }

    #[test]
    fn a_closed_path_with_a_corner_mitres_where_it_closes() {
        let prof = Curve::Segs(vec![Seg::Circle { center: Vec3::ZERO, radius: 1.0, normal: Vec3::Z }]);
        let sq = [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 0.0], [0.0, 10.0, 0.0]];
        let path = Curve::Segs((0..4).map(|i| line(sq[i], sq[(i + 1) % 4])).collect());
        let runs = sweep_runs(&prof, &path).unwrap();
        assert_eq!(runs.len(), 4);
        assert!(runs.iter().all(|r| r.cuts.len() == 2), "{runs:?}");
        // Back at the start the profile has turned all the way round.
        let Curve::Segs(c) = &runs[3].profile else { panic!() };
        let [Seg::Circle { normal, .. }] = c.as_slice() else { panic!() };
        assert!(near(*normal, Vec3::new(0.0, -1.0, 0.0)));
    }

    #[test]
    fn curves_reverse_and_close() {
        let open = Curve::Segs(vec![line([0.0, 0.0, 0.0], [1.0, 0.0, 0.0]), line([1.0, 0.0, 0.0], [1.0, 1.0, 0.0])]);
        assert!(!open.is_closed());
        assert_eq!(open.reversed().ends(), Some((Vec3::new(1.0, 1.0, 0.0), Vec3::ZERO)));
        assert_eq!(open.reversed().reversed(), open);
        let tri =
            Curve::Segs(vec![line([0.0, 0.0, 0.0], [1.0, 0.0, 0.0]), line([1.0, 0.0, 0.0], [1.0, 1.0, 0.0]), line([1.0, 1.0, 0.0], [0.0, 0.0, 0.0])]);
        assert!(tri.is_closed());
        let sp = Curve::Spline { points: vec![Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0)], tangents: Some((Vec3::Z, Vec3::new(1.0, 0.0, 0.0))) };
        assert_eq!(sp.start(), Some((Vec3::ZERO, Vec3::Z)));
        assert_eq!(sp.reversed().start(), Some((Vec3::new(1.0, 0.0, 0.0), Vec3::new(-1.0, 0.0, 0.0))));
    }
}
