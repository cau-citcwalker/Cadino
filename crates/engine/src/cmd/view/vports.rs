//! Tiled model space viewports: -VPORTS (SIngle, 2/3/4 with an arrangement, Join, Save, Restore,
//! Delete, ?), VPORTS and View > Viewports in the Model tab, and the current tile.
//!
//! The tiles shown are the drawing's `*Active` viewport configuration (saved in the DXF VPORT
//! table), the first tile being the current one; named configurations sit next to it. Arranging
//! tiles is an undo step. The session keeps each tile's view ([`crate::ModelTiles`]); the current
//! tile shows the model space view itself, so ZOOM, PAN and every pick work in it unchanged. The
//! UI draws the tiles from [`Session::model_tiles`] and hands clicks in other tiles to
//! [`Session::click_tile`]. In a layout -VPORTS is MVIEW.
//!
//! Each tile also has its own view direction and visual style ([`TileLook`]), as Rhino's
//! viewports do: the current tile's are the model view's ([`DocState::view_dir`]), so making
//! another tile current trades them too and every view command acts on the current tile. The
//! drawing stores each tile's look with its view; undo and redo bring the tiles back as they
//! were on show ([`crate::ModelTiles::left`]).
//! VIEWPORTS.RHINO lays out Rhino's four views; VIEWPORTS.SINGLE and VIEWPORTS.MAXIMIZE show
//! the current tile alone and bring the others back.
//!
//! Each tile has a construction plane too (CPLANE, [`Session::cplane`]). Unlike the looks it
//! lives in the drawing, so setting one is an undo step: the `*Active` tiles keep theirs
//! ([`VportTile::cplane`]), one view the header's UCS. Tiles are the same tiles whatever their
//! planes ([`same_tiles`]), so undoing a CPLANE leaves the views on show alone.

use std::sync::Arc;

use cadcraft_doc::{Drawing, Space, VportConfig, VportTile};
use cadcraft_geom::{Mat4, Vec2, Vec3, ViewDir};
use serde_json::{Value, json};

use super::super::qselect::wildcard;
use super::super::*;
use crate::{Accept, DocState, EngineError, Input, Interactive, LeftTiles, ModelTile, ModelTiles, Prompt, Result, Session, Step, TileLook, View};

/// Most tiles on screen at once.
const MAX_TILES: usize = 64;
/// The smallest tile side, as a fraction of the drawing area.
const MIN_SIDE: f64 = 1.0 / 64.0;
const EPS: f64 = 1e-6;

/// The -VPORTS Join prompts; a click in a tile answers them ([`Session::click_tile`]).
const DOMINANT_PROMPT: &str = "Select dominant viewport";
const JOIN_PROMPT: &str = "Select viewport to join";

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("-vports", "Viewports", run_vports)
            .params(
                "{option: single|2|3|4|join|save|restore|delete|list, arrangement?: horizontal|vertical (2) | horizontal|vertical|above|below|left|right (3; default right), applyTo?: current|display (default current: split the current tile), name? (save/restore), names? (delete/list; wildcards), dominant?, join? (tile numbers from 1; dominant defaults to the current tile)} (model space; in a layout: MVIEW)",
            )
            .interactive(vports_factory),
        CommandSpec::new("vports.activate", "Set Current Viewport", run_activate).params("{index: tile number from 1}").noundo(),
        CommandSpec::new("viewports.rhino", "Four Rhino Views", run_rhino)
            .menu(&["View", "Viewports", "Four Rhino Views"])
            .params("{} (Model tab): Top (current), Perspective (a south-west isometric, parallel projection), Front and Right, each zoomed to extents"),
        CommandSpec::new("viewports.single", "Maximize Viewport", run_single)
            .menu(&["View", "Viewports", "Maximize Viewport"])
            .params("{} (Model tab): the current tile alone, with its view; VIEWPORTS.MAXIMIZE brings the others back"),
        CommandSpec::new("viewports.maximize", "Maximize or Restore Viewport", run_maximize)
            .params("{} (Model tab): tiled, the current tile alone (VIEWPORTS.SINGLE); one view, the tiles it replaced back, else the four Rhino views"),
    ]
}

/// -VPORTS: the tile prompts in the Model tab, MVIEW's in a layout.
fn vports_factory(s: &Session) -> Result<Box<dyn Interactive>> {
    if let Some(m) = model_vports(s, None) {
        return Ok(m);
    }
    let mview = find_command("mview").and_then(|c| c.interactive).ok_or_else(|| EngineError::UnknownCommand("mview".into()))?;
    mview(s)
}

/// The tile machine for the Model tab (`count`: View > Viewports > n), else `None` (the caller
/// runs its layout form).
pub(in crate::cmd) fn model_vports(s: &Session, count: Option<usize>) -> Option<Box<dyn Interactive>> {
    if s.layout_space() != Space::Model {
        return None;
    }
    Some(Box::new(VportsM { count, ..Default::default() }))
}

/// The JSON form of VPORTS / View > Viewports > n in the Model tab: `count` tiles (1: single).
pub(in crate::cmd) fn tile_model(s: &mut Session, p: &Value, count: usize) -> Result<Value> {
    let option = if count <= 1 { "single".to_string() } else { count.to_string() };
    run_vports(s, &with_option(p, &option))
}

fn with_option(p: &Value, option: &str) -> Value {
    let mut p = if p.is_object() { p.clone() } else { json!({}) };
    if let Some(o) = p.as_object_mut() {
        o.insert("option".into(), json!(option));
    }
    p
}

/// The tiles of viewport configuration `name` (any case), for MVIEW Restore: `*Active` is the
/// model space as shown now (one full tile with the model view when it isn't tiled), any other
/// name a saved configuration. `None` when there is no such configuration.
pub(in crate::cmd) fn config_tiles(s: &Session, name: &str) -> Option<Vec<VportTile>> {
    let name = name.trim();
    if name.eq_ignore_ascii_case(VportConfig::ACTIVE) {
        return Some(live(s.state().ok()?, s.viewport_px).0.into_iter().map(|(t, l)| with_look(t, l)).collect());
    }
    s.doc().ok()?.vports.iter().find(|c| !c.is_active() && c.name.eq_ignore_ascii_case(name)).map(|c| c.tiles.clone())
}

/// The saved viewport configurations' names.
pub(in crate::cmd) fn config_names(s: &Session) -> Vec<String> {
    s.doc().map(|d| d.vports.iter().filter(|c| !c.is_active()).map(|c| c.name.clone()).collect()).unwrap_or_default()
}

// ---------------- tile state ----------------

type Rect = (Vec2, Vec2);

/// A tile with its look.
type Tile = (VportTile, TileLook);

fn rect(t: &VportTile) -> Rect {
    (t.min, t.max)
}

fn view_of(t: &VportTile) -> View {
    View { center: t.center, height: t.height }
}

fn active_config(d: &Drawing) -> Option<&VportConfig> {
    d.vports.iter().find(|c| c.is_active() && c.tiles.len() > 1)
}

/// The drawing's `*Active` tiles (empty: one view).
fn active_tiles(d: &Drawing) -> Vec<VportTile> {
    active_config(d).map(|c| c.tiles.clone()).unwrap_or_default()
}

/// The same tiles, whatever their construction planes (CPLANE changes the drawing's tiles; the
/// views and looks on show stay theirs).
fn same_tiles(a: &[VportTile], b: &[VportTile]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| VportTile { cplane: y.cplane, ..*x } == *y)
}

/// The look the drawing stores with tile `t` (plan, wireframe when it stores none).
fn look_of(t: &VportTile) -> TileLook {
    let [x, y, z] = t.view_dir;
    let v = Vec3::new(x, y, z);
    // A unit direction as stored, so a round trip keeps it bit for bit; anything else normalised.
    let dir = if v.is_finite() && (v.len() - 1.0).abs() < 1e-12 { ViewDir { dir: v } } else { ViewDir::new(v) };
    TileLook { dir, shaded: t.shaded, edges: t.edges }
}

/// Tile `t` with look `l` stored in it.
fn with_look(t: VportTile, l: TileLook) -> VportTile {
    let d = l.dir.dir;
    VportTile { view_dir: [d.x, d.y, d.z], shaded: l.shaded, edges: l.edges, ..t }
}

/// The model view shows view `v` along look `l`; from a layout too (undo or redo there), for
/// when the Model tab shows again.
fn show(st: &mut DocState, v: View, l: TileLook) {
    if st.space == Space::Model && st.mspace.is_none() {
        st.set_view_quiet(v);
    } else if crate::usable_view(&v) {
        match st.views.iter_mut().find(|(sp, _)| *sp == Space::Model) {
            Some((_, m)) => *m = v,
            None => st.views.push((Space::Model, v)),
        }
    }
    // The direction and style are the Model tab's whatever tab is on show.
    st.set_look(l);
}

/// The tiles on show with their views and looks as they are now (the current one's are the
/// model view's), and the current tile. One full tile when there is no tiling.
fn live(st: &DocState, px: (f64, f64)) -> (Vec<Tile>, usize) {
    let now = st.views.iter().find(|(sp, _)| *sp == Space::Model).map(|(_, v)| *v).unwrap_or_default();
    let Some(cfg) = active_config(&st.doc) else {
        let aspect = px.0 / px.1.max(1.0);
        let cplane = st.doc.header.cplane();
        let one =
            VportTile { min: Vec2::ZERO, max: Vec2::new(1.0, 1.0), center: now.center, height: now.height, aspect, cplane, ..Default::default() };
        return (vec![(one, st.look())], 0);
    };
    let n = cfg.tiles.len();
    let synced = same_tiles(&st.tiles.config, &cfg.tiles) && st.tiles.views.len() == n && st.tiles.looks.len() == n;
    let mut tiles: Vec<Tile> = if synced {
        cfg.tiles
            .iter()
            .zip(&st.tiles.views)
            .zip(&st.tiles.looks)
            .map(|((t, v), l)| (VportTile { center: v.center, height: v.height, ..*t }, *l))
            .collect()
    } else {
        cfg.tiles.iter().map(|t| (*t, look_of(t))).collect()
    };
    // Tiles the session hasn't shown yet (a drawing just opened): the first is current, as the
    // drawing stores them.
    let active = if synced { st.tiles.active.min(n - 1) } else { 0 };
    if let Some((t, l)) = tiles.get_mut(active) {
        (t.center, t.height) = (now.center, now.height);
        *l = st.look();
    }
    // Screen aspects from the current tile's size in pixels.
    if let Some((cur, _)) = tiles.get(active).copied() {
        let (fw, fh) = ((cur.max.x - cur.min.x).max(EPS), (cur.max.y - cur.min.y).max(EPS));
        let (w, h) = (px.0 / fw, px.1.max(1.0) / fh);
        for (t, _) in tiles.iter_mut() {
            t.aspect = ((t.max.x - t.min.x) * w / ((t.max.y - t.min.y) * h).max(EPS)).max(EPS);
        }
    }
    (tiles, active)
}

/// The drawing as a file should keep it: its `*Active` tiles with the views, directions and
/// visual styles on show (making a tile current, orbiting and VSCURRENT change only the
/// session), the current tile first. `None` when there is nothing to bring up to date.
/// One view (maximized by VIEWPORTS.SINGLE) keeps the tiles it replaced, itself in its place,
/// as VIEWPORTS.MAXIMIZE would bring them back; one view that looks along a direction or in a
/// style of its own is a one-tile `*Active`.
pub(crate) fn with_live_tiles(s: &Session) -> Option<Drawing> {
    let st = s.state().ok()?;
    if st.block_edit.is_some() {
        return None;
    }
    let (mut tiles, mut active) = live(st, s.viewport_px);
    if active_config(&st.doc).is_none() {
        let one = tiles.first().copied()?;
        match st.tiles.maximized.clone() {
            Some((ts, looks, current)) if current < ts.len() && ts.len() == looks.len() => {
                tiles = ts.into_iter().zip(looks).collect();
                if let Some((t, l)) = tiles.get_mut(current) {
                    (t.center, t.height, t.cplane) = (one.0.center, one.0.height, one.0.cplane);
                    *l = one.1;
                }
                active = current;
            }
            _ if one.1 != TileLook::default() => {}
            // Plan wireframe: the file's own one view (if it kept one) is out of date.
            _ if st.doc.vports.iter().any(|c| c.is_active()) => {}
            _ => return None,
        }
    }
    if active < tiles.len() {
        let cur = tiles.remove(active);
        tiles.insert(0, cur);
    }
    let mut d = (*st.doc).clone();
    let plain = tiles.len() == 1 && tiles.first().is_some_and(|t| t.1 == TileLook::default());
    let config = VportConfig { name: VportConfig::ACTIVE.into(), tiles: tiles.iter().map(|(t, l)| with_look(*t, *l)).collect() };
    d.vports.retain(|c| !c.is_active());
    if !plain {
        d.vports.insert(0, config);
    }
    Some(d)
}

impl DocState {
    /// Remember the tiles as they are on show, before the drawing's tiles change (VPORTS, undo,
    /// redo): coming back to these tiles brings them back exactly ([`ModelTiles::left`]).
    pub(crate) fn leave_tiles(&mut self, px: (f64, f64)) {
        let config = active_tiles(&self.doc);
        let (tiles, active) = live(self, px);
        let doc = Arc::downgrade(&self.doc);
        let left = &mut self.tiles.left;
        left.retain(|l| !l.doc.ptr_eq(&doc));
        left.push(LeftTiles { doc, config, tiles: tiles.into_iter().map(|(t, l)| with_look(t, l)).collect(), active });
        let over = left.len().saturating_sub(ModelTiles::LEFT);
        left.drain(..over);
    }

    /// After undo or redo put back a drawing whose tiles are not the ones on show: show them as
    /// the session left them, the current one current again with its view and look, or (never
    /// left in this session) as the drawing stores them, the first one current.
    pub(crate) fn tiles_back(&mut self) {
        let cfg = active_tiles(&self.doc);
        if same_tiles(&self.tiles.config, &cfg) {
            return;
        }
        let n = cfg.len().max(1);
        // The tiles left with this very drawing, else the last left with the same tiles.
        let doc = Arc::downgrade(&self.doc);
        let fits = |l: &&LeftTiles| same_tiles(&l.config, &cfg) && l.tiles.len() == n && l.active < n;
        let mine = self.tiles.left.iter().rev().filter(fits).find(|l| l.doc.ptr_eq(&doc));
        let left = mine.or_else(|| self.tiles.left.iter().rev().find(fits)).map(|l| (l.tiles.clone(), l.active));
        let (tiles, active) = left.unwrap_or_else(|| (cfg.clone(), 0));
        let old = std::mem::take(&mut self.tiles);
        // The tiles a VIEWPORTS.SINGLE replaced belong to the view it left: saving writes what is
        // on show from here on.
        // ponytail: redoing VIEWPORTS.SINGLE forgets them too (MAXIMIZE then gives Rhino's four);
        // upgrade: key them by the drawing, as `left` is.
        self.tiles = ModelTiles { left: old.left, ..Default::default() };
        // Other tiles' views: ZOOM Previous starts again.
        self.view_history.clear();
        if cfg.len() > 1 {
            self.tiles.views = tiles.iter().map(view_of).collect();
            self.tiles.looks = tiles.iter().map(look_of).collect();
            self.tiles.config = cfg;
            self.tiles.active = active;
        }
        // One view never left in this session: nothing is known about it, the view stays.
        if let Some(t) = tiles.get(active) {
            show(self, view_of(t), look_of(t));
        }
    }

    /// A drawing just opened shows its current tile's direction and style as the file stores
    /// them (a plan wireframe tile keeps the view the drawing opens on).
    /// (One view keeps a one-tile `*Active` when it has a look of its own.)
    pub(crate) fn tiles_opened(&mut self) {
        if let Some(t) = self.doc.vports.iter().find(|c| c.is_active()).and_then(|c| c.tiles.first()).copied()
            && look_of(&t) != TileLook::default()
        {
            show(self, view_of(&t), look_of(&t));
        }
    }
}

/// Show `tiles` with tile `current` current: the drawing's `*Active` configuration with each
/// tile's view and look (one undo step with the command) and the session's views and looks.
fn apply(s: &mut Session, mut tiles: Vec<Tile>, current: usize) -> Result<()> {
    if tiles.is_empty() {
        return Err(EngineError::Other("A viewport configuration needs a viewport.".into()));
    }
    let px = s.viewport_px;
    let cur = tiles.remove(current.min(tiles.len() - 1));
    tiles.insert(0, cur);
    let st = s.state_mut()?;
    st.leave_tiles(px);
    let left = std::mem::take(&mut st.tiles.left);
    // New tiles: ZOOM Previous starts again (the views it kept belong to the old ones).
    st.view_history.clear();
    let d = Arc::make_mut(&mut st.doc);
    d.vports.retain(|c| !c.is_active());
    // One view keeps the current tile's construction plane as the drawing's UCS.
    if tiles.len() == 1 && d.header.cplane() != cur.0.cplane {
        d.header.set_cplane(&cur.0.cplane);
    }
    if tiles.len() > 1 {
        let config: Vec<VportTile> = tiles.iter().map(|(t, l)| with_look(*t, *l)).collect();
        d.vports.insert(0, VportConfig { name: VportConfig::ACTIVE.into(), tiles: config.clone() });
        st.tiles = ModelTiles {
            config,
            views: tiles.iter().map(|t| view_of(&t.0)).collect(),
            looks: tiles.iter().map(|t| t.1).collect(),
            left,
            ..Default::default()
        };
    } else {
        st.tiles = ModelTiles { left, ..Default::default() };
    }
    show(st, view_of(&cur.0), cur.1);
    s.touch();
    Ok(())
}

/// A look's visual style as VSCURRENT names it.
fn style_name(l: &TileLook) -> &'static str {
    match (l.shaded, l.edges) {
        (false, _) => "2dWireframe",
        (true, false) => "Shaded",
        (true, true) => "ShadedWithEdges",
    }
}

/// A stored construction plane as used: exactly World when it is within rounding of it (plan
/// behaviour stays exact), World too when it isn't an orthonormal frame (hostile files).
fn usable(c: Mat4) -> Mat4 {
    let frame = c.is_affine() && Mat4::frame(c.col(3), c.col(0), c.col(1)).is_some_and(|f| near(&f, &c, 1e-9));
    if near(&c, &Mat4::IDENTITY, 1e-12) || !frame { Mat4::IDENTITY } else { c }
}

fn near(a: &Mat4, b: &Mat4, tol: f64) -> bool {
    a.m.iter().flatten().zip(b.m.iter().flatten()).all(|(x, y)| (x - y).abs() <= tol)
}

fn summary(s: &Session) -> Result<Value> {
    let (tiles, active) = live(s.state()?, s.viewport_px);
    let rows: Vec<Value> = tiles
        .iter()
        .enumerate()
        .map(|(i, (t, l))| {
            let d = l.dir.dir;
            json!({ "index": i + 1, "min": [t.min.x, t.min.y], "max": [t.max.x, t.max.y], "center": [t.center.x, t.center.y], "height": t.height,
                "direction": [d.x, d.y, d.z], "style": style_name(l), "current": i == active })
        })
        .collect();
    Ok(json!({ "tiles": rows }))
}

/// Split rectangle `r` into `n` tiles (2 to 4) by `arrangement`; the first is the main one.
fn split(r: Rect, n: usize, arrangement: &str) -> Option<Vec<Rect>> {
    let ((x0, y0), (x1, y1)) = ((r.0.x, r.0.y), (r.1.x, r.1.y));
    let (xm, ym) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let b = |a: f64, b: f64, c: f64, d: f64| (Vec2::new(a, b), Vec2::new(c, d));
    let third = |a: f64, z: f64, k: f64| a + (z - a) * k / 3.0;
    let a = arrangement.trim().to_ascii_lowercase();
    let a = a.as_str();
    let out = match (n, a) {
        (2, "h" | "horizontal") => vec![b(x0, ym, x1, y1), b(x0, y0, x1, ym)],
        (2, "" | "v" | "vertical") => vec![b(x0, y0, xm, y1), b(xm, y0, x1, y1)],
        (3, "h" | "horizontal") => {
            let (ya, yb) = (third(y0, y1, 2.0), third(y0, y1, 1.0));
            vec![b(x0, ya, x1, y1), b(x0, yb, x1, ya), b(x0, y0, x1, yb)]
        }
        (3, "v" | "vertical") => {
            let (xa, xb) = (third(x0, x1, 1.0), third(x0, x1, 2.0));
            vec![b(x0, y0, xa, y1), b(xa, y0, xb, y1), b(xb, y0, x1, y1)]
        }
        (3, "a" | "above") => vec![b(x0, ym, x1, y1), b(x0, y0, xm, ym), b(xm, y0, x1, ym)],
        (3, "b" | "below") => vec![b(x0, y0, x1, ym), b(x0, ym, xm, y1), b(xm, ym, x1, y1)],
        (3, "l" | "left") => vec![b(x0, y0, xm, y1), b(xm, ym, x1, y1), b(xm, y0, x1, ym)],
        (3, "" | "r" | "right") => vec![b(xm, y0, x1, y1), b(x0, ym, xm, y1), b(x0, y0, xm, ym)],
        (4, _) => vec![b(x0, ym, xm, y1), b(xm, ym, x1, y1), b(x0, y0, xm, ym), b(xm, y0, x1, ym)],
        _ => return None,
    };
    Some(out)
}

/// The rectangle two tiles make together, if they share a whole side.
fn joined(a: Rect, b: Rect) -> Option<Rect> {
    let near = |p: f64, q: f64| (p - q).abs() < EPS;
    let rows = near(a.0.y, b.0.y) && near(a.1.y, b.1.y) && (near(a.1.x, b.0.x) || near(b.1.x, a.0.x));
    let cols = near(a.0.x, b.0.x) && near(a.1.x, b.1.x) && (near(a.1.y, b.0.y) || near(b.1.y, a.0.y));
    (rows || cols).then(|| (Vec2::new(a.0.x.min(b.0.x), a.0.y.min(b.0.y)), Vec2::new(a.1.x.max(b.1.x), a.1.y.max(b.1.y))))
}

fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.chars().count() <= 255 && !n.starts_with('*') && !n.chars().any(|c| "<>/\\\":;?*|,=`".contains(c))
}

fn model_only(s: &Session) -> Result<()> {
    if s.layout_space() == Space::Model {
        Ok(())
    } else {
        Err(EngineError::Other("Tiled viewports are for the Model tab; use MVIEW in a layout.".into()))
    }
}

/// A tile number (from 1) in `p[key]`.
fn tile_param(p: &Value, key: &str, n: usize) -> Result<Option<usize>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let i = v.as_u64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok())).and_then(|i| usize::try_from(i).ok());
            match i {
                Some(i) if (1..=n).contains(&i) => Ok(Some(i - 1)),
                _ => Err(bad("-vports", format!("`{key}` must be a viewport number from 1 to {n}"))),
            }
        }
    }
}

fn names(p: &Value) -> Vec<String> {
    let raw: Vec<String> = match p.get("names").or_else(|| p.get("name")) {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(Value::String(t)) => t.split(',').map(str::to_string).collect(),
        _ => Vec::new(),
    };
    raw.into_iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).take(1000).collect()
}

fn run_vports(s: &mut Session, p: &Value) -> Result<Value> {
    let option = match p.get("option") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(t)) => t.trim().to_ascii_lowercase(),
        _ => "list".into(),
    };
    let count = match option.as_str() {
        "single" | "si" | "1" => Some(1),
        "2" | "3" | "4" => option.parse::<u64>().ok(),
        _ => None,
    };
    if let Some(count) = count
        && (s.layout_space() != Space::Model || p.get("layout").is_some())
    {
        // In a layout, -VPORTS makes layout viewports: MVIEW.
        let mview = find_command("mview").ok_or_else(|| EngineError::UnknownCommand("mview".into()))?;
        let mut p = p.clone();
        if let Some(o) = p.as_object_mut() {
            o.remove("option");
            o.insert("count".into(), json!(count));
        }
        return (mview.run)(s, &p);
    }
    let (tiles, current) = live(s.state()?, s.viewport_px);
    let n = tiles.len();
    match option.as_str() {
        "single" | "si" | "1" => {
            model_only(s)?;
            let (cur, look) = tiles.get(current).copied().ok_or(EngineError::NoDocument)?;
            apply(s, vec![(VportTile { min: Vec2::ZERO, max: Vec2::new(1.0, 1.0), ..cur }, look)], 0)?;
        }
        "2" | "3" | "4" => {
            model_only(s)?;
            let count = option.parse::<usize>().unwrap_or(2);
            let display = str_param(p, "applyTo").is_some_and(|a| a.eq_ignore_ascii_case("display"));
            let (cur, look) = tiles.get(current).copied().ok_or(EngineError::NoDocument)?;
            let area = if display { (Vec2::ZERO, Vec2::new(1.0, 1.0)) } else { rect(&cur) };
            let arrangement = str_param(p, "arrangement").unwrap_or("");
            let parts = split(area, count, arrangement)
                .ok_or_else(|| bad("-vports", format!("unknown arrangement `{arrangement}` for {count} viewports")))?;
            if parts.iter().any(|r| r.1.x - r.0.x < MIN_SIDE - EPS || r.1.y - r.0.y < MIN_SIDE - EPS) {
                return Err(EngineError::Other("The current viewport is too small to divide.".into()));
            }
            let mut out: Vec<Tile> =
                if display { Vec::new() } else { tiles.iter().enumerate().filter(|(i, _)| *i != current).map(|(_, t)| *t).collect() };
            if out.len() + parts.len() > MAX_TILES {
                return Err(EngineError::Other(format!("At most {MAX_TILES} viewports can be shown.")));
            }
            let first = out.len();
            out.extend(parts.into_iter().map(|(min, max)| (VportTile { min, max, ..cur }, look)));
            apply(s, out, first)?;
        }
        "join" | "j" => {
            model_only(s)?;
            let dominant = tile_param(p, "dominant", n)?.unwrap_or(current);
            let other = tile_param(p, "join", n)?.ok_or_else(|| bad("-vports", "`join` (the viewport to join) is required"))?;
            let (Some(a), Some(b)) = (tiles.get(dominant).copied(), tiles.get(other).copied()) else {
                return Err(EngineError::Other("There is only one viewport.".into()));
            };
            if dominant == other {
                return Err(EngineError::Other("Select a different viewport to join.".into()));
            }
            let (min, max) =
                joined(rect(&a.0), rect(&b.0)).ok_or_else(|| EngineError::Other("The selected viewports do not form a rectangle.".into()))?;
            let mut out: Vec<Tile> = tiles.iter().enumerate().filter(|(i, _)| *i != dominant && *i != other).map(|(_, t)| *t).collect();
            out.push((VportTile { min, max, ..a.0 }, a.1));
            let current = out.len() - 1;
            apply(s, out, current)?;
        }
        "save" | "s" => {
            model_only(s)?;
            let name = str_param(p, "name").map(str::trim).unwrap_or_default().to_string();
            if !valid_name(&name) {
                return Err(EngineError::Other(format!("\"{name}\" is not a valid viewport configuration name.")));
            }
            // The current tile first, as in the shown configuration.
            let mut saved = tiles.clone();
            let cur = saved.remove(current);
            saved.insert(0, cur);
            let d = s.doc_mut()?;
            d.vports.retain(|c| !c.name.eq_ignore_ascii_case(&name));
            d.vports.push(VportConfig { name: name.clone(), tiles: saved.into_iter().map(|(t, l)| with_look(t, l)).collect() });
            return Ok(json!({ "name": name, "message": format!("Viewport configuration \"{name}\" saved.") }));
        }
        "restore" | "r" => {
            model_only(s)?;
            let name = str_param(p, "name").map(str::trim).unwrap_or_default();
            let cfg = s
                .doc()?
                .vports
                .iter()
                .find(|c| !c.is_active() && c.name.eq_ignore_ascii_case(name))
                .cloned()
                .ok_or_else(|| EngineError::Other(format!("Could not find viewport configuration \"{name}\".")))?;
            apply(s, cfg.tiles.iter().map(|t| (*t, look_of(t))).collect(), 0)?;
        }
        "delete" | "d" => {
            let pats = names(p);
            if pats.is_empty() {
                return Err(bad("-vports", "`names` is required"));
            }
            let d = s.doc_mut()?;
            let gone: Vec<String> =
                d.vports.iter().filter(|c| !c.is_active() && pats.iter().any(|q| wildcard(q, &c.name))).map(|c| c.name.clone()).collect();
            if gone.is_empty() {
                return Err(EngineError::Other("No matching viewport configurations found.".into()));
            }
            d.vports.retain(|c| !gone.contains(&c.name));
            return Ok(json!({ "deleted": gone, "message": format!("{} viewport configuration(s) deleted.", gone.len()) }));
        }
        "list" | "?" => {
            let pats = names(p);
            let pats = if pats.is_empty() { vec!["*".to_string()] } else { pats };
            let mut lines = vec!["Current configuration:".to_string()];
            let corners = |t: &VportTile| format!("{:.4},{:.4}  {:.4},{:.4}", t.min.x, t.min.y, t.max.x, t.max.y);
            for (i, (t, _)) in tiles.iter().enumerate() {
                lines.push(format!("  {}{}  {}", i + 1, if i == current { "*" } else { " " }, corners(t)));
            }
            let saved: Vec<&VportConfig> = s.doc()?.vports.iter().filter(|c| !c.is_active() && pats.iter().any(|q| wildcard(q, &c.name))).collect();
            lines.push("Saved configurations:".into());
            if saved.is_empty() {
                lines.push("  (none)".into());
            }
            for c in &saved {
                lines.push(format!("  {}: {} viewport(s)", c.name, c.tiles.len()));
            }
            let configs: Vec<Value> = saved.iter().map(|c| json!({ "name": c.name, "viewports": c.tiles.len() })).collect();
            let mut r = summary(s)?;
            if let Some(o) = r.as_object_mut() {
                o.insert("configurations".into(), json!(configs));
                o.insert("message".into(), json!(lines.join("\n")));
            }
            return Ok(r);
        }
        _ => return Err(bad("-vports", "option must be single, 2, 3, 4, join, save, restore, delete or list")),
    }
    summary(s)
}

/// Rhino's four views (4VIEW), quadrant by quadrant as [`split`] makes them: Top, Perspective,
/// Front, Right (directions toward the viewer), with their construction planes (World, World,
/// Front, Right).
/// ponytail: "Perspective" is a parallel south-west isometric; a perspective camera needs a
/// projection in `ViewDir`, the render crate and the canvas.
const RHINO_VIEWS: [([f64; 3], &str); 4] =
    [([0.0, 0.0, 1.0], "top"), ([-1.0, -1.0, 1.0], "world"), ([0.0, -1.0, 0.0], "front"), ([1.0, 0.0, 0.0], "right")];

fn run_rhino(s: &mut Session, _p: &Value) -> Result<Value> {
    model_only(s)?;
    let st = s.state()?;
    let (tiles, current) = live(st, s.viewport_px);
    let look = st.look();
    let (cur, _) = tiles.get(current).copied().ok_or(EngineError::NoDocument)?;
    // The drawing area's size in pixels, from the current tile's.
    let (w, h) = (s.viewport_px.0 / (cur.max.x - cur.min.x).max(EPS), s.viewport_px.1.max(1.0) / (cur.max.y - cur.min.y).max(EPS));
    let parts = split((Vec2::ZERO, Vec2::new(1.0, 1.0)), 4, "").unwrap_or_default();
    let mut out: Vec<Tile> = Vec::with_capacity(4);
    for ((min, max), ([x, y, z], plane)) in parts.into_iter().zip(RHINO_VIEWS) {
        let dir = ViewDir::new(Vec3::new(x, y, z));
        let aspect = ((max.x - min.x) * w / ((max.y - min.y) * h).max(EPS)).max(EPS);
        let v = s.extents_frame_along(dir, aspect)?;
        let cplane = super::cplane::preset(plane).unwrap_or(Mat4::IDENTITY);
        out.push((VportTile { min, max, center: v.center, height: v.height, aspect, cplane, ..Default::default() }, TileLook { dir, ..look }));
    }
    apply(s, out, 0)?;
    summary(s)
}

/// The current tile alone, the others remembered for VIEWPORTS.MAXIMIZE. One view stays as it is.
fn run_single(s: &mut Session, _p: &Value) -> Result<Value> {
    model_only(s)?;
    let (tiles, current) = live(s.state()?, s.viewport_px);
    if tiles.len() < 2 {
        return summary(s);
    }
    let r = run_vports(s, &json!({ "option": "single" }))?;
    let (t, l) = tiles.into_iter().unzip();
    s.state_mut()?.tiles.maximized = Some((t, l, current));
    Ok(r)
}

/// Tiled: VIEWPORTS.SINGLE. One view: the tiles VIEWPORTS.SINGLE replaced, the current one
/// showing the view as it is now; with none remembered, the four Rhino views.
fn run_maximize(s: &mut Session, p: &Value) -> Result<Value> {
    model_only(s)?;
    let st = s.state()?;
    if active_config(&st.doc).is_some() {
        return run_single(s, p);
    }
    let Some((tiles, looks, current)) = st.tiles.maximized.clone() else { return run_rhino(s, p) };
    let now = st.views.iter().find(|(sp, _)| *sp == Space::Model).map(|(_, v)| *v).unwrap_or_default();
    let mut out: Vec<Tile> = tiles.into_iter().zip(looks).collect();
    if let Some((t, l)) = out.get_mut(current) {
        (t.center, t.height, t.cplane) = (now.center, now.height, st.doc.header.cplane());
        *l = st.look();
    }
    apply(s, out, current)?;
    summary(s)
}

fn run_activate(s: &mut Session, p: &Value) -> Result<Value> {
    let st = s.state()?;
    let n = active_config(&st.doc).map_or(1, |c| c.tiles.len());
    let i = tile_param(p, "index", n)?.ok_or_else(|| bad("vports.activate", "`index` is required"))?;
    s.activate_tile(i)?;
    summary(s)
}

impl Session {
    /// The model space tiles to draw (empty: one view, or not in the Model tab).
    pub fn model_tiles(&self) -> Vec<ModelTile> {
        let Ok(st) = self.state() else { return Vec::new() };
        if st.space != Space::Model || active_config(&st.doc).is_none() {
            return Vec::new();
        }
        let (tiles, active) = live(st, self.viewport_px);
        // The Block Editor works in World (see `cplane`).
        let editing = st.block_edit.is_some();
        tiles
            .iter()
            .enumerate()
            .map(|(i, (t, l))| ModelTile {
                min: t.min,
                max: t.max,
                view: view_of(t),
                look: *l,
                active: i == active,
                cplane: if editing { Mat4::IDENTITY } else { usable(t.cplane) },
            })
            .collect()
    }

    /// The current construction plane (CPLANE, UCS): its coordinates (x, y in the plane, z along
    /// its normal) → world. The current tile's on the Model tab, or with one view the drawing's
    /// ([`cadcraft_doc::Header::cplane`]; a file from another program opens in World); World
    /// (the identity) in layouts, MSPACE viewports and the Block Editor, and for a plan-only
    /// command while it runs ([`crate::cplane::PLAN_ONLY`]). Points given to commands are in these
    /// coordinates.
    pub fn cplane(&self) -> Mat4 {
        if self.plan_world.load(std::sync::atomic::Ordering::Relaxed) {
            return Mat4::IDENTITY;
        }
        let Ok(st) = self.state() else { return Mat4::IDENTITY };
        if st.space != Space::Model || st.mspace.is_some() || st.block_edit.is_some() {
            return Mat4::IDENTITY;
        }
        let (tiles, active) = live(st, self.viewport_px);
        tiles.get(active).map_or(Mat4::IDENTITY, |t| usable(t.0.cplane))
    }

    /// Make `c` (an orthonormal frame) the current construction plane: the current tile's, or with
    /// one view the drawing's UCS. Changes the drawing (an undo step with the command).
    pub(crate) fn set_cplane(&mut self, c: Mat4) -> Result<()> {
        let st = self.state()?;
        if st.block_edit.is_some() {
            return Err(EngineError::Other("The Block Editor works in the World construction plane.".into()));
        }
        if st.space != Space::Model || st.mspace.is_some() {
            return Err(EngineError::Other("Construction planes are for the Model tab.".into()));
        }
        if self.cplane() == c {
            return Ok(());
        }
        let (_, active) = live(st, self.viewport_px);
        let d = self.doc_mut()?;
        match d.vports.iter_mut().find(|k| k.is_active() && k.tiles.len() > 1) {
            Some(cfg) => {
                if let Some(t) = cfg.tiles.get_mut(active) {
                    t.cplane = c;
                }
            }
            None => d.header.set_cplane(&c),
        }
        Ok(())
    }

    /// Make tile `index` (from 0, in [`Session::model_tiles`] order) the current one: the views
    /// trade places; nothing in the drawing changes.
    pub fn activate_tile(&mut self, index: usize) -> Result<()> {
        let (tiles, active) = live(self.state()?, self.viewport_px);
        if tiles.len() < 2 || index >= tiles.len() {
            return Err(EngineError::Other("No such viewport.".into()));
        }
        if index == active {
            return Ok(());
        }
        let st = self.state_mut()?;
        // ZOOM Previous: each tile its own views.
        let mut histories = std::mem::take(&mut st.tiles.histories);
        histories.resize(tiles.len(), Vec::new());
        if let Some(h) = histories.get_mut(active) {
            *h = std::mem::take(&mut st.view_history);
        }
        st.view_history = histories.get_mut(index).map(std::mem::take).unwrap_or_default();
        st.tiles = ModelTiles {
            config: active_tiles(&st.doc),
            views: tiles.iter().map(|t| view_of(&t.0)).collect(),
            looks: tiles.iter().map(|t| t.1).collect(),
            active: index,
            left: std::mem::take(&mut st.tiles.left),
            maximized: st.tiles.maximized.take(),
            histories,
        };
        if let Some((t, l)) = tiles.get(index) {
            st.set_view_quiet(view_of(t));
            st.set_look(*l);
        }
        // No new revision: the drawing is as it was, so the canvas keeps every tile's display
        // list and only trades them (a revision would rebuild them all).
        Ok(())
    }

    /// A click in tile `index` (or on its title): it answers a -VPORTS Join prompt, else makes
    /// the tile current (also in the middle of a command, as in AutoCAD).
    pub fn click_tile(&mut self, index: usize) -> Result<()> {
        if self.prompts_for_tile() {
            return self.input(Input::Text((index + 1).to_string()));
        }
        self.activate_tile(index)
    }

    /// Whether -VPORTS Join is asking for a viewport (a click in a tile answers it).
    pub fn prompts_for_tile(&self) -> bool {
        self.current_prompt().is_some_and(|p| p.message == DOMINANT_PROMPT || p.message == JOIN_PROMPT)
    }
}

// ---------------- prompts ----------------

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Stage {
    #[default]
    Main,
    Arrange(usize),
    Save,
    Restore,
    Delete,
    List,
    Dominant,
    Join(Option<usize>),
}

/// -VPORTS in the Model tab: "Enter an option [Save/Restore/Delete/Join/SIngle/?/2/3/4] <3>:".
#[derive(Default)]
struct VportsM {
    stage: Stage,
    /// View > Viewports > n: start at that count.
    count: Option<usize>,
}

impl VportsM {
    fn run(s: &mut Session, p: Value) -> Result<Step> {
        let r = run_vports(s, &p)?;
        if let Some(m) = r.get("message").and_then(Value::as_str) {
            for l in m.lines() {
                s.echo(l.to_string());
            }
        }
        Ok(Step::Done)
    }
    fn count(&mut self, s: &mut Session, n: usize) -> Result<Step> {
        if n == 4 {
            return Self::run(s, json!({ "option": "4" }));
        }
        if n <= 1 {
            return Self::run(s, json!({ "option": "single" }));
        }
        self.stage = Stage::Arrange(n);
        Ok(Step::Continue)
    }
}

impl Interactive for VportsM {
    fn name(&self) -> &'static str {
        "VPORTS"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        match self.count {
            Some(n) => self.count(s, n),
            None => Ok(Step::Continue),
        }
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.stage {
            Stage::Main => {
                Prompt::new("Enter an option", Accept::TEXT).kw(&["Save", "Restore", "Delete", "Join", "SIngle", "?", "2", "3", "4"]).default("3")
            }
            Stage::Arrange(2) => Prompt::new("Enter a configuration option", Accept::TEXT).kw(&["Horizontal", "Vertical"]).default("Vertical"),
            Stage::Arrange(_) => Prompt::new("Enter a configuration option", Accept::TEXT)
                .kw(&["Horizontal", "Vertical", "Above", "Below", "Left", "Right"])
                .default("Right"),
            Stage::Save => Prompt::new("Enter name for new viewport configuration", Accept::TEXT).kw(&["?"]),
            Stage::Restore => Prompt::new("Enter name of viewport configuration to restore", Accept::TEXT).kw(&["?"]),
            Stage::Delete => Prompt::new("Enter name(s) of viewport configurations to delete", Accept::TEXT).default("none"),
            Stage::List => Prompt::new("Enter name(s) of viewport configurations to list", Accept::TEXT).default("*"),
            Stage::Dominant => Prompt::new(DOMINANT_PROMPT, Accept::POINT_OR_NUMBER).default("current viewport"),
            Stage::Join(_) => Prompt::new(JOIN_PROMPT, Accept::POINT_OR_NUMBER),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let text = match &i {
            Input::Text(t) | Input::Keyword(t) => Some(t.trim().to_ascii_lowercase()),
            _ => None,
        };
        let bad_kw = || EngineError::Other("Invalid option keyword.".into());
        match self.stage {
            Stage::Main => {
                let t = match (&i, text) {
                    (Input::Enter, _) => "3".to_string(),
                    (_, Some(t)) => t,
                    _ => return Err(bad_kw()),
                };
                match t.as_str() {
                    "2" | "3" | "4" => self.count(s, t.parse().unwrap_or(3)),
                    "si" | "single" => Self::run(s, json!({ "option": "single" })),
                    "s" | "save" => Ok(self.go(Stage::Save)),
                    "r" | "restore" => Ok(self.go(Stage::Restore)),
                    "d" | "delete" => Ok(self.go(Stage::Delete)),
                    "?" => Ok(self.go(Stage::List)),
                    "j" | "join" => {
                        model_only(s)?;
                        Ok(self.go(Stage::Dominant))
                    }
                    _ => Err(bad_kw()),
                }
            }
            Stage::Arrange(n) => {
                let arrangement = match (&i, text) {
                    (Input::Enter, _) => if n == 2 { "vertical" } else { "right" }.to_string(),
                    (_, Some(t)) => t,
                    _ => return Err(bad_kw()),
                };
                Self::run(s, json!({ "option": n.to_string(), "arrangement": arrangement }))
            }
            Stage::Save | Stage::Restore => {
                let Some(name) = text.filter(|t| !t.is_empty()) else { return Ok(Step::Done) };
                if name == "?" {
                    let r = run_vports(s, &json!({ "option": "list" }))?;
                    if let Some(m) = r.get("message").and_then(Value::as_str) {
                        m.lines().for_each(|l| s.echo(l.to_string()));
                    }
                    return Ok(Step::Continue);
                }
                // The name as typed (keywords and text arrive in lower case above).
                let typed = match &i {
                    Input::Text(t) | Input::Keyword(t) => t.trim().to_string(),
                    _ => name,
                };
                let option = if self.stage == Stage::Save { "save" } else { "restore" };
                Self::run(s, json!({ "option": option, "name": typed }))
            }
            Stage::Delete => match &i {
                Input::Text(t) if !t.trim().is_empty() => Self::run(s, json!({ "option": "delete", "names": t.trim() })),
                _ => Ok(Step::Done),
            },
            Stage::List => {
                let names = match &i {
                    Input::Text(t) if !t.trim().is_empty() => t.trim().to_string(),
                    _ => "*".into(),
                };
                Self::run(s, json!({ "option": "list", "names": names }))
            }
            Stage::Dominant => {
                let d = self.tile(s, &i)?;
                self.stage = Stage::Join(d);
                Ok(Step::Continue)
            }
            Stage::Join(d) => {
                let other = self.tile(s, &i)?.ok_or_else(|| EngineError::Other("Select a viewport.".into()))?;
                let mut p = json!({ "option": "join", "join": other + 1 });
                if let (Some(d), Some(o)) = (d, p.as_object_mut()) {
                    o.insert("dominant".into(), json!(d + 1));
                }
                Self::run(s, p)
            }
        }
    }
}

impl VportsM {
    fn go(&mut self, stage: Stage) -> Step {
        self.stage = stage;
        Step::Continue
    }
    /// A tile answer: a number from 1, a point (in the current tile) or Enter (the current tile).
    fn tile(&self, s: &Session, i: &Input) -> Result<Option<usize>> {
        let (_, current) = live(s.state()?, s.viewport_px);
        match i {
            Input::Enter | Input::Point(_) => Ok(Some(current)),
            Input::Text(t) => {
                let n = active_config(s.doc()?).map_or(1, |c| c.tiles.len());
                tile_param(&json!({ "tile": t.trim() }), "tile", n)
            }
            _ => Err(EngineError::Other("Requires a viewport.".into())),
        }
    }
}
