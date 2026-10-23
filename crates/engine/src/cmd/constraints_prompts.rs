//! -PARAMETERS (-PAR) and -CONSTRAINTSETTINGS typed at the command line (the app opens the
//! Parameters Manager and Constraint Settings dialogs for the bare names). Every answer runs
//! the JSON form, so validation matches it and the command is one undo step.

use std::collections::VecDeque;

use cadcraft_constraints as cs;
use serde_json::{Value, json};

use super::super::qselect::wildcard;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

const KW: Accept = Accept { point: false, number: false, text: false, select: false, enter: true };

fn typed(i: &Input) -> Option<String> {
    match i {
        Input::Text(t) | Input::Keyword(t) => Some(t.trim().to_string()).filter(|t| !t.is_empty()),
        _ => None,
    }
}

fn need(t: Option<String>, what: &str) -> Result<String> {
    t.ok_or_else(|| EngineError::Other(format!("Requires {what}.")))
}

/// A parameter of the drawing called `n`, as written there.
fn existing(s: &Session, n: &str) -> Result<String> {
    if cs::name_in_use(s.doc()?, n) { Ok(n.to_string()) } else { Err(EngineError::Other(format!("Parameter \"{n}\" not found."))) }
}

#[derive(Clone, Debug, Default)]
enum Stage {
    #[default]
    Option,
    NewName,
    NewExpr(String),
    EditName,
    EditExpr(String),
    RenameFrom,
    RenameTo(String),
    Delete,
    List,
}

/// -PARAMETERS: New, Edit, Rename, Delete or list (`?`) a parameter.
#[derive(Default)]
pub(super) struct ParametersM {
    stage: Stage,
}

fn next_user(s: &Session) -> String {
    let d = s.doc().ok();
    (1..=100_000).map(|i| format!("user{i}")).find(|n| d.is_none_or(|d| !cs::name_in_use(d, n))).unwrap_or_else(|| "user".into())
}

fn run(s: &mut Session, p: Value) -> Result<Step> {
    super::run_parameters(s, &p)?;
    Ok(Step::Done)
}

impl Interactive for ParametersM {
    fn name(&self) -> &'static str {
        "-PARAMETERS"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match &self.stage {
            Stage::Option => Prompt::new("Enter a parameters option", KW).kw(&["New", "Edit", "Rename", "Delete", "?"]),
            Stage::NewName => Prompt::new("Enter name for new parameter", Accept::WORD).default(next_user(s)),
            Stage::NewExpr(_) => Prompt::new("Enter expression for new parameter", Accept::TEXT),
            Stage::EditName => Prompt::new("Enter name of parameter to edit", Accept::WORD),
            Stage::EditExpr(n) => {
                let cur = s.doc().ok().and_then(|d| cs::parameter_table(d).into_iter().find(|r| &r.name == n)).map(|r| r.expr).unwrap_or_default();
                Prompt::new("Enter new expression for parameter", Accept::TEXT).default(cur)
            }
            Stage::RenameFrom => Prompt::new("Enter name of parameter to rename", Accept::WORD),
            Stage::RenameTo(_) => Prompt::new("Enter new name for parameter", Accept::WORD),
            Stage::Delete => Prompt::new("Enter name of parameter to delete", Accept::WORD),
            Stage::List => Prompt::new("Enter parameter(s) to list", Accept::TEXT).default("*"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let t = typed(&i);
        self.stage = match self.stage.clone() {
            Stage::Option => match i {
                Input::Enter => return Ok(Step::Done),
                Input::Keyword(k) => match k.as_str() {
                    "New" => Stage::NewName,
                    "Edit" => Stage::EditName,
                    "Rename" => Stage::RenameFrom,
                    "Delete" => Stage::Delete,
                    _ => Stage::List,
                },
                _ => return Err(EngineError::Other("Invalid option keyword.".into())),
            },
            Stage::NewName => {
                let n = t.unwrap_or_else(|| next_user(s));
                if cs::name_in_use(s.doc()?, &n) {
                    return Err(EngineError::Other(format!("Parameter \"{n}\" already exists.")));
                }
                if !cs::expr::valid_name(&n) {
                    return Err(EngineError::Other(format!("\"{n}\" is not a valid parameter name.")));
                }
                Stage::NewExpr(n)
            }
            Stage::NewExpr(n) => return run(s, json!({ "name": n, "expr": need(t, "an expression")? })),
            Stage::EditName => Stage::EditExpr(existing(s, &need(t, "a parameter name")?)?),
            Stage::EditExpr(n) => match t {
                Some(e) => return run(s, json!({ "name": n, "expr": e })),
                None => return Ok(Step::Done),
            },
            Stage::RenameFrom => Stage::RenameTo(existing(s, &need(t, "a parameter name")?)?),
            Stage::RenameTo(n) => return run(s, json!({ "rename": n, "to": need(t, "a parameter name")? })),
            Stage::Delete => return run(s, json!({ "delete": existing(s, &need(t, "a parameter name")?)? })),
            Stage::List => {
                let pat = t.unwrap_or_else(|| "*".into());
                let rows: Vec<cs::ParamRow> = cs::parameter_table(s.doc()?).into_iter().filter(|r| wildcard(&pat, &r.name)).collect();
                if rows.is_empty() {
                    s.echo("No matching parameters.");
                }
                for r in rows.iter().take(10_000) {
                    let v = r.value.map(cs::format_value).unwrap_or_else(|| "error".into());
                    s.echo(format!("{} = {} = {v}", r.name, r.expr));
                }
                return Ok(Step::Done);
            }
        };
        Ok(Step::Continue)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum C {
    Option,
    Infer,
    Bars,
    Transparency,
    Dims,
    DistTol,
    AngTol,
}

/// -CONSTRAINTSETTINGS: the Constraint Settings dialog's Geometric, Dimensional and
/// AutoConstrain options, one question each, until eXit.
pub(super) struct SettingsM {
    queue: VecDeque<C>,
}

impl Default for SettingsM {
    fn default() -> Self {
        SettingsM { queue: VecDeque::from([C::Option]) }
    }
}

fn yn(b: bool) -> &'static str {
    if b { "Yes" } else { "No" }
}

fn number(i: &Input, lo: f64, hi: f64) -> Result<Option<f64>> {
    match i {
        Input::Enter => Ok(None),
        Input::Text(t) => match t.trim().parse::<f64>() {
            Ok(v) if v.is_finite() && (lo..=hi).contains(&v) => Ok(Some(v)),
            _ => Err(EngineError::Other(format!("Requires a number between {lo} and {hi}."))),
        },
        _ => Err(EngineError::Other(format!("Requires a number between {lo} and {hi}."))),
    }
}

impl Interactive for SettingsM {
    fn name(&self) -> &'static str {
        "-CONSTRAINTSETTINGS"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let set = s.doc().map(|d| d.parametric.settings.clone()).unwrap_or_default();
        let yes_no = |m: &str, b: bool| Prompt::new(m, KW).kw(&["Yes", "No"]).default(yn(b));
        match self.queue.front().copied().unwrap_or(C::Option) {
            C::Option => Prompt::new("Enter an option", KW).kw(&["Geometric", "Dimensional", "Autoconstrain", "eXit"]).default("eXit"),
            C::Infer => yes_no("Infer geometric constraints?", set.infer),
            C::Bars => yes_no("Show constraint bars?", set.bars_visible),
            C::Transparency => Prompt::new("Enter constraint bar transparency (0-90)", Accept::NUMBER).default(set.bar_transparency.to_string()),
            C::Dims => yes_no("Show dynamic constraints?", set.dims_visible),
            C::DistTol => Prompt::new("Enter distance tolerance", Accept::NUMBER).default(cs::format_value(set.distance_tolerance)),
            C::AngTol => Prompt::new("Enter angle tolerance", Accept::NUMBER).default(cs::format_value(set.angle_tolerance)),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let q = self.queue.front().copied().unwrap_or(C::Option);
        let yes = || match &i {
            Input::Keyword(k) => Ok(Some(k == "Yes")),
            Input::Enter => Ok(None),
            _ => Err(EngineError::Other("Invalid option keyword.".into())),
        };
        let set: Option<(&str, Value)> = match q {
            C::Option => {
                let next: &[C] = match &i {
                    Input::Keyword(k) if k == "Geometric" => &[C::Infer, C::Bars, C::Transparency],
                    Input::Keyword(k) if k == "Dimensional" => &[C::Dims],
                    Input::Keyword(k) if k == "Autoconstrain" => &[C::DistTol, C::AngTol],
                    Input::Keyword(_) | Input::Enter => return Ok(Step::Done),
                    _ => return Err(EngineError::Other("Invalid option keyword.".into())),
                };
                // The asked questions, then the option prompt again.
                self.queue = next.iter().copied().chain([C::Option]).collect();
                return Ok(Step::Continue);
            }
            C::Infer => yes()?.map(|v| ("infer", json!(v))),
            C::Bars => yes()?.map(|v| ("barsVisible", json!(v))),
            C::Dims => yes()?.map(|v| ("dimsVisible", json!(v))),
            C::Transparency => number(&i, 0.0, 90.0)?.map(|v| ("barTransparency", json!(v))),
            C::DistTol => number(&i, 0.0, 1e6)?.map(|v| ("distanceTolerance", json!(v))),
            C::AngTol => number(&i, 0.0, 45.0)?.map(|v| ("angleTolerance", json!(v))),
        };
        if let Some((k, v)) = set {
            super::run_constraintsettings(s, &json!({ k: v }))?;
        }
        self.queue.pop_front();
        Ok(Step::Continue)
    }
}
