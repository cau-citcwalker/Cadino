//! ETRANSMIT: package the drawing with the files it refers to (xrefs, images, font files, plot
//! style tables) in one `.zip`, with a transmittal report listing what went in and what could
//! not be found. The drawing goes in as saved (or as it is now, when it has unsaved changes);
//! files found by a relative path keep their folders, others go next to it, where the drawing
//! looks for them (by file name in its folder) when the package is opened elsewhere. Each xref
//! brings its own attached xrefs and images (found from its folder, put where it looks for
//! them in the package), at most `MAX_DEPTH` levels deep; a drawing already packed (a
//! circular reference) isn't read again.
//!
//! The archive is a plain "stored" ZIP (no compression) written here: no ZIP library for one
//! writer.
// ponytail: no ZIP64 records, and the package is built in memory: one past 4 GB or
// `MAX_ENTRIES` files is refused, saying so. Stream it to the file with ZIP64 records if
// packages that big matter.

use serde_json::{Value, json};

use super::file::{base64_encode, io};
use super::{CommandSpec, bad, bool_or, str_param};
use crate::{Accept, EngineError, Input, Interactive, Picker, Prompt, Result, Session, Step};

/// Largest package written (ZIP without its 64-bit extension).
const MAX_ZIP_BYTES: u64 = u32::MAX as u64 - (64 << 20);
/// Most files in one package.
const MAX_ENTRIES: usize = 10_000;
/// Largest referenced file packed.
const MAX_FILE_BYTES: u64 = 1 << 30;
/// Largest referenced drawing read for its own references.
const MAX_READ_BYTES: u64 = 512 << 20;
/// Deepest nesting of xrefs packed (the drawing's own are the first level).
const MAX_DEPTH: usize = cadcraft_doc::MAX_BLOCK_DEPTH;

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("etransmit", "Package Drawing...", run_etransmit)
            .menu(&["File", "Package Drawing..."])
            .params(
                "{path?: \"NAME.zip\" (default: the drawing's name beside it; without one, returns base64 `data`), reportOnly?: bool (just the report)} \
                 → {path? | data, bytes, files: [{name, kind, from}], missing: [{name, kind}], report}",
            )
            .noundo()
            .interactive(|_| Ok(Box::new(TransmitM::default()))),
        CommandSpec::new("-etransmit", "Package Drawing (command line)", run_etransmit)
            .params("as ETRANSMIT")
            .noundo()
            .interactive(|_| Ok(Box::new(TransmitM { typed: true, ..TransmitM::default() }))),
    ]
}

// ---------- the ZIP writer ----------

/// CRC-32 (IEEE, as ZIP uses) lookup table.
const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |c, b| CRC_TABLE.get(((c ^ u32::from(*b)) & 0xFF) as usize).copied().unwrap_or(0) ^ (c >> 8))
}

/// A stored (uncompressed) ZIP archive written one file at a time, so a file's contents are
/// held only while it is added. Names are UTF-8 with `/` between folders.
#[derive(Default)]
pub(crate) struct Zip {
    out: Vec<u8>,
    central: Vec<u8>,
    files: u16,
}

impl Zip {
    /// Add file `name` holding `data`. `None` past the 32-bit ZIP limits.
    pub(crate) fn add(&mut self, name: &str, data: &[u8]) -> Option<()> {
        let files = self.files.checked_add(1)?;
        let total = (self.out.len() + self.central.len()) as u64 + 76 + 2 * name.len() as u64 + data.len() as u64 + 22;
        if total > u64::from(u32::MAX) {
            return None;
        }
        // 1980-01-01 00:00, UTF-8 names (flag bit 11), stored (method 0).
        let (time, date, flags) = (0u16, 0x21u16, 0x0800u16);
        let offset = u32::try_from(self.out.len()).ok()?;
        let (crc, size, nlen) = (crc32(data), u32::try_from(data.len()).ok()?, u16::try_from(name.len()).ok()?);
        let head = |sig: u32, central: bool, out: &mut Vec<u8>| {
            out.extend(sig.to_le_bytes());
            if central {
                out.extend(20u16.to_le_bytes());
            }
            for v in [20u16, flags, 0, time, date] {
                out.extend(v.to_le_bytes());
            }
            for v in [crc, size, size] {
                out.extend(v.to_le_bytes());
            }
            out.extend(nlen.to_le_bytes());
            out.extend(0u16.to_le_bytes());
        };
        head(0x0403_4B50, false, &mut self.out);
        self.out.extend(name.as_bytes());
        self.out.extend(data);
        head(0x0201_4B50, true, &mut self.central);
        // Comment length, disk, internal and external attributes, the local header's offset.
        for v in [0u16, 0, 0] {
            self.central.extend(v.to_le_bytes());
        }
        self.central.extend(0u32.to_le_bytes());
        self.central.extend(offset.to_le_bytes());
        self.central.extend(name.as_bytes());
        self.files = files;
        Some(())
    }

    /// The archive. `None` past the 32-bit ZIP limits.
    pub(crate) fn finish(mut self) -> Option<Vec<u8>> {
        let (at, size, n) = (u32::try_from(self.out.len()).ok()?, u32::try_from(self.central.len()).ok()?, self.files);
        self.out.append(&mut self.central);
        self.out.extend(0x0605_4B50u32.to_le_bytes());
        for v in [0u16, 0, n, n] {
            self.out.extend(v.to_le_bytes());
        }
        self.out.extend(size.to_le_bytes());
        self.out.extend(at.to_le_bytes());
        self.out.extend(0u16.to_le_bytes());
        Some(self.out)
    }
}

/// A stored ZIP archive of `files` (name, contents).
#[cfg(test)]
pub(crate) fn zip(files: &[(String, Vec<u8>)]) -> Option<Vec<u8>> {
    let mut z = Zip::default();
    for (n, b) in files {
        z.add(n, b)?;
    }
    z.finish()
}

// ---------- what goes in ----------

/// What a packaged file holds.
enum Src {
    /// The drawing, written out.
    Bytes(Vec<u8>),
    /// A referenced file and its size, read when the archive is written.
    File(std::path::PathBuf),
    /// Not found: listed in the report only.
    Missing,
}

/// A file the package holds or misses.
struct Item {
    /// Its name in the archive, or as the drawing refers to it.
    name: String,
    kind: &'static str,
    /// Where it was read from.
    from: Option<String>,
    src: Src,
}

impl Item {
    fn packed(&self) -> bool {
        !matches!(self.src, Src::Missing)
    }
}

/// The file name of `path` (either separator).
fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).trim().to_string()
}

/// Where a drawing packed as `parent` (an archive name) looks for a file it saves as `saved`:
/// that path from its folder while it stays in the package, else the file name beside it.
/// `None` for a path naming no file.
fn nested_name(parent: &str, saved: &str) -> Option<String> {
    let dir: Vec<&str> = parent.rsplit_once('/').map_or(Vec::new(), |(d, _)| d.split('/').collect());
    let norm = saved.trim().replace('\\', "/");
    let rooted = norm.starts_with('/') || norm.get(1..2) == Some(":");
    let mut parts = dir.clone();
    // Inside the package, and ending in a name.
    let (mut inside, mut named) = (!rooted, false);
    for p in norm.split('/') {
        if !inside {
            break;
        }
        match p {
            "" | "." => {}
            ".." => (inside, named) = (parts.pop().is_some(), false),
            p => {
                parts.push(p);
                named = true;
            }
        }
    }
    if !(inside && named) {
        let name = file_name(saved);
        if matches!(name.as_str(), "" | "." | "..") {
            return None;
        }
        return Some(dir.into_iter().map(str::to_string).chain(std::iter::once(name)).collect::<Vec<_>>().join("/"));
    }
    Some(parts.join("/"))
}

/// Referenced drawing `file` (`len` bytes) read for its own references; `None` when it can't be.
fn read_drawing(file: &std::path::Path, len: u64) -> Option<cadcraft_doc::Drawing> {
    let hooks = io()?;
    if len > MAX_READ_BYTES {
        return None;
    }
    let bytes = read_file(file).ok()?;
    (hooks.read)(&bytes, &file.to_string_lossy()).ok()
}

/// The archive name of a file the drawing saves as `saved`: its relative path (folders kept,
/// never above the package), else its file name.
fn archive_name(saved: &str) -> String {
    let norm = saved.trim().replace('\\', "/");
    let rooted = norm.starts_with('/') || norm.get(1..2) == Some(":");
    let parts: Vec<&str> = norm.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    if rooted || parts.contains(&"..") || parts.is_empty() { file_name(saved) } else { parts.join("/") }
}

/// The file a reference saved as `saved` is found at (as xrefs are looked for), the same file
/// under any spelling, and its size.
fn locate(saved: &str, dir: Option<&std::path::Path>) -> Option<(std::path::PathBuf, std::path::PathBuf, u64)> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let f = super::blocks::xref::load::find(saved, dir)?;
        let len = std::fs::metadata(&f).ok()?.len();
        let key = std::fs::canonicalize(&f).unwrap_or_else(|_| f.clone());
        Some((f, key, len))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (saved, dir);
        None
    }
}

/// A font the app finds without the drawing's folder: the built-in stroke font or an installed
/// one. It isn't packed, nor reported missing.
fn system_font(name: &str) -> bool {
    let n = name.trim();
    n.eq_ignore_ascii_case(cadcraft_fonts::BUILTIN_FONT) || cadcraft_fonts::ttf::find(n).is_some()
}

/// The drawing's own file for the package: the saved file when there are no unsaved changes and
/// its references keep their paths, else the drawing as it is now with references saved as
/// `renamed` (saved path → archive name) pointing at their archive names.
fn drawing_item(s: &Session, renamed: &[(String, String)]) -> Result<Item> {
    let st = s.state()?;
    let name = drawing_name(st);
    #[cfg(not(target_arch = "wasm32"))]
    if renamed.is_empty()
        && let Some(p) = st.path.as_deref().filter(|_| !st.is_dirty())
        && let Ok(bytes) = std::fs::read(p)
    {
        return Ok(Item { name, kind: "drawing", from: Some(p.to_string()), src: Src::Bytes(bytes) });
    }
    let hooks = io().ok_or_else(|| bad("etransmit", "file formats are not available in this build"))?;
    let bytes = if renamed.is_empty() { (hooks.write)(st.drawing(), &name) } else { (hooks.write)(&repath(st.drawing(), renamed), &name) }
        .map_err(|e| bad("etransmit", e))?;
    Ok(Item { name, kind: "drawing", from: None, src: Src::Bytes(bytes) })
}

/// The drawing's file name in the package.
fn drawing_name(st: &crate::DocState) -> String {
    let mut name = file_name(st.path.as_deref().unwrap_or(&st.title));
    let lower = name.to_ascii_lowercase();
    if !(lower.ends_with(".dwg") || lower.ends_with(".dxf")) {
        name.push_str(".dxf");
    }
    name
}

/// `d` with its references saved as one of `renamed`'s paths saved as its archive name instead.
fn repath(d: &cadcraft_doc::Drawing, renamed: &[(String, String)]) -> cadcraft_doc::Drawing {
    let mut d = d.clone();
    let to = |p: &str| renamed.iter().find(|(from, _)| from.eq_ignore_ascii_case(p.trim())).map(|(_, to)| to.clone());
    let fix = |st: &mut cadcraft_doc::EntityStore| {
        let hs: Vec<cadcraft_doc::Handle> =
            st.iter().filter(|e| matches!(&e.kind, cadcraft_doc::EntityKind::Image(i) if to(&i.path).is_some())).map(|e| e.handle).collect();
        for h in hs {
            st.modify(h, |e| {
                if let cadcraft_doc::EntityKind::Image(i) = &mut e.kind
                    && let Some(n) = to(&i.path)
                {
                    // The definition keeps its name when its file is renamed.
                    if i.name.trim().is_empty() {
                        i.name = super::images::def_name(i);
                    }
                    i.path = n;
                }
            });
        }
    };
    fix(&mut d.model);
    for l in &mut d.layouts {
        fix(&mut l.entities);
    }
    for b in d.blocks.values_mut() {
        let b = std::sync::Arc::make_mut(b);
        if let Some(n) = b.xref_path.as_deref().and_then(to) {
            b.xref_path = Some(n);
        }
        if b.xref_path.is_none() && !b.name.contains('|') {
            fix(&mut b.entities);
        }
    }
    for t in &mut d.text_styles {
        for f in [&mut t.font, &mut t.big_font] {
            if let Some(n) = to(f) {
                *f = n;
            }
        }
    }
    for l in &mut d.layouts {
        if let Some(n) = to(&l.page.plot_style_table) {
            l.page.plot_style_table = n;
        }
    }
    d
}

/// Everything the package holds or misses, the drawing first. Each file goes in once (one file
/// under several spellings, too); two files that would share a name are told apart, and the
/// drawing then goes in with its references pointing at their names in the package. Sizes are
/// checked before anything is read.
fn items(s: &Session) -> Result<Vec<Item>> {
    let st = s.state()?;
    let d = st.drawing();
    let dir = st.path.as_deref().and_then(|p| std::path::Path::new(p).parent()).map(std::path::Path::to_path_buf);
    let mut wanted: Vec<(String, &'static str)> = Vec::new();
    let mut want = |saved: &str, kind: &'static str| {
        let saved = saved.trim();
        if !saved.is_empty() && !wanted.iter().any(|(w, _)| w.eq_ignore_ascii_case(saved)) {
            wanted.push((saved.to_string(), kind));
        }
    };
    for b in d.blocks.values().filter(|b| !b.name.contains('|')) {
        if let Some(p) = &b.xref_path {
            want(p, "xref");
        }
    }
    for def in super::images::image_defs(d, st.path.as_deref()) {
        want(&def.path, "image");
    }
    for t in &d.text_styles {
        for f in [&t.font, &t.big_font] {
            want(f, "font");
        }
    }
    for l in &d.layouts {
        let n = l.page.plot_style_table.trim();
        // Tables kept in the drawing or built in travel with it.
        if !cadcraft_doc::no_plot_style_table(n) && cadcraft_doc::plot_style_table(d, n).is_none() {
            want(n, "plot style");
        }
    }
    let mut out: Vec<Item> = Vec::new();
    let own = drawing_name(st);
    // Files packed: the file, its archive name.
    let mut packed: Vec<(std::path::PathBuf, String)> = Vec::new();
    // The drawing's own file is in the package as itself (an xref referring back to it).
    let host = st.path.as_deref().and_then(|p| locate(p, None)).map(|(_, key, _)| key);
    let mut read: Vec<std::path::PathBuf> = host.iter().cloned().collect();
    // References whose archive name isn't where the drawing would look for them.
    let mut renamed: Vec<(String, String)> = Vec::new();
    let mut total: u64 = 0;
    for (saved, kind) in wanted.into_iter().take(MAX_ENTRIES) {
        // A font named without its extension is a shape font.
        let look = if kind == "font" && !saved.contains('.') { format!("{saved}.shx") } else { saved.clone() };
        let found = locate(&look, dir.as_deref()).filter(|(_, _, len)| *len <= MAX_FILE_BYTES);
        let Some((file, key, len)) = found else {
            if !(kind == "font" && system_font(&saved)) {
                out.push(Item { name: saved, kind, from: None, src: Src::Missing });
            }
            continue;
        };
        let expected = archive_name(&look);
        let name = match packed.iter().find(|(k, _)| *k == key) {
            // The same file under another spelling: packed once.
            Some((_, n)) => n.clone(),
            None => {
                total = total.saturating_add(len);
                if total > MAX_ZIP_BYTES {
                    return Err(too_big());
                }
                // Two files can't share a name in the archive.
                let taken = |n: &str| n.eq_ignore_ascii_case(&own) || packed.iter().any(|(_, p)| p.eq_ignore_ascii_case(n));
                let mut name = expected.clone();
                let mut k = 2;
                while taken(&name) && k < 100_000 {
                    name = match expected.rsplit_once('.') {
                        Some((a, b)) => format!("{a}_{k}.{b}"),
                        None => format!("{expected}_{k}"),
                    };
                    k += 1;
                }
                packed.push((key, name.clone()));
                out.push(Item { name: name.clone(), kind, from: Some(file.to_string_lossy().into_owned()), src: Src::File(file) });
                name
            }
        };
        // Opened from the package, the drawing looks for it as saved (relative to its folder),
        // then by its file name beside it: where `archive_name` puts it.
        if name != expected {
            renamed.push((saved.clone(), name));
        }
    }
    nested(&mut out, &mut packed, &mut read, &mut total, (&own, host.as_deref()))?;
    // A shape font is saved without its extension.
    for (saved, name) in &mut renamed {
        if !saved.contains('.') && name.to_ascii_lowercase().ends_with(".shx") {
            name.truncate(name.len() - 4);
        }
    }
    let mut all = vec![drawing_item(s, &renamed)?];
    all.extend(out);
    Ok(all)
}

/// Add the xrefs and images the packed xrefs of `out` refer to, and theirs in turn (`packed`:
/// the files in the package and their names; `read`: the drawings already read, by file;
/// `total`: the bytes packed so far; the drawing itself is packed as `own`, from `host` when it
/// has a file). The xrefs go in as they are, so each file goes where its drawing looks for it;
/// when another file has that name already, it is reported missing.
// ponytail: a drawing reached by several paths is read once, its references placed from where
// it was first packed.
fn nested(
    out: &mut Vec<Item>,
    packed: &mut Vec<(std::path::PathBuf, String)>,
    read: &mut Vec<std::path::PathBuf>,
    total: &mut u64,
    (own, host): (&str, Option<&std::path::Path>),
) -> Result<()> {
    // Drawings to read: file, size, archive name, level.
    let mut queue: Vec<(std::path::PathBuf, u64, String, usize)> = Vec::new();
    for i in out.iter().filter(|i| i.kind == "xref") {
        if let (Src::File(f), Some((key, _))) = (&i.src, packed.iter().find(|(_, n)| *n == i.name)) {
            let len = std::fs::metadata(f).map(|m| m.len()).unwrap_or(u64::MAX);
            queue.push((f.clone(), len, i.name.clone(), 1));
            read.push(key.clone());
        }
    }
    let mut next = 0;
    while let Some((file, len, name, level)) = queue.get(next).cloned() {
        next += 1;
        if level >= MAX_DEPTH {
            continue;
        }
        let Some(x) = read_drawing(&file, len) else { continue };
        let dir = file.parent().map(std::path::Path::to_path_buf);
        let from = file.to_string_lossy().into_owned();
        let xrefs = x.blocks.values().filter(|b| !b.xref_overlay && !b.name.contains('|')).filter_map(|b| b.xref_path.clone()).map(|p| (p, "xref"));
        let images = super::images::image_defs(&x, Some(&from)).into_iter().map(|d| (d.path, "image"));
        for (saved, kind) in xrefs.chain(images).collect::<Vec<_>>() {
            if saved.trim().is_empty() {
                continue;
            }
            if out.len() >= MAX_ENTRIES {
                return Err(too_many());
            }
            let Some(want) = nested_name(&name, &saved) else { continue };
            let found = locate(&saved, dir.as_deref()).filter(|(_, _, len)| *len <= MAX_FILE_BYTES);
            let missing = |out: &mut Vec<Item>| {
                if !out.iter().any(|i| !i.packed() && i.name.eq_ignore_ascii_case(&want)) {
                    out.push(Item { name: want.clone(), kind, from: None, src: Src::Missing });
                }
            };
            let Some((f, key, flen)) = found else {
                missing(out);
                continue;
            };
            // The file already at that name: one packed, or the drawing itself.
            let there = match packed.iter().find(|(_, n)| n.eq_ignore_ascii_case(&want)) {
                Some((k, _)) => Some(Some(k.clone())),
                None => want.eq_ignore_ascii_case(own).then(|| host.map(std::path::Path::to_path_buf)),
            };
            match there {
                Some(Some(k)) if k == key => {}
                Some(_) => missing(out),
                None => {
                    *total = total.saturating_add(flen);
                    if *total > MAX_ZIP_BYTES {
                        return Err(too_big());
                    }
                    packed.push((key.clone(), want.clone()));
                    out.push(Item { name: want.clone(), kind, from: Some(f.to_string_lossy().into_owned()), src: Src::File(f.clone()) });
                    if kind == "xref" && !read.contains(&key) {
                        read.push(key);
                        queue.push((f, flen, want, level + 1));
                    }
                }
            }
        }
    }
    Ok(())
}

fn too_big() -> EngineError {
    bad("etransmit", "the package would be larger than a ZIP file can hold (4 GB)")
}

fn too_many() -> EngineError {
    bad("etransmit", format!("the package would hold more than {MAX_ENTRIES} files"))
}

/// The transmittal report.
fn report(drawing: &str, items: &[Item]) -> String {
    let mut r = format!("Transmittal Report\n\nDrawing: {drawing}\n\nFiles in this package:\n");
    for i in items.iter().filter(|i| i.packed()) {
        r.push_str(&format!("  {} ({})\n", i.name, i.kind));
    }
    let missing: Vec<&Item> = items.iter().filter(|i| !i.packed()).collect();
    if !missing.is_empty() {
        r.push_str("\nNot found, not included (send them separately):\n");
        for i in missing {
            r.push_str(&format!("  {} ({})\n", i.name, i.kind));
        }
    }
    r
}

/// The contents of a referenced file being packed.
fn read_file(f: &std::path::Path) -> std::result::Result<Vec<u8>, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::fs::read(f).map_err(|e| e.to_string())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = f;
        Err("files are not available on the web".into())
    }
}

fn write(path: &str, bytes: &[u8]) -> Result<()> {
    crate::limits::files("etransmit")?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let tmp = format!("{path}.cadcraft-tmp");
        std::fs::write(&tmp, bytes).map_err(|e| bad("etransmit", format!("{path}: {e}")))?;
        std::fs::rename(&tmp, path).map_err(|e| bad("etransmit", format!("{path}: {e}")))?;
        Ok(())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let deliver = super::file::deliver().ok_or_else(|| bad("etransmit", "omit `path` to get the package as base64 `data`"))?;
        deliver(&file_name(path), "application/zip", bytes).map_err(|e| bad("etransmit", e))
    }
}

fn run_etransmit(s: &mut Session, p: &Value) -> Result<Value> {
    crate::limits::files("etransmit")?;
    let items = items(s)?;
    let drawing = items.first().map(|i| i.name.clone()).unwrap_or_default();
    let stem = drawing.rsplit_once('.').map_or(drawing.as_str(), |(a, _)| a).to_string();
    let text = report(&drawing, &items);
    let list = |have: bool| -> Vec<Value> {
        items.iter().filter(|i| i.packed() == have).map(|i| json!({ "name": i.name, "kind": i.kind, "from": i.from })).collect()
    };
    let (files, missing) = (list(true), list(false));
    if bool_or(p, "reportOnly", false) {
        return Ok(json!({ "files": files, "missing": missing, "report": text }));
    }
    if items.len() >= MAX_ENTRIES {
        return Err(too_many());
    }
    // One file at a time: only the archive and the file being added are held.
    let mut z = Zip::default();
    for i in &items {
        match &i.src {
            Src::Bytes(b) => z.add(&i.name, b).ok_or_else(too_big)?,
            Src::File(f) => {
                let bytes = read_file(f).map_err(|e| bad("etransmit", format!("{}: {e}", f.to_string_lossy())))?;
                z.add(&i.name, &bytes).ok_or_else(too_big)?
            }
            Src::Missing => {}
        }
    }
    z.add(&format!("{stem} - Transmittal Report.txt"), text.as_bytes()).ok_or_else(too_big)?;
    let bytes = z.finish().ok_or_else(too_big)?;
    let host = s.state()?.path.clone();
    let path = match str_param(p, "path").map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => Some(t.to_string()),
        None => host.as_deref().map(|h| {
            let base = h.rsplit_once('.').map_or(h, |(a, _)| a);
            format!("{base}.zip")
        }),
    };
    let mut r = json!({ "bytes": bytes.len(), "files": files, "missing": missing, "report": text });
    match path {
        Some(path) => {
            write(&path, &bytes)?;
            s.echo(format!("Transmittal package created: {path}"));
            r["path"] = json!(path);
        }
        None => r["data"] = json!(base64_encode(&bytes)),
    }
    Ok(r)
}

/// ETRANSMIT typed: create the package (the file dialog with FILEDIA 1) or show the report.
/// ponytail: no Create Transmittal dialog (file tree, setups); the package always holds every
/// file it finds.
#[derive(Default)]
struct TransmitM {
    /// -ETRANSMIT: the name is typed, no file dialog.
    typed: bool,
    /// The option was answered: now the package's file name.
    file: bool,
}

impl Interactive for TransmitM {
    fn name(&self) -> &'static str {
        "ETRANSMIT"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        if !self.file {
            return Prompt::new("Enter an option", Accept::WORD)
                .kw(&["Create transmittal package", "Report only"])
                .default("Create transmittal package");
        }
        let st = s.state().ok();
        let base = st.and_then(|st| st.path.clone()).unwrap_or_else(|| st.map(|st| st.title.clone()).unwrap_or_default());
        let name = format!("{}.zip", base.rsplit_once('.').map_or(base.as_str(), |(a, _)| a));
        let p = Prompt::new("Enter name of transmittal package file", Accept::TEXT).default(name);
        if self.typed { p } else { p.picker(Picker::File { save: true, label: "ZIP archives", exts: &["zip"] }) }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let t = match &i {
            Input::Text(t) | Input::Keyword(t) => t.trim().trim_matches('"').trim().to_string(),
            Input::Enter => String::new(),
            _ => return Ok(Step::Continue),
        };
        if !self.file {
            let l = t.to_ascii_lowercase();
            if l.starts_with('r') {
                let r = run_etransmit(s, &json!({ "reportOnly": true }))?;
                for line in r["report"].as_str().unwrap_or_default().lines() {
                    s.echo(line.to_string());
                }
                return Ok(Step::Done);
            }
            if !(l.is_empty() || l.starts_with('c')) {
                return Err(EngineError::Other("Invalid option keyword.".into()));
            }
            self.file = true;
            return Ok(Step::Continue);
        }
        let path = if t.is_empty() { self.prompt(s).default.unwrap_or_default() } else { t };
        let path = if path.to_ascii_lowercase().ends_with(".zip") { path } else { format!("{path}.zip") };
        run_etransmit(s, &json!({ "path": path }))?;
        Ok(Step::Done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The entries of a stored ZIP, read back by its central directory.
    fn unzip(z: &[u8]) -> Vec<(String, Vec<u8>)> {
        let u16_at = |o: usize| u16::from_le_bytes([z[o], z[o + 1]]) as usize;
        let u32_at = |o: usize| u32::from_le_bytes([z[o], z[o + 1], z[o + 2], z[o + 3]]) as usize;
        let end = z.len() - 22;
        assert_eq!(u32_at(end), 0x0605_4B50);
        let (n, mut at) = (u16_at(end + 10), u32_at(end + 16));
        let mut out = Vec::new();
        for _ in 0..n {
            assert_eq!(u32_at(at), 0x0201_4B50);
            let (crc, size, nlen, local) = (u32_at(at + 16), u32_at(at + 24), u16_at(at + 28), u32_at(at + 42));
            let name = String::from_utf8(z[at + 46..at + 46 + nlen].to_vec()).unwrap();
            assert_eq!(u32_at(local), 0x0403_4B50);
            let data = z[local + 30 + nlen..local + 30 + nlen + size].to_vec();
            assert_eq!(crc32(&data) as usize, crc);
            out.push((name, data));
            at += 46 + nlen;
        }
        out
    }

    #[test]
    fn crc_and_zip_read_back() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        let files =
            vec![("a.txt".to_string(), b"hello".to_vec()), ("photos/kitchen.png".to_string(), vec![0, 1, 2, 255]), ("empty".to_string(), Vec::new())];
        assert_eq!(unzip(&zip(&files).unwrap()), files);
        let empty = zip(&[]).unwrap();
        assert_eq!(empty.len(), 22);
    }

    #[test]
    fn archive_names_never_leave_the_package() {
        assert_eq!(archive_name("photos\\site.jpg"), "photos/site.jpg");
        assert_eq!(archive_name("./a/b.png"), "a/b.png");
        assert_eq!(archive_name("../elsewhere/b.png"), "b.png");
        assert_eq!(archive_name("C:\\pics\\c.png"), "c.png");
        assert_eq!(archive_name("/home/me/d.png"), "d.png");
    }

    #[test]
    fn a_nested_reference_goes_where_its_drawing_looks_for_it() {
        let n = |parent: &str, saved: &str| nested_name(parent, saved);
        assert_eq!(n("xrefs/a.dxf", "b.dxf").as_deref(), Some("xrefs/b.dxf"));
        assert_eq!(n("xrefs/a.dxf", "..\\pics\\p.png").as_deref(), Some("pics/p.png"));
        assert_eq!(n("x/y/a.dxf", "../b.png").as_deref(), Some("x/b.png"));
        assert_eq!(n("a.dxf", "./sub/b.dxf").as_deref(), Some("sub/b.dxf"));
        // Leaving the package or rooted: the file name beside the drawing.
        assert_eq!(n("a.dxf", "../../far/b.dxf").as_deref(), Some("b.dxf"));
        assert_eq!(n("x/y/a.dxf", "C:\\work\\b.dxf").as_deref(), Some("x/y/b.dxf"));
        assert_eq!(n("x/a.dxf", "/home/me/c.png").as_deref(), Some("x/c.png"));
        // No file named: nothing (never a name that climbs out of the package).
        assert_eq!(n("x/a.dxf", ".."), None);
        assert_eq!(n("x/a.dxf", "sub/.."), None);
    }
}
