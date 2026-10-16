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
pub mod tools;

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
    /// The system prompt with the user's own instructions.
    fn system(&self) -> String {
        let custom = self.custom_instructions.trim();
        if custom.is_empty() { prompt::SYSTEM.to_string() } else { format!("{}\n\n# User instructions\n{custom}", prompt::SYSTEM) }
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
/// earlier turns are never edited (Claude's thinking blocks and prompt caching depend on it).
#[derive(Clone, Debug, Default)]
pub struct Conversation {
    messages: Vec<Value>,
    /// The provider and model the messages were written for.
    model: Option<(ProviderKind, String)>,
    /// A custom server refused images: requests leave the tools' images out.
    no_images: bool,
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
const FILES_OFF: &str = "File access is off for the assistant (Options ▸ AI).";
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
/// between stream chunks and before each tool, leaving `conv` ready for the next message.
/// `Err` is a failure that ended the turn (no key, an API or connection error); a refusal by the
/// model is [`Event::Error`] and `Ok`.
pub fn run(
    cfg: &Config,
    conv: &mut Conversation,
    user_text: &str,
    backend: &mut dyn tools::Backend,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(Event),
) -> Result<(), String> {
    if cfg!(target_arch = "wasm32") {
        return Err(http::DESKTOP_ONLY.into());
    }
    let model = cfg.model().to_string();
    if model.is_empty() {
        return Err("Choose a model in Options ▸ AI.".into());
    }
    if cfg.provider.needs_key() && cfg.api_key.trim().is_empty() {
        return Err(format!("Add your {} API key in Options ▸ AI.", cfg.provider.label()));
    }
    let id = (cfg.provider, model.clone());
    if conv.model.as_ref().is_some_and(|m| *m != id) && !conv.is_empty() {
        conv.clear();
        on_event(Event::Notice(format!("Switched to {model}: the conversation starts over.")));
    }
    conv.model = Some(id);
    let tools = tool_list(backend.has_ui(), cfg.allow_files);
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
            on_event(Event::Notice("Stopped".into()));
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
                on_event(Event::Notice("Stopped".into()));
                return Ok(());
            }
            Ok(Next::Again) => {
                rounds += 1;
                if rounds > MAX_ROUNDS {
                    on_event(Event::Notice(format!("Stopped after {MAX_ROUNDS} rounds.")));
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
            on_event(Event::Notice(format!("Stopped after {MAX_ROUNDS} tool rounds; send a message to continue.")));
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
            let out = execute(backend, call, cfg.allow_files);
            on_event(Event::ToolResult { id: call.id.clone(), ok: out.ok, text: out.text.clone(), image_png_base64: out.image.clone() });
            outs.push(out);
        }
        push_results(cfg.provider, conv, &calls, outs);
    }
}

/// The tools offered: all of them, or without `open` and `save` when files are off.
fn tool_list(has_ui: bool, allow_files: bool) -> Vec<Value> {
    let defs = tools::tool_definitions(has_ui);
    let all = defs["tools"].as_array().cloned().unwrap_or_default();
    all.into_iter().filter(|t| allow_files || !matches!(t["name"].as_str(), Some("open" | "save"))).collect()
}

/// Whether `v` has a key named `path` at any depth.
fn has_path(v: &Value) -> bool {
    match v {
        Value::Object(o) => o.iter().any(|(k, v)| k == "path" || has_path(v)),
        Value::Array(a) => a.iter().any(has_path),
        _ => false,
    }
}

/// Run one call. A call the tool set refuses (unknown tool, arguments that don't fit) is an error
/// result the model can correct, not a failure of the turn.
fn execute(backend: &mut dyn tools::Backend, call: &Call, allow_files: bool) -> Output {
    let input = match &call.input {
        Ok(v) => v,
        Err(raw) => return Output::error(json!({ "INVALID_JSON": raw }).to_string()),
    };
    // ponytail: only structured `path` arguments are caught; a file command typed at
    // `command_line` (e.g. SAVEAS) still reaches the disk. Close it with an engine-level policy.
    if !allow_files && (matches!(call.name.as_str(), "open" | "save") || has_path(input)) {
        return Output::error(FILES_OFF);
    }
    match tools::call_tool(backend, &call.name, input) {
        Ok(r) => mcp_output(&r),
        Err(e) => Output::error(e),
    }
}

/// An MCP tool result (`{content: [{type: "text", text} | {type: "image", data}], isError}`) as
/// text and the first image, the text cut at 30000 characters.
fn mcp_output(r: &Value) -> Output {
    let parts = r["content"].as_array().map(Vec::as_slice).unwrap_or_default();
    let text: Vec<&str> = parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect();
    let mut text = text.join("\n");
    if let Some((cut, _)) = text.char_indices().nth(MAX_RESULT_CHARS) {
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
    let v = http::get_json(&url, &headers).map_err(|f| f.message())?;
    let mut ids: Vec<String> = v["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_string)).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Check the URL and key by listing the models.
pub fn test_connection(cfg: &Config) -> Result<String, String> {
    list_models(cfg).map(|m| format!("Connected: {} models", m.len()))
}

#[cfg(test)]
mod tests;
