//! OOPS: put back the objects the last ERASE removed — also the Delete key, BLOCK's and -BLOCK's
//! Delete and the objects -WBLOCK wrote — as AutoCAD's OOPS does. The set is kept per drawing
//! ([`crate::DocState::oops`]) until the next erase replaces it or OOPS uses it up.

use cadcraft_doc::Handle;
use serde_json::{Value, json};

use super::CommandSpec;
use crate::{EngineError, Result, Session};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("oops", "Oops", run)
            .params("{} → {restored}: brings back the objects the last ERASE (the Delete key, BLOCK's Delete, -WBLOCK) removed"),
    ]
}

/// Remove `hs` from the drawing and remember them for OOPS (replacing the set remembered before,
/// unless nothing was removed). Objects on locked layers (or outside a REFEDIT working set) stay.
/// Returns how many were removed.
pub(crate) fn erase(s: &mut Session, hs: &[Handle]) -> Result<usize> {
    let hs: Vec<Handle> = hs.iter().copied().filter(|h| !super::curves::is_locked(s, *h)).collect();
    let d = s.doc_mut()?;
    let gone: Vec<_> = hs
        .iter()
        .filter_map(|h| {
            let sp = d.space_of(*h)?;
            Some((sp, d.remove_entity(*h)?))
        })
        .collect();
    let n = gone.len();
    if n > 0 {
        s.state_mut()?.oops = gone;
    }
    Ok(n)
}

fn run(s: &mut Session, _p: &Value) -> Result<Value> {
    if s.state()?.block_edit.is_some() {
        return Err(EngineError::Other("OOPS is not available in the Block Editor; use UNDO.".into()));
    }
    let set = std::mem::take(&mut s.state_mut()?.oops);
    if set.is_empty() {
        return Err(EngineError::Other("Nothing to restore: OOPS brings back what the last ERASE removed.".into()));
    }
    let d = s.doc_mut()?;
    let mut n = 0;
    for (sp, e) in set {
        // Already back (UNDO), or its layout is gone, or UNDO went back past its creation
        // (handles from the handseed on were never given out in the drawing as it now is;
        // restoring one would let the next new object take its handle).
        if d.entity(e.handle).is_some() || d.space(&sp).is_none() || e.handle.0 >= d.handseed {
            continue;
        }
        d.ensure_layer(&e.common.layer);
        if let Some(store) = d.space_mut(&sp) {
            store.push((*e).clone());
            n += 1;
        }
    }
    let message = format!("{n} object{} restored.", if n == 1 { "" } else { "s" });
    Ok(json!({ "restored": n, "message": message }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::Session;

    fn count(s: &Session) -> usize {
        s.doc().unwrap().model.len()
    }

    fn line(s: &mut Session, y: f64) -> String {
        s.execute("line", &json!({"points": [[0, y], [10, y]]})).unwrap()["handles"][0].as_str().unwrap().to_string()
    }

    #[test]
    fn oops_restores_the_last_erase_once() {
        let mut s = Session::new();
        let a = line(&mut s, 0.0);
        let b = line(&mut s, 1.0);
        let c = line(&mut s, 2.0);
        assert!(s.execute("oops", &json!({})).is_err(), "nothing erased yet");
        s.execute("erase", &json!({"handles": [a]})).unwrap();
        s.execute("erase", &json!({"handles": [b, c]})).unwrap();
        // Another command in between doesn't matter.
        line(&mut s, 3.0);
        assert_eq!(count(&s), 1);
        let r = s.execute("oops", &json!({})).unwrap();
        assert_eq!(r["restored"], 2);
        assert_eq!(count(&s), 3);
        // The same objects (handles) come back, and only the last erase's.
        let d = s.doc().unwrap();
        assert!(d.entity(cadcraft_doc::Handle::parse_hex(&b).unwrap()).is_some());
        assert!(d.entity(cadcraft_doc::Handle::parse_hex(&a).unwrap()).is_none());
        // Used up; UNDO takes the OOPS back.
        assert!(s.execute("oops", &json!({})).is_err());
        s.execute("undo", &json!({})).unwrap();
        assert_eq!(count(&s), 1);
    }

    #[test]
    fn oops_typed_after_interactive_erase_and_delete_key() {
        let mut s = Session::new();
        let a = line(&mut s, 0.0);
        s.cmdline("erase").unwrap();
        s.cmdline("l").unwrap();
        s.cmdline("").unwrap();
        assert_eq!(count(&s), 0);
        s.cmdline("oops").unwrap();
        assert_eq!(count(&s), 1);
        assert!(s.log.iter().any(|l| l == "1 object restored."), "{:?}", s.log);
        // The Delete key (erase.selection) too.
        s.set_selection(vec![cadcraft_doc::Handle::parse_hex(&a).unwrap()]);
        s.execute("erase.selection", &json!({})).unwrap();
        assert_eq!(count(&s), 0);
        s.execute("oops", &json!({})).unwrap();
        assert_eq!(count(&s), 1);
        // UNDO of the erase brings them back; OOPS then has nothing more to add.
        s.cmdline("erase l ").unwrap();
        s.execute("undo", &json!({})).unwrap();
        assert_eq!(s.execute("oops", &json!({})).unwrap()["restored"], 0);
        assert_eq!(count(&s), 1);
    }

    #[test]
    fn oops_never_restores_an_object_whose_creation_was_undone() {
        let mut s = Session::new();
        let a = line(&mut s, 0.0);
        s.execute("erase", &json!({"handles": [a.clone()]})).unwrap();
        s.execute("undo", &json!({})).unwrap();
        s.execute("undo", &json!({})).unwrap();
        assert_eq!(count(&s), 0);
        assert_eq!(s.execute("oops", &json!({})).unwrap()["restored"], 0);
        // The next object takes the freed handle; nothing is replaced or duplicated.
        let b = line(&mut s, 1.0);
        assert_eq!(b, a);
        assert_eq!(count(&s), 1);
        // Across spaces: the handle must not come back in another space either.
        let mut s = Session::new();
        s.execute("layout", &json!({ "option": "set", "name": "Layout1" })).unwrap();
        let a = line(&mut s, 0.0);
        s.execute("erase", &json!({"handles": [a]})).unwrap();
        s.execute("undo", &json!({})).unwrap();
        s.execute("undo", &json!({})).unwrap();
        s.execute("oops", &json!({})).unwrap();
        s.execute("layout", &json!({ "option": "set", "name": "Model" })).unwrap();
        line(&mut s, 1.0);
        let d = s.doc().unwrap();
        let mut hs: Vec<_> = d.model.iter().map(|e| e.handle).collect();
        for l in &d.layouts {
            hs.extend(l.entities.iter().map(|e| e.handle));
        }
        let n = hs.len();
        hs.sort();
        hs.dedup();
        assert_eq!(hs.len(), n, "duplicate handles");
    }

    #[test]
    fn erasing_commands_leave_locked_objects() {
        let mut s = Session::new();
        let a = line(&mut s, 0.0);
        s.execute("layer.set", &json!({"name": "0", "locked": true})).unwrap();
        // MVSETUP's Delete objects and the Delete key go through the same erase.
        assert_eq!(s.execute("mvsetup", &json!({"option": "delete", "handles": [a.clone()]})).unwrap()["erased"], 0);
        s.set_selection(vec![cadcraft_doc::Handle::parse_hex(&a).unwrap()]);
        assert_eq!(s.execute("erase.selection", &json!({})).unwrap()["erased"], 0);
        assert_eq!(count(&s), 1);
    }

    #[test]
    fn oops_after_block_delete() {
        let mut s = Session::new();
        let a = line(&mut s, 0.0);
        s.execute("block", &json!({"name": "B", "base": [0, 0], "handles": [a], "keep": "delete"})).unwrap();
        assert_eq!(count(&s), 0);
        s.execute("oops", &json!({})).unwrap();
        assert_eq!(count(&s), 1);
        // Used up.
        assert!(s.execute("oops", &json!({})).is_err());

        // -WBLOCK's objects (erased once written) are covered with the file hooks in
        // tests/typed_bare.rs; a bad `keep` is refused before anything is written.
        assert!(s.execute("wblock", &json!({"path": "x.dxf", "handles": [a], "keep": "nope"})).is_err());
    }
}
