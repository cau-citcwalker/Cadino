//! Authoring dynamic blocks: what the Block Editor's BPARAMETER, BACTION, BVSTATE, BVSHOW,
//! BVHIDE and BGRIPSET write.
//!
//! A dynamic block is written the way [`super::dynamic_block`] reads one (and a file keeps it):
//! the block's BLOCK_RECORD gets an extension dictionary whose `ACAD_ENHANCEDBLOCK` entry is an
//! evaluation graph (`ACAD_EVALUATION_GRAPH`) listing parameter, grip and action objects
//! (`BLOCKLINEARPARAMETER`, `BLOCKLINEARGRIP`, `BLOCKSTRETCHACTION`…), each with its subclass
//! sections. Changing one rewrites only the sections that change, so data another program wrote
//! in the others stays as it was. Coordinates are block coordinates.
//!
//! ponytail: the graph lists its nodes but no edges (AutoCAD's graph also wires grips to
//! parameters to actions); add the edges if AutoCAD turns out to need them to evaluate a block
//! made here. A graph another program wired (edges after its nodes) is kept as it is: its
//! elements' properties can change, but adding or removing elements is refused ([`wired`]).

use std::collections::HashSet;
use std::sync::Arc;

use cadcraft_geom::{Vec2, Vec3};

use super::{
    ActionKind, DynamicBlock, LookupColumn, OWN_LOOKUP, Parameter, ParameterKind, SET1, SET2, SetCodes, ValueSet, VisibilityState, dynamic_block,
    section, sections,
};
use crate::foreign::{Foreign, ForeignObject};
use crate::{Drawing, EntityKind, EntityStore, Handle, RawTag};

/// Most parameters and actions one block is authored with.
pub(super) const MAX_ELEMENTS: usize = 2_000;

pub(super) fn tag(code: i32, value: impl Into<String>) -> RawTag {
    RawTag { code, value: value.into() }
}

fn point(code: i32, p: Vec3) -> [RawTag; 3] {
    [tag(code, p.x.to_string()), tag(code + 10, p.y.to_string()), tag(code + 20, p.z.to_string())]
}

pub(super) fn key(block: &str) -> (String, String) {
    ("BLOCK_RECORD".to_string(), block.trim().to_ascii_uppercase())
}

pub(super) fn foreign(d: &mut Drawing) -> &mut Foreign {
    Arc::make_mut(&mut d.foreign)
}

pub(super) fn object(d: &Drawing, h: Handle) -> Option<&ForeignObject> {
    d.foreign.objects.iter().find(|o| o.handle == h)
}

pub(super) fn object_mut(d: &mut Drawing, h: Handle) -> Option<&mut ForeignObject> {
    foreign(d).objects.iter_mut().find(|o| o.handle == h)
}

/// The handle the kept data uses for block `name`'s BLOCK_RECORD (made when it has none).
pub(super) fn record_target(d: &mut Drawing, name: &str) -> Handle {
    let k = key(name);
    if let Some(h) = d.foreign.targets.iter().find(|(_, v)| **v == k).map(|(h, _)| *h) {
        return h;
    }
    let h = d.new_handle();
    foreign(d).targets.insert(h, k);
    h
}

/// The entry `name` of dictionary object `dict`.
pub(super) fn entry(d: &Drawing, dict: Handle, name: &str) -> Option<Handle> {
    dict_entry(object(d, dict)?, name)
}

/// The entry `name` of dictionary object `o`.
pub(super) fn dict_entry(o: &ForeignObject, name: &str) -> Option<Handle> {
    let mut tags = o.tags.iter();
    while let Some(t) = tags.next() {
        if t.code == 3 && t.value.eq_ignore_ascii_case(name) {
            return tags.next().filter(|t| t.code == 350 || t.code == 360).and_then(|t| Handle::parse_hex(&t.value));
        }
    }
    None
}

/// Block `block`'s evaluation graph, when it has one.
pub(super) fn graph(d: &Drawing, block: &str) -> Option<Handle> {
    let xdict = d.foreign.records.get(&key(block))?.xdict?;
    entry(d, xdict, "ACAD_ENHANCEDBLOCK").filter(|g| object(d, *g).is_some_and(|o| o.kind.eq_ignore_ascii_case("ACAD_EVALUATION_GRAPH")))
}

/// Block `block`'s evaluation graph, made (with the record's extension dictionary) when it has
/// none.
fn ensure_graph(d: &mut Drawing, block: &str) -> Handle {
    if let Some(g) = graph(d, block) {
        return g;
    }
    let rec = record_target(d, block);
    let xdict = d.foreign.records.get(&key(block)).and_then(|x| x.xdict).filter(|x| object(d, *x).is_some());
    let g = d.new_handle();
    let xdict = match xdict {
        Some(x) => x,
        None => {
            let x = d.new_handle();
            let tags = vec![tag(5, x.hex()), tag(330, rec.hex()), tag(100, "AcDbDictionary"), tag(280, "1"), tag(281, "1")];
            let f = foreign(d);
            f.objects.push(ForeignObject { handle: x, kind: "DICTIONARY".into(), tags });
            f.records.entry(key(block)).or_default().xdict = Some(x);
            x
        }
    };
    let tags = vec![
        tag(5, g.hex()),
        tag(102, "{ACAD_REACTORS"),
        tag(330, xdict.hex()),
        tag(102, "}"),
        tag(330, xdict.hex()),
        tag(100, "AcDbEvalGraph"),
        tag(96, "0"),
        tag(97, "0"),
    ];
    if let Some(x) = object_mut(d, xdict) {
        x.tags.push(tag(3, "ACAD_ENHANCEDBLOCK"));
        x.tags.push(tag(360, g.hex()));
    }
    let f = foreign(d);
    f.objects.push(ForeignObject { handle: g, kind: "ACAD_EVALUATION_GRAPH".into(), tags });
    class(f, "ACAD_EVALUATION_GRAPH", "AcDbEvalGraph");
    g
}

/// A CLASS record for objects of type `dxf` (C++ class `cpp`), unless the drawing has one.
pub(super) fn class(f: &mut Foreign, dxf: &str, cpp: &str) {
    if f.classes.iter().any(|c| c.iter().any(|t| t.code == 1 && t.value.eq_ignore_ascii_case(dxf))) {
        return;
    }
    // ponytail: proxy flags 1152 (R13 format proxy, no proxy warning), as AutoCAD's own
    // ObjectDBX classes commonly have; check against AutoCAD if it ever objects.
    f.classes.push(vec![tag(1, dxf), tag(2, cpp), tag(3, "ObjectDBX Classes"), tag(90, "1152"), tag(91, "0"), tag(280, "0"), tag(281, "0")]);
}

/// The graph's member list entries: (node id, object).
pub(super) fn members(g: &ForeignObject) -> Vec<(i32, Handle)> {
    let mut out = Vec::new();
    let mut node = None;
    for t in section(&sections(&g.tags), "AcDbEvalGraph") {
        match t.code {
            95 => node = t.value.trim().parse().ok(),
            360 => {
                if let (Some(n), Some(h)) = (node.take(), Handle::parse_hex(&t.value)) {
                    out.push((n, h));
                }
            }
            _ => {}
        }
    }
    out
}

/// The group codes of one node record of a graph, as written here.
const NODE: [i32; 8] = [91, 93, 95, 360, 92, 92, 92, 92];

/// Where graph `o`'s node records are: from its first 91 to the end of the last record laid out
/// as [`NODE`]. Anything after them is another program's wiring (edges between the nodes).
fn node_span(o: &ForeignObject) -> (usize, usize) {
    let start = o.tags.iter().position(|t| t.code == 91).unwrap_or(o.tags.len());
    let mut end = start;
    while o.tags.get(end..end + NODE.len()).is_some_and(|w| w.iter().map(|t| t.code).eq(NODE)) {
        end += NODE.len();
    }
    (start, end)
}

/// Whether block `block`'s graph holds more than node records: edges another program (AutoCAD)
/// wired its parameters, grips and actions with. Adding or removing nodes would have to keep them
/// consistent, which isn't done here, so such blocks refuse it ([`wiring_kept`]).
pub(super) fn wired(d: &Drawing, block: &str) -> bool {
    graph(d, block).and_then(|g| object(d, g)).is_some_and(wired_graph)
}

/// Whether graph object `o` holds more than node records ([`wired`]).
pub(super) fn wired_graph(o: &ForeignObject) -> bool {
    node_span(o).1 < o.tags.len()
}

/// Whether block `block` still has a record of Cadino's own data that wasn't read in (a newer
/// version's, or a damaged one: [`super::file::from_file`] reads the others in). Changing the
/// block's elements would lose what it holds, so such blocks refuse it.
fn unread_record(d: &Drawing, block: &str) -> bool {
    d.foreign.records.get(&key(block)).and_then(|x| x.xdict).and_then(|x| entry(d, x, super::file::KEY)).is_some()
}

const NEWER_RECORD: &str =
    "This dynamic block keeps data saved by a newer version of Cadino: adding or removing its parameters and actions here would lose it.";

/// Refuse to add or remove elements of a block whose graph another program wired ([`wired`]), or
/// that keeps Cadino data this version can't read ([`unread_record`]).
fn wiring_kept(d: &Drawing, block: &str) -> Result<(), String> {
    if wired(d, block) {
        return Err("This dynamic block was made in another program, which wired its parameters and actions together: adding or removing them here isn't available yet (their properties and visibility states can be changed).".into());
    }
    if unread_record(d, block) {
        return Err(NEWER_RECORD.into());
    }
    Ok(())
}

/// Add `(node, object)` to graph `g`'s members, after its node records.
pub(super) fn add_member(d: &mut Drawing, g: Handle, node: i32, h: Handle) {
    let Some(o) = object_mut(d, g) else { return };
    let (start, end) = node_span(o);
    let n = (end - start) / NODE.len();
    let mut rec = vec![tag(91, n.to_string()), tag(93, "32"), tag(95, node.to_string()), tag(360, h.hex())];
    rec.extend((0..4).map(|_| tag(92, "-1")));
    o.tags.splice(end..end, rec);
    count_members(o);
}

/// Graph `o`'s node counts after its member list changed. A graph with more than node records
/// (another program's: [`wired`]) keeps the counts it has.
fn count_members(o: &mut ForeignObject) {
    let (start, end) = node_span(o);
    if end < o.tags.len() {
        return;
    }
    let n = ((end - start) / NODE.len()).to_string();
    for t in o.tags.iter_mut().filter(|t| t.code == 96 || t.code == 97) {
        t.value.clone_from(&n);
    }
}

/// Remove the members `gone` from graph `g` (its node records, renumbered). A graph another
/// program wired ([`wired`]) is left as it is: callers refuse first ([`wiring_kept`]).
pub(super) fn remove_members(d: &mut Drawing, g: Handle, gone: &HashSet<Handle>) {
    edit_members(d, g, |entries| {
        entries.retain(|e| !e.iter().any(|t| t.code == 360 && Handle::parse_hex(&t.value).is_some_and(|h| gone.contains(&h))))
    });
}

/// Put graph `g`'s node records in node id order (the order they were added in here).
pub(super) fn sort_members(d: &mut Drawing, g: Handle) {
    let node = |e: &Vec<RawTag>| e.iter().find(|t| t.code == 95).and_then(|t| t.value.trim().parse::<i32>().ok()).unwrap_or(i32::MAX);
    edit_members(d, g, |entries| entries.sort_by_key(node));
}

/// Change graph `g`'s node records with `f`, then number them again. A graph another program
/// wired ([`wired`]) is left as it is.
fn edit_members(d: &mut Drawing, g: Handle, f: impl FnOnce(&mut Vec<Vec<RawTag>>)) {
    let Some(o) = object_mut(d, g) else { return };
    let (start, end) = node_span(o);
    if end < o.tags.len() {
        return;
    }
    let mut entries: Vec<Vec<RawTag>> = o.tags.drain(start..end).collect::<Vec<_>>().chunks(NODE.len()).map(<[RawTag]>::to_vec).collect();
    f(&mut entries);
    for (i, e) in entries.iter_mut().enumerate() {
        if let Some(t) = e.iter_mut().find(|t| t.code == 91) {
            t.value = i.to_string();
        }
    }
    o.tags.splice(start..start, entries.into_iter().flatten());
    count_members(o);
}

/// The next free node id of graph `g`.
fn next_node(d: &Drawing, g: Handle) -> i32 {
    object(d, g).map_or(0, |o| members(o).iter().map(|(n, _)| *n).max().unwrap_or(0)).saturating_add(1)
}

/// The groups every graph member starts with: handle, owner (the graph), its node id and its
/// element name.
fn head(h: Handle, g: Handle, node: i32, element: &str) -> Vec<RawTag> {
    vec![
        tag(5, h.hex()),
        tag(330, g.hex()),
        tag(100, "AcDbEvalExpr"),
        tag(90, node.to_string()),
        tag(98, "33"),
        tag(99, "232"),
        tag(100, "AcDbBlockElement"),
        tag(300, element),
        tag(98, "33"),
        tag(99, "232"),
        tag(1071, "0"),
    ]
}

/// Replace object `h`'s section `marker` (its groups after the subclass marker up to the next
/// one) with `body`, adding the section at the end when the object has none.
pub(super) fn replace_section(d: &mut Drawing, h: Handle, marker: &str, body: Vec<RawTag>) {
    let Some(o) = object_mut(d, h) else { return };
    let at = o.tags.iter().position(|t| t.code == 100 && t.value.eq_ignore_ascii_case(marker));
    match at {
        Some(i) => {
            let end = o.tags.iter().skip(i + 1).position(|t| t.code == 100).map_or(o.tags.len(), |j| i + 1 + j);
            o.tags.splice(i + 1..end, body);
        }
        None => {
            o.tags.push(tag(100, marker));
            o.tags.extend(body);
        }
    }
}

/// Patch object `h`'s section `marker` with `body`: each group code `body` has replaces the
/// section's groups with that code, in order (a longer run of a code grows after its last one, a
/// shorter one drops the rest), so the groups `body` doesn't have, and the codes in `keep` (what
/// `body` writes as placeholders), stay as the file had them. A section the object lacks is added.
fn patch_section(d: &mut Drawing, h: Handle, marker: &str, body: Vec<RawTag>, keep: &[i32]) {
    let Some(o) = object_mut(d, h) else { return };
    let Some(i) = o.tags.iter().position(|t| t.code == 100 && t.value.eq_ignore_ascii_case(marker)) else {
        o.tags.push(tag(100, marker));
        o.tags.extend(body);
        return;
    };
    let end = o.tags.iter().skip(i + 1).position(|t| t.code == 100).map_or(o.tags.len(), |j| i + 1 + j);
    let mut sec: Vec<RawTag> = o.tags.get(i + 1..end).map(<[RawTag]>::to_vec).unwrap_or_default();
    let mut codes: Vec<i32> = body.iter().map(|t| t.code).filter(|c| !keep.contains(c)).collect();
    codes.dedup();
    for code in codes {
        let new: Vec<RawTag> = body.iter().filter(|t| t.code == code).cloned().collect();
        let at: Vec<usize> = sec.iter().enumerate().filter(|(_, t)| t.code == code).map(|(k, _)| k).collect();
        let Some(&last) = at.last() else {
            sec.extend(new);
            continue;
        };
        for (k, t) in at.iter().zip(&new) {
            if let Some(slot) = sec.get_mut(*k) {
                slot.value.clone_from(&t.value);
            }
        }
        if new.len() > at.len() {
            let more: Vec<RawTag> = new.get(at.len()..).map(<[RawTag]>::to_vec).unwrap_or_default();
            sec.splice(last + 1..last + 1, more);
        } else {
            for k in at.iter().skip(new.len()).rev() {
                sec.remove(*k);
            }
        }
    }
    o.tags.splice(i + 1..end, sec);
}

/// The codes [`kind_section`] writes as placeholders for kind `k` (not read back): a patch keeps
/// the file's.
fn placeholders(k: &ParameterKind) -> &'static [i32] {
    match k {
        ParameterKind::Linear { .. } => &[140, 307],
        ParameterKind::Polar { .. } | ParameterKind::Xy { .. } => &[140, 307, 410],
        ParameterKind::Rotation { .. } => &[307],
        ParameterKind::Lookup { .. } => &[94],
        _ => &[],
    }
}

/// The codes [`points_section`] writes as placeholders.
const POINT_PLACEHOLDERS: &[i32] = &[170, 171, 172, 173, 174];

/// The DXF type, C++ class and element name of a parameter kind, and its grips' type and class.
fn parameter_type(k: &ParameterKind) -> (&'static str, &'static str, &'static str, Option<(&'static str, &'static str)>) {
    match k {
        ParameterKind::Linear { .. } => {
            ("BLOCKLINEARPARAMETER", "AcDbBlockLinearParameter", "Linear", Some(("BLOCKLINEARGRIP", "AcDbBlockLinearGrip")))
        }
        ParameterKind::Polar { .. } => ("BLOCKPOLARPARAMETER", "AcDbBlockPolarParameter", "Polar", Some(("BLOCKPOLARGRIP", "AcDbBlockPolarGrip"))),
        ParameterKind::Xy { .. } => ("BLOCKXYPARAMETER", "AcDbBlockXYParameter", "XY", Some(("BLOCKXYGRIP", "AcDbBlockXYGrip"))),
        ParameterKind::Rotation { .. } => {
            ("BLOCKROTATIONPARAMETER", "AcDbBlockRotationParameter", "Rotation", Some(("BLOCKROTATIONGRIP", "AcDbBlockRotationGrip")))
        }
        ParameterKind::Alignment { .. } => {
            ("BLOCKALIGNMENTPARAMETER", "AcDbBlockAlignmentParameter", "Alignment", Some(("BLOCKALIGNMENTGRIP", "AcDbBlockAlignmentGrip")))
        }
        ParameterKind::Flip { .. } => ("BLOCKFLIPPARAMETER", "AcDbBlockFlipParameter", "Flip", Some(("BLOCKFLIPGRIP", "AcDbBlockFlipGrip"))),
        ParameterKind::Point { .. } => ("BLOCKPOINTPARAMETER", "AcDbBlockPointParameter", "Point", Some(("BLOCKXYGRIP", "AcDbBlockXYGrip"))),
        ParameterKind::Visibility { .. } => {
            ("BLOCKVISIBILITYPARAMETER", "AcDbBlockVisibilityParameter", "Visibility", Some(("BLOCKVISIBILITYGRIP", "AcDbBlockVisibilityGrip")))
        }
        ParameterKind::Lookup { .. } => {
            ("BLOCKLOOKUPPARAMETER", "AcDbBlockLookUpParameter", "Lookup", Some(("BLOCKLOOKUPGRIP", "AcDbBlockLookupGrip")))
        }
        ParameterKind::BasePoint { .. } | ParameterKind::Other { .. } => {
            ("BLOCKBASEPOINTPARAMETER", "AcDbBlockBasepointParameter", "Base Point", None)
        }
    }
}

/// The most grips a parameter kind has (AutoCAD's choices go up to it).
pub fn max_grips(k: &ParameterKind) -> usize {
    match k {
        ParameterKind::Linear { .. } | ParameterKind::Polar { .. } => 2,
        ParameterKind::Xy { .. } => 4,
        ParameterKind::BasePoint { .. } | ParameterKind::Other { .. } => 0,
        _ => 1,
    }
}

fn two_point(k: &ParameterKind) -> bool {
    matches!(
        k,
        ParameterKind::Linear { .. }
            | ParameterKind::Polar { .. }
            | ParameterKind::Xy { .. }
            | ParameterKind::Rotation { .. }
            | ParameterKind::Alignment { .. }
            | ParameterKind::Flip { .. }
    )
}

fn value_set_tags(v: &ValueSet, k: SetCodes) -> Vec<RawTag> {
    let [a, b, i] = k.limits;
    let mut out = vec![
        tag(k.description, ""),
        tag(k.flags, v.flags.to_string()),
        tag(a, v.minimum.to_string()),
        tag(b, v.maximum.to_string()),
        tag(i, v.increment.to_string()),
        tag(k.count, v.list.len().to_string()),
    ];
    out.extend(v.list.iter().map(|x| tag(k.value, x.to_string())));
    out
}

/// The point section of a parameter: a two-point parameter's points and grips (the base grip
/// first), a one-point parameter's point and grip.
fn points_section(k: &ParameterKind, grips: &[i32]) -> (&'static str, Vec<RawTag>) {
    let (base, end) = k.points().unwrap_or_default();
    let g = |i: usize| grips.get(i).copied().unwrap_or(0).to_string();
    if two_point(k) {
        let mut t: Vec<RawTag> = point(1010, base).into_iter().chain(point(1011, end)).collect();
        t.push(tag(170, "4"));
        t.extend((0..4).map(|i| tag(91, g(i))));
        t.extend([tag(171, "0"), tag(172, "0"), tag(173, "0"), tag(174, "0")]);
        let mid = matches!(k, ParameterKind::Linear { from_midpoint: true, .. });
        t.push(tag(177, if mid { "1" } else { "0" }));
        ("AcDbBlock2PtParameter", t)
    } else {
        let mut t: Vec<RawTag> = point(1010, base).into();
        t.extend([tag(93, g(0)), tag(170, "0"), tag(171, "0")]);
        ("AcDbBlock1PtParameter", t)
    }
}

/// A parameter's own section (after its point section).
fn kind_section(k: &ParameterKind) -> (&'static str, Vec<RawTag>) {
    let s = |v: &str| v.to_string();
    match k {
        ParameterKind::Linear { label, description, values, .. } => {
            let mut t = vec![tag(305, s(label)), tag(306, s(description)), tag(140, "0")];
            t.extend(value_set_tags(values, SET1));
            ("AcDbBlockLinearParameter", t)
        }
        ParameterKind::Polar { label, description, label2, description2, values, values2, .. }
        | ParameterKind::Xy { label, description, label2, description2, values, values2, .. } => {
            let mut t = vec![tag(305, s(label)), tag(306, s(description)), tag(308, s(label2)), tag(309, s(description2)), tag(140, "0")];
            t.extend(value_set_tags(values, SET1));
            t.extend(value_set_tags(values2, SET2));
            (if matches!(k, ParameterKind::Polar { .. }) { "AcDbBlockPolarParameter" } else { "AcDbBlockXYParameter" }, t)
        }
        ParameterKind::Rotation { label, description, base_angle, values, .. } => {
            let mut t = vec![tag(305, s(label)), tag(306, s(description)), tag(140, base_angle.to_string())];
            t.extend(value_set_tags(values, SET1));
            ("AcDbBlockRotationParameter", t)
        }
        ParameterKind::Alignment { perpendicular, .. } => ("AcDbBlockAlignmentParameter", vec![tag(280, if *perpendicular { "1" } else { "0" })]),
        ParameterKind::Flip { label, description, label_at, states, .. } => {
            let [a, b] = states;
            let mut t = vec![tag(305, s(label)), tag(306, s(description)), tag(307, s(a)), tag(308, s(b))];
            t.extend(point(1012, *label_at));
            ("AcDbBlockFlipParameter", t)
        }
        ParameterKind::Point { label, description, at } => {
            let mut t = vec![tag(303, s(label)), tag(304, s(description))];
            t.extend(point(1011, *at));
            ("AcDbBlockPointParameter", t)
        }
        ParameterKind::Visibility { label, description, controlled, states, .. } => {
            ("AcDbBlockVisibilityParameter", visibility_tags(label, description, controlled, states))
        }
        ParameterKind::Lookup { label, description, .. } => {
            ("AcDbBlockLookUpParameter", vec![tag(94, "0"), tag(303, s(label)), tag(304, s(description))])
        }
        ParameterKind::BasePoint { at } => ("AcDbBlockBasepointParameter", point(1011, *at).into()),
        ParameterKind::Other { .. } => ("AcDbBlockBasepointParameter", Vec::new()),
    }
}

/// A visibility parameter's section, as [`super::visibility`] reads it.
fn visibility_tags(label: &str, description: &str, controlled: &[Handle], states: &[VisibilityState]) -> Vec<RawTag> {
    let mut t = vec![tag(281, "1"), tag(301, label), tag(302, description), tag(91, "0"), tag(93, controlled.len().to_string())];
    t.extend(controlled.iter().map(|h| tag(331, h.hex())));
    t.push(tag(92, states.len().to_string()));
    for s in states {
        t.push(tag(303, s.name.clone()));
        t.push(tag(94, s.visible.len().to_string()));
        t.extend(s.visible.iter().map(|h| tag(332, h.hex())));
        t.push(tag(95, s.active.len().to_string()));
        t.extend(s.active.iter().map(|h| tag(333, h.hex())));
    }
    t
}

/// Every element of block `m` a visibility state lists as active: its parameters, grips and
/// actions other than the visibility parameter and its grip.
fn active_elements(m: &DynamicBlock) -> Vec<Handle> {
    let vis: Option<&Parameter> = m.parameters.iter().find(|p| matches!(p.kind, ParameterKind::Visibility { .. }));
    let vis_grips: HashSet<i32> = vis.map(|p| p.grips.iter().copied().collect()).unwrap_or_default();
    let mut out: Vec<Handle> = m.parameters.iter().filter(|p| Some(p.handle) != vis.map(|v| v.handle)).map(|p| p.handle).collect();
    out.extend(m.grip_objects.iter().filter(|(n, _)| !vis_grips.contains(n)).map(|(_, h)| *h));
    out.extend(m.actions.iter().map(|a| a.handle));
    out
}

/// Check that block `block` can take one more element.
fn room(d: &Drawing, block: &str) -> Result<Option<DynamicBlock>, String> {
    let m = dynamic_block(d, block);
    if m.as_ref().is_some_and(|m| m.parameters.len() + m.actions.len() >= MAX_ELEMENTS) {
        return Err(format!("A block has at most {MAX_ELEMENTS} parameters and actions."));
    }
    Ok(m)
}

/// A property name no parameter of `m` gives yet: `base` followed by the first free number
/// ("Distance1"), or `base` itself when `numbered` is false and it is free.
pub fn next_label(m: Option<&DynamicBlock>, base: &str, numbered: bool) -> String {
    let used: HashSet<String> =
        m.map(|m| m.parameters.iter().flat_map(|p| p.kind.labels()).map(str::to_ascii_uppercase).collect()).unwrap_or_default();
    if !numbered && !used.contains(&base.to_ascii_uppercase()) {
        return base.to_string();
    }
    (1..=MAX_ELEMENTS + 1).map(|i| format!("{base}{i}")).find(|l| !used.contains(&l.to_ascii_uppercase())).unwrap_or_else(|| base.to_string())
}

/// Add parameter `kind` with `grips` grips (capped by [`max_grips`]) to block `block`; `chain`
/// is its Chain Actions property. Returns its node id.
pub fn add_parameter(d: &mut Drawing, block: &str, kind: ParameterKind, grips: usize, chain: bool) -> Result<i32, String> {
    wiring_kept(d, block)?;
    let m = room(d, block)?;
    let unique = |k: &ParameterKind| matches!(k, ParameterKind::Visibility { .. } | ParameterKind::BasePoint { .. });
    if unique(&kind) && m.as_ref().is_some_and(|m| m.parameters.iter().any(|p| std::mem::discriminant(&p.kind) == std::mem::discriminant(&kind))) {
        let what = if matches!(kind, ParameterKind::Visibility { .. }) { "visibility" } else { "base point" };
        return Err(format!("A block has one {what} parameter; it has one already."));
    }
    if let Some(p) = kind
        .labels()
        .into_iter()
        .find(|l| m.as_ref().is_some_and(|m| m.parameters.iter().flat_map(|p| p.kind.labels()).any(|o| o.eq_ignore_ascii_case(l))))
    {
        return Err(format!("A parameter is already labelled \"{p}\"."));
    }
    if kind.points().is_none_or(|(a, b)| !(a.is_finite() && b.is_finite())) {
        return Err("Invalid parameter points.".into());
    }
    let g = ensure_graph(d, block);
    let node = next_node(d, g);
    let (dxf, cpp, element, grip_type) = parameter_type(&kind);
    let n = if grip_type.is_some() { grips.min(max_grips(&kind)) } else { 0 };
    // Grip ids follow the parameter's; a two-point parameter's single grip is its end grip.
    let ids: Vec<i32> = (0..n).map(|i| node.saturating_add(1 + i as i32)).collect();
    let grip_ids: Vec<i32> = if two_point(&kind) && n == 1 { vec![0, ids.first().copied().unwrap_or(0)] } else { ids.clone() };
    let h = d.new_handle();
    let mut tags = head(h, g, node, element);
    tags.extend([tag(100, "AcDbBlockParameter"), tag(280, "1"), tag(281, if chain { "1" } else { "0" })]);
    for (marker, body) in [points_section(&kind, &grip_ids), kind_section(&kind)] {
        tags.push(tag(100, marker));
        tags.extend(body);
    }
    let mut new = vec![ForeignObject { handle: h, kind: dxf.into(), tags }];
    let (base, end) = kind.points().unwrap_or_default();
    for (i, id) in ids.iter().enumerate() {
        let Some((gdxf, _)) = grip_type else { break };
        let gh = d.new_handle();
        // The grip sits at the end point (a single grip), else at the base then the end.
        let at = if two_point(&kind) && (n == 1 || i == 1) { end } else { base };
        let mut t = head(gh, g, *id, "Grip");
        t.extend([tag(100, "AcDbBlockGrip"), tag(91, node.to_string()), tag(92, i.to_string())]);
        t.extend(point(1010, at));
        t.push(tag(280, "0"));
        t.push(tag(93, "-1"));
        new.push(ForeignObject { handle: gh, kind: gdxf.into(), tags: t });
    }
    let handles: Vec<(i32, Handle)> = std::iter::once((node, h)).chain(ids.iter().copied().zip(new.iter().skip(1).map(|o| o.handle))).collect();
    let f = foreign(d);
    class(f, dxf, cpp);
    if let Some((gdxf, gcpp)) = grip_type {
        class(f, gdxf, gcpp);
    }
    f.objects.extend(new);
    for (id, oh) in &handles {
        add_member(d, g, *id, *oh);
    }
    // New elements are active in every visibility state (a new visibility parameter's states
    // list what is there).
    let added: Vec<Handle> = handles.iter().map(|(_, h)| *h).collect();
    if matches!(kind, ParameterKind::Visibility { .. }) {
        if let Some(m) = dynamic_block(d, block) {
            let active = active_elements(&m);
            edit_visibility(d, block, |_, states| {
                for s in states.iter_mut() {
                    s.active.clone_from(&active);
                }
                Ok(())
            })?;
        }
    } else {
        activate(d, block, &added);
    }
    Ok(node)
}

/// Add `hs` to the active elements of every visibility state of block `block`.
fn activate(d: &mut Drawing, block: &str, hs: &[Handle]) {
    let has = dynamic_block(d, block).is_some_and(|m| m.parameters.iter().any(|p| matches!(p.kind, ParameterKind::Visibility { .. })));
    if has {
        let _ = edit_visibility(d, block, |_, states| {
            for s in states.iter_mut() {
                for h in hs {
                    if !s.active.contains(h) {
                        s.active.push(*h);
                    }
                }
            }
            Ok(())
        });
    }
}

/// What a new action is: its kind, the parameter it follows with the values it reads, its
/// objects and where the Block Editor shows it.
pub struct NewAction {
    pub kind: ActionKind,
    /// The parameter values it follows: (parameter node, value name), at most two.
    pub connections: Vec<(i32, String)>,
    pub selection: Vec<Handle>,
    pub at: Vec3,
}

/// The DXF type, C++ class and element name of an action kind.
fn action_type(k: &ActionKind) -> (&'static str, &'static str, &'static str) {
    match k {
        ActionKind::Stretch { .. } => ("BLOCKSTRETCHACTION", "AcDbBlockStretchAction", "Stretch"),
        ActionKind::PolarStretch { .. } => ("BLOCKPOLARSTRETCHACTION", "AcDbBlockPolarStretchAction", "Polar Stretch"),
        ActionKind::Lookup { .. } => ("BLOCKLOOKUPACTION", "AcDbBlockLookupAction", "Lookup"),
        ActionKind::Move { .. } => ("BLOCKMOVEACTION", "AcDbBlockMoveAction", "Move"),
        ActionKind::Scale { .. } => ("BLOCKSCALEACTION", "AcDbBlockScaleAction", "Scale"),
        ActionKind::Rotate { .. } => ("BLOCKROTATEACTION", "AcDbBlockRotateAction", "Rotate"),
        ActionKind::Flip => ("BLOCKFLIPACTION", "AcDbBlockFlipAction", "Flip"),
        ActionKind::Array { .. } => ("BLOCKARRAYACTION", "AcDbBlockArrayAction", "Array"),
        ActionKind::Other { .. } => ("BLOCKMOVEACTION", "AcDbBlockMoveAction", "Move"),
    }
}

/// Two connections as the move and stretch actions write them (92, 93 ids; 301, 302 names).
fn connection_tags(c: &[(i32, String)]) -> Vec<RawTag> {
    let get = |i: usize| c.get(i).cloned().or_else(|| c.first().cloned()).unwrap_or((0, String::new()));
    let ((n1, v1), (n2, v2)) = (get(0), get(1));
    vec![tag(92, n1.to_string()), tag(93, n2.to_string()), tag(301, v1), tag(302, v2)]
}

/// A frame and the vertices it moves, as the stretch actions write them.
fn frame_tags(frame: &[Vec2], vertices: &[(Handle, Vec<i32>)]) -> Vec<RawTag> {
    let mut t = vec![tag(72, frame.len().to_string())];
    for p in frame {
        t.push(tag(1011, p.x.to_string()));
        t.push(tag(1021, p.y.to_string()));
    }
    t.push(tag(73, vertices.len().to_string()));
    for (h, idx) in vertices {
        t.push(tag(331, h.hex()));
        t.push(tag(74, idx.len().to_string()));
        t.extend(idx.iter().map(|i| tag(94, i.to_string())));
    }
    t
}

fn action_section(a: &NewAction) -> (&'static str, Vec<RawTag>) {
    let c = connection_tags(&a.connections);
    let (_, cpp, _) = action_type(&a.kind);
    let t = match &a.kind {
        ActionKind::Stretch { frame, vertices, multiplier, angle_offset } => {
            let mut t = c;
            t.extend(frame_tags(frame, vertices));
            t.extend([tag(75, "0"), tag(140, multiplier.to_string()), tag(141, angle_offset.to_string()), tag(280, "0")]);
            t
        }
        ActionKind::PolarStretch { frame, vertices, rotate } => {
            let mut t = c;
            t.extend(frame_tags(frame, vertices));
            t.push(tag(75, rotate.len().to_string()));
            t.extend(rotate.iter().map(|h| tag(332, h.hex())));
            t
        }
        ActionKind::Move { multiplier, angle_offset } => {
            let mut t = c;
            t.extend([tag(140, multiplier.to_string()), tag(141, angle_offset.to_string()), tag(280, "0")]);
            t
        }
        ActionKind::Scale { base } | ActionKind::Rotate { base } => {
            let mut t = c;
            t.push(tag(280, if base.is_some() { "1" } else { "0" }));
            t.extend(point(1011, base.unwrap_or_default()));
            t
        }
        ActionKind::Array { column, row } => {
            let mut t = c;
            t.extend([tag(140, column.to_string()), tag(141, row.to_string())]);
            t
        }
        ActionKind::Lookup { columns, rows } => lookup_tags(columns, rows),
        ActionKind::Flip | ActionKind::Other { .. } => c,
    };
    (cpp, t)
}

/// Cadino's own section of a new action, when its kind has one.
fn own_action_section(k: &ActionKind) -> Option<(&'static str, Vec<RawTag>)> {
    match k {
        ActionKind::Lookup { columns, .. } => Some((OWN_LOOKUP, own_lookup_tags(columns))),
        _ => None,
    }
}

/// A lookup table, as [`super::lookup`] reads it.
fn lookup_tags(columns: &[LookupColumn], rows: &[Vec<String>]) -> Vec<RawTag> {
    let mut t = vec![tag(92, rows.len().to_string()), tag(93, columns.len().to_string()), tag(301, "")];
    for r in rows {
        t.extend((0..columns.len()).map(|i| tag(302, r.get(i).cloned().unwrap_or_default())));
    }
    for c in columns {
        t.extend([
            tag(303, ""),
            tag(94, c.node.to_string()),
            tag(95, "1"),
            tag(96, "0"),
            tag(282, if c.lookup { "1" } else { "0" }),
            tag(305, c.unmatched.clone()),
            tag(281, if c.lookup { "1" } else { "0" }),
            tag(304, c.property.clone()),
        ]);
    }
    t.push(tag(280, "1"));
    t
}

/// A lookup table's Cadino section ([`OWN_LOOKUP`]): each column's Allow reverse lookup.
fn own_lookup_tags(columns: &[LookupColumn]) -> Vec<RawTag> {
    let mut t = vec![tag(90, columns.len().to_string())];
    t.extend(columns.iter().map(|c| tag(290, if c.reverse { "1" } else { "0" })));
    t
}

/// Give lookup action `node` of block `block` the table `columns` × `rows` (BLOOKUPTABLE, the
/// Property Lookup Table dialog). Columns must name the block's parameters.
pub fn set_lookup(d: &mut Drawing, block: &str, node: i32, columns: Vec<LookupColumn>, rows: Vec<Vec<String>>) -> Result<(), String> {
    let m = dynamic_block(d, block).ok_or("The block has no dynamic data.")?;
    // ponytail: a table another program wrote may hold groups not modelled here, so it isn't
    // rewritten (the dialog shows it); allow it once AutoCAD's whole layout is known.
    if wired(d, block) {
        return Err("This dynamic block was made in another program: its lookup tables can't be changed here yet.".into());
    }
    if unread_record(d, block) {
        return Err(NEWER_RECORD.into());
    }
    let a = m.actions.iter().find(|a| a.node == node).ok_or("No such action.")?;
    if !matches!(a.kind, ActionKind::Lookup { .. }) {
        return Err(format!("{} is not a lookup action.", a.name));
    }
    if let Some(c) = columns.iter().find(|c| !m.parameters.iter().any(|p| p.node == c.node)) {
        return Err(format!("A lookup table column names parameter {}, which the block doesn't have.", c.node));
    }
    let h = a.handle;
    replace_section(d, h, "AcDbBlockLookupAction", lookup_tags(&columns, &rows));
    replace_section(d, h, OWN_LOOKUP, own_lookup_tags(&columns));
    Ok(())
}

/// An action name block `m` doesn't use yet: `base`, else `base` with the first free number.
fn action_name(m: Option<&DynamicBlock>, base: &str) -> String {
    let used: HashSet<String> = m.map(|m| m.actions.iter().map(|a| a.name.to_ascii_uppercase()).collect()).unwrap_or_default();
    std::iter::once(base.to_string())
        .chain((1..=MAX_ELEMENTS + 1).map(|i| format!("{base}{i}")))
        .find(|n| !used.contains(&n.to_ascii_uppercase()))
        .unwrap_or_else(|| base.to_string())
}

/// Add action `a` to block `block`. Returns its node id and name.
pub fn add_action(d: &mut Drawing, block: &str, a: NewAction) -> Result<(i32, String), String> {
    let m = room(d, block)?.ok_or("The block has no parameters: add one first (BPARAMETER).")?;
    wiring_kept(d, block)?;
    if a.connections.is_empty() || a.connections.iter().any(|(n, _)| !m.parameters.iter().any(|p| p.node == *n)) {
        return Err("An action needs one of the block's parameters.".into());
    }
    if !a.at.is_finite() {
        return Err("Invalid action location.".into());
    }
    let g = ensure_graph(d, block);
    let node = next_node(d, g);
    let (dxf, cpp, element) = action_type(&a.kind);
    let name = action_name(Some(&m), element);
    let h = d.new_handle();
    let mut tags = head(h, g, node, &name);
    tags.extend([tag(100, "AcDbBlockAction"), tag(70, "0"), tag(71, a.selection.len().to_string())]);
    tags.extend(a.selection.iter().map(|s| tag(330, s.hex())));
    tags.extend(point(1010, a.at));
    let (marker, body) = action_section(&a);
    tags.push(tag(100, marker));
    tags.extend(body);
    if let Some((marker, body)) = own_action_section(&a.kind) {
        tags.push(tag(100, marker));
        tags.extend(body);
    }
    let f = foreign(d);
    class(f, dxf, cpp);
    f.objects.push(ForeignObject { handle: h, kind: dxf.into(), tags });
    add_member(d, g, node, h);
    activate(d, block, &[h]);
    Ok((node, name))
}

/// Change parameter `node` of block `block` with `f`: its name, label, description, value sets,
/// Chain Actions, points (its kind stays). Rewrites the sections that change.
pub fn set_parameter(d: &mut Drawing, block: &str, node: i32, f: impl FnOnce(&mut Parameter) -> Result<(), String>) -> Result<(), String> {
    let m = dynamic_block(d, block).ok_or("The block has no dynamic data.")?;
    let mut p = m.parameters.iter().find(|p| p.node == node).cloned().ok_or("No such parameter.")?;
    let before = p.clone();
    f(&mut p)?;
    if std::mem::discriminant(&p.kind) != std::mem::discriminant(&before.kind) {
        return Err("A parameter's type can't change.".into());
    }
    let others: Vec<&str> = m.parameters.iter().filter(|o| o.node != node).flat_map(|o| o.kind.labels()).collect();
    if let Some(l) = p.kind.labels().into_iter().find(|l| others.iter().any(|o| o.eq_ignore_ascii_case(l))) {
        return Err(format!("A parameter is already labelled \"{l}\"."));
    }
    // Only the groups modelled here change: what another program wrote in the others stays.
    if p.element != before.element {
        patch_section(d, p.handle, "AcDbBlockElement", vec![tag(300, p.element.clone())], &[]);
    }
    if p.chain != before.chain || p.show_properties != before.show_properties {
        let body = vec![tag(280, if p.show_properties { "1" } else { "0" }), tag(281, if p.chain { "1" } else { "0" })];
        patch_section(d, p.handle, "AcDbBlockParameter", body, &[]);
    }
    if p.kind.points() != before.kind.points() {
        let (marker, body) = points_section(&p.kind, &p.grips);
        patch_section(d, p.handle, marker, body, POINT_PLACEHOLDERS);
    }
    if p.kind != before.kind {
        let (marker, body) = kind_section(&p.kind);
        patch_section(d, p.handle, marker, body, placeholders(&p.kind));
    }
    Ok(())
}

/// Give parameter `node` of block `block` `n` grips (BGRIPSET), capped by [`max_grips`].
pub fn set_grips(d: &mut Drawing, block: &str, node: i32, n: usize) -> Result<usize, String> {
    let m = dynamic_block(d, block).ok_or("The block has no dynamic data.")?;
    wiring_kept(d, block)?;
    let p = m.parameters.iter().find(|p| p.node == node).cloned().ok_or("No such parameter.")?;
    let n = n.min(max_grips(&p.kind));
    let g = graph(d, block).ok_or("The block has no dynamic data.")?;
    // The old grips go; new ones take free ids.
    let old: HashSet<Handle> = m.grip_objects.iter().filter(|(id, _)| *id != 0 && p.grips.contains(id)).map(|(_, h)| *h).collect();
    remove_members(d, g, &old);
    foreign(d).objects.retain(|o| !old.contains(&o.handle));
    let (_, _, _, grip_type) = parameter_type(&p.kind);
    let Some((gdxf, _)) = grip_type else { return Ok(0) };
    let first = next_node(d, g);
    let ids: Vec<i32> = (0..n).map(|i| first.saturating_add(i as i32)).collect();
    let (base, end) = p.kind.points().unwrap_or_default();
    let mut added = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let gh = d.new_handle();
        let at = if two_point(&p.kind) && (n == 1 || i == 1) { end } else { base };
        let mut t = head(gh, g, *id, "Grip");
        t.extend([tag(100, "AcDbBlockGrip"), tag(91, node.to_string()), tag(92, i.to_string())]);
        t.extend(point(1010, at));
        t.extend([tag(280, "0"), tag(93, "-1")]);
        foreign(d).objects.push(ForeignObject { handle: gh, kind: gdxf.into(), tags: t });
        add_member(d, g, *id, gh);
        added.push(gh);
    }
    let grip_ids: Vec<i32> = if two_point(&p.kind) && n == 1 { vec![0, ids.first().copied().unwrap_or(0)] } else { ids };
    let (marker, body) = points_section(&p.kind, &grip_ids);
    replace_section(d, p.handle, marker, body);
    // The grips are shown where the parameter is.
    let _ = edit_visibility(d, block, |_, states| {
        for s in states.iter_mut() {
            s.active.retain(|h| !old.contains(h));
            if s.active.contains(&p.handle) {
                s.active.extend(added.iter().copied());
            }
        }
        Ok(())
    });
    Ok(n)
}

/// Remove parameter or action `node` from block `block`, with a parameter's grips and the actions
/// that follow it (they can't run without it). Returns the names removed.
pub fn remove(d: &mut Drawing, block: &str, node: i32) -> Result<Vec<String>, String> {
    let m = dynamic_block(d, block).ok_or("The block has no dynamic data.")?;
    wiring_kept(d, block)?;
    let g = graph(d, block).ok_or("The block has no dynamic data.")?;
    let mut gone: HashSet<Handle> = HashSet::new();
    let mut names = Vec::new();
    if let Some(p) = m.parameters.iter().find(|p| p.node == node) {
        gone.insert(p.handle);
        names.push(p.kind.label().map_or_else(|| p.element.clone(), str::to_string));
        gone.extend(m.grip_objects.iter().filter(|(id, _)| *id != 0 && p.grips.contains(id)).map(|(_, h)| *h));
        for a in &m.actions {
            let follows = a.connections.iter().any(|(n, _)| *n == node)
                || matches!(&a.kind, ActionKind::Lookup { columns, .. } if columns.iter().any(|c| c.node == node));
            if follows {
                gone.insert(a.handle);
                names.push(a.name.clone());
            }
        }
    } else if let Some(a) = m.actions.iter().find(|a| a.node == node) {
        gone.insert(a.handle);
        names.push(a.name.clone());
    } else {
        return Err("No such parameter or action.".into());
    }
    remove_members(d, g, &gone);
    foreign(d).objects.retain(|o| !gone.contains(&o.handle));
    let _ = edit_visibility(d, block, |_, states| {
        for s in states.iter_mut() {
            s.active.retain(|h| !gone.contains(h));
        }
        Ok(())
    });
    Ok(names)
}

/// Change block `block`'s visibility parameter (its controlled objects and states) with `f`.
pub fn edit_visibility<R>(
    d: &mut Drawing,
    block: &str,
    f: impl FnOnce(&mut Vec<Handle>, &mut Vec<VisibilityState>) -> Result<R, String>,
) -> Result<R, String> {
    let m = dynamic_block(d, block).ok_or("The block has no visibility parameter: add one first (BPARAMETER Visibility).")?;
    let p = m
        .parameters
        .iter()
        .find(|p| matches!(p.kind, ParameterKind::Visibility { .. }))
        .ok_or("The block has no visibility parameter: add one first (BPARAMETER Visibility).")?;
    let ParameterKind::Visibility { label, description, controlled, states, .. } = &p.kind else { return Err("No visibility parameter.".into()) };
    let (mut controlled, mut states) = (controlled.clone(), states.clone());
    let r = f(&mut controlled, &mut states)?;
    let body = visibility_tags(label, description, &controlled, &states);
    replace_section(d, p.handle, "AcDbBlockVisibilityParameter", body);
    Ok(r)
}

/// How a new visibility state shows the block's objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewState {
    /// Every controlled object hidden.
    HideAll,
    /// Every object shown.
    ShowAll,
    /// As state `from` shows them (the current state).
    Current,
}

/// Add visibility state `name` to block `block`, its objects shown per `how` (`from` is the
/// current state's name for [`NewState::Current`]; `objects` are the block's objects, which
/// Hide all hides).
pub fn add_state(d: &mut Drawing, block: &str, name: &str, how: NewState, from: Option<&str>, objects: &[Handle]) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A visibility state needs a name.".into());
    }
    let m = dynamic_block(d, block).ok_or("The block has no visibility parameter: add one first (BPARAMETER Visibility).")?;
    let all = active_elements(&m);
    edit_visibility(d, block, |controlled, states| {
        if states.iter().any(|s| s.name.eq_ignore_ascii_case(name)) {
            return Err(format!("Visibility state \"{name}\" already exists."));
        }
        if states.len() >= MAX_ELEMENTS {
            return Err(format!("A visibility parameter has at most {MAX_ELEMENTS} states."));
        }
        let visible = match how {
            NewState::HideAll => Vec::new(),
            NewState::ShowAll => {
                // Showing everything: objects not controlled yet are shown in every state anyway.
                controlled.clone()
            }
            NewState::Current => from
                .and_then(|f| states.iter().find(|s| s.name.eq_ignore_ascii_case(f)))
                .or(states.first())
                .map(|s| s.visible.clone())
                .unwrap_or_else(|| controlled.clone()),
        };
        if how == NewState::HideAll {
            // Hide all hides every object: they all become controlled (shown in the other states).
            for h in objects {
                if !controlled.contains(h) {
                    controlled.push(*h);
                    for s in states.iter_mut() {
                        s.visible.push(*h);
                    }
                }
            }
        }
        states.push(VisibilityState { name: name.to_string(), visible, active: all });
        Ok(())
    })
}

/// Remove visibility state `name` (a visibility parameter keeps at least one).
pub fn delete_state(d: &mut Drawing, block: &str, name: &str) -> Result<(), String> {
    edit_visibility(d, block, |_, states| {
        let i = states.iter().position(|s| s.name.eq_ignore_ascii_case(name.trim())).ok_or_else(|| format!("No visibility state \"{name}\"."))?;
        if states.len() <= 1 {
            return Err("A visibility parameter keeps at least one state.".into());
        }
        states.remove(i);
        Ok(())
    })
}

/// Rename visibility state `old` to `new`.
pub fn rename_state(d: &mut Drawing, block: &str, old: &str, new: &str) -> Result<(), String> {
    let new = new.trim();
    if new.is_empty() {
        return Err("A visibility state needs a name.".into());
    }
    edit_visibility(d, block, |_, states| {
        if states.iter().any(|s| s.name.eq_ignore_ascii_case(new) && !s.name.eq_ignore_ascii_case(old.trim())) {
            return Err(format!("Visibility state \"{new}\" already exists."));
        }
        let s = states.iter_mut().find(|s| s.name.eq_ignore_ascii_case(old.trim())).ok_or_else(|| format!("No visibility state \"{old}\"."))?;
        s.name = new.to_string();
        Ok(())
    })
}

/// Move visibility state `name` up (`by` < 0) or down the list; the first is a new
/// reference's.
pub fn move_state(d: &mut Drawing, block: &str, name: &str, by: i32) -> Result<(), String> {
    edit_visibility(d, block, |_, states| {
        let i = states.iter().position(|s| s.name.eq_ignore_ascii_case(name.trim())).ok_or_else(|| format!("No visibility state \"{name}\"."))?;
        let j = i.saturating_add_signed(by as isize).min(states.len().saturating_sub(1));
        let s = states.remove(i);
        states.insert(j, s);
        Ok(())
    })
}

/// Show (`show`) or hide objects `hs` in visibility state `state`, or in every state when it is
/// `None` (BVSHOW, BVHIDE). An object no state controlled yet was shown in all of them.
pub fn show_objects(d: &mut Drawing, block: &str, hs: &[Handle], show: bool, state: Option<&str>) -> Result<(), String> {
    edit_visibility(d, block, |controlled, states| {
        if let Some(s) = state
            && !states.iter().any(|x| x.name.eq_ignore_ascii_case(s))
        {
            return Err(format!("No visibility state \"{s}\"."));
        }
        for h in hs {
            if !controlled.contains(h) {
                controlled.push(*h);
                for s in states.iter_mut() {
                    s.visible.push(*h);
                }
            }
        }
        for s in states.iter_mut().filter(|s| state.is_none_or(|n| s.name.eq_ignore_ascii_case(n))) {
            s.visible.retain(|h| !hs.contains(h));
            if show {
                s.visible.extend(hs.iter().copied());
            }
        }
        Ok(())
    })
}

/// Which objects of `store` visibility state `state` of `m` shows or hides (the first state for
/// `None` or a name it doesn't have): `(object, shown)` for each controlled object.
pub fn state_visibility(m: &DynamicBlock, state: Option<&str>) -> Vec<(Handle, bool)> {
    let Some(ParameterKind::Visibility { controlled, states, .. }) =
        m.parameters.iter().map(|p| &p.kind).find(|k| matches!(k, ParameterKind::Visibility { .. }))
    else {
        return Vec::new();
    };
    let s = state.and_then(|n| states.iter().find(|s| s.name.eq_ignore_ascii_case(n))).or(states.first());
    let shown: HashSet<Handle> = s.map(|s| s.visible.iter().copied().collect()).unwrap_or_default();
    controlled.iter().map(|h| (*h, shown.contains(h))).collect()
}

/// The vertices a stretch frame moves: of the objects `hs` of `store`, those with a stretch
/// point inside `frame` (two corners, or a polygon). A line's or polyline's partly inside
/// lists its vertices inside; any other object moves whole when all its grips are inside.
/// Returns (the objects acted on, the vertex lists).
pub fn stretch_vertices(store: &EntityStore, hs: &[Handle], frame: &[Vec2]) -> (Vec<Handle>, Vec<(Handle, Vec<i32>)>) {
    let inside = |p: Vec2| super::apply::in_frame(frame, p);
    let mut objects = Vec::new();
    let mut vertices = Vec::new();
    for h in hs {
        let Some(e) = store.get(*h) else { continue };
        let pts: Vec<Vec2> = match &e.kind {
            EntityKind::Line(l) => vec![l.a.xy(), l.b.xy()],
            EntityKind::LwPolyline(pl) => pl.vertices.iter().map(|v| v.p).collect(),
            // ponytail: other objects move whole when the frame holds all their grips; AutoCAD
            // also stretches an arc with one end inside, and moves a circle or text by its
            // centre or insertion point alone.
            _ => {
                let g = e.kind.grips();
                if !g.is_empty() && g.iter().all(|p| inside(*p)) {
                    objects.push(*h);
                }
                continue;
            }
        };
        let idx: Vec<i32> = pts.iter().enumerate().filter(|(_, p)| inside(**p)).filter_map(|(i, _)| i32::try_from(i).ok()).collect();
        if idx.is_empty() {
            continue;
        }
        objects.push(*h);
        if idx.len() < pts.len() {
            vertices.push((*h, idx));
        }
    }
    (objects, vertices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, Entity, Line};

    fn door() -> (Drawing, Vec<Handle>) {
        let mut d = Drawing::default();
        let mut b = Block::new("Door");
        let mut hs = Vec::new();
        for (a, e) in [((0.0, 0.0), (36.0, 0.0)), ((0.0, 0.0), (0.0, 36.0))] {
            let h = d.new_handle();
            b.entities.push(Entity::new(h, EntityKind::Line(Line { a: Vec3::new(a.0, a.1, 0.0), b: Vec3::new(e.0, e.1, 0.0) })));
            hs.push(h);
        }
        d.blocks.insert("Door".into(), Arc::new(b));
        (d, hs)
    }

    fn linear(label: &str) -> ParameterKind {
        ParameterKind::Linear {
            base: Vec3::ZERO,
            end: Vec3::new(36.0, 0.0, 0.0),
            label: label.into(),
            description: String::new(),
            values: ValueSet { flags: 8, list: vec![30.0, 36.0], ..Default::default() },
            from_midpoint: false,
        }
    }

    #[test]
    fn authored_elements_read_back() {
        let (mut d, hs) = door();
        let n = add_parameter(&mut d, "door", linear("Width"), 2, true).unwrap();
        assert!(add_parameter(&mut d, "Door", linear("width"), 1, false).is_err(), "labels are unique");
        let frame = vec![Vec2::new(30.0, -2.0), Vec2::new(40.0, 2.0)];
        let (sel, vertices) = stretch_vertices(&d.block("Door").unwrap().entities, &hs, &frame);
        assert_eq!((sel.as_slice(), vertices.as_slice()), (&hs[..1], &[(hs[0], vec![1])][..]));
        let kind = ActionKind::Stretch { frame, vertices, multiplier: 1.0, angle_offset: 0.0 };
        let conns = vec![(n, "EndXDelta".to_string()), (n, "EndYDelta".to_string())];
        let (_, name) = add_action(&mut d, "Door", NewAction { kind, connections: conns, selection: sel, at: Vec3::new(40.0, -6.0, 0.0) }).unwrap();
        assert_eq!(name, "Stretch");
        let m = dynamic_block(&d, "Door").unwrap();
        let p = &m.parameters[0];
        assert!(p.chain && p.kind == linear("Width") && p.grips.len() == 2 && p.grips.iter().all(|g| *g > n));
        assert_eq!(m.grip_objects.len(), 2);
        assert_eq!(m.actions[0].connections[0], (n, "EndXDelta".to_string()));

        // Visibility: states, objects shown per state, and the new grips active in every state.
        add_parameter(
            &mut d,
            "Door",
            ParameterKind::Visibility {
                at: Vec3::ZERO,
                label: "Visibility1".into(),
                description: String::new(),
                controlled: vec![],
                states: vec![VisibilityState { name: "Open".into(), ..Default::default() }],
            },
            1,
            false,
        )
        .unwrap();
        add_state(&mut d, "Door", "Closed", NewState::Current, Some("Open"), &hs).unwrap();
        show_objects(&mut d, "Door", &hs[1..], false, Some("Closed")).unwrap();
        let m = dynamic_block(&d, "Door").unwrap();
        assert_eq!(state_visibility(&m, Some("closed")), [(hs[1], false)]);
        assert_eq!(state_visibility(&m, None), [(hs[1], true)]);
        let ParameterKind::Visibility { states, .. } = &m.parameters[1].kind else { panic!() };
        assert!(states.iter().all(|s| s.active.contains(&m.parameters[0].handle) && s.active.contains(&m.actions[0].handle)));
        assert!(delete_state(&mut d, "Door", "Open").is_ok() && delete_state(&mut d, "Door", "Closed").is_err());

        // Removing the parameter takes its grips and its action along.
        assert_eq!(remove(&mut d, "Door", n).unwrap(), ["Width", "Stretch"]);
        let m = dynamic_block(&d, "Door").unwrap();
        assert!(m.actions.is_empty() && m.parameters.len() == 1 && m.grip_objects.len() == 1);
    }

    /// A parameter another program wrote keeps the groups Cadino doesn't model when its
    /// properties change.
    #[test]
    fn editing_a_parameter_keeps_the_groups_not_modelled() {
        let (mut d, _) = door();
        let n = add_parameter(&mut d, "Door", linear("Width"), 2, false).unwrap();
        let h = dynamic_block(&d, "Door").unwrap().parameters[0].handle;
        // As if another program had written a value-set description and an offset.
        let o = object_mut(&mut d, h).unwrap();
        for t in o.tags.iter_mut() {
            match t.code {
                140 => t.value = "4.5".into(),
                307 => t.value = "Door widths".into(),
                _ => {}
            }
        }
        o.tags.push(tag(1001, "OTHERAPP"));
        set_parameter(&mut d, "Door", n, |p| {
            if let ParameterKind::Linear { label, values, .. } = &mut p.kind {
                *label = "DoorWidth".into();
                values.list = vec![30.0, 33.0, 36.0];
            }
            Ok(())
        })
        .unwrap();
        let o = object(&d, h).unwrap();
        let value = |c: i32| o.tags.iter().find(|t| t.code == c).map(|t| t.value.clone());
        assert_eq!((value(140).as_deref(), value(307).as_deref(), value(305).as_deref()), (Some("4.5"), Some("Door widths"), Some("DoorWidth")));
        assert_eq!(o.tags.iter().filter(|t| t.code == 144).count(), 3);
        assert!(o.tags.iter().any(|t| t.code == 1001));
        let m = dynamic_block(&d, "Door").unwrap();
        let ParameterKind::Linear { values, .. } = &m.parameters[0].kind else { panic!() };
        assert_eq!(values.list, [30.0, 33.0, 36.0]);
    }

    /// A graph another program wired (edges after its nodes) is never rewired here.
    #[test]
    fn a_graph_wired_elsewhere_refuses_adding_and_removing() {
        let (mut d, _) = door();
        let n = add_parameter(&mut d, "Door", linear("Width"), 1, false).unwrap();
        let g = graph(&d, "Door").unwrap();
        // An edge record after the nodes, as AutoCAD writes them.
        let edge = [(92, "0"), (93, "0"), (94, "0"), (91, "0"), (91, "1"), (92, "-1"), (92, "-1"), (92, "-1"), (92, "-1"), (92, "-1")];
        object_mut(&mut d, g).unwrap().tags.extend(edge.iter().map(|(c, v)| tag(*c, *v)));
        let before = object(&d, g).unwrap().tags.clone();
        assert!(remove(&mut d, "Door", n).is_err());
        assert!(set_grips(&mut d, "Door", n, 2).is_err());
        assert!(add_parameter(&mut d, "Door", linear("Depth"), 1, false).is_err());
        assert_eq!(object(&d, g).unwrap().tags, before, "the graph is as it was");
        // Its properties still change.
        set_parameter(&mut d, "Door", n, |p| {
            p.chain = true;
            Ok(())
        })
        .unwrap();
        assert!(dynamic_block(&d, "Door").unwrap().parameters[0].chain);
    }
}
