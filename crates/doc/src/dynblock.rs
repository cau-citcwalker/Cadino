//! Dynamic blocks: their parameters and actions, read from the data kept for them and written by
//! the Block Editor's authoring commands ([`author`]).
//!
//! A dynamic block's behaviour lives in objects kept as [`Foreign`](crate::foreign::Foreign)
//! data, as a file has them: the block's BLOCK_RECORD has an extension dictionary
//! whose `ACAD_ENHANCEDBLOCK` entry is an evaluation graph (`ACAD_EVALUATION_GRAPH`), and the
//! graph lists the block's *parameters* (a distance, a point, visibility states, a lookup…) and
//! *actions* (what changing a parameter does to the block's objects: stretch, move, look up…).
//! [`dynamic_block`] reads them into the model below without changing the kept data, so a drawing
//! saved again still carries them exactly as read. Authoring writes objects of the same layout
//! (see [`author`]), so the reader, the references' runtime and the file round trip are shared.
//!
//! Every count and index here comes from the file: they are capped by the data actually present
//! and unknown or damaged objects are skipped, never trusted.

use std::collections::HashMap;

use cadcraft_geom::{Vec2, Vec3};
use serde::Serialize;

use crate::foreign::ForeignObject;
use crate::{Drawing, Handle, RawTag};

mod apply;
pub mod author;
mod grips;
mod reference;
pub use apply::{drag_grip, set_property};
pub use grips::{DynGrip, GripPart, GripShape, grips};
pub use reference::{CustomProperty, Reference, Value, custom_properties, reference};

/// Most objects one evaluation graph is read with.
const MAX_NODES: usize = 10_000;

/// Handles as hex strings in JSON, as commands report them.
fn hex<S: serde::Serializer>(h: &Handle, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&h.hex())
}
fn hexes<S: serde::Serializer>(hs: &[Handle], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(hs.iter().map(|h| h.hex()))
}
fn hex_vertices<S: serde::Serializer>(v: &[(Handle, Vec<i32>)], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(v.iter().map(|(h, i)| (h.hex(), i)))
}

/// A dynamic block's parameters and actions.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicBlock {
    pub parameters: Vec<Parameter>,
    pub actions: Vec<Action>,
    /// Graph members of types not modelled here (grips, constraints…), by DXF type.
    pub other: Vec<String>,
    /// The grip objects by graph id (parameters name their grips by id; visibility states list
    /// the grips shown in them by object).
    #[serde(skip)]
    pub grip_objects: Vec<(i32, Handle)>,
}

/// One parameter: a property a reference to the block can change.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Parameter {
    /// Its id in the evaluation graph (actions and lookups refer to parameters by it).
    pub node: i32,
    #[serde(serialize_with = "hex")]
    pub handle: Handle,
    /// The name the block editor shows ("Linear", "Visibility State").
    pub element: String,
    /// The parameter's properties appear in a reference's Properties palette.
    pub show_properties: bool,
    /// Chain Actions: its actions also run when another action moves it.
    pub chain: bool,
    pub kind: ParameterKind,
    /// Graph ids of its grips: a two-point parameter's base and end grips (0 for none), a
    /// one-point parameter's grip. A reference's history records grip drags by these.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub grips: Vec<i32>,
}

/// What a parameter is, with its definition geometry in block coordinates.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ParameterKind {
    /// A distance between two points (Distance1).
    Linear {
        base: Vec3,
        end: Vec3,
        label: String,
        description: String,
        values: ValueSet,
        /// The value changes the end point (`false`) or both points from the midpoint (`true`).
        from_midpoint: bool,
    },
    /// A distance (`label`, `values`) and an angle (`label2`, `values2`, radians) from a base
    /// point.
    Polar { base: Vec3, end: Vec3, label: String, description: String, label2: String, description2: String, values: ValueSet, values2: ValueSet },
    /// Horizontal (`label`, `values`) and vertical (`label2`, `values2`) distances from a base
    /// point.
    Xy { base: Vec3, end: Vec3, label: String, description: String, label2: String, description2: String, values: ValueSet, values2: ValueSet },
    /// An angle about a base point: the end point is at the radius, at the default angle; the
    /// value shown is measured from `base_angle` (radians, as `values`).
    Rotation { base: Vec3, end: Vec3, label: String, description: String, base_angle: f64, values: ValueSet },
    /// Aligns the reference with nearby geometry.
    Alignment { base: Vec3, end: Vec3, perpendicular: bool },
    /// Mirrors the objects about a reflection line; `states` are the names of its two states (not
    /// flipped, flipped).
    Flip { base: Vec3, end: Vec3, label: String, description: String, label_at: Vec3, states: [String; 2] },
    /// A position (Position1).
    Point { at: Vec3, label: String, description: String },
    /// Named sets of visible objects.
    Visibility {
        at: Vec3,
        label: String,
        description: String,
        #[serde(serialize_with = "hexes")]
        controlled: Vec<Handle>,
        states: Vec<VisibilityState>,
    },
    /// A named value chosen from a lookup table.
    Lookup { at: Vec3, label: String, description: String },
    /// Moves the block's base point.
    BasePoint { at: Vec3 },
    /// A parameter type not modelled yet.
    Other { dxf_type: String },
}

impl ParameterKind {
    /// The property name a reference shows ("Distance1", "Visibility1"), when it has one.
    pub fn label(&self) -> Option<&str> {
        match self {
            ParameterKind::Linear { label, .. }
            | ParameterKind::Point { label, .. }
            | ParameterKind::Visibility { label, .. }
            | ParameterKind::Lookup { label, .. }
            | ParameterKind::Polar { label, .. }
            | ParameterKind::Xy { label, .. }
            | ParameterKind::Rotation { label, .. }
            | ParameterKind::Flip { label, .. } => Some(label),
            _ => None,
        }
    }
    /// Every property name it gives a reference: a polar parameter's angle and an XY
    /// parameter's vertical distance besides [`ParameterKind::label`].
    pub fn labels(&self) -> Vec<&str> {
        match self {
            ParameterKind::Polar { label, label2, .. } | ParameterKind::Xy { label, label2, .. } => vec![label, label2],
            k => k.label().into_iter().collect(),
        }
    }
    /// Its definition points: a two-point parameter's base and end, a one-point parameter's
    /// point twice.
    pub fn points(&self) -> Option<(Vec3, Vec3)> {
        match self {
            ParameterKind::Linear { base, end, .. }
            | ParameterKind::Polar { base, end, .. }
            | ParameterKind::Xy { base, end, .. }
            | ParameterKind::Rotation { base, end, .. }
            | ParameterKind::Alignment { base, end, .. }
            | ParameterKind::Flip { base, end, .. } => Some((*base, *end)),
            ParameterKind::Point { at, .. }
            | ParameterKind::Visibility { at, .. }
            | ParameterKind::Lookup { at, .. }
            | ParameterKind::BasePoint { at } => Some((*at, *at)),
            ParameterKind::Other { .. } => None,
        }
    }
}

/// One visibility state: its name and the objects shown in it (the parameter's other controlled
/// objects are hidden).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisibilityState {
    pub name: String,
    #[serde(serialize_with = "hexes")]
    pub visible: Vec<Handle>,
    /// The parameters and actions active in this state (their objects).
    #[serde(serialize_with = "hexes")]
    pub active: Vec<Handle>,
}

/// The values a distance or angle may take.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueSet {
    /// Which of the limits below apply (see the methods).
    pub flags: i32,
    pub minimum: f64,
    pub maximum: f64,
    pub increment: f64,
    /// The allowed values, for a list.
    pub list: Vec<f64>,
}

impl ValueSet {
    pub fn has_minimum(&self) -> bool {
        self.flags & 1 != 0
    }
    pub fn has_maximum(&self) -> bool {
        self.flags & 2 != 0
    }
    pub fn has_increment(&self) -> bool {
        self.flags & 4 != 0
    }
    pub fn is_list(&self) -> bool {
        self.flags & 8 != 0
    }
}

/// One action: what changing parameters does to the block's objects.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub node: i32,
    #[serde(serialize_with = "hex")]
    pub handle: Handle,
    /// Its name in the block editor ("Stretch", "Lookup1").
    pub name: String,
    /// Where the block editor shows it.
    pub at: Vec3,
    /// The objects it acts on.
    #[serde(serialize_with = "hexes")]
    pub selection: Vec<Handle>,
    /// Graph ids of the parameters it follows, with the parameter value each connection reads
    /// ("EndXDelta").
    pub connections: Vec<(i32, String)>,
    pub kind: ActionKind,
}

/// What an action does.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ActionKind {
    /// Moves the vertices inside `frame` of the objects listed (all of an object not listed).
    Stretch {
        frame: Vec<Vec2>,
        #[serde(serialize_with = "hex_vertices")]
        vertices: Vec<(Handle, Vec<i32>)>,
        multiplier: f64,
        angle_offset: f64,
    },
    /// A table: each row sets the listed parameter values together.
    Lookup { columns: Vec<LookupColumn>, rows: Vec<Vec<String>> },
    /// Moves the objects by the change of the parameter values it follows.
    Move { multiplier: f64, angle_offset: f64 },
    /// Scales the objects by the change of a distance, about the parameter's base point or an
    /// independent `base`.
    Scale { base: Option<Vec3> },
    /// Rotates the objects by the change of an angle, about the parameter's base point or an
    /// independent `base`.
    Rotate { base: Option<Vec3> },
    /// Mirrors the objects about the flip parameter's reflection line when its state changes.
    Flip,
    /// Copies the objects along the parameter's distance, `column` apart (and `row` apart
    /// across, for an XY parameter).
    Array { column: f64, row: f64 },
    /// Moves the vertices inside `frame` with the polar parameter's end point (rotating them
    /// about its base) and rotates the objects of `rotate`.
    PolarStretch {
        frame: Vec<Vec2>,
        #[serde(serialize_with = "hex_vertices")]
        vertices: Vec<(Handle, Vec<i32>)>,
        #[serde(serialize_with = "hexes")]
        rotate: Vec<Handle>,
    },
    /// An action type not modelled yet.
    Other { dxf_type: String },
}

/// A lookup table column: the parameter value it reads or sets.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LookupColumn {
    /// The parameter's graph id.
    pub node: i32,
    /// The value it stands for ("VisibilityState", "lookupString").
    pub property: String,
    /// A lookup property (shown on the reference) rather than an input.
    pub lookup: bool,
    /// What the reference shows when no row matches ("Custom").
    pub unmatched: String,
}

/// The groups of an object split at its subclass markers (100): `(marker, groups after it)`.
fn sections(tags: &[RawTag]) -> Vec<(&str, &[RawTag])> {
    let mut out: Vec<(&str, &[RawTag])> = Vec::new();
    let mut start = 0;
    let mut name = "";
    for (i, t) in tags.iter().enumerate() {
        if t.code == 100 {
            out.push((name, tags.get(start..i).unwrap_or_default()));
            name = t.value.as_str();
            start = i + 1;
        }
    }
    out.push((name, tags.get(start..).unwrap_or_default()));
    out
}

/// The groups after subclass marker `marker`, empty when the object has none.
fn section<'a>(secs: &[(&str, &'a [RawTag])], marker: &str) -> &'a [RawTag] {
    secs.iter().find(|(n, _)| n.eq_ignore_ascii_case(marker)).map_or(&[], |(_, t)| t)
}

/// Reads groups in order, as an object writes them.
struct Cursor<'a> {
    tags: &'a [RawTag],
    at: usize,
    /// Groups scanned so far. A missing group neither moves the cursor nor ends the loops that
    /// ask for it, so a hostile record could make nested counts rescan it again and again; past
    /// `budget` every lookup fails at once and every count is zero.
    work: usize,
    budget: usize,
}

impl<'a> Cursor<'a> {
    fn new(tags: &'a [RawTag]) -> Self {
        Cursor { tags, at: 0, work: 0, budget: tags.len().saturating_mul(16).saturating_add(1024) }
    }
    /// The next group with `code` from here on (the cursor moves past it).
    fn next(&mut self, code: i32) -> Option<&'a str> {
        if self.work >= self.budget {
            return None;
        }
        let rest = self.tags.get(self.at..)?;
        let Some(rel) = rest.iter().position(|t| t.code == code) else {
            self.work = self.work.saturating_add(rest.len().max(1));
            return None;
        };
        self.work = self.work.saturating_add(rel + 1);
        self.at += rel + 1;
        self.tags.get(self.at - 1).map(|t| t.value.as_str())
    }
    fn string(&mut self, code: i32) -> String {
        self.next(code).unwrap_or_default().to_string()
    }
    fn int(&mut self, code: i32) -> i32 {
        self.next(code).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
    }
    fn real(&mut self, code: i32) -> f64 {
        self.next(code).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite()).unwrap_or(0.0)
    }
    /// A count read from the file, capped by the groups left (each item needs at least one).
    fn count(&mut self, code: i32) -> usize {
        let n = usize::try_from(self.int(code)).unwrap_or(0).min(self.tags.len().saturating_sub(self.at));
        if self.work >= self.budget { 0 } else { n }
    }
    fn handle(&mut self, code: i32) -> Option<Handle> {
        self.next(code).and_then(Handle::parse_hex)
    }
    fn handles(&mut self, code: i32, n: usize) -> Vec<Handle> {
        (0..n).filter_map(|_| self.handle(code)).collect()
    }
    /// A point written with codes `x`, `x + 10`, `x + 20`.
    fn point(&mut self, x: i32) -> Vec3 {
        let px = self.real(x);
        let py = self.real(x + 10);
        let at = self.at;
        // Z is optional: only take it when it is the very next group.
        let pz = match self.tags.get(at) {
            Some(t) if t.code == x + 20 => self.real(x + 20),
            _ => 0.0,
        };
        Vec3::new(px, py, pz)
    }
}

/// The objects `xdict`'s `ACAD_ENHANCEDBLOCK` evaluation graph lists, when the dictionary has one.
fn graph_members<'a>(objects: &HashMap<Handle, &'a ForeignObject>, xdict: Handle) -> Option<Vec<&'a ForeignObject>> {
    let dict = objects.get(&xdict)?;
    let mut graph = None;
    let mut tags = dict.tags.iter();
    while let Some(t) = tags.next() {
        if t.code == 3 && t.value.eq_ignore_ascii_case("ACAD_ENHANCEDBLOCK") {
            graph = tags.next().filter(|t| t.code == 350 || t.code == 360).and_then(|t| Handle::parse_hex(&t.value));
            break;
        }
    }
    let graph = objects.get(&graph?).filter(|o| o.kind.eq_ignore_ascii_case("ACAD_EVALUATION_GRAPH"))?;
    let members = section(&sections(&graph.tags), "AcDbEvalGraph").iter().filter(|t| t.code == 360).filter_map(|t| Handle::parse_hex(&t.value));
    Some(members.take(MAX_NODES).filter_map(|h| objects.get(&h).copied()).collect())
}

/// The dynamic block data of block `name` (any case; `*Model_Space` for a drawing that is a
/// block's own drawing), or `None` when it has none.
pub fn dynamic_block(d: &Drawing, name: &str) -> Option<DynamicBlock> {
    let xdict = d.foreign.records.get(&("BLOCK_RECORD".to_string(), name.trim().to_ascii_uppercase()))?.xdict?;
    let objects: HashMap<Handle, &ForeignObject> = d.foreign.objects.iter().map(|o| (o.handle, o)).collect();
    let mut out = DynamicBlock::default();
    for o in graph_members(&objects, xdict)? {
        let secs = sections(&o.tags);
        let node = Cursor::new(section(&secs, "AcDbEvalExpr")).int(90);
        let element = Cursor::new(section(&secs, "AcDbBlockElement")).string(300);
        let kind = o.kind.to_ascii_uppercase();
        if kind.ends_with("PARAMETER") {
            let mut c = Cursor::new(section(&secs, "AcDbBlockParameter"));
            let show_properties = c.int(280) != 0;
            let chain = c.int(281) != 0;
            let grips = grip_ids(&secs);
            out.parameters.push(Parameter { node, handle: o.handle, element, show_properties, chain, kind: parameter(&kind, &secs), grips });
        } else if kind.ends_with("ACTION") {
            out.actions.push(action(o.handle, node, element, &kind, &secs));
        } else {
            if kind.ends_with("GRIP") {
                out.grip_objects.push((node, o.handle));
            }
            out.other.push(o.kind.clone());
        }
    }
    Some(out)
}

/// A parameter's grip ids: after a two-point parameter's points, the grip count (170) and the
/// base and end grips (91); after a one-point parameter's point, its grip (93).
fn grip_ids(secs: &[(&str, &[RawTag])]) -> Vec<i32> {
    let two = section(secs, "AcDbBlock2PtParameter");
    if !two.is_empty() {
        let mut c = Cursor::new(two);
        c.point(1010);
        c.point(1011);
        let n = c.count(170).min(2);
        return (0..n).map(|_| c.int(91)).collect();
    }
    let mut c = Cursor::new(section(secs, "AcDbBlock1PtParameter"));
    c.point(1010);
    c.next(93).and_then(|v| v.trim().parse().ok()).into_iter().collect()
}

fn parameter(kind: &str, secs: &[(&str, &[RawTag])]) -> ParameterKind {
    let two = || {
        let mut c = Cursor::new(section(secs, "AcDbBlock2PtParameter"));
        (c.point(1010), c.point(1011))
    };
    let one = || Cursor::new(section(secs, "AcDbBlock1PtParameter")).point(1010);
    match kind {
        "BLOCKLINEARPARAMETER" => {
            let (base, end) = two();
            let mut c = Cursor::new(section(secs, "AcDbBlockLinearParameter"));
            let label = c.string(305);
            let description = c.string(306);
            let from_midpoint = Cursor::new(section(secs, "AcDbBlock2PtParameter")).int(177) == 1;
            ParameterKind::Linear { base, end, label, description, values: value_set(&mut c, SET1), from_midpoint }
        }
        "BLOCKPOINTPARAMETER" => {
            let mut c = Cursor::new(section(secs, "AcDbBlockPointParameter"));
            ParameterKind::Point { at: one(), label: c.string(303), description: c.string(304) }
        }
        "BLOCKVISIBILITYPARAMETER" => visibility(one(), section(secs, "AcDbBlockVisibilityParameter")),
        "BLOCKLOOKUPPARAMETER" => {
            let mut c = Cursor::new(section(secs, "AcDbBlockLookupParameter"));
            ParameterKind::Lookup { at: one(), label: c.string(303), description: c.string(304) }
        }
        "BLOCKBASEPOINTPARAMETER" => ParameterKind::BasePoint { at: one() },
        "BLOCKPOLARPARAMETER" | "BLOCKXYPARAMETER" => {
            let (base, end) = two();
            let polar = kind == "BLOCKPOLARPARAMETER";
            let mut c = Cursor::new(section(secs, if polar { "AcDbBlockPolarParameter" } else { "AcDbBlockXYParameter" }));
            let (label, description, label2, description2) = (c.string(305), c.string(306), c.string(308), c.string(309));
            let values = value_set(&mut c, SET1);
            let values2 = value_set(&mut c, SET2);
            if polar {
                ParameterKind::Polar { base, end, label, description, label2, description2, values, values2 }
            } else {
                ParameterKind::Xy { base, end, label, description, label2, description2, values, values2 }
            }
        }
        "BLOCKROTATIONPARAMETER" => {
            let (base, end) = two();
            let mut c = Cursor::new(section(secs, "AcDbBlockRotationParameter"));
            let (label, description, base_angle) = (c.string(305), c.string(306), c.real(140));
            ParameterKind::Rotation { base, end, label, description, base_angle, values: value_set(&mut c, SET1) }
        }
        "BLOCKALIGNMENTPARAMETER" => {
            let (base, end) = two();
            ParameterKind::Alignment { base, end, perpendicular: Cursor::new(section(secs, "AcDbBlockAlignmentParameter")).int(280) != 0 }
        }
        "BLOCKFLIPPARAMETER" => {
            let (base, end) = two();
            let mut c = Cursor::new(section(secs, "AcDbBlockFlipParameter"));
            let (label, description) = (c.string(305), c.string(306));
            let states = [c.string(307), c.string(308)];
            ParameterKind::Flip { base, end, label, description, label_at: c.point(1012), states }
        }
        other => ParameterKind::Other { dxf_type: other.to_string() },
    }
}

/// The group codes of a value set: description, flags, minimum, maximum, increment, the count of
/// listed values and each listed value.
#[derive(Clone, Copy)]
pub(crate) struct SetCodes {
    pub description: i32,
    pub flags: i32,
    pub limits: [i32; 3],
    pub count: i32,
    pub value: i32,
}

/// A parameter's (first) value set: description (307), flags (96), minimum, maximum and increment
/// (141–143), then the listed values (175 count, 144 each).
pub(crate) const SET1: SetCodes = SetCodes { description: 307, flags: 96, limits: [141, 142, 143], count: 175, value: 144 };
/// A polar parameter's angle set, an XY parameter's vertical set.
// ponytail: our own codes for the second set (the DXF Reference doesn't document them); AutoCAD's
// files keep theirs raw.
pub(crate) const SET2: SetCodes = SetCodes { description: 410, flags: 97, limits: [145, 146, 147], count: 176, value: 148 };

/// A value set after a parameter's name and description, with codes `k`.
fn value_set(c: &mut Cursor, k: SetCodes) -> ValueSet {
    c.next(k.description);
    let flags = c.int(k.flags);
    let [a, b, i] = k.limits;
    let (minimum, maximum, increment) = (c.real(a), c.real(b), c.real(i));
    let n = c.count(k.count);
    let list = (0..n).filter_map(|_| c.next(k.value).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite())).collect();
    ValueSet { flags, minimum, maximum, increment, list }
}

/// A visibility parameter: its name and description (301, 302), the objects it controls (93
/// count, 331 each), then each state (92 count): name (303), visible objects (94, 332) and the
/// parameters and actions active in it (95, 333).
fn visibility(at: Vec3, tags: &[RawTag]) -> ParameterKind {
    let mut c = Cursor::new(tags);
    let label = c.string(301);
    let description = c.string(302);
    let n = c.count(93);
    let controlled = c.handles(331, n);
    let states = (0..c.count(92))
        .map(|_| {
            let name = c.string(303);
            let n = c.count(94);
            let visible = c.handles(332, n);
            let n = c.count(95);
            VisibilityState { name, visible, active: c.handles(333, n) }
        })
        .collect();
    ParameterKind::Visibility { at, label, description, controlled, states }
}

fn action(handle: Handle, node: i32, name: String, kind: &str, secs: &[(&str, &[RawTag])]) -> Action {
    let mut c = Cursor::new(section(secs, "AcDbBlockAction"));
    c.next(70);
    let n = c.count(71);
    let selection = c.handles(330, n);
    let at = c.point(1010);
    let (connections, kind) = match kind {
        "BLOCKSTRETCHACTION" => stretch(section(secs, "AcDbBlockStretchAction")),
        "BLOCKLOOKUPACTION" => (Vec::new(), lookup(section(secs, "AcDbBlockLookupAction"))),
        "BLOCKMOVEACTION" => {
            let tags = section(secs, "AcDbBlockMoveAction");
            let mut c = Cursor::new(tags);
            let (multiplier, angle_offset) = (c.real(140), c.real(141));
            (two_connections(tags), ActionKind::Move { multiplier, angle_offset })
        }
        "BLOCKSCALEACTION" | "BLOCKROTATEACTION" => {
            let scale = kind == "BLOCKSCALEACTION";
            let tags = section(secs, if scale { "AcDbBlockScaleAction" } else { "AcDbBlockRotateAction" });
            let mut c = Cursor::new(tags);
            let base = (c.int(280) != 0).then(|| c.point(1011));
            (two_connections(tags), if scale { ActionKind::Scale { base } } else { ActionKind::Rotate { base } })
        }
        "BLOCKFLIPACTION" => (two_connections(section(secs, "AcDbBlockFlipAction")), ActionKind::Flip),
        "BLOCKARRAYACTION" => {
            let tags = section(secs, "AcDbBlockArrayAction");
            let mut c = Cursor::new(tags);
            let (column, row) = (c.real(140), c.real(141));
            (two_connections(tags), ActionKind::Array { column, row })
        }
        "BLOCKPOLARSTRETCHACTION" => {
            let (connections, s) = stretch(section(secs, "AcDbBlockPolarStretchAction"));
            match s {
                ActionKind::Stretch { frame, vertices, .. } => {
                    let mut c = Cursor::new(section(secs, "AcDbBlockPolarStretchAction"));
                    c.next(73);
                    let n = c.count(75);
                    (connections, ActionKind::PolarStretch { frame, vertices, rotate: c.handles(332, n) })
                }
                other => (connections, other),
            }
        }
        other => (Vec::new(), ActionKind::Other { dxf_type: other.to_string() }),
    };
    Action { node, handle, name, at, selection, connections, kind }
}

/// Two connections: parameter ids (92, 93) and the values they read (301, 302). Writers order
/// these groups differently (92 93 301 302, or 92 301 93 302), so each is read on its own.
fn two_connections(tags: &[RawTag]) -> Vec<(i32, String)> {
    let first = |code: i32| Cursor::new(tags).next(code).unwrap_or_default().to_string();
    let int = |code: i32| first(code).trim().parse().unwrap_or(0);
    vec![(int(92), first(301)), (int(93), first(302))]
}

/// A stretch action: its two connections, the frame (72 count, 1011/1021 points), the objects
/// whose vertices it moves (73 count; each a 331 handle, a 74 count and 94 vertex indices), then
/// the distance multiplier (140) and angle offset (141).
fn stretch(tags: &[RawTag]) -> (Vec<(i32, String)>, ActionKind) {
    let connections = two_connections(tags);
    let mut c = Cursor::new(tags);
    let n = c.count(72);
    let frame = (0..n)
        .map(|_| {
            let x = c.real(1011);
            Vec2::new(x, c.real(1021))
        })
        .collect();
    let objects = c.count(73);
    let vertices = (0..objects)
        .filter_map(|_| {
            let h = c.handle(331)?;
            let n = c.count(74);
            Some((h, (0..n).map(|_| c.int(94)).collect()))
        })
        .collect();
    c.next(75);
    let (multiplier, angle_offset) = (c.real(140), c.real(141));
    (connections, ActionKind::Stretch { frame, vertices, multiplier, angle_offset })
}

/// A lookup table: rows (92) and columns (93), the cells row by row (302, after a 301 header),
/// then each column: unmatched name (303 then 305), parameter id (94), lookup flag (282) and the
/// parameter value it stands for (304).
fn lookup(tags: &[RawTag]) -> ActionKind {
    let mut c = Cursor::new(tags);
    let rows = c.count(92);
    let cols = c.count(93);
    c.next(301);
    let cells = rows.saturating_mul(cols).min(tags.len());
    let flat: Vec<String> = (0..cells).map(|_| c.string(302)).collect();
    let rows = flat.chunks(cols.max(1)).map(<[String]>::to_vec).collect();
    let columns = (0..cols)
        .map(|_| {
            c.next(303);
            let node = c.int(94);
            let lookup = c.int(282) != 0;
            let unmatched = c.string(305);
            let property = c.string(304);
            LookupColumn { node, property, lookup, unmatched }
        })
        .collect();
    ActionKind::Lookup { columns, rows }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(code: i32, value: &str) -> RawTag {
        RawTag { code, value: value.to_string() }
    }

    /// Counts that promise groups the record doesn't have: each nested loop would rescan the
    /// record for them. The work budget keeps this linear in the record's size.
    #[test]
    fn hostile_counts_with_missing_groups_stay_linear() {
        let n = 20_000;
        let mut tags = vec![tag(93, "0"), tag(92, &n.to_string())];
        tags.extend((0..n).map(|_| tag(94, &n.to_string())));
        let ParameterKind::Visibility { states, .. } = visibility(Vec3::default(), &tags) else { panic!("a visibility parameter") };
        assert!(states.len() <= n && states.iter().all(|s| s.visible.is_empty()));

        let mut tags = vec![tag(73, &n.to_string())];
        tags.extend((0..n).flat_map(|_| [tag(331, "1F"), tag(74, &n.to_string())]));
        let (_, ActionKind::Stretch { vertices, .. }) = stretch(&tags) else { panic!("a stretch action") };
        let ints: usize = vertices.iter().map(|(_, v)| v.len()).sum();
        assert!(ints <= 16 * tags.len() + 1024, "{ints}");
    }
}
