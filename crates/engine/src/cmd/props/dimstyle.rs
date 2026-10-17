//! -DIMSTYLE at the command line: "Enter a dimension style option
//! [ANnotative/Save/Restore/STatus/Variables/Apply/?] <Restore>:". Save, Restore and Apply run
//! the JSON forms (`dimstyle`, `dimstyle.current`, `dimstyle.update`).

use cadcraft_color::Color;
use cadcraft_doc::{DIMVARS, DimStyle, Drawing, EntityKind, Handle};
use serde_json::{Value, json};

use super::super::machines::{SelOutcome, SelectPhase};
use crate::{Accept, Input, Interactive, Prompt, Result, Session, Step};

const KEYWORDS: Accept = Accept { point: false, number: false, text: false, select: false, enter: true };

/// What a style name is wanted for.
#[derive(Clone, Copy, PartialEq)]
enum Use {
    Restore,
    Variables,
}

#[derive(Default)]
enum Stage {
    #[default]
    Option,
    /// ANnotative: "Create annotative dimension style [Yes/No] <Yes>:".
    Annotative,
    /// "Enter name for new dimension style or [?]:"; `Some` makes it annotative or not.
    SaveName(Option<bool>),
    /// "That name is already in use, redefine it?".
    Redefine(String, Option<bool>),
    /// "Enter a dimension style name, [?] or <select dimension>:".
    Name(Use),
    /// "Select dimension:" for Restore or Variables.
    Pick(Use),
    /// "Enter dimension style(s) to list <*>:".
    List,
    /// Apply: "Select objects:".
    Apply(SelectPhase),
}

#[derive(Default)]
pub(super) struct DimStyleM {
    stage: Stage,
}

fn current_name(d: &Drawing) -> String {
    d.header.str("DIMSTYLE", "Standard")
}

/// The current settings: the current style with the DIM* variables set over it.
fn current_settings(d: &Drawing) -> DimStyle {
    d.dim_style(&current_name(d)).cloned().unwrap_or_default().with_overrides(&d.dim_overrides())
}

/// One variable's value as listed: numbers as stored, On/Off, colour names, quoted text.
fn shown(v: &Value) -> String {
    match v {
        Value::Bool(b) => (if *b { "On" } else { "Off" }).into(),
        Value::String(t) => format!("\"{t}\""),
        Value::Object(_) => serde_json::from_value::<Color>(v.clone()).map(Color::name).unwrap_or_else(|_| v.to_string()),
        Value::Number(n) if n.is_f64() => format!("{:.4}", n.as_f64().unwrap_or(0.0)),
        other => other.to_string(),
    }
}

/// Every DIM* variable of `st`, one per line.
fn variables(st: &DimStyle) -> Vec<String> {
    let v = serde_json::to_value(st).unwrap_or(Value::Null);
    DIMVARS.iter().filter_map(|(var, field)| v.get(*field).map(|x| format!("{var:<10}{}", shown(x)))).collect()
}

impl DimStyleM {
    fn list(s: &mut Session, pattern: &str) -> Result<()> {
        let pattern = if pattern.trim().is_empty() { "*" } else { pattern.trim() };
        let names: Vec<String> =
            s.doc()?.dim_styles.iter().filter(|st| super::super::qselect::wildcard(pattern, &st.name)).map(|st| st.name.clone()).collect();
        s.echo("Named dimension styles:");
        for n in names {
            s.echo(format!("  {n}"));
        }
        Ok(())
    }

    /// Save the current settings as style `name` (made current), annotative as asked.
    fn save(s: &mut Session, name: &str, annotative: Option<bool>) -> Result<Step> {
        let mut fields = serde_json::to_value(current_settings(s.doc()?)).unwrap_or(Value::Null);
        if let Some(o) = fields.as_object_mut() {
            o.insert("name".into(), json!(name));
            o.insert("current".into(), json!(true));
            if let Some(a) = annotative {
                o.insert("annotative".into(), json!(a));
            }
        }
        super::run_dimstyle(s, &fields)?;
        Ok(Step::Done)
    }

    /// Restore or list the style `name`.
    fn named(s: &mut Session, purpose: Use, name: &str) -> Result<Step> {
        let Some(st) = s.doc()?.dim_style(name).cloned() else {
            s.echo(format!("Dimension style \"{name}\" not found."));
            return Ok(Step::Continue);
        };
        match purpose {
            Use::Restore => {
                super::run_dimstyle_current(s, &json!({ "name": st.name }))?;
            }
            Use::Variables => {
                s.echo(format!("Dimension style: {}", st.name));
                for l in variables(&st) {
                    s.echo(l);
                }
            }
        }
        Ok(Step::Done)
    }
}

impl Interactive for DimStyleM {
    fn name(&self) -> &'static str {
        "DIMSTYLE"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let d = s.doc()?;
        let msg = format!("Current dimension style: {}  Annotative: {}", current_name(d), if current_settings(d).annotative { "Yes" } else { "No" });
        s.echo(msg);
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match &self.stage {
            Stage::Option => Prompt::new("Enter a dimension style option", KEYWORDS)
                .kw(&["ANnotative", "Save", "Restore", "STatus", "Variables", "Apply", "?"])
                .default("Restore"),
            Stage::Annotative => Prompt::new("Create annotative dimension style", KEYWORDS).kw(&["Yes", "No"]).default("Yes"),
            Stage::SaveName(_) => Prompt::new("Enter name for new dimension style", Accept::TEXT).kw(&["?"]),
            Stage::Redefine(..) => Prompt::new("That name is already in use, redefine it?", KEYWORDS).kw(&["Yes", "No"]).default("No"),
            // `?` arrives as text: as a keyword the prompt would offer it twice.
            Stage::Name(_) => Prompt::new("Enter a dimension style name, [?] or <select dimension>", Accept::TEXT),
            Stage::Pick(_) => Prompt::new("Select dimension", Accept::SELECT),
            Stage::List => Prompt::new("Enter dimension style(s) to list", Accept::TEXT).default("*"),
            Stage::Apply(sel) => sel.prompt(),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        match &mut self.stage {
            Stage::Option => {
                let k = match i {
                    Input::Keyword(k) => k,
                    Input::Enter => "Restore".into(),
                    Input::Text(_) => {
                        s.echo("Invalid option keyword.");
                        return Ok(Step::Continue);
                    }
                    _ => return Ok(Step::Continue),
                };
                self.stage = match k.as_str() {
                    "ANnotative" => Stage::Annotative,
                    "Save" => Stage::SaveName(None),
                    "STatus" => {
                        let st = current_settings(s.doc()?);
                        for l in variables(&st) {
                            s.echo(l);
                        }
                        return Ok(Step::Done);
                    }
                    "Variables" => Stage::Name(Use::Variables),
                    "Apply" => Stage::Apply(SelectPhase::begin(s)),
                    "?" => Stage::List,
                    _ => Stage::Name(Use::Restore),
                };
                // Apply with a pickfirst selection acts on it at once.
                if let Stage::Apply(sel) = &self.stage
                    && sel.done
                {
                    let hs = sel.picked.clone();
                    return apply(s, &hs);
                }
                Ok(Step::Continue)
            }
            Stage::Annotative => {
                let a = !matches!(&i, Input::Keyword(k) if k == "No");
                self.stage = Stage::SaveName(Some(a));
                Ok(Step::Continue)
            }
            Stage::SaveName(anno) => {
                let anno = *anno;
                match i {
                    Input::Keyword(_) => Self::list(s, "*")?,
                    Input::Text(t) if !t.trim().is_empty() && t.trim().len() <= 255 => {
                        let name = t.trim().to_string();
                        match s.doc()?.dim_style(&name).map(|st| st.name.clone()) {
                            Some(existing) => self.stage = Stage::Redefine(existing, anno),
                            None => return Self::save(s, &name, anno),
                        }
                    }
                    Input::Enter => return Ok(Step::Cancel),
                    _ => s.echo("Invalid style name."),
                }
                Ok(Step::Continue)
            }
            Stage::Redefine(name, anno) => match i {
                Input::Keyword(k) if k == "Yes" => {
                    let (name, anno) = (name.clone(), *anno);
                    Self::save(s, &name, anno)
                }
                _ => {
                    let a = *anno;
                    self.stage = Stage::SaveName(a);
                    Ok(Step::Continue)
                }
            },
            Stage::Name(purpose) => {
                let purpose = *purpose;
                match i {
                    Input::Text(t) if t.trim() == "?" => {
                        Self::list(s, "*")?;
                        Ok(Step::Continue)
                    }
                    Input::Text(t) => Self::named(s, purpose, t.trim()),
                    Input::Enter => {
                        self.stage = Stage::Pick(purpose);
                        Ok(Step::Continue)
                    }
                    _ => Ok(Step::Continue),
                }
            }
            Stage::Pick(purpose) => {
                let purpose = *purpose;
                let hs = match i {
                    Input::Pick(hs) => hs,
                    Input::Enter => return Ok(Step::Cancel),
                    _ => return Ok(Step::Continue),
                };
                let d = s.doc()?;
                let style = hs.iter().find_map(|h| match d.entity(*h).map(|e| &e.kind) {
                    Some(EntityKind::Dimension(dm)) => Some(dm.style.clone()),
                    _ => None,
                });
                match style {
                    Some(name) => Self::named(s, purpose, &name),
                    None => {
                        if !hs.is_empty() {
                            s.echo("Object selected is not a dimension.");
                        }
                        Ok(Step::Continue)
                    }
                }
            }
            Stage::List => {
                let pattern = if let Input::Text(t) = &i { t.clone() } else { String::new() };
                Self::list(s, &pattern)?;
                Ok(Step::Done)
            }
            Stage::Apply(sel) => match sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Done),
                SelOutcome::Done(hs) => {
                    let step = apply(s, &hs)?;
                    s.set_selection(Vec::new());
                    Ok(step)
                }
            },
        }
    }
}

/// Apply: the current style and settings to the selected dimensions (DIMSTYLE Apply, as Dimension
/// ▸ Update does).
fn apply(s: &mut Session, hs: &[Handle]) -> Result<Step> {
    let r = super::super::annotate::run_update(s, &json!({ "handles": hs.iter().map(|h| h.hex()).collect::<Vec<_>>() }))?;
    if let Some(n) = r.get("updated").and_then(Value::as_u64) {
        s.echo(format!("{n} dimension(s) updated."));
    }
    Ok(Step::Done)
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::{EntityKind, Handle};
    use serde_json::json;

    use crate::{Input, Session};

    fn dim(s: &mut Session) -> Handle {
        let r = s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [10, 0], "at": [5, 3] })).unwrap();
        Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap()
    }

    fn style_of(s: &Session, h: Handle) -> String {
        match &s.doc().unwrap().entity(h).unwrap().kind {
            EntityKind::Dimension(d) => d.style.clone(),
            other => panic!("{other:?}"),
        }
    }

    /// Typed bare (or as -DIMSTYLE) the command prompts instead of refusing "`name` is required".
    #[test]
    fn dash_dimstyle_saves_restores_lists_and_applies() {
        let mut s = Session::new();
        let a = dim(&mut s);
        s.cmdline("-dimstyle").unwrap();
        assert!(s.log.iter().any(|l| l == "Current dimension style: Standard  Annotative: No"), "{:?}", s.log);
        assert_eq!(s.prompt_text(), "-DIMSTYLE Enter a dimension style option [ANnotative/Save/Restore/STatus/Variables/Apply/?] <Restore>:");
        s.cancel();

        // Save the current settings (with a DIMTXT override) as a new style; it becomes current.
        s.doc_mut().unwrap().header.set_f64("DIMTXT", 5.0);
        s.cmdline("dimstyle s").unwrap();
        assert_eq!(s.prompt_text(), "DIMSTYLE Enter name for new dimension style or [?]:");
        s.cmdline("Big").unwrap();
        assert!(s.running.is_none());
        let d = s.doc().unwrap();
        assert_eq!(d.header.str("DIMSTYLE", ""), "Big");
        assert_eq!(d.dim_style("Big").unwrap().text_height, 5.0);

        // Saving over an existing name asks first.
        s.cmdline("dimstyle s big").unwrap();
        assert!(s.prompt_text().ends_with("redefine it? [Yes/No] <No>:"), "{}", s.prompt_text());
        s.cmdline("y").unwrap();
        assert!(s.running.is_none());

        // Restore by name, by picking a dimension, and Enter's default is Restore.
        s.cmdline("dimstyle r standard").unwrap();
        assert_eq!(s.doc().unwrap().header.str("DIMSTYLE", ""), "Standard");
        s.cmdline("dimstyle").unwrap();
        s.cmdline("").unwrap();
        assert!(s.prompt_text().contains("Enter a dimension style name, [?] or <select dimension>"), "{}", s.prompt_text());
        s.cmdline("nope").unwrap();
        assert!(s.running.is_some() && s.log.last().is_some_and(|l| l.contains("not found")));
        s.cmdline("Big").unwrap();
        assert_eq!(s.doc().unwrap().header.str("DIMSTYLE", ""), "Big");

        // Apply puts the selected dimensions on the current style, dropping their own overrides.
        s.execute("dimoverride", &json!({ "handles": [a.hex()], "DIMASZ": 9 })).unwrap();
        s.cmdline("dimstyle a").unwrap();
        assert!(s.prompt_text().contains("Select objects"), "{}", s.prompt_text());
        s.input(Input::Pick(vec![a])).unwrap();
        s.input(Input::Enter).unwrap();
        assert!(s.running.is_none());
        assert_eq!(style_of(&s, a), "Big");
        assert!(matches!(&s.doc().unwrap().entity(a).unwrap().kind, EntityKind::Dimension(d) if d.overrides.is_empty()));

        // Restore from a picked dimension.
        s.cmdline("dimstyle r standard").unwrap();
        s.cmdline("dimstyle r").unwrap();
        s.cmdline("").unwrap();
        assert!(s.prompt_text().ends_with("Select dimension:"), "{}", s.prompt_text());
        s.input(Input::Pick(vec![a])).unwrap();
        assert_eq!(s.doc().unwrap().header.str("DIMSTYLE", ""), "Big");

        // STatus and Variables list the DIM variables; ? lists the styles.
        let n = s.log.len();
        s.cmdline("dimstyle st").unwrap();
        assert!(s.running.is_none());
        assert!(s.log[n..].iter().any(|l| l.starts_with("DIMTXT") && l.ends_with("5.0000")), "{:?}", &s.log[n..]);
        assert!(s.log[n..].iter().any(|l| l.starts_with("DIMCLRD ")), "{:?}", &s.log[n..]);
        let n = s.log.len();
        s.cmdline("dimstyle v standard").unwrap();
        assert!(s.log[n..].iter().any(|l| l == "Dimension style: Standard"), "{:?}", &s.log[n..]);
        assert!(s.log[n..].iter().any(|l| l.starts_with("DIMTXT") && !l.ends_with("5.0000")), "{:?}", &s.log[n..]);
        s.cmdline("dimstyle ?").unwrap();
        s.cmdline("b*").unwrap();
        assert_eq!(&s.log[s.log.len() - 2..], ["Named dimension styles:", "  Big"]);

        // ANnotative saves an annotative style.
        s.cmdline("dimstyle an").unwrap();
        s.cmdline("").unwrap();
        s.cmdline("Anno").unwrap();
        assert!(s.doc().unwrap().dim_style("Anno").unwrap().annotative);

        // The JSON form is unchanged: it still needs a name.
        assert!(s.execute("dimstyle", &json!({})).is_err());
        assert!(s.running.is_none());
    }
}
