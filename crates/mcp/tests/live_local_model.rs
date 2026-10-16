//! The built-in assistant (`cadcraft_ai::run`) driving a headless session through a real local
//! model. Ignored by default; with Ollama serving the model, run
//! `cargo test -p cadcraft-mcp --test live_local_model -- --ignored --nocapture`
//! (`CADINO_LIVE_MODEL` picks another model).

use std::sync::atomic::AtomicBool;

use cadcraft_ai::{Config, Conversation, Event, ProviderKind, run};
use cadcraft_mcp::{Backend, Headless};
use serde_json::json;

#[test]
#[ignore = "needs a local model server (Ollama at localhost:11434)"]
fn live_local_model_draws_a_circle() {
    let model = std::env::var("CADINO_LIVE_MODEL").unwrap_or_else(|_| "qwen2.5:7b-instruct".into());
    let cfg = Config { provider: ProviderKind::Custom, base_url: "http://localhost:11434/v1".into(), model, ..Config::default() };
    let mut b = Headless::default();
    let mut conv = Conversation::new();
    let r = run(&cfg, &mut conv, "Draw a circle with radius 5 centred at 0,0, then stop.", &mut b, &AtomicBool::new(false), &mut |e| match e {
        Event::Text(t) => print!("{t}"),
        Event::ToolCall { name, input, .. } => println!("\n> {name} {input}"),
        Event::ToolResult { ok, text, .. } => println!("< ok={ok} {}", text.chars().take(300).collect::<String>()),
        e => println!("\n{e:?}"),
    });
    assert_eq!(r, Ok(()));
    let circles = b.call("engine.execute", json!({"command": "entities", "params": {"type": "Circle"}})).unwrap();
    println!("\ncircles: {circles}");
    assert!(circles["count"].as_u64().is_some_and(|n| n >= 1), "no circle: {circles}");
}

#[test]
#[ignore = "needs a local model server (Ollama at localhost:11434)"]
fn live_local_server_lists_models() {
    let cfg = Config { provider: ProviderKind::Custom, base_url: "http://localhost:11434/v1".into(), ..Config::default() };
    let r = cadcraft_ai::test_connection(&cfg);
    println!("{r:?}: {:?}", cadcraft_ai::list_models(&cfg));
    assert!(r.is_ok_and(|m| m.starts_with("Connected: ")));
}
