//! -EXPORT and -PUBLISH typed at the command line (the app asks with its file dialogs for a bare
//! EXPORT or PUBLISH). Both end in the JSON form, so validation matches it.

use serde_json::{Value, json};

use super::EXPORT_FORMATS;
use crate::{Accept, EngineError, Input, Interactive, Picker, Prompt, Result, Session, Step};

const KW: Accept = Accept { point: false, number: false, text: false, select: false, enter: true };
/// The format keywords, as EXPORT_FORMATS in capitals.
const FORMATS: &[&str] = &["DXF", "DWG", "SVG", "PNG", "PDF"];
const CURRENT: &str = "Current sheets";

/// `<drawing folder>/<drawing name><suffix>.<ext>`.
fn default_file(s: &Session, suffix: &str, ext: &str) -> String {
    let st = s.state().ok();
    let title = st.map(|st| st.title.clone()).unwrap_or_default();
    let stem = std::path::Path::new(&title).file_stem().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty());
    let name = format!("{}{suffix}.{ext}", stem.unwrap_or_else(|| "Drawing".into()));
    match st.and_then(|st| st.path.as_deref()).and_then(|p| std::path::Path::new(p).parent()).filter(|d| !d.as_os_str().is_empty()) {
        Some(d) => d.join(name).to_string_lossy().into_owned(),
        None => name,
    }
}

/// The typed file name (quotes trimmed) with `.ext` added when it has none; `None` for Enter.
fn typed_file(i: &Input, ext: &str) -> Option<String> {
    let t = match i {
        Input::Text(t) | Input::Keyword(t) => t.trim().trim_matches('"').trim(),
        _ => return None,
    };
    if t.is_empty() {
        return None;
    }
    Some(if std::path::Path::new(t).extension().is_some() { t.to_string() } else { format!("{t}.{ext}") })
}

fn picker(ext: &str) -> Picker {
    let (label, exts): (&'static str, &'static [&'static str]) = match ext {
        "dxf" => ("DXF", &["dxf"]),
        "dwg" => ("DWG", &["dwg"]),
        "svg" => ("SVG", &["svg"]),
        "png" => ("PNG", &["png"]),
        _ => ("PDF", &["pdf"]),
    };
    Picker::File { save: true, label, exts }
}

/// -EXPORT: the format, then the file name.
#[derive(Default)]
pub(super) struct ExportM {
    format: Option<String>,
}

impl ExportM {
    fn export(s: &mut Session, path: String, format: &str) -> Result<Step> {
        let r = super::run_export(s, &json!({ "path": path, "format": format }))?;
        if let Some(m) = r.get("message").and_then(Value::as_str) {
            s.echo(m.to_string());
        }
        Ok(Step::Done)
    }
}

impl Interactive for ExportM {
    fn name(&self) -> &'static str {
        "-EXPORT"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match &self.format {
            None => Prompt::new("Enter file format", KW).kw(FORMATS).default("PDF"),
            Some(f) => Prompt::new("Enter file name", Accept::WORD).default(default_file(s, "", f)).picker(picker(f)),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(f) = self.format.clone() else {
            match &i {
                Input::Keyword(k) => self.format = Some(k.to_ascii_lowercase()),
                Input::Enter => self.format = Some("pdf".into()),
                // A file name with a format's extension answers both questions.
                _ => {
                    // No extension added here: "txt" is a wrong format, not "txt.pdf".
                    let path = typed_file(&i, "").unwrap_or_default();
                    let ext = std::path::Path::new(&path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                    if !EXPORT_FORMATS.contains(&ext.as_str()) {
                        return Err(EngineError::Other("Invalid option keyword.".into()));
                    }
                    return Self::export(s, path, &ext);
                }
            }
            return Ok(Step::Continue);
        };
        let path = typed_file(&i, &f).unwrap_or_else(|| default_file(s, "", &f));
        Self::export(s, path, &f)
    }
}

/// -PUBLISH: a sheet list file (one sheet per line: `Model` or a layout name; blank lines and
/// lines starting with `;` or `#` are skipped) or the current sheets, then the output file.
#[derive(Default)]
pub(super) struct PublishM {
    /// `Some`: the sheets are chosen (empty: the current sheets, PUBLISH's default).
    sheets: Option<Vec<String>>,
}

fn read_sheet_list(_path: &str) -> Result<Vec<String>> {
    crate::limits::files("publish")?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::io::Read;
        /// Most bytes and sheets read from a sheet list file.
        const MAX_LIST_BYTES: u64 = 1 << 20;
        const MAX_LIST_SHEETS: usize = 1000;
        let path = _path;
        let mut text = String::new();
        std::fs::File::open(path)
            .and_then(|f| f.take(MAX_LIST_BYTES).read_to_string(&mut text))
            .map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
        let sheets: Vec<String> =
            text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with([';', '#'])).take(MAX_LIST_SHEETS).map(str::to_string).collect();
        if sheets.is_empty() {
            return Err(EngineError::Other(format!("{path}: the sheet list is empty.")));
        }
        Ok(sheets)
    }
    #[cfg(target_arch = "wasm32")]
    Err(EngineError::Other("Sheet list files are not available on the web.".into()))
}

impl Interactive for PublishM {
    fn name(&self) -> &'static str {
        "-PUBLISH"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match self.sheets {
            None => Prompt::new("Enter name of sheet list file", Accept::WORD).kw(&[CURRENT]).default(CURRENT),
            Some(_) => Prompt::new("Enter name of output file", Accept::WORD).default(default_file(s, "", "pdf")).picker(picker("pdf")),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(sheets) = &self.sheets else {
            self.sheets = Some(match &i {
                Input::Text(t) => read_sheet_list(t.trim().trim_matches('"'))?,
                _ => Vec::new(),
            });
            return Ok(Step::Continue);
        };
        let path = typed_file(&i, "pdf").unwrap_or_else(|| default_file(s, "", "pdf"));
        let mut p = json!({ "path": path });
        if !sheets.is_empty() {
            p["layouts"] = json!(sheets);
        }
        let r = super::run_publish(s, &p)?;
        if let Some(m) = r.get("message").and_then(Value::as_str) {
            s.echo(m.to_string());
        }
        Ok(Step::Done)
    }
}
