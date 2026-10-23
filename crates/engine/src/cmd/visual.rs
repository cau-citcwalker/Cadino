//! The look of the 3D view beyond VSCURRENT: HIDE (hidden lines removed until the next REGEN),
//! -VISUALSTYLES (the Visual Styles Manager at the command line; typed bare, VISUALSTYLES opens
//! the manager in the UI) and NAVVCUBE (the ViewCube, which the UI shows: the engine passes the
//! choice on in [`Session::navvcube`]).

use serde_json::{Value, json};

use super::curves::KW;
use super::*;
use crate::{EngineError, Input, Interactive, Prompt, Result, Session, Step};

/// The visual styles as VSCURRENT takes them, with their names as the manager shows them.
pub const STYLES: &[(&str, &str)] = &[
    ("2dwireframe", "2D Wireframe"),
    ("wireframe", "Wireframe"),
    ("hidden", "Hidden"),
    ("realistic", "Realistic"),
    ("shaded", "Shaded"),
    ("shadedwithedges", "Shaded with Edges"),
    ("conceptual", "Conceptual"),
];

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("hide", "Hide", run_hide).menu(&["View", "Hide"]).alias(&["hi"]).noundo(),
        CommandSpec::new("vscurrent.hidden", "Hidden", |s, _| run_body(s, "vscurrent", &json!({ "style": "hidden" })))
            .menu(&["View", "Visual Styles", "Hidden"])
            .noundo(),
        CommandSpec::new("-visualstyles", "Visual Styles", run_visualstyles)
            .params(
                "{style: 2dwireframe | wireframe | hidden | realistic | shaded | shadedwithedges | conceptual} sets the current one; {} lists them",
            )
            .noundo()
            .interactive(|_| Ok(Box::new(VisualStylesM { current: false }))),
        CommandSpec::new("navvcube", "ViewCube", run_navvcube)
            .params("{on: bool} | {option: on | off | settings}")
            .noundo()
            .enabled(always)
            .interactive(|_| Ok(Box::new(NavVCubeM))),
    ]
}

fn run_hide(s: &mut Session, _p: &Value) -> Result<Value> {
    if s.layout_space() != cadcraft_doc::Space::Model {
        return Err(EngineError::Other("HIDE works on the Model tab only.".into()));
    }
    s.state_mut()?.hide = true;
    s.touch();
    Ok(json!({ "message": "Regenerating model." }))
}

fn run_visualstyles(s: &mut Session, p: &Value) -> Result<Value> {
    match str_param(p, "style") {
        Some(style) => run_body(s, "vscurrent", &json!({ "style": style })),
        None => Ok(json!({ "styles": STYLES.iter().map(|(_, n)| *n).collect::<Vec<_>>() })),
    }
}

/// NAVVCUBE: ON or OFF passes the choice to the UI; Settings is not available yet.
fn run_navvcube(s: &mut Session, p: &Value) -> Result<Value> {
    let on = match (p.get("on").and_then(Value::as_bool), str_param(p, "option").map(str::to_ascii_lowercase).as_deref()) {
        (Some(on), _) => on,
        (None, Some("on")) | (None, None) => true,
        (None, Some("off")) => false,
        (None, Some("settings" | "s")) => return Err(EngineError::Other("ViewCube settings are not available yet.".into())),
        (None, Some(o)) => return Err(bad("navvcube", format!("unknown option `{o}` (on, off or settings)"))),
    };
    s.navvcube = Some(on);
    Ok(json!({ "on": on }))
}

/// NAVVCUBE at the command line.
struct NavVCubeM;

impl Interactive for NavVCubeM {
    fn name(&self) -> &'static str {
        "NAVVCUBE"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter an option", KW).kw(&["ON", "OFF", "Settings"]).default("ON")
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let option = match i {
            Input::Keyword(k) => k.to_ascii_lowercase(),
            Input::Enter => "on".into(),
            Input::Cancel => return Ok(Step::Cancel),
            _ => return Ok(Step::Continue),
        };
        run_navvcube(s, &json!({ "option": option }))?;
        Ok(Step::Done)
    }
}

/// -VISUALSTYLES at the command line: set Current asks for the style; ? lists them.
// ponytail: the styles are the built-in ones: Saveas, Rename and Delete say so instead of
// keeping named styles in the drawing.
struct VisualStylesM {
    /// set Current was chosen: the next answer is the style.
    current: bool,
}

impl Interactive for VisualStylesM {
    fn name(&self) -> &'static str {
        "-VISUALSTYLES"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.current {
            return Prompt::new("Enter an option", KW).kw(&[
                "2dwireframe",
                "Wireframe",
                "Hidden",
                "Realistic",
                "Shaded",
                "shaded with Edges",
                "Conceptual",
            ]);
        }
        Prompt::new("Enter an option", KW).kw(&["set Current", "Saveas", "Rename", "Delete", "?"])
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let k = match i {
            Input::Keyword(k) | Input::Text(k) => k,
            Input::Enter | Input::Cancel => return Ok(Step::Done),
            _ => return Ok(Step::Continue),
        };
        if self.current {
            run_body(s, "vscurrent", &json!({ "style": k }))?;
            return Ok(Step::Done);
        }
        match k.as_str() {
            "set Current" => {
                self.current = true;
                Ok(Step::Continue)
            }
            "?" => {
                for (_, name) in STYLES {
                    s.echo(format!("  {name}"));
                }
                Ok(Step::Done)
            }
            _ => Err(EngineError::Other("The visual styles are built in: saving, renaming and deleting them is not available yet.".into())),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// HIDE shows hidden lines removed until REGEN; VSCURRENT Hidden is the unshaded look with
    /// edges and clears HIDE's.
    #[test]
    fn hide_lasts_until_regen() {
        let mut s = Session::new();
        s.cmdline("HI").unwrap();
        assert!(s.state().unwrap().hide);
        assert!(s.log.iter().any(|l| l == "Regenerating model."), "{:?}", s.log);
        s.execute("regen", &json!({})).unwrap();
        assert!(!s.state().unwrap().hide);
        s.execute("hide", &json!({})).unwrap();
        assert_eq!(s.execute("vscurrent", &json!({ "style": "hidden" })).unwrap()["style"], "Hidden");
        let st = s.state().unwrap();
        assert_eq!((st.shaded, st.shaded_edges, st.hide), (false, true, false));
        s.cmdline("-VISUALSTYLES").unwrap();
        assert!(s.prompt_text().ends_with("Enter an option [set Current/Saveas/Rename/Delete/?]:"), "{}", s.prompt_text());
        s.cmdline("C").unwrap();
        s.cmdline("E").unwrap();
        let st = s.state().unwrap();
        assert_eq!((st.shaded, st.shaded_edges), (true, true));
    }

    /// NAVVCUBE passes ON and OFF to the UI; Settings is refused with a reason.
    #[test]
    fn navvcube_passes_the_choice_on() {
        let mut s = Session::new();
        s.cmdline("NAVVCUBE").unwrap();
        assert!(s.prompt_text().ends_with("Enter an option [ON/OFF/Settings] <ON>:"), "{}", s.prompt_text());
        s.cmdline("OFF").unwrap();
        assert_eq!(s.navvcube.take(), Some(false));
        assert_eq!(s.execute("navvcube", &json!({ "on": true })).unwrap()["on"], true);
        assert_eq!(s.navvcube, Some(true));
        assert!(s.execute("navvcube", &json!({ "option": "settings" })).is_err());
    }
}
