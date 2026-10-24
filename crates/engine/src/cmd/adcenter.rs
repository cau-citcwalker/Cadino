//! ADCENTER (DesignCenter; ADC, DC, Ctrl+2): what a drawing holds — its blocks, layers,
//! linetypes, text, dimension, table and multileader styles, layouts and xrefs — and copying one
//! of those named definitions into this drawing, with what it needs (a block's nested blocks and
//! layers, a layer's linetype, a style's text style). The source is a drawing file (`file`, read
//! through the host's io hooks like INSERT) or an open drawing (`document`). A name this drawing
//! already has keeps this drawing's definition, as DesignCenter does. Typed bare or Ctrl+2 opens
//! the DesignCenter palette; the palette adds through this command.
// ponytail: a block brings the source's text and dimension styles this drawing lacks (INSERT's
// table merge), not only the ones it uses; a dimension style's arrow blocks are not copied.

use std::collections::BTreeSet;
use std::sync::Arc;

use cadcraft_doc::{Drawing, Entity, EntityKind, EntityStore, HVal, Layout};
use cadcraft_geom::Vec3;
use serde_json::{Value, json};

use super::*;
use crate::{Result, Session};

const CMD: &str = "adcenter";

/// The content types, as `kind` names them, in the palette's order.
pub const KINDS: [&str; 9] = ["block", "dimstyle", "layer", "layout", "linetype", "mleaderstyle", "tablestyle", "textstyle", "xref"];

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::new(CMD, "DesignCenter", run).menu(&["Window", "DesignCenter"]).key("Cmd+2").alias(&["adc", "dc"]).params(
        "{file?: DXF/DWG path | document?: open drawing index or title (default: this drawing)} → its content {blocks, dimstyles, \
                 layers, layouts, linetypes, mleaderstyles, tablestyles, textstyles, xrefs: [{name, path}]}; with kind: \
                 block|dimstyle|layer|layout|linetype|mleaderstyle|tablestyle|textstyle|xref and name: copies that definition and what it \
                 needs into this drawing (a name this drawing has keeps its own), a block with at?: [x,y] (scale?, rotation?) is also \
                 inserted, an xref is attached (typed or Ctrl+2: the DesignCenter palette)",
    )]
}

/// The names of `kind` in `d` that DesignCenter lists: no anonymous, layout or xref blocks among
/// the blocks, no xref-dependent (`XREF|NAME`) names, no ByLayer/ByBlock linetypes.
pub fn names(d: &Drawing, kind: &str) -> Vec<String> {
    fn own<'a>(it: impl Iterator<Item = &'a String>) -> Vec<String> {
        it.filter(|n| !n.contains('|')).cloned().collect()
    }
    match kind {
        "block" => d
            .blocks
            .iter()
            .filter(|(n, b)| !b.anonymous && !n.starts_with('*') && b.xref_path.is_none() && !n.contains('|'))
            .map(|(n, _)| n.clone())
            .collect(),
        "xref" => d.blocks.iter().filter(|(_, b)| b.xref_path.is_some()).map(|(n, _)| n.clone()).collect(),
        "layer" => own(d.layers.iter().map(|l| &l.name)),
        "linetype" => own(d.linetypes.iter().map(|l| &l.name).filter(|n| !n.eq_ignore_ascii_case("ByLayer") && !n.eq_ignore_ascii_case("ByBlock"))),
        "textstyle" => own(d.text_styles.iter().map(|t| &t.name)),
        "dimstyle" => own(d.dim_styles.iter().map(|t| &t.name)),
        "tablestyle" => own(d.table_styles.iter().map(|t| &t.name)),
        "mleaderstyle" => own(d.mleader_styles.iter().map(|t| &t.name)),
        "layout" => {
            let mut ls: Vec<&Layout> = d.layouts.iter().collect();
            ls.sort_by_key(|l| l.tab_order);
            ls.into_iter().map(|l| l.name.clone()).collect()
        }
        _ => Vec::new(),
    }
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    if !(p.is_object() || p.is_null()) {
        return Err(bad(CMD, "parameters must be an object"));
    }
    let (src, path) = source(s, p)?;
    let Some(kind) = str_param(p, "kind").map(str::to_ascii_lowercase) else {
        let mut out = serde_json::Map::new();
        for k in KINDS {
            let list = names(&src, k);
            let v = if k == "xref" {
                json!(list.iter().map(|n| json!({ "name": n, "path": src.block(n).and_then(|b| b.xref_path.clone()) })).collect::<Vec<_>>())
            } else {
                json!(list)
            };
            out.insert(format!("{k}s"), v);
        }
        return Ok(Value::Object(out));
    };
    if !KINDS.contains(&kind.as_str()) {
        return Err(bad(CMD, format!("unknown kind `{kind}`: {}", KINDS.join(", "))));
    }
    let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(CMD, "`name` is required with `kind`"))?;
    let found = names(&src, &kind)
        .into_iter()
        .find(|n| n.eq_ignore_ascii_case(name))
        .ok_or_else(|| bad(CMD, format!("the drawing has no {kind} \"{name}\"")))?;
    let mut out = match kind.as_str() {
        "block" => add_block(s, &src, &found, p)?,
        "layout" => add_layout(s, &src, &found)?,
        "xref" => {
            let file = src.block(&found).and_then(|b| b.xref_path.clone()).unwrap_or_default();
            let r = run_body(s, "xattach", &json!({ "path": beside(path.as_deref(), &file), "name": found }))?;
            json!({ "added": true, "xref": r })
        }
        _ => json!({ "added": add_table(s.doc_mut()?, &src, &kind, &found) }),
    };
    let added = out["added"].as_bool().unwrap_or(false);
    out["kind"] = json!(kind);
    out["name"] = json!(found);
    out["message"] = json!(if added {
        format!("Added {kind} \"{found}\".")
    } else {
        format!("\"{found}\" is already in this drawing: its own definition is kept.")
    });
    Ok(out)
}

/// The source drawing and its path (for an xref's relative path).
fn source(s: &Session, p: &Value) -> Result<(Arc<Drawing>, Option<String>)> {
    if let Some(path) = str_param(p, "file") {
        crate::limits::files(CMD)?;
        let hooks = super::file::io().ok_or_else(|| bad(CMD, "file formats are not available in this build"))?;
        let bytes = read(path)?;
        let mut d = (hooks.read)(&bytes, path).map_err(|e| bad(CMD, format!("{path}: {e}")))?;
        // Its pictures are found from its own folder.
        let _ = super::images::load_pictures(&mut d, Some(path), None);
        return Ok((Arc::new(d), Some(path.to_string())));
    }
    let st = match p.get("document").filter(|v| !v.is_null()) {
        None => s.state()?,
        Some(d) => {
            let st = match (d.as_u64(), d.as_str()) {
                (Some(i), _) => usize::try_from(i).ok().and_then(|i| s.docs.get(i)),
                (_, Some(t)) => s.docs.iter().find(|st| st.title.eq_ignore_ascii_case(t.trim())),
                _ => None,
            };
            st.ok_or_else(|| bad(CMD, format!("no open drawing {d}")))?
        }
    };
    Ok((st.doc.clone(), st.path.clone()))
}

fn read(_path: &str) -> Result<Vec<u8>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::fs::read(_path).map_err(|e| bad(CMD, format!("{_path}: {e}")))
    }
    #[cfg(target_arch = "wasm32")]
    Err(bad(CMD, "paths are not available on the web: open the drawing and use it by name (`document`)"))
}

/// An xref path as the source drawing gave it, made usable from here: a relative one is taken
/// from the source drawing's folder.
fn beside(source: Option<&str>, file: &str) -> String {
    let p = std::path::Path::new(file);
    match source.and_then(|s| std::path::Path::new(s).parent()) {
        Some(dir) if p.is_relative() && !dir.as_os_str().is_empty() => dir.join(p).to_string_lossy().to_string(),
        _ => file.to_string(),
    }
}

/// Copy `name` from `theirs` unless `ours` has it. True when it was copied.
fn copy<T: Clone>(ours: &mut Vec<T>, theirs: &[T], name: &str, key: fn(&T) -> &str) -> bool {
    if name.is_empty() || ours.iter().any(|o| key(o).eq_ignore_ascii_case(name)) {
        return false;
    }
    match theirs.iter().find(|t| key(t).eq_ignore_ascii_case(name)) {
        Some(t) => {
            ours.push(t.clone());
            true
        }
        None => false,
    }
}

/// A layer, linetype or style, with the linetype or text style it uses.
fn add_table(d: &mut Drawing, src: &Drawing, kind: &str, name: &str) -> bool {
    let text_style = |d: &mut Drawing, n: &str| copy(&mut d.text_styles, &src.text_styles, n, |t| &t.name);
    match kind {
        "layer" => {
            let added = copy(&mut d.layers, &src.layers, name, |l| &l.name);
            if let Some(lt) = src.layer(name).map(|l| l.linetype.clone()) {
                copy(&mut d.linetypes, &src.linetypes, &lt, |l| &l.name);
            }
            added
        }
        "linetype" => {
            let added = copy(&mut d.linetypes, &src.linetypes, name, |l| &l.name);
            let styles: Vec<String> = src.linetype(name).into_iter().flat_map(|l| l.pattern.iter().filter_map(|e| e.style.clone())).collect();
            for st in styles {
                text_style(d, &st);
            }
            added
        }
        "textstyle" => text_style(d, name),
        "dimstyle" => {
            let added = copy(&mut d.dim_styles, &src.dim_styles, name, |t| &t.name);
            if let Some(st) = src.dim_styles.iter().find(|t| t.name.eq_ignore_ascii_case(name)) {
                text_style(d, &st.text_style);
            }
            added
        }
        "tablestyle" => copy(&mut d.table_styles, &src.table_styles, name, |t| &t.name),
        "mleaderstyle" => {
            let added = copy(&mut d.mleader_styles, &src.mleader_styles, name, |t| &t.name);
            if let Some(st) = src.mleader_styles.iter().find(|t| t.name.eq_ignore_ascii_case(name)) {
                text_style(d, &st.text_style);
            }
            added
        }
        _ => false,
    }
}

/// The blocks `ents` reference, directly or nested (block references and dimensions).
fn needed_blocks(src: &Drawing, ents: &[&Entity]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut todo: Vec<&Entity> = ents.to_vec();
    while let Some(e) = todo.pop() {
        let name = match &e.kind {
            EntityKind::Insert(i) => Some(&i.block),
            EntityKind::Dimension(dm) => dm.block.as_ref(),
            _ => None,
        };
        // Each block is walked once, so nesting (or a cycle) in a hostile file stays bounded.
        if let Some((n, b)) = name.and_then(|n| src.blocks.get_key_value(n))
            && out.insert(n.clone())
        {
            todo.extend(b.entities.iter().map(|e| e.as_ref()));
        }
    }
    out
}

/// Define block `name` here from `ents` of `src` (base point `base`) through INSERT's file
/// definition: fresh handles, nested blocks, and the layers and linetypes they use.
fn define(s: &mut Session, src: &Drawing, name: &str, ents: &[&Entity], base: Vec3) -> Result<String> {
    let blocks = needed_blocks(src, ents);
    let mut all: Vec<&Entity> = ents.to_vec();
    all.extend(blocks.iter().filter_map(|n| src.blocks.get(n)).flat_map(|b| b.entities.iter().map(|e| e.as_ref())));
    let layers: BTreeSet<String> = all.iter().map(|e| e.common.layer.to_ascii_lowercase()).chain(["0".to_string()]).collect();
    let mut tmp = src.clone();
    tmp.layers.retain(|l| layers.contains(&l.name.to_ascii_lowercase()));
    let linetypes: BTreeSet<String> =
        all.iter().map(|e| e.common.linetype.to_ascii_lowercase()).chain(tmp.layers.iter().map(|l| l.linetype.to_ascii_lowercase())).collect();
    tmp.linetypes.retain(|l| linetypes.contains(&l.name.to_ascii_lowercase()));
    tmp.blocks.retain(|n, _| blocks.contains(n));
    tmp.model = EntityStore::new();
    for e in ents {
        tmp.model.push(Entity::clone(e));
    }
    tmp.layouts = Vec::new();
    tmp.header.set("INSBASE", HVal::Point(base));
    super::blocks::insert_file::define_from(s, &tmp, name)
}

fn add_block(s: &mut Session, src: &Drawing, name: &str, p: &Value) -> Result<Value> {
    let exists = s.doc()?.block(name).is_some();
    if !exists {
        let b = src.block(name).ok_or_else(|| bad(CMD, format!("no block \"{name}\"")))?;
        let ents: Vec<&Entity> = b.entities.iter().map(|e| e.as_ref()).collect();
        define(s, src, name, &ents, b.base)?;
    }
    let mut out = json!({ "added": !exists });
    if p.get("at").is_some_and(|v| !v.is_null()) {
        let mut q = json!({ "name": name, "at": p["at"] });
        for k in ["scale", "rotation"] {
            if let Some(v) = p.get(k).filter(|v| !v.is_null()) {
                q[k] = v.clone();
            }
        }
        out["insert"] = run_body(s, "insert", &q)?;
    }
    Ok(out)
}

/// A copy of layout `name` (its page setup and paper space objects) under a free name.
fn add_layout(s: &mut Session, src: &Drawing, name: &str) -> Result<Value> {
    let l = src.layouts.iter().find(|l| l.name == name).ok_or_else(|| bad(CMD, format!("no layout \"{name}\"")))?;
    let d = s.doc()?;
    let free = |n: &str| d.layout(n).is_none();
    let to = if free(name) {
        name.to_string()
    } else {
        (2..=10_000).map(|i| format!("{name} ({i})")).find(|n| free(n)).ok_or_else(|| bad(CMD, "no free layout name"))?
    };
    // The objects come over as a temporary block's would, then move onto the new layout.
    let tmp = (1..=10_000).map(|i| format!("$ADCENTER${i}")).find(|n| d.block(n).is_none()).ok_or_else(|| bad(CMD, "no free block name"))?;
    let ents: Vec<&Entity> = l.entities.iter().map(|e| e.as_ref()).collect();
    let tmp = define(s, src, &tmp, &ents, Vec3::ZERO)?;
    let d = s.doc_mut()?;
    let mut store = EntityStore::new();
    if let Some(b) = d.blocks.remove(&tmp) {
        for e in b.entities.iter() {
            store.push(Entity::clone(e));
        }
    }
    let order = d.layouts.iter().map(|l| l.tab_order).max().unwrap_or(0).saturating_add(1);
    d.layouts.push(Layout { name: to.clone(), tab_order: order, page: l.page.clone(), entities: store, view: l.view });
    Ok(json!({ "added": true, "layout": to }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A library drawing: a chair block (nested seat block on its own layer), styles and a layout.
    fn library() -> Session {
        let mut s = Session::new();
        let all = |s: &Session| -> Vec<String> { s.doc().unwrap().model.iter().map(|e| e.handle.hex()).collect() };
        s.execute("linetype", &json!({ "load": "DASHED" })).unwrap();
        s.execute("layer.new", &json!({ "name": "Furniture", "color": 3, "linetype": "DASHED" })).unwrap();
        s.execute("layer.new", &json!({ "name": "Unused", "color": 5 })).unwrap();
        s.execute("layer.current", &json!({ "name": "Furniture" })).unwrap();
        s.execute("rectang", &json!({ "p1": [0, 0], "p2": [4, 4] })).unwrap();
        s.execute("block", &json!({ "name": "Seat", "base": [0, 0], "handles": all(&s), "keep": "delete" })).unwrap();
        s.execute("insert", &json!({ "name": "Seat", "at": [0, 0] })).unwrap();
        s.execute("line", &json!({ "points": [[0, 4], [0, 8]] })).unwrap();
        s.execute("block", &json!({ "name": "Chair", "base": [2, 0], "handles": all(&s), "keep": "delete" })).unwrap();
        s.execute("style", &json!({ "name": "Labels", "height": 2.5 })).unwrap();
        s.execute("dimstyle", &json!({ "name": "Shop", "arrowSize": 3 })).unwrap();
        s
    }

    #[test]
    fn lists_and_copies_definitions_from_an_open_drawing() {
        let mut s = library();
        s.execute("new", &json!({})).unwrap();
        // The library is drawing 0; this one is current.
        let list = s.execute(CMD, &json!({ "document": 0 })).unwrap();
        assert_eq!(list["blocks"], json!(["Chair", "Seat"]));
        assert!(list["layers"].as_array().unwrap().contains(&json!("Furniture")));
        assert!(list["textstyles"].as_array().unwrap().contains(&json!("Labels")));
        assert!(list["layouts"].as_array().unwrap().contains(&json!("Layout1")));

        let undo = s.state().unwrap().undo.len();
        let r = s.execute(CMD, &json!({ "document": 0, "kind": "block", "name": "chair", "at": [10, 10] })).unwrap();
        assert_eq!((r["added"].clone(), r["name"].clone()), (json!(true), json!("Chair")));
        let d = s.doc().unwrap();
        assert!(d.block("Chair").is_some() && d.block("Seat").is_some(), "the nested block comes too");
        assert!(d.layer("Furniture").is_some() && d.linetype("DASHED").is_some(), "with its layer and that layer's linetype");
        assert!(d.layer("Unused").is_none(), "not the library's other layers");
        assert_eq!(d.model.len(), 1, "and it was inserted");
        assert_eq!(s.state().unwrap().undo.len(), undo + 1, "one undo step");

        // A name this drawing has keeps its own definition.
        let r = s.execute(CMD, &json!({ "document": 0, "kind": "block", "name": "Chair" })).unwrap();
        assert_eq!(r["added"], json!(false));

        s.execute(CMD, &json!({ "document": 0, "kind": "textstyle", "name": "Labels" })).unwrap();
        s.execute(CMD, &json!({ "document": 0, "kind": "dimstyle", "name": "Shop" })).unwrap();
        s.execute(CMD, &json!({ "document": 0, "kind": "layer", "name": "Unused" })).unwrap();
        let d = s.doc().unwrap();
        assert!(d.text_styles.iter().any(|t| t.name == "Labels") && d.dim_styles.iter().any(|t| t.name == "Shop"));
        assert_eq!(d.layer("Unused").map(|l| l.color), Some(cadcraft_color::Color::Index(5)));

        let r = s.execute(CMD, &json!({ "document": 0, "kind": "layout", "name": "Layout1" })).unwrap();
        assert_eq!(r["layout"], json!("Layout1 (2)"));
        assert!(s.doc().unwrap().layout("Layout1 (2)").is_some());
        assert!(s.doc().unwrap().blocks.keys().all(|k| !k.starts_with("$ADCENTER$")));
    }

    #[test]
    fn bad_requests_are_refused() {
        let mut s = library();
        for p in [
            json!({ "kind": "block" }),
            json!({ "kind": "nothing", "name": "x" }),
            json!({ "kind": "block", "name": "NoSuch" }),
            json!({ "document": 9, "kind": "block", "name": "Chair" }),
            json!({ "file": "/no/such/file.dxf" }),
            json!([1]),
        ] {
            assert!(s.execute(CMD, &p).is_err(), "{p}");
        }
        // File access can be denied (the AI assistant's setting).
        let _deny = crate::limits::deny_files("files are off");
        assert!(s.execute(CMD, &json!({ "file": "x.dxf" })).unwrap_err().to_string().contains("files are off"));
    }

    #[test]
    fn relative_xref_paths_are_taken_from_the_source_folder() {
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(beside(Some("lib/a.dxf"), "parts.dxf"), format!("lib{sep}parts.dxf"));
        assert_eq!(beside(Some("a.dxf"), "parts.dxf"), "parts.dxf");
        assert_eq!(beside(None, "parts.dxf"), "parts.dxf");
    }
}
