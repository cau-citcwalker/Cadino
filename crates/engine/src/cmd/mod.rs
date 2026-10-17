//! The command registry. Ids are AutoCAD command names in lower case; menu paths follow
//! AutoCAD's menu bar so the catalog doubles as the parity metric.

pub(crate) mod annoscale;
mod annotate;
mod array;
mod audit;
pub(crate) mod blocks;
pub mod clipboard;
pub mod constraints;
mod draw;
mod draw2;
mod draworder;
mod edit;
mod exchange;
pub mod file;
mod gripcmds;
mod griphot;
pub(crate) mod groups;
pub(crate) mod hatch;
mod inquiry;
mod layer;
mod layout;
mod lisp;
mod mleader_content;
mod mleader_edit;
mod modify;
mod modify2;
mod mtext_opts;
mod overkill;
mod pline_opts;
pub mod plotters;
mod props;
pub use props::MatchProps;
mod exchange3d;
mod furniture;
mod qselect;
mod selecttools;
mod settings;
mod solids;
mod solids2;
mod spline_opts;
mod table;
mod tolerance;
mod trimextend;
mod utility;
mod view;
mod view3d;
pub use view3d::orbit;

pub mod curves;
pub mod helpers;
pub mod machines;
pub mod options;

use cadcraft_doc::Handle;
use cadcraft_geom::Vec2;
use serde::Serialize;
use serde_json::Value;

use crate::{EngineError, Interactive, Result, Session};

pub type Run = fn(&mut Session, &Value) -> Result<Value>;
pub type Enabled = fn(&Session) -> std::result::Result<(), String>;
pub type Factory = fn(&Session) -> Result<Box<dyn Interactive>>;

pub struct CommandSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// Menu placement, e.g. `["Draw", "Circle", "Center, Radius"]`. Empty = not in menus.
    pub menu: &'static [&'static str],
    pub shortcut: Option<&'static str>,
    pub aliases: &'static [&'static str],
    pub params: &'static str,
    pub enabled: Enabled,
    pub run: Run,
    pub undoable: bool,
    pub interactive: Option<Factory>,
    /// Can run inside another command with a leading apostrophe.
    pub transparent: bool,
}

impl CommandSpec {
    pub const fn new(id: &'static str, label: &'static str, run: Run) -> Self {
        CommandSpec {
            id,
            label,
            menu: &[],
            shortcut: None,
            aliases: &[],
            params: "",
            enabled: has_doc,
            run,
            undoable: true,
            interactive: None,
            transparent: false,
        }
    }
    pub const fn menu(mut self, m: &'static [&'static str]) -> Self {
        self.menu = m;
        self
    }
    pub const fn key(mut self, k: &'static str) -> Self {
        self.shortcut = Some(k);
        self
    }
    pub const fn alias(mut self, a: &'static [&'static str]) -> Self {
        self.aliases = a;
        self
    }
    pub const fn params(mut self, p: &'static str) -> Self {
        self.params = p;
        self
    }
    pub const fn enabled(mut self, e: Enabled) -> Self {
        self.enabled = e;
        self
    }
    pub const fn noundo(mut self) -> Self {
        self.undoable = false;
        self
    }
    pub const fn interactive(mut self, f: Factory) -> Self {
        self.interactive = Some(f);
        self
    }
    pub const fn transparent(mut self) -> Self {
        self.transparent = true;
        self
    }
    pub fn info(&self, s: &Session) -> CommandInfo {
        let e = (self.enabled)(s);
        CommandInfo {
            id: self.id,
            label: self.label,
            menu: self.menu.to_vec(),
            shortcut: self.shortcut,
            aliases: self.aliases.to_vec(),
            user_aliases: file::user_aliases_of(self.id),
            params: self.params,
            interactive: self.interactive.is_some(),
            enabled: e.is_ok(),
            disabled_reason: e.err(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub menu: Vec<&'static str>,
    pub shortcut: Option<&'static str>,
    pub aliases: Vec<&'static str>,
    /// The user's own aliases for it (ALIASEDIT, REINIT, acad.pgp).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub user_aliases: Vec<String>,
    pub params: &'static str,
    pub interactive: bool,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
}

pub fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}
pub fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.state().map(|_| ()).map_err(|_| "no drawing open".into())
}
pub fn has_selection(s: &Session) -> std::result::Result<(), String> {
    has_doc(s)?;
    if s.selection().is_empty() { Err("nothing selected".into()) } else { Ok(()) }
}

pub fn command_specs() -> &'static [CommandSpec] {
    static SPECS: std::sync::OnceLock<Vec<CommandSpec>> = std::sync::OnceLock::new();
    SPECS.get_or_init(|| {
        let mut v = Vec::new();
        v.extend(file::specs());
        v.extend(file::alias_specs());
        v.extend(audit::specs());
        v.extend(edit::specs());
        v.extend(qselect::specs());
        v.extend(selecttools::specs());
        v.extend(view::specs());
        v.extend(view3d::specs());
        v.extend(solids::specs());
        v.extend(exchange3d::specs());
        v.extend(furniture::specs());
        v.extend(solids2::specs());
        v.extend(draw::specs());
        v.extend(draw2::specs());
        v.extend(annotate::specs());
        v.extend(mleader_edit::specs());
        v.extend(annoscale::specs());
        v.extend(hatch::specs());
        v.extend(overkill::specs());
        v.extend(blocks::specs());
        v.extend(modify::specs());
        v.extend(draworder::specs());
        v.extend(array::specs());
        v.extend(modify2::specs());
        v.extend(groups::specs());
        v.extend(gripcmds::specs());
        v.extend(file::compare::specs());
        v.extend(layer::specs());
        v.extend(layout::specs());
        v.extend(exchange::specs());
        v.extend(props::specs());
        v.extend(inquiry::specs());
        v.extend(settings::specs());
        v.extend(options::specs());
        v.extend(utility::specs());
        v.extend(table::specs());
        v.extend(tolerance::specs());
        v.extend(constraints::specs());
        v.extend(lisp::specs());
        v
    })
}

pub fn find_command(id: &str) -> Option<&'static CommandSpec> {
    let l = id.to_ascii_lowercase();
    command_specs().iter().find(|c| c.id == l)
}

/// Resolve a typed name or alias (AutoCAD-style short names such as `L`, `C`, `PL`) to an id.
/// Command names win; the user's aliases (ALIASEDIT, REINIT) come before the built-in ones.
pub fn resolve_alias(name: &str) -> String {
    let l = name.to_ascii_lowercase();
    if find_command(&l).is_some() {
        return l;
    }
    if let Some(id) = file::user_alias(&l) {
        return id;
    }
    if let Some(c) = command_specs().iter().find(|c| c.aliases.iter().any(|a| a.eq_ignore_ascii_case(&l))) {
        return c.id.to_string();
    }
    // Rhino's command names, so 3D work reads as it does there; then AutoCAD's other names.
    RHINO_NAMES.iter().chain(ACAD_NAMES).find(|(r, _)| *r == l).map(|(_, id)| (*id).to_string()).filter(|id| find_command(id).is_some()).unwrap_or(l)
}

/// AutoCAD's aliases and legacy names (the DD… dialogs) for commands that go by another id here.
/// Command ids and the commands' own aliases win over these.
const ACAD_NAMES: &[(&str, &str)] = &[
    ("dimsty", "dimstyle"),
    ("3s", "3dscale"),
    ("bc", "bclose"),
    ("bs", "bsave"),
    ("cbar", "constraintbar"),
    ("delcon", "delconstraint"),
    ("dl", "datalink"),
    ("dlu", "datalinkupdate"),
    ("drm", "drawingrecovery"),
    ("er", "xref"),
    ("hb", "hatchtoback"),
    ("isolate", "isolateobjects"),
    ("unisolate", "unisolateobjects"),
    ("unhide", "unisolateobjects"),
    ("mea", "measuregeom"),
    ("mla", "mleaderalign"),
    ("mlc", "mleadercollect"),
    ("mle", "mleaderedit"),
    ("mo", "properties"),
    ("sn", "snap"),
    ("tedit", "textedit"),
    ("sha", "vscurrent"),
    ("shademode", "vscurrent"),
    ("viewports", "vports"),
    ("ra", "redraw"),
    ("redrawall", "redraw"),
    ("3dforbit", "3dorbit"),
    ("3dcorbit", "3dorbit"),
    ("dce", "centermark"),
    ("dimcenter", "centermark"),
    ("ddunits", "units"),
    ("ddcolor", "color"),
    ("ddlmodes", "layer"),
    ("ddltype", "linetype"),
    ("ddrmodes", "dsettings"),
    ("ddselect", "options"),
    ("ddgrips", "options"),
    ("ddview", "view"),
    ("ddattdef", "attdef"),
    ("ddatte", "attedit"),
    ("ddchprop", "properties"),
    ("ddmodify", "properties"),
    ("ddrename", "rename"),
    ("ddstyle", "style"),
    // AutoCAD's Object Snap tab of Drafting Settings.
    ("ddosnap", "dsettings"),
    // The Viewpoint Presets dialog and the Visual Styles Manager: their command-line forms here.
    ("ddvpoint", "vpoint"),
    ("vp", "vpoint"),
    ("visualstyles", "vscurrent"),
    ("vsm", "vscurrent"),
];

/// AutoCAD's aliases for system variables: typed bare, they act as SETVAR on that variable.
const SYSVAR_NAMES: &[(&str, &str)] = &[("th", "THICKNESS"), ("ti", "TILEMODE")];

/// The system variable a typed name sets (TH sets THICKNESS), in upper case.
pub fn sysvar_name(name: &str) -> String {
    let l = name.to_ascii_lowercase();
    SYSVAR_NAMES.iter().find(|(a, _)| *a == l).map_or_else(|| l.to_ascii_uppercase(), |(_, v)| (*v).to_string())
}

/// Rhino command names → ours.
const RHINO_NAMES: &[(&str, &str)] = &[
    ("extrudecrv", "extrude"),
    ("extrudesrf", "extrude"),
    ("booleanunion", "union"),
    ("booleandifference", "subtract"),
    ("booleanintersection", "intersect"),
    ("booleansplit", "slice"),
    ("sweep1", "sweep"),
    ("planarsrf", "planesurf"),
    ("polyline", "pline"),
    ("curve", "spline"),
    ("interpcrv", "spline"),
    ("rectangle", "rectang"),
    ("setview", "vpoint"),
    ("rotate3d", "3drotate"),
    ("selall", "selectall"),
    ("filletedges", "filletedge"),
    ("chamferedges", "chamferedge"),
    ("4view", "viewports.rhino"),
    ("maxviewport", "viewports.maximize"),
];

// ---------- param helpers ----------

pub(crate) fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}
pub(crate) fn f64_or(p: &Value, key: &str, default: f64) -> f64 {
    p.get(key).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(default)
}
pub(crate) fn f64_req(cmd: &str, p: &Value, key: &str) -> Result<f64> {
    p.get(key).and_then(Value::as_f64).filter(|v| v.is_finite()).ok_or_else(|| bad(cmd, format!("`{key}` (number) is required")))
}
/// An optional size: `None` when absent or null, else a finite number above zero (or zero too
/// with `zero_ok`). Anything else is refused, not clamped or made absolute, so a bad value never
/// makes a degenerate object.
pub(crate) fn size_param(cmd: &str, p: &Value, key: &str, zero_ok: bool) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_f64().filter(|x| x.is_finite()) {
            Some(x) if x > 0.0 || (zero_ok && x == 0.0) => Ok(Some(x)),
            _ => Err(bad(cmd, format!("`{key}` must be {}", if zero_ok { "zero or positive" } else { "positive" }))),
        },
    }
}
/// `pts` without consecutive repeats (closer than `1e-12`), as the interactive LINE ignores a
/// point picked twice.
pub(crate) fn distinct_points(pts: &[Vec2]) -> Vec<Vec2> {
    let mut out: Vec<Vec2> = Vec::with_capacity(pts.len());
    for p in pts {
        if !out.last().is_some_and(|l| l.near(*p, 1e-12)) {
            out.push(*p);
        }
    }
    out
}
pub(crate) fn bool_or(p: &Value, key: &str, default: bool) -> bool {
    p.get(key).and_then(Value::as_bool).unwrap_or(default)
}
pub(crate) fn str_param<'a>(p: &'a Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str)
}
pub fn point_value(v: &Value) -> Option<Vec2> {
    if let Some(a) = v.as_array() {
        let x = a.first()?.as_f64()?;
        let y = a.get(1)?.as_f64()?;
        let p = Vec2::new(x, y);
        return p.is_finite().then_some(p);
    }
    if let Some(s) = v.as_str() {
        return crate::prompt::parse_point(s, Vec2::ZERO);
    }
    let x = v.get("x")?.as_f64()?;
    let y = v.get("y")?.as_f64()?;
    let p = Vec2::new(x, y);
    p.is_finite().then_some(p)
}
pub(crate) fn point_param(p: &Value, key: &str) -> Option<Vec2> {
    p.get(key).and_then(point_value)
}
pub(crate) fn point_req(cmd: &str, p: &Value, key: &str) -> Result<Vec2> {
    point_param(p, key).ok_or_else(|| bad(cmd, format!("`{key}` ([x, y]) is required")))
}
pub(crate) fn points_param(p: &Value, key: &str) -> Option<Vec<Vec2>> {
    p.get(key)?.as_array()?.iter().map(point_value).collect()
}
/// Handles from `handles` (hex strings or numbers), `handle`, or else the current selection. A
/// `handle`/`handles` that can't be read is an error, never a silent fall back to the selection.
pub(crate) fn targets(s: &Session, p: &Value) -> Result<Vec<Handle>> {
    let handle = |v: &Value| v.as_str().and_then(Handle::parse_hex).or_else(|| v.as_u64().map(Handle));
    let not_handle = |key: &str, v: &Value| EngineError::Other(format!("`{key}`: {v} is not an object handle (a hex string such as \"1A2\")"));
    match p.get("handles") {
        None | Some(Value::Null) => {}
        Some(Value::Array(a)) => return a.iter().map(|v| handle(v).ok_or_else(|| not_handle("handles", v))).collect(),
        Some(v) => return Err(EngineError::Other(format!("`handles` must be an array of object handles, got {v}"))),
    }
    match p.get("handle") {
        None | Some(Value::Null) => Ok(s.selection()),
        Some(v) => Ok(vec![handle(v).ok_or_else(|| not_handle("handle", v))?]),
    }
}
pub(crate) fn ok() -> Result<Value> {
    Ok(Value::Null)
}

#[cfg(test)]
mod rhino_name_tests {
    use super::resolve_alias;

    #[test]
    fn rhino_names_reach_our_commands() {
        assert_eq!(resolve_alias("BooleanDifference"), "subtract");
        assert_eq!(resolve_alias("ExtrudeCrv"), "extrude");
        assert_eq!(resolve_alias("PlanarSrf"), "planesurf");
        assert_eq!(resolve_alias("MaxViewport"), "viewports.maximize");
        assert_eq!(resolve_alias("4View"), "viewports.rhino");
        // Ours and AutoCAD's names come first.
        assert_eq!(resolve_alias("box"), "box");
        // A Rhino name whose command doesn't exist yet stays as typed.
        assert!(super::find_command(&resolve_alias("Rotate3D")).is_none() || resolve_alias("Rotate3D") == "3drotate");
        assert_eq!(resolve_alias("SelAll"), "selectall");
    }

    #[test]
    fn autocad_aliases_and_legacy_names_reach_our_commands() {
        // Every target is a command (a typo would leave the name unknown).
        for (name, id) in super::ACAD_NAMES {
            assert!(super::find_command(id).is_some(), "{name} → {id} is not a command");
        }
        assert_eq!(resolve_alias("DIMSTY"), "dimstyle");
        assert_eq!(resolve_alias("mo"), "properties");
        assert_eq!(resolve_alias("DDLMODES"), "layer");
        assert_eq!(resolve_alias("le"), "qleader");
        assert_eq!(resolve_alias("ddosnap"), "dsettings");
        assert_eq!(resolve_alias("VP"), "vpoint");
        assert_eq!(resolve_alias("vsm"), "vscurrent");
        assert_eq!(super::sysvar_name("th"), "THICKNESS");
        assert_eq!(super::sysvar_name("ThickNess"), "THICKNESS");
        // Command ids and their own aliases still win.
        assert_eq!(resolve_alias("dimstyle"), "dimstyle");
        assert_eq!(resolve_alias("l"), "line");
    }

    #[test]
    fn dimsty_typed_starts_dimstyle() {
        let mut s = crate::Session::new();
        let _ = s.cmdline("DIMSTY");
        assert!(s.log.iter().any(|l| l == "Command: DIMSTYLE"), "{:?}", s.log);
        assert!(!s.log.iter().any(|l| l.contains("unknown command")), "{:?}", s.log);
    }

    #[test]
    fn dash_aliases_reach_the_dash_form() {
        for name in ["-LT", "-ltype", "_-LT"] {
            let mut s = crate::Session::new();
            s.cmdline(name).unwrap();
            assert_eq!(s.running.as_ref().map(|r| r.id.as_str()), Some("-linetype"), "{name}");
            assert!(s.prompt_text().contains("[?/Load/Set]"), "{name}: {}", s.prompt_text());
        }
    }

    #[test]
    fn dash_forms_without_prompts_are_refused_not_run_with_defaults() {
        let mut s = crate::Session::new();
        for name in ["-PLOT", "-publish", "-CH", "-EXPORT"] {
            let e = s.cmdline(name).unwrap_err().to_string();
            assert!(e.contains("command-line form isn't available yet"), "{name}: {e}");
            assert!(s.running.is_none());
        }
        assert!(!s.log.iter().any(|l| l.contains("Published")), "{:?}", s.log);
        // Dash forms with prompts, of their own or the command's, still start them.
        for (name, id) in [("-layer", "layer"), ("-hatch", "hatch"), ("-attedit", "-attedit"), ("-pan", "pan")] {
            s.cmdline(name).unwrap();
            assert_eq!(s.running.as_ref().map(|r| r.id.as_str()), Some(id), "{name}");
            s.cancel();
        }
    }
}
