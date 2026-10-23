//! ATTIPEDIT (ATI): one attribute's value, picked in place. ATTEXT and -ATTEXT: block references'
//! attributes written to a file in AutoCAD's comma (CDF) or space (SDF) delimited format, as a
//! template file lays out the fields, or as CSV.

use cadcraft_doc::{Drawing, Entity, EntityKind, Handle, Insert};
use cadcraft_geom::Vec2;
use serde_json::{Value, json};

use super::curves::KW;
use super::machines::{SelOutcome, SelectPhase};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Picker, Prompt, Result, Session, Step};

/// Most fields a template may lay out, most records written, and most bytes in an extract file.
const MAX_FIELDS: usize = 1000;
const MAX_RECORDS: usize = 1_000_000;
const MAX_BYTES: usize = 256 << 20;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("attipedit", "Edit Attribute In Place", run_attipedit)
            .alias(&["ati"])
            .params("{handle (a block reference), tag (any case), value} → {changed}")
            .interactive(|_| Ok(Box::new(AttipeditM::default()))),
        CommandSpec::new("attext", "Attribute Extraction", run_attext)
            .params(
                "{path, format?: cdf|sdf|csv (default: csv for a .csv path, else cdf), template?: path | templateText? | fields?: [\"BL:NAME C031000\", \"TAG N010002\", ...] (AutoCAD's template lines: BL:LEVEL, BL:NAME, BL:X, BL:Y, BL:Z, BL:NUMBER, BL:HANDLE, BL:LAYER, BL:ORIENT, BL:XSCALE, BL:YSCALE, BL:ZSCALE, BL:XEXTRUDE, BL:YEXTRUDE, BL:ZEXTRUDE or an attribute tag; C or N, width, decimals; C:DELIM and C:QUOTE characters; default BL:NAME and every tag), handles? (default: every block reference with attributes)} → {records, path}",
            )
            .interactive(|_| Ok(Box::new(AttextM::default()))),
    ]
}

fn other(m: impl Into<String>) -> EngineError {
    EngineError::Other(m.into())
}

// ---------------- ATTIPEDIT ----------------

fn run_attipedit(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "attipedit";
    let h = super::curves::handle_param(p, "handle").ok_or_else(|| bad(CMD, "`handle` (a block reference) is required"))?;
    let tag = str_param(p, "tag").ok_or_else(|| bad(CMD, "`tag` is required"))?;
    let value = match p.get("value") {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err(bad(CMD, "`value` (text) is required")),
    };
    if super::curves::is_locked(s, h) {
        return Err(other("The object is on a locked layer."));
    }
    let d = s.doc()?;
    let Some(EntityKind::Insert(ins)) = d.entity(h).map(|e| &e.kind) else { return Err(bad(CMD, "`handle` is not a block reference")) };
    let k = ins
        .attribs
        .iter()
        .position(|a| a.tag.eq_ignore_ascii_case(tag.trim()))
        .ok_or_else(|| bad(CMD, format!("the block reference has no attribute `{tag}`")))?;
    let mut changed = false;
    s.doc_mut()?.modify_entity(h, |e| {
        if let EntityKind::Insert(i) = &mut e.kind
            && let Some(a) = i.attribs.get_mut(k)
            && a.text.value != value
        {
            a.text.value = value;
            changed = true;
        }
    })?;
    Ok(json!({ "changed": usize::from(changed) }))
}

/// The visible attribute of a block reference nearest `p`: its handle, tag and value.
fn attribute_at(s: &Session, p: Vec2) -> Option<(Handle, String, String)> {
    let h = super::curves::pick_at(s, p)?;
    let d = s.doc().ok()?;
    let EntityKind::Insert(ins) = &d.entity(h)?.kind else { return None };
    let dist = |b: cadcraft_geom::Bounds2| {
        let q = Vec2::new(p.x.clamp(b.min.x, b.max.x), p.y.clamp(b.min.y, b.max.y));
        q.dist(p)
    };
    ins.attribs
        .iter()
        .filter(|a| !a.invisible)
        .map(|a| (dist(cadcraft_render::place_text_entity(d, &a.text, &a.text.value).1), a))
        .filter(|(dd, _)| dd.is_finite())
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, a)| (h, a.tag.clone(), a.text.value.clone()))
}

/// ATTIPEDIT typed: pick an attribute, then its new value.
// ponytail: the value is typed at the command line; AutoCAD opens an in-place editor on it.
#[derive(Default)]
struct AttipeditM {
    picked: Option<(Handle, String, String)>,
}

impl Interactive for AttipeditM {
    fn name(&self) -> &'static str {
        "ATTIPEDIT"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match &self.picked {
            None => Prompt::new("Select attribute to edit", Accept::POINT),
            Some((_, tag, v)) => Prompt::new(format!("Enter new value for {tag}"), Accept::TEXT).default(v.clone()),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match (&self.picked, i) {
            (_, Input::Cancel) => Ok(Step::Cancel),
            (None, Input::Point(p)) => {
                self.picked = Some(attribute_at(s, p).ok_or_else(|| other("Select a block reference with attributes."))?);
                Ok(Step::Continue)
            }
            (None, Input::Enter) | (Some(_), Input::Enter) => Ok(Step::Done),
            (Some((h, tag, _)), Input::Text(t)) => {
                run_attipedit(s, &json!({ "handle": h.hex(), "tag": tag, "value": t }))?;
                Ok(Step::Done)
            }
            _ => Ok(Step::Continue),
        }
    }
}

// ---------------- ATTEXT ----------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Cdf,
    Sdf,
    Csv,
}

/// A template field: its name (upper case), numeric or character, width and decimals.
#[derive(Clone, Debug, PartialEq)]
struct Field {
    name: String,
    numeric: bool,
    width: usize,
    decimals: usize,
}

/// A template's fields and its CDF delimiter and quote.
#[derive(Clone, Debug, PartialEq)]
struct Template {
    fields: Vec<Field>,
    delim: char,
    quote: char,
}

/// Parse template lines (`NAME Cwwwddd`, `NAME Nwwwddd`, `C:DELIM x`, `C:QUOTE x`).
fn template<'a>(lines: impl IntoIterator<Item = &'a str>) -> Result<Template> {
    let mut t = Template { fields: Vec::new(), delim: ',', quote: '\'' };
    for (n, line) in lines.into_iter().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (name, spec) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let (name, spec) = (name.to_ascii_uppercase(), spec.trim());
        match name.as_str() {
            "C:DELIM" | "C:QUOTE" => {
                let c = spec.chars().next().ok_or_else(|| other(format!("Template line {}: {name} needs a character.", n + 1)))?;
                if name == "C:DELIM" {
                    t.delim = c;
                } else {
                    t.quote = c;
                }
                continue;
            }
            _ => {}
        }
        let bad_line = || other(format!("Template line {}: \"{line}\" is not NAME Cwwwddd or NAME Nwwwddd.", n + 1));
        let mut cs = spec.chars();
        let numeric = match cs.next().map(|c| c.to_ascii_uppercase()) {
            Some('N') => true,
            Some('C') => false,
            _ => return Err(bad_line()),
        };
        let digits: String = cs.collect();
        if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_digit()) {
            return Err(bad_line());
        }
        let width: usize = digits.get(..3).and_then(|w| w.parse().ok()).ok_or_else(bad_line)?;
        let decimals: usize = digits.get(3..).and_then(|w| w.parse().ok()).ok_or_else(bad_line)?;
        if t.fields.len() >= MAX_FIELDS {
            return Err(other(format!("A template has at most {MAX_FIELDS} fields.")));
        }
        t.fields.push(Field { name, numeric, width, decimals });
    }
    if t.fields.is_empty() {
        return Err(other("The template lays out no fields."));
    }
    Ok(t)
}

/// The block references to extract from: `handles` when given, else every one with attributes
/// in model space and the layouts.
fn references(s: &Session, p: &Value) -> Result<Vec<Entity>> {
    let d = s.doc()?;
    let given = p.get("handles").is_some() || p.get("handle").is_some();
    let attributed = |e: &&std::sync::Arc<Entity>| matches!(&e.kind, EntityKind::Insert(i) if !i.attribs.is_empty());
    Ok(if given {
        targets(s, p)?.iter().filter_map(|h| d.entity(*h)).filter(attributed).take(MAX_RECORDS).map(|e| (**e).clone()).collect()
    } else {
        std::iter::once(&d.model)
            .chain(d.layouts.iter().map(|l| &l.entities))
            .flat_map(|st| st.iter())
            .filter(attributed)
            .take(MAX_RECORDS)
            .map(|e| (**e).clone())
            .collect()
    })
}

/// One field of one record, as text before formatting (numbers as numbers).
enum Cell {
    Text(String),
    Num(f64),
}

fn cell(d: &Drawing, e: &Entity, ins: &Insert, number: usize, f: &Field) -> Cell {
    let deg = |a: f64| cadcraft_geom::norm_angle(a).to_degrees();
    match f.name.as_str() {
        "BL:LEVEL" => Cell::Num(1.0),
        "BL:NAME" => Cell::Text(ins.block.clone()),
        "BL:X" => Cell::Num(ins.insert.x),
        "BL:Y" => Cell::Num(ins.insert.y),
        "BL:Z" => Cell::Num(ins.insert.z),
        "BL:NUMBER" => Cell::Num(number as f64),
        "BL:HANDLE" => Cell::Text(e.handle.hex()),
        "BL:LAYER" => Cell::Text(e.common.layer.clone()),
        "BL:ORIENT" => Cell::Num(deg(ins.rotation)),
        "BL:XSCALE" => Cell::Num(ins.scale.x),
        "BL:YSCALE" => Cell::Num(ins.scale.y),
        "BL:ZSCALE" => Cell::Num(ins.scale.z),
        "BL:XEXTRUDE" => Cell::Num(e.common.extrusion.x),
        "BL:YEXTRUDE" => Cell::Num(e.common.extrusion.y),
        "BL:ZEXTRUDE" => Cell::Num(e.common.extrusion.z),
        tag => {
            let v = ins.attribs.iter().find(|a| a.tag.eq_ignore_ascii_case(tag)).map(|a| a.text.value.clone()).or_else(|| {
                // A constant attribute lives in the block definition.
                d.block(&ins.block)?.entities.iter().find_map(|be| match &be.kind {
                    EntityKind::AttDef(a) if a.constant && a.tag.eq_ignore_ascii_case(tag) => Some(a.text.value.clone()),
                    _ => None,
                })
            });
            let v = v.unwrap_or_default();
            if f.numeric { Cell::Num(v.trim().parse::<f64>().ok().filter(|x| x.is_finite()).unwrap_or(0.0)) } else { Cell::Text(v) }
        }
    }
}

fn number_text(v: f64, decimals: usize) -> String {
    format!("{v:.*}", decimals.min(16))
}

fn csv_field(t: &str) -> String {
    if t.contains([',', '"', '\n', '\r']) { format!("\"{}\"", t.replace('"', "\"\"")) } else { t.to_string() }
}

/// The extract file's text, refused past `cap` bytes (a template can ask for 999 characters a
/// field and 1000 fields a record).
fn extract_text(d: &Drawing, refs: &[Entity], t: &Template, format: Format, cap: usize) -> Result<String> {
    let mut out = String::new();
    if format == Format::Csv {
        out.push_str(&t.fields.iter().map(|f| csv_field(&f.name)).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    let sep = match format {
        Format::Sdf => String::new(),
        Format::Cdf => t.delim.to_string(),
        Format::Csv => ",".into(),
    };
    for (n, e) in refs.iter().enumerate() {
        let EntityKind::Insert(ins) = &e.kind else { continue };
        for (k, f) in t.fields.iter().enumerate() {
            if k > 0 {
                out.push_str(&sep);
            }
            let c = cell(d, e, ins, n + 1, f);
            out.push_str(&match (format, c) {
                (Format::Sdf, Cell::Num(v)) => format!("{:>w$}", number_text(v, f.decimals), w = f.width),
                (Format::Sdf, Cell::Text(v)) => {
                    let v: String = v.chars().take(f.width).collect();
                    format!("{v:<w$}", w = f.width)
                }
                (Format::Cdf, Cell::Num(v)) => number_text(v, f.decimals),
                (Format::Cdf, Cell::Text(v)) => format!("{q}{v}{q}", q = t.quote),
                (Format::Csv, Cell::Num(v)) => number_text(v, f.decimals),
                (Format::Csv, Cell::Text(v)) => csv_field(&v),
            });
            if out.len() > cap {
                return Err(other(format!("The extract file would be larger than {} MB; extract fewer objects or fields.", cap >> 20)));
            }
        }
        out.push_str("\r\n");
    }
    Ok(out)
}

fn write_file(path: &str, text: &str) -> Result<()> {
    crate::limits::files("attext")?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::fs::write(path, text).map_err(|e| other(format!("{path}: {e}")))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = text;
        Err(other(format!("{path}: files can't be written in the browser")))
    }
}

fn run_attext(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "attext";
    let path = str_param(p, "path").map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(CMD, "`path` is required"))?.to_string();
    let format = match str_param(p, "format").map(str::to_ascii_lowercase).as_deref() {
        Some("cdf") => Format::Cdf,
        Some("sdf") => Format::Sdf,
        Some("csv") => Format::Csv,
        Some("dxf" | "dxx") => return Err(bad(CMD, "DXF (DXX) extract files are not available yet; use CDF, SDF or CSV")),
        Some(f) => return Err(bad(CMD, format!("`format` must be cdf, sdf or csv, not `{f}`"))),
        None if path.to_ascii_lowercase().ends_with(".csv") => Format::Csv,
        None => Format::Cdf,
    };
    let refs = references(s, p)?;
    let d = s.doc()?;
    let t = if let Some((text, _)) = super::helpers::support_text(CMD, p, "template", "templateText")? {
        template(text.lines())?
    } else if let Some(a) = p.get("fields").and_then(Value::as_array) {
        let lines: Vec<&str> = a.iter().map(|v| v.as_str().ok_or_else(|| bad(CMD, "`fields` must be template lines"))).collect::<Result<_>>()?;
        template(lines)?
    } else {
        // BL:NAME and every tag found, in the order first met.
        let mut lines = vec!["BL:NAME C031000".to_string()];
        for e in &refs {
            if let EntityKind::Insert(i) = &e.kind {
                for a in &i.attribs {
                    let l = format!("{} C080000", a.tag.to_ascii_uppercase());
                    if !lines.contains(&l) && lines.len() < MAX_FIELDS {
                        lines.push(l);
                    }
                }
            }
        }
        template(lines.iter().map(String::as_str))?
    };
    let text = extract_text(d, &refs, &t, format, MAX_BYTES)?;
    write_file(&path, &text)?;
    let n = refs.len();
    Ok(json!({ "records": n, "path": path, "message": format!("{n} record{} in extract file.", if n == 1 { "" } else { "s" }) }))
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum AttextAsk {
    #[default]
    Kind,
    Select,
    Template,
    Output,
}

/// ATTEXT and -ATTEXT typed: the extract format (Objects picks the block references), the
/// template file, then the extract file.
#[derive(Default)]
struct AttextM {
    ask: AttextAsk,
    format: Option<&'static str>,
    sel: SelectPhase,
    picked: Option<Vec<Handle>>,
    template: String,
}

impl Interactive for AttextM {
    fn name(&self) -> &'static str {
        "ATTEXT"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match self.ask {
            AttextAsk::Kind => Prompt::new("Enter extraction type", KW).kw(&["Cdf", "Sdf", "Dxf", "Objects"]).default("C"),
            AttextAsk::Select => self.sel.prompt(),
            AttextAsk::Template => {
                Prompt::new("Enter template file name", Accept::WORD).picker(Picker::File { save: false, label: "Template", exts: &["txt"] })
            }
            AttextAsk::Output => {
                let stem = s.state().map(|st| st.title.clone()).unwrap_or_default();
                let stem = std::path::Path::new(&stem).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or(stem);
                Prompt::new("Enter extract file name", Accept::WORD).default(stem).picker(Picker::File {
                    save: true,
                    label: "Extract",
                    exts: &["txt", "csv"],
                })
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        match (self.ask, i) {
            (AttextAsk::Kind, Input::Keyword(k)) if k == "Objects" => {
                self.sel = SelectPhase::default();
                self.ask = AttextAsk::Select;
            }
            (AttextAsk::Kind, Input::Keyword(k)) if k == "Dxf" => {
                return Err(other("DXF (DXX) extract files are not available yet; choose Cdf or Sdf."));
            }
            (AttextAsk::Kind, Input::Keyword(k)) => {
                self.format = Some(if k == "Sdf" { "sdf" } else { "cdf" });
                self.ask = AttextAsk::Template;
            }
            (AttextAsk::Kind, Input::Enter) => {
                self.format = Some("cdf");
                self.ask = AttextAsk::Template;
            }
            (AttextAsk::Select, i) => match self.sel.feed(s, &i)? {
                SelOutcome::More => {}
                SelOutcome::Empty => self.ask = AttextAsk::Kind,
                SelOutcome::Done(hs) => {
                    self.picked = Some(hs);
                    s.set_selection(Vec::new());
                    self.ask = AttextAsk::Kind;
                }
            },
            (AttextAsk::Template, Input::Text(t)) => {
                self.template = t.trim().trim_matches('"').to_string();
                self.ask = AttextAsk::Output;
            }
            (AttextAsk::Template, Input::Enter) => return Ok(Step::Cancel),
            (AttextAsk::Output, i @ (Input::Text(_) | Input::Enter)) => {
                let typed = match i {
                    Input::Text(t) => t.trim().trim_matches('"').to_string(),
                    _ => String::new(),
                };
                let name = if typed.is_empty() {
                    let st = s.state()?;
                    let base = st.path.clone().unwrap_or_else(|| st.title.clone());
                    std::path::Path::new(&base).with_extension("txt").to_string_lossy().into_owned()
                } else if std::path::Path::new(&typed).extension().is_none() {
                    format!("{typed}.txt")
                } else {
                    typed
                };
                let mut p = json!({ "path": name, "format": self.format.unwrap_or("cdf"), "template": self.template });
                if let (Some(o), Some(hs)) = (p.as_object_mut(), &self.picked) {
                    o.insert("handles".into(), json!(hs.iter().map(|h| h.hex()).collect::<Vec<_>>()));
                }
                let r = run_attext(s, &p)?;
                s.echo(r["message"].as_str().unwrap_or_default().to_string());
                return Ok(Step::Done);
            }
            (_, Input::Text(_)) => return Err(other("Invalid option keyword.")),
            _ => {}
        }
        Ok(Step::Continue)
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::{EntityKind, Handle};
    use cadcraft_geom::Vec2;
    use serde_json::json;

    use crate::{Input, Session};

    /// Two references of a block with TAG and QTY attributes.
    fn drawing() -> (Session, Handle, Handle) {
        let mut s = Session::new();
        let c = s.execute("circle", &json!({"center": [0, 0], "radius": 1})).unwrap();
        let t = s.execute("attdef", &json!({"tag": "TAG", "at": [0, 2], "default": "D1", "height": 1})).unwrap();
        let q = s.execute("attdef", &json!({"tag": "QTY", "at": [0, -3], "default": "2", "height": 1})).unwrap();
        let hs = [&c, &t, &q].map(|v| v["handle"].as_str().unwrap().to_string());
        s.execute("block", &json!({"name": "Door", "base": [0, 0], "handles": hs, "keep": "delete"})).unwrap();
        let a = s.execute("insert", &json!({"name": "Door", "at": [10, 5], "attribs": {"TAG": "D1", "QTY": "3"}})).unwrap();
        let b = s.execute("insert", &json!({"name": "Door", "at": [20, 5], "attribs": {"TAG": "D2, wide", "QTY": "x"}})).unwrap();
        let h = |v: serde_json::Value| Handle::parse_hex(v["handle"].as_str().unwrap()).unwrap();
        (s, h(a), h(b))
    }

    fn value(s: &Session, h: Handle, tag: &str) -> String {
        match &s.doc().unwrap().entity(h).unwrap().kind {
            EntityKind::Insert(i) => i.attribs.iter().find(|a| a.tag == tag).unwrap().text.value.clone(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_extract_file_past_the_size_cap_is_refused() {
        let (s, _, _) = drawing();
        let d = s.doc().unwrap();
        let refs = super::references(&s, &json!({})).unwrap();
        let wide = vec!["TAG C999000"; super::MAX_FIELDS];
        let t = super::template(wide).unwrap();
        // 2 records × 1000 fields × 999 characters ≈ 2 MB.
        assert!(super::extract_text(d, &refs, &t, super::Format::Sdf, 1 << 20).is_err());
        assert!(super::extract_text(d, &refs, &t, super::Format::Sdf, 4 << 20).is_ok());
    }

    #[test]
    fn attipedit_sets_one_attribute() {
        let (mut s, a, _) = drawing();
        assert_eq!(s.execute("attipedit", &json!({"handle": a.hex(), "tag": "tag", "value": "D9"})).unwrap()["changed"], 1);
        assert_eq!(value(&s, a, "TAG"), "D9");
        assert!(s.execute("attipedit", &json!({"handle": a.hex(), "tag": "NOPE", "value": "x"})).is_err());
        assert!(s.execute("attipedit", &json!({"handle": a.hex(), "tag": "TAG"})).is_err());

        // Typed as ATI: pick the reference below its circle, nearest the QTY attribute's text.
        s.cmdline("ati").unwrap();
        assert!(s.prompt_text().ends_with("Select attribute to edit:"), "{}", s.prompt_text());
        s.input(Input::Point(Vec2::new(10.0, 4.0))).unwrap();
        assert!(s.prompt_text().ends_with("Enter new value for QTY <3>:"), "{}", s.prompt_text());
        s.cmdline("12 pcs").unwrap();
        assert!(s.running.is_none());
        assert_eq!(value(&s, a, "QTY"), "12 pcs");
    }

    #[test]
    fn attext_writes_cdf_sdf_and_csv() {
        let (mut s, a, _) = drawing();
        let dir = std::env::temp_dir().join(format!("cadcraft-attext-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tpl = "BL:NAME C008000\nBL:X N008002\nTAG C006000\nQTY N004000\n";
        let path = |n: &str| dir.join(n).to_string_lossy().to_string();
        let r = s.execute("attext", &json!({"path": path("out.txt"), "templateText": tpl})).unwrap();
        assert_eq!(r["records"], 2);
        let cdf = std::fs::read_to_string(path("out.txt")).unwrap();
        assert_eq!(cdf, "'Door',10.00,'D1',3\r\n'Door',20.00,'D2, wide',0\r\n");
        s.execute("attext", &json!({"path": path("out.sdf"), "format": "sdf", "templateText": tpl, "handles": [a.hex()]})).unwrap();
        assert_eq!(std::fs::read_to_string(path("out.sdf")).unwrap(), "Door       10.00D1       3\r\n");
        s.execute("attext", &json!({"path": path("out.csv"), "templateText": "C:DELIM ;\nBL:NAME C008000\nTAG C010000"})).unwrap();
        assert_eq!(std::fs::read_to_string(path("out.csv")).unwrap(), "BL:NAME,TAG\r\nDoor,D1\r\nDoor,\"D2, wide\"\r\n");
        // Default fields: the block name and every tag.
        s.execute("attext", &json!({"path": path("all.txt")})).unwrap();
        assert!(std::fs::read_to_string(path("all.txt")).unwrap().starts_with("'Door','D1','3'\r\n"));
        for bad in [json!({"path": path("x.txt"), "templateText": "TAG X001000"}), json!({"path": path("x.txt"), "format": "dxf"}), json!({})] {
            assert!(s.execute("attext", &bad).is_err(), "{bad}");
        }
        // The file guard holds.
        {
            let _deny = crate::limits::deny_files("no files here");
            assert!(s.execute("attext", &json!({"path": path("denied.txt")})).is_err());
        }
        assert!(!dir.join("denied.txt").exists());

        // Typed: -ATTEXT with Sdf, a template file and the extract file.
        std::fs::write(path("t.txt"), tpl).unwrap();
        s.cmdline("-attext").unwrap();
        assert!(s.prompt_text().ends_with("Enter extraction type or [Cdf/Sdf/Dxf/Objects] <C>:"), "{}", s.prompt_text());
        s.cmdline("s").unwrap();
        s.cmdline(&path("t.txt")).unwrap();
        s.cmdline(&path("typed")).unwrap();
        assert!(s.running.is_none(), "{}", s.prompt_text());
        assert!(s.log.iter().any(|l| l == "2 records in extract file."), "{:?}", s.log);
        assert!(std::fs::read_to_string(path("typed.txt")).unwrap().starts_with("Door       10.00"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
