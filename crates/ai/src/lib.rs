//! Cadino's AI assistant.
//!
//! [`tools`] is the tool set the MCP server exposes; [`run`] drives the same tools from a chat:
//! one user message in, the model's text, thinking and tool calls out as [`Event`]s, looping
//! while the model asks for tools. Providers: Anthropic Claude (Messages API), OpenAI GPT and
//! any OpenAI-compatible server (Ollama, LM Studio, llama.cpp, vLLM), all over raw HTTP with
//! server-sent events. The desktop app only: on wasm every request reports that.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

mod anthropic;
mod http;
pub mod keys;
mod openai;
pub mod prompt;
mod sse;
pub mod text;
pub mod tools;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Who answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[default]
    Anthropic,
    OpenAi,
    /// An OpenAI-compatible server, usually a local model.
    Custom,
}

impl ProviderKind {
    pub fn id(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenAi => "openai",
            ProviderKind::Custom => "custom",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "Anthropic Claude",
            ProviderKind::OpenAi => "OpenAI GPT",
            ProviderKind::Custom => "Local / custom (OpenAI-compatible)",
        }
    }
    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "https://api.anthropic.com",
            ProviderKind::OpenAi => "https://api.openai.com/v1",
            ProviderKind::Custom => "http://localhost:11434/v1",
        }
    }
    pub fn default_model(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "claude-opus-5-5",
            ProviderKind::OpenAi => "gpt-5",
            ProviderKind::Custom => "",
        }
    }
    pub fn suggested_models(self) -> &'static [&'static str] {
        match self {
            ProviderKind::Anthropic => &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1"],
            ProviderKind::OpenAi => &["gpt-5", "gpt-5-mini"],
            ProviderKind::Custom => &[],
        }
    }
    /// Whether requests need an API key (local servers usually don't).
    pub fn needs_key(self) -> bool {
        self != ProviderKind::Custom
    }
}

/// The assistant's settings (Options ▸ AI). The key is never saved with them: it lives in the
/// system keychain ([`keys`]) and the app fills `api_key` before a request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: ProviderKind,
    /// Empty = the provider's default model.
    pub model: String,
    /// Empty = the provider's default URL.
    pub base_url: String,
    #[serde(skip)]
    pub api_key: String,
    /// Anthropic's thinking effort: `low`, `medium`, `high`, `xhigh` or `max`.
    pub effort: String,
    pub max_tokens: u32,
    /// Appended to the system prompt.
    pub custom_instructions: String,
    /// Let the model open and save files.
    pub allow_files: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            provider: ProviderKind::Anthropic,
            model: ProviderKind::Anthropic.default_model().into(),
            base_url: String::new(),
            api_key: String::new(),
            effort: "high".into(),
            max_tokens: 32000,
            custom_instructions: String::new(),
            allow_files: false,
        }
    }
}

impl Config {
    /// The model to ask: the configured one or the provider's default.
    fn model(&self) -> &str {
        let m = self.model.trim();
        if m.is_empty() { self.provider.default_model() } else { m }
    }
    /// The base URL without a trailing slash.
    fn base(&self) -> &str {
        let b = self.base_url.trim();
        if b.is_empty() { self.provider.default_base_url() } else { b.trim_end_matches('/') }
    }
    /// The system prompt with the user's own instructions; the compact one for a local model,
    /// whose server may have a small context (see [`prompt::COMPACT`]).
    fn system(&self) -> String {
        let base = if self.provider == ProviderKind::Custom { prompt::COMPACT } else { prompt::SYSTEM };
        let custom = self.custom_instructions.trim();
        if custom.is_empty() { base.to_string() } else { format!("{base}\n\n# User instructions\n{custom}") }
    }
    /// What to tell the user about a failed request.
    fn failure(&self, f: http::Failure) -> String {
        match f {
            http::Failure::Unreachable(_) if self.provider == ProviderKind::Custom => text::CANT_REACH.replace("{base}", self.base()),
            f => f.message(),
        }
    }
}

/// What happens during [`run`], in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// More of the model's answer.
    Text(String),
    /// More of the model's (summarised) reasoning.
    Thinking(String),
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        id: String,
        ok: bool,
        text: String,
        image_png_base64: Option<String>,
    },
    /// One model response finished; `usage` has `input_tokens`, `output_tokens`,
    /// `cache_read_input_tokens` and `cache_creation_input_tokens` when the provider reports them.
    TurnDone {
        stop_reason: String,
        usage: Value,
    },
    /// Something the user should know (a retry, a limit, a stop).
    Notice(String),
    /// The model declined the request.
    Error(String),
}

/// A chat with one provider and model, in that provider's own message format. Append-only:
/// earlier turns are never edited, nor the system prompt and tools sent with them (Claude's
/// thinking blocks are bound to all of it, and prompt caching depends on it).
#[derive(Clone, Debug, Default)]
pub struct Conversation {
    messages: Vec<Value>,
    /// The provider and model the messages were written for.
    model: Option<(ProviderKind, String)>,
    /// The system prompt the messages were written under.
    system: String,
    /// A custom server refused images: requests leave the tools' images out.
    no_images: bool,
    /// The tools' images in `messages`, and their base64 bytes ([`admit_image`]).
    images: usize,
    image_bytes: usize,
}

impl Conversation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// Tool rounds per user message before the loop stops.
const MAX_ROUNDS: usize = 40;
/// Tool result text beyond this many characters is cut.
const MAX_RESULT_CHARS: usize = 30_000;
/// The same for a local model: a server cuts the start of a prompt that its last message alone
/// overflows (Ollama's context is often 4096 or 8192 tokens), and with it the system prompt and
/// the tool format.
const MAX_LOCAL_RESULT_CHARS: usize = 8_000;
/// The longest side of an image sent to the model: Claude refuses larger ones once a request
/// holds more than 20 images, which a chat that renders after every change soon does.
const MAX_IMAGE_SIDE: u32 = 2000;
/// The largest image sent, in base64 bytes (Claude's limit is 10 MB, 5 MB on some clouds).
const MAX_IMAGE_BYTES: usize = 5 << 20;
/// Images one conversation sends at most, and their bytes: every request carries them all again
/// (Claude takes 32 MB per request), and earlier ones can't be dropped without breaking the
/// thinking blocks after them.
const MAX_CONVERSATION_IMAGES: usize = 50;
const MAX_CONVERSATION_IMAGE_BYTES: usize = 20 << 20;
/// The answer to a call that would read or write a file while the user keeps the assistant from
/// files; the app also denies file access with it while it serves the assistant.
pub const FILES_OFF: &str = "File access is off for the assistant (Options ▸ AI).";
const SETTINGS_OFF: &str = "The AI and MCP settings are the user's to change (Options ▸ AI).";
/// The answer to a call that would close a drawing or quit; the app also denies closing with it.
pub const CLOSE_OFF: &str = "Closing drawings and quitting are left to the user: unsaved changes would be lost without asking.";
const CANCELLED: &str = "Cancelled by the user";

/// A tool call the model made.
#[derive(Clone, Debug, PartialEq)]
struct Call {
    id: String,
    name: String,
    /// The arguments, or the raw text when it was not a JSON object.
    input: Result<Value, String>,
}

/// A tool's answer.
#[derive(Clone, Debug, PartialEq)]
struct Output {
    ok: bool,
    text: String,
    image: Option<String>,
}

impl Output {
    fn error(text: impl Into<String>) -> Self {
        Output { ok: false, text: text.into(), image: None }
    }
}

/// What a model response asks for next.
#[derive(Debug, PartialEq)]
enum Next {
    /// The turn is over.
    Done,
    /// Run these tools and send the results.
    Tools(Vec<Call>),
    /// The reply was cut off: answer each call with this error instead of running it.
    Refuse(Vec<Call>, String),
    /// Send the conversation again as it is (`pause_turn`).
    Again,
    /// Stopped by the user before the response was complete; nothing was appended.
    Cancelled,
}

/// Answer `user_text`: ask the model, run the tools it calls on `backend`, and repeat until it
/// is done, at most 40 tool rounds. Progress arrives through `on_event`. Setting `cancel` stops
/// while waiting for the server, between stream chunks and before each tool, leaving `conv`
/// ready for the next message. `Err` is a failure that ended the turn (no key, an API or
/// connection error); a refusal by the model is [`Event::Error`] and `Ok`.
pub fn run(
    cfg: &Config,
    conv: &mut Conversation,
    user_text: &str,
    backend: &mut dyn tools::Backend,
    cancel: &Arc<AtomicBool>,
    on_event: &mut dyn FnMut(Event),
) -> Result<(), String> {
    if cfg!(target_arch = "wasm32") {
        return Err(text::DESKTOP_ONLY.into());
    }
    let model = cfg.model().to_string();
    if model.is_empty() {
        return Err(text::CHOOSE_MODEL.into());
    }
    if cfg.provider.needs_key() && cfg.api_key.trim().is_empty() {
        return Err(text::ADD_KEY.replace("{provider}", cfg.provider.label()));
    }
    let id = (cfg.provider, model.clone());
    if conv.model.as_ref().is_some_and(|m| *m != id) && !conv.is_empty() {
        conv.clear();
        on_event(Event::Notice(text::SWITCHED.replace("{model}", &model)));
    }
    conv.model = Some(id);
    // New custom instructions start over: the system prompt can't change under earlier turns.
    let system = cfg.system();
    if conv.system != system && !conv.is_empty() {
        conv.clear();
        conv.model = Some((cfg.provider, model.clone()));
        on_event(Event::Notice(text::INSTRUCTIONS_CHANGED.into()));
    }
    conv.system = system;
    let _watch = http::watch(cancel);
    let local = cfg.provider == ProviderKind::Custom;
    let tools = tool_list(backend.has_ui(), local);
    let max_chars = if local { MAX_LOCAL_RESULT_CHARS } else { MAX_RESULT_CHARS };
    let guard = Guard::new(backend, cfg.allow_files);
    let mut repeats = local.then(Repeats::default);
    let start = conv.messages.len();
    conv.messages.push(match cfg.provider {
        ProviderKind::Anthropic => json!({"role": "user", "content": [{"type": "text", "text": user_text}]}),
        _ => json!({"role": "user", "content": user_text}),
    });
    // A first request that failed or was refused leaves the user's message unanswered: drop it,
    // so it isn't asked again with the next one.
    let unanswered = |conv: &mut Conversation| {
        if conv.messages.len() == start + 1 {
            conv.messages.truncate(start);
        }
    };
    let mut rounds = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            unanswered(conv);
            on_event(Event::Notice(text::STOPPED.into()));
            return Ok(());
        }
        let next = match cfg.provider {
            ProviderKind::Anthropic => anthropic::turn(cfg, &model, conv, &tools, cancel, on_event),
            _ => openai::turn(cfg, &model, conv, &tools, cancel, on_event),
        };
        let calls = match next {
            Err(e) => {
                unanswered(conv);
                return Err(e);
            }
            Ok(Next::Done) => {
                unanswered(conv);
                return Ok(());
            }
            Ok(Next::Cancelled) => {
                unanswered(conv);
                on_event(Event::Notice(text::STOPPED.into()));
                return Ok(());
            }
            Ok(Next::Again) => {
                rounds += 1;
                if rounds > MAX_ROUNDS {
                    on_event(Event::Notice(text::STOPPED_ROUNDS.replace("{rounds}", &MAX_ROUNDS.to_string())));
                    return Ok(());
                }
                continue;
            }
            Ok(Next::Refuse(calls, why)) => {
                let outs = calls.iter().map(|_| Output::error(why.clone())).collect();
                push_results(cfg.provider, conv, &calls, outs);
                return Ok(());
            }
            Ok(Next::Tools(calls)) => calls,
        };
        if rounds >= MAX_ROUNDS {
            let why = format!("Not run: the assistant stops after {MAX_ROUNDS} tool rounds per message. Ask the user to continue.");
            let outs = calls.iter().map(|_| Output::error(why.clone())).collect();
            push_results(cfg.provider, conv, &calls, outs);
            on_event(Event::Notice(text::STOPPED_TOOL_ROUNDS.replace("{rounds}", &MAX_ROUNDS.to_string())));
            return Ok(());
        }
        rounds += 1;
        let mut outs = Vec::with_capacity(calls.len());
        for call in &calls {
            if cancel.load(Ordering::Relaxed) {
                outs.push(Output::error(CANCELLED));
                continue;
            }
            on_event(Event::ToolCall { id: call.id.clone(), name: call.name.clone(), input: call.input.clone().unwrap_or_else(Value::String) });
            let mut out = match repeats.as_ref().and_then(|r| r.earlier(call)) {
                Some(out) => out,
                None => {
                    let out = execute(backend, call, &guard, max_chars);
                    if let Some(r) = repeats.as_mut() {
                        r.note(&guard, call, &out);
                    }
                    out
                }
            };
            admit_image(conv, &mut out);
            on_event(Event::ToolResult { id: call.id.clone(), ok: out.ok, text: out.text.clone(), image_png_base64: out.image.clone() });
            outs.push(out);
        }
        push_results(cfg.provider, conv, &calls, outs);
    }
}

/// The tools offered: all of them, and for a `local` model (whose context is small) without the
/// window-only tools and `script`. The same whatever the settings, so a conversation's tools
/// never change under it: `open` and `save` are refused when files are off ([`Guard`]).
fn tool_list(has_ui: bool, local: bool) -> Vec<Value> {
    let defs = tools::tool_definitions(has_ui && !local);
    let all = defs["tools"].as_array().cloned().unwrap_or_default();
    all.into_iter().filter(|t| !local || t["name"] != "script").collect()
}

/// Engine commands that read or write files whatever their parameters: refused, with their
/// aliases, while file access is off. LISP is one because its `open` reads and writes files;
/// ALIASEDIT and REINIT because a new alias would rename one of these. Commands that reach a file
/// only through a parameter (`insert {file}`, `cutlist {csv}`, `compare {path}`, `new
/// {template}`…) are refused by [`FILE_KEYS`] instead. The app also denies file access itself
/// while it serves the assistant (`cadcraft_engine::files`), which covers a file name typed at a
/// running command's prompt.
pub const FILE_COMMANDS: &[&str] = &[
    "open",
    "qsave",
    "saveas",
    "recover",
    "drawingrecovery",
    "script",
    "reinit",
    "import",
    "export",
    "wblock",
    "plot",
    "exportpdf",
    "publish",
    "pdfbook",
    "stepin",
    "stepout",
    "stlout",
    "igesin",
    "igesout",
    "rhinoout",
    "3dprint",
    "dwgout",
    // RENDER writes the picture to a PNG with `path`.
    "render",
    "xattach",
    "-xref",
    "xopen",
    "attach",
    "imageattach",
    "-image",
    "etransmit",
    "-etransmit",
    "partialopen",
    "-partialopen",
    "datalink",
    "attext",
    "appload",
    "lisp",
    "aliasedit",
    "reinit",
    // DesignCenter reads other drawings (`file`), and its typed form opens the palette.
    "adcenter",
];

/// Parameters that name a file or folder (except `path` of SWEEP, LOFT and the path arrays: a
/// curve's handle).
pub const FILE_KEYS: &[&str] = &["path", "file", "csv", "template", "patternFile", "savefilepath"];

/// File ▸ Open Recent's items, `ui.recent.<n>`: they open a file without asking.
const RECENT_FILE: &str = "ui.recent.";
/// OPTIONS, whose `ai*` and `mcp*` settings (file access, the provider, the control server) are
/// the user's alone.
const OPTIONS: &[&str] = &["options"];
/// Commands that close drawings without asking about unsaved changes (from a JSON call), or the
/// app: never the assistant's.
const CLOSING: &[&str] = &["close", "closeall", "ui.quit"];

/// Commands a model may repeat in one message ([`Repeats`]): they look, or move the view.
const LOOKING: &[&str] = &[
    "zoom",
    "pan",
    "regen",
    "regenall",
    "redraw",
    "redrawall",
    "vpoint",
    "view",
    "plan",
    "3dorbit",
    "cancel",
    "entities",
    "texts",
    "list",
    "dist",
    "id",
    "area",
    "measuregeom",
    "massprop",
    "drawing.inspect",
    "status",
];
/// Commands after which a call made before them may be wanted again: the drawing went back or
/// forward, or is another one.
const AGAIN: &[&str] = &["undo", "u", "redo", "mredo", "new", "qnew", "open"];

/// A command name as typed or listed: lower case, without the `_ - . + '` prefixes.
fn command_key(name: &str) -> String {
    name.trim_start_matches(['_', '-', '.', '+', '\'']).to_ascii_lowercase()
}

/// What the assistant may not do, with the names it could do it by, read once per run.
struct Guard {
    options: HashSet<String>,
    closing: HashSet<String>,
    /// [`FILE_COMMANDS`] by any name, while file access is off.
    files: Option<HashSet<String>>,
    /// [`LOOKING`] and [`AGAIN`] by any name.
    looking: HashSet<String>,
    again: HashSet<String>,
}

impl Guard {
    fn new(backend: &mut dyn tools::Backend, allow_files: bool) -> Self {
        let catalog = backend.call("engine.commands", json!({})).unwrap_or_default();
        let names = |ids: &[&str]| command_names(&catalog, ids);
        Guard {
            options: names(OPTIONS),
            closing: names(CLOSING),
            files: (!allow_files).then(|| names(FILE_COMMANDS)),
            looking: names(LOOKING),
            again: names(AGAIN),
        }
    }

    /// Why `call` is refused, if it is.
    fn refuses(&self, call: &Call, input: &Value) -> Option<&'static str> {
        let typed = input["text"].as_str().unwrap_or_default();
        let script = call.name == "script";
        match call.name.as_str() {
            "execute" => {
                let id = command_key(input["command"].as_str().unwrap_or_default());
                if self.options.contains(&id) && sets_ai_or_mcp(&input["params"]) {
                    return Some(SETTINGS_OFF);
                }
                if self.closing.contains(&id) {
                    return Some(CLOSE_OFF);
                }
                let curve_path = matches!(id.as_str(), "sweep" | "arraypath" | "array" | "loft");
                let files = self.files.as_ref()?;
                (files.contains(&id) || id.starts_with(RECENT_FILE) || names_file(input, curve_path)).then_some(FILES_OFF)
            }
            "command_line" | "script" => {
                if typed_lines(typed).any(|(name, json)| self.options.contains(&name) && json.is_some_and(|p| sets_ai_or_mcp(&p))) {
                    return Some(SETTINGS_OFF);
                }
                if types_command(typed, |n| self.closing.contains(n), script) {
                    return Some(CLOSE_OFF);
                }
                let files = self.files.as_ref()?;
                let file_command = |n: &str| files.contains(n) || n.starts_with(RECENT_FILE);
                (types_command(typed, file_command, script) || typed_lines(typed).any(|(_, json)| json.is_some_and(|p| names_file(&p, false))))
                    .then_some(FILES_OFF)
            }
            "open" | "save" => self.files.as_ref().map(|_| FILES_OFF),
            _ => self.files.as_ref().filter(|_| names_file(input, false)).map(|_| FILES_OFF),
        }
    }
}

/// `ids` and their aliases in the backend's `catalog`: built in, the user's own (ALIASEDIT,
/// acad.pgp) and the Rhino names.
fn command_names(catalog: &Value, ids: &[&str]) -> HashSet<String> {
    let mut names: HashSet<String> = ids.iter().map(|c| command_key(c)).collect();
    for c in catalog.as_array().into_iter().flatten().filter(|c| c["id"].as_str().is_some_and(|id| ids.contains(&id))) {
        for key in ["aliases", "userAliases"] {
            names.extend(c[key].as_array().into_iter().flatten().filter_map(Value::as_str).map(command_key));
        }
    }
    names
}

/// Whether OPTIONS `params` change an `ai*` or `mcp*` setting.
fn sets_ai_or_mcp(params: &Value) -> bool {
    params.as_object().is_some_and(|o| {
        o.keys().any(|k| {
            let k = k.to_ascii_lowercase();
            k.starts_with("ai") || k.starts_with("mcp")
        })
    })
}

/// Each typed line's first input as a command name, with its JSON parameters when the line is
/// `name {json}`.
fn typed_lines(text: &str) -> impl Iterator<Item = (String, Option<Value>)> + '_ {
    text.lines().filter_map(|line| {
        let line = line.trim();
        let first = line.split_whitespace().next()?;
        let json = line.get(first.len()..).and_then(|p| serde_json::from_str::<Value>(p.trim()).ok());
        Some((command_key(first), json))
    })
}

/// Whether `v` has a parameter in [`FILE_KEYS`] at any depth; `curve_path`: `path` is a handle.
fn names_file(v: &Value, curve_path: bool) -> bool {
    match v {
        Value::Object(o) => {
            o.iter().any(|(k, v)| (FILE_KEYS.iter().any(|f| f.eq_ignore_ascii_case(k)) && !(curve_path && k == "path")) || names_file(v, curve_path))
        }
        Value::Array(a) => a.iter().any(|v| names_file(v, curve_path)),
        _ => false,
    }
}

/// Whether text typed at the command line (or run as a `script`) would start a command `hits`
/// names: the first input of a line, a transparent `'name`, or (for the file commands) a LISP
/// `(…)` at the start. A script line goes on after its command ends, so there any later input of
/// two letters or more counts too (single letters are mostly keywords, such as W for Window).
fn types_command(text: &str, hits: impl Fn(&str) -> bool, script: bool) -> bool {
    text.lines().any(|line| {
        line.split_whitespace().enumerate().any(|(i, input)| {
            let name = command_key(input);
            (i == 0 && input.starts_with('(') && hits("lisp")) || ((i == 0 || input.starts_with('\'') || (script && name.len() > 1)) && hits(&name))
        })
    })
}

/// The calls of one user message that changed the drawing, with what they returned. A small
/// local model makes them again in a later round (qwen2.5:7b made a box twice, a rectangle and a
/// circle twice): a call the same as one of them (the tool and its parsed arguments) is answered
/// with an error naming the first one's result instead of running. Only for a local model: a
/// cloud model keeps track of what it did, and a repeat from it is meant (a second step of the
/// same move or copy). The latest call of a command replaces its earlier ones, so a setting can be
/// set back (`layer.current` Walls, Doors, Walls).
#[derive(Default)]
struct Repeats {
    /// The tool, the command (by [`command_key`]), the arguments and the result.
    ran: Vec<(String, String, Value, String)>,
    /// A command runs at the command line: a typed line answers its prompt (`0,0`, `@100,0`,
    /// Enter) and may well be the same as an earlier one.
    busy: bool,
}

impl Repeats {
    /// The answer to `call` when the same call already changed the drawing in this message.
    fn earlier(&self, call: &Call) -> Option<Output> {
        let input = call.input.as_ref().ok()?;
        if self.busy && call.name != "execute" {
            return None;
        }
        let (.., text) = self.ran.iter().find(|(name, _, args, _)| *name == call.name && args == input)?;
        let gist: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect();
        Some(Output::error(format!(
            "Not run: this same call already ran in this message and succeeded, returning {gist}. Don't make it again: go on with the next step, or finish."
        )))
    }

    /// Note what `call` did, answered with `out`.
    fn note(&mut self, guard: &Guard, call: &Call, out: &Output) {
        let (Ok(input), true) = (&call.input, out.ok) else { return };
        let command = match call.name.as_str() {
            "execute" => input["command"].as_str(),
            "command_line" | "script" => {
                // A typed line's result is the command line's state (a script's under `state`).
                let v: Value = serde_json::from_str(&out.text).unwrap_or_default();
                let st = if v["state"].is_object() { &v["state"] } else { &v };
                let was_busy = std::mem::replace(&mut self.busy, !st.get("running").is_some_and(Value::is_null));
                let first = input["text"].as_str().and_then(|t| t.split_whitespace().next());
                // UNDO and MREDO ask how many steps: the line that starts one may take the drawing
                // back, whatever its prompt's answer.
                if [first, st["running"].as_str()].into_iter().flatten().any(|c| guard.again.contains(&command_key(c))) {
                    self.ran.clear();
                    return;
                }
                // Only a line that starts and ends a command: one answering a prompt, refused or
                // whose result was cut (whatever runs is unknown) may come again.
                if was_busy || self.busy || st.get("error").is_some() {
                    return;
                }
                first
            }
            "cancel" => {
                self.busy = false;
                return;
            }
            "new_drawing" | "open" => Some("new"),
            _ => return,
        };
        let command = command_key(command.unwrap_or_default());
        if guard.again.contains(&command) {
            self.ran.clear();
        } else if !command.is_empty() && !guard.looking.contains(&command) {
            self.ran.retain(|(name, c, ..)| *name != call.name || *c != command);
            self.ran.push((call.name.clone(), command, input.clone(), out.text.clone()));
        }
    }
}

/// Run one call, unless `guard` refuses it. A call the tool set refuses (unknown tool, arguments
/// that don't fit) is an error result the model can correct, not a failure of the turn.
fn execute(backend: &mut dyn tools::Backend, call: &Call, guard: &Guard, max_chars: usize) -> Output {
    let input = match &call.input {
        Ok(v) => v,
        Err(raw) => return Output::error(json!({ "INVALID_JSON": raw }).to_string()),
    };
    if let Some(why) = guard.refuses(call, input) {
        return Output::error(why);
    }
    match tools::call_tool(backend, &call.name, input) {
        Ok(r) => mcp_output(&r, max_chars),
        Err(e) => Output::error(e),
    }
}

/// Keep a tool's image only when the provider takes it and the conversation has room for it;
/// otherwise its text says why, so the model can ask for a smaller one.
fn admit_image(conv: &mut Conversation, out: &mut Output) {
    let Some(data) = &out.image else { return };
    let why = match png_size(data) {
        _ if data.len() > MAX_IMAGE_BYTES => format!("it is {} MB", data.len() >> 20),
        Some((w, h)) if w.max(h) > MAX_IMAGE_SIDE => format!("it is {w} × {h} px; ask for a width and height of {MAX_IMAGE_SIDE} or less"),
        _ if conv.images >= MAX_CONVERSATION_IMAGES || conv.image_bytes.saturating_add(data.len()) > MAX_CONVERSATION_IMAGE_BYTES => {
            "this chat holds all the images it can; in a new chat the assistant sees images again".into()
        }
        _ => {
            conv.images += 1;
            conv.image_bytes += data.len();
            return;
        }
    };
    out.image = None;
    out.text.push_str(&format!("\n(The image was left out: {why}.)"));
}

/// A base64 PNG's width and height, from its header.
fn png_size(b64: &str) -> Option<(u32, u32)> {
    let sextet = |c: u8| -> Option<u32> {
        Some(u32::from(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        }))
    };
    // The signature and the IHDR chunk's start: 24 bytes, 32 base64 characters.
    let mut head = Vec::with_capacity(24);
    for quad in b64.as_bytes().get(..32)?.chunks(4) {
        let n = quad.iter().try_fold(0u32, |n, &c| Some(n << 6 | sextet(c)?))?;
        head.extend_from_slice(n.to_be_bytes().get(1..)?);
    }
    if head.get(..8)? != b"\x89PNG\r\n\x1a\n" || head.get(12..16)? != b"IHDR" {
        return None;
    }
    let be = |i: usize| head.get(i..i + 4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_be_bytes);
    Some((be(16)?, be(20)?))
}

/// An MCP tool result (`{content: [{type: "text", text} | {type: "image", data}], isError}`) as
/// text and the first image, the text cut at `max_chars` characters.
fn mcp_output(r: &Value, max_chars: usize) -> Output {
    let parts = r["content"].as_array().map(Vec::as_slice).unwrap_or_default();
    let text: Vec<&str> = parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect();
    let mut text = text.join("\n");
    if let Some((cut, _)) = text.char_indices().nth(max_chars) {
        let more = text.split_off(cut).chars().count();
        text.push_str(&format!("\n[cut: {more} more characters. Ask for less: a filter, a window, a limit or offset.]"));
    }
    let image = parts.iter().find(|p| p["type"] == "image").and_then(|p| p["data"].as_str()).map(str::to_string);
    Output { ok: r["isError"] != true, text, image }
}

/// Append the answers to `calls`, in the provider's format.
fn push_results(provider: ProviderKind, conv: &mut Conversation, calls: &[Call], outs: Vec<Output>) {
    match provider {
        ProviderKind::Anthropic => conv.messages.push(anthropic::results(calls, outs)),
        _ => openai::push_results(conv, calls, outs),
    }
}

/// The models the provider offers, sorted.
pub fn list_models(cfg: &Config) -> Result<Vec<String>, String> {
    let key = cfg.api_key.trim();
    let (url, headers) = match cfg.provider {
        ProviderKind::Anthropic => (format!("{}/v1/models?limit=1000", cfg.base()), anthropic::headers(key, false)),
        _ => (format!("{}/models", cfg.base()), openai::headers(key)),
    };
    let v = http::get_json(&url, &headers).map_err(|f| cfg.failure(f))?;
    let mut ids: Vec<String> = v["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_string)).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Check the URL and key by listing the models.
pub fn test_connection(cfg: &Config) -> Result<String, String> {
    list_models(cfg).map(|m| text::CONNECTED.replace("{count}", &m.len().to_string()))
}

#[cfg(test)]
mod tests;
