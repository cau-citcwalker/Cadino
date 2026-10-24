//! BVSTATE (-BVSTATE), BVSHOW and BVHIDE: a dynamic block's visibility states in the Block
//! Editor. A visibility state is a named set of the block's objects shown together; a reference
//! picks one from its visibility grip or Properties. The editor shows one state at a time (the
//! first is the one new references show); BVSHOW and BVHIDE make objects visible or invisible in
//! it, or in every state. Typed bare, BVSTATE opens the Visibility States dialog (the UI's).

use cadcraft_doc::Handle;
use cadcraft_doc::dynblock::{ParameterKind, author};
use serde_json::{Value, json};

use super::{authored, authoring, authoring_enabled, show_state};
use crate::cmd::curves::KW;
use crate::cmd::machines::{SelOutcome, SelectPhase};
use crate::cmd::{CommandSpec, bad, bool_or, str_param, targets};
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("bvstate", "Visibility States", run_bvstate)
            .menu(&["Tools", "Block Editor", "Visibility States"])
            .alias(&["-bvstate"])
            .params("{operation?: new|set|delete|rename|up|down (default set), name, visibility?: hide|show|current (new: how it shows the objects; default current), newName? (rename)} → {states, current}")
            .enabled(authoring_enabled)
            .interactive(|_| Ok(Box::new(BvstateM::default()))),
        CommandSpec::new("bvshow", "Make Visible", run_bvshow)
            .menu(&["Tools", "Block Editor", "Make Visible"])
            .params("{handles? (default the selection), all?: bool (in every visibility state; default the current one)} → {states, current}")
            .enabled(authoring_enabled)
            .interactive(|_| Ok(Box::new(ShowHideM { show: true, ..ShowHideM::default() }))),
        CommandSpec::new("bvhide", "Make Invisible", run_bvhide)
            .menu(&["Tools", "Block Editor", "Make Invisible"])
            .params("{handles? (default the selection), all?: bool (in every visibility state; default the current one)} → {states, current}")
            .enabled(authoring_enabled)
            .interactive(|_| Ok(Box::new(ShowHideM::default()))),
    ]
}

/// The block's visibility states and the one the editor shows.
fn states(s: &Session) -> Result<(Vec<String>, Option<String>)> {
    let names: Vec<String> = authored(s)?
        .and_then(|m| {
            m.parameters.into_iter().find_map(|p| match p.kind {
                ParameterKind::Visibility { states, .. } => Some(states.into_iter().map(|s| s.name).collect()),
                _ => None,
            })
        })
        .unwrap_or_default();
    let vstate = s.state()?.block_edit.as_ref().and_then(|b| b.vstate.clone());
    let current = vstate.and_then(|v| names.iter().find(|n| n.eq_ignore_ascii_case(&v)).cloned()).or_else(|| names.first().cloned());
    Ok((names, current))
}

/// The editor shows visibility state `name` (`None`: the first).
fn set_current(s: &mut Session, name: Option<String>) -> Result<()> {
    if let Some(be) = s.state_mut()?.block_edit.as_mut() {
        be.vstate = name;
    }
    show_state(s)
}

fn report(s: &Session) -> Result<Value> {
    let (states, current) = states(s)?;
    Ok(json!({ "states": states, "current": current }))
}

/// The editor's objects (what Hide all hides).
fn objects(s: &Session) -> Result<Vec<Handle>> {
    Ok(s.doc()?.model.handles())
}

fn run_bvstate(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "bvstate";
    authoring(s)?;
    let op = str_param(p, "operation").map_or_else(|| "set".to_string(), |o| o.trim().to_ascii_lowercase());
    let name = str_param(p, "name").map(|n| n.trim().to_string());
    let need = |n: &Option<String>| n.clone().filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd, "`name` is required"));
    let (_, current) = states(s)?;
    match op.as_str() {
        "new" => {
            let n = need(&name)?;
            let how = match str_param(p, "visibility").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
                Some("hide" | "hideall" | "hide all") => author::NewState::HideAll,
                Some("show" | "showall" | "show all") => author::NewState::ShowAll,
                None | Some("current" | "current visibility") => author::NewState::Current,
                Some(other) => return Err(bad(cmd, format!("unknown visibility `{other}` (hide, show or current)"))),
            };
            let objs = objects(s)?;
            super::author(s, |d, block| author::add_state(d, block, &n, how, current.as_deref(), &objs))?;
            // The new state is the one the editor shows.
            set_current(s, Some(n))?;
        }
        "set" => {
            let n = need(&name)?;
            let (names, _) = states(s)?;
            let found = names.iter().find(|x| x.eq_ignore_ascii_case(&n)).cloned().ok_or_else(|| bad(cmd, format!("no visibility state \"{n}\"")))?;
            set_current(s, Some(found))?;
        }
        "delete" => {
            let n = need(&name)?;
            super::author(s, |d, block| author::delete_state(d, block, &n))?;
            if current.is_some_and(|c| c.eq_ignore_ascii_case(&n)) {
                set_current(s, None)?;
            }
        }
        "rename" => {
            let n = need(&name)?;
            let new = str_param(p, "newName").map(str::trim).filter(|x| !x.is_empty()).ok_or_else(|| bad(cmd, "`newName` is required"))?.to_string();
            super::author(s, |d, block| author::rename_state(d, block, &n, &new))?;
            if current.is_some_and(|c| c.eq_ignore_ascii_case(&n)) {
                set_current(s, Some(new))?;
            }
        }
        "up" | "down" => {
            let n = need(&name)?;
            let by = if op == "up" { -1 } else { 1 };
            super::author(s, |d, block| author::move_state(d, block, &n, by))?;
        }
        other => return Err(bad(cmd, format!("unknown operation `{other}`"))),
    }
    report(s)
}

fn show_hide(s: &mut Session, p: &Value, show: bool) -> Result<Value> {
    let cmd = if show { "bvshow" } else { "bvhide" };
    let hs: Vec<Handle> = {
        let d = s.doc()?;
        targets(s, p)?.into_iter().filter(|h| d.model.contains(*h)).collect()
    };
    if hs.is_empty() {
        return Err(bad(cmd, "select the objects or give `handles`"));
    }
    let (_, current) = states(s)?;
    let state = if bool_or(p, "all", false) { None } else { current };
    super::author(s, |d, block| author::show_objects(d, block, &hs, show, state.as_deref()))?;
    let n = hs.len();
    s.echo(if show { format!("{n} object(s) made visible.") } else { format!("{n} object(s) made invisible.") });
    report(s)
}

fn run_bvshow(s: &mut Session, p: &Value) -> Result<Value> {
    show_hide(s, p, true)
}

fn run_bvhide(s: &mut Session, p: &Value) -> Result<Value> {
    show_hide(s, p, false)
}

// ---------------- interactive ----------------

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Phase {
    #[default]
    Option,
    Name,
    Visibility,
}

/// -BVSTATE: New, Set or Delete on the command line.
#[derive(Default)]
struct BvstateM {
    op: Option<&'static str>,
    phase: Phase,
    name: String,
}

impl Interactive for BvstateM {
    fn name(&self) -> &'static str {
        "BVSTATE"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if states(s)?.0.is_empty() {
            return Err(EngineError::Other("The block has no visibility parameter: add one first (BPARAMETER Visibility).".into()));
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match self.phase {
            Phase::Option => Prompt::new("Enter an option", KW).kw(&["New", "Set", "Delete"]).default("New"),
            Phase::Name => {
                let msg = match self.op {
                    Some("new") => "Enter name of new visibility state",
                    Some("delete") => "Enter visibility state name to delete",
                    _ => "Enter visibility state name to make current",
                };
                let current = states(s).ok().and_then(|(_, c)| c);
                let p = Prompt::new(msg, Accept::TEXT);
                if self.op == Some("new") { p } else { p.default(current.unwrap_or_default()) }
            }
            Phase::Visibility => Prompt::new("Enter object visibility for new state", KW)
                .kw(&["Hide all", "Show all", "Current visibility"])
                .default("Current visibility"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let word = match &i {
            Input::Keyword(k) | Input::Text(k) => k.trim().to_string(),
            Input::Enter => String::new(),
            Input::Cancel => return Ok(Step::Cancel),
            _ => return Ok(Step::Continue),
        };
        match self.phase {
            Phase::Option => {
                let k =
                    if word.is_empty() { Some("New".to_string()) } else { Prompt::new("", KW).kw(&["New", "Set", "Delete"]).match_keyword(&word) };
                self.op = match k.as_deref() {
                    Some("New") => Some("new"),
                    Some("Set") => Some("set"),
                    Some("Delete") => Some("delete"),
                    _ => return Err(EngineError::Other("Invalid option keyword.".into())),
                };
                self.phase = Phase::Name;
                Ok(Step::Continue)
            }
            Phase::Name => {
                let name = if word.is_empty() && self.op != Some("new") { states(s)?.1.unwrap_or_default() } else { word };
                if name.is_empty() {
                    return Ok(Step::Done);
                }
                if self.op == Some("new") {
                    self.name = name;
                    self.phase = Phase::Visibility;
                    return Ok(Step::Continue);
                }
                run_bvstate(s, &json!({ "operation": self.op, "name": name }))?;
                Ok(Step::Done)
            }
            Phase::Visibility => {
                let k =
                    if word.is_empty() { None } else { Prompt::new("", KW).kw(&["Hide all", "Show all", "Current visibility"]).match_keyword(&word) };
                let how = match k.as_deref() {
                    Some("Hide all") => "hide",
                    Some("Show all") => "show",
                    _ => "current",
                };
                run_bvstate(s, &json!({ "operation": "new", "name": self.name, "visibility": how }))?;
                Ok(Step::Done)
            }
        }
    }
}

/// BVSHOW and BVHIDE: the objects, then the current state or all of them.
#[derive(Default)]
struct ShowHideM {
    show: bool,
    sel: SelectPhase,
    picked: Option<Vec<Handle>>,
}

impl Interactive for ShowHideM {
    fn name(&self) -> &'static str {
        if self.show { "BVSHOW" } else { "BVHIDE" }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if states(s)?.0.is_empty() {
            return Err(EngineError::Other("The block has no visibility parameter: add one first (BPARAMETER Visibility).".into()));
        }
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.picked = Some(self.sel.picked.clone());
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.picked {
            None => self.sel.prompt(),
            Some(_) => Prompt::new("Apply action to", KW).kw(&["Current state", "All visibility states"]).default("Current state"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        let Some(hs) = self.picked.clone() else {
            return match self.sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Done),
                SelOutcome::Done(hs) => {
                    self.picked = Some(hs);
                    Ok(Step::Continue)
                }
            };
        };
        let all = match &i {
            Input::Keyword(k) | Input::Text(k) => {
                Prompt::new("", KW).kw(&["Current state", "All visibility states"]).match_keyword(k).is_some_and(|k| k.starts_with('A'))
            }
            _ => false,
        };
        let hs: Vec<String> = hs.iter().map(|h| h.hex()).collect();
        show_hide(s, &json!({ "handles": hs, "all": all }), self.show)?;
        s.set_selection(Vec::new());
        Ok(Step::Done)
    }
}
