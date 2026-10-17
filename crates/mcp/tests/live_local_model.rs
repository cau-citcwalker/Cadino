//! The built-in assistant (`cadcraft_ai::run`) driving a headless session through a real local
//! model. Ignored by default; with Ollama serving the model, run
//! `cargo test -p cadcraft-mcp --test live_local_model -- --ignored --nocapture`
//! (`CADINO_LIVE_MODEL` picks another model, `CADINO_LIVE_URL` another server).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use cadcraft_ai::{Config, Conversation, Event, ProviderKind, run};
use cadcraft_mcp::{Backend, Headless};
use serde_json::{Value, json};

fn config() -> Config {
    let model = std::env::var("CADINO_LIVE_MODEL").unwrap_or_else(|_| "qwen2.5:7b-instruct".into());
    let base_url = std::env::var("CADINO_LIVE_URL").unwrap_or_else(|_| "http://localhost:11434/v1".into());
    Config { provider: ProviderKind::Custom, base_url, model, ..Config::default() }
}

/// Ask `text` and print the run; the tool calls made.
fn ask(b: &mut dyn Backend, text: &str) -> Vec<String> {
    let mut calls = Vec::new();
    let r = run(&config(), &mut Conversation::new(), text, b, &Arc::new(AtomicBool::new(false)), &mut |e| match e {
        Event::Text(t) => print!("{t}"),
        Event::ToolCall { name, input, .. } => {
            println!("\n> {name} {input}");
            calls.push(name);
        }
        Event::ToolResult { ok, text, .. } => println!("< ok={ok} {}", text.chars().take(300).collect::<String>()),
        e => println!("\n{e:?}"),
    });
    assert_eq!(r, Ok(()));
    calls
}

fn circles(b: &mut dyn Backend, params: Value) -> u64 {
    let mut p = params;
    p["type"] = json!("Circle");
    let r = b.call("engine.execute", json!({"command": "entities", "params": p})).unwrap();
    r["count"].as_u64().unwrap_or(0)
}

#[test]
#[ignore = "needs a local model server (Ollama at localhost:11434)"]
fn live_local_model_draws_a_circle() {
    let mut b = Headless::default();
    ask(&mut b, "Draw a circle with radius 5 centred at 0,0, then stop.");
    assert!(circles(&mut b, json!({})) >= 1, "no circle");
}

/// The in-app assistant's view: a window is attached, so `has_ui` is true. The window's own
/// methods fail here; `ui.render` is served headless.
struct WithWindow(Headless);

impl Backend for WithWindow {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        if method.starts_with("ui.") && method != "ui.render" {
            return Err(format!("{method}: no window in this test"));
        }
        self.0.call(method, params)
    }
    fn has_ui(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "headless, as if in the app".into()
    }
}

/// In the app (with a window) the model made tool calls as text; it must make a real one.
#[test]
#[ignore = "needs a local model server (Ollama at localhost:11434)"]
fn live_local_model_calls_tools_in_the_app() {
    let mut b = WithWindow(Headless::default());
    let calls = ask(&mut b, "Draw a circle with radius 5 centred at 0,0");
    assert!(!calls.is_empty(), "no tool call");
    assert!(circles(&mut b, json!({"window": [[-6, -6], [6, 6]], "crossing": false})) >= 1, "no circle at 0,0");
}

/// A large tool result (here 400 circles' records) used to overflow the server's context on its
/// own, which cut the system prompt and the tool format; the model must still go on with tools.
#[test]
#[ignore = "needs a local model server (Ollama at localhost:11434)"]
fn live_local_model_goes_on_after_a_large_result() {
    let mut b = WithWindow(Headless::default());
    for i in 0..400 {
        b.call("engine.execute", json!({"command": "circle", "params": {"center": [i * 20, 0], "radius": 3}})).unwrap();
    }
    ask(&mut b, "Call inspect_drawing with entities true. Then draw a circle with radius 5 centred at 0,5000.");
    assert!(circles(&mut b, json!({"window": [[-6, 4994], [6, 5006]], "crossing": false})) >= 1, "no circle at 0,5000");
}

#[test]
#[ignore = "needs a local model server (Ollama at localhost:11434)"]
fn live_local_server_lists_models() {
    let cfg = config();
    let r = cadcraft_ai::test_connection(&cfg);
    println!("{r:?}: {:?}", cadcraft_ai::list_models(&cfg));
    assert!(r.is_ok_and(|m| m.starts_with("Connected: ")));
}

/// qwen2.5:7b made the box again in a later round of the same message (and rectangles and circles
/// twice): it is made once at most. Needs `--features cadcraft-engine/occt` to make a box at all.
#[test]
#[ignore = "needs a local model server (Ollama at localhost:11434)"]
fn live_local_model_makes_a_box_once() {
    let mut b = WithWindow(Headless::default());
    ask(&mut b, "Make a box 600 x 400 x 750 at the origin and show me an iso render.");
    let r = b.call("engine.execute", json!({"command": "entities", "params": {"type": "3DSOLID"}})).unwrap();
    println!("\n{} box(es)", r["count"]);
    assert!(r["count"].as_u64().unwrap_or(0) <= 1, "made twice: {r}");
}
