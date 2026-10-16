//! The agent loop end to end against a local mock HTTP server and a fake backend: no network.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{Value, json};

use crate::anthropic::tests::{reply, tool_use};
use crate::*;

/// A request the mock server saw: its head (request line and headers) and its JSON body.
type Seen = Arc<Mutex<Vec<(String, Value)>>>;

/// A server on 127.0.0.1 answering one request per connection with `replies` in order
/// (status, content type, body), recording what it was sent.
fn mock(replies: Vec<(u16, &'static str, String)>) -> (String, Seen, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Seen::default();
    let log = seen.clone();
    let t = std::thread::spawn(move || {
        for (status, ctype, body) in replies {
            let (mut s, _) = listener.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line.to_ascii_lowercase());
            }
            let len = head.lines().find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse().ok())).unwrap_or(0);
            let mut b = vec![0; len];
            r.read_exact(&mut b).unwrap();
            log.lock().unwrap().push((head, serde_json::from_slice(&b).unwrap_or_default()));
            write!(s, "HTTP/1.1 {status} X\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (url, seen, t)
}

const SSE: &str = "text/event-stream";

/// Records calls and answers like the headless backend would; optionally sets `cancel` when called.
#[derive(Default)]
struct Fake {
    calls: Vec<(String, Value)>,
    cancel: Option<Arc<AtomicBool>>,
}

impl tools::Backend for Fake {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.calls.push((method.to_string(), params));
        if let Some(c) = &self.cancel {
            c.store(true, Ordering::Relaxed);
        }
        Ok(match method {
            "ui.render" => json!({"pngBase64": "iVBORw0KGgo=", "width": 16, "height": 16}),
            _ => json!({"prompt": "Command:", "running": null}),
        })
    }
    fn has_ui(&self) -> bool {
        false
    }
    fn describe(&self) -> String {
        "fake".into()
    }
}

fn anthropic(url: &str) -> Config {
    Config { base_url: url.into(), api_key: "k".into(), ..Config::default() }
}

fn go(cfg: &Config, conv: &mut Conversation, b: &mut Fake, cancel: &AtomicBool) -> (Result<(), String>, Vec<Event>) {
    let mut events = Vec::new();
    let r = run(cfg, conv, "Draw a circle", b, cancel, &mut |e| events.push(e));
    (r, events)
}

fn text(t: &str) -> (Value, Vec<Value>) {
    (json!({"type": "text", "text": ""}), vec![json!({"type": "text_delta", "text": t})])
}

#[test]
fn anthropic_tool_round_trip() {
    let thinking = (
        json!({"type": "thinking", "thinking": "", "signature": ""}),
        vec![json!({"type": "thinking_delta", "thinking": "A circle, then look."}), json!({"type": "signature_delta", "signature": "c2ln"})],
    );
    let first = reply(
        &[
            thinking,
            text("Drawing."),
            tool_use("toolu_1", "command_line", &["{\"text\": ", "\"circle 0,0 5\"}"]),
            tool_use("toolu_2", "render", &["{\"view\": \"iso\"}"]),
        ],
        "tool_use",
    );
    let (url, seen, server) = mock(vec![(200, SSE, first), (200, SSE, reply(&[text("Done.")], "end_turn"))]);
    let (mut conv, mut b) = (Conversation::new(), Fake::default());
    let (r, events) = go(&anthropic(&url), &mut conv, &mut b, &AtomicBool::new(false));
    server.join().unwrap();
    assert_eq!(r, Ok(()));
    assert_eq!(b.calls, [("cmdline.input".to_string(), json!({"text": "circle 0,0 5"})), ("ui.render".to_string(), json!({"view": "iso"}))]);
    assert!(events.contains(&Event::ToolCall { id: "toolu_1".into(), name: "command_line".into(), input: json!({"text": "circle 0,0 5"}) }));
    assert!(events.iter().any(|e| matches!(e, Event::ToolResult { id, ok: true, image_png_base64: Some(_), .. } if id == "toolu_2")));
    assert_eq!(events.iter().filter(|e| matches!(e, Event::TurnDone { .. })).count(), 2);
    assert_eq!(
        events.last(),
        Some(&Event::TurnDone {
            stop_reason: "end_turn".into(),
            usage: json!({"input_tokens": 10, "cache_read_input_tokens": 0, "output_tokens": 7})
        })
    );

    let seen = seen.lock().unwrap();
    let (head, body) = &seen[0];
    assert!(head.starts_with("post /v1/messages ") && head.contains("x-api-key: k") && head.contains("anthropic-version: 2023-06-01"), "{head}");
    assert!(!head.contains("anthropic-beta"), "no fallback beta off Anthropic's own API: {head}");
    assert_eq!(body["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!((body["stream"].clone(), body["model"].clone(), body["max_tokens"].clone()), (json!(true), json!("claude-opus-5-5"), json!(32000)));
    assert_eq!(body["thinking"], json!({"type": "adaptive", "display": "summarized"}));
    assert!(body["system"][0]["text"].as_str().unwrap().starts_with(prompt::SYSTEM));
    let tools = body["tools"].as_array().unwrap();
    assert!(tools.iter().all(|t| t["eager_input_streaming"] == true && t["input_schema"]["type"] == "object"));
    assert!(!tools.iter().any(|t| t["name"] == "save" || t["name"] == "open"), "files are off by default");
    // The second request carries the assistant turn verbatim (thinking with its signature) and
    // every result in one user message.
    let msgs = seen[1].1["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[1]["content"][0], json!({"type": "thinking", "thinking": "A circle, then look.", "signature": "c2ln"}));
    let results = msgs[2]["content"].as_array().unwrap();
    assert_eq!(results.iter().map(|r| r["tool_use_id"].as_str().unwrap()).collect::<Vec<_>>(), ["toolu_1", "toolu_2"]);
    assert_eq!(results[0]["is_error"], false);
    assert_eq!(results[1]["content"][0]["source"], json!({"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}));
    assert_eq!(conv.messages.len(), 4);
}

#[test]
fn openai_compatible_tool_round_trip() {
    let chunks = |c: &[Value]| -> String { c.iter().map(|v| format!("data: {v}\n\n")).chain(["data: [DONE]\n\n".to_string()]).collect() };
    let first = chunks(&[
        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"name": "execute", "arguments": "{\"command\": \"circle\", "}}]}}]}),
        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "\"params\": {\"center\": [0, 0], \"radius\": 5}}"}}]}, "finish_reason": "tool_calls"}]}),
    ]);
    let second = chunks(&[json!({"choices": [{"delta": {"content": "Done."}, "finish_reason": "stop"}]})]);
    let (url, seen, server) = mock(vec![(200, SSE, first), (200, SSE, second)]);
    let cfg = Config { provider: ProviderKind::Custom, base_url: url, model: "qwen".into(), ..Config::default() };
    // A conversation from another model starts over.
    let mut conv = Conversation { messages: vec![json!({"role": "user", "content": "old"})], ..Conversation::default() };
    conv.model = Some((ProviderKind::Anthropic, "claude-opus-5-5".into()));
    let mut b = Fake::default();
    let (r, events) = go(&cfg, &mut conv, &mut b, &AtomicBool::new(false));
    server.join().unwrap();
    assert_eq!(r, Ok(()));
    assert!(matches!(&events[0], Event::Notice(n) if n.contains("starts over")), "{events:?}");
    assert_eq!(b.calls, [("engine.execute".to_string(), json!({"command": "circle", "params": {"center": [0, 0], "radius": 5}}))]);
    assert!(events.contains(&Event::Text("Done.".into())));

    let seen = seen.lock().unwrap();
    let (head, body) = &seen[0];
    assert!(head.starts_with("post /chat/completions ") && !head.contains("authorization"), "{head}");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1], json!({"role": "user", "content": "Draw a circle"}));
    assert_eq!(body["tools"][0]["type"], "function");
    assert!(body.get("stream_options").is_none());
    let msgs = seen[1].1["messages"].as_array().unwrap();
    assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_1", "a missing id is made up");
    assert_eq!(msgs[2]["content"], Value::Null);
    assert_eq!(msgs[3]["role"], "tool");
    assert_eq!(msgs[3]["tool_call_id"], "call_1");
    assert_eq!(conv.messages.len(), 4);
}

#[test]
fn overload_is_retried_and_a_bad_key_is_not() {
    let overloaded = json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}).to_string();
    let (url, seen, server) = mock(vec![(529, "application/json", overloaded), (200, SSE, reply(&[text("Hi")], "end_turn"))]);
    let (mut conv, mut b) = (Conversation::new(), Fake::default());
    let (r, events) = go(&anthropic(&url), &mut conv, &mut b, &AtomicBool::new(false));
    server.join().unwrap();
    assert_eq!(r, Ok(()));
    assert!(matches!(&events[0], Event::Notice(n) if n.contains("529") && n.contains("Overloaded")), "{events:?}");
    assert!(events.contains(&Event::Text("Hi".into())));
    assert_eq!(seen.lock().unwrap().len(), 2);

    let bad = json!({"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}}).to_string();
    let (url, seen, server) = mock(vec![(401, "application/json", bad)]);
    let mut conv = Conversation::new();
    let (r, _) = go(&anthropic(&url), &mut conv, &mut b, &AtomicBool::new(false));
    server.join().unwrap();
    let e = r.unwrap_err();
    assert!(e.contains("invalid x-api-key") && e.contains("Options ▸ AI"), "{e}");
    assert!(conv.is_empty(), "the failed message is not sent again with the next one");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn a_refusal_appends_nothing() {
    let refusal = json!([
        {"type": "message_start", "message": {"usage": {}}},
        {"type": "message_delta", "delta": {"stop_reason": "refusal", "stop_details": {"type": "refusal", "category": "cyber", "explanation": "Declined."}}, "usage": {}},
        {"type": "message_stop"},
    ]);
    let (url, _, server) = mock(vec![(200, SSE, crate::anthropic::tests::sse(refusal.as_array().unwrap()))]);
    let (mut conv, mut b) = (Conversation::new(), Fake::default());
    let (r, events) = go(&anthropic(&url), &mut conv, &mut b, &AtomicBool::new(false));
    server.join().unwrap();
    assert_eq!(r, Ok(()));
    assert!(events.iter().any(|e| matches!(e, Event::Error(m) if m.contains("cyber") && m.contains("Declined."))), "{events:?}");
    assert!(conv.is_empty(), "the declined request is not asked again");
}

#[test]
fn cancel_leaves_a_valid_history() {
    // Stopped by the first of two tools: the second is answered as cancelled, nothing else is sent.
    let calls = reply(&[tool_use("a", "command_line", &["{\"text\": \"line\"}"]), tool_use("b", "cancel", &[])], "tool_use");
    let (url, seen, server) = mock(vec![(200, SSE, calls)]);
    let flag = Arc::new(AtomicBool::new(false));
    let mut b = Fake { cancel: Some(flag.clone()), ..Fake::default() };
    let mut conv = Conversation::new();
    let (r, events) = go(&anthropic(&url), &mut conv, &mut b, &flag);
    server.join().unwrap();
    assert_eq!((r, b.calls.len(), seen.lock().unwrap().len()), (Ok(()), 1, 1));
    assert_eq!(events.last(), Some(&Event::Notice("Stopped".into())));
    let ids = |m: &Value, key: &str| -> Vec<String> {
        m["content"].as_array().unwrap().iter().filter_map(|c| c[key].as_str().map(str::to_string)).collect()
    };
    let [_, asked, answered] = conv.messages.as_slice() else { panic!("{:?}", conv.messages) };
    assert_eq!(ids(asked, "id"), ids(answered, "tool_use_id"));
    assert_eq!(answered["content"][1]["is_error"], true);
    assert_eq!(answered["content"][1]["content"][0]["text"], CANCELLED);

    // Stopped mid-stream: the partial reply is dropped.
    let (url, _, server) = mock(vec![(200, SSE, reply(&[text("Hel"), text("lo")], "end_turn"))]);
    let flag = AtomicBool::new(false);
    let mut conv = Conversation::new();
    let mut events = Vec::new();
    let r = run(&anthropic(&url), &mut conv, "Hi", &mut Fake::default(), &flag, &mut |e| {
        if matches!(e, Event::Text(_)) {
            flag.store(true, Ordering::Relaxed);
        }
        events.push(e);
    });
    server.join().unwrap();
    assert_eq!(r, Ok(()));
    assert_eq!(events, [Event::Text("Hel".into()), Event::Notice("Stopped".into())]);
    assert_eq!(conv.messages.len(), 1, "only the user's message");
}

#[test]
fn forty_tool_rounds_at_most() {
    let replies = (0..=MAX_ROUNDS).map(|i| (200, SSE, reply(&[tool_use(&format!("t{i}"), "cancel", &[])], "tool_use"))).collect();
    let (url, seen, server) = mock(replies);
    let (mut conv, mut b) = (Conversation::new(), Fake::default());
    let (r, events) = go(&anthropic(&url), &mut conv, &mut b, &AtomicBool::new(false));
    server.join().unwrap();
    assert_eq!(r, Ok(()));
    assert_eq!((b.calls.len(), seen.lock().unwrap().len()), (MAX_ROUNDS, MAX_ROUNDS + 1));
    assert!(matches!(events.last(), Some(Event::Notice(n)) if n.contains("40 tool rounds")), "{:?}", events.last());
    let last = conv.messages.last().unwrap();
    assert_eq!((last["content"][0]["tool_use_id"].as_str(), last["content"][0]["is_error"].as_bool()), (Some("t40"), Some(true)));
}

#[test]
fn file_access_is_refused_unless_allowed() {
    let mut b = Fake::default();
    let call = |name: &str, input: Value| Call { id: "1".into(), name: name.into(), input: Ok(input) };
    for c in [
        call("render", json!({"path": "C:/x.png"})),
        call("execute", json!({"command": "saveas", "params": {"path": "x.dxf"}})),
        call("execute", json!({"command": "x", "params": {"list": [{"path": "y"}]}})),
        call("save", json!({})),
    ] {
        assert_eq!(execute(&mut b, &c, false), Output::error(FILES_OFF), "{c:?}");
    }
    assert!(b.calls.is_empty());
    assert!(execute(&mut b, &call("render", json!({"path": "x.png"})), true).ok);
    let names = |files| tool_list(true, files).iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect::<Vec<_>>();
    assert!(!names(false).iter().any(|n| n == "open" || n == "save") && names(false).contains(&"screenshot".to_string()));
    assert!(names(true).iter().any(|n| n == "save"));
}

#[test]
fn bad_calls_become_error_results() {
    let mut b = Fake::default();
    let raw = Call { id: "1".into(), name: "command_line".into(), input: Err("{\"text\": \"x".into()) };
    let o = execute(&mut b, &raw, true);
    assert!(!o.ok && o.text.contains("INVALID_JSON") && o.text.contains("\\\"x"), "{o:?}");
    let o = execute(&mut b, &Call { id: "2".into(), name: "nope".into(), input: Ok(json!({})) }, true);
    assert!(!o.ok && o.text.contains("unknown tool"), "{o:?}");
    let o = execute(&mut b, &Call { id: "3".into(), name: "command_line".into(), input: Ok(json!({"text": 5})) }, true);
    assert!(!o.ok && o.text.contains("must match"), "{o:?}");
    assert!(b.calls.is_empty());
}

#[test]
fn long_results_are_cut() {
    let long = "é".repeat(MAX_RESULT_CHARS + 5);
    let o = mcp_output(&json!({"content": [{"type": "text", "text": long}], "isError": true}));
    assert!(!o.ok && o.text.starts_with(&"é".repeat(MAX_RESULT_CHARS)) && o.text.contains("5 more characters"), "{}", &o.text[o.text.len() - 80..]);
}

#[test]
fn provider_defaults_and_config_serde() {
    let c: Config = serde_json::from_value(json!({"provider": "openai", "api_key": "never"})).unwrap();
    assert_eq!((c.provider, c.api_key.as_str(), c.max_tokens, c.effort.as_str(), c.allow_files), (ProviderKind::OpenAi, "", 32000, "high", false));
    assert_eq!(serde_json::to_value(ProviderKind::OpenAi).unwrap(), "openai");
    assert!(serde_json::to_value(&c).unwrap().get("api_key").is_none(), "the key is never saved with the settings");
    assert_eq!(Config { model: " ".into(), ..c }.model(), "gpt-5");
    assert_eq!(Config { base_url: "http://h:1/v1/".into(), ..Config::default() }.base(), "http://h:1/v1");
    assert!(!ProviderKind::Custom.needs_key() && ProviderKind::Custom.default_model().is_empty());
}
