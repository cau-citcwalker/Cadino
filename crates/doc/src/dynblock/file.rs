//! A dynamic block made here, as a drawing file keeps it.
//!
//! Cadino writes the parameters and actions whose layout follows what AutoCAD's own files show
//! (linear, point, visibility, lookup and base point parameters; stretch, move and lookup
//! actions; their grips) as AutoCAD's objects. The others' layouts are Cadino's own and unchecked
//! in AutoCAD ([`OWN_TYPES`]: polar, XY, rotation, alignment and flip parameters with their grips;
//! scale, rotate, flip, array and polar stretch actions), so a file never carries them as AutoCAD
//! objects that might stop it opening: they go, with the elements that depend on them (actions
//! that follow them or carry them along, lookups that read them), into an XRECORD
//! `CADCRAFT_DYNBLOCK` in the block's extension dictionary, which AutoCAD keeps without reading.
//! So do the sections Cadino adds to elements it writes as AutoCAD's ([`super::OWN_SECTION`]).
//! Reading the file puts them back ([`from_file`]); in memory a block made here is one evaluation
//! graph, as [`super::author`] builds it.
//!
//! A block another program wired (its graph has edges: AutoCAD's) is written as it was read:
//! nothing of it moves into the record. A record whose block AutoCAD changed since (its graph is
//! wired now, or has the record's node ids) is dropped on reading: AutoCAD's edit wins.
//!
//! The record (after `100 AcDbXrecord`, `280 1`): the format version (90), the elements kept in it
//! (91 count; each its DXF type (2), node id (92), group count (93) and groups), the Cadino
//! sections of written elements (95 count; each its node id (92), group count (93) and groups),
//! and the elements each visibility state has active among the kept ones (96 state count; each a
//! 97 count of 98 node ids). A group is `70 code` + `1 value`, or `71 code` + `94 node` for a
//! reference to another element of the graph (whose handle a file changes).

use std::collections::{HashMap, HashSet};

use super::author::{
    self, MAX_ELEMENTS, add_member, class, dict_entry, entry, foreign, graph, key, members, object, object_mut, remove_members, sort_members, tag,
    wired, wired_graph,
};
use super::{ActionKind, OWN_SECTION, ParameterKind, dynamic_block, dynamic_block_in};
use crate::foreign::ForeignObject;
use crate::{Drawing, Handle, RawTag};

/// The extension dictionary entry of the record.
pub const KEY: &str = "CADCRAFT_DYNBLOCK";
/// The record's format version.
const VERSION: i32 = 1;
/// Most elements or sections one record is read with: as many as a block is authored with, each
/// parameter with up to four grips ([`author::max_grips`]), so whatever is written reads back.
const MAX_RECORDS: usize = MAX_ELEMENTS * 5;

/// Element types whose layout is Cadino's own: a file keeps them in the record, never as objects
/// (DXF type, C++ class).
pub const OWN_TYPES: &[(&str, &str)] = &[
    ("BLOCKPOLARPARAMETER", "AcDbBlockPolarParameter"),
    ("BLOCKXYPARAMETER", "AcDbBlockXYParameter"),
    ("BLOCKROTATIONPARAMETER", "AcDbBlockRotationParameter"),
    ("BLOCKALIGNMENTPARAMETER", "AcDbBlockAlignmentParameter"),
    ("BLOCKFLIPPARAMETER", "AcDbBlockFlipParameter"),
    ("BLOCKPOLARGRIP", "AcDbBlockPolarGrip"),
    ("BLOCKROTATIONGRIP", "AcDbBlockRotationGrip"),
    ("BLOCKALIGNMENTGRIP", "AcDbBlockAlignmentGrip"),
    ("BLOCKFLIPGRIP", "AcDbBlockFlipGrip"),
    ("BLOCKSCALEACTION", "AcDbBlockScaleAction"),
    ("BLOCKROTATEACTION", "AcDbBlockRotateAction"),
    ("BLOCKFLIPACTION", "AcDbBlockFlipAction"),
    ("BLOCKARRAYACTION", "AcDbBlockArrayAction"),
    ("BLOCKPOLARSTRETCHACTION", "AcDbBlockPolarStretchAction"),
];

/// Element types of AutoCAD's layout that can go into the record with an element they depend on
/// (an XY parameter's grip, an action following a polar parameter).
const DEPENDENT_TYPES: &[(&str, &str)] = &[
    ("BLOCKXYGRIP", "AcDbBlockXYGrip"),
    ("BLOCKMOVEACTION", "AcDbBlockMoveAction"),
    ("BLOCKSTRETCHACTION", "AcDbBlockStretchAction"),
    ("BLOCKLOOKUPACTION", "AcDbBlockLookupAction"),
];

/// The C++ class of an element type the record may hold.
fn class_of(dxf: &str) -> Option<&'static str> {
    OWN_TYPES.iter().chain(DEPENDENT_TYPES).find(|(t, _)| t.eq_ignore_ascii_case(dxf)).map(|(_, c)| *c)
}

fn own_parameter(k: &ParameterKind) -> bool {
    matches!(
        k,
        ParameterKind::Polar { .. }
            | ParameterKind::Xy { .. }
            | ParameterKind::Rotation { .. }
            | ParameterKind::Alignment { .. }
            | ParameterKind::Flip { .. }
    )
}

fn own_action(k: &ActionKind) -> bool {
    matches!(k, ActionKind::Scale { .. } | ActionKind::Rotate { .. } | ActionKind::Flip | ActionKind::Array { .. } | ActionKind::PolarStretch { .. })
}

/// Group codes whose values are handles of other objects.
fn is_pointer(code: i32) -> bool {
    (320..=369).contains(&code)
}

/// Whether a subclass marker starts a Cadino section.
fn own_marker(t: &RawTag) -> bool {
    t.code == 100 && t.value.trim().get(..OWN_SECTION.len()).is_some_and(|p| p.eq_ignore_ascii_case(OWN_SECTION))
}

/// An object's Cadino sections (each from its marker to the next marker).
fn own_sections(tags: &[RawTag]) -> Vec<RawTag> {
    let mut out = Vec::new();
    let mut inside = false;
    for t in tags {
        if t.code == 100 {
            inside = own_marker(t);
        }
        if inside {
            out.push(t.clone());
        }
    }
    out
}

/// An object's groups without its Cadino sections.
fn without_own_sections(tags: &[RawTag]) -> Vec<RawTag> {
    let mut inside = false;
    tags.iter()
        .filter(|t| {
            if t.code == 100 {
                inside = own_marker(t);
            }
            !inside
        })
        .cloned()
        .collect()
}

/// Append groups `body` as the record writes them (see the module comment).
fn put(rec: &mut Vec<RawTag>, body: &[RawTag], node_of: &HashMap<Handle, i32>) {
    rec.push(tag(93, body.len().to_string()));
    for t in body {
        match is_pointer(t.code).then(|| Handle::parse_hex(&t.value)).flatten().and_then(|h| node_of.get(&h)) {
            Some(n) => rec.extend([tag(71, t.code.to_string()), tag(94, n.to_string())]),
            None => rec.extend([tag(70, t.code.to_string()), tag(1, t.value.clone())]),
        }
    }
}

/// What the file gets of one block: the elements kept in the record and the record's groups.
struct Plan {
    graph: Handle,
    kept: HashSet<Handle>,
    /// Written elements whose Cadino sections go into the record.
    sectioned: Vec<Handle>,
    record: Vec<RawTag>,
}

/// What block `name`'s dynamic data puts in the record, `None` when nothing. `objects` are `d`'s
/// by handle.
fn plan(d: &Drawing, objects: &HashMap<Handle, &ForeignObject>, name: &str) -> Option<Plan> {
    let get = |h: Handle| objects.get(&h).copied();
    let dict = get(d.foreign.records.get(&key(name))?.xdict?)?;
    // A record this version couldn't read (a newer one's) stays as the file had it: its block
    // refuses the edits that would need a new one (author's `unread_record`).
    if dict_entry(dict, KEY).is_some() {
        return None;
    }
    let g = dict_entry(dict, "ACAD_ENHANCEDBLOCK")?;
    let go = get(g).filter(|o| o.kind.eq_ignore_ascii_case("ACAD_EVALUATION_GRAPH"))?;
    let all = members(go);
    let node_of: HashMap<Handle, i32> = all.iter().map(|(n, h)| (*h, *n)).collect();
    // ponytail: "made here" is "its graph has no edges" (AutoCAD wires every graph it writes);
    // an edgeless AutoCAD graph with these kinds would keep them in the record too. Mark the
    // elements authoring writes if that ever shows up.
    // A graph another program wired is written as it was read: only Cadino's sections can move.
    let m = if wired_graph(go) { None } else { Some(dynamic_block_in(d, objects, name)?) };
    let mut kept: HashSet<Handle> = HashSet::new();
    if let Some(m) = &m {
        let grips: HashMap<i32, Handle> = m.grip_objects.iter().copied().collect();
        let mut gone: HashSet<i32> = HashSet::new();
        for p in m.parameters.iter().filter(|p| own_parameter(&p.kind)) {
            gone.insert(p.node);
            kept.insert(p.handle);
            kept.extend(p.grips.iter().filter(|g| **g != 0).filter_map(|g| grips.get(g)));
        }
        for a in &m.actions {
            let reads = a.connections.iter().any(|(n, _)| gone.contains(n))
                || matches!(&a.kind, ActionKind::Lookup { columns, .. } if columns.iter().any(|c| gone.contains(&c.node)));
            if own_action(&a.kind) || reads || a.selection.iter().any(|h| kept.contains(h)) {
                kept.insert(a.handle);
            }
        }
    }
    let sectioned: Vec<(i32, Handle, Vec<RawTag>)> = all
        .iter()
        .filter(|(_, h)| !kept.contains(h))
        .filter_map(|(n, h)| Some((*n, *h, own_sections(&get(*h)?.tags))))
        .filter(|(_, _, s)| !s.is_empty())
        .collect();
    if kept.is_empty() && sectioned.is_empty() {
        return None;
    }
    let mut record = vec![tag(90, VERSION.to_string())];
    let elements: Vec<(i32, &ForeignObject)> = all.iter().filter(|(_, h)| kept.contains(h)).filter_map(|(n, h)| Some((*n, get(*h)?))).collect();
    record.push(tag(91, elements.len().to_string()));
    for (n, o) in elements {
        // Its groups from its first subclass marker: the handle and owner are the file's.
        let body = o.tags.iter().position(|t| t.code == 100).and_then(|i| o.tags.get(i..)).unwrap_or_default();
        record.extend([tag(2, o.kind.clone()), tag(92, n.to_string())]);
        put(&mut record, body, &node_of);
    }
    record.push(tag(95, sectioned.len().to_string()));
    for (n, _, s) in &sectioned {
        record.push(tag(92, n.to_string()));
        put(&mut record, s, &node_of);
    }
    let states = m.as_ref().and_then(|m| {
        m.parameters.iter().find_map(|p| match &p.kind {
            ParameterKind::Visibility { states, .. } => Some(states.as_slice()),
            _ => None,
        })
    });
    let states = states.unwrap_or_default();
    record.push(tag(96, states.len().to_string()));
    for s in states {
        let active: Vec<i32> = s.active.iter().filter(|h| kept.contains(h)).filter_map(|h| node_of.get(h).copied()).collect();
        record.push(tag(97, active.len().to_string()));
        record.extend(active.iter().map(|n| tag(98, n.to_string())));
    }
    Some(Plan { graph: g, kept, sectioned: sectioned.into_iter().map(|(_, h, _)| h).collect(), record })
}

/// Remove entry `name` of dictionary `dict` (and the object it names).
fn remove_entry(d: &mut Drawing, dict: Handle, name: &str) {
    let Some(h) = entry(d, dict, name) else { return };
    if let Some(o) = object_mut(d, dict) {
        let at = o.tags.windows(2).position(|w| w.first().is_some_and(|t| t.code == 3 && t.value.eq_ignore_ascii_case(name)));
        if let Some(i) = at {
            o.tags.drain(i..(i + 2).min(o.tags.len()));
        }
    }
    foreign(d).objects.retain(|o| o.handle != h);
}

/// Apply `p` to block `name` of `d` (the drawing as the file gets it).
fn write_plan(d: &mut Drawing, name: &str, p: Plan) {
    let Some(xdict) = d.foreign.records.get(&key(name)).and_then(|x| x.xdict) else { return };
    remove_members(d, p.graph, &p.kept);
    for h in &p.sectioned {
        if let Some(o) = object_mut(d, *h) {
            o.tags = without_own_sections(&o.tags);
        }
    }
    let active_kept = dynamic_block(d, name).is_some_and(|m| {
        m.parameters.iter().any(
            |q| matches!(&q.kind, ParameterKind::Visibility { states, .. } if states.iter().any(|s| s.active.iter().any(|h| p.kept.contains(h)))),
        )
    });
    if active_kept {
        let _ = author::edit_visibility(d, name, |_, states| {
            for s in states.iter_mut() {
                s.active.retain(|h| !p.kept.contains(h));
            }
            Ok(())
        });
    }
    foreign(d).objects.retain(|o| !p.kept.contains(&o.handle));
    remove_entry(d, xdict, KEY);
    let h = d.new_handle();
    let mut tags = vec![
        tag(5, h.hex()),
        tag(102, "{ACAD_REACTORS"),
        tag(330, xdict.hex()),
        tag(102, "}"),
        tag(330, xdict.hex()),
        tag(100, "AcDbXrecord"),
        tag(280, "1"),
    ];
    tags.extend(p.record);
    foreign(d).objects.push(ForeignObject { handle: h, kind: "XRECORD".into(), tags });
    if let Some(x) = object_mut(d, xdict) {
        x.tags.extend([tag(3, KEY), tag(360, h.hex())]);
    }
}

/// The drawing as a file gets it: each block made here with the elements whose layout is
/// Cadino's own (and Cadino's sections) in its record, the CLASS records of the types the record
/// holds that no object has any more dropped. `None` when there is nothing to change (the drawing
/// is written as it is).
pub fn for_file(d: &Drawing) -> Option<Drawing> {
    let objects: HashMap<Handle, &ForeignObject> = d.foreign.objects.iter().map(|o| (o.handle, o)).collect();
    let plans: Vec<(String, Plan)> = d
        .foreign
        .records
        .iter()
        .filter(|((k, _), x)| k == "BLOCK_RECORD" && x.xdict.is_some())
        .filter_map(|((_, n), _)| Some((n.clone(), plan(d, &objects, n)?)))
        .collect();
    let unused = |c: &[RawTag], used: &HashSet<String>| {
        c.iter().find(|t| t.code == 1).is_some_and(|n| class_of(n.value.trim()).is_some() && !used.contains(&n.value.trim().to_ascii_uppercase()))
    };
    let kinds = |v: &Drawing| -> HashSet<String> { v.foreign.objects.iter().map(|o| o.kind.to_ascii_uppercase()).collect() };
    if plans.is_empty() && !d.foreign.classes.iter().any(|c| unused(c, &kinds(d))) {
        return None;
    }
    let mut out = d.clone();
    for (name, p) in plans {
        write_plan(&mut out, &name, p);
    }
    let used = kinds(&out);
    foreign(&mut out).classes.retain(|c| !unused(c, &used));
    Some(out)
}

/// One group read back: a value, or a reference to element `node`.
enum Group {
    Value(i32, String),
    Node(i32, i32),
}

/// Reads a record's groups in order: each read takes the next group, which must have the code
/// asked for.
struct Reader<'a> {
    tags: &'a [RawTag],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, code: i32) -> Option<&str> {
        let t = self.tags.get(self.at).filter(|t| t.code == code)?;
        self.at += 1;
        Some(t.value.as_str())
    }
    fn int(&mut self, code: i32) -> Option<i32> {
        self.take(code)?.trim().parse().ok()
    }
    /// A count, capped by the groups left.
    fn count(&mut self, code: i32) -> Option<usize> {
        usize::try_from(self.int(code)?).ok().map(|n| n.min(self.tags.len().saturating_sub(self.at)))
    }
    fn groups(&mut self) -> Option<Vec<Group>> {
        let n = self.count(93)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            if let Some(code) = self.int(70) {
                out.push(Group::Value(code, self.take(1)?.to_string()));
            } else {
                let code = self.int(71)?;
                out.push(Group::Node(code, self.int(94)?));
            }
        }
        Some(out)
    }
}

/// A record read back.
struct Record {
    elements: Vec<(String, i32, Vec<Group>)>,
    sections: Vec<(i32, Vec<Group>)>,
    states: Vec<Vec<i32>>,
}

/// The record's groups after its header, `None` for another version or a damaged record.
fn parse(tags: &[RawTag]) -> Option<Record> {
    let start = tags.iter().position(|t| t.code == 90)?;
    let mut r = Reader { tags: tags.get(start..)?, at: 0 };
    if r.int(90)? != VERSION {
        return None;
    }
    let n = r.count(91)?.min(MAX_RECORDS);
    let mut elements = Vec::with_capacity(n);
    for _ in 0..n {
        let kind = r.take(2)?.trim().to_ascii_uppercase();
        let node = r.int(92)?;
        elements.push((kind, node, r.groups()?));
    }
    let n = r.count(95)?.min(MAX_RECORDS);
    let mut sections = Vec::with_capacity(n);
    for _ in 0..n {
        let node = r.int(92)?;
        sections.push((node, r.groups()?));
    }
    let n = r.count(96)?.min(MAX_RECORDS);
    let mut states = Vec::with_capacity(n);
    for _ in 0..n {
        let k = r.count(97)?;
        states.push((0..k).map(|_| r.int(98)).collect::<Option<Vec<i32>>>()?);
    }
    Some(Record { elements, sections, states })
}

/// Groups read back as an object's, references resolved through `handles` (a reference to an
/// element that is gone is the null handle). A group that would be a record's own (0, 5, 105)
/// is not taken.
fn resolve(groups: &[Group], handles: &HashMap<i32, Handle>) -> Vec<RawTag> {
    groups
        .iter()
        .map(|g| match g {
            Group::Value(c, v) => RawTag { code: *c, value: v.clone() },
            Group::Node(c, n) => RawTag { code: *c, value: handles.get(n).map_or_else(|| "0".to_string(), |h| h.hex()) },
        })
        .filter(|t| t.code > 0 && t.code != 5 && t.code != 105)
        .collect()
}

/// Put back what a file kept in the records of blocks made here (see the module comment). A
/// record of another version is kept as it is.
pub fn from_file(d: &mut Drawing) {
    let names: Vec<(String, Handle)> =
        d.foreign.records.iter().filter(|((k, _), _)| k == "BLOCK_RECORD").filter_map(|((_, n), x)| Some((n.clone(), x.xdict?))).collect();
    for (name, xdict) in names {
        let Some(xh) = entry(d, xdict, KEY) else { continue };
        let Some(rec) = object(d, xh).filter(|o| o.kind.eq_ignore_ascii_case("XRECORD")).and_then(|o| parse(&o.tags)) else { continue };
        // The record is read into the block: the file's copy goes.
        remove_entry(d, xdict, KEY);
        let Some(g) = graph(d, &name) else { continue };
        let mut handles: HashMap<i32, Handle> = object(d, g).map(members).unwrap_or_default().into_iter().collect();
        let mut nodes = HashSet::new();
        let fits = !wired(d, &name) && rec.elements.iter().all(|(_, n, _)| !handles.contains_key(n) && nodes.insert(*n));
        let mut restored: Vec<Handle> = Vec::new();
        if fits {
            let new: Vec<(i32, Handle, &str, &[Group])> = rec
                .elements
                .iter()
                .filter(|(k, _, _)| class_of(k).is_some())
                .map(|(k, n, gs)| (*n, d.new_handle(), k.as_str(), gs.as_slice()))
                .collect();
            handles.extend(new.iter().map(|(n, h, _, _)| (*n, *h)));
            for (n, h, kind, gs) in new {
                let mut tags = vec![tag(5, h.hex()), tag(330, g.hex())];
                tags.extend(resolve(gs, &handles));
                let f = foreign(d);
                if let Some(cpp) = class_of(kind) {
                    class(f, kind, cpp);
                }
                f.objects.push(ForeignObject { handle: h, kind: kind.to_string(), tags });
                add_member(d, g, n, h);
                restored.push(h);
            }
            sort_members(d, g);
        }
        for (n, gs) in &rec.sections {
            let Some(h) = handles.get(n).copied() else { continue };
            let body = resolve(gs, &handles);
            if let Some(o) = object_mut(d, h)
                && own_sections(&o.tags).is_empty()
                && body.first().is_some_and(own_marker)
            {
                o.tags.extend(body);
            }
        }
        if restored.is_empty() {
            continue;
        }
        // The visibility states keep them active where they were.
        let _ = author::edit_visibility(d, &name, |_, states| {
            let by_state = states.len() == rec.states.len();
            for (i, s) in states.iter_mut().enumerate() {
                let active: Vec<Handle> = match rec.states.get(i).filter(|_| by_state) {
                    Some(ns) => ns.iter().filter_map(|n| handles.get(n).copied()).filter(|h| restored.contains(h)).collect(),
                    None => restored.clone(),
                };
                for h in active {
                    if !s.active.contains(&h) {
                        s.active.push(h);
                    }
                }
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynblock::author::{NewAction, add_action, add_parameter};
    use crate::dynblock::{LookupColumn, ValueSet, VisibilityState};
    use crate::{Block, Entity, EntityKind, Line};
    use cadcraft_geom::Vec3;
    use std::sync::Arc;

    /// Block "Shelf": a board and a bracket, made dynamic with every kind of element.
    fn shelf() -> (Drawing, Vec<Handle>) {
        let mut d = Drawing::default();
        let mut b = Block::new("Shelf");
        let mut hs = Vec::new();
        for (a, e) in [((0.0, 0.0), (36.0, 0.0)), ((2.0, 0.0), (2.0, -6.0))] {
            let h = d.new_handle();
            b.entities.push(Entity::new(h, EntityKind::Line(Line { a: Vec3::new(a.0, a.1, 0.0), b: Vec3::new(e.0, e.1, 0.0) })));
            hs.push(h);
        }
        d.blocks.insert("Shelf".into(), Arc::new(b));
        let lin = ParameterKind::Linear {
            base: Vec3::ZERO,
            end: Vec3::new(36.0, 0.0, 0.0),
            label: "Width".into(),
            description: String::new(),
            values: ValueSet::default(),
            from_midpoint: false,
        };
        let w = add_parameter(&mut d, "Shelf", lin, 1, false).unwrap();
        let vis = ParameterKind::Visibility {
            at: Vec3::new(0.0, 10.0, 0.0),
            label: "Visibility1".into(),
            description: String::new(),
            controlled: vec![],
            states: vec![VisibilityState { name: "Plain".into(), ..Default::default() }],
        };
        add_parameter(&mut d, "Shelf", vis, 1, false).unwrap();
        let flip = ParameterKind::Flip {
            base: Vec3::new(-1.0, 0.0, 0.0),
            end: Vec3::new(40.0, 0.0, 0.0),
            label: "Flip state1".into(),
            description: String::new(),
            label_at: Vec3::new(18.0, 0.0, 0.0),
            states: ["Not Flipped".into(), "Flipped".into()],
        };
        let f = add_parameter(&mut d, "Shelf", flip, 1, false).unwrap();
        let stretch = ActionKind::Stretch { frame: vec![], vertices: vec![], multiplier: 1.0, angle_offset: 0.0 };
        let conns = |n: i32, a: &str, b: &str| vec![(n, a.to_string()), (n, b.to_string())];
        let new = |kind, connections, selection| NewAction { kind, connections, selection, at: Vec3::new(40.0, 4.0, 0.0) };
        add_action(&mut d, "Shelf", new(stretch, conns(w, "EndXDelta", "EndYDelta"), vec![hs[0]])).unwrap();
        add_action(&mut d, "Shelf", new(ActionKind::Array { column: 12.0, row: 0.0 }, conns(w, "UpdatedDistance", "UpdatedBase"), vec![hs[1]]))
            .unwrap();
        // The flip carries the width along (a reference to another element of the graph).
        let wh = dynamic_block(&d, "Shelf").unwrap().parameters[0].handle;
        add_action(&mut d, "Shelf", new(ActionKind::Flip, conns(f, "UpdatedFlip", "UpdatedBase"), vec![hs[0], hs[1], wh])).unwrap();
        let col = |node, lookup| LookupColumn { node, property: "UpdatedDistance".into(), lookup, unmatched: "Custom".into(), reverse: true };
        let lookup = ActionKind::Lookup { columns: vec![col(w, false), col(w, true)], rows: vec![] };
        add_action(&mut d, "Shelf", new(lookup, vec![(w, "lookupString".into())], vec![])).unwrap();
        (d, hs)
    }

    /// The model with each element named by its graph id instead of its handle.
    fn by_node(d: &Drawing) -> Vec<String> {
        let m = dynamic_block(d, "Shelf").unwrap();
        let node: HashMap<Handle, i32> = m
            .parameters
            .iter()
            .map(|p| (p.handle, p.node))
            .chain(m.actions.iter().map(|a| (a.handle, a.node)))
            .chain(m.grip_objects.iter().map(|(n, h)| (*h, *n)))
            .collect();
        let name = |h: &Handle| node.get(h).map_or_else(|| h.hex(), |n| format!("#{n}"));
        let mut out: Vec<String> = m.parameters.iter().map(|p| format!("{} {:?} {:?}", p.node, p.kind.labels(), p.grips)).collect();
        out.extend(m.actions.iter().map(|a| format!("{} {} {:?} {:?}", a.node, a.name, a.selection.iter().map(name).collect::<Vec<_>>(), a.kind)));
        let ParameterKind::Visibility { states, .. } = &m.parameters[1].kind else { panic!("visibility") };
        let mut active: Vec<String> = states[0].active.iter().map(name).collect();
        active.sort();
        out.push(format!("{active:?} {}", m.grip_objects.len()));
        out
    }

    #[test]
    fn own_layouts_go_into_the_record_and_come_back() {
        let (d, _) = shelf();
        let before = by_node(&d);
        let file = for_file(&d).expect("the block has elements of Cadino's layout");
        let kinds: HashSet<String> = file.foreign.objects.iter().map(|o| o.kind.clone()).collect();
        for (t, _) in OWN_TYPES {
            assert!(!kinds.contains(*t), "{t} written as an object");
        }
        assert!(file.foreign.classes.iter().all(|c| c.iter().all(|t| !OWN_TYPES.iter().any(|(a, b)| t.value == *a || t.value == *b))));
        // The width, its stretch, the visibility parameter and their grips stay AutoCAD's objects;
        // the flip, its grip and action, the array and the lookup (reading nothing kept) too.
        let m = dynamic_block(&file, "Shelf").unwrap();
        let names: Vec<&str> = m.actions.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Stretch", "Lookup"]);
        assert_eq!(m.parameters.len(), 2);
        let ParameterKind::Visibility { states, .. } = &m.parameters[1].kind else { panic!() };
        assert!(states[0].active.iter().all(|h| file.foreign.objects.iter().any(|o| o.handle == *h)), "no state lists what isn't written");
        // The lookup's reverse flag is Cadino's: in the record, not on the written action.
        let lk = file.foreign.objects.iter().find(|o| o.kind == "BLOCKLOOKUPACTION").unwrap();
        assert!(own_sections(&lk.tags).is_empty());

        // Read back (here without the file: the same objects with other handles would do too).
        let mut back = file.clone();
        from_file(&mut back);
        assert_eq!(by_node(&back), before);
        assert!(entry(&back, back.foreign.records.get(&key("Shelf")).unwrap().xdict.unwrap(), KEY).is_none(), "the record is read in");
        // And written again the same way.
        let again = for_file(&back).unwrap();
        let count = |d: &Drawing| d.foreign.objects.len();
        assert_eq!(count(&again), count(&file));
    }

    /// A record whose block AutoCAD changed since (its graph wired, or using the record's ids)
    /// is dropped; one of another version is kept as it is.
    #[test]
    fn a_stale_or_newer_record_is_not_put_back() {
        let (d, _) = shelf();
        let file = for_file(&d).unwrap();
        let g = graph(&file, "Shelf").unwrap();
        let mut wired_since = file.clone();
        object_mut(&mut wired_since, g).unwrap().tags.extend([tag(92, "0"), tag(93, "0"), tag(94, "0")]);
        let before = dynamic_block(&wired_since, "Shelf").unwrap();
        from_file(&mut wired_since);
        assert_eq!(dynamic_block(&wired_since, "Shelf").unwrap(), before);

        let mut newer = file.clone();
        let xh = newer.foreign.objects.iter().find(|o| o.kind == "XRECORD").unwrap().handle;
        for t in object_mut(&mut newer, xh).unwrap().tags.iter_mut().filter(|t| t.code == 90) {
            t.value = "99".into();
        }
        let kept = newer.foreign.objects.clone();
        from_file(&mut newer);
        assert_eq!(newer.foreign.objects, kept);
    }

    /// Block "Plate" with one line and an XY parameter with `grips` grips.
    fn plate(grips: usize) -> Drawing {
        let mut d = Drawing::default();
        let mut b = Block::new("Plate");
        let h = d.new_handle();
        b.entities.push(Entity::new(h, EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(10.0, 0.0, 0.0) })));
        d.blocks.insert("Plate".into(), Arc::new(b));
        let xy = ParameterKind::Xy {
            base: Vec3::ZERO,
            end: Vec3::new(10.0, 5.0, 0.0),
            label: "Horizontal".into(),
            description: String::new(),
            label2: "Vertical".into(),
            description2: String::new(),
            values: ValueSet::default(),
            values2: ValueSet::default(),
        };
        add_parameter(&mut d, "Plate", xy, grips, false).unwrap();
        d
    }

    /// An XY parameter's four grips all go into the record with it: no grip is left in the
    /// file's graph naming a parameter that isn't there.
    #[test]
    fn every_grip_of_an_xy_parameter_goes_with_it() {
        let d = plate(4);
        let m = dynamic_block(&d, "Plate").unwrap();
        assert_eq!(m.parameters[0].grips.len(), 4, "all four grips are read");
        assert_eq!(m.grip_objects.len(), 4);
        let file = for_file(&d).unwrap();
        assert!(file.foreign.objects.iter().all(|o| !o.kind.eq_ignore_ascii_case("BLOCKXYGRIP")), "no grip left behind");
        let mut back = file.clone();
        from_file(&mut back);
        let m2 = dynamic_block(&back, "Plate").unwrap();
        assert_eq!(m2.grip_objects.len(), 4);
        assert_eq!(m2.parameters[0].grips, m.parameters[0].grips);
        // Fewer grips: the unused slots aren't read as grips.
        assert_eq!(dynamic_block(&plate(1), "Plate").unwrap().parameters[0].grips.len(), 2);
        // BGRIPSET from four to one and removing the parameter leave no grip behind.
        let mut d = plate(4);
        let node = m.parameters[0].node;
        author::set_grips(&mut d, "Plate", node, 1).unwrap();
        assert_eq!(dynamic_block(&d, "Plate").unwrap().grip_objects.len(), 1);
        author::set_grips(&mut d, "Plate", node, 4).unwrap();
        author::remove(&mut d, "Plate", node).unwrap();
        assert!(dynamic_block(&d, "Plate").unwrap().grip_objects.is_empty());
        assert!(d.foreign.objects.iter().all(|o| !o.kind.eq_ignore_ascii_case("BLOCKXYGRIP")));
    }

    /// The CLASS record of an element type Cadino keeps in the record goes once no object has
    /// that type, even when no block has anything for the record any more.
    #[test]
    fn classes_of_elements_gone_are_dropped() {
        let mut d = plate(1);
        let node = dynamic_block(&d, "Plate").unwrap().parameters[0].node;
        assert!(d.foreign.classes.iter().any(|c| c.iter().any(|t| t.value == "BLOCKXYPARAMETER")));
        author::remove(&mut d, "Plate", node).unwrap();
        let file = for_file(&d).expect("the stale classes go");
        for (t, _) in OWN_TYPES.iter().chain(DEPENDENT_TYPES) {
            assert!(file.foreign.classes.iter().all(|c| c.iter().all(|g| g.value != *t)), "{t}");
        }
        assert!(for_file(&file).is_none(), "nothing left to change");
    }

    /// A block another program wired is written as it was read, its elements of Cadino's layout
    /// too (AutoCAD's own polar parameters).
    #[test]
    fn a_wired_block_is_written_as_read() {
        let mut d = plate(2);
        let g = graph(&d, "Plate").unwrap();
        object_mut(&mut d, g).unwrap().tags.extend([tag(92, "0"), tag(93, "0"), tag(94, "0")]);
        assert!(for_file(&d).is_none());
    }

    /// As many elements as a block can be authored with (each parameter with its grips) read
    /// back from a record.
    #[test]
    fn a_full_record_reads_back() {
        let n = MAX_ELEMENTS * 2;
        let mut tags = vec![tag(100, "AcDbXrecord"), tag(90, "1"), tag(91, n.to_string())];
        for i in 0..n {
            tags.extend([tag(2, "BLOCKPOLARGRIP"), tag(92, i.to_string()), tag(93, "0")]);
        }
        tags.extend([tag(95, "0"), tag(96, "0")]);
        assert_eq!(parse(&tags).map(|r| r.elements.len()), Some(n));
    }

    /// A record of a newer version is kept as it is, and its block refuses the edits that would
    /// replace it.
    #[test]
    fn a_newer_record_is_never_replaced() {
        let (d, hs) = shelf();
        let mut newer = for_file(&d).unwrap();
        let xh = newer.foreign.objects.iter().find(|o| o.kind == "XRECORD").unwrap().handle;
        for t in object_mut(&mut newer, xh).unwrap().tags.iter_mut().filter(|t| t.code == 90) {
            t.value = "99".into();
        }
        from_file(&mut newer);
        let record = object(&newer, xh).unwrap().clone();
        let m = dynamic_block(&newer, "Shelf").unwrap();
        let w = m.parameters[0].node;
        let scale = NewAction {
            kind: ActionKind::Scale { base: None },
            connections: vec![(w, "UpdatedDistance".into())],
            selection: vec![hs[1]],
            at: Vec3::ZERO,
        };
        assert!(add_action(&mut newer, "Shelf", scale).is_err());
        let lookup = m.actions.iter().find(|a| matches!(a.kind, ActionKind::Lookup { .. })).unwrap().node;
        assert!(author::set_lookup(&mut newer, "Shelf", lookup, vec![], vec![]).is_err());
        let file = for_file(&newer).unwrap_or(newer);
        assert_eq!(object(&file, xh), Some(&record), "the newer record is written as it was read");
    }

    /// Damaged and hostile records read without panicking and add nothing that isn't there.
    #[test]
    fn hostile_records_are_skipped() {
        let (d, _) = shelf();
        let file = for_file(&d).unwrap();
        let xh = file.foreign.objects.iter().find(|o| o.kind == "XRECORD").unwrap().handle;
        let original = object(&file, xh).unwrap().tags.clone();
        let cases: Vec<Vec<RawTag>> = vec![
            original.iter().take(original.len() / 2).cloned().collect(),
            original.iter().map(|t| if t.code == 93 { tag(93, "2147483647") } else { t.clone() }).collect(),
            original.iter().map(|t| if t.code == 94 { tag(94, "-7") } else { t.clone() }).collect(),
            original.iter().map(|t| if t.code == 2 { tag(2, "LINE") } else { t.clone() }).collect(),
            original.iter().map(|t| if t.code == 70 { tag(70, "5") } else { t.clone() }).collect(),
            vec![tag(100, "AcDbXrecord"), tag(90, "1"), tag(91, "999999999")],
        ];
        for tags in cases {
            let mut x = file.clone();
            object_mut(&mut x, xh).unwrap().tags = tags;
            from_file(&mut x);
            let m = dynamic_block(&x, "Shelf").unwrap();
            assert!(m.parameters.len() <= 3 && m.actions.len() <= 4);
            assert!(x.foreign.objects.iter().all(|o| o.kind != "LINE"));
        }
    }
}
