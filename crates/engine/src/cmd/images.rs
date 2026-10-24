//! Raster images. IMAGEATTACH (and ATTACH, for an image file) puts a PNG, JPEG or BMP picture in
//! the drawing: an IMAGE object referring to the file, which stays outside the drawing. -IMAGE
//! manages the attached files (`?`, Detach, Path, Reload, Unload, Attach); IMAGE lists them (the
//! app opens the External References palette). Pictures are read when a drawing opens and when an
//! image is attached or reloaded; an image whose file can't be found or read shows its frame and
//! file name. The editing commands (IMAGECLIP, IMAGEADJUST, IMAGEFRAME, IMAGEQUALITY,
//! TRANSPARENCY) are in [`edit`].
//!
//! A file is looked for as saved (a relative path against the drawing's folder), then by its name
//! in the drawing's folder, as xrefs are. Decoding is capped (file size, pixel count, memory) and
//! guarded, so a hostile file fails with a message.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use cadcraft_doc::{Drawing, EntityKind, EntityStore, Handle, Image, Raster};
use cadcraft_geom::{Vec2, Vec3};
use serde::Serialize;
use serde_json::{Value, json};

use super::machines::number;
use super::{CommandSpec, bad, point_param, run_body, str_param};
use crate::{Accept, EngineError, Input, Interactive, Picker, Prompt, Result, Session, Step};

mod edit;

/// The picture files images attach.
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "bmp"];
/// Underlays ATTACH lists but can't attach yet.
const UNDERLAY_EXTS: &[&str] = &["pdf", "dwf", "dwfx", "dgn"];
/// What ATTACH's file dialog lists: pictures, drawings and the underlays it refuses.
const ATTACH_EXTS: &[&str] = &["png", "jpg", "jpeg", "bmp", "dwg", "dxf", "pdf", "dwf", "dwfx", "dgn"];
/// Largest image file read.
#[cfg(not(target_arch = "wasm32"))]
const MAX_FILE_BYTES: u64 = 256 << 20;
/// Most pixels, and the widest side, of a picture decoded.
const MAX_PIXELS: u64 = 120_000_000;
const MAX_SIDE: u32 = 60_000;
/// Most memory the decoder may allocate.
const MAX_ALLOC: u64 = 640 << 20;
/// Pictures are kept at most this many pixels on their long side.
/// ponytail: one level of detail; keep the full picture (tiles) if plots need more than 4096 px.
const KEEP_SIDE: u32 = 4096;
/// Most image files read in one go (a drawing opening).
const MAX_FILES: usize = 1000;
/// Most memory the pictures read in one go keep decoded (a drawing opening, a reload). Past it
/// further images show their frames: a drawing can't run the app out of memory.
/// ponytail: per read, not for the whole app; share one budget if many drawings with large
/// pictures open at once.
const MAX_DECODED_BYTES: usize = if cfg!(target_arch = "wasm32") { 256 << 20 } else { 1536 << 20 };

pub(super) fn specs() -> Vec<CommandSpec> {
    let mut v = vec![
        CommandSpec::new("imageattach", "Raster Image Reference...", run_imageattach)
            .menu(&["Insert", "Raster Image Reference..."])
            .alias(&["iat"])
            .params(ATTACH_PARAMS)
            .interactive(|_| Ok(Box::new(AttachM::new(Kind::Image)))),
        CommandSpec::new("attach", "Attach...", run_attach)
            .menu(&["File", "Attach..."])
            .params("{path: a PNG/JPEG/BMP picture (as IMAGEATTACH) or a DWG/DXF drawing (as XATTACH), …their parameters} → their result (PDF, DWF and DGN underlays are not available yet)")
            .interactive(|_| Ok(Box::new(AttachM::new(Kind::Any)))),
        CommandSpec::new("image", "Image", run_image)
            .alias(&["im"])
            .params("{} → {images: [{name, path, foundAt, status: Loaded|Unloaded|Not Found|Unreadable, references, width, height}]} (the app opens the External References palette instead)")
            .noundo(),
        CommandSpec::new("-image", "Image (command line)", run_dash_image)
            .alias(&["-im"])
            .params(
                "{option?: ?|detach|path|reload|unload|attach (default ?), names?: \"a,b*\" (wildcards) | [names] (exact names) (default all; detach and unload need them), \
                 path? (path: the new path; attach: the file, with IMAGEATTACH's parameters)} → {images | detached | reloaded | unloaded | IMAGEATTACH's result}",
            )
            .interactive(|_| Ok(Box::new(DashImageM::default()))),
    ];
    v.extend(edit::specs());
    v
}

const ATTACH_PARAMS: &str = "{path: PNG, JPEG or BMP file, at?: [x,y] (insertion point, the lower-left corner; default 0,0), scale?: width in drawing units (default 1), \
    rotation?: degrees, name? (default: the file name), pathType?: full|relative|none (default relative while the drawing has a folder)} \
    → {handle, name, path, width, height (pixels), size: [w, h] (drawing units)}; the image goes behind the other objects (DRAWORDER brings it forward)";

fn extension(path: &str) -> String {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    file.rsplit_once('.').map(|(_, e)| e.trim().to_ascii_lowercase()).unwrap_or_default()
}

/// The file name without folders and extension.
fn stem(path: &str) -> String {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match file.rsplit_once('.') {
        Some((s, _)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => file.trim().to_string(),
    }
}

/// An image's definition name: its own, or its file's name.
pub(crate) fn def_name(i: &Image) -> String {
    let n = i.name.trim();
    if n.is_empty() { stem(&i.path) } else { n.to_string() }
}

/// The folder of the drawing saved at `host`.
fn folder(host: Option<&str>) -> Option<PathBuf> {
    let p = std::path::Path::new(host?).parent()?;
    Some(if p.as_os_str().is_empty() { PathBuf::from(".") } else { p.to_path_buf() })
}

// ---------- reading pictures ----------

/// A PNG, JPEG or BMP file's picture (at most [`KEEP_SIDE`] pixels on its long side) and its size
/// in pixels. Hostile files fail with a message: never a panic, never unbounded memory.
pub(crate) fn decode(bytes: &[u8]) -> std::result::Result<(Arc<Raster>, (u32, u32)), String> {
    use image::ImageFormat;
    let format = image::guess_format(bytes).map_err(|_| "not a PNG, JPEG or BMP picture".to_string())?;
    if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Bmp) {
        return Err(format!("{format:?} pictures can't be attached (use PNG, JPEG or BMP)"));
    }
    let reader = || {
        let mut r = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
        let mut l = image::Limits::default();
        l.max_image_width = Some(MAX_SIDE);
        l.max_image_height = Some(MAX_SIDE);
        l.max_alloc = Some(MAX_ALLOC);
        r.limits(l);
        r
    };
    let read = || -> std::result::Result<(Arc<Raster>, (u32, u32)), String> {
        let (w, h) = reader().into_dimensions().map_err(|e| e.to_string())?;
        if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
            return Err(format!("{w} x {h} pixels is more than this app reads ({} million at most)", MAX_PIXELS / 1_000_000));
        }
        let img = reader().decode().map_err(|e| e.to_string())?;
        let alpha = img.color().has_alpha();
        let img = if img.width().max(img.height()) > KEEP_SIDE { img.thumbnail(KEEP_SIDE, KEEP_SIDE) } else { img };
        let rgba = img.into_rgba8();
        let (rw, rh) = rgba.dimensions();
        let r = Raster::new(rw, rh, rgba.into_raw(), alpha).ok_or("the picture is empty")?;
        Ok((r, (w, h)))
    };
    std::panic::catch_unwind(read).unwrap_or_else(|_| Err("the file could not be read".into()))
}

/// What reading an image file gives: the picture, its size in pixels and where it was found.
type Read = (Arc<Raster>, (u32, u32), PathBuf);

/// Read the picture of the image file saved as `path` in the drawing saved at `host`.
fn read(path: &str, host: Option<&str>) -> std::result::Result<Read, String> {
    let file = locate(path, host)?;
    let (r, size) = read_file(&file)?;
    Ok((r, size, file))
}

/// The file an image saved as `path` in the drawing saved at `host` is read from.
fn locate(path: &str, host: Option<&str>) -> std::result::Result<PathBuf, String> {
    crate::limits::files("image").map_err(|e| e.to_string())?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = folder(host);
        super::blocks::xref::load::find(path, dir.as_deref()).ok_or_else(|| "not found".to_string())
    }
    #[cfg(target_arch = "wasm32")]
    {
        // ponytail: no file system on the web; attach from `data` (base64) when that matters.
        let _ = (path, host);
        Err("image files can't be read on the web".into())
    }
}

/// The picture in image file `file` and its size in pixels.
fn read_file(file: &std::path::Path) -> std::result::Result<(Arc<Raster>, (u32, u32)), String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let len = std::fs::metadata(file).map_err(|e| e.to_string())?.len();
        if len > MAX_FILE_BYTES {
            return Err(format!("the file is larger than {} MB", MAX_FILE_BYTES >> 20));
        }
        let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
        decode(&bytes)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (file, decode);
        Err("image files can't be read on the web".into())
    }
}

/// Whether block `b` is the drawing's own: not an xref nor one an xref brought (`XREF|name`).
/// Their images belong to the referenced drawings: they are read from the xref's own folder when
/// it loads (`xref::load`), and -IMAGE doesn't manage them.
fn own_block(b: &cadcraft_doc::Block) -> bool {
    b.xref_path.is_none() && !b.name.contains('|')
}

/// Change the images of `d` (model space, layouts and its own blocks) that `pick` chooses with
/// `f`; returns how many there were.
fn change_images(d: &mut Drawing, pick: &dyn Fn(&Image) -> bool, f: &dyn Fn(&mut Image)) -> usize {
    let chosen = |e: &cadcraft_doc::Entity| matches!(&e.kind, EntityKind::Image(i) if pick(i));
    let change = |st: &mut EntityStore| {
        let hs: Vec<Handle> = st.iter().filter(|e| chosen(e)).map(|e| e.handle).collect();
        for h in &hs {
            st.modify(*h, |e| {
                if let EntityKind::Image(i) = &mut e.kind {
                    f(i);
                }
            });
        }
        hs.len()
    };
    let mut n = change(&mut d.model);
    for l in &mut d.layouts {
        n += change(&mut l.entities);
    }
    for b in d.blocks.values_mut() {
        if own_block(b) && b.entities.iter().any(|e| chosen(e)) {
            n += change(&mut Arc::make_mut(b).entities);
        }
    }
    n
}

/// Every image of `d` (model space, layouts and its own blocks: [`own_block`]).
fn all_images(d: &Drawing) -> impl Iterator<Item = &Image> {
    fn of(st: &EntityStore) -> impl Iterator<Item = &Image> {
        st.iter().filter_map(|e| match &e.kind {
            EntityKind::Image(i) => Some(i),
            _ => None,
        })
    }
    of(&d.model).chain(d.layouts.iter().flat_map(|l| of(&l.entities))).chain(d.blocks.values().filter(|b| own_block(b)).flat_map(|b| of(&b.entities)))
}

/// Read the pictures of the images in `d` that have none yet and aren't unloaded; with `names`,
/// of the images with those definition names (reading their files again and loading unloaded
/// ones). Returns what couldn't be read, as messages.
pub(crate) fn load_pictures(d: &mut Drawing, host: Option<&str>, names: Option<&[String]>) -> Vec<String> {
    load_within(d, host, names, MAX_DECODED_BYTES)
}

/// [`load_pictures`] keeping at most about `budget` bytes of decoded pictures.
fn load_within(d: &mut Drawing, host: Option<&str>, names: Option<&[String]>, budget: usize) -> Vec<String> {
    let wanted = |i: &Image| match names {
        Some(ns) => ns.iter().any(|n| n.eq_ignore_ascii_case(&def_name(i))),
        None => i.pixels.is_none() && !i.unloaded,
    };
    let mut paths: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for i in all_images(d).filter(|i| wanted(i)) {
        if seen.insert(i.path.clone()) {
            paths.push(i.path.clone());
        }
    }
    let mut got: HashMap<String, Arc<Raster>> = HashMap::new();
    // One file under any spelling is decoded once, and the decoded pictures have a budget.
    let mut by_file: HashMap<PathBuf, Arc<Raster>> = HashMap::new();
    let (mut spent, mut over) = (0usize, 0usize);
    let mut notes = Vec::new();
    for p in paths.iter().take(MAX_FILES) {
        let file = match locate(p, host) {
            Ok(f) => f,
            Err(e) => {
                notes.push(format!("Image \"{p}\": {e}."));
                continue;
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        let key = std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone());
        #[cfg(target_arch = "wasm32")]
        let key = file.clone();
        if let Some(r) = by_file.get(&key) {
            got.insert(p.clone(), r.clone());
            continue;
        }
        if spent >= budget {
            over += 1;
            continue;
        }
        match read_file(&file) {
            Ok((r, _)) => {
                spent = spent.saturating_add(r.rgba.len());
                by_file.insert(key, r.clone());
                got.insert(p.clone(), r);
            }
            Err(e) => notes.push(format!("Image \"{p}\": {e}.")),
        }
    }
    if over > 0 {
        notes.push(format!("{over} image file(s) not read: the pictures read use all the memory set aside for them ({} MB).", budget >> 20));
    }
    if paths.len() > MAX_FILES {
        notes.push(format!("Only the first {MAX_FILES} image files were read."));
    }
    let reload = names.is_some();
    change_images(d, &wanted, &|i| {
        i.pixels = got.get(&i.path).cloned();
        if reload {
            i.unloaded = false;
        }
    });
    notes
}

/// After a drawing opens: read its images' pictures. Reading them doesn't count as a change.
pub(crate) fn after_open(s: &mut Session, i: usize) {
    let Some(st) = s.docs.get_mut(i) else { return };
    if all_images(&st.doc).next().is_none() {
        return;
    }
    let host = st.path.clone();
    let clean = Arc::ptr_eq(&st.doc, &st.saved);
    let notes = load_pictures(Arc::make_mut(&mut st.doc), host.as_deref(), None);
    if clean {
        st.saved = st.doc.clone();
    }
    for n in notes {
        s.echo(n);
    }
}

// ---------- listing ----------

/// One image definition (the images sharing a name), as -IMAGE `?` and the External References
/// palette list it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageDef {
    pub name: String,
    /// The path saved in the drawing.
    pub path: String,
    /// The file it is read from, when there is one.
    pub found_at: Option<String>,
    /// Loaded, Unloaded, Not Found or Unreadable.
    pub status: &'static str,
    pub references: usize,
    /// Size in pixels.
    pub width: u32,
    pub height: u32,
}

/// The image definitions of `d` (saved at `host`), by name.
pub fn image_defs(d: &Drawing, host: Option<&str>) -> Vec<ImageDef> {
    let mut defs: Vec<(ImageDef, bool, bool)> = Vec::new();
    for i in all_images(d) {
        let name = def_name(i);
        let at = match defs.iter().position(|(x, _, _)| x.name.eq_ignore_ascii_case(&name)) {
            Some(at) => at,
            None => {
                let (w, h) = (i.size.x.max(0.0).min(f64::from(u32::MAX)) as u32, i.size.y.max(0.0).min(f64::from(u32::MAX)) as u32);
                let def = ImageDef { name, path: i.path.clone(), found_at: None, status: "", references: 0, width: w, height: h };
                defs.push((def, false, false));
                defs.len() - 1
            }
        };
        if let Some((def, loaded, unloaded)) = defs.get_mut(at) {
            def.references += 1;
            *loaded |= i.pixels.is_some();
            *unloaded |= i.unloaded;
        }
    }
    let dir = folder(host);
    defs.into_iter()
        .map(|(mut def, loaded, unloaded)| {
            def.found_at = super::blocks::xref::load::find(&def.path, dir.as_deref()).map(|f| f.to_string_lossy().into_owned());
            def.status = match (unloaded, loaded, def.found_at.is_some()) {
                (true, _, _) => "Unloaded",
                (_, true, _) => "Loaded",
                (_, _, true) => "Unreadable",
                _ => "Not Found",
            };
            def
        })
        .collect()
}

fn host(s: &Session) -> Option<String> {
    s.state().ok().and_then(|st| st.path.clone())
}

fn run_image(s: &mut Session, _p: &Value) -> Result<Value> {
    let defs = image_defs(s.doc()?, host(s).as_deref());
    Ok(json!({ "images": defs }))
}

// ---------- attaching ----------

/// How an attached image's path is saved (REFPATHTYPE).
fn saved_path(typed: &str, file: &std::path::Path, host: Option<&str>, kind: Option<&str>) -> std::result::Result<String, String> {
    let abs = std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf());
    let dir = folder(host).map(|d| std::path::absolute(&d).unwrap_or(d));
    let kind = kind.map(|k| k.trim().to_ascii_lowercase());
    match kind.as_deref() {
        Some("full" | "f" | "absolute") => Ok(abs.to_string_lossy().into_owned()),
        Some("none" | "n" | "no path") => Ok(file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| typed.to_string())),
        None | Some("relative" | "r") => {
            // A file in the drawing's folder (or below it) is saved relative to it.
            match dir.as_deref().and_then(|d| abs.strip_prefix(d).ok()) {
                Some(rel) if !rel.as_os_str().is_empty() => Ok(rel.to_string_lossy().into_owned()),
                _ => Ok(abs.to_string_lossy().into_owned()),
            }
        }
        Some(k) => Err(format!("`pathType` must be full, relative or none, not \"{}\"", k.chars().take(20).collect::<String>())),
    }
}

/// A definition name for the file saved as `path`: the name of an image of the same file, else
/// `wanted` (or the file name) made unique.
fn image_name(d: &Drawing, path: &str, wanted: Option<&str>) -> String {
    let images: Vec<&Image> = all_images(d).collect();
    if wanted.is_none()
        && let Some(i) = images.iter().find(|i| i.path == path)
    {
        return def_name(i);
    }
    let base = wanted.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| stem(path));
    let base = if base.is_empty() { "Image".to_string() } else { base };
    let taken = |n: &str| images.iter().any(|i| def_name(i).eq_ignore_ascii_case(n) && i.path != path);
    let mut name = base.clone();
    let mut k = 2usize;
    while taken(&name) && k < 100_000 {
        name = format!("{base}_{k}");
        k += 1;
    }
    name
}

/// Where and how big an image goes.
#[derive(Clone, Copy, Debug)]
struct Placing {
    at: Vec2,
    /// Width in drawing units.
    scale: f64,
    /// Radians.
    rotation: f64,
}

/// Add the picture `r` read for `typed` as an image placed by `pl`.
fn place(s: &mut Session, cmd: &str, typed: &str, r: Read, pl: Placing, name: Option<&str>, path_type: Option<&str>) -> Result<Value> {
    let (pixels, (w, h), file) = r;
    let host = host(s);
    let path = saved_path(typed, &file, host.as_deref(), path_type).map_err(|e| bad(cmd, e))?;
    let name = image_name(s.doc()?, &path, name);
    let px = pl.scale / f64::from(w.max(1));
    let (sn, c) = pl.rotation.sin_cos();
    let img = Image {
        insert: pl.at.to3(0.0),
        u: Vec3::new(c * px, sn * px, 0.0),
        v: Vec3::new(-sn * px, c * px, 0.0),
        size: Vec2::new(f64::from(w), f64::from(h)),
        path: path.clone(),
        name: name.clone(),
        pixels: Some(pixels),
        ..Image::default()
    };
    let handle = s.add_entity(EntityKind::Image(img))?;
    // Behind the drawing, as the canvas shows pictures (see `canvas/images.rs`), so plots and
    // exports look the same; DRAWORDER brings it forward in them.
    run_body(s, "draworder", &json!({ "handles": [handle.hex()], "order": "back" }))?;
    let size = [pl.scale, pl.scale * f64::from(h) / f64::from(w.max(1))];
    Ok(json!({ "handle": handle.hex(), "name": name, "path": path, "width": w, "height": h, "size": size }))
}

/// The placement a JSON call asks for.
fn placing(cmd: &str, p: &Value) -> Result<Placing> {
    let at = point_param(p, "at").unwrap_or(Vec2::ZERO);
    let scale = match p.get("scale") {
        None | Some(Value::Null) => 1.0,
        Some(v) => v.as_f64().filter(|x| x.is_finite() && *x > 0.0 && *x < 1e12).ok_or_else(|| bad(cmd, "`scale` must be a positive number"))?,
    };
    let rotation = match p.get("rotation") {
        None | Some(Value::Null) => 0.0,
        Some(v) => {
            v.as_f64().filter(|x| x.is_finite() && x.abs() < 1e9).ok_or_else(|| bad(cmd, "`rotation` must be a number (degrees)"))?.to_radians()
        }
    };
    Ok(Placing { at, scale, rotation })
}

fn attach_image(s: &mut Session, cmd: &str, p: &Value) -> Result<Value> {
    let path =
        str_param(p, "path").map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(cmd, "`path` (a PNG, JPEG or BMP file) is required"))?;
    let pl = placing(cmd, p)?;
    let r = read(path, host(s).as_deref()).map_err(|e| bad(cmd, format!("{path}: {e}")))?;
    place(s, cmd, path, r, pl, str_param(p, "name"), str_param(p, "pathType"))
}

fn run_imageattach(s: &mut Session, p: &Value) -> Result<Value> {
    attach_image(s, "imageattach", p)
}

fn underlay_refused(ext: &str) -> EngineError {
    EngineError::Other(format!(
        "ATTACH: {} underlays are not available yet; attach a PNG, JPEG or BMP picture or a DWG/DXF drawing",
        ext.to_ascii_uppercase()
    ))
}

fn run_attach(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_param(p, "path").map(str::trim).unwrap_or("");
    let ext = extension(path);
    match ext.as_str() {
        _ if path.is_empty() => Err(bad("attach", "`path` is required")),
        "dwg" | "dxf" => run_body(s, "xattach", p),
        e if UNDERLAY_EXTS.contains(&e) => Err(underlay_refused(e)),
        _ => attach_image(s, "attach", p),
    }
}

/// What ATTACH typed at the command line takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// IMAGEATTACH and -IMAGE Attach: pictures.
    Image,
    /// ATTACH: pictures and drawings.
    Any,
}

enum AStage {
    File,
    Insert,
    Scale,
    Rotate,
}

/// IMAGEATTACH and ATTACH typed (the file dialog with FILEDIA 1), and -IMAGE Attach: the file,
/// then the insertion point, scale and rotation, as the reference app asks once its Attach dialog
/// is closed with "Specify on-screen".
/// ponytail: no Attach Image dialog; its choices (path type, name) are JSON parameters.
struct AttachM {
    kind: Kind,
    /// -IMAGE Attach: the name is typed, no file dialog.
    typed: bool,
    stage: AStage,
    file: String,
    /// The picture read (`None`: a drawing, attached as an xref).
    read: Option<Read>,
    at: Vec2,
    scale: f64,
}

impl AttachM {
    fn new(kind: Kind) -> Self {
        AttachM { kind, typed: false, stage: AStage::File, file: String::new(), read: None, at: Vec2::ZERO, scale: 1.0 }
    }
    /// The image being placed at `pl`, for the preview.
    fn ghost(&self, pl: Placing) -> Option<EntityKind> {
        let (pixels, (w, h), _) = self.read.as_ref()?;
        let px = pl.scale / f64::from((*w).max(1));
        let (sn, c) = pl.rotation.sin_cos();
        Some(EntityKind::Image(Image {
            insert: pl.at.to3(0.0),
            u: Vec3::new(c * px, sn * px, 0.0),
            v: Vec3::new(-sn * px, c * px, 0.0),
            size: Vec2::new(f64::from(*w), f64::from(*h)),
            pixels: Some(pixels.clone()),
            ..Image::default()
        }))
    }
    fn finish(&mut self, s: &mut Session, rotation: f64) -> Result<Step> {
        let pl = Placing { at: self.at, scale: self.scale, rotation };
        let cmd = if self.kind == Kind::Any { "attach" } else { "imageattach" };
        match self.read.take() {
            Some(r) => {
                place(s, cmd, &self.file.clone(), r, pl, None, None)?;
            }
            None => {
                let p = json!({ "path": self.file, "at": [pl.at.x, pl.at.y], "scale": pl.scale, "rotation": rotation.to_degrees() });
                run_body(s, "xattach", &p)?;
            }
        }
        Ok(Step::Done)
    }
}

/// Typed text without surrounding quotes; `None` for Enter.
fn text(i: &Input) -> Option<String> {
    match i {
        Input::Text(t) | Input::Keyword(t) => Some(t.trim().trim_matches('"').trim().to_string()).filter(|t| !t.is_empty()),
        _ => None,
    }
}

impl Interactive for AttachM {
    fn name(&self) -> &'static str {
        match self.kind {
            Kind::Any => "ATTACH",
            Kind::Image => "IMAGEATTACH",
        }
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.stage {
            AStage::File => {
                let (msg, label, exts) = match self.kind {
                    Kind::Image => ("Enter image file name to attach", "Image files (PNG, JPEG, BMP)", IMAGE_EXTS),
                    Kind::Any => ("Enter name of file to attach", "Reference files (images, DWG, DXF)", ATTACH_EXTS),
                };
                let p = Prompt::new(msg, Accept::TEXT);
                if self.typed { p } else { p.picker(Picker::File { save: false, label, exts }) }
            }
            AStage::Insert => Prompt::new("Specify insertion point", Accept::POINT).default("0,0"),
            AStage::Scale => Prompt::new("Specify scale factor", Accept::POINT_OR_NUMBER).base(self.at).default("1"),
            AStage::Rotate => Prompt::new("Specify rotation angle", Accept::POINT_OR_NUMBER).base(self.at).default("0"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match self.stage {
            AStage::File => {
                let Some(file) = text(&i) else { return Ok(Step::Done) };
                let ext = extension(&file);
                if UNDERLAY_EXTS.contains(&ext.as_str()) {
                    return Err(underlay_refused(&ext));
                }
                if self.kind == Kind::Any && matches!(ext.as_str(), "dwg" | "dxf") {
                    self.read = None;
                } else {
                    let r = read(&file, host(s).as_deref()).map_err(|e| EngineError::Other(format!("{file}: {e}")))?;
                    self.read = Some(r);
                }
                self.file = file;
                self.stage = AStage::Insert;
                Ok(Step::Continue)
            }
            AStage::Insert => {
                self.at = match i {
                    Input::Point(p) => p,
                    Input::Enter => Vec2::ZERO,
                    Input::Text(t) => crate::prompt::parse_point(&t, Vec2::ZERO).ok_or_else(|| EngineError::Other("Requires a point.".into()))?,
                    _ => return Ok(Step::Continue),
                };
                if let Some((_, (w, h), _)) = &self.read {
                    let ratio = f64::from(*h) / f64::from((*w).max(1));
                    s.echo(format!("Base image size: Width: 1.000000, Height: {ratio:.6}, Unitless"));
                }
                self.stage = AStage::Scale;
                Ok(Step::Continue)
            }
            AStage::Scale => {
                let v = match i {
                    Input::Enter => 1.0,
                    Input::Point(p) => p.dist(self.at),
                    Input::Text(t) => number(&t).ok_or_else(|| EngineError::Other("Requires a positive number.".into()))?,
                    _ => return Ok(Step::Continue),
                };
                if !(v.is_finite() && v > 0.0 && v < 1e12) {
                    return Err(EngineError::Other("Value must be positive and nonzero.".into()));
                }
                self.scale = v;
                self.stage = AStage::Rotate;
                Ok(Step::Continue)
            }
            AStage::Rotate => {
                let r = match i {
                    Input::Enter => 0.0,
                    Input::Point(p) => (p - self.at).angle(),
                    Input::Text(t) => s.angle_settings().amount(&t).ok_or_else(|| EngineError::Other("Requires an angle.".into()))?,
                    _ => return Ok(Step::Continue),
                };
                self.finish(s, r)
            }
        }
    }
    fn preview(&self, _s: &Session, cursor: Vec2) -> Vec<EntityKind> {
        let pl = match self.stage {
            AStage::File => return Vec::new(),
            AStage::Insert => Placing { at: cursor, scale: self.scale, rotation: 0.0 },
            AStage::Scale => Placing { at: self.at, scale: cursor.dist(self.at), rotation: 0.0 },
            AStage::Rotate => Placing { at: self.at, scale: self.scale, rotation: (cursor - self.at).angle() },
        };
        if !(pl.scale.is_finite() && pl.scale > 0.0) {
            return Vec::new();
        }
        self.ghost(pl).into_iter().collect()
    }
}

// ---------- -IMAGE ----------

/// The definition names a JSON call names (`names`, default `default`): a string is a
/// comma-separated list matched with wildcards, as typed; an array names images exactly (a name
/// may hold commas, `#` or `*`, as file names do).
fn named(d: &Drawing, cmd: &str, p: &Value, default: Option<&str>) -> Result<Vec<String>> {
    let (given, exact) = match p.get("names") {
        Some(Value::Array(a)) => (a.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>(), true),
        Some(Value::String(t)) => (vec![t.clone()], false),
        _ => match str_param(p, "name").or(default) {
            Some(n) => (vec![n.to_string()], false),
            None => return Err(bad(cmd, "`names` (image names, wildcards allowed) is required")),
        },
    };
    let pats: Vec<&str> =
        if exact { given.iter().map(|p| p.trim()).collect() } else { given.iter().flat_map(|p| p.split(',')).map(str::trim).collect() };
    let pats: Vec<&str> = pats.into_iter().filter(|p| !p.is_empty()).collect();
    let pattern = given.join(",");
    let mut names: Vec<String> = Vec::new();
    for i in all_images(d) {
        let n = def_name(i);
        let hit = |p: &&str| if exact { p.eq_ignore_ascii_case(&n) } else { super::qselect::wildcard(p, &n) };
        if pats.iter().any(hit) && !names.iter().any(|x| x.eq_ignore_ascii_case(&n)) {
            names.push(n);
        }
    }
    if names.is_empty() {
        return Err(bad(cmd, format!("no image matches \"{}\"", pattern.chars().take(80).collect::<String>())));
    }
    Ok(names)
}

fn is_named(names: &[String]) -> impl Fn(&Image) -> bool + '_ {
    move |i: &Image| {
        let n = def_name(i);
        names.iter().any(|x| x.eq_ignore_ascii_case(&n))
    }
}

/// Erase every image named in `names` (model space, layouts and blocks); returns how many.
fn detach(d: &mut Drawing, names: &[String]) -> usize {
    let pick = is_named(names);
    let gone =
        |st: &EntityStore| -> Vec<Handle> { st.iter().filter(|e| matches!(&e.kind, EntityKind::Image(i) if pick(i))).map(|e| e.handle).collect() };
    let mut n = 0;
    for h in gone(&d.model) {
        n += usize::from(d.model.remove(h).is_some());
    }
    for l in &mut d.layouts {
        for h in gone(&l.entities) {
            n += usize::from(l.entities.remove(h).is_some());
        }
    }
    for b in d.blocks.values_mut().filter(|b| own_block(b)) {
        let hs = gone(&b.entities);
        if !hs.is_empty() {
            let b = Arc::make_mut(b);
            for h in hs {
                n += usize::from(b.entities.remove(h).is_some());
            }
        }
    }
    n
}

fn run_dash_image(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "-image";
    let option = str_param(p, "option").unwrap_or("?").trim().to_ascii_lowercase();
    let host = host(s);
    match option.as_str() {
        "?" | "list" => {
            let pats = match p.get("names") {
                None | Some(Value::Null) => None,
                Some(_) => Some(named(s.doc()?, cmd, p, None)?),
            };
            let defs: Vec<ImageDef> = image_defs(s.doc()?, host.as_deref())
                .into_iter()
                .filter(|d| pats.as_ref().is_none_or(|ns| ns.iter().any(|n| n.eq_ignore_ascii_case(&d.name))))
                .collect();
            Ok(json!({ "images": defs }))
        }
        "detach" | "d" => {
            let names = named(s.doc()?, cmd, p, None)?;
            let erased = detach(s.doc_mut()?, &names);
            let left: Vec<Handle> = s.selection().into_iter().filter(|h| s.doc().is_ok_and(|d| d.entity(*h).is_some())).collect();
            s.set_selection(left);
            Ok(json!({ "detached": names, "erased": erased }))
        }
        "unload" | "u" => {
            let names = named(s.doc()?, cmd, p, None)?;
            change_images(s.doc_mut()?, &is_named(&names), &|i| {
                i.unloaded = true;
                i.pixels = None;
            });
            Ok(json!({ "unloaded": names }))
        }
        "reload" | "r" => {
            let names = named(s.doc()?, cmd, p, Some("*"))?;
            let notes = load_pictures(s.doc_mut()?, host.as_deref(), Some(&names));
            for n in &notes {
                s.echo(n.clone());
            }
            Ok(json!({ "reloaded": names, "notFound": notes }))
        }
        "path" | "p" => {
            let path = str_param(p, "path")
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| bad(cmd, "`path` (the new path) is required"))?
                .to_string();
            let names = named(s.doc()?, cmd, p, Some("*"))?;
            let new = path.clone();
            change_images(s.doc_mut()?, &is_named(&names), &|i| {
                // The definition keeps its name when its file moves.
                if i.name.trim().is_empty() {
                    i.name = def_name(i);
                }
                i.path = new.clone();
            });
            let notes = load_pictures(s.doc_mut()?, host.as_deref(), Some(&names));
            for n in &notes {
                s.echo(n.clone());
            }
            Ok(json!({ "images": names, "path": path, "notFound": notes }))
        }
        "attach" | "a" => attach_image(s, cmd, p),
        o => Err(bad(cmd, format!("unknown option \"{}\" (?, detach, path, reload, unload or attach)", o.chars().take(20).collect::<String>()))),
    }
}

/// -IMAGE `?`: the definitions, one per line.
fn list(s: &mut Session, defs: &[ImageDef]) {
    s.echo(format!("{:<24} {:<12} {:>6} Path", "Image name", "Status", "Refs"));
    for d in defs {
        s.echo(format!("{:<24} {:<12} {:>6} {}", d.name, d.status, d.references, d.path));
    }
    let refs: usize = defs.iter().map(|d| d.references).sum();
    s.echo(format!("Total Image Definition(s): {}, Image Reference(s): {refs}", defs.len()));
}

#[derive(Default)]
enum DStage {
    #[default]
    Option,
    /// The images to list or act on.
    Names(&'static str),
    PathNames,
    /// The images whose paths are being changed, one at a time.
    NewPath(Vec<String>),
    Attach(Box<AttachM>),
}

/// -IMAGE typed at the command line.
#[derive(Default)]
struct DashImageM {
    stage: DStage,
}

impl Interactive for DashImageM {
    fn name(&self) -> &'static str {
        "-IMAGE"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match &self.stage {
            DStage::Option => {
                Prompt::new("Enter image option", crate::cmd::curves::KW).kw(&["?", "Detach", "Path", "Reload", "Unload", "Attach"]).default("Attach")
            }
            DStage::Names("?") => Prompt::new("Images to list", Accept::TEXT).default("*"),
            DStage::Names("reload") => Prompt::new("Enter list of images to reload", Accept::TEXT).default("*"),
            DStage::Names(op) => Prompt::new(format!("Enter list of images to {op}"), Accept::TEXT),
            DStage::PathNames => Prompt::new("Enter list of images for path modification", Accept::TEXT).default("*"),
            DStage::NewPath(_) => Prompt::new("Enter new path", Accept::TEXT),
            DStage::Attach(m) => m.prompt(s),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match &mut self.stage {
            DStage::Option => {
                let k = match &i {
                    Input::Enter => "attach".to_string(),
                    Input::Text(_) | Input::Keyword(_) => text(&i).unwrap_or_default().to_ascii_lowercase(),
                    _ => return Ok(Step::Continue),
                };
                self.stage = match k.as_str() {
                    "?" => DStage::Names("?"),
                    "detach" | "d" => DStage::Names("detach"),
                    "path" | "p" => DStage::PathNames,
                    "reload" | "r" => DStage::Names("reload"),
                    "unload" | "u" => DStage::Names("unload"),
                    "attach" | "a" => DStage::Attach(Box::new(AttachM { typed: true, ..AttachM::new(Kind::Image) })),
                    _ => return Err(EngineError::Other("Invalid option keyword.".into())),
                };
                Ok(Step::Continue)
            }
            DStage::Names(op) => {
                let op = *op;
                let names = match text(&i) {
                    Some(t) => t,
                    None if matches!(op, "?" | "reload") => "*".into(),
                    None => return Ok(Step::Done),
                };
                if s.doc().is_ok_and(|d| all_images(d).next().is_none()) {
                    s.echo("No images are attached.");
                    return Ok(Step::Done);
                }
                let r = run_dash_image(s, &json!({ "option": op, "names": names }))?;
                match op {
                    "?" => {
                        let defs: Vec<ImageDef> = image_defs(s.doc()?, host(s).as_deref())
                            .into_iter()
                            .filter(|d| r["images"].as_array().is_some_and(|a| a.iter().any(|x| x["name"] == d.name.as_str())))
                            .collect();
                        list(s, &defs);
                    }
                    "detach" => s.echo(format!("Detached {} image(s).", r["erased"].as_u64().unwrap_or(0))),
                    "unload" => s.echo(format!("Unloaded {}.", names)),
                    _ => {}
                }
                Ok(Step::Done)
            }
            DStage::PathNames => {
                let t = text(&i).unwrap_or_else(|| "*".into());
                let names = match named(s.doc()?, "-image", &json!({ "names": t }), None) {
                    Ok(n) => n,
                    Err(_) => {
                        s.echo(format!("No image matches \"{t}\"."));
                        return Ok(Step::Done);
                    }
                };
                if let Some(first) = names.first() {
                    let old = s.doc().ok().and_then(|d| all_images(d).find(|i| def_name(i).eq_ignore_ascii_case(first)).map(|i| i.path.clone()));
                    s.echo(format!("Old path: {}", old.unwrap_or_default()));
                }
                self.stage = DStage::NewPath(names);
                Ok(Step::Continue)
            }
            DStage::NewPath(names) => {
                let n = if names.is_empty() { String::new() } else { names.remove(0) };
                if let Some(t) = text(&i) {
                    run_dash_image(s, &json!({ "option": "path", "names": n, "path": t }))?;
                }
                if let Some(next) = names.first() {
                    let old = s.doc().ok().and_then(|d| all_images(d).find(|i| def_name(i).eq_ignore_ascii_case(next)).map(|i| i.path.clone()));
                    s.echo(format!("Old path: {}", old.unwrap_or_default()));
                    return Ok(Step::Continue);
                }
                Ok(Step::Done)
            }
            DStage::Attach(m) => m.input(s, i),
        }
    }
    fn preview(&self, s: &Session, cursor: Vec2) -> Vec<EntityKind> {
        match &self.stage {
            DStage::Attach(m) => m.preview(s, cursor),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
