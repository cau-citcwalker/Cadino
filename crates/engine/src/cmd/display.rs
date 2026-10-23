//! DIMREGEN (associative dimensions put back on their objects) and VIEWRES (circle zoom percent).

use cadcraft_doc::{Dimension, Drawing, EntityKind, Handle};
use serde_json::{Value, json};

use super::curves::KW;
use super::machines::number;
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("dimregen", "Update Associative Dimensions", run_dimregen)
            .params("{} → {updated}: every associative dimension's attached points back on its objects (after edits made outside Cadino)"),
        CommandSpec::new("viewres", "View Resolution", run_viewres)
            .params("{percent: 1..=20000 (circle zoom percent, default 1000), fastZooms?: bool} → {percent}: kept with the drawing; Cadino draws curves smooth to the screen at every zoom, so it doesn't change the display")
            .interactive(|s| Ok(Box::new(ViewresM { asked_fast: false, percent: viewres(s) }))),
    ]
}

/// `dm` with the points attached to objects where those objects now have them; `None` when
/// nothing moved.
fn regenerated(d: &Drawing, dm: &Dimension) -> Option<Dimension> {
    let mut nd = dm.clone();
    let mut moves = Vec::new();
    for a in &dm.assoc {
        let (Some(p), Some(old)) = (crate::assoc::snap_point(d, a), crate::assoc::def_point(dm, &a.point)) else { continue };
        let slot = match a.point.as_str() {
            "defpt" => &mut nd.defpt,
            "p13" => &mut nd.p13,
            "p14" => &mut nd.p14,
            "p15" => &mut nd.p15,
            "p16" => &mut nd.p16,
            _ => continue,
        };
        (slot.x, slot.y) = (p.x, p.y);
        moves.push(p - old.xy());
    }
    // Every attached point moved alike: the text goes along.
    if let Some(first) = moves.first().copied()
        && moves.iter().all(|m| m.near(first, 1e-9))
    {
        nd.text_mid.x += first.x;
        nd.text_mid.y += first.y;
    }
    (nd != *dm).then_some(Dimension { block: None, ..nd })
}

fn run_dimregen(s: &mut Session, _p: &Value) -> Result<Value> {
    let d = s.doc()?;
    let changes: Vec<(Handle, Dimension)> = std::iter::once(&d.model)
        .chain(d.layouts.iter().map(|l| &l.entities))
        .flat_map(|st| st.iter())
        .filter_map(|e| match &e.kind {
            EntityKind::Dimension(dm) if !dm.assoc.is_empty() => Some((e.handle, regenerated(d, dm)?)),
            _ => None,
        })
        .collect();
    let n = changes.len();
    if n > 0 {
        let d = s.doc_mut()?;
        for (h, dm) in changes {
            d.modify_entity(h, |e| e.kind = EntityKind::Dimension(dm))?;
        }
    }
    s.touch();
    Ok(json!({ "updated": n, "message": format!("{n} associative dimension{} updated.", if n == 1 { "" } else { "s" }) }))
}

/// The drawing's circle zoom percent (VIEWRES).
fn viewres(s: &Session) -> i64 {
    s.doc().map_or(1000, |d| d.header.i64("VIEWRES", 1000))
}

fn set_viewres(s: &mut Session, v: i64) -> Result<Value> {
    if !(1..=20_000).contains(&v) {
        return Err(EngineError::Other("Requires an integer between 1 and 20000.".into()));
    }
    s.doc_mut()?.header.set_i64("VIEWRES", v);
    Ok(json!({ "percent": v }))
}

fn run_viewres(s: &mut Session, p: &Value) -> Result<Value> {
    let v = match p.get("percent") {
        None | Some(Value::Null) => return Ok(json!({ "percent": viewres(s) })),
        Some(v) => v.as_i64().ok_or_else(|| bad("viewres", "`percent` must be an integer from 1 to 20000"))?,
    };
    set_viewres(s, v)
}

/// VIEWRES typed: "Do you want fast zooms?", then the circle zoom percent.
struct ViewresM {
    asked_fast: bool,
    percent: i64,
}

impl Interactive for ViewresM {
    fn name(&self) -> &'static str {
        "VIEWRES"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.asked_fast {
            Prompt::new("Enter circle zoom percent (1-20000)", Accept::NUMBER).default(self.percent.to_string())
        } else {
            Prompt::new("Do you want fast zooms?", KW).kw(&["Yes", "No"]).default("Y")
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if !self.asked_fast {
            return Ok(match i {
                Input::Cancel => Step::Cancel,
                Input::Keyword(_) | Input::Enter => {
                    self.asked_fast = true;
                    Step::Continue
                }
                _ => Step::Continue,
            });
        }
        match i {
            Input::Text(t) => {
                let v = number(&t)
                    .filter(|v| v.fract() == 0.0 && (1.0..=20_000.0).contains(v))
                    .ok_or_else(|| EngineError::Other("Requires an integer between 1 and 20000.".into()))?;
                set_viewres(s, v as i64)?;
                Ok(Step::Done)
            }
            Input::Enter => Ok(Step::Done),
            Input::Cancel => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::{EntityKind, Handle};
    use serde_json::json;

    use crate::Session;

    #[test]
    fn dimregen_puts_dimensions_back_on_their_objects() {
        let mut s = Session::new();
        s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
        let l = s.doc().unwrap().model.iter().next().unwrap().handle;
        let r = s.execute("dimlinear", &json!({"p1": [0, 0], "p2": [10, 0], "at": [5, 3]})).unwrap();
        let dh = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
        let attached = matches!(&s.doc().unwrap().entity(dh).unwrap().kind, EntityKind::Dimension(d) if !d.assoc.is_empty());
        assert!(attached, "the dimension is associative");
        // An edit that skipped the update (as a file written elsewhere would be).
        s.doc_mut()
            .unwrap()
            .modify_entity(l, |e| {
                if let EntityKind::Line(ln) = &mut e.kind {
                    ln.b.x = 20.0;
                }
            })
            .unwrap();
        let r = s.execute("dimregen", &json!({})).unwrap();
        assert_eq!(r["updated"], 1);
        let EntityKind::Dimension(d) = &s.doc().unwrap().entity(dh).unwrap().kind else { panic!() };
        assert!(d.p13.x == 20.0 || d.p14.x == 20.0, "{:?} {:?}", d.p13, d.p14);
        assert_eq!(s.execute("dimregen", &json!({})).unwrap()["updated"], 0);
    }

    #[test]
    fn viewres_is_kept() {
        let mut s = Session::new();
        assert_eq!(s.execute("viewres", &json!({})).unwrap()["percent"], 1000);
        s.execute("viewres", &json!({"percent": 500})).unwrap();
        assert!(s.execute("viewres", &json!({"percent": 0})).is_err());
        s.cmdline("viewres").unwrap();
        assert!(s.prompt_text().ends_with("Do you want fast zooms? [Yes/No] <Y>:"), "{}", s.prompt_text());
        s.cmdline("y").unwrap();
        assert!(s.prompt_text().ends_with("Enter circle zoom percent (1-20000) <500>:"), "{}", s.prompt_text());
        s.cmdline("30000").unwrap();
        assert!(s.running.is_some(), "asked again");
        s.cmdline("2000").unwrap();
        assert!(s.running.is_none());
        assert_eq!(s.doc().unwrap().header.i64("VIEWRES", 0), 2000);
    }
}
