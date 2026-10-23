//! WBLOCK and BASE typed at the command line (-WBLOCK's prompts; the app opens the Write Block
//! dialog for a bare WBLOCK). Both end in the JSON form, so validation matches it.

use cadcraft_doc::Handle;
use cadcraft_geom::Vec2;
use serde_json::{Value, json};

use super::super::machines::{SelOutcome, SelectPhase};
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

const EQUAL: &str = "= (block=output file)";
const WHOLE: &str = "* (whole drawing)";

/// -WBLOCK: the output file, then an existing block, `=` (the block named like the file), `*`
/// (the whole drawing) or Enter (a new drawing from a base point and objects, which are then
/// erased from the drawing: OOPS brings them back, as in AutoCAD).
// ponytail: `*` writes model space only.
#[derive(Default)]
pub(super) struct WblockM {
    path: Option<String>,
    /// A file that exists, waiting for "replace it?".
    exists: Option<String>,
    /// Enter at the block prompt: a new drawing from objects.
    objects: bool,
    base: Option<Vec2>,
    sel: SelectPhase,
}

/// The typed file name, with `.dwg` added when it has no extension (as AutoCAD does).
fn output_file(t: &str) -> Option<String> {
    let t = t.trim().trim_matches('"').trim();
    if t.is_empty() {
        return None;
    }
    Some(if std::path::Path::new(t).extension().is_some() { t.to_string() } else { format!("{t}.dwg") })
}

fn exists(path: &str) -> bool {
    cfg!(not(target_arch = "wasm32")) && std::path::Path::new(path).exists()
}

impl WblockM {
    fn write(&self, s: &mut Session, mut p: Value) -> Result<Step> {
        let path = self.path.clone().unwrap_or_default();
        if let Some(o) = p.as_object_mut() {
            o.insert("path".into(), json!(path));
        }
        let r = super::run_wblock(s, &p)?;
        let n = r.get("entities").and_then(Value::as_u64).unwrap_or(0);
        s.echo(format!("Wrote {n} object{} to {path}.", if n == 1 { "" } else { "s" }));
        Ok(Step::Done)
    }
}

impl Interactive for WblockM {
    fn name(&self) -> &'static str {
        "-WBLOCK"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.exists.is_some() {
            return Prompt::new("A drawing with this name already exists. Do you want to replace it?", super::super::curves::KW)
                .kw(&["Yes", "No"])
                .default("N");
        }
        match (&self.path, self.objects, self.base) {
            (None, _, _) => Prompt::new("Enter name of output file", Accept::WORD),
            (Some(_), false, _) => Prompt::new("Enter name of existing block", Accept::TEXT).kw(&[EQUAL, WHOLE]).default("define new drawing"),
            (Some(_), true, None) => Prompt::new("Specify insertion base point", Accept::POINT),
            (Some(_), true, Some(_)) => self.sel.prompt(),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if let Some(path) = self.exists.take() {
            return Ok(match i {
                Input::Keyword(k) if k == "Yes" => {
                    self.path = Some(path);
                    Step::Continue
                }
                _ => Step::Cancel,
            });
        }
        match (&self.path, self.objects, self.base, i) {
            (None, _, _, Input::Text(t)) => match output_file(&t) {
                Some(p) if exists(&p) => {
                    self.exists = Some(p);
                    Ok(Step::Continue)
                }
                Some(p) => {
                    self.path = Some(p);
                    Ok(Step::Continue)
                }
                None => Ok(Step::Cancel),
            },
            (None, _, _, Input::Enter) => Ok(Step::Cancel),
            (Some(_), false, _, Input::Enter) => {
                self.objects = true;
                Ok(Step::Continue)
            }
            (Some(path), false, _, Input::Keyword(k)) if k == EQUAL => {
                let name = std::path::Path::new(path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if s.doc()?.block(&name).is_none() {
                    return Err(EngineError::Other(format!("Block \"{name}\" not found.")));
                }
                self.write(s, json!({ "name": name }))
            }
            (Some(_), false, _, Input::Keyword(k)) if k == WHOLE => {
                let all: Vec<String> = s.doc()?.model.iter().map(|e| e.handle.hex()).collect();
                self.write(s, json!({ "handles": all }))
            }
            (Some(_), false, _, Input::Text(t)) => {
                let name = s.doc()?.block(t.trim()).map(|b| b.name.clone());
                match name {
                    Some(name) => self.write(s, json!({ "name": name })),
                    None => Err(EngineError::Other(format!("Block \"{}\" not found.", t.trim()))),
                }
            }
            (Some(_), true, None, Input::Point(p)) => {
                self.base = Some(p);
                self.sel = SelectPhase::begin(s);
                if self.sel.done {
                    let hs = self.sel.picked.clone();
                    return self.objects_out(s, p, &hs);
                }
                Ok(Step::Continue)
            }
            (Some(_), true, Some(b), i) => match self.sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Cancel),
                SelOutcome::Done(hs) => self.objects_out(s, b, &hs),
            },
            _ => Ok(Step::Continue),
        }
    }
}

impl WblockM {
    fn objects_out(&self, s: &mut Session, base: Vec2, hs: &[Handle]) -> Result<Step> {
        let step = self.write(s, json!({ "handles": hs.iter().map(|h| h.hex()).collect::<Vec<_>>(), "base": [base.x, base.y], "keep": "delete" }));
        s.set_selection(Vec::new());
        step
    }
}

/// BASE: "Enter base point <current>", Enter keeps it.
pub(super) struct BaseM;

impl Interactive for BaseM {
    fn name(&self) -> &'static str {
        "BASE"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let d = s.doc().ok();
        let world = d.and_then(|d| d.header.point("INSBASE")).unwrap_or_default();
        // $INSBASE is a world point; it is shown in the construction plane, as points are typed.
        let p = s.cplane().inverse().map_or(world, |m| m.apply(world));
        let (lu, pr) = d.map_or((2, 4), |d| (d.header.i64("LUNITS", 2), d.header.i64("LUPREC", 4)));
        let f = |v: f64| crate::units::format_distance(v, lu, pr);
        Prompt::new("Enter base point", Accept::POINT).default(format!("{},{},{}", f(p.x), f(p.y), f(p.z)))
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Point(p) => {
                super::run_base(s, &json!({ "at": [p.x, p.y] }))?;
                Ok(Step::Done)
            }
            Input::Enter => Ok(Step::Done),
            Input::Cancel => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
}
