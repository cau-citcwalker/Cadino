//! Options ▸ Display Detail: how the drawing area simplifies what it draws as the view zooms out.
//!
//! Each kind of object has a switch "Simplify when zoomed out" and, when on, the on-screen size
//! from which it simplifies; off, it is drawn at a fixed detail. The defaults are what the drawing
//! area always did. The OPTIONS JSON form reads and sets them as `displayDetail`:
//!
//! ```json
//! {"displayDetail": {"grid": {"simplify": true, "pixels": 8},
//!                    "curves": {"simplify": true, "pixels": 0.5}, "curveTolerance": 0.01,
//!                    "linetypes": {"simplify": true, "pixels": 2},
//!                    "hatches": {"simplify": true, "pixels": 1},
//!                    "text": {"simplify": true, "pixels": 2}}}
//! ```
//!
//! A call may give only some of them (`{"displayDetail": {"text": {"simplify": false}}}`).
//!
//! | kind | simplified (on) | fixed (off) |
//! |---|---|---|
//! | `grid` | lines 5× further apart while closer than `pixels` | always GRIDUNIT apart |
//! | `curves` | arcs and splines in chords off by at most `pixels` | never coarser than `curveTolerance` drawing units |
//! | `linetypes` | a dash pattern shorter than `pixels` draws as a continuous line | always dashed |
//! | `hatches` | pattern lines closer than `pixels` run into a solid fill | every pattern line |
//! | `text` | filled text whose letters are under `pixels` high draws as boxes | always the letters |
//!
//! ponytail: solids keep the kernel's mesh at every zoom (it has no zoom-dependent detail), so
//! there is no row for them; add one if solid meshes ever get a level of detail.

use egui::RichText;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::dialog::{heading, hint};
use crate::CadApp;
use crate::theme::Tokens;

/// One kind's level of detail.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lod {
    /// Simplify when zoomed out.
    pub simplify: bool,
    /// From what on-screen size (pixels).
    pub pixels: f64,
}

/// The drawing area's level of detail (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DisplayDetail {
    pub grid: Lod,
    pub curves: Lod,
    /// The chord tolerance (drawing units) curves never exceed when they aren't simplified.
    pub curve_tolerance: f64,
    pub linetypes: Lod,
    pub hatches: Lod,
    pub text: Lod,
}

impl Default for DisplayDetail {
    fn default() -> Self {
        let on = |pixels| Lod { simplify: true, pixels };
        DisplayDetail { grid: on(8.0), curves: on(0.5), curve_tolerance: 0.01, linetypes: on(2.0), hatches: on(1.0), text: on(2.0) }
    }
}

/// The kinds: JSON name and the `pixels` range.
const KINDS: [(&str, std::ops::RangeInclusive<f64>); 5] =
    [("grid", 1.0..=100.0), ("curves", 0.05..=10.0), ("linetypes", 0.5..=50.0), ("hatches", 0.25..=20.0), ("text", 0.5..=100.0)];

/// A kind's label and what its setting means when on and when off.
fn texts(kind: &str) -> (&'static str, &'static str, &'static str) {
    match kind {
        "grid" => (
            crate::tl!("Grid"),
            crate::tl!("Coarsen the grid while its lines are closer than"),
            crate::tl!("The grid keeps its own spacing until its lines would run together."),
        ),
        "curves" => {
            (crate::tl!("Curves"), crate::tl!("Chords may stray from arcs and curves by up to"), crate::tl!("Chords never stray by more than"))
        }
        "linetypes" => {
            (crate::tl!("Linetypes"), crate::tl!("Dash patterns shorter than this draw as solid lines:"), crate::tl!("Dashes are always drawn."))
        }
        "hatches" => {
            (crate::tl!("Hatches"), crate::tl!("Hatch lines closer than this merge into a solid fill:"), crate::tl!("Every hatch line is drawn."))
        }
        _ => (crate::tl!("Text"), crate::tl!("Text smaller than this draws as boxes:"), crate::tl!("Letters are always drawn.")),
    }
}

/// Fixed chord tolerances `curveTolerance` takes (drawing units).
const CURVE_TOLERANCE: std::ops::RangeInclusive<f64> = 0.001..=1000.0;

impl DisplayDetail {
    fn lod(&self, kind: &str) -> Option<Lod> {
        Some(match kind {
            "grid" => self.grid,
            "curves" => self.curves,
            "linetypes" => self.linetypes,
            "hatches" => self.hatches,
            "text" => self.text,
            _ => return None,
        })
    }

    /// Identifies the detail display lists are built with (part of their keys: a change builds
    /// them again in the background, as a new view does, while the old ones stay on screen).
    pub fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for l in [self.grid, self.curves, self.linetypes, self.hatches, self.text] {
            (l.simplify, l.pixels.to_bits()).hash(&mut h);
        }
        self.curve_tolerance.to_bits().hash(&mut h);
        h.finish()
    }

    /// The closest grid lines may come on screen (pixels) before the grid is coarsened; 0: never.
    pub fn grid_min_px(&self) -> f64 {
        if self.grid.simplify { self.grid.pixels } else { 0.0 }
    }

    /// Set the level of detail of an on-screen build: `px` drawing units per pixel at the zoom it
    /// tessellates for, `finest` at the finest zoom showing it (its detail must hold there).
    ///
    /// Not simplified, a kind still leaves out what no screen shows: chords off by under a
    /// sixteenth of a pixel, dashes, hatch lines and letters under an eighth. Nothing visible
    /// changes, and a fixed detail can't make a list grow without bound as the view zooms out
    /// (20,000 circles of radius 1000 at a 0.001 tolerance would be some 44 million points).
    pub fn apply(&self, o: &mut cadcraft_render::Options, px: f64, finest: f64) {
        let size = |l: Lod| if l.simplify { l.pixels * finest } else { finest / 8.0 };
        let chord = self.curves.pixels * px;
        o.tolerance = if self.curves.simplify { chord } else { self.curve_tolerance.max(px / 16.0).min(chord) };
        o.min_dash = size(self.linetypes);
        o.hatch_pixel = Some(size(self.hatches));
        // The render's text pixel is half the letter height that becomes a box.
        o.text_pixel = Some(size(self.text) / 2.0);
    }

    /// `self` with `changes` (an object of some of its fields, as the JSON form reports them)
    /// applied, if every name and value is valid.
    pub fn with(&self, changes: &Value) -> Result<DisplayDetail, String> {
        let err = || format!("displayDetail takes {}: {{simplify, pixels}}, and curveTolerance", KINDS.map(|k| k.0).join(", "));
        let mut merged = serde_json::to_value(self).map_err(|e| e.to_string())?;
        let (Some(into), Some(from)) = (merged.as_object_mut(), changes.as_object()) else { return Err(err()) };
        for (k, v) in from {
            match (into.get_mut(k), v) {
                (Some(Value::Object(lod)), Value::Object(fields)) => lod.extend(fields.clone()),
                (Some(_), Value::Number(_)) if k == "curveTolerance" => {
                    into.insert(k.clone(), v.clone());
                }
                _ => return Err(err()),
            }
        }
        let d: DisplayDetail = serde_json::from_value(merged).map_err(|_| err())?;
        for (kind, range) in KINDS {
            if !d.lod(kind).is_some_and(|l| range.contains(&l.pixels)) {
                return Err(format!("displayDetail.{kind}.pixels must be from {} to {}", range.start(), range.end()));
            }
        }
        if !CURVE_TOLERANCE.contains(&d.curve_tolerance) {
            return Err(format!("displayDetail.curveTolerance must be from {} to {}", CURVE_TOLERANCE.start(), CURVE_TOLERANCE.end()));
        }
        Ok(d)
    }

    pub fn json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// The Display Detail tab.
pub(super) fn tab(app: &CadApp, ui: &mut egui::Ui, changes: &mut Map<String, Value>) {
    let d = app.options.detail;
    let mut patch = Map::new();
    ui.label(crate::tl!("As the view zooms out, small details are drawn simpler so the drawing stays fast. Choose per kind of object."));
    ui.add_space(6.0);
    super::dialog::grid(ui, "options_detail", |ui| {
        for (kind, range) in KINDS {
            let Some(l) = d.lod(kind) else { continue };
            let (label, on, off) = texts(kind);
            heading(ui, label);
            ui.vertical(|ui| {
                let mut simplify = l.simplify;
                if ui.checkbox(&mut simplify, crate::tl!("Simplify when zoomed out")).changed() {
                    patch.insert(kind.into(), json!({ "simplify": simplify }));
                }
                ui.horizontal(|ui| {
                    if simplify {
                        ui.label(RichText::new(on).color(Tokens::get().text_dim));
                        let mut px = l.pixels;
                        let speed = (range.end() - range.start()) / 500.0;
                        if ui.add(egui::DragValue::new(&mut px).range(range.clone()).speed(speed).max_decimals(2).suffix(" px")).changed() {
                            patch.insert(kind.into(), json!({ "pixels": px }));
                        }
                    } else {
                        ui.label(RichText::new(off).color(Tokens::get().text_dim));
                        if kind == "curves" {
                            let mut tol = d.curve_tolerance;
                            let drag = egui::DragValue::new(&mut tol).range(CURVE_TOLERANCE).speed(0.001).max_decimals(4);
                            if ui.add(drag).changed() {
                                patch.insert("curveTolerance".into(), json!(tol));
                            }
                            ui.label(crate::tl!("drawing units"));
                        }
                    }
                });
            });
            ui.end_row();
        }
    });
    ui.add_space(4.0);
    hint(
        ui,
        crate::tl!(
            "Less simplification looks finer when zoomed out but draws more slowly on large drawings. Detail far below a pixel is never drawn."
        ),
    );
    if ui.button(crate::tl!("Restore Defaults")).clicked() {
        patch = DisplayDetail::default().json().as_object().cloned().unwrap_or_default();
    }
    if !patch.is_empty() {
        changes.insert("displayDetail".into(), Value::Object(patch));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_build_what_the_canvas_always_built() {
        // The canvas's own options before Display Detail: half a pixel chords, two-pixel dashes,
        // hatches and text at one pixel (half of min_dash).
        let mut o = cadcraft_render::Options::default();
        DisplayDetail::default().apply(&mut o, 0.25, 0.125);
        assert_eq!((o.tolerance, o.min_dash, o.hatch_pixel, o.text_pixel), (0.125, 0.25, Some(0.125), Some(0.125)));
        assert_eq!(DisplayDetail::default().grid_min_px(), 8.0);
    }

    #[test]
    fn off_is_fixed_detail_and_partial_changes_merge() {
        let d = DisplayDetail::default()
            .with(&json!({"curves": {"simplify": false}, "curveTolerance": 0.002, "text": {"simplify": false}, "grid": {"simplify": false}, "hatches": {"pixels": 4}}))
            .unwrap();
        assert_eq!((d.curves.pixels, d.linetypes, d.hatches.pixels), (0.5, DisplayDetail::default().linetypes, 4.0));
        let mut o = cadcraft_render::Options::default();
        d.apply(&mut o, 0.01, 0.01);
        // Zoomed out: never coarser than the fixed tolerance; zoomed in, still half a pixel.
        assert_eq!(o.tolerance, 0.002);
        assert_eq!((o.text_pixel, o.hatch_pixel), (Some(0.01 / 16.0), Some(0.04)));
        d.apply(&mut o, 0.001, 0.001);
        assert_eq!(o.tolerance, 0.0005);
        assert_eq!(d.grid_min_px(), 0.0);
        for bad in [
            json!({"grid": {"pixels": 0}}),
            json!({"text": {"pixels": f64::INFINITY}}),
            json!({"text": {"simplify": "no"}}),
            json!({"text": {"size": 3}}),
            json!({"solids": {"simplify": false}}),
            json!({"curveTolerance": 0}),
            json!({"curveTolerance": "fine"}),
            json!({"grid": true}),
            json!([]),
        ] {
            assert!(d.with(&bad).is_err(), "{bad}");
        }
    }

    /// Every kind fixed (not simplified) and zoomed far out: the build still leaves out what is
    /// far below a pixel, so it stays bounded (chords a sixteenth of a pixel, the rest an eighth).
    #[test]
    fn fixed_detail_stops_below_a_pixel() {
        let off = json!({"simplify": false});
        let d =
            DisplayDetail::default().with(&json!({"curves": off, "curveTolerance": 0.001, "linetypes": off, "hatches": off, "text": off})).unwrap();
        let mut o = cadcraft_render::Options::default();
        // 4 drawing units per pixel (a 10 m drawing on 2500 px).
        d.apply(&mut o, 4.0, 4.0);
        assert_eq!((o.tolerance, o.min_dash, o.hatch_pixel, o.text_pixel), (0.25, 0.5, Some(0.5), Some(0.25)));
        // A radius-1000 circle in chords off by 0.25 is some 140 of them, not thousands.
        let segments = std::f64::consts::PI / (1.0 - o.tolerance / 1000.0).acos();
        assert!(segments < 200.0, "{segments}");
    }
}
