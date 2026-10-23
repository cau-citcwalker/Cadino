//! DWGPROPS: the drawing's summary information — title, subject, author, keywords, comments,
//! last saved by, revision number, hyperlink base and custom properties. It is kept as AutoCAD
//! keeps it in a DXF file: an XRECORD under the key `DWGPROPS` of the named object dictionary,
//! starting with the group `1 DWGPROPS COOKIE` (2 title, 3 subject, 4 author, 6 comments,
//! 7 keywords, 8 last saved by, 9 revision number, 300–309 custom `name=value`, 40–42 times,
//! then `1` the hyperlink base). The record travels with the file's other kept objects
//! ([`cadcraft_doc::foreign`]), so reading and saving a DXF keeps it, and a drawing that had none
//! gets one.

use std::sync::Arc;

use cadcraft_doc::foreign::ForeignObject;
use cadcraft_doc::{Drawing, RawTag};
use serde_json::{Map, Value, json};

use super::curves::KW;
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

const KEY: &str = "DWGPROPS";
const COOKIE: &str = "DWGPROPS COOKIE";
/// Custom properties the record holds (groups 300–309).
// ponytail: AutoCAD 2004 and later keep more in the DWG's summary section and $CUSTOMPROPERTY
// header pairs; ten fit the XRECORD every version reads.
const MAX_CUSTOM: usize = 10;
/// Longest value kept (DXF strings).
const MAX_LEN: usize = 2048;

/// The summary fields: JSON key, XRECORD group, name.
const FIELDS: &[(&str, i32, &str)] = &[
    ("title", 2, "Title"),
    ("subject", 3, "Subject"),
    ("author", 4, "Author"),
    ("comments", 6, "Comments"),
    ("keywords", 7, "Keywords"),
    ("lastSavedBy", 8, "Last saved by"),
    ("revisionNumber", 9, "Revision number"),
];

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("dwgprops", "Drawing Properties...", run)
            .menu(&["File", "Drawing Properties..."])
            .params("{title?, subject?, author?, keywords?, comments?, lastSavedBy?, revisionNumber?, hyperlinkBase?, custom?: {name: value} (replaces the custom properties; at most 10)} → the drawing's properties (no parameters: just reads them)")
            .interactive(|_| Ok(Box::new(DwgpropsM::default()))),
    ]
}

fn other(m: impl Into<String>) -> EngineError {
    EngineError::Other(m.into())
}

/// The drawing's summary information.
#[derive(Clone, Debug, Default, PartialEq)]
struct Props {
    /// By group code (2, 3, 4, 6, 7, 8, 9).
    fields: Vec<(i32, String)>,
    hyperlink: String,
    custom: Vec<(String, String)>,
}

impl Props {
    fn get(&self, code: i32) -> &str {
        self.fields.iter().find(|(c, _)| *c == code).map_or("", |(_, v)| v.as_str())
    }
    fn set(&mut self, code: i32, v: String) {
        self.fields.retain(|(c, _)| *c != code);
        self.fields.push((code, v));
    }
    fn json(&self) -> Value {
        let mut o = Map::new();
        for (k, code, _) in FIELDS {
            o.insert((*k).into(), json!(self.get(*code)));
        }
        o.insert("hyperlinkBase".into(), json!(self.hyperlink));
        o.insert("custom".into(), Value::Object(self.custom.iter().map(|(k, v)| (k.clone(), json!(v))).collect()));
        Value::Object(o)
    }
}

/// The DWGPROPS record of the drawing's kept objects, if it has one.
fn record(d: &Drawing) -> Option<&ForeignObject> {
    let h = d.foreign.dictionary.iter().find(|(k, _)| k.eq_ignore_ascii_case(KEY)).map(|(_, h)| *h)?;
    d.foreign.objects.iter().find(|o| o.handle == h && o.kind.eq_ignore_ascii_case("XRECORD"))
}

/// The record's data: the groups from the cookie on.
fn data(o: &ForeignObject) -> &[RawTag] {
    let start = o.tags.iter().position(|t| t.code == 1 && t.value.trim() == COOKIE).unwrap_or(o.tags.len());
    o.tags.get(start + 1..).unwrap_or(&[])
}

fn read(d: &Drawing) -> Props {
    let mut p = Props::default();
    let Some(o) = record(d) else { return p };
    for t in data(o) {
        match t.code {
            2 | 3 | 4 | 6 | 7 | 8 | 9 => p.set(t.code, t.value.clone()),
            300..=309 => {
                if let Some((n, v)) = t.value.split_once('=')
                    && !n.trim().is_empty()
                {
                    p.custom.push((n.to_string(), v.to_string()));
                }
            }
            1 if p.hyperlink.is_empty() => p.hyperlink.clone_from(&t.value),
            _ => {}
        }
    }
    p
}

fn tag(code: i32, value: impl Into<String>) -> RawTag {
    RawTag { code, value: value.into() }
}

/// Store `p` in the drawing's DWGPROPS record (made, with its dictionary entry, when missing).
fn write(d: &mut Drawing, p: &Props) {
    let old: Vec<RawTag> = record(d).map(|o| data(o).to_vec()).unwrap_or_default();
    let mut groups = vec![tag(1, COOKIE)];
    groups.extend([2, 3, 4, 6, 7, 8, 9].map(|c| tag(c, p.get(c))));
    for i in 0..MAX_CUSTOM {
        let v = p.custom.get(i).map_or_else(|| "=".to_string(), |(n, v)| format!("{n}={v}"));
        groups.push(tag(300 + i as i32, v));
    }
    // The times AutoCAD keeps (editing time, created, updated) stay as they were.
    groups.extend(old.iter().filter(|t| (40..=42).contains(&t.code)).cloned());
    groups.push(tag(1, p.hyperlink.clone()));
    // Whatever followed the hyperlink base in the record read.
    if let Some(i) = old.iter().position(|t| t.code == 1) {
        groups.extend(old.iter().skip(i + 1).cloned());
    }
    let existing = record(d).map(|o| o.handle);
    let (xh, nod) = match existing {
        Some(h) => (h, None),
        None => {
            let nod = d.foreign.targets.iter().find(|(_, k)| k.0 == "NOD" && k.1.is_empty()).map(|(h, _)| *h);
            let nod = nod.unwrap_or_else(|| d.new_handle());
            (d.new_handle(), Some(nod))
        }
    };
    let f = Arc::make_mut(&mut d.foreign);
    match nod {
        None => {
            if let Some(o) = f.objects.iter_mut().find(|o| o.handle == xh) {
                let keep = o.tags.iter().position(|t| t.code == 1 && t.value.trim() == COOKIE).unwrap_or(o.tags.len());
                o.tags.truncate(keep);
                o.tags.extend(groups);
            }
        }
        Some(nod) => {
            let n = nod.hex();
            let mut tags = vec![
                tag(5, xh.hex()),
                tag(102, "{ACAD_REACTORS"),
                tag(330, n.clone()),
                tag(102, "}"),
                tag(330, n),
                tag(100, "AcDbXrecord"),
                tag(280, "1"),
            ];
            tags.extend(groups);
            f.targets.insert(nod, ("NOD".into(), String::new()));
            f.objects.push(ForeignObject { handle: xh, kind: "XRECORD".into(), tags });
            f.dictionary.push((KEY.into(), xh));
        }
    }
}

fn text(cmd: &str, v: &Value, key: &str) -> Result<String> {
    let t = match v {
        Value::String(t) => t.clone(),
        Value::Number(n) => n.to_string(),
        _ => return Err(bad(cmd, format!("`{key}` must be text"))),
    };
    if t.len() > MAX_LEN || t.contains(['\n', '\r']) {
        return Err(bad(cmd, format!("`{key}` must be one line of at most {MAX_LEN} bytes")));
    }
    Ok(t)
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "dwgprops";
    let mut props = read(s.doc()?);
    let before = props.clone();
    for (k, code, _) in FIELDS {
        if let Some(v) = p.get(*k).filter(|v| !v.is_null()) {
            props.set(*code, text(CMD, v, k)?);
        }
    }
    if let Some(v) = p.get("hyperlinkBase").filter(|v| !v.is_null()) {
        props.hyperlink = text(CMD, v, "hyperlinkBase")?;
    }
    if let Some(c) = p.get("custom").filter(|v| !v.is_null()) {
        let c = c.as_object().ok_or_else(|| bad(CMD, "`custom` must be {name: value}"))?;
        if c.len() > MAX_CUSTOM {
            return Err(bad(CMD, format!("at most {MAX_CUSTOM} custom properties")));
        }
        props.custom = c
            .iter()
            .map(|(n, v)| {
                let n = n.trim();
                if n.is_empty() || n.contains('=') || n.len() > 255 {
                    return Err(bad(CMD, format!("\"{n}\" is not a custom property name")));
                }
                Ok((n.to_string(), text(CMD, v, n)?))
            })
            .collect::<Result<_>>()?;
    }
    if props != before {
        write(s.doc_mut()?, &props);
    }
    Ok(props.json())
}

#[derive(Clone, Debug, Default, PartialEq)]
enum Ask {
    #[default]
    Option,
    Field(&'static str),
    CustomName,
    CustomValue(String),
}

const OPTIONS: &[(&str, &str)] = &[
    ("Title", "title"),
    ("Subject", "subject"),
    ("Author", "author"),
    ("Keywords", "keywords"),
    ("COmments", "comments"),
    ("Hyperlink base", "hyperlinkBase"),
];

/// DWGPROPS at the command line where no dialog opens (scripts, the CLI; the app opens the
/// Drawing Properties dialog): field after field until Enter.
#[derive(Default)]
struct DwgpropsM {
    ask: Ask,
}

impl DwgpropsM {
    fn current(s: &Session, key: &str) -> String {
        let p = s.doc().map(read).unwrap_or_default();
        match key {
            "hyperlinkBase" => p.hyperlink,
            k => FIELDS.iter().find(|(f, _, _)| *f == k).map_or_else(String::new, |(_, c, _)| p.get(*c).to_string()),
        }
    }
}

impl Interactive for DwgpropsM {
    fn name(&self) -> &'static str {
        "DWGPROPS"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let p = read(s.doc()?);
        for (_, code, label) in FIELDS {
            if !p.get(*code).is_empty() {
                s.echo(format!("{label}: {}", p.get(*code)));
            }
        }
        for (n, v) in &p.custom {
            s.echo(format!("{n}: {v}"));
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match &self.ask {
            Ask::Option => {
                let mut kw: Vec<&str> = OPTIONS.iter().map(|(k, _)| *k).collect();
                kw.push("CUstom");
                Prompt::new("Enter an option", KW).kw(&kw).default("eXit")
            }
            Ask::Field(k) => {
                let label = OPTIONS.iter().find(|(_, key)| key == k).map_or("value", |(l, _)| l).to_ascii_lowercase();
                Prompt::new(format!("Enter {label}"), Accept::TEXT).default(Self::current(s, k))
            }
            Ask::CustomName => Prompt::new("Enter custom property name", Accept::TEXT),
            Ask::CustomValue(n) => {
                let cur = s.doc().map(read).unwrap_or_default().custom.into_iter().find(|(k, _)| k == n).map(|(_, v)| v).unwrap_or_default();
                Prompt::new(format!("Enter value for {n}"), Accept::TEXT).default(cur)
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        match (self.ask.clone(), i) {
            (Ask::Option, Input::Keyword(k)) if k == "CUstom" => self.ask = Ask::CustomName,
            (Ask::Option, Input::Keyword(k)) => {
                if let Some((_, key)) = OPTIONS.iter().find(|(l, _)| *l == k) {
                    self.ask = Ask::Field(key);
                }
            }
            (Ask::Option, Input::Enter) => return Ok(Step::Done),
            (Ask::Field(k), Input::Text(t)) => {
                let mut q = Map::new();
                q.insert(k.into(), json!(t));
                run(s, &Value::Object(q))?;
                self.ask = Ask::Option;
            }
            (Ask::CustomName, Input::Text(t)) => {
                let n = t.trim().to_string();
                if n.is_empty() || n.contains('=') {
                    return Err(other("Invalid custom property name."));
                }
                self.ask = Ask::CustomValue(n);
            }
            (Ask::CustomValue(n), Input::Text(t)) => {
                let mut custom: Map<String, Value> = read(s.doc()?).custom.into_iter().map(|(k, v)| (k, json!(v))).collect();
                custom.insert(n, json!(t));
                run(s, &json!({ "custom": custom }))?;
                self.ask = Ask::Option;
            }
            (_, Input::Enter) => self.ask = Ask::Option,
            (Ask::Option, Input::Text(_)) => return Err(other("Invalid option keyword.")),
            _ => {}
        }
        Ok(Step::Continue)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::Session;

    #[test]
    fn dwgprops_set_read_and_round_trip_through_dxf() {
        let mut s = Session::new();
        let r = s.execute("dwgprops", &json!({})).unwrap();
        assert_eq!(r["title"], "");
        s.execute("dwgprops", &json!({"title": "Kitchen", "author": "Kim", "keywords": "oak, cabinet", "custom": {"Client": "Lee", "Job": 42}}))
            .unwrap();
        let r = s.execute("dwgprops", &json!({"subject": "Units"})).unwrap();
        assert_eq!((r["title"].as_str(), r["subject"].as_str(), r["custom"]["Job"].as_str()), (Some("Kitchen"), Some("Units"), Some("42")));
        for bad in [json!({"title": ["x"]}), json!({"custom": {"a=b": "x"}}), json!({"comments": "two\nlines"})] {
            assert!(s.execute("dwgprops", &bad).is_err(), "{bad}");
        }
        // Saved and read back as AutoCAD keeps it: an XRECORD named DWGPROPS.
        let dxf = cadcraft_io::write_dxf(s.doc().unwrap());
        assert!(dxf.contains("DWGPROPS COOKIE") && dxf.matches("DWGPROPS").count() >= 2, "no DWGPROPS record");
        let back = cadcraft_io::read_dxf(dxf.as_bytes()).unwrap();
        let mut s2 = Session::new();
        s2.open_drawing(back, "back", None);
        let r = s2.execute("dwgprops", &json!({})).unwrap();
        assert_eq!(r["title"], "Kitchen");
        assert_eq!(r["author"], "Kim");
        assert_eq!(r["custom"]["Client"], "Lee");
        // Changed after reading: the same record is updated, not doubled.
        s2.execute("dwgprops", &json!({"title": "Bath"})).unwrap();
        let dxf2 = cadcraft_io::write_dxf(s2.doc().unwrap());
        assert_eq!(dxf2.matches("DWGPROPS COOKIE").count(), 1);
        let back2 = cadcraft_io::read_dxf(dxf2.as_bytes()).unwrap();
        let mut s3 = Session::new();
        s3.open_drawing(back2, "back2", None);
        assert_eq!(s3.execute("dwgprops", &json!({})).unwrap()["title"], "Bath");
    }

    #[test]
    fn dwgprops_typed_sets_fields() {
        let mut s = Session::new();
        s.cmdline("dwgprops").unwrap();
        assert!(
            s.prompt_text().ends_with("Enter an option [Title/Subject/Author/Keywords/COmments/Hyperlink base/CUstom] <eXit>:"),
            "{}",
            s.prompt_text()
        );
        s.cmdline("t").unwrap();
        s.cmdline("Living room plan").unwrap();
        s.cmdline("cu").unwrap();
        s.cmdline("Phase").unwrap();
        s.cmdline("2").unwrap();
        s.cmdline("").unwrap();
        assert!(s.running.is_none(), "{}", s.prompt_text());
        let r = s.execute("dwgprops", &json!({})).unwrap();
        assert_eq!((r["title"].as_str(), r["custom"]["Phase"].as_str()), (Some("Living room plan"), Some("2")));
    }
}
