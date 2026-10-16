//! Options ▸ AI: the built-in assistant (provider, model, key, effort, instructions) and the
//! control server external AI apps connect to over MCP.
//!
//! Settings change through the OPTIONS JSON form like every other tab. API keys never do: they go
//! straight to the system keychain and are never shown again.

use std::sync::{Arc, Mutex};

use cadcraft_ai::ProviderKind;
use cadcraft_ai::keys::KeySource;
use egui::{Button, RichText, TextEdit};
use serde_json::{Map, Value, json};

use super::dialog::{grid, heading, hint};
use crate::CadApp;
use crate::ai_settings::{self, EFFORTS, PORTS};
use crate::theme::Tokens;

/// The tallest the tab grows before it scrolls.
const MAX_HEIGHT: f32 = 520.0;
const FIELD_WIDTH: f32 = 330.0;

/// Local servers' usual OpenAI-compatible addresses.
const PRESETS: [(&str, &str); 4] = [
    ("Ollama", "http://localhost:11434/v1"),
    ("LM Studio", "http://localhost:1234/v1"),
    ("llama.cpp", "http://localhost:8080/v1"),
    ("vLLM", "http://localhost:8000/v1"),
];

pub(super) fn tab(app: &CadApp, ui: &mut egui::Ui, changes: &mut Map<String, Value>) {
    egui::ScrollArea::vertical().max_height(MAX_HEIGHT).auto_shrink([false, true]).show(ui, |ui| {
        assistant(app, ui, changes);
        ui.add_space(12.0);
        ui.separator();
        mcp(app, ui, changes);
    });
}

/// A request on a worker thread (Refresh models, Test connection), kept in egui's memory.
#[derive(Clone, Default)]
enum Answer<T> {
    #[default]
    Idle,
    Waiting,
    Done(Result<T, String>),
}

type Job<T> = Arc<Mutex<Answer<T>>>;

fn job<T: Clone + Send + Sync + 'static>(ui: &egui::Ui, id: egui::Id) -> Job<T> {
    ui.data_mut(|d| Arc::clone(d.get_temp_mut_or_default::<Job<T>>(id)))
}

fn answer<T: Clone>(job: &Job<T>) -> Answer<T> {
    job.lock().map(|a| a.clone()).unwrap_or(Answer::Idle)
}

fn set<T>(job: &Job<T>, a: Answer<T>) {
    if let Ok(mut j) = job.lock() {
        *j = a;
    }
}

/// Ask the provider on a worker thread with the key from the keychain, so the dialog never waits.
fn spawn<T: Send + 'static>(ui: &egui::Ui, job: &Job<T>, mut cfg: cadcraft_ai::Config, work: fn(&cadcraft_ai::Config) -> Result<T, String>) {
    set(job, Answer::Waiting);
    let (done, ctx) = (Arc::clone(job), ui.ctx().clone());
    let started = std::thread::Builder::new().name("ai-options".into()).spawn(move || {
        cfg.api_key = cadcraft_ai::keys::load(cfg.provider).unwrap_or_default();
        set(&done, Answer::Done(work(&cfg)));
        ctx.request_repaint();
    });
    if let Err(e) = started {
        set(job, Answer::Done(Err(e.to_string())));
    }
}

fn error(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(Tokens::get().warn));
}

/// A text field edited in egui's memory and handed back when it loses focus, so a value half
/// typed is never applied.
fn edited(ui: &mut egui::Ui, id: egui::Id, current: &str, multiline: bool) -> Option<String> {
    let mut text = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| current.to_string());
    let field = if multiline { TextEdit::multiline(&mut text).desired_rows(4) } else { TextEdit::singleline(&mut text) };
    let r = ui.add(field.desired_width(FIELD_WIDTH));
    if r.changed() {
        ui.data_mut(|d| d.insert_temp(id, text.clone()));
    }
    if r.lost_focus() {
        ui.data_mut(|d| d.remove::<String>(id));
        return (text != current).then_some(text);
    }
    None
}

fn assistant(app: &CadApp, ui: &mut egui::Ui, changes: &mut Map<String, Value>) {
    let ai = &app.ai;
    let p = ai.provider;
    let models: Job<Vec<String>> = job(ui, egui::Id::new(("cc_ai_models", p.id())));
    heading(ui, crate::tl!("Built-in assistant"));
    grid(ui, "options_ai", |ui| {
        ui.label(crate::tl!("Provider"));
        let mut provider = p;
        egui::ComboBox::from_id_salt("options_ai_provider").selected_text(crate::i18n::t(p.label())).show_ui(ui, |ui| {
            for k in [ProviderKind::Anthropic, ProviderKind::OpenAi, ProviderKind::Custom] {
                ui.selectable_value(&mut provider, k, crate::i18n::t(k.label()));
            }
        });
        if provider != p {
            changes.insert("aiProvider".into(), json!(provider));
        }
        ui.end_row();

        if p == ProviderKind::Custom {
            ui.label(crate::tl!("Server address"));
            ui.vertical(|ui| {
                let id = egui::Id::new("cc_ai_base_url");
                if let Some(url) = edited(ui, id, &ai.custom_base_url, false) {
                    if ai_settings::base_url(&json!(url)).is_some() {
                        changes.insert("aiBaseUrl".into(), json!(url));
                    } else {
                        ui.data_mut(|d| d.insert_temp(id.with("error"), true));
                    }
                }
                if ui.data(|d| d.get_temp::<bool>(id.with("error"))).unwrap_or(false) {
                    error(ui, crate::tl!("The address must start with http:// or https://"));
                }
                ui.horizontal(|ui| {
                    for (name, url) in PRESETS {
                        if ui.small_button(name).clicked() {
                            changes.insert("aiBaseUrl".into(), json!(url));
                        }
                    }
                });
            });
            if changes.contains_key("aiBaseUrl") {
                ui.data_mut(|d| d.remove::<bool>(egui::Id::new("cc_ai_base_url").with("error")));
            }
            ui.end_row();
        }

        ui.label(crate::tl!("Model"));
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                if let Some(m) = edited(ui, egui::Id::new(("cc_ai_model", p.id())), ai.model(), false) {
                    changes.insert("aiModel".into(), json!(m.trim()));
                }
                let fetched = match answer(&models) {
                    Answer::Done(Ok(list)) => list,
                    _ => Vec::new(),
                };
                egui::ComboBox::from_id_salt(("options_ai_models", p.id())).selected_text("").width(24.0).height(300.0).show_ui(ui, |ui| {
                    let suggested = p.suggested_models().iter().map(|m| m.to_string());
                    for m in suggested.chain(fetched.into_iter().filter(|m| !p.suggested_models().contains(&m.as_str()))) {
                        if ui.selectable_label(ai.model() == m, &m).clicked() {
                            changes.insert("aiModel".into(), json!(m));
                        }
                    }
                });
            });
            ui.horizontal(|ui| {
                let waiting = matches!(answer(&models), Answer::Waiting);
                if ui.add_enabled(!waiting, Button::new(crate::tl!("Refresh models"))).clicked() {
                    spawn(ui, &models, ai.config(), cadcraft_ai::list_models);
                }
                match answer(&models) {
                    Answer::Waiting => {
                        ui.spinner();
                    }
                    Answer::Done(Ok(list)) => hint(ui, &crate::i18n::count(list.len() as u64, "{n} model", "{n} models")),
                    Answer::Done(Err(e)) => error(ui, &e),
                    Answer::Idle => {}
                }
            });
        });
        ui.end_row();

        if p.needs_key() {
            ui.label(crate::tl!("API key"));
            ui.vertical(|ui| key(ui, p));
            ui.end_row();
        }

        if p == ProviderKind::Anthropic {
            ui.label(crate::tl!("Thinking effort"));
            let mut effort = ai.effort.clone();
            egui::ComboBox::from_id_salt("options_ai_effort").selected_text(&effort).show_ui(ui, |ui| {
                for e in EFFORTS {
                    ui.selectable_value(&mut effort, e.to_string(), e);
                }
            });
            if effort != ai.effort {
                changes.insert("aiEffort".into(), json!(effort));
            }
            ui.end_row();
        }
    });
    ui.add_space(6.0);
    let mut files = ai.allow_files;
    if ui.checkbox(&mut files, crate::tl!("Allow the assistant to open, save and export files")).changed() {
        changes.insert("aiAllowFiles".into(), json!(files));
    }
    ui.add_space(6.0);
    ui.label(crate::tl!("Custom instructions"));
    if let Some(text) = edited(ui, egui::Id::new("cc_ai_instructions"), &ai.custom_instructions, true) {
        let text: String = text.chars().take(ai_settings::MAX_INSTRUCTIONS_CHARS).collect();
        changes.insert("aiCustomInstructions".into(), json!(text));
    }
    hint(ui, crate::tl!("Added to every conversation, e.g. “Units are millimetres; walls are 200 thick.”"));
    ui.add_space(6.0);
    let test: Job<String> = job(ui, egui::Id::new(("cc_ai_test", p.id())));
    ui.horizontal(|ui| {
        let waiting = matches!(answer(&test), Answer::Waiting);
        if ui.add_enabled(!waiting, Button::new(crate::tl!("Test connection"))).clicked() {
            spawn(ui, &test, ai.config(), cadcraft_ai::test_connection);
        }
        match answer(&test) {
            Answer::Waiting => {
                ui.spinner();
            }
            Answer::Done(Ok(text)) => {
                ui.label(text);
            }
            Answer::Done(Err(e)) => error(ui, &e),
            Answer::Idle => {}
        }
    });
}

/// The key field: typed (masked), saved to the keychain and cleared; a saved key is never shown.
fn key(ui: &mut egui::Ui, p: ProviderKind) {
    let id = egui::Id::new(("cc_ai_key", p.id()));
    let mut typed = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_default();
    let source = ai_settings::key_source(p);
    ui.horizontal(|ui| {
        let r = ui.add(TextEdit::singleline(&mut typed).password(true).hint_text(crate::tl!("Paste your API key")).desired_width(220.0));
        if r.changed() {
            ui.data_mut(|d| d.insert_temp(id, typed.clone()));
        }
        let mut result = None;
        if ui.add_enabled(!typed.trim().is_empty(), Button::new(crate::tl!("Save"))).clicked() {
            result = Some(ai_settings::store_key(p, &typed));
            ui.data_mut(|d| d.remove::<String>(id));
        }
        if ui.add_enabled(source == KeySource::Keychain, Button::new(crate::tl!("Remove"))).clicked() {
            result = Some(ai_settings::delete_key(p));
        }
        if let Some(r) = result {
            ui.data_mut(|d| d.insert_temp(id.with("error"), r.err()));
        }
    });
    match source {
        KeySource::Keychain => hint(ui, crate::tl!("Saved in the system keychain")),
        KeySource::Environment => {
            let var = if p == ProviderKind::OpenAi { "OPENAI_API_KEY" } else { "ANTHROPIC_API_KEY" };
            hint(ui, &crate::tf!("Using {var} from the environment", var = var));
        }
        KeySource::Missing => hint(ui, crate::tl!("Not set")),
    }
    if let Some(e) = ui.data(|d| d.get_temp::<Option<String>>(id.with("error"))).flatten() {
        error(ui, &e);
    }
}

fn mcp(app: &CadApp, ui: &mut egui::Ui, changes: &mut Map<String, Value>) {
    heading(ui, crate::tl!("External AI apps (MCP)"));
    if cfg!(target_arch = "wasm32") {
        hint(ui, crate::tl!("Not available in the browser: AI apps connect to the desktop app."));
        return;
    }
    let ai = &app.ai;
    ui.horizontal(|ui| {
        let mut on = ai.mcp_enabled;
        if ui.checkbox(&mut on, crate::tl!("Let AI apps on this computer control Cadino")).changed() {
            changes.insert("mcpEnabled".into(), json!(on));
        }
        ui.label(crate::tl!("Port"));
        // Kept while dragging or typing, so the server moves once, to the port chosen.
        let id = egui::Id::new("cc_ai_mcp_port");
        let mut port = ui.data(|d| d.get_temp::<u16>(id)).unwrap_or(ai.mcp_port);
        let r = ui.add(egui::DragValue::new(&mut port).range(PORTS).update_while_editing(false));
        if r.dragged() || r.has_focus() {
            ui.data_mut(|d| d.insert_temp(id, port));
        } else {
            ui.data_mut(|d| d.remove::<u16>(id));
            if port != ai.mcp_port {
                changes.insert("mcpPort".into(), json!(port));
            }
        }
    });
    let status = if ai.mcp_status.is_empty() { crate::tl!("Off") } else { &ai.mcp_status };
    ui.label(status);
    let t = Tokens::get();
    ui.label(RichText::new(crate::tl!("While this is on, any program on this computer can control Cadino.")).small().color(if ai.mcp_enabled {
        t.warn
    } else {
        t.text_dim
    }));
    ui.add_space(6.0);
    let (cli, found) = cli_path();
    if !found {
        error(ui, crate::tl!("cadcraft-cli was not found next to Cadino. Build or install it in the same folder for these to work."));
    }
    for (title, text) in snippets(&cli, ai.mcp_port) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).strong());
            if ui.small_button(crate::tl!("Copy")).clicked() {
                ui.ctx().copy_text(text.clone());
            }
        });
        ui.label(RichText::new(text).monospace().small());
    }
    ui.add_space(4.0);
    hint(ui, crate::tl!("ChatGPT's app only connects to remote HTTPS servers: use Codex CLI, or the built-in assistant with OpenAI GPT."));
}

/// `cadcraft-cli` next to the running app, and whether it is there.
fn cli_path() -> (String, bool) {
    let exe = format!("cadcraft-cli{}", std::env::consts::EXE_SUFFIX);
    match std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(&exe))) {
        Some(p) => (p.to_string_lossy().into_owned(), p.is_file()),
        None => (exe, false),
    }
}

/// A path for the shell: double quotes on Windows (where paths can't contain them), single quotes
/// elsewhere.
fn shell_quote(s: &str) -> String {
    if cfg!(windows) { format!("\"{s}\"") } else { format!("'{}'", s.replace('\'', r"'\''")) }
}

/// The MCP client configurations that start `cli mcp --connect 127.0.0.1:<port> --lazy`, titled.
fn snippets(cli: &str, port: u16) -> [(&'static str, String); 4] {
    let addr = format!("127.0.0.1:{port}");
    // A JSON string is also a TOML basic string: the same escapes for `\` and `"`.
    let quote = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let (command, args) = (quote(cli), ["mcp", "--connect", addr.as_str(), "--lazy"].map(quote).join(", "));
    let json = format!("{{\n  \"mcpServers\": {{\n    \"cadino\": {{\n      \"command\": {command},\n      \"args\": [{args}]\n    }}\n  }}\n}}");
    [
        ("Claude Desktop · claude_desktop_config.json", json.clone()),
        ("Claude Code", format!("claude mcp add cadino -- {} mcp --connect {addr} --lazy", shell_quote(cli))),
        ("Codex CLI · ~/.codex/config.toml", format!("[mcp_servers.cadino]\ncommand = {command}\nargs = [{args}]")),
        ("Cursor · .cursor/mcp.json", json),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_escape_windows_paths() {
        let cli = r"C:\Program Files\Cadino\cadcraft-cli.exe";
        let [(_, desktop), (_, code), (_, codex), (_, cursor)] = snippets(cli, 7979);
        let v: Value = serde_json::from_str(&desktop).unwrap();
        assert_eq!(v["mcpServers"]["cadino"]["command"], cli);
        assert_eq!(v["mcpServers"]["cadino"]["args"], json!(["mcp", "--connect", "127.0.0.1:7979", "--lazy"]));
        assert_eq!(cursor, desktop);
        assert_eq!(
            codex,
            "[mcp_servers.cadino]\ncommand = \"C:\\\\Program Files\\\\Cadino\\\\cadcraft-cli.exe\"\nargs = [\"mcp\", \"--connect\", \"127.0.0.1:7979\", \"--lazy\"]"
        );
        if cfg!(windows) {
            assert_eq!(code, r#"claude mcp add cadino -- "C:\Program Files\Cadino\cadcraft-cli.exe" mcp --connect 127.0.0.1:7979 --lazy"#);
        }
    }

    #[test]
    fn snippets_quote_unusual_paths() {
        let cli = "/home/o'neil/My \"tools\"/cadcraft-cli";
        let [(_, desktop), (_, code), (_, codex), _] = snippets(cli, 8123);
        let v: Value = serde_json::from_str(&desktop).unwrap();
        assert_eq!(v["mcpServers"]["cadino"]["command"], cli);
        assert!(codex.contains(r#"command = "/home/o'neil/My \"tools\"/cadcraft-cli""#), "{codex}");
        if !cfg!(windows) {
            assert!(code.contains(r#"-- '/home/o'\''neil/My "tools"/cadcraft-cli' mcp --connect 127.0.0.1:8123"#), "{code}");
        }
    }
}
