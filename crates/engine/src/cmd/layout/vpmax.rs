//! VPMAX / VPMIN: maximize a layout viewport to edit in it, and restore it.
//!
//! VPMAX makes the viewport current (MSPACE) and zooms the sheet so the viewport fills the
//! drawing area; pans and zooms inside it change only its view. VPMIN brings back the sheet's
//! view and the viewport's centre and scale, and returns to paper space.
// ponytail: the viewport fills the screen by zooming the sheet, so a viewport of another shape
// than the drawing area leaves paper beside it, and no red border marks the maximized state;
// enlarge the viewport for display only if that matters.

use cadcraft_doc::{EntityKind, Handle, Space};
use serde_json::{Value, json};

use super::layout_viewports;
use super::viewports::viewport_at;
use crate::cmd::{CommandSpec, bad};
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step, View, VpMax};

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("vpmax", "Maximize Viewport", run_vpmax)
            .params("{handle?: viewport (default: the current one, the selected one or the layout's only one)} → {viewport}")
            .noundo()
            .interactive(|_| Ok(Box::new(VpmaxM))),
        CommandSpec::new("vpmin", "Minimize Viewport", run_vpmin).params("{} (restores the viewport VPMAX maximized)").noundo(),
    ]
}

/// The viewport VPMAX acts on when none is given: the current one, a selected one, or the
/// layout's only one.
fn default_viewport(s: &Session) -> Result<Option<Handle>> {
    let vps = layout_viewports(s)?;
    let st = s.state()?;
    if let Some((h, _)) = st.active_viewport() {
        return Ok(Some(h));
    }
    if let Some(h) = st.selection.iter().find(|h| vps.iter().any(|(v, _)| v == *h)) {
        return Ok(Some(*h));
    }
    Ok(match vps.as_slice() {
        [(h, _)] => Some(*h),
        _ => None,
    })
}

pub(super) fn maximize(s: &mut Session, h: Handle) -> Result<Value> {
    let cmd = "vpmax";
    let vps = layout_viewports(s)?;
    let Some((_, vp)) = vps.into_iter().find(|(x, _)| *x == h) else { return Err(bad(cmd, "not a viewport of this layout that is on")) };
    let Space::Paper(layout) = s.layout_space() else { return Err(bad(cmd, "** Command not allowed in Model Tab **")) };
    let (w, hpx) = s.viewport_px;
    let aspect = (w / hpx.max(1.0)).max(1e-6);
    let st = s.state_mut()?;
    if st.vpmax.as_ref().is_some_and(|m| on_this_layout(st, m)) {
        return Err(bad(cmd, "a viewport is already maximized (VPMIN restores it)"));
    }
    // Left maximized on a layout since switched from, renamed or deleted: let it go.
    forget(st);
    let max = VpMax { layout, viewport: h, paper: st.paper_view(), view: View { center: vp.view_center, height: vp.view_height } };
    // The sheet zoomed to the viewport, set while in paper space so it is the sheet's view.
    st.mspace = None;
    st.set_view_quiet(View { center: vp.center.xy(), height: vp.height.max(vp.width / aspect) });
    st.mspace = Some(h);
    st.vpmax = Some(max);
    st.selection.clear();
    s.pending_window = None;
    Ok(json!({ "viewport": h.hex() }))
}

fn run_vpmax(s: &mut Session, p: &Value) -> Result<Value> {
    let want = p.get("handle").and_then(|v| v.as_str().and_then(Handle::parse_hex).or_else(|| v.as_u64().map(Handle)));
    let h = match want {
        Some(h) => h,
        None => default_viewport(s)?.ok_or_else(|| bad("vpmax", "pass `handle`: the layout has more than one viewport"))?,
    };
    maximize(s, h)
}

/// Is the maximized viewport on the layout shown (also after a rename)?
fn on_this_layout(st: &crate::DocState, m: &VpMax) -> bool {
    matches!(st.space, Space::Paper(_)) && st.doc.space(&st.space).is_some_and(|e| e.contains(m.viewport))
}

/// Put the maximized viewport's own view back (unless it was erased meanwhile) and end VPMAX,
/// leaving the sheet's view as it is.
fn forget(st: &mut crate::DocState) -> Option<VpMax> {
    let max = st.vpmax.take()?;
    let doc = std::sync::Arc::make_mut(&mut st.doc);
    let _ = doc.modify_entity(max.viewport, |e| {
        if let EntityKind::Viewport(v) = &mut e.kind {
            v.view_center = max.view.center;
            v.view_height = max.view.height;
        }
    });
    Some(max)
}

fn run_vpmin(s: &mut Session, _p: &Value) -> Result<Value> {
    let st = s.state_mut()?;
    let here = st.vpmax.as_ref().is_some_and(|m| on_this_layout(st, m));
    // A viewport left maximized on another layout (or one renamed or deleted since) is only let go.
    let Some(max) = forget(st) else { return Err(bad("vpmin", "no viewport is maximized")) };
    if here {
        st.mspace = None;
        st.set_view_quiet(max.paper);
    }
    st.selection.clear();
    s.pending_window = None;
    Ok(json!({ "viewport": max.viewport.hex() }))
}

/// VPMAX typed with several viewports and none current: pick one.
struct VpmaxM;

impl Interactive for VpmaxM {
    fn name(&self) -> &'static str {
        "VPMAX"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if s.layout_space() == Space::Model {
            s.echo("** Command not allowed in Model Tab **");
            return Ok(Step::Done);
        }
        if let Some(h) = default_viewport(s)? {
            maximize(s, h)?;
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Select a viewport to maximize", Accept::POINT)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let h = match &i {
            Input::Point(p) => viewport_at(s, *p),
            Input::Pick(hs) => hs.first().copied(),
            Input::Enter => return Ok(Step::Done),
            _ => None,
        };
        let Some(h) = h else { return Err(EngineError::Other("No viewport found there.".into())) };
        maximize(s, h)?;
        Ok(Step::Done)
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_geom::Vec2;

    use super::*;

    fn layout_with(n: usize) -> Session {
        let mut s = Session::new();
        s.execute("circle", &json!({ "center": [0, 0], "radius": 10 })).unwrap();
        s.execute("layout", &json!({ "option": "set", "name": "Layout1" })).unwrap();
        let vps = s.execute("mview", &json!({ "p1": [10, 10], "p2": [110, 60], "count": n })).unwrap();
        assert_eq!(vps["viewports"].as_array().unwrap().len(), n);
        s
    }

    fn vp(s: &Session, h: Handle) -> cadcraft_doc::Viewport {
        match &s.doc().unwrap().entity(h).unwrap().kind {
            EntityKind::Viewport(v) => v.clone(),
            _ => panic!("a viewport"),
        }
    }

    #[test]
    fn vpmax_edits_inside_the_viewport_and_vpmin_restores_it() {
        let mut s = layout_with(1);
        let paper = s.state().unwrap().paper_view();
        let r = s.execute("vpmax", &json!({})).unwrap();
        let h = Handle::parse_hex(r["viewport"].as_str().unwrap()).unwrap();
        let before = vp(&s, h);
        let st = s.state().unwrap();
        assert_eq!(st.mspace, Some(h));
        // The sheet is zoomed to the viewport.
        assert!(st.paper_view().center.near(Vec2::new(60.0, 35.0), 1e-9));
        assert!(st.paper_view().height >= 50.0 - 1e-9);
        assert!(s.execute("vpmax", &json!({})).is_err(), "already maximized");
        // Zooming inside changes the viewport's view…
        s.execute("zoom", &json!({ "mode": "in" })).unwrap();
        assert_ne!(vp(&s, h).view_height, before.view_height);
        // …and VPMIN brings back its scale and centre, the sheet's view and paper space.
        s.execute("vpmin", &json!({})).unwrap();
        let after = vp(&s, h);
        assert_eq!((after.view_center, after.view_height), (before.view_center, before.view_height));
        let st = s.state().unwrap();
        assert_eq!((st.mspace, st.paper_view()), (None, paper));
        assert!(s.execute("vpmin", &json!({})).is_err(), "nothing maximized");
    }

    #[test]
    fn vpmax_needs_a_viewport_and_a_layout() {
        let mut s = Session::new();
        assert!(s.execute("vpmax", &json!({})).is_err(), "Model tab");
        let mut s = layout_with(2);
        assert!(s.execute("vpmax", &json!({})).is_err(), "two viewports, none chosen");
        assert!(s.execute("vpmax", &json!({ "handle": "FFFFF" })).is_err());
        // Typed: pick the viewport.
        s.cmdline("vpmax").unwrap();
        s.input(Input::Point(Vec2::new(20.0, 30.0))).unwrap();
        assert!(s.running.is_none());
        assert!(s.state().unwrap().vpmax.is_some());
    }

    #[test]
    fn a_viewport_left_maximized_on_a_renamed_layout_can_be_let_go() {
        let mut s = layout_with(1);
        let r = s.execute("vpmax", &json!({})).unwrap();
        let h = Handle::parse_hex(r["viewport"].as_str().unwrap()).unwrap();
        let before = vp(&s, h);
        s.execute("zoom", &json!({ "mode": "in" })).unwrap();
        s.execute("layout", &json!({ "option": "rename", "from": "Layout1", "to": "Sheet" })).unwrap();
        // VPMIN still restores it on the renamed layout.
        s.execute("vpmin", &json!({})).unwrap();
        assert!(s.state().unwrap().vpmax.is_none());
        assert_eq!(s.state().unwrap().mspace, None);
        assert_eq!(vp(&s, h).view_height, before.view_height);
        s.execute("vpmax", &json!({ "handle": h.hex() })).unwrap();
        // Maximized on a layout since left: VPMAX elsewhere takes over.
        s.execute("layout", &json!({ "option": "set", "name": "Layout2" })).unwrap();
        s.execute("mview", &json!({ "p1": [10, 10], "p2": [110, 60] })).unwrap();
        s.execute("vpmax", &json!({})).unwrap();
        assert_eq!(s.state().unwrap().vpmax.as_ref().map(|m| m.layout.as_str()), Some("Layout2"));
    }
}
