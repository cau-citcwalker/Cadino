//! RENDER (RR): the current view of the Model tab as a shaded picture, perspective or parallel,
//! as the view shows it ([`cadcraft_render::shade`]): the solids in their part materials'
//! colours (else their own) under a key light, a fill light and ambient light, with their
//! edges if asked. Not photoreal: no shadows, reflections or textures. The JSON form writes a
//! PNG (`path`, a file command) or hands it back as base64 (`data`); typed, RENDER and RR open
//! the Render window in the UI.

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

fn run_render(s: &mut Session, p: &Value) -> Result<Value> {
    super::view3d::model_only(s)?;
    let path = str_param(p, "path").map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
    if path.is_some() {
        crate::limits::files("render")?;
    }
    let (w, h) = s.viewport_px;
    let st = s.state()?;
    let (view, dir) = (st.view(), st.screen_dir());
    let (width, height) = size(p, w / h.max(1.0))?;
    let o = RenderOptions { width, height, edges: bool_or(p, "edges", false) };
    let pm = cadcraft_render::shade::render(&st.doc, dir, view.center, view.height, &o)
        .ok_or_else(|| EngineError::Other("The view can't be rendered at that size.".into()))?;
    let png = pm.encode_png().map_err(|e| EngineError::Other(format!("render: {e}")))?;
    let mut out = json!({ "width": width, "height": height, "perspective": dir.persp.is_some() });
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
}
