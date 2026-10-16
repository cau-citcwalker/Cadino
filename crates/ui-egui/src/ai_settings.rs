//! The AI settings (Options ▸ AI): the built-in assistant's provider, model and instructions, and
//! the loopback control server that lets external AI apps (MCP) drive Cadino.
//!
//! The API keys are never part of these settings: they live in the system keychain
//! ([`cadcraft_ai::keys`]) and are read only when a request is made.

use std::cell::Cell;
use std::ops::RangeInclusive;
use std::sync::mpsc::Receiver;

use cadcraft_ai::ProviderKind;
use cadcraft_ai::keys::KeySource;
use serde_json::{Value, json};

use crate::{CadApp, ControlRequest};

/// Anthropic's thinking efforts, least first.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// The ports the MCP control server may use (no privileged ones).
pub const PORTS: RangeInclusive<u16> = 1024..=65535;
/// The port docs/mcp.md uses.
pub const DEFAULT_PORT: u16 = 7979;
/// The longest custom instructions.
pub const MAX_INSTRUCTIONS_CHARS: usize = 8000;
const MAX_MODEL_CHARS: usize = 200;
const MAX_URL_CHARS: usize = 2048;

#[derive(Clone, Debug, PartialEq)]
pub struct AiSettings {
    pub provider: ProviderKind,
    /// Each provider's model, so switching back and forth keeps them.
    pub anthropic_model: String,
    pub openai_model: String,
    pub custom_model: String,
    /// The OpenAI-compatible server the Custom provider talks to.
    pub custom_base_url: String,
    pub effort: String,
    pub custom_instructions: String,
    /// Let the assistant open, save and export files.
    pub allow_files: bool,
    /// Run the control server external AI apps connect to.
    pub mcp_enabled: bool,
    pub mcp_port: u16,
    /// What the host made of `mcp_enabled` ("Listening on 127.0.0.1:7979"); not saved.
    pub mcp_status: String,
}

impl Default for AiSettings {
    fn default() -> Self {
        AiSettings {
            provider: ProviderKind::default(),
            anthropic_model: ProviderKind::Anthropic.default_model().into(),
            openai_model: ProviderKind::OpenAi.default_model().into(),
            custom_model: ProviderKind::Custom.default_model().into(),
            custom_base_url: ProviderKind::Custom.default_base_url().into(),
            effort: "high".into(),
            custom_instructions: String::new(),
            allow_files: false,
            mcp_enabled: false,
            mcp_port: DEFAULT_PORT,
            mcp_status: String::new(),
        }
    }
}

/// A provider name: `"anthropic"`, `"openai"` or `"custom"`.
pub(crate) fn provider(v: &Value) -> Option<ProviderKind> {
    serde_json::from_value(v.clone()).ok()
}

/// One line of text of at most `max` characters, trimmed.
fn line(v: &Value, max: usize) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| s.chars().count() <= max && !s.chars().any(char::is_control)).map(str::to_string)
}

/// A model name (empty: the provider's default).
pub(crate) fn model(v: &Value) -> Option<String> {
    line(v, MAX_MODEL_CHARS)
}

/// A server URL: `http://…` or `https://…`, or empty for the default.
pub(crate) fn base_url(v: &Value) -> Option<String> {
    line(v, MAX_URL_CHARS).filter(|u| u.is_empty() || u.starts_with("http://") || u.starts_with("https://"))
}

pub(crate) fn effort(v: &Value) -> Option<String> {
    v.as_str().filter(|e| EFFORTS.contains(e)).map(str::to_string)
}

pub(crate) fn instructions(v: &Value) -> Option<String> {
    v.as_str().filter(|s| s.chars().count() <= MAX_INSTRUCTIONS_CHARS).map(str::to_string)
}

pub(crate) fn port(v: &Value) -> Option<u16> {
    v.as_u64().and_then(|p| u16::try_from(p).ok()).filter(|p| PORTS.contains(p))
}

impl AiSettings {
    /// The chosen provider's model.
    pub fn model(&self) -> &str {
        match self.provider {
            ProviderKind::Anthropic => &self.anthropic_model,
            ProviderKind::OpenAi => &self.openai_model,
            ProviderKind::Custom => &self.custom_model,
        }
    }

    pub fn model_mut(&mut self) -> &mut String {
        match self.provider {
            ProviderKind::Anthropic => &mut self.anthropic_model,
            ProviderKind::OpenAi => &mut self.openai_model,
            ProviderKind::Custom => &mut self.custom_model,
        }
    }

    /// The settings as saved with the interface prefs. Never holds a key.
    pub fn to_json(&self) -> Value {
        json!({
            "provider": self.provider,
            "anthropicModel": self.anthropic_model,
            "openaiModel": self.openai_model,
            "customModel": self.custom_model,
            "customBaseUrl": self.custom_base_url,
            "effort": self.effort,
            "customInstructions": self.custom_instructions,
            "allowFiles": self.allow_files,
            "mcpEnabled": self.mcp_enabled,
            "mcpPort": self.mcp_port,
        })
    }

    /// Restore settings saved by [`Self::to_json`]; missing, malformed or out-of-range values keep
    /// what is there.
    pub fn load(&mut self, v: &Value) {
        let get = |key: &str| v.get(key).unwrap_or(&Value::Null);
        if let Some(p) = provider(get("provider")) {
            self.provider = p;
        }
        for (key, slot) in
            [("anthropicModel", &mut self.anthropic_model), ("openaiModel", &mut self.openai_model), ("customModel", &mut self.custom_model)]
        {
            if let Some(m) = model(get(key)) {
                *slot = m;
            }
        }
        if let Some(u) = base_url(get("customBaseUrl")) {
            self.custom_base_url = u;
        }
        if let Some(e) = effort(get("effort")) {
            self.effort = e;
        }
        if let Some(s) = instructions(get("customInstructions")) {
            self.custom_instructions = s;
        }
        if let Some(b) = get("allowFiles").as_bool() {
            self.allow_files = b;
        }
        if let Some(b) = get("mcpEnabled").as_bool() {
            self.mcp_enabled = b;
        }
        if let Some(p) = port(get("mcpPort")) {
            self.mcp_port = p;
        }
    }

    /// The assistant's configuration; the key is left for the caller to fill.
    pub fn config(&self) -> cadcraft_ai::Config {
        cadcraft_ai::Config {
            provider: self.provider,
            model: self.model().to_string(),
            base_url: if self.provider == ProviderKind::Custom { self.custom_base_url.clone() } else { String::new() },
            effort: self.effort.clone(),
            custom_instructions: self.custom_instructions.clone(),
            allow_files: self.allow_files,
            ..cadcraft_ai::Config::default()
        }
    }

    /// Whether the assistant can be asked: a model is chosen and, where the provider needs one,
    /// a key is set.
    pub fn is_configured(&self) -> bool {
        !self.model().trim().is_empty() && (!self.provider.needs_key() || key_source(self.provider) != KeySource::Missing)
    }
}

thread_local! {
    /// Where each provider's key comes from, asked once: the keychain is too slow to ask every frame.
    static KEY_SOURCES: Cell<[Option<KeySource>; 3]> = const { Cell::new([None; 3]) };
}

/// Where `provider`'s key comes from ([`cadcraft_ai::keys::source`], cached).
pub fn key_source(provider: ProviderKind) -> KeySource {
    KEY_SOURCES.with(|cache| {
        let mut sources = cache.get();
        let i = provider as usize;
        if let Some(s) = sources.get(i).copied().flatten() {
            return s;
        }
        let s = cadcraft_ai::keys::source(provider);
        if let Some(slot) = sources.get_mut(i) {
            *slot = Some(s);
        }
        cache.set(sources);
        s
    })
}

/// Save `provider`'s key in the keychain (empty: remove it).
pub fn store_key(provider: ProviderKind, key: &str) -> Result<(), String> {
    let r = cadcraft_ai::keys::store(provider, key);
    forget_key_sources();
    r
}

pub fn delete_key(provider: ProviderKind) -> Result<(), String> {
    let r = cadcraft_ai::keys::delete(provider);
    forget_key_sources();
    r
}

fn forget_key_sources() {
    KEY_SOURCES.with(|cache| cache.set([None; 3]));
}

impl CadApp {
    /// The port the MCP control server should listen on, if the user turned it on.
    pub fn wanted_control_port(&self) -> Option<u16> {
        self.ai.mcp_enabled.then_some(self.ai.mcp_port)
    }

    /// Serve requests from a control server the host started.
    pub fn attach_control(&mut self, rx: Receiver<ControlRequest>) {
        self.mcp_rx = Some(rx);
    }

    /// Stop serving them; requests still on their way are refused.
    pub fn detach_control(&mut self) {
        self.mcp_rx = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_without_secrets() {
        let ai = AiSettings {
            provider: ProviderKind::Custom,
            anthropic_model: "claude-sonnet-5-5".into(),
            openai_model: "gpt-5-mini".into(),
            custom_model: "qwen2.5:7b-instruct".into(),
            custom_base_url: "http://localhost:1234/v1".into(),
            effort: "max".into(),
            custom_instructions: "Units are millimetres.\nWalls are 200 thick.".into(),
            allow_files: true,
            mcp_enabled: true,
            mcp_port: 8123,
            mcp_status: "Listening on 127.0.0.1:8123".into(),
        };
        let v = ai.to_json();
        let text = v.to_string();
        for secret in ["key", "Key", "token", "secret", "Listening"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
        let mut back = AiSettings::default();
        back.load(&v);
        assert_eq!(back, AiSettings { mcp_status: String::new(), ..ai });
        // The key is never in the configuration either; the base URL is only the custom server's.
        let cfg = back.config();
        assert!(cfg.api_key.is_empty());
        assert_eq!((cfg.model.as_str(), cfg.base_url.as_str()), ("qwen2.5:7b-instruct", "http://localhost:1234/v1"));
        back.provider = ProviderKind::Anthropic;
        assert_eq!((back.config().model.as_str(), back.config().base_url.as_str()), ("claude-sonnet-5-5", ""));
    }

    #[test]
    fn hostile_saved_settings_are_ignored() {
        let long = "x".repeat(MAX_INSTRUCTIONS_CHARS + 1);
        for bad in [
            json!({"provider": "gemini", "anthropicModel": 5, "openaiModel": "a\u{0}b", "customModel": "m".repeat(MAX_MODEL_CHARS + 1)}),
            json!({"customBaseUrl": "file:///etc/passwd", "effort": "extreme", "customInstructions": long}),
            json!({"allowFiles": "yes", "mcpEnabled": 1, "mcpPort": 80}),
            json!({"mcpPort": 70000}),
            json!({"mcpPort": -1}),
            json!({"mcpPort": 7979.5}),
            json!({"customBaseUrl": "javascript:alert(1)", "provider": null}),
            json!([1, 2, 3]),
            json!("text"),
            Value::Null,
        ] {
            let mut ai = AiSettings::default();
            ai.load(&bad);
            assert_eq!(ai, AiSettings::default(), "{bad}");
        }
        let mut ai = AiSettings::default();
        ai.load(&json!({"mcpPort": 1024, "customBaseUrl": "  https://example.com/v1  ", "openaiModel": ""}));
        assert_eq!((ai.mcp_port, ai.custom_base_url.as_str(), ai.openai_model.as_str()), (1024, "https://example.com/v1", ""));
    }

    #[test]
    fn a_local_model_needs_no_key_but_a_model() {
        let mut ai = AiSettings { provider: ProviderKind::Custom, ..AiSettings::default() };
        assert!(!ai.is_configured(), "no model chosen yet");
        ai.custom_model = "llama3.1".into();
        assert!(ai.is_configured());
    }

    #[test]
    fn the_control_port_follows_the_setting() {
        let mut app = CadApp::new(cadcraft_engine::Session::empty(), crate::Services::default());
        assert_eq!(app.wanted_control_port(), None);
        app.ai.mcp_enabled = true;
        assert_eq!(app.wanted_control_port(), Some(DEFAULT_PORT));
        let (tx, rx) = std::sync::mpsc::channel();
        app.attach_control(rx);
        app.detach_control();
        let (req, _pending) = ControlRequest::new("ping", Value::Null);
        assert!(tx.send(req).is_err(), "a detached channel refuses requests");
    }
}
