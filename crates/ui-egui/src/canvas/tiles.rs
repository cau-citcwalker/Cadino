//! Rhino-style model tiles: the display list of each tile that looks another way than the
//! current one, the viewport title with its menu, and Rhino's right-drag navigation in 3D views.
//!
//! The current tile is the canvas's own view (its list, picks and snaps follow
//! `DocState::view_dir`); every other tile draws, and a click in one makes it current (and in a
//! 3D layout picks there, [`click`]). A tile looking the way the current one does draws the
//! canvas's list; one looking along another direction or in another visual style gets a list of
//! its own, built in the background like the canvas's view rebuilds (see [`super::rebuild`]) a
//! band coarser, and shown when it lands. Making a tile current trades its list with the
//! canvas's ([`trade`]).

use std::sync::Arc;

use cadcraft_doc::Space;
use cadcraft_engine::{ModelTile, TileLook};
use cadcraft_geom::Bounds2;
use cadcraft_render::DisplayList;
use egui::{Rect, Sense, pos2, vec2};
use serde_json::{Value, json};

use super::{ListKey, ORBIT_DEG_PER_PX, build_window, rebuild, tile_xf};
use crate::CadApp;
use crate::theme::Tokens;

/// The display list of the tiles that look one way (see the module docs).
pub(crate) struct TileList {
    look: TileLook,
    key: ListKey,
    list: Option<DisplayList>,
    window: Option<Bounds2>,
    job: Option<rebuild::Job>,
}

impl super::CanvasState {
    /// The list drawn in the tiles that look `look`'s way, when they have their own (the current
    /// tile's look draws the canvas's list).
    pub fn tile_list(&self, look: &TileLook) -> Option<&DisplayList> {
        self.tile_lists.iter().find(|l| l.look == *look).and_then(|l| l.list.as_ref())
    }

    /// Whether a tile's list is still building in the background.
    pub fn tiles_building(&self) -> bool {
        self.tile_lists.iter().any(|l| l.job.is_some())
    }
}

/// A tile list's key field for a look's visual style ([`ListKey`]'s eighth field).
fn style_bits(l: &TileLook) -> u64 {
    u64::from(l.shaded) | u64::from(l.edges) << 1
}

/// When the current tile has come to look the way another tile's list was built (a click or a
/// right-drag in that tile, VPORTS.ACTIVATE, an undo), the lists trade places: that list becomes
/// the canvas's and the canvas's goes to the tile that was current, so neither tile draws blank
/// or turned while their next builds (for their exact zoom) run. Call before the canvas uses its
/// list.
pub(super) fn trade(app: &mut CadApp) {
    let Ok(st) = app.session.state() else { return };
    if st.space != Space::Model {
        return;
    }
    let now = st.look();
    let style = super::list_style(st, super::split_viewport(st, super::stencil_clip(app)).map(|(h, ..)| h));
    let fresh = |k: &ListKey| (k.0, k.1, k.6) == (st.uid, st.revision, Arc::as_ptr(&st.doc) as usize);
    let c = &mut app.canvas;
    // The canvas's list must be the one built for the look it is leaving (an orbit's builds lag).
    let Some(was) = c.last_look.filter(|w| *w != now && c.list_dir == w.dir) else { return };
    if c.list.is_none() || !fresh(&c.key) {
        return;
    }
    let Some(tl) = c.tile_lists.iter_mut().find(|l| l.look == now && l.list.is_some() && fresh(&l.key)) else { return };
    let mut key = tl.key;
    key.7 = style;
    let mut old = c.key;
    old.7 = style_bits(&was);
    let list = std::mem::replace(&mut c.list, tl.list.take());
    let window = std::mem::replace(&mut c.window, tl.window);
    *tl = TileList { look: was, key: old, list, window, job: None };
    // A build in flight for the old look is no longer wanted (dropping it cancels it).
    (c.key, c.list_dir, c.job, c.last_look) = (key, now.dir, None, Some(now));
    c.list_gen = c.list_gen.wrapping_add(1);
}

/// A primary click in a tile other than the current one: it answers -VPORTS Join's prompts, else
/// makes the tile current ([`cadcraft_engine::Session::click_tile`]). In a 3D layout (a tile
/// looking other than in plan) the click then picks in that tile too, as in Rhino, with its own
/// list ([`trade`]) and view; tiles all in plan keep AutoCAD's way (the click only makes the
/// tile current). `None`: no such click; else whether the click goes on to pick.
/// ponytail: inactive tiles show no rollover highlight or snap marker until clicked (the click
/// itself picks and snaps in its tile); run the hover pass with the hovered tile's list and
/// transform if that is missed.
pub(super) fn click(app: &mut CadApp, ui: &egui::Ui, canvas: &egui::Response, tiles: &[(Rect, ModelTile)]) -> Option<bool> {
    let (clicked, at) = ui.input(|i| (i.pointer.primary_clicked(), i.pointer.hover_pos()));
    let at = at.filter(|_| clicked && canvas.hovered())?;
    let i = tiles.iter().position(|(r, t)| !t.active && r.contains(at))?;
    let answers = app.session.prompts_for_tile();
    ui.ctx().request_repaint();
    if let Err(e) = app.session.click_tile(i) {
        app.session.echo(e.to_string());
        return Some(false);
    }
    if answers || tiles.iter().all(|(_, t)| t.look.dir.is_plan()) {
        return Some(false);
    }
    trade(app);
    // It picks only where the canvas has a list for the tile's direction.
    // ponytail: right after an edit, before the tile's background rebuild lands, the click only
    // makes the tile current (the next click picks); build the list on the UI thread here, or
    // pick plan tiles without one, if that is missed.
    Some(app.session.state().is_ok_and(|st| app.canvas.list.is_some() && app.canvas.list_dir == st.view_dir))
}

/// Make sure every tile that looks another way than the current one has a list for its view:
/// a finished background build goes in, a stale or missing one starts building (at once where
/// there are no threads). Lists no tile looks for any more are let go.
/// ponytail: a tile's first list and its lists after an edit arrive a build later than the
/// current tile's; build them on the UI thread too if that lag shows.
pub(super) fn ensure_lists(app: &mut CadApp, tiles: &[(Rect, ModelTile)], ctx: &egui::Context) {
    let Ok(st) = app.session.state() else { return };
    let current = st.look();
    // Each look other than the current one, with the views showing it.
    let mut looks: Vec<(TileLook, Vec<(Bounds2, f64)>)> = Vec::new();
    for (r, t) in tiles.iter().filter(|(r, t)| !t.active && t.look != current && r.width() >= 1.0 && r.height() >= 1.0) {
        let xf = tile_xf(*r, t, t.look.dir);
        let v = (xf.view_bounds(), 1.0 / xf.scale);
        match looks.iter_mut().find(|(l, _)| *l == t.look) {
            Some((_, vs)) => vs.push(v),
            None => looks.push((t.look, vec![v])),
        }
    }
    let lists = &mut app.canvas.tile_lists;
    lists.retain(|l| looks.iter().any(|(k, _)| *k == l.look));
    let (lwdisplay, transparency) = (app.session.settings.lwdisplay, app.session.settings.transparency_display);
    let band = |px: f64| px.max(1e-300).log2().floor() as i32;
    for (look, views) in looks {
        let tl = match lists.iter().position(|l| l.look == look) {
            Some(i) => lists.get_mut(i),
            None => {
                lists.push(TileList { look, key: ListKey::default(), list: None, window: None, job: None });
                lists.last_mut()
            }
        };
        let Some(tl) = tl else { continue };
        let mut lost = false;
        if let Some(job) = &tl.job {
            match job.poll() {
                rebuild::Poll::Pending => {}
                rebuild::Poll::Done(built) => {
                    (tl.key, tl.window) = (job.key, job.window);
                    tl.list = Some(built.list);
                    tl.job = None;
                }
                rebuild::Poll::Lost => {
                    // As the canvas's own list does: build on the UI thread, not the failing
                    // worker again every frame.
                    log::warn!("canvas: a viewport's background rebuild failed; rebuilding on the UI thread");
                    (tl.job, tl.list, lost) = (None, None, true);
                }
            }
        }
        // Inactive tiles draw a tessellation band coarser than the finest zoom showing them.
        let finest = views.iter().map(|v| v.1).fold(f64::INFINITY, f64::min);
        let b = band(finest).saturating_add(1);
        let d = look.dir.dir;
        let key: ListKey = (
            st.uid,
            st.revision,
            b,
            b,
            lwdisplay,
            transparency,
            std::sync::Arc::as_ptr(&st.doc) as usize,
            style_bits(&look),
            [d.x.to_bits(), d.y.to_bits(), d.z.to_bits()],
        );
        let visible = views.iter().fold(Bounds2::EMPTY, |acc, v| acc.union(&v.0));
        let inside = tl.window.is_none_or(|w| visible.is_empty() || w.contains_box(&visible));
        if (tl.list.is_some() && tl.key == key && inside) || tl.job.as_ref().is_some_and(|j| j.serves(&key, &visible)) {
            continue;
        }
        let opts = cadcraft_render::Options {
            tolerance: 2f64.powi(b) * 0.5,
            min_dash: 2f64.powi(b) * 2.0,
            text: true,
            fill: true,
            lineweights: lwdisplay,
            view: build_window(&views),
            transparency,
            view_dir: look.dir,
            shaded: look.shaded,
            edges: look.edges,
            ..Default::default()
        };
        // Dropping a job for an older key cancels it.
        tl.job = if lost {
            None
        } else {
            rebuild::spawn(rebuild::Request { doc: st.doc.clone(), space: Space::Model, opts: opts.clone(), mesh: None }, key, opts.view, ctx)
        };
        if tl.job.is_none() {
            tl.list = Some(cadcraft_render::build(&st.doc, &Space::Model, &opts));
            (tl.key, tl.window) = (key, opts.view);
        }
    }
}

/// The list and view direction tile `t` draws with: the canvas's for the current tile's look,
/// else its own (none until its first build lands).
pub(super) fn list_for<'a>(app: &'a CadApp, t: &ModelTile) -> (Option<&'a DisplayList>, cadcraft_geom::ViewDir) {
    if app.session.state().is_ok_and(|st| st.look() == t.look) {
        (app.canvas.list.as_ref(), app.canvas.list_dir)
    } else {
        (app.canvas.tile_list(&t.look), t.look.dir)
    }
}

/// Rhino's right button in a 3D (non-plan) model view: dragging orbits, Shift+drag pans and
/// Ctrl+drag zooms (up zooms in). A right click that doesn't move stays AutoCAD's (Enter, the
/// shortcut menus), and plan views keep AutoCAD's navigation. A drag starting in another 3D tile
/// makes it current and navigates it; returns true on that frame (the tiles have changed).
pub(super) fn right_drag(app: &mut CadApp, ui: &egui::Ui, canvas: &egui::Response, rect: Rect, tiles: &[(Rect, ModelTile)]) -> bool {
    let secondary = egui::PointerButton::Secondary;
    let (pressed, down, origin, pos, delta, mods) = ui.input(|i| {
        let p = &i.pointer;
        (p.button_pressed(secondary), p.button_down(secondary), p.press_origin(), p.latest_pos(), p.delta(), i.modifiers)
    });
    if !down {
        app.canvas.rnav = None;
        return false;
    }
    if pressed {
        app.canvas.rnav = origin.filter(|_| canvas.hovered()).map(|o| (o, false));
    }
    let Some((from, moving)) = app.canvas.rnav else { return false };
    if !moving {
        if pos.is_none_or(|p| p.distance(from) <= crate::context_menu::DRAG_PX) {
            return false;
        }
        let other = tiles.iter().position(|(r, t)| !t.active && r.contains(from));
        let dir = match other.and_then(|i| tiles.get(i)) {
            Some((_, t)) => Some(t.look.dir),
            None if rect.contains(from) => app.session.state().ok().filter(|st| st.space == Space::Model).map(|st| st.view_dir),
            None => None,
        };
        if dir.is_none_or(|d| d.is_plan()) || app.session.layout_space() != Space::Model {
            // Not a 3D model view: the right button is left to AutoCAD's ways.
            app.canvas.rnav = None;
            return false;
        }
        app.canvas.rnav = Some((from, true));
        if let Some(i) = other {
            if let Err(e) = app.session.activate_tile(i) {
                app.session.echo(e.to_string());
            }
            ui.ctx().request_repaint();
            return true;
        }
    }
    let (dx, dy) = (f64::from(delta.x), f64::from(delta.y));
    if dx == 0.0 && dy == 0.0 {
        return false;
    }
    if mods.shift {
        if let Ok(st) = app.session.state_mut() {
            let v = st.view();
            let scale = f64::from(rect.height()) / v.height.max(1e-12);
            st.set_view_quiet(cadcraft_engine::View { center: v.center - cadcraft_geom::Vec2::new(dx / scale, -dy / scale), height: v.height });
        }
    } else if mods.command || mods.ctrl {
        let center = app.session.state().map(|st| st.view().center);
        if let Ok(c) = center {
            let _ = app.session.zoom_about(1.01f64.powf(-dy), c);
        }
    } else {
        let _ = cadcraft_engine::orbit(&mut app.session, -dx * ORBIT_DEG_PER_PX, dy * ORBIT_DEG_PER_PX);
    }
    false
}

/// The preset views the title menu offers: label and command.
const PRESETS: [(&str, &str); 10] = [
    ("Top", "view.top"),
    ("Bottom", "view.bottom"),
    ("Front", "view.front"),
    ("Back", "view.back"),
    ("Left", "view.left"),
    ("Right", "view.right"),
    ("SW Isometric", "view.swiso"),
    ("SE Isometric", "view.seiso"),
    ("NE Isometric", "view.neiso"),
    ("NW Isometric", "view.nwiso"),
];

/// A look's visual style, as the title names it.
fn style_label(l: &TileLook) -> &'static str {
    match (l.shaded, l.edges) {
        (true, true) => crate::tl!("Shaded with Edges"),
        (true, false) => crate::tl!("Shaded"),
        _ => crate::tl!("2D Wireframe"),
    }
}

/// The title text of a tile looking `l`'s way.
pub(super) fn title_text(l: &TileLook) -> String {
    format!("+  |  {}  |  {}", crate::viewcube::view_name(l.dir), style_label(l))
}

/// Where the title of the tile drawn in `r` sits.
pub(super) fn title_rect(r: Rect, text_size: egui::Vec2) -> Rect {
    Rect::from_min_size(pos2(r.left() + 6.0, r.top() + 4.0), text_size + vec2(8.0, 8.0))
}

/// The viewport title in the corner of the tile drawn in `r` (`tile`: its index and whether it
/// is current, `None` for one view), as Rhino's: the view's name and visual style. On the Model
/// tab a click (either button) makes the tile current and opens the viewport menu: the preset
/// views, the visual styles, Maximize or Restore, and Rhino's four views; a double-click
/// maximizes the tile or brings the others back (VIEWPORTS.MAXIMIZE). While -VPORTS Join asks
/// for a viewport a click on a title answers it, as one in the tile does, and opens no menu.
/// Returns whether the pointer is on the title (the canvas leaves its pointing hand alone).
pub(super) fn title(app: &mut CadApp, ui: &egui::Ui, r: Rect, tile: Option<(usize, bool)>, look: TileLook) -> bool {
    let t = Tokens::get();
    let p = ui.painter_at(r);
    let galley = p.layout_no_wrap(title_text(&look), crate::theme::small(), t.canvas_ink);
    let at = title_rect(r, galley.size());
    if app.session.layout_space() != Space::Model {
        p.galley(at.min + vec2(4.0, 4.0), galley, t.canvas_ink);
        return false;
    }
    let id = ui.id().with(("viewport_title", tile.map(|t| t.0)));
    let resp = ui.interact(at, id, Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.hovered() {
        p.rect_filled(at, 3.0, t.control_hover);
    }
    p.galley(at.min + vec2(4.0, 4.0), galley, t.canvas_ink);
    let (clicked, double) = (resp.clicked() || resp.secondary_clicked(), resp.double_clicked());
    let joining = app.session.prompts_for_tile();
    if (clicked || double)
        && let Some((i, current)) = tile
        && (joining || !current)
        && let Err(e) = app.session.click_tile(i)
    {
        app.session.echo(e.to_string());
    }
    if joining {
        return resp.hovered();
    }
    let open = if double { Some(egui::SetOpenCommand::Bool(false)) } else { clicked.then_some(egui::SetOpenCommand::Toggle) };
    let tiled = tile.is_some();
    let remembered = app.session.state().is_ok_and(|st| st.tiles.maximized.is_some());
    let mut chosen: Option<(&str, Value)> = None;
    egui::Popup::menu(&resp).open_memory(open).show(|ui| {
        for (label, id) in PRESETS {
            if ui.radio(crate::viewcube::view_name(look.dir) == crate::i18n::t(label), crate::i18n::t(label)).clicked() {
                chosen = Some((id, json!({})));
            }
        }
        ui.separator();
        let styles = [
            (crate::tl!("Wireframe"), "2dwireframe", !look.shaded),
            (crate::tl!("Shaded"), "shaded", look.shaded && !look.edges),
            (crate::tl!("Shaded with Edges"), "shadedwithedges", look.shaded && look.edges),
        ];
        for (label, style, on) in styles {
            if ui.radio(on, label).clicked() {
                chosen = Some(("vscurrent", json!({ "style": style })));
            }
        }
        ui.separator();
        if tiled && ui.button(crate::tl!("Maximize")).clicked() {
            chosen = Some(("viewports.maximize", json!({})));
        }
        if !tiled && remembered && ui.button(crate::tl!("Restore Viewports")).clicked() {
            chosen = Some(("viewports.maximize", json!({})));
        }
        if ui.button(crate::tl!("Four Views")).clicked() {
            chosen = Some(("viewports.rhino", json!({})));
        }
        if chosen.is_some() {
            ui.close();
        }
    });
    if double {
        chosen = Some(("viewports.maximize", json!({})));
    }
    if let Some((id, params)) = chosen {
        let _ = app.run(id, params);
        ui.ctx().request_repaint();
    }
    resp.hovered()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A tile whose background build died is built on the UI thread, as the canvas's own list
    /// is, instead of starting the failing worker again every frame.
    #[test]
    fn a_lost_tile_build_falls_back_to_the_ui_thread() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.session.viewport_px = (800.0, 600.0);
        app.session.execute("line", &json!({ "points": [[0, 0], [20, 0], [20, 10]] })).unwrap();
        app.session.execute("viewports.rhino", &json!({})).unwrap();
        let tiles = super::super::tile_rects(&app, Rect::from_min_size(egui::Pos2::ZERO, vec2(1600.0, 1200.0)));
        let ctx = egui::Context::default();
        ensure_lists(&mut app, &tiles, &ctx);
        assert_eq!(app.canvas.tile_lists.len(), 3, "Perspective, Front and Right have lists of their own");
        for l in &mut app.canvas.tile_lists {
            (l.list, l.job) = (None, Some(rebuild::Job::lost(l.key)));
        }
        ensure_lists(&mut app, &tiles, &ctx);
        assert!(app.canvas.tile_lists.iter().all(|l| l.job.is_none() && l.list.is_some()), "built at once, no new worker");
    }
}
