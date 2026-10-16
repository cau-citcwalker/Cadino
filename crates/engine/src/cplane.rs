//! Construction planes (Rhino's CPlane, AutoCAD's UCS) on the engine side.
//!
//! Each model tile has a construction plane ([`Session::cplane`]: its coordinates → world; World
//! is the identity), set by CPLANE / UCS (`cmd/view/cplane.rs`). Commands keep working in 2D:
//! the points they are given (clicks, typed coordinates, JSON points) are CPlane coordinates,
//! with [`Session::point_z`] the height along the plane's normal. Objects they create are placed
//! in the plane ([`Session::add_entity`]); MOVE, COPY, ROTATE, SCALE, MIRROR and STRETCH act in
//! 3D through it ([`Session::cplane_matrix`]); picks, snaps and grips see the drawing in its
//! coordinates ([`Session::pick_doc`]). In World every one of these is the plan behaviour itself.
//!
//! The plan-only commands ([`PLAN_ONLY`]) compute in world XY. In a level plane (AutoCAD's
//! everyday 2D UCS: an origin moved, an XY rotated) they run in World: the points given at their
//! prompts are taken from the plane into the world and their rubber bands back
//! ([`crate::guard`]), and their JSON forms take world coordinates. A tilted plane refuses them.
//! The view commands that take world points ([`WORLD_POINTS`]) are taken into the world the same
//! way in a level plane. Selection windows are the screen's: world plan boxes in a level plane
//! ([`Session::select_box`]).

use std::sync::{Arc, Mutex, PoisonError, Weak};

use cadcraft_doc::{Drawing, Entity, EntityKind, EntityStore, Handle};
use cadcraft_geom::{Bounds2, Mat3, Mat4, Vec2};

use crate::{EngineError, Input, Result, Session};

/// Commands that combine picked points with existing geometry in plan. In a level construction
/// plane they run in World (see the module notes); a tilted one refuses them.
/// ponytail: World, not the plane: in a rotated UCS their own directions (horizontal and
/// vertical constraints, a hatch's angle, XLINE-like Hor/Ver options) follow world X, and
/// what they build from scratch in a raised plane sits at z = 0 as their plan geometry does;
/// upgrade: run them on [`Session::pick_doc`]'s objects and place the results with
/// [`Session::place_local`], as grips and STRETCH do.
pub const PLAN_ONLY: &[&str] = &[
    "offset",
    "trim",
    "extend",
    "fillet",
    "chamfer",
    "break",
    "breakatpoint",
    "lengthen",
    "join",
    "pedit",
    "splinedit",
    "blend",
    "hatch",
    "gradient",
    "boundary",
    "region",
    "hatchedit",
    "hatchpick",
    "divide",
    "measure",
    "centermark",
    "centerline",
    "dim",
    "dimradius",
    "dimdiameter",
    "dimangular",
    "dimarc",
    "dimjogged",
    "dimordinate",
    "dimcontinue",
    "dimbaseline",
    "qdim",
    "dimspace",
    "dimbreak",
    "array",
    "arrayrect",
    "arraypolar",
    "arraypath",
    "arrayedit",
    "pasteclip",
    "pasteblock",
    "pasteorig",
    "block",
    "-block",
    "wblock",
    "revcloud",
    "wipeout",
    "mleadercollect",
    "ncopy",
    "geomconstraint",
    "autoconstrain",
    "dimconstraint",
    "dclinear",
    "gccoincident",
    "gcperpendicular",
    "gcparallel",
    "gctangent",
    "gchorizontal",
    "gcvertical",
    "gccollinear",
    "gcconcentric",
    "gcsmooth",
    "gcsymmetric",
    "gcequal",
    "gcfix",
    "dcaligned",
    "dchorizontal",
    "dcvertical",
    "dcangular",
    "dcradius",
    "dcdiameter",
    // Their base point is the one PASTECLIP and PASTEBLOCK move from.
    "copybase",
    // Edits that store a picked point in an existing (world) object.
    "dimtedit",
    "dimjogline",
    "mledit",
    "mleaderedit",
    "mleaderedit.add",
    "mleaderedit.remove",
    "mleaderalign",
];

/// View commands whose points are world plan points (ZOOM Window, Center and Dynamic; PAN): in a
/// level plane other than World their prompts' points are taken into the world as the plan-only
/// commands' are; a tilted plane leaves them as they were.
pub const WORLD_POINTS: &[&str] = &["zoom", "zoom.window", "zoom.dynamic", "pan"];

/// Whether construction plane `c` is tilted: its normal is not the world Z axis (either way).
pub(crate) fn tilted(c: &Mat4) -> bool {
    let n = c.col(2);
    n.x.hypot(n.y) > 1e-9
}

/// Why command `id` can't run in the current construction plane, if it can't.
pub(crate) fn refused(s: &Session, id: &str) -> Option<String> {
    (PLAN_ONLY.contains(&id) && tilted(&s.cplane()))
        .then(|| format!("{} works in plan: use it in a level construction plane (CPLANE World, or a UCS turned about Z).", id.to_ascii_uppercase()))
}

/// Whether construction plane `c` is level (its normal ±Z) but not World itself.
pub(crate) fn level_moved(c: &Mat4) -> bool {
    *c != Mat4::IDENTITY && !tilted(c)
}

/// Whether command `id` runs in World for the current construction plane: a plan-only one (or a
/// view command taking world points) in a level plane other than World.
pub(crate) fn in_world(s: &Session, id: &str) -> bool {
    (PLAN_ONLY.contains(&id) || WORLD_POINTS.contains(&id)) && level_moved(&s.cplane())
}

/// Point `p` of plane `c` at height `z` (along its normal), in the world: its plan point and
/// height (`None` on the XY plane).
fn world_point(c: &Mat4, p: Vec2, z: Option<f64>) -> (Vec2, Option<f64>) {
    let w = c.apply(p.to3(z.filter(|z| z.is_finite()).unwrap_or(0.0)));
    (w.xy(), (z.is_some() || w.z != 0.0).then_some(w.z))
}

/// An input given in plane `c` (points in its coordinates, `z` the height [`Session::point_z`]
/// gives), as a command working in World takes it, with the height it then has.
/// ponytail: a deferred tangent/perpendicular snap arrives as the point it was picked at;
/// upgrade: carry its curve into the world.
pub(crate) fn input_to_world(c: &Mat4, i: Input, z: Option<f64>) -> (Input, Option<f64>) {
    match i {
        Input::Point(p) => {
            let (q, z) = world_point(c, p, z);
            (Input::Point(q), z)
        }
        Input::Deferred(d) => {
            let (q, z) = world_point(c, d.at, z);
            (Input::Point(q), z)
        }
        other => (other, z),
    }
}

/// The point of plane `c` seen at world plan point `p` (on the plane), the cursor or a rubber
/// band's base: the inverse of [`input_to_world`].
pub(crate) fn plan_to_plane(c: &Mat4, p: Vec2) -> Vec2 {
    c.inverse().map_or(p, |i| i.apply(p.to3(0.0)).xy())
}

/// The world plan point of point `p` of plane `c`.
pub(crate) fn plane_to_plan(c: &Mat4, p: Vec2) -> Vec2 {
    world_point(c, p, None).0
}

/// Rubber-band kinds built in the world, in plane `c`'s coordinates (what can't be expressed
/// there is left out).
pub(crate) fn kinds_to_plane(c: &Mat4, kinds: Vec<EntityKind>) -> Vec<EntityKind> {
    kinds.into_iter().filter_map(|k| Entity::new(Handle(0), k).on(c).map(|e| e.kind)).collect()
}

/// A drag's plan transform `m` of the world as the same transform of plane `c`'s coordinates
/// (`c` level: its plan part is a rigid motion of the XY plane).
pub(crate) fn matrix_to_plane(c: &Mat4, m: Mat3) -> Mat3 {
    match c.inverse() {
        Some(i) => i.plane_xy().then_before(m).then_before(c.plane_xy()),
        None => m,
    }
}

/// A drawing seen in a construction plane, kept while its drawing and plane stay the same.
struct Seen {
    of: Weak<Drawing>,
    cplane: Mat4,
    doc: Arc<Drawing>,
}

/// The last drawing seen in a construction plane.
/// ponytail: one entry, rebuilt whole (O(objects)) after every change of the drawing; upgrade:
/// reproject only the store chunks that changed, as the spatial index does.
static SEEN: Mutex<Option<Seen>> = Mutex::new(None);

/// `d` with its model space in the coordinates of construction plane `c` ([`Entity::on`]);
/// objects that can't be expressed there are left out.
fn seen_in(d: &Drawing, c: &Mat4) -> Drawing {
    let mut out = d.clone();
    let mut model = EntityStore::new();
    for e in d.model.iter() {
        if let Some(l) = e.on(c) {
            model.push(l);
        }
    }
    out.model = model;
    out
}

impl Session {
    /// The construction plane the user set, also while a plan-only command runs in World (where
    /// [`Session::cplane`] reads World): AutoCAD's current UCS, for what a command keeps of it
    /// (DIMORDINATE's datum).
    pub(crate) fn user_cplane(&self) -> Mat4 {
        let was = self.plan_world.swap(false, std::sync::atomic::Ordering::Relaxed);
        let c = self.cplane();
        self.plan_world.store(was, std::sync::atomic::Ordering::Relaxed);
        c
    }

    /// What a selection window from `a` to `b` (CPlane points) takes, as the screen shows it: in a
    /// level plane the world plan box with those corners (AutoCAD's windows are the screen's, not
    /// the UCS's), crossing when it was drawn right to left on screen; in a tilted plane the box
    /// in its coordinates.
    pub(crate) fn select_box(&self, a: Vec2, b: Vec2) -> Result<Vec<Handle>> {
        let (c, space) = (self.cplane(), self.space());
        let (a, b, d) = if tilted(&c) { (a, b, self.pick_doc()?) } else { (plane_to_plan(&c, a), plane_to_plan(&c, b), self.state()?.doc.clone()) };
        Ok(crate::select::select_window(&d, &space, Bounds2::new(a, b), b.x < a.x))
    }

    /// What a typed selection mode (W, C, Box, F, WP, CP) takes once its points are in: in a level
    /// plane its points in the world on the drawing itself, so boxes are the screen's.
    pub(crate) fn select_by_mode(&self, mode: &crate::select::SelectMode) -> Result<Option<Vec<Handle>>> {
        let (c, space) = (self.cplane(), self.space());
        if tilted(&c) {
            return Ok(mode.select(&*self.pick_doc()?, &space));
        }
        let world = crate::select::SelectMode { kind: mode.kind, pts: mode.pts.iter().map(|p| plane_to_plan(&c, *p)).collect() };
        Ok(world.select(self.doc()?, &space))
    }

    /// The drawing as the current construction plane sees it, for picks, snaps and grips: the
    /// drawing itself in World (and outside model space); else a copy whose model space is in
    /// CPlane coordinates ([`Entity::on`]), without the objects that can't be expressed there
    /// (what stays level, seen from a tilted plane). Read only: handles are the drawing's.
    pub fn pick_doc(&self) -> Result<Arc<Drawing>> {
        let doc = self.state()?.doc.clone();
        let c = self.cplane();
        if c == Mat4::IDENTITY {
            return Ok(doc);
        }
        let mut seen = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(k) = seen.as_ref().filter(|k| k.cplane == c && Weak::ptr_eq(&k.of, &Arc::downgrade(&doc))) {
            return Ok(k.doc.clone());
        }
        let out = Arc::new(seen_in(&doc, &c));
        *seen = Some(Seen { of: Arc::downgrade(&doc), cplane: c, doc: out.clone() });
        Ok(out)
    }

    /// Object `h` in the coordinates of the current construction plane (itself in World).
    pub(crate) fn local_entity(&self, h: Handle) -> Result<Entity> {
        if let Some(e) = self.pick_doc()?.entity(h) {
            return Ok((**e).clone());
        }
        Err(EngineError::Other(if self.doc()?.entity(h).is_some() {
            "The object can't be edited in this construction plane.".into()
        } else {
            "no such object".into()
        }))
    }

    /// An object in CPlane coordinates placed back in the world (itself in World); an error when
    /// it can't be (see [`Entity::transform3`]).
    pub(crate) fn place_local(&self, local: &Entity) -> Result<Entity> {
        let c = self.cplane();
        let mut e = local.clone();
        if c == Mat4::IDENTITY || e.transform3(&c) {
            Ok(e)
        } else {
            Err(EngineError::Other(format!("The {} can't be placed in this construction plane.", e.kind.type_name().to_ascii_lowercase())))
        }
    }

    /// Give object `h`, which a command added ([`Session::add_entity`]) and is still building,
    /// the shape `kind` in CPlane coordinates (placed in the plane as `add_entity` places it).
    pub(crate) fn set_local_kind(&mut self, h: Handle, kind: EntityKind) -> Result<()> {
        if self.cplane() == Mat4::IDENTITY {
            return Ok(self.doc_mut()?.modify_entity(h, |e| e.kind = kind)?);
        }
        let mut common = self.doc()?.entity(h).map(|e| e.common.clone()).ok_or_else(|| EngineError::Other("no such object".into()))?;
        common.extrusion = cadcraft_geom::Vec3::Z;
        let e = self.place_local(&Entity { handle: h, common, kind })?;
        Ok(self.doc_mut()?.modify_entity(h, |x| *x = e)?)
    }

    /// The world transform of `m`, a 2D transform of CPlane coordinates (MOVE, ROTATE, SCALE,
    /// MIRROR): C·M·C⁻¹, z scaled by √|det| as objects in tilted planes and solids scale. `None`
    /// in World (`m` itself is the plan transform).
    pub fn cplane_matrix(&self, m: &Mat3) -> Option<Mat4> {
        let c = self.cplane();
        if c == Mat4::IDENTITY {
            return None;
        }
        let mut t = Mat4::from_mat3(m);
        t.m[2][2] = m.det().abs().sqrt();
        Some(c.mul(&t).mul(&c.inverse()?))
    }

    /// `e` (in the world) after `m`, a transform of CPlane coordinates, as preview kinds in CPlane
    /// coordinates (what a rubber band shows; the UI places them); in World the plan preview.
    pub(crate) fn local_preview(&self, e: &Entity, m: &Mat3) -> Vec<EntityKind> {
        let Some(t) = self.cplane_matrix(m) else { return crate::cmd::helpers::plan_preview(e, m) };
        let mut moved = e.clone();
        if !moved.transform3(&t) {
            return Vec::new();
        }
        moved.on(&self.cplane()).map(|l| crate::cmd::helpers::plan_kinds(&l)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_only_commands_exist() {
        for id in PLAN_ONLY {
            assert!(crate::find_command(id).is_some(), "{id}");
        }
    }
}
