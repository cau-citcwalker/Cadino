//! The command line: history, the active prompt with clickable keywords, typed input (keyboard
//! focus stays on the drawing — typing anywhere goes here) and AutoComplete. It floats over the
//! drawing, moved by the handle at its left end, or docks full-width at the drawing area's top or
//! bottom edge, as AutoCAD's does ([`Position`], `ui.cmdline.position`).

use std::collections::HashSet;

use cadcraft_engine::Input;
use egui::{Key, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::CadApp;
use crate::theme::Tokens;

/// Where the command line docks: nowhere (it floats over the drawing), or at the drawing area's
/// top or bottom edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dock {
    #[default]
    Floating,
    Top,
    Bottom,
}

/// Where the command line sits (saved with the workspace).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Position {
    pub dock: Dock,
    /// Where the floating bar sits in the room it can move in: `[0, 0]` the drawing area's
    /// top-left corner, `[1, 1]` its bottom-right, so it keeps its place when the area resizes.
    /// `None`: centred at the bottom. Kept while docked, for Floating to return to.
    pub at: Option<[f64; 2]>,
}

impl Position {
    /// `at` within `[0, 1]`, or `None` when it isn't a pair of numbers.
    pub fn sanitized(self) -> Self {
        let at = self.at.filter(|a| a.iter().all(|v| v.is_finite())).map(|a| a.map(|v| v.clamp(0.0, 1.0)));
        Self { at, ..self }
    }
}

/// `ui.cmdline.position` `{"dock": "floating"|"top"|"bottom", "at": [x, y]|null}`: dock the
/// command line, or float it at `at` ([`Position::at`]; null: the bottom centre). What is left out
/// stays; replies with the position.
pub fn route(app: &mut CadApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    (id == "ui.cmdline.position").then(|| set_position(app, params))
}

fn set_position(app: &mut CadApp, params: &Value) -> Result<Value, String> {
    let mut pos = app.ui.command_line_position.sanitized();
    if let Some(d) = params.get("dock") {
        pos.dock = serde_json::from_value(d.clone()).map_err(|_| "dock must be floating, top or bottom")?;
    }
    match params.get("at") {
        Some(Value::Null) => pos.at = None,
        Some(a) => {
            let at = serde_json::from_value::<[f64; 2]>(a.clone()).ok().filter(|a| a.iter().all(|v| v.is_finite()));
            pos.at = Some(at.ok_or("at must be [x, y] (0 to 1 across the drawing area)")?);
        }
        None => {}
    }
    app.ui.command_line_position = pos.sanitized();
    serde_json::to_value(app.ui.command_line_position).map_err(|e| e.to_string())
}

/// The command line menu's placement items (Dock at Bottom, Dock at Top, Floating, Reset Position).
fn position_entries(app: &CadApp) -> Vec<crate::menus::Entry> {
    let p = app.ui.command_line_position;
    [
        ("Dock at Bottom", json!({"dock": "bottom"}), p.dock != Dock::Bottom),
        ("Dock at Top", json!({"dock": "top"}), p.dock != Dock::Top),
        ("Floating", json!({"dock": "floating"}), p.dock != Dock::Floating),
        ("Reset Position", json!({"dock": "floating", "at": null}), p != Position::default()),
    ]
    .into_iter()
    .map(|(label, params, enabled)| crate::menus::Entry::Item {
        label: label.into(),
        id: format!("run:ui.cmdline.position:{params}"),
        shortcut: None,
        enabled,
    })
    .collect()
}

#[derive(Default)]
pub struct CmdLine {
    pub buffer: String,
    /// The selection as of the last frame, to notice when it changes.
    seen_selection: Vec<cadcraft_doc::Handle>,
    /// Something was typed at the command line since the selection was made.
    typed_since_selection: bool,
    pub history: Vec<String>,
    pub history_pos: Option<usize>,
    /// What was typed at command prompts (not command names), oldest first: the in-command
    /// shortcut menu's Recent Input.
    pub recent_input: Vec<String>,
    /// The expanded history window is open (F2, the chevron, or a click on the history lines).
    pub expanded: bool,
    /// Dragging the history's top edge: (pointer y, line count) when the drag started.
    resize_from: Option<(f32, usize)>,
    /// The Dynamic Input options menu at the cursor (Down Arrow at a prompt with keywords).
    pub options: Option<crate::dyninput::OptionsMenu>,
    /// The text whose second Dynamic Input value Tab locked; the lock holds while it is unchanged.
    locked_second: Option<String>,
    /// Where the options menu's rows were drawn last frame (screen), for clicks on them.
    pub option_rows: Vec<(Rect, String)>,
    /// Where the Dynamic Input tooltip stays while the options menu is open.
    pub options_anchor: Option<Pos2>,
    /// Moving the command line by its handle: the grab point from the bar's top-left corner, and
    /// the pointer.
    moving: Option<(Vec2, Pos2)>,
}

/// The most history lines the command line shows above its bar.
pub const MAX_LINES: usize = 12;
const LINE_H: f32 = 16.0;
const BAR_H: f32 = 24.0;
/// The move handle at the bar's left end.
const HANDLE_W: f32 = 12.0;
/// A command line dropped this near the drawing area's top or bottom edge docks there.
const DOCK_SNAP: f32 = 4.0;

/// The Dynamic Input frame for the prompt on screen, or `None` when the value boxes are not live
/// (Dynamic Input off, no point prompt or a pending selection window).
pub fn dyn_frame(app: &CadApp) -> Option<crate::dyninput::Frame> {
    let st = &app.session.settings;
    if !st.dynmode || app.session.pending_window.is_some() {
        return None;
    }
    let p = app.session.current_prompt().filter(cadcraft_engine::pointmod::accepts)?;
    Some(crate::dyninput::Frame { base: p.base, cartesian: st.dynpi_cartesian, absolute: st.dynpi_absolute })
}

/// Take Tab away from egui before it moves keyboard focus to the next widget (which would leave
/// the command line deaf to typing). While no widget has focus, a pressed Tab becomes a `"\t"`
/// text event, which keeps its place among the other keystrokes; [`keyboard`] handles it.
pub fn capture_tab(ctx: &egui::Context, raw: &mut egui::RawInput) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    raw.events.retain_mut(|e| {
        let tab = match e {
            egui::Event::Key { key: Key::Tab, pressed, .. } => Some(*pressed),
            _ => None,
        };
        match tab {
            Some(true) => {
                *e = egui::Event::Text("\t".into());
                true
            }
            Some(false) => false,
            None => true,
        }
    });
}

/// Tab on the command line: cycle the object snaps under the cursor, type the Dynamic Input
/// separator (`,` or `<`) after a value, or complete a command name.
fn tab(app: &mut CadApp) {
    // Grip dimensional input: Tab locks the typed value and moves to the next field.
    if crate::grip_dyn::tab(app) {
        return;
    }
    // Over a snap marker with nothing typed, Tab cycles the candidate snaps.
    if app.cmd.buffer.is_empty() && app.canvas.snap.is_some() {
        app.canvas.snap_tab = true;
    } else if let Some(f) = dyn_frame(app)
        && let Some(sep) = crate::dyninput::tab_separator(&app.cmd.buffer, &f)
    {
        app.cmd.buffer.push(sep);
    } else if let Some(f) = dyn_frame(app)
        && crate::dyninput::can_lock_second(&app.cmd.buffer, &f)
    {
        // Both values typed: Tab locks the second one as well.
        app.cmd.locked_second = Some(app.cmd.buffer.clone());
    } else if let Some(s) = suggestions(&app.cmd.buffer).first() {
        app.cmd.buffer = s.id.clone();
    }
}

/// An AutoComplete suggestion for the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// Command identifier (uppercase, e.g. "RECTANG"), used for buffer completion and execution.
    pub id: String,
    /// Display text shown in the AutoComplete list (e.g. "REC (RECTANG)" or "RECOVER").
    pub display: String,
    /// Human-readable command label (e.g. "Rectangle").
    pub label: &'static str,
}

/// Matching command names/aliases for AutoComplete.
fn suggestions(prefix: &str) -> Vec<Suggestion> {
    if prefix.is_empty() || prefix.contains(' ') {
        return Vec::new();
    }
    let p = prefix.to_ascii_lowercase();

    let mut out: Vec<Suggestion> = Vec::new();
    let mut seen_ids: HashSet<&'static str> = HashSet::new();

    // 1. Exact match: alias resolution or exact command name match.
    let resolved = cadcraft_engine::cmd::resolve_alias(&p);
    if let Some(spec) = cadcraft_engine::cmd::find_command(&resolved)
        && !spec.id.contains('.')
    {
        let display =
            if spec.id == p { spec.id.to_ascii_uppercase() } else { format!("{} ({})", p.to_ascii_uppercase(), spec.id.to_ascii_uppercase()) };
        out.push(Suggestion { id: spec.id.to_ascii_uppercase(), display, label: spec.label });
        seen_ids.insert(spec.id);
    }

    // 2. Command ID prefix matches (excluding already matched exact command).
    let mut id_matches: Vec<&'static cadcraft_engine::CommandSpec> =
        cadcraft_engine::command_specs().iter().filter(|c| !c.id.contains('.') && !seen_ids.contains(c.id) && c.id.starts_with(&p)).collect();
    id_matches.sort_by_key(|c| (c.id.len(), c.id));
    for c in id_matches {
        out.push(Suggestion { id: c.id.to_ascii_uppercase(), display: c.id.to_ascii_uppercase(), label: c.label });
        seen_ids.insert(c.id);
    }

    // 3. Command alias prefix matches (for commands not yet added).
    let mut alias_matches: Vec<(&'static str, &'static cadcraft_engine::CommandSpec)> = Vec::new();
    for c in cadcraft_engine::command_specs() {
        if c.id.contains('.') || seen_ids.contains(c.id) {
            continue;
        }
        let matching_alias = c.aliases.iter().copied().filter(|a| a.starts_with(&p)).min_by_key(|a| (a.len(), *a));
        if let Some(alias) = matching_alias {
            alias_matches.push((alias, c));
        }
    }
    alias_matches.sort_by_key(|(alias, c)| (alias.len(), *alias, c.id));
    for (alias, c) in alias_matches {
        if seen_ids.insert(c.id) {
            out.push(Suggestion {
                id: c.id.to_ascii_uppercase(),
                display: format!("{} ({})", alias.to_ascii_uppercase(), c.id.to_ascii_uppercase()),
                label: c.label,
            });
        }
    }

    out.truncate(8);
    out
}

impl CmdLine {
    /// Track the selection: a new selection starts with nothing typed since.
    pub fn watch_selection(&mut self, sel: &[cadcraft_doc::Handle]) {
        if self.seen_selection != sel {
            self.seen_selection = sel.to_vec();
            self.typed_since_selection = false;
        }
    }

    /// Note that the command line holds typed text.
    pub fn note_typing(&mut self) {
        if !self.buffer.is_empty() {
            self.typed_since_selection = true;
        }
    }

    /// Tab locked the second Dynamic Input value and nothing was typed since.
    pub fn second_locked(&self) -> bool {
        self.locked_second.as_deref() == Some(self.buffer.as_str())
    }

    /// The options menu row under `at` (screen), if the menu is open.
    pub fn option_at(&self, at: Pos2) -> Option<String> {
        self.options.as_ref()?;
        self.option_rows.iter().find(|(r, _)| r.contains(at)).map(|(_, k)| k.clone())
    }
}

/// Keep the options menu only while its prompt is on screen with Dynamic Input and DYNPROMPT on.
fn options_still_fit(app: &mut CadApp) {
    let st = &app.session.settings;
    let on = st.dynmode && st.dynprompt && app.canvas.hot_grips.is_empty();
    let kws = app.session.current_prompt().map(|p| p.keywords).unwrap_or_default();
    if app.cmd.options.as_ref().is_some_and(|m| !on || !m.fits(&kws)) {
        app.cmd.options = None;
    }
}

/// Down Arrow at a prompt with keywords (Dynamic Input on, DYNPROMPT on, not browsing the
/// history): open the options menu at the cursor, or move its highlight. `false` when the key is
/// left for the history.
fn options_key(app: &mut CadApp, step: isize) -> bool {
    if let Some(m) = app.cmd.options.as_mut() {
        m.step(step);
        return true;
    }
    let st = &app.session.settings;
    if step < 0 || !st.dynmode || !st.dynprompt || app.cmd.history_pos.is_some() || !app.canvas.hot_grips.is_empty() {
        return false;
    }
    let kws = app.session.current_prompt().map(|p| p.keywords).unwrap_or_default();
    app.cmd.options = crate::dyninput::OptionsMenu::open(&kws);
    app.cmd.options.is_some()
}

/// Send a keyword picked from the options menu, as a click on it at the command line does.
pub fn pick_option(app: &mut CadApp, keyword: String) {
    app.cmd.options = None;
    app.cmd.buffer.clear();
    if let Err(e) = app.session.input(Input::Keyword(keyword)) {
        app.session.echo(e.to_string());
    }
}

/// Whether Backspace should erase the selection instead of editing the command line. The key
/// labelled "delete" on a Mac sends Backspace, so on macOS it erases selected objects, but only
/// when it can't be meant for the command line: a fresh, plain press (never an auto-repeat, so
/// holding it to clear typing can't run on into the drawing, and not Option/Cmd+delete, which
/// delete a word or line of text on macOS), no command running, the command line empty, and
/// nothing typed there since the selection was made.
pub fn backspace_erases(mac: bool, press: KeyPress, buffer: &str, command_running: bool, has_selection: bool, typed_since_selection: bool) -> bool {
    mac && press.plain && !press.repeat && buffer.is_empty() && !command_running && has_selection && !typed_since_selection
}

/// How a key was pressed.
#[derive(Clone, Copy, Debug)]
pub struct KeyPress {
    /// The operating system's auto-repeat while the key is held.
    pub repeat: bool,
    /// No Option, Control or Command held (Shift doesn't matter).
    pub plain: bool,
}

/// Handle keyboard input destined for the command line (when no text field has focus).
pub fn keyboard(app: &mut CadApp, ctx: &egui::Context) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    let events = ctx.input(|i| i.events.clone());
    let selection = app.session.selection();
    app.cmd.watch_selection(&selection);
    let mac = ctx.os() == egui::os::OperatingSystem::Mac;
    let text_prompt = app.session.current_prompt().is_some_and(|p| p.accept.text && !p.accept.point && !p.accept.number);
    options_still_fit(app);
    for ev in events {
        // The options menu takes the arrows, Enter and Escape; typing closes it.
        if app.cmd.options.is_some() {
            let space = matches!(&ev, egui::Event::Text(t) if t == " ") && !text_prompt;
            match &ev {
                _ if space => {
                    if let Some(k) = app.cmd.options.as_ref().and_then(|m| m.current()).map(str::to_owned) {
                        pick_option(app, k);
                    }
                    continue;
                }
                egui::Event::Key { key: Key::Enter, pressed: true, .. } => {
                    if let Some(k) = app.cmd.options.as_ref().and_then(|m| m.current()).map(str::to_owned) {
                        pick_option(app, k);
                    }
                    continue;
                }
                egui::Event::Key { key: Key::Escape, pressed: true, .. } => {
                    app.cmd.options = None;
                    continue;
                }
                egui::Event::Key { key: Key::ArrowUp, pressed: true, modifiers, .. } if !modifiers.any() => {
                    options_key(app, -1);
                    continue;
                }
                egui::Event::Key { key: Key::ArrowDown | Key::ArrowUp, pressed: true, .. } => {}
                // Space arrives as a key and as text; the text picks.
                egui::Event::Key { key: Key::Space, .. } => {}
                egui::Event::Text(_) | egui::Event::Key { pressed: true, .. } => app.cmd.options = None,
                _ => {}
            }
        }
        match ev {
            egui::Event::Text(t) => {
                if t == "\t" {
                    tab(app);
                } else if t == " " && !text_prompt {
                    submit(app);
                } else {
                    app.cmd.buffer.push_str(&t);
                }
            }
            egui::Event::Key { key, pressed: true, repeat, modifiers, .. } => match key {
                Key::Enter => submit(app),
                Key::Escape => {
                    app.cmd.buffer.clear();
                    app.session.cancel();
                }
                Key::Backspace => {
                    let press = KeyPress { repeat, plain: !(modifiers.alt || modifiers.ctrl || modifiers.command) };
                    let erase = backspace_erases(
                        mac,
                        press,
                        &app.cmd.buffer,
                        app.session.prompting(),
                        !app.session.selection().is_empty(),
                        app.cmd.typed_since_selection,
                    );
                    if erase {
                        let _ = app.run("erase.selection", serde_json::json!({}));
                    } else {
                        app.cmd.buffer.pop();
                    }
                }
                Key::ArrowUp if !modifiers.any() => {
                    if !app.cmd.history.is_empty() {
                        let n = app.cmd.history.len();
                        let pos = app.cmd.history_pos.map(|p| p.saturating_sub(1)).unwrap_or(n - 1);
                        app.cmd.history_pos = Some(pos);
                        app.cmd.buffer = app.cmd.history.get(pos).cloned().unwrap_or_default();
                    }
                }
                Key::ArrowDown if !modifiers.any() => {
                    if !options_key(app, 1)
                        && let Some(p) = app.cmd.history_pos
                    {
                        let np = p + 1;
                        if np >= app.cmd.history.len() {
                            app.cmd.history_pos = None;
                            app.cmd.buffer.clear();
                        } else {
                            app.cmd.history_pos = Some(np);
                            app.cmd.buffer = app.cmd.history.get(np).cloned().unwrap_or_default();
                        }
                    }
                }
                Key::Tab => tab(app),
                Key::Delete if app.cmd.buffer.is_empty() && !app.session.prompting() => {
                    let _ = app.run("erase.selection", serde_json::json!({}));
                }
                _ => {}
            },
            _ => {}
        }
        app.cmd.note_typing();
    }
}

pub fn submit(app: &mut CadApp) {
    // Grip dimensional input: the typed lengths and angles place the grip.
    if crate::grip_dyn::submit(app) {
        return;
    }
    // Dynamic Input: a split entry (`40,30`, `40<90`, `40,`) is completed from the cursor and
    // measured from the last point.
    if let Some(f) = dyn_frame(app) {
        let cursor = app.canvas.cursor.unwrap_or(app.session.cursor);
        let last = app.session.last_point;
        let done = crate::dyninput::parse(&app.cmd.buffer, &f)
            .and_then(|e| crate::dyninput::complete(&e, &f, cursor, last))
            .or_else(|| crate::dyninput::complete_3d(&app.cmd.buffer, &f, last));
        if let Some(text) = done {
            app.cmd.buffer = text;
        }
    }
    let text = std::mem::take(&mut app.cmd.buffer);
    app.cmd.locked_second = None;
    app.cmd.history_pos = None;
    if !text.trim().is_empty() && !app.session.prompting() {
        app.cmd.history.push(text.clone());
        if app.cmd.history.len() > 200 {
            app.cmd.history.remove(0);
        }
    }
    crate::context_menu::note_input(app, &text);
    if text.is_empty() {
        if let Err(e) = app.session.input(Input::Enter) {
            app.session.echo(e.to_string());
        }
        return;
    }
    // No height left from a 3D snap: typed points (and Dynamic Input's) carry their own
    // (`x,y,z`), others are on the XY plane.
    app.session.point_z = None;
    // AutoComplete: at `Command:` a name nothing runs as typed runs the highlighted (first)
    // suggestion, as AutoCAD's does (DIMST runs DIMSTYLE). Whatever runs as typed (a command or
    // alias, a system variable, a LISP command, a UI name such as AI) still runs as typed, so the
    // line is tried first and only an unknown-command refusal of this very name is replaced.
    let pick = if app.session.prompting() { None } else { suggestions(text.trim()).into_iter().next() };
    let refused = app.cmdline(&text);
    if let (Some(s), Some(e)) = (pick, refused)
        && e == cadcraft_engine::EngineError::UnknownCommand(text.trim().to_string()).to_string()
        && app.session.log.last() == Some(&e)
    {
        app.session.log.pop();
        app.start(&s.id.to_ascii_lowercase());
    }
}

/// Where the command line is docked, while it is shown and not being moved.
fn docked(app: &CadApp) -> Option<Dock> {
    let dock = app.ui.command_line_position.dock;
    (app.ui.show_command_line && app.cmd.moving.is_none() && dock != Dock::Floating).then_some(dock)
}

/// The input bar in `area` (the drawing panel, a docked strip included), and whether the history
/// and AutoComplete open below it (it sits high) rather than above.
fn bar_rect(app: &CadApp, area: Rect) -> (Rect, bool) {
    match docked(app) {
        Some(Dock::Top) => return (Rect::from_min_size(area.min, vec2(area.width(), BAR_H)), true),
        Some(_) => return (Rect::from_min_size(pos2(area.left(), area.bottom() - BAR_H), vec2(area.width(), BAR_H)), false),
        None => {}
    }
    let w = (area.width() * 0.48).clamp(360.0, 760.0).min((area.width() - 8.0).max(0.0));
    let room = vec2((area.width() - w).max(0.0), (area.height() - BAR_H).max(0.0));
    let min = match (app.cmd.moving, app.ui.command_line_position.sanitized().at) {
        (Some((grab, pointer)), _) => pointer - grab,
        (None, Some([x, y])) => area.min + vec2(x as f32, y as f32) * room,
        (None, None) => pos2(area.center().x - w / 2.0, area.bottom() - BAR_H - 10.0),
    };
    // Inside the drawing area, whatever its size now (min/max: NaN-safe).
    let min = pos2(min.x.min(area.left() + room.x).max(area.left()), min.y.min(area.top() + room.y).max(area.top()));
    let bar = Rect::from_min_size(min, vec2(w, BAR_H));
    (bar, bar.center().y < area.center().y)
}

/// Where a command line moved to `min` (its bar's top-left corner, before keeping it inside the
/// area) docks when dropped: pushed against the drawing area's top or bottom edge.
fn drop_dock(min: Pos2, area: Rect) -> Option<Dock> {
    if min.y <= area.top() + DOCK_SNAP {
        Some(Dock::Top)
    } else if min.y + BAR_H >= area.bottom() - DOCK_SNAP {
        Some(Dock::Bottom)
    } else {
        None
    }
}

/// How many history lines show beside the bar, logged or not: a docked strip holds the chosen
/// number (at most half the panel); a floating bar as many as fit before the area's edge.
fn visible_lines(app: &CadApp, area: Rect, bar: Rect, below: bool) -> usize {
    let room = match docked(app) {
        Some(_) => area.height() * 0.5 - BAR_H - 4.0,
        None if below => area.bottom() - bar.bottom() - 8.0,
        None => bar.top() - area.top() - 8.0,
    };
    app.ui.history_lines.min(MAX_LINES).min((room.max(0.0) / LINE_H) as usize)
}

/// The docked strip: the bar and its history lines (`None` unless docked).
fn strip(app: &CadApp, area: Rect) -> Option<Rect> {
    let dock = docked(app)?;
    let (bar, below) = bar_rect(app, area);
    let n = visible_lines(app, area, bar, below);
    let h = BAR_H + if n > 0 { LINE_H * n as f32 + 4.0 } else { 0.0 };
    Some(if dock == Dock::Top {
        Rect::from_min_size(area.min, vec2(area.width(), h))
    } else {
        Rect::from_min_max(pos2(area.left(), area.bottom() - h), area.max)
    })
}

/// The drawing area left beside a docked command line (all of `area` otherwise).
fn drawing_area(app: &CadApp, area: Rect) -> Rect {
    let mut r = area;
    match (docked(app), strip(app, area)) {
        (Some(Dock::Top), Some(s)) => r.min.y = s.max.y,
        (_, Some(s)) => r.max.y = s.min.y,
        _ => {}
    }
    r
}

/// The drawing panel's body: the drawing, with the command line docked above or below it.
pub fn panel(app: &mut CadApp, ui: &mut egui::Ui) {
    if docked(app).is_none() {
        crate::canvas::show(app, ui);
        return;
    }
    let area = ui.max_rect();
    let drawing = drawing_area(app, area);
    ui.scope_builder(egui::UiBuilder::new().max_rect(drawing), |ui| crate::canvas::show(app, ui));
    draw(app, ui, area);
}

/// The floating command line over the drawing (`canvas`, the whole drawing panel); [`panel`]
/// draws a docked one.
pub fn show(app: &mut CadApp, ui: &mut egui::Ui, canvas: Rect) {
    if docked(app).is_none() {
        draw(app, ui, canvas);
    }
}

fn draw(app: &mut CadApp, ui: &mut egui::Ui, area: Rect) {
    let t = Tokens::get();
    if let Some((_, pointer)) = app.cmd.moving.as_mut()
        && let Some(p) = ui.input(|i| i.pointer.latest_pos())
    {
        *pointer = p;
    }
    let p = ui.painter_at(area);
    // The docked strip's background, under the history window.
    if let Some(s) = strip(app, area) {
        p.rect_filled(s, 0.0, t.cmd_bg);
        p.hline(s.x_range(), if docked(app) == Some(Dock::Top) { s.bottom() } else { s.top() }, Stroke::new(1.0, t.border));
    }
    history(app, ui, area);
    let z = zones(app, area);
    let (bar, h) = (z.bar, BAR_H);
    // Moving: where it docks if dropped now.
    if let Some((grab, pointer)) = app.cmd.moving
        && let Some(dock) = drop_dock(pointer - grab, area)
    {
        let y = if dock == Dock::Top { area.top() } else { area.bottom() - h };
        p.rect_filled(Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), h)), 0.0, t.accent.gamma_multiply(0.35));
    }
    // History lines beside the bar (the expanded window replaces them), newest nearest it.
    if let Some(r) = z.lines {
        let lines: Vec<String> = app.session.log.iter().rev().take(z.slots).rev().cloned().collect();
        if z.strip.is_none() {
            p.rect_filled(r, 3.0, t.cmd_history);
        }
        let top = if z.below { r.top() + 1.0 } else { r.bottom() - 1.0 - LINE_H * lines.len() as f32 };
        for (i, l) in lines.iter().enumerate() {
            p.text(pos2(r.left() + 8.0, top + LINE_H * i as f32), egui::Align2::LEFT_TOP, crate::i18n::line(l), crate::theme::small(), t.text_dim);
        }
    }
    let radius = if z.strip.is_some() { 0.0 } else { 3.0 };
    p.rect_filled(bar, radius, t.cmd_bg);
    p.rect_stroke(bar, radius, Stroke::new(1.0, t.cmd_border), egui::StrokeKind::Inside);
    // The move handle: two columns of dots.
    for i in 0..8 {
        let at = pos2(z.handle.left() + 4.5 + 3.5 * (i % 2) as f32, bar.center().y - 5.25 + 3.5 * (i / 2) as f32);
        p.circle_filled(at, 1.0, t.text_faint);
    }
    // Prompt glyph.
    p.text(pos2(bar.left() + HANDLE_W + 4.0, bar.center().y), egui::Align2::LEFT_CENTER, ">_", crate::theme::mono(), t.text_dim);
    let mut x = bar.left() + HANDLE_W + 26.0;
    let prompt = app.session.current_prompt();
    let font = crate::theme::body();
    match &prompt {
        Some(pr) => {
            // The grip modes name themselves ("** STRETCH **…").
            let name = app.session.running.as_ref().filter(|r| r.id != "grip").map(|r| r.shown() + " ").unwrap_or_default();
            let g = p.layout_no_wrap(name, font.clone(), t.text_faint);
            let gw = g.size().x;
            p.galley(pos2(x, bar.center().y - g.size().y / 2.0), g, t.text_faint);
            x += gw;
            // A LISP `get…` prompt shows as the routine wrote it (no keywords, default or colon added).
            let message = if pr.raw { pr.display() } else { crate::i18n::line(&pr.message) };
            let g = p.layout_no_wrap(message, font.clone(), t.text);
            let gw = g.size().x;
            p.galley(pos2(x, bar.center().y - g.size().y / 2.0), g, t.text);
            x += gw;
            if !pr.raw && !pr.keywords.is_empty() {
                let open = match pr.keywords_open() {
                    " or [" => crate::tl!(" or ["),
                    open => open,
                };
                let g = p.layout_no_wrap(open.into(), font.clone(), t.text);
                let gw = g.size().x;
                p.galley(pos2(x, bar.center().y - g.size().y / 2.0), g, t.text);
                x += gw;
                let kws = pr.keywords.clone();
                for (i, k) in kws.iter().enumerate() {
                    let g = p.layout_no_wrap(k.clone(), font.clone(), t.cmd_keyword);
                    let r = Rect::from_min_size(pos2(x, bar.top() + 3.0), vec2(g.size().x, h - 6.0));
                    let resp = ui.interact(r, ui.id().with(("kw", i)), Sense::click());
                    if resp.hovered() {
                        p.rect_filled(r, 2.0, t.cmd_keyword_hover);
                    }
                    let gw = g.size().x;
                    p.galley(pos2(x, bar.center().y - g.size().y / 2.0), g, t.cmd_keyword);
                    x += gw;
                    if resp.clicked() {
                        let _ = app.session.input(Input::Keyword(k.clone()));
                    }
                    if i + 1 < kws.len() {
                        let g = p.layout_no_wrap("/".into(), font.clone(), t.text);
                        let gw = g.size().x;
                        p.galley(pos2(x, bar.center().y - g.size().y / 2.0), g, t.text);
                        x += gw;
                    }
                }
                let g = p.layout_no_wrap("]".into(), font.clone(), t.text);
                let gw = g.size().x;
                p.galley(pos2(x, bar.center().y - g.size().y / 2.0), g, t.text);
                x += gw;
            }
            if let Some(d) = pr.default.as_ref().filter(|_| !pr.raw) {
                let g = p.layout_no_wrap(format!(" <{d}>"), font.clone(), t.text);
                let gw = g.size().x;
                p.galley(pos2(x, bar.center().y - g.size().y / 2.0), g, t.text);
                x += gw;
            }
            if !pr.raw {
                let g = p.layout_no_wrap(": ".into(), font.clone(), t.text);
                let gw = g.size().x;
                p.galley(pos2(x, bar.center().y - g.size().y / 2.0), g, t.text);
                x += gw;
            } else if !pr.message.ends_with(' ') {
                x += 4.0;
            }
        }
        None => {
            if app.cmd.buffer.is_empty() {
                p.text(
                    pos2(x, bar.center().y),
                    egui::Align2::LEFT_CENTER,
                    crate::tl!("Type a command"),
                    egui::FontId::proportional(12.5),
                    t.text_faint,
                );
            }
        }
    }
    let g = p.layout_no_wrap(app.cmd.buffer.clone(), font, t.text);
    let gw = g.size().x;
    p.galley(pos2(x, bar.center().y - g.size().y / 2.0), g, t.text);
    // Caret.
    let blink = (ui.input(|i| i.time) * 2.0).floor() as i64 % 2 == 0;
    if blink {
        p.line_segment([pos2(x + gw + 1.0, bar.top() + 5.0), pos2(x + gw + 1.0, bar.bottom() - 5.0)], Stroke::new(1.0, t.text));
    }
    // History chevron.
    crate::icons::paint(
        &p,
        Rect::from_center_size(pos2(bar.right() - 14.0, bar.center().y), vec2(14.0, 14.0)),
        // Pointing where the history window opens, or back when it is open.
        if app.cmd.expanded != z.below { crate::icons::Icon::ChevronDown } else { crate::icons::Icon::ChevronUp },
        false,
    );
    // AutoComplete list.
    if let Some(lr) = z.list {
        let sug = suggestions(&app.cmd.buffer);
        let rh = 20.0;
        p.rect_filled(lr, 3.0, t.list_bg);
        p.rect_stroke(lr, 3.0, Stroke::new(1.0, t.cmd_border), egui::StrokeKind::Inside);
        for (i, s) in sug.iter().enumerate() {
            let r = Rect::from_min_size(pos2(lr.left(), lr.top() + rh * i as f32), vec2(lr.width(), rh));
            let resp = ui.interact(r, ui.id().with(("sug", i)), Sense::click());
            if resp.hovered() || i == 0 {
                p.rect_filled(r.shrink(1.0), 2.0, if resp.hovered() { t.cmd_keyword_hover } else { t.list_row });
            }
            p.text(Pos2::new(r.left() + 8.0, r.center().y), egui::Align2::LEFT_CENTER, &s.display, crate::theme::body(), t.text);
            p.text(
                Pos2::new(r.right() - 8.0, r.center().y),
                egui::Align2::RIGHT_CENTER,
                crate::i18n::t(s.label),
                crate::theme::small(),
                t.text_faint,
            );
            if resp.clicked() {
                app.cmd.buffer.clear();
                app.start(&s.id.to_ascii_lowercase());
            }
        }
    }
}

/// The command line's areas, laid out like [`draw`] draws them.
struct Zones {
    /// The input line.
    bar: Rect,
    /// The history and AutoComplete open below the bar (it sits high), not above.
    below: bool,
    /// The docked strip: the bar and its history lines.
    strip: Option<Rect>,
    /// How many history lines show beside the bar.
    slots: usize,
    /// The history lines (when there are any and the window is closed).
    lines: Option<Rect>,
    /// The expanded history window.
    window: Option<Rect>,
    chevron: Rect,
    /// The move handle at the bar's left end.
    handle: Rect,
    /// The far edge that resizes the history lines.
    grip: Rect,
    /// The AutoComplete list.
    list: Option<Rect>,
    /// Everything, AutoComplete list included: kept away from the drawing underneath.
    all: Rect,
}

fn zones(app: &CadApp, area: Rect) -> Zones {
    let (bar, below) = bar_rect(app, area);
    let strip = strip(app, area);
    let n = visible_lines(app, area, bar, below);
    // A docked strip keeps its lines while the log is shorter; a floating bar shows what's logged.
    let slots = if app.cmd.expanded {
        0
    } else if strip.is_some() {
        n
    } else {
        n.min(app.session.log.len())
    };
    // A rect `h` high on the history's side of the bar.
    let beside = |gap: f32, h: f32| {
        let y = if below { bar.bottom() + gap } else { bar.top() - gap - h };
        Rect::from_min_size(pos2(bar.left(), y), vec2(bar.width(), h))
    };
    let lines = (slots > 0).then(|| beside(2.0, LINE_H * slots as f32 + 2.0));
    let window = app.cmd.expanded.then(|| {
        // As tall as the log (at least three lines), at most half the drawing area.
        let full = LINE_H * app.session.log.len().max(3) as f32 + 4.0;
        beside(2.0, full.min((area.height() * 0.5).max(3.0 * LINE_H)) + 2.0)
    });
    let far = window.or(lines).unwrap_or(bar);
    let edge = if below { far.bottom() } else { far.top() };
    let grip = Rect::from_min_max(pos2(bar.left(), edge - 3.0), pos2(bar.right(), edge + 3.0));
    let k = if app.session.prompting() { 0 } else { suggestions(&app.cmd.buffer).len() };
    let list = (k > 0).then(|| {
        let h = 20.0 * k as f32;
        let y = match lines {
            Some(r) if below => r.bottom() + 8.0,
            Some(r) => r.top() - 8.0 - h,
            None if below => bar.bottom() + 6.0,
            None => bar.top() - 6.0 - h,
        };
        Rect::from_min_size(pos2(bar.left() + 24.0, y), vec2((bar.width() - 24.0).clamp(0.0, 736.0), h))
    });
    let mut all = bar.union(grip);
    for r in [strip, list].into_iter().flatten() {
        all = all.union(r);
    }
    Zones {
        bar,
        below,
        strip,
        slots,
        lines,
        window,
        chevron: Rect::from_center_size(pos2(bar.right() - 14.0, bar.center().y), vec2(24.0, 22.0)),
        handle: Rect::from_min_size(bar.min, vec2(HANDLE_W, BAR_H)),
        grip,
        list,
        all,
    }
}

/// The pointer on the command line: one widget under all of it keeps clicks, drags and the wheel
/// off the drawing (the keywords and AutoComplete rows sit on top of it). Clicking the history
/// lines or the chevron opens/closes the history window (`ui.toggle.history`, also F2); dragging
/// the far edge sets how many lines show (`ui.cmdline.lines`); dragging the handle moves it, and
/// dropping it against the drawing area's top or bottom edge docks it (`ui.cmdline.position`).
fn history(app: &mut CadApp, ui: &mut egui::Ui, area: Rect) {
    let z = zones(app, area);
    // One id wherever it is drawn, so a move started docked carries on floating.
    let resp = ui.interact(z.all, egui::Id::new("cadcraft-cmdline"), Sense::click_and_drag());
    let at = resp.hover_pos().or(resp.interact_pointer_pos());
    let on_grip = |p: Pos2| z.grip.contains(p) && z.window.is_none();
    let on_handle = |p: Pos2| z.handle.contains(p) && !on_grip(p);
    let on_toggle = |p: Pos2| z.chevron.contains(p) || z.lines.is_some_and(|r| r.contains(p));
    if app.cmd.moving.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if app.cmd.resize_from.is_some() || at.is_some_and(on_grip) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    } else if at.is_some_and(on_handle) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    } else if at.is_some_and(on_toggle) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.drag_started()
        && let Some(o) = ui.input(|i| i.pointer.press_origin())
    {
        if on_grip(o) {
            app.cmd.resize_from = Some((o.y, visible_lines(app, area, z.bar, z.below)));
        } else if on_handle(o) {
            app.cmd.moving = Some((o - z.bar.min, o));
        }
    }
    if let Some((y0, n0)) = app.cmd.resize_from {
        if let (true, Some(p)) = (resp.dragged(), resp.interact_pointer_pos()) {
            let dy = if z.below { p.y - y0 } else { y0 - p.y };
            let n = (n0 as f32 + dy / LINE_H).round().clamp(0.0, MAX_LINES as f32) as usize;
            if n != app.ui.history_lines {
                let _ = app.run("ui.cmdline.lines", json!({ "lines": n }));
            }
        } else {
            app.cmd.resize_from = None;
        }
    }
    if let Some((grab, pointer)) = app.cmd.moving
        && !resp.dragged()
    {
        app.cmd.moving = None;
        let params = match drop_dock(pointer - grab, area) {
            Some(dock) => json!({ "dock": dock }),
            None => {
                let room = (area.size() - z.bar.size()).max(Vec2::ZERO);
                let d = z.bar.min - area.min;
                let frac = |d: f32, room: f32| if room > 0.0 { f64::from(d) / f64::from(room) } else { 0.0 };
                json!({ "dock": Dock::Floating, "at": [frac(d.x, room.x), frac(d.y, room.y)] })
            }
        };
        let _ = app.run("ui.cmdline.position", params);
    }
    if resp.clicked() && resp.interact_pointer_pos().is_some_and(on_toggle) {
        let _ = app.run("ui.toggle.history", json!({}));
    }
    crate::shortcut_menus::attach(app, &resp, |app| {
        let mut groups = crate::shortcut_menus::command_line_entries(app);
        // Placement goes before the last group (Copy History, Clear).
        groups.insert(groups.len().saturating_sub(1), position_entries(app));
        groups
    });
    if let Some(r) = zones(app, area).window {
        history_window(app, ui, r);
    }
}

/// The expanded history: the whole command log, scrollable, newest at the bottom. Lines show in
/// the interface language ([`crate::i18n::line`]); the log itself stays English.
fn history_window(app: &CadApp, ui: &mut egui::Ui, r: Rect) {
    let t = Tokens::get();
    ui.painter().rect_filled(r, 3.0, t.cmd_bg);
    ui.painter().rect_stroke(r, 3.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let inner = r.shrink2(vec2(8.0, 3.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
    child.spacing_mut().item_spacing.y = 0.0;
    let log = &app.session.log;
    egui::ScrollArea::vertical().id_salt("cmd_history").stick_to_bottom(true).auto_shrink([false, false]).show_rows(
        &mut child,
        LINE_H,
        log.len(),
        |ui, rows| {
            for l in log.get(rows).into_iter().flatten() {
                let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), LINE_H), Sense::hover());
                let p = ui.painter().with_clip_rect(row.intersect(ui.clip_rect()));
                p.text(pos2(row.left(), row.center().y), egui::Align2::LEFT_CENTER, crate::i18n::line(l), crate::theme::small(), t.text_dim);
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;
    use egui::{Event, PointerButton};

    #[test]
    fn compact_canvas_contains_history_controls_without_changing_line_preference() {
        let mut app = CadApp::new(Session::empty(), crate::Services::default());
        app.ui.history_lines = 12;
        for i in 0..20 {
            app.session.echo(format!("line {i}"));
        }
        let canvas = Rect::from_min_size(Pos2::ZERO, vec2(300.0, 180.0));
        let z = zones(&app, canvas);
        assert!(canvas.contains_rect(z.bar));
        assert!(canvas.contains_rect(z.chevron));
        assert!(z.lines.is_some_and(|r| canvas.contains_rect(r)));
        assert!(canvas.contains_rect(z.grip));
        assert_eq!(app.ui.history_lines, 12);
    }

    fn frame(app: &mut CadApp, ctx: &egui::Context, events: Vec<Event>) {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1400.0, 900.0))), events, ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            app.logic(ui.ctx());
            app.ui(ui);
        });
        out.textures_delta.clear();
    }

    fn press(app: &mut CadApp, ctx: &egui::Context, at: Pos2, pressed: bool) {
        frame(app, ctx, vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: egui::Modifiers::default() }]);
    }

    /// A point typed at the command line (or into Dynamic Input) is on the XY plane: a height left
    /// from a 3D snap is dropped before the point goes to the command.
    #[test]
    fn typed_point_drops_a_3d_snap_height() {
        let mut app = CadApp::new(Session::new(), crate::Services::default());
        app.session.cmdline("line").unwrap();
        app.session.point_z = Some(20.0);
        app.cmd.buffer = "3,4".into();
        submit(&mut app);
        assert_eq!((app.session.last_point, app.session.point_z), (cadcraft_geom::Vec2::new(3.0, 4.0), None));
    }

    /// A point typed as `x,y,z` keeps its height for the commands that build in 3D; `@dx,dy,dz`
    /// adds to the last point's, also through Dynamic Input, where `x,y,z` is relative like `x,y`.
    #[test]
    fn typed_3d_point_keeps_its_height() {
        let mut app = CadApp::new(Session::new(), crate::Services::default());
        app.session.settings.dynmode = true;
        app.session.cmdline("line").unwrap();
        app.session.point_z = Some(20.0);
        app.cmd.buffer = "3,4,5".into();
        submit(&mut app);
        assert_eq!((app.session.last_point, app.session.point_z), (cadcraft_geom::Vec2::new(3.0, 4.0), Some(5.0)));
        app.cmd.buffer = "@1,0,2".into();
        submit(&mut app);
        assert_eq!((app.session.last_point, app.session.point_z), (cadcraft_geom::Vec2::new(4.0, 4.0), Some(7.0)));
        // Dynamic Input: measured from the last point, height included.
        app.cmd.buffer = "1,1,1".into();
        submit(&mut app);
        assert_eq!((app.session.last_point, app.session.point_z), (cadcraft_geom::Vec2::new(5.0, 5.0), Some(8.0)));
        // Plain `x,y` is on the XY plane (with Dynamic Input on it would be relative, `@x,y`).
        app.session.settings.dynmode = false;
        app.cmd.buffer = "9,9".into();
        submit(&mut app);
        assert_eq!((app.session.last_point, app.session.point_z), (cadcraft_geom::Vec2::new(9.0, 9.0), None));
    }

    #[test]
    fn history_expands_by_click_chevron_f2_and_resizes_by_drag_without_touching_the_drawing() {
        let mut app = CadApp::new(Session::empty(), crate::Services::default());
        app.start("ui.sample");
        for i in 0..20 {
            app.session.echo(format!("line {i}"));
        }
        let ctx = egui::Context::default();
        frame(&mut app, &ctx, Vec::new());
        frame(&mut app, &ctx, Vec::new());
        let canvas = app.canvas.rect.unwrap_or(Rect::NOTHING);
        let z = zones(&app, canvas);
        let Some(lines) = z.lines else { panic!("three history lines show by default") };
        // A click on the history lines opens the history window, and doesn't start a selection.
        for (at, pressed) in [(lines.center(), true), (lines.center(), false)] {
            frame(&mut app, &ctx, vec![Event::PointerMoved(at)]);
            press(&mut app, &ctx, at, pressed);
        }
        frame(&mut app, &ctx, Vec::new());
        assert!(app.cmd.expanded);
        assert!(app.session.pending_window.is_none(), "the click stayed on the command line");
        // The chevron closes it; F2 and TEXTSCR open it.
        let chevron = z.chevron.center();
        press(&mut app, &ctx, chevron, true);
        press(&mut app, &ctx, chevron, false);
        frame(&mut app, &ctx, Vec::new());
        assert!(!app.cmd.expanded);
        let f2 = |pressed| Event::Key { key: Key::F2, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE };
        frame(&mut app, &ctx, vec![f2(true), f2(false)]);
        assert!(app.cmd.expanded);
        assert_eq!(app.run("ui.toggle.history", serde_json::json!({"on": false})), Ok(serde_json::json!({"on": false})));
        app.cmdline("textscr");
        assert!(app.cmd.expanded);
        let _ = app.run("ui.toggle.history", serde_json::json!({"on": false}));
        frame(&mut app, &ctx, Vec::new());
        // Dragging the top edge up by two lines shows two more lines (capped).
        let top = pos2(lines.center().x, lines.top() - 1.0);
        frame(&mut app, &ctx, vec![Event::PointerMoved(top)]);
        press(&mut app, &ctx, top, true);
        for dy in [8.0, 20.0, 2.0 * LINE_H] {
            frame(&mut app, &ctx, vec![Event::PointerMoved(top - vec2(0.0, dy))]);
        }
        press(&mut app, &ctx, top - vec2(0.0, 2.0 * LINE_H), false);
        frame(&mut app, &ctx, Vec::new());
        assert_eq!(app.ui.history_lines, 5);
        assert!(!app.cmd.expanded, "a drag isn't a click");
        assert!(app.session.pending_window.is_none());
        assert_eq!(app.run("ui.cmdline.lines", serde_json::json!({"lines": 99})), Ok(serde_json::json!({"lines": MAX_LINES})));
    }
}

#[cfg(test)]
mod delete_key_tests {
    use super::*;
    use cadcraft_doc::Handle;

    const PRESS: KeyPress = KeyPress { repeat: false, plain: true };

    #[test]
    fn mac_delete_erases_a_fresh_selection() {
        assert!(backspace_erases(true, PRESS, "", false, true, false));
        // Not on Windows/Linux, where Backspace never deletes objects.
        assert!(!backspace_erases(false, PRESS, "", false, true, false));
        // Nothing selected, a command running, or text on the command line: just edit text.
        assert!(!backspace_erases(true, PRESS, "", false, false, false));
        assert!(!backspace_erases(true, PRESS, "", true, true, false));
        assert!(!backspace_erases(true, PRESS, "m", false, true, false));
        // Option/Cmd+delete are text-editing gestures on macOS.
        assert!(!backspace_erases(true, KeyPress { plain: false, ..PRESS }, "", false, true, false));
    }

    #[test]
    fn holding_delete_to_clear_typing_never_erases() {
        // Auto-repeat after the last character is gone.
        assert!(!backspace_erases(true, KeyPress { repeat: true, ..PRESS }, "", false, true, false));
        // Typed, then cleared with separate presses: still not erased.
        assert!(!backspace_erases(true, PRESS, "", false, true, true));
    }

    #[test]
    fn typing_is_forgotten_when_the_selection_changes() {
        let mut c = CmdLine::default();
        c.watch_selection(&[Handle(1)]);
        c.buffer.push('m');
        c.note_typing();
        c.buffer.clear();
        c.watch_selection(&[Handle(1)]);
        assert!(c.typed_since_selection);
        c.watch_selection(&[Handle(1), Handle(2)]);
        assert!(!c.typed_since_selection);
    }
}

#[cfg(test)]
mod autocomplete_tests {
    use super::*;

    #[test]
    fn exact_alias_is_first_suggestion_with_alias_display() {
        let rec = suggestions("rec");
        assert!(!rec.is_empty());
        assert_eq!(rec[0].id, "RECTANG");
        assert_eq!(rec[0].display, "REC (RECTANG)");
        assert_eq!(rec[0].label, "Rectangle");
        // RECOVER should follow RECTANG, not precede it
        assert!(rec.iter().any(|s| s.id == "RECOVER"));
        assert_eq!(rec[1].id, "RECOVER");
        assert_eq!(rec[1].display, "RECOVER");

        let l = suggestions("l");
        assert_eq!(l[0].id, "LINE");
        assert_eq!(l[0].display, "L (LINE)");

        let c = suggestions("c");
        assert_eq!(c[0].id, "CIRCLE");
        assert_eq!(c[0].display, "C (CIRCLE)");

        let co = suggestions("co");
        assert_eq!(co[0].id, "COPY");
        assert_eq!(co[0].display, "CO (COPY)");
    }

    #[test]
    fn prefix_without_alias_shows_command_id() {
        let rect = suggestions("rect");
        assert_eq!(rect[0].id, "RECTANG");
        assert_eq!(rect[0].display, "RECTANG");

        let line = suggestions("line");
        assert_eq!(line[0].id, "LINE");
        assert_eq!(line[0].display, "LINE");
    }

    #[test]
    fn tab_autocompletes_alias_to_resolved_command() {
        let mut app = CadApp::new(cadcraft_engine::Session::empty(), crate::Services::default());
        app.cmd.buffer = "rec".to_string();
        tab(&mut app);
        assert_eq!(app.cmd.buffer, "RECTANG");
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    /// Type `text` and end it with `end`, as keystrokes through [`keyboard`].
    fn type_line(app: &mut CadApp, ctx: &egui::Context, text: &str, end: egui::Event) {
        let events = vec![egui::Event::Text(text.into()), end];
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))),
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| keyboard(app, ui.ctx()));
        out.textures_delta.clear();
    }

    fn unknown(app: &CadApp) -> bool {
        app.session.log.iter().any(|l| l.starts_with("unknown command"))
    }

    /// AutoComplete: Enter or Space on a partial name runs the highlighted (first) suggestion.
    #[test]
    fn enter_or_space_runs_the_first_suggestion_of_a_partial_name() {
        let ctx = egui::Context::default();
        // (typed, first suggestion, what shows it ran: the log, the prompt or the dialog).
        let cases = [
            ("mirr", "mirror", "command: mirror"),
            ("OFFS", "offset", "command: offset"),
            ("polyg", "polygon", "command: polygon"),
            ("rectan", "rectang", "command: rectang"),
            ("xli", "xline", "command: xline"),
            ("dimli", "dimlinear", "command: dimlinear"),
            ("layis", "layiso", "command: layiso"),
            ("hatche", "hatchedit", "select hatch object"),
            ("filletedg", "filletedge", "filletedge"),
            ("extr", "extrude", "extrude"),
            ("dimst", "dimstyle", "dimstyle"),
            ("dimsty", "dimstyle", "dimstyle"),
        ];
        for (typed, id, sign) in cases {
            for end in [key(egui::Key::Enter), egui::Event::Text(" ".into())] {
                let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
                let want = suggestions(typed).first().map(|s| s.id.to_ascii_lowercase());
                assert_eq!(want.as_deref(), Some(id), "{typed} suggests {id} first");
                type_line(&mut app, &ctx, typed, end);
                assert!(!unknown(&app), "{typed}: {:?}", app.session.log);
                let seen = format!("{:?} {} {:?}", app.session.log, app.session.prompt_text(), app.ui.dialog).to_lowercase();
                assert!(seen.contains(sign), "{typed} ran {id}: {seen}");
            }
        }
    }

    /// Whatever runs as typed still does: a UI name (AI is not AI_DESELECT), a LISP command named
    /// like a command's prefix, a whole command name; and at a prompt the text is the answer.
    #[test]
    fn a_name_that_runs_as_typed_is_not_completed() {
        let ctx = egui::Context::default();
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        assert!(suggestions("ai").iter().any(|s| s.id == "AI_DESELECT"));
        type_line(&mut app, &ctx, "ai", key(egui::Key::Enter));
        assert!(app.ui.docking.layout.contains(&crate::docking::Panel::Assistant), "AI opens the assistant");
        assert!(!app.session.log.iter().any(|l| l.contains("AI_DESELECT")));

        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.cmdline("(defun c:mirr () (setq ran 1))");
        type_line(&mut app, &ctx, "mirr", key(egui::Key::Enter));
        assert!(app.session.running.is_none(), "the LISP command ran, not MIRROR");
        assert!(app.session.log.iter().any(|l| l == "Command: MIRR"), "{:?}", app.session.log);

        // At a prompt the typed text goes to the command.
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.cmdline("line");
        type_line(&mut app, &ctx, "mirr", key(egui::Key::Enter));
        assert_eq!(app.session.running.as_ref().map(|r| r.id.as_str()), Some("line"));

        // Nothing suggested: the refusal stays.
        type_line(&mut app, &ctx, "", key(egui::Key::Escape));
        type_line(&mut app, &ctx, "zzqx", key(egui::Key::Enter));
        assert!(unknown(&app));
    }
}

#[cfg(test)]
mod position_tests {
    use super::*;
    use cadcraft_engine::Session;
    use egui::{Event, Modifiers, PointerButton};
    use egui_kittest::Harness;

    const SIZE: Vec2 = vec2(1280.0, 800.0);

    fn harness(app: CadApp, size: Vec2) -> Harness<'static, CadApp> {
        let mut h = Harness::builder().with_size(size).build_ui_state(
            |ui, app: &mut CadApp| {
                app.logic(&ui.ctx().clone());
                app.ui(ui);
            },
            app,
        );
        h.run_steps(4);
        h
    }

    fn app(position: Position) -> CadApp {
        let mut app = CadApp::new(Session::new(), crate::Services::default());
        for i in 0..5 {
            app.session.echo(format!("line {i}"));
        }
        app.ui.command_line_position = position;
        app
    }

    /// The drawing panel at `size` (the drawing area while the command line floats).
    fn area(size: Vec2) -> Rect {
        let h = harness(app(Position::default()), size);
        h.state().canvas.rect.unwrap_or(Rect::NOTHING)
    }

    fn drawing(h: &Harness<'_, CadApp>) -> Rect {
        h.state().canvas.rect.unwrap_or(Rect::NOTHING)
    }

    fn drag(h: &mut Harness<'_, CadApp>, from: Pos2, to: Pos2) {
        h.event(Event::PointerMoved(from));
        h.run_steps(2);
        h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.run_steps(2);
        for step in 1..=8 {
            h.event(Event::PointerMoved(from.lerp(to, step as f32 / 8.0)));
            h.run_steps(1);
        }
        h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        h.run_steps(4);
    }

    #[test]
    fn dragging_the_handle_floats_and_docks_it_and_the_place_persists() {
        let area = area(SIZE);
        let mut h = harness(app(Position::default()), SIZE);
        let z = zones(h.state(), area);
        assert_eq!((z.bar.center().x, z.below), (area.center().x, false), "bottom centre by default");

        // Into the middle of the drawing: it floats there, and the drawing saw no click.
        let to = area.center() + vec2(-200.0, -100.0);
        drag(&mut h, z.handle.center(), to);
        assert_eq!(h.state().ui.command_line_position.dock, Dock::Floating);
        let moved = zones(h.state(), area);
        assert!((moved.handle.center() - to).length() < 0.5, "{:?} {to:?}", moved.handle);
        assert!(moved.below, "above the middle: the history opens below");
        assert!(h.state().session.pending_window.is_none() && h.state().session.selection().is_empty());
        assert_eq!(drawing(&h), area, "a floating command line takes no room");

        // The saved workspace puts it back in the same place.
        let saved = serde_json::to_string(&h.state().ui).unwrap_or_default();
        let mut again = app(Position::default());
        again.ui = serde_json::from_str(&saved).unwrap_or_default();
        let reloaded = harness(again, SIZE);
        assert_eq!(zones(reloaded.state(), area).bar, moved.bar);

        // Against the top edge: docked there, full width, above the drawing.
        drag(&mut h, moved.handle.center(), pos2(moved.handle.center().x, area.top() - 40.0));
        assert_eq!(h.state().ui.command_line_position.dock, Dock::Top);
        let top = zones(h.state(), area);
        assert_eq!((top.bar.top(), top.bar.width(), top.below), (area.top(), area.width(), true));
        let strip = top.strip.unwrap_or(Rect::NOTHING);
        assert_eq!((drawing(&h).top(), drawing(&h).bottom()), (strip.bottom(), area.bottom()));
        assert!(strip.height() >= BAR_H + 3.0 * LINE_H, "three history lines under the bar");

        // From there against the bottom edge: docked below the drawing.
        drag(&mut h, top.handle.center(), pos2(top.handle.center().x, area.bottom() + 40.0));
        assert_eq!(h.state().ui.command_line_position.dock, Dock::Bottom);
        let bottom = zones(h.state(), area);
        assert_eq!((bottom.bar.bottom(), bottom.bar.width(), bottom.below), (area.bottom(), area.width(), false));
        let strip = bottom.strip.unwrap_or(Rect::NOTHING);
        assert_eq!((drawing(&h).top(), drawing(&h).bottom()), (area.top(), strip.top()));

        // Ctrl+9 hides it and gives the drawing its room back; again shows it docked.
        let ctrl = if cfg!(target_os = "macos") { Modifiers::MAC_CMD | Modifiers::COMMAND } else { Modifiers::CTRL | Modifiers::COMMAND };
        h.key_press_modifiers(ctrl, Key::Num9);
        h.run_steps(3);
        assert!(!h.state().ui.show_command_line);
        assert_eq!(drawing(&h), area);
        h.key_press_modifiers(ctrl, Key::Num9);
        h.run_steps(3);
        assert_eq!(drawing(&h).bottom(), strip.top());

        // The menu's Floating returns to where it floated; Reset Position to the bottom centre.
        let _ = h.state_mut().run("ui.cmdline.position", json!({"dock": "floating"}));
        h.run_steps(3);
        assert_eq!(zones(h.state(), area).bar, moved.bar);
        let _ = h.state_mut().run("ui.cmdline.position", json!({"dock": "floating", "at": null}));
        h.run_steps(3);
        assert_eq!(zones(h.state(), area).bar, z.bar);
    }

    #[test]
    fn a_floating_command_line_keeps_inside_a_resized_drawing_area() {
        // Dragged near the bottom-right corner, then the window shrinks and grows.
        let area = area(SIZE);
        let mut h = harness(app(Position::default()), SIZE);
        let z = zones(h.state(), area);
        drag(&mut h, z.handle.center(), area.right_bottom() - vec2(20.0, 30.0));
        assert_eq!(h.state().ui.command_line_position.dock, Dock::Floating, "not pushed against the edge");
        for size in [vec2(820.0, 560.0), vec2(1600.0, 1000.0)] {
            h.set_size(size);
            h.run_steps(4);
            let a = drawing(&h);
            let z = zones(h.state(), a);
            assert!(a.contains_rect(z.bar) && z.lines.is_some_and(|r| a.contains_rect(r)), "{size:?}: {:?} in {a:?}", z.bar);
            assert!((a.right() - z.bar.right()).abs() < 1.0, "{size:?}: still at the right");
        }
        // Hostile saved places are kept inside too.
        for at in [[5.0, -3.0], [f64::INFINITY, f64::NAN]] {
            h.state_mut().ui.command_line_position.at = Some(at);
            h.run_steps(2);
            let a = drawing(&h);
            assert!(a.contains_rect(zones(h.state(), a).bar), "{at:?}");
        }
    }

    #[test]
    fn history_and_autocomplete_open_away_from_the_edge() {
        let area = area(SIZE);
        let high = Position { dock: Dock::Floating, at: Some([0.5, 0.1]) };
        let cases = [
            (high, true),
            (Position::default(), false),
            (Position { dock: Dock::Top, at: None }, true),
            (Position { dock: Dock::Bottom, at: None }, false),
        ];
        for (position, below) in cases {
            let mut app = app(position);
            app.cmd.buffer = "l".into();
            let mut h = harness(app, SIZE);
            let z = zones(h.state(), area);
            let (Some(lines), Some(list)) = (z.lines, z.list) else { panic!("{position:?}: lines and list show") };
            assert_eq!(z.below, below, "{position:?}");
            if below {
                assert!(z.bar.bottom() < lines.top() && lines.bottom() < list.top(), "{position:?}");
            } else {
                assert!(list.bottom() < lines.top() && lines.bottom() < z.bar.top(), "{position:?}");
            }
            assert!(area.contains_rect(list), "{position:?}: {list:?}");
            // The flipped list still takes clicks: its first row runs LINE.
            let row = pos2(list.left() + 20.0, list.top() + 10.0);
            h.event(Event::PointerMoved(row));
            h.run_steps(1);
            for pressed in [true, false] {
                h.event(Event::PointerButton { pos: row, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
                h.run_steps(1);
            }
            h.run_steps(2);
            assert_eq!(h.state().session.running.as_ref().map(|r| r.id.as_str()), Some("line"), "{position:?}");
        }
    }

    #[test]
    fn position_command_checks_its_parameters() {
        let mut app = CadApp::new(Session::new(), crate::Services::default());
        assert_eq!(app.run("ui.cmdline.position", json!({"dock": "top"})), Ok(json!({"dock": "top", "at": null})));
        assert_eq!(app.run("ui.cmdline.position", json!({"at": [0.25, 2.0]})), Ok(json!({"dock": "top", "at": [0.25, 1.0]})));
        for bad in [json!({"dock": "left"}), json!({"dock": 3}), json!({"at": "x"}), json!({"at": [1.0]}), json!({"at": [null, 0]})] {
            assert!(app.run("ui.cmdline.position", bad.clone()).is_err(), "{bad}");
        }
        assert_eq!(app.ui.command_line_position, Position { dock: Dock::Top, at: Some([0.25, 1.0]) });
        assert_eq!(app.run("ui.cmdline.position", json!({"dock": "floating", "at": null})), Ok(json!({"dock": "floating", "at": null})));
    }
}
