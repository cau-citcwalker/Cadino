//! RENDER (RR): the current view of the Model tab as a shaded picture, perspective or parallel,
//! as the view shows it ([`cadcraft_render::shade`]): the solids in their part materials'
//! colours (else their own) under a key light, a fill light and ambient light, with their
//! edges if asked. Not photoreal: no shadows, reflections or textures. The JSON form writes a
//! PNG (`path`, a file command) or hands it back as base64 (`data`); typed, RENDER and RR open
//! the Render window in the UI, which renders a [`RenderJob`] on a worker thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cadcraft_doc::Drawing;
use cadcraft_geom::ViewDir;
use cadcraft_render::shade::{MAX_SIDE, RenderOptions};
use serde_json::{Value, json};

use super::*;
use crate::{EngineError, Result, Session};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("render", "Render", run_render)
            .alias(&["rr"])
            .params(
                "{width?, height? (pixels, 16 to 4096; default 1280 wide in the view's shape), edges?: bool, path?: PNG file to write, data?: bool (the PNG as base64)} → {width, height, perspective, path?, data?}: the current Model view shaded with the parts' materials (not photoreal)",
            )
            .noundo(),
    ]
}

/// A picture size: `width` and `height` as given, else the side given and the other in the
/// view's shape `aspect`, else 1280 wide.
fn size(p: &Value, aspect: f64) -> Result<(u32, u32)> {
    let side = |k: &str| -> Result<Option<u32>> {
        match p.get(k).filter(|v| !v.is_null()) {
            None => Ok(None),
            Some(v) => v
                .as_f64()
                .filter(|f| f.is_finite() && (16.0..=f64::from(MAX_SIDE)).contains(f))
                .map(|f| Some(f.round() as u32))
                .ok_or_else(|| bad("render", format!("`{k}` is a whole number of pixels from 16 to {MAX_SIDE}"))),
        }
    };
    let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 1.0 };
    let fit = |v: f64| v.round().clamp(16.0, f64::from(MAX_SIDE)) as u32;
    Ok(match (side("width")?, side("height")?) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, fit(f64::from(w) / aspect)),
        (None, Some(h)) => (fit(f64::from(h) * aspect), h),
        (None, None) => (1280, fit(1280.0 / aspect)),
    })
}

/// RENDER's picture of the current Model view, ready to render anywhere: a snapshot of the drawing
/// and the view, so a worker thread can render it while the session goes on (it never sees the
/// session).
#[derive(Clone)]
pub struct RenderJob {
    doc: Arc<Drawing>,
    dir: ViewDir,
    view: crate::View,
    pub options: RenderOptions,
    /// Set (from any thread) to stop the render: see [`RenderJob::cancel`].
    stop: Arc<AtomicBool>,
}

impl RenderJob {
    /// The job RENDER with parameters `p` (`width`, `height`, `edges`) would render now.
    pub fn new(s: &Session, p: &Value) -> Result<RenderJob> {
        super::view3d::model_only(s)?;
        let (w, h) = s.viewport_px;
        let st = s.state()?;
        let (width, height) = size(p, w / h.max(1.0))?;
        let options = RenderOptions { width, height, edges: bool_or(p, "edges", false) };
        Ok(RenderJob { doc: st.doc.clone(), dir: st.screen_dir(), view: st.view(), options, stop: Arc::default() })
    }
    /// Stop the render, from any thread: [`RenderJob::png`] (this job's or a clone's) gives up
    /// soon after.
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
    /// The view is a perspective one.
    pub fn perspective(&self) -> bool {
        self.dir.persp.is_some()
    }
    /// Render the picture: its PNG.
    pub fn png(&self) -> Result<Vec<u8>> {
        let pm = cadcraft_render::shade::render_cancellable(
            &self.doc,
            self.dir,
            self.view.center,
            self.view.height,
            &self.options,
            Some(self.stop.clone()),
        );
        let pm = pm.ok_or_else(|| {
            let cancelled = self.stop.load(Ordering::Relaxed);
            EngineError::Other(if cancelled { "The render was cancelled." } else { "The view can't be rendered at that size." }.into())
        })?;
        pm.encode_png().map_err(|e| EngineError::Other(format!("render: {e}")))
    }
}

fn run_render(s: &mut Session, p: &Value) -> Result<Value> {
    super::view3d::model_only(s)?;
    let path = str_param(p, "path").map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
    if path.is_some() {
        crate::limits::files("render")?;
    }
    let job = RenderJob::new(s, p)?;
    let png = job.png()?;
    let RenderOptions { width, height, .. } = job.options;
    let mut out = json!({ "width": width, "height": height, "perspective": job.perspective() });
    if let Some(path) = &path {
        write_png(path, &png)?;
        out["path"] = json!(path);
    }
    if bool_or(p, "data", false) {
        out["data"] = json!(super::file::base64_encode(&png));
    }
    Ok(out)
}

/// Write `png` to `path` through a temporary file, so a failed write keeps the old file.
fn write_png(path: &str, png: &[u8]) -> Result<()> {
    crate::limits::files("render")?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let tmp = format!("{path}.cadcraft-tmp");
        std::fs::write(&tmp, png).map_err(|e| bad("render", format!("{path}: {e}")))?;
        std::fs::rename(&tmp, path).map_err(|e| bad("render", format!("{path}: {e}")))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = png;
        Err(bad("render", format!("{path}: files can't be written in the browser; ask for `data` instead")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_doc::{EntityKind, Line, Space};
    use cadcraft_geom::Vec3;

    #[test]
    fn render_sizes_follow_the_view_and_bad_ones_are_refused() {
        assert_eq!(size(&json!({}), 2.0).unwrap(), (1280, 640));
        assert_eq!(size(&json!({ "width": 400 }), 2.0).unwrap(), (400, 200));
        assert_eq!(size(&json!({ "height": 100, "width": 300 }), 2.0).unwrap(), (300, 100));
        for bad in [json!({ "width": 0 }), json!({ "height": 1e9 }), json!({ "width": "x" }), json!({ "width": -16 })] {
            assert!(size(&bad, 1.0).is_err(), "{bad}");
        }
    }

    /// A job renders on another thread what RENDER renders, from its snapshot, while the session
    /// changes.
    #[test]
    fn a_render_job_renders_off_the_session_thread() {
        let mut s = Session::new();
        s.viewport_px = (400.0, 300.0);
        let line = EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(100.0, 100.0, 100.0) });
        s.doc_mut().unwrap().add(&Space::Model, Default::default(), line).unwrap();
        s.execute("view.swiso", &json!({})).unwrap();
        let job = RenderJob::new(&s, &json!({ "width": 40, "height": 30, "edges": true })).unwrap();
        let worker = std::thread::spawn(move || job.png());
        s.execute("erase", &json!({ "all": true })).ok();
        let img = image::load_from_memory(&worker.join().unwrap().unwrap()).unwrap();
        assert_eq!((img.width(), img.height()), (40, 30));
        assert!(RenderJob::new(&s, &json!({ "width": 3 })).is_err(), "sizes are checked when the job is made");
        // A cancelled job (its clone shares the flag) renders nothing and says so.
        let job = RenderJob::new(&s, &json!({ "width": 40, "height": 30 })).unwrap();
        job.clone().cancel();
        assert!(job.png().is_err_and(|e| e.to_string().contains("cancelled")));
    }

    #[test]
    fn render_writes_a_png_unless_files_are_denied() {
        let mut s = Session::new();
        s.viewport_px = (400.0, 300.0);
        let line = EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(100.0, 100.0, 100.0) });
        s.doc_mut().unwrap().add(&Space::Model, Default::default(), line).unwrap();
        s.execute("view.swiso", &json!({})).unwrap();
        let r = s.execute("render", &json!({ "width": 64, "height": 48, "data": true })).unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64(), r["perspective"].as_bool()), (Some(64), Some(48), Some(false)));
        assert!(r["data"].as_str().is_some_and(|d| d.len() > 100));
        let dir = std::env::temp_dir().join(format!("cadino-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("view.png");
        let p = path.to_string_lossy().to_string();
        {
            let _deny = crate::limits::deny_files("files are off");
            assert!(s.execute("render", &json!({ "path": p })).is_err());
            assert!(!path.exists());
        }
        s.execute("render", &json!({ "path": p, "width": 64 })).unwrap();
        assert!(std::fs::read(&path).unwrap().starts_with(b"\x89PNG"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_cuts_an_xclipped_references_solids_at_its_boundary() {
        use cadcraft_doc::xclip::XClip;
        use cadcraft_doc::{Block, Common, Insert, Solid3d};
        use cadcraft_geom::{Mesh3, Vec2};
        // A 10 x 10 x 10 box in a block, clipped to its left 4 units.
        let v = |i: u32| Vec3::new(10.0 * f64::from(i & 1), 10.0 * f64::from((i >> 1) & 1), 10.0 * f64::from((i >> 2) & 1));
        let corners =
            [[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
        let mesh = Mesh3 {
            verts: corners.iter().flatten().map(|&i| v(i)).collect(),
            tris: (0..12).map(|t| [3 * t, 3 * t + 1, 3 * t + 2]).collect(),
            edges: vec![],
            open: false,
        };
        let solid = Solid3d { brep: std::sync::Arc::from(""), mesh: std::sync::Arc::new(mesh), ..Default::default() };
        let mut s = Session::new();
        s.viewport_px = (100.0, 100.0);
        let d = s.doc_mut().unwrap();
        let mut blk = Block::new("BOX");
        blk.entities.push(cadcraft_doc::Entity { handle: d.new_handle(), common: Common::default(), kind: EntityKind::Solid3d(solid) });
        d.blocks.insert("BOX".into(), std::sync::Arc::new(blk));
        let mut common = Common::default();
        XClip::put(&mut common, Some(&XClip { boundary: vec![Vec2::new(-1.0, -1.0), Vec2::new(4.0, 11.0)], on: true, front: None, back: None }));
        let ins = Insert {
            block: "BOX".into(),
            insert: Vec3::ZERO,
            scale: Vec3::new(1.0, 1.0, 1.0),
            rotation: 0.0,
            attribs: vec![],
            cols: 1,
            rows: 1,
            col_spacing: 0.0,
            row_spacing: 0.0,
        };
        d.add(&Space::Model, common, EntityKind::Insert(ins)).unwrap();
        s.execute("zoom", &json!({ "mode": "center", "center": [5, 5], "height": 20 })).unwrap();
        let r = s.execute("render", &json!({ "width": 100, "height": 100, "data": true })).unwrap();
        let png = super::super::file::base64_decode(r["data"].as_str().unwrap()).unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgb8();
        // Seen from above, 5 pixels a unit: x = 2 is the box's top, x = 8 was cut away.
        let at = |x: f64| *img.get_pixel((50.0 + (x - 5.0) * 5.0) as u32, 50);
        // The background (shaded top to bottom) on the same row, left of the box.
        let background = *img.get_pixel(5, 50);
        assert_ne!(at(2.0), background, "the part inside the boundary renders");
        assert_eq!(at(8.0), background, "the part outside it doesn't");
    }
}
