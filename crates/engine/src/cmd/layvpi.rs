//! LAYVPI (Format ▸ Layer Tools ▸ Isolate Layer to Current Viewport): the layers of the selected
//! objects are frozen in every other layout viewport and thawed in the current one, so they show
//! in the current viewport only. Layouts only, from inside a viewport (MSPACE). The "Isolate all
//! layouts" setting reaches the viewports of every layout instead of the current layout's.
//! Typed: "Select objects on the layer to be isolated in viewport or [Settings/Undo]", repeated
//! until Enter; Undo takes back the last isolation. The whole command is one undo step.
// ponytail: no Viewports setting (AutoCAD's choice of which viewports); in paper space outside a
// viewport the command asks to enter one, instead of picking through viewports.

use std::sync::atomic::{AtomicBool, Ordering};

use cadcraft_doc::{EntityKind, Handle, Space};
use serde_json::{Value, json};

use super::machines::SelectPhase;
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

const CMD: &str = "layvpi";
/// "Isolate all layouts": the setting lasts for the session, as AutoCAD keeps it.
static ALL_LAYOUTS: AtomicBool = AtomicBool::new(false);

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(CMD, "Isolate Layer to Current Viewport", run)
            .menu(&["Format", "Layer Tools", "Isolate Layer to Current Viewport"])
            .params(
                "{handles? (default: the selection) | layers?: [name], viewport?: handle (default: the current viewport, MSPACE), \
                 allLayouts?: bool (default: the setting, Current layout)} → {layers, frozenIn: [viewport handles], message}",
            )
            .interactive(|_| Ok(Box::new(LayvpiM::default()))),
    ]
}

/// A viewport's frozen layers before a change, for Undo.
type Before = Vec<(Handle, Vec<String>)>;

/// Freeze `layers` in the viewports other than `cur` (of its layout, or of every layout with
/// `all`) and thaw them in `cur`. Returns what changed.
fn isolate(s: &mut Session, cur: Handle, layers: &[String], all: bool) -> Result<Before> {
    let d = s.doc()?;
    let home = d.space_of(cur);
    let vps: Vec<(Handle, Vec<String>)> = d
        .layouts
        .iter()
        .filter(|l| all || home.as_ref().and_then(Space::layout) == Some(l.name.as_str()))
        .flat_map(|l| l.entities.iter())
        .filter_map(|e| match &e.kind {
            EntityKind::Viewport(v) if v.id != 1 => Some((e.handle, v.frozen_layers.clone())),
            _ => None,
        })
        .collect();
    let has = |list: &[String], n: &str| list.iter().any(|x| x.eq_ignore_ascii_case(n));
    let mut before = Vec::new();
    let d = s.doc_mut()?;
    for (h, old) in vps {
        let mut new = old.clone();
        if h == cur {
            new.retain(|x| !has(layers, x));
        } else {
            new.extend(layers.iter().filter(|n| !has(&old, n)).cloned());
        }
        if new != old {
            d.modify_entity(h, |e| {
                if let EntityKind::Viewport(v) = &mut e.kind {
                    v.frozen_layers = new;
                }
            })?;
            before.push((h, old));
        }
    }
    Ok(before)
}

fn undo(s: &mut Session, before: Before) -> Result<()> {
    let d = s.doc_mut()?;
    for (h, old) in before {
        d.modify_entity(h, |e| {
            if let EntityKind::Viewport(v) = &mut e.kind {
                v.frozen_layers = old;
            }
        })?;
    }
    Ok(())
}

/// The layers of objects `hs` (each once, in drawing order of first use).
fn layers_of(s: &Session, hs: &[Handle]) -> Result<Vec<String>> {
    let d = s.doc()?;
    let mut out: Vec<String> = Vec::new();
    for e in hs.iter().filter_map(|h| d.entity(*h)) {
        if !out.iter().any(|n| n.eq_ignore_ascii_case(&e.common.layer)) {
            out.push(e.common.layer.clone());
        }
    }
    Ok(out)
}

fn message(layers: &[String], frozen: usize) -> String {
    let names = layers.iter().map(|l| format!("\"{l}\"")).collect::<Vec<_>>().join(", ");
    if frozen == 0 {
        format!("Layer {names} is already isolated to the current viewport.")
    } else {
        format!("Layer {names} has been isolated to the current viewport ({frozen} other viewport(s) changed).")
    }
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let st = s.state()?;
    if st.space == Space::Model {
        return Err(bad(CMD, "** Command not allowed in Model Tab ** (switch to a layout)"));
    }
    let cur = match p.get("viewport").filter(|v| !v.is_null()) {
        Some(v) => {
            let h = v.as_str().and_then(Handle::parse_hex).ok_or_else(|| bad(CMD, "`viewport` must be a viewport handle"))?;
            let ok = s.doc()?.entity(h).is_some_and(|e| matches!(&e.kind, EntityKind::Viewport(v) if v.id != 1));
            if !ok {
                return Err(bad(CMD, format!("{} is not a layout viewport", h.hex())));
            }
            h
        }
        None => st.active_viewport().map(|(h, _)| h).ok_or_else(|| bad(CMD, "enter a viewport (MSPACE) first, or pass `viewport`"))?,
    };
    let layers: Vec<String> = match p.get("layers").and_then(Value::as_array) {
        Some(a) => {
            let d = s.doc()?;
            let mut out = Vec::new();
            for n in a.iter().take(10_000) {
                let n = n.as_str().ok_or_else(|| bad(CMD, "`layers` are names"))?;
                out.push(d.layer(n).map(|l| l.name.clone()).ok_or_else(|| bad(CMD, format!("no layer `{n}`")))?);
            }
            out
        }
        None => layers_of(s, &targets(s, p)?)?,
    };
    if layers.is_empty() {
        return Err(bad(CMD, "select objects on the layers to isolate"));
    }
    let all = bool_or(p, "allLayouts", ALL_LAYOUTS.load(Ordering::Relaxed));
    let changed = isolate(s, cur, &layers, all)?;
    let frozen: Vec<String> = changed.iter().filter(|(h, _)| *h != cur).map(|(h, _)| h.hex()).collect();
    Ok(json!({ "layers": layers, "frozenIn": frozen, "message": message(&layers, frozen.len()) }))
}

#[derive(Default)]
struct LayvpiM {
    settings: bool,
    /// Each isolation so far, for Undo.
    done: Vec<Before>,
}

impl LayvpiM {
    fn pick(&mut self, s: &mut Session, hs: &[Handle]) -> Result<()> {
        let layers = layers_of(s, hs)?;
        s.set_selection(Vec::new());
        let Some((cur, _)) = s.state()?.active_viewport() else { return Ok(()) };
        if layers.is_empty() {
            return Ok(());
        }
        let changed = isolate(s, cur, &layers, ALL_LAYOUTS.load(Ordering::Relaxed))?;
        s.echo(message(&layers, changed.iter().filter(|(h, _)| *h != cur).count()));
        self.done.push(changed);
        Ok(())
    }
}

impl Interactive for LayvpiM {
    fn name(&self) -> &'static str {
        "LAYVPI"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let (model, inside) = {
            let st = s.state()?;
            (st.space == Space::Model, st.active_viewport().is_some())
        };
        if model {
            s.echo("** Command not allowed in Model Tab **");
            return Ok(Step::Done);
        }
        if !inside {
            s.echo("Double-click inside a layout viewport to make it current, then run LAYVPI.");
            return Ok(Step::Done);
        }
        let all = ALL_LAYOUTS.load(Ordering::Relaxed);
        s.echo(format!("Current settings: Isolate all layouts={}", if all { "All layouts" } else { "Current layout" }));
        let pre = SelectPhase::begin(s);
        if pre.done {
            self.pick(s, &pre.picked)?;
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.settings {
            const KW: Accept = Accept { point: false, number: false, text: false, select: false, enter: true };
            let cur = if ALL_LAYOUTS.load(Ordering::Relaxed) { "All layouts" } else { "Current layout" };
            return Prompt::new("Isolate layers in the viewports of which layouts?", KW).kw(&["All layouts", "Current layout"]).default(cur);
        }
        Prompt::new("Select objects on the layer to be isolated in viewport", Accept::SELECT).kw(&["Settings", "Undo"])
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if self.settings {
            self.settings = false;
            match i {
                Input::Keyword(k) | Input::Text(k) => {
                    let k = k.trim().to_ascii_lowercase();
                    if k.starts_with('a') {
                        ALL_LAYOUTS.store(true, Ordering::Relaxed);
                    } else if k.starts_with('c') {
                        ALL_LAYOUTS.store(false, Ordering::Relaxed);
                    } else {
                        self.settings = true;
                        return Err(EngineError::Other("Invalid option keyword.".into()));
                    }
                }
                Input::Cancel => return Ok(Step::Cancel),
                _ => {}
            }
            return Ok(Step::Continue);
        }
        match i {
            Input::Pick(hs) => {
                self.pick(s, &hs)?;
                Ok(Step::Continue)
            }
            Input::Keyword(k) | Input::Text(k) => match k.trim().to_ascii_lowercase().as_str() {
                "settings" | "s" => {
                    self.settings = true;
                    Ok(Step::Continue)
                }
                "undo" | "u" => {
                    match self.done.pop() {
                        Some(before) => {
                            undo(s, before)?;
                            s.echo("Undid the last isolation.");
                        }
                        None => s.echo("Nothing to undo."),
                    }
                    Ok(Step::Continue)
                }
                _ => Err(EngineError::Other("Invalid option keyword.".into())),
            },
            Input::Enter => Ok(Step::Done),
            Input::Cancel => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A line on Walls and one on Doors; viewports in Layout1 (at least two) and Layout2.
    /// Returns the session (in Layout1) and each layout's viewports.
    fn session() -> (Session, Vec<Handle>, Vec<Handle>) {
        let mut s = Session::new();
        for n in ["Walls", "Doors"] {
            s.execute("layer.new", &json!({ "name": n })).unwrap();
            s.execute("layer.current", &json!({ "name": n })).unwrap();
            s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
        }
        for (layout, x) in [("Layout1", 0), ("Layout1", 120), ("Layout2", 0)] {
            s.execute("layout.set", &json!({ "name": layout })).unwrap();
            s.execute("mview", &json!({ "p1": [x, 0], "p2": [x + 100, 80] })).unwrap();
        }
        s.execute("layout.set", &json!({ "name": "Layout1" })).unwrap();
        let vps = |s: &Session, name: &str| -> Vec<Handle> {
            let d = s.doc().unwrap();
            let l = d.layout(name).unwrap();
            l.entities.iter().filter(|e| matches!(&e.kind, EntityKind::Viewport(v) if v.id != 1)).map(|e| e.handle).collect()
        };
        let (one, two) = (vps(&s, "Layout1"), vps(&s, "Layout2"));
        assert!(one.len() >= 2 && !two.is_empty());
        (s, one, two)
    }

    fn frozen(s: &Session, h: Handle) -> Vec<String> {
        match &s.doc().unwrap().entity(h).unwrap().kind {
            EntityKind::Viewport(v) => v.frozen_layers.clone(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn isolates_to_the_current_viewport() {
        let (mut s, one, two) = session();
        let a = one[0];
        assert!(s.execute(CMD, &json!({ "layers": ["Walls"], "allLayouts": false })).is_err(), "paper space, no current viewport");
        s.execute("mspace", &json!({ "handle": a.hex() })).unwrap();
        // This viewport had Walls frozen: it is thawed here and frozen in the others.
        s.execute("vplayer", &json!({ "handle": a.hex(), "freeze": ["Walls"] })).unwrap();
        let r = s.execute(CMD, &json!({ "layers": ["walls"], "allLayouts": false })).unwrap();
        let others: Vec<String> = one.iter().skip(1).map(|h| h.hex()).collect();
        assert_eq!(r["frozenIn"], json!(others));
        assert!(frozen(&s, a).is_empty());
        assert!(one.iter().skip(1).all(|h| frozen(&s, *h) == vec!["Walls".to_string()]));
        assert!(two.iter().all(|h| frozen(&s, *h).is_empty()), "another layout's viewports are left alone");
        // Objects give their layers; all layouts reaches Layout2.
        let doors = s.doc().unwrap().model.iter().find(|e| e.common.layer == "Doors").unwrap().handle;
        s.execute(CMD, &json!({ "handles": [doors.hex()], "allLayouts": true })).unwrap();
        assert_eq!(frozen(&s, one[1]), vec!["Walls".to_string(), "Doors".to_string()]);
        assert!(two.iter().all(|h| frozen(&s, *h) == vec!["Doors".to_string()]));
        assert!(s.execute(CMD, &json!({ "layers": ["Nope"] })).is_err());
        s.execute("layout.set", &json!({ "name": "Model" })).unwrap();
        assert!(s.execute(CMD, &json!({ "layers": ["Walls"] })).is_err(), "not in the Model tab");
    }

    #[test]
    fn typed_loop_with_settings_and_undo() {
        let (mut s, one, two) = session();
        s.execute("mspace", &json!({ "handle": one[0].hex() })).unwrap();
        let walls = s.doc().unwrap().model.iter().find(|e| e.common.layer == "Walls").unwrap().handle;
        s.set_selection(Vec::new());
        s.start("layvpi").unwrap();
        s.input(Input::Keyword("Settings".into())).unwrap();
        s.input(Input::Keyword("All layouts".into())).unwrap();
        s.input(Input::Pick(vec![walls])).unwrap();
        assert_eq!(frozen(&s, two[0]), vec!["Walls".to_string()]);
        s.input(Input::Keyword("Undo".into())).unwrap();
        assert!(frozen(&s, one[1]).is_empty() && frozen(&s, two[0]).is_empty());
        s.input(Input::Keyword("Settings".into())).unwrap();
        s.input(Input::Keyword("Current layout".into())).unwrap();
        s.input(Input::Pick(vec![walls])).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(s.running.is_none());
        assert_eq!(frozen(&s, one[1]), vec!["Walls".to_string()]);
        assert!(frozen(&s, two[0]).is_empty());
    }
}
