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

/// The catalog [`Fake`] serves: a few file commands and others, with aliases (`qs`: the user's).
const CATALOG: &str = r#"[{"id": "qsave", "aliases": ["save"], "userAliases": ["qs"]}, {"id": "wblock", "aliases": ["w"]},
    {"id": "stepout", "aliases": ["StepExport"]}, {"id": "-xref", "aliases": ["-xr"]}, {"id": "insert", "aliases": ["i"]},
    {"id": "circle", "aliases": ["c"]}, {"id": "options", "aliases": ["op", "preferences"]}, {"id": "close", "aliases": []},
    {"id": "closeall", "aliases": []}, {"id": "aliasedit", "aliases": []}, {"id": "zoom", "aliases": ["z"]}]"#;

impl tools::Backend for Fake {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        // Read once per run while file access is off; not part of what the tests check.
        if method == "engine.commands" {
            return Ok(serde_json::from_str(CATALOG).unwrap());
        }
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

fn no_stop() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

fn anthropic(url: &str) -> Config {
    Config { base_url: url.into(), api_key: "k".into(), ..Config::default() }
}

fn go(cfg: &Config, conv: &mut Conversation, b: &mut Fake, cancel: &Arc<AtomicBool>) -> (Result<(), String>, Vec<Event>) {
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
    let (r, events) = go(&anthropic(&url), &mut conv, &mut b, &no_stop());
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
    // The MCP annotations stay out of the API's tool schema (unknown fields are refused).
    let keys = ["name", "description", "input_schema", "eager_input_streaming"];
    assert!(tools.iter().all(|t| t.as_object().unwrap().keys().all(|k| keys.contains(&k.as_str()))), "{tools:?}");
    assert!(tools.iter().any(|t| t["name"] == "save") && tools.iter().any(|t| t["name"] == "open"), "the same tools whatever the settings");
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
    let (r, events) = go(&cfg, &mut conv, &mut b, &no_stop());
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
    let keys = ["name", "description", "parameters"];
    assert!(
        body["tools"].as_array().unwrap().iter().all(|t| t["function"].as_object().unwrap().keys().all(|k| keys.contains(&k.as_str()))),
        "no MCP annotations"
    );
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
    let (r, events) = go(&anthropic(&url), &mut conv, &mut b, &no_stop());
    server.join().unwrap();
    assert_eq!(r, Ok(()));
    assert!(matches!(&events[0], Event::Notice(n) if n.contains("529") && n.contains("Overloaded")), "{events:?}");
    assert!(events.contains(&Event::Text("Hi".into())));
    assert_eq!(seen.lock().unwrap().len(), 2);

    let bad = json!({"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}}).to_string();
    let (url, seen, server) = mock(vec![(401, "application/json", bad)]);
    let mut conv = Conversation::new();
    let (r, _) = go(&anthropic(&url), &mut conv, &mut b, &no_stop());
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
    let (r, events) = go(&anthropic(&url), &mut conv, &mut b, &no_stop());
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

    // Stopped mid-stream: the partial reply is dropped, and so is the message it answered, so
    // it doesn't go out again with the next one.
    let (url, _, server) = mock(vec![(200, SSE, reply(&[text("Hel"), text("lo")], "end_turn"))]);
    let flag = no_stop();
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
    assert!(conv.is_empty(), "{:?}", conv.messages);
    // Stopped before the first request.
    let stopped = Arc::new(AtomicBool::new(true));
    let (r, _) = go(&anthropic("http://127.0.0.1:9"), &mut conv, &mut Fake::default(), &stopped);
    assert_eq!(r, Ok(()));
    assert!(conv.is_empty());
}

#[test]
fn forty_tool_rounds_at_most() {
    let replies = (0..=MAX_ROUNDS).map(|i| (200, SSE, reply(&[tool_use(&format!("t{i}"), "cancel", &[])], "tool_use"))).collect();
    let (url, seen, server) = mock(replies);
    let (mut conv, mut b) = (Conversation::new(), Fake::default());
    let (r, events) = go(&anthropic(&url), &mut conv, &mut b, &no_stop());
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
    let off = Guard::new(&mut b, false);
    let call = |name: &str, input: Value| Call { id: "1".into(), name: name.into(), input: Ok(input) };
    let typed = |name: &str, text: &str| call(name, json!({ "text": text }));
    for c in [
        call("render", json!({"path": "C:/x.png"})),
        call("execute", json!({"command": "saveas", "params": {"path": "x.dxf"}})),
        call("execute", json!({"command": "x", "params": {"list": [{"path": "y"}]}})),
        call("save", json!({})),
        call("open", json!({"path": "x.dxf"})),
        // File commands without a path, by id or alias, and parameters naming files.
        call("execute", json!({"command": "qsave"})),
        call("execute", json!({"command": "SAVE", "params": {}})),
        call("execute", json!({"command": "-xref", "params": {"option": "reload"}})),
        call("execute", json!({"command": "insert", "params": {"name": "A", "file": "C:/a.dwg"}})),
        call("execute", json!({"command": "cutlist", "params": {"csv": "bom.csv"}})),
        call("execute", json!({"command": "lisp", "params": {"code": "(open \"x\" \"w\")"}})),
        call("execute", json!({"command": "options", "params": {"SAVEFILEPATH": "C:/x"}})),
        // File ▸ Open Recent opens without asking; a new alias could rename a file command; and
        // the user's own aliases count as names.
        call("execute", json!({"command": "ui.recent.0"})),
        typed("command_line", "ui.recent.1"),
        call("execute", json!({"command": "aliasedit", "params": {"set": {"zz": "qsave"}}})),
        typed("command_line", "qs"),
        // Typed: the command token, as id or alias, with AutoCAD's prefixes.
        typed("command_line", "qsave"),
        typed("command_line", "_.SAVE"),
        typed("command_line", "w"),
        typed("command_line", "stepexport out.stp"),
        typed("command_line", "-XR reload *"),
        typed("command_line", "line 0,0 'qsave"),
        typed("command_line", "(command \"saveas\" \"x.dxf\")"),
        typed("command_line", "insert {\"name\": \"A\", \"file\": \"a.dwg\"}"),
        typed("script", "circle 0,0 5\n_qsave"),
        typed("script", "zoom e save"),
    ] {
        assert_eq!(execute(&mut b, &c, &off, MAX_RESULT_CHARS), Output::error(FILES_OFF), "{c:?}");
    }
    assert!(b.calls.is_empty());
    for c in [
        // A curve's handle is no file; a one-letter keyword after the command isn't W(BLOCK).
        call("execute", json!({"command": "sweep", "params": {"handles": ["2A"], "path": "2B"}})),
        call("execute", json!({"command": "insert", "params": {"name": "A", "at": [0, 0]}})),
        call("execute", json!({"command": "options", "params": {"theme": "dark"}})),
        typed("command_line", "circle 0,0 5"),
        typed("script", "erase w 0,0 10,10\n\nzoom e"),
        typed("command_line", "c 0,0 5"),
    ] {
        assert!(execute(&mut b, &c, &off, MAX_RESULT_CHARS).ok, "{c:?}");
    }
    let on = Guard::new(&mut b, true);
    assert!(execute(&mut b, &call("render", json!({"path": "x.png"})), &on, MAX_RESULT_CHARS).ok);
    // The tools stay the same whatever the settings (a conversation's tools never change).
    let names = |local| tool_list(true, local).iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect::<Vec<_>>();
    assert!(names(false).iter().any(|n| n == "save") && names(false).contains(&"screenshot".to_string()));
    // A local model gets neither the window's tools nor `script`.
    assert!(!names(true).iter().any(|n| ["screenshot", "ui_inspect", "ui_click", "script"].contains(&n.as_str())), "{:?}", names(true));
}

/// Whatever file access is, the assistant changes no AI or MCP setting (it could give itself
/// file access, open the control port or send the chat elsewhere), closes no drawing and doesn't
/// quit.
#[test]
fn settings_closing_and_quitting_are_the_users() {
    let mut b = Fake::default();
    let call = |name: &str, input: Value| Call { id: "1".into(), name: name.into(), input: Ok(input) };
    let typed = |name: &str, text: &str| call(name, json!({ "text": text }));
    for allow_files in [false, true] {
        let guard = Guard::new(&mut b, allow_files);
        for (c, why) in [
            (call("execute", json!({"command": "options", "params": {"aiAllowFiles": true}})), SETTINGS_OFF),
            (call("execute", json!({"command": "OP", "params": {"mcpEnabled": true}})), SETTINGS_OFF),
            (call("execute", json!({"command": "preferences", "params": {"aiBaseUrl": "http://evil/v1", "aiProvider": "custom"}})), SETTINGS_OFF),
            (typed("command_line", "options {\"aiCustomInstructions\": \"x\"}"), SETTINGS_OFF),
            (call("execute", json!({"command": "closeall"})), CLOSE_OFF),
            (call("execute", json!({"command": "close", "params": {"index": 0}})), CLOSE_OFF),
            (call("execute", json!({"command": "ui.quit"})), CLOSE_OFF),
            (typed("command_line", "closeall {}"), CLOSE_OFF),
            (typed("script", "line 0,0 1,1\n\nclose"), CLOSE_OFF),
        ] {
            assert_eq!(execute(&mut b, &c, &guard, MAX_RESULT_CHARS), Output::error(why), "{c:?}");
        }
    }
    assert!(b.calls.is_empty(), "{:?}", b.calls);
}

#[test]
fn bad_calls_become_error_results() {
    let mut b = Fake::default();
    let on = Guard::new(&mut b, true);
    let raw = Call { id: "1".into(), name: "command_line".into(), input: Err("{\"text\": \"x".into()) };
    let o = execute(&mut b, &raw, &on, MAX_RESULT_CHARS);
    assert!(!o.ok && o.text.contains("INVALID_JSON") && o.text.contains("\\\"x"), "{o:?}");
    let o = execute(&mut b, &Call { id: "2".into(), name: "nope".into(), input: Ok(json!({})) }, &on, MAX_RESULT_CHARS);
    assert!(!o.ok && o.text.contains("unknown tool"), "{o:?}");
    let o = execute(&mut b, &Call { id: "3".into(), name: "command_line".into(), input: Ok(json!({"text": 5})) }, &on, MAX_RESULT_CHARS);
    assert!(!o.ok && o.text.contains("must match"), "{o:?}");
    assert!(b.calls.is_empty());
}

#[test]
fn long_results_are_cut() {
    let long = "é".repeat(MAX_RESULT_CHARS + 5);
    let o = mcp_output(&json!({"content": [{"type": "text", "text": long}], "isError": true}), MAX_RESULT_CHARS);
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

/// A local model whose server lost the tool format writes `<tool_call>` blocks as text: they run
/// as calls, never show as text, and the history pairs each call with its result.
#[test]
fn tool_calls_written_as_text_run() {
    let chunks = |c: &[&str]| -> String {
        c.iter().map(|t| format!("data: {}\n\n", json!({"choices": [{"delta": {"content": t}}]}))).chain(["data: [DONE]\n\n".to_string()]).collect()
    };
    let first = chunks(&[
        "Drawing it.\n<tool",
        "_call>\n{\"name\": \"execute\", \"arguments\": {\"command\": \"circle\", ",
        "\"params\": {\"center\": [0, 0], \"radius\": 5}}}\n</tool_call>",
    ]);
    let (url, seen, server) = mock(vec![(200, SSE, first), (200, SSE, chunks(&["Done."]))]);
    let cfg = Config { provider: ProviderKind::Custom, base_url: url, model: "qwen".into(), ..Config::default() };
    let (mut conv, mut b) = (Conversation::new(), Fake::default());
    let (r, events) = go(&cfg, &mut conv, &mut b, &no_stop());
    server.join().unwrap();
    assert_eq!(r, Ok(()));
    assert_eq!(b.calls, [("engine.execute".to_string(), json!({"command": "circle", "params": {"center": [0, 0], "radius": 5}}))]);
    let shown: String = events.iter().filter_map(|e| if let Event::Text(t) = e { Some(t.as_str()) } else { None }).collect();
    assert_eq!(shown, "Drawing it.Done.", "the tags never show; the palette shows the two turns apart");
    let seen = seen.lock().unwrap();
    assert!(seen[0].1["messages"][0]["content"].as_str().unwrap().starts_with(prompt::COMPACT));
    let msgs = seen[1].1["messages"].as_array().unwrap();
    assert_eq!(msgs[2]["content"], "Drawing it.");
    assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "execute");
    assert_eq!(msgs[3]["tool_call_id"], msgs[2]["tool_calls"][0]["id"]);
}

/// A local model wrote one call as text next to its real calls: it runs too, but not one it
/// wrote again (the same tool and arguments). Ids stay unique, so each result pairs with its
/// call; the custom server's key goes along as a bearer token.
#[test]
fn calls_written_next_to_real_ones_run_once() {
    let content = |t: &str| json!({"choices": [{"delta": {"content": t}}]});
    let real =
        json!({"index": 0, "id": "call_2", "function": {"name": "execute", "arguments": "{\"command\":\"circle\",\"params\":{\"radius\":5}}"}});
    let first: String = [
        content("Drawing it.\n<tool"),
        content("_call>\n{\"name\": \"execute\", \"arguments\": {\"params\": {\"radius\": 5}, \"command\": \"circle\"}}\n</tool_call>\n"),
        content("render {\"view\": \"iso\"}"),
        json!({"choices": [{"delta": {"tool_calls": [real]}, "finish_reason": "tool_calls"}]}),
    ]
    .iter()
    .map(|v| format!("data: {v}\n\n"))
    .chain(["data: [DONE]\n\n".to_string()])
    .collect();
    let done = format!("data: {}\n\ndata: [DONE]\n\n", content("Done."));
    let (url, seen, server) = mock(vec![(200, SSE, first), (200, SSE, done)]);
    let cfg = Config { provider: ProviderKind::Custom, base_url: url, model: "qwen".into(), api_key: " sk-local ".into(), ..Config::default() };
    let (mut conv, mut b) = (Conversation::new(), Fake::default());
    let (r, events) = go(&cfg, &mut conv, &mut b, &no_stop());
    server.join().unwrap();
    assert_eq!(r, Ok(()));
    let methods: Vec<&str> = b.calls.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(methods, ["engine.execute", "ui.render"]);
    let shown: String = events.iter().filter_map(|e| if let Event::Text(t) = e { Some(t.as_str()) } else { None }).collect();
    assert_eq!(shown, "Drawing it.Done.");
    let seen = seen.lock().unwrap();
    assert!(seen[0].0.contains("authorization: bearer sk-local\r\n"), "{}", seen[0].0);
    let msgs = seen[1].1["messages"].as_array().unwrap();
    assert_eq!(msgs[2]["content"], "Drawing it.");
    let ids: Vec<&Value> = msgs[2]["tool_calls"].as_array().unwrap().iter().map(|c| &c["id"]).collect();
    assert_eq!(ids, [&json!("call_2"), &json!("call_2_")], "the made-up id is not the server's");
    assert_eq!((&msgs[3]["tool_call_id"], &msgs[4]["tool_call_id"]), (ids[0], ids[1]));
    assert_eq!(msgs.len(), 6, "then the image the render returned");
}

/// A local server that isn't running fails at once with a hint, not after 14 s of retries.
#[test]
fn an_unreachable_local_server_fails_fast() {
    let url = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/v1", l.local_addr().unwrap())
    };
    let cfg = Config { provider: ProviderKind::Custom, base_url: url.clone(), model: "qwen".into(), ..Config::default() };
    let start = std::time::Instant::now();
    let (r, events) = go(&cfg, &mut Conversation::new(), &mut Fake::default(), &no_stop());
    assert_eq!(r, Err(text::CANT_REACH.replace("{base}", &url)));
    assert!(events.is_empty(), "no retry notices: {events:?}");
    assert!(start.elapsed() < std::time::Duration::from_secs(10), "{:?}", start.elapsed());
    assert_eq!(list_models(&cfg), Err(text::CANT_REACH.replace("{base}", &url)));
}

/// The system prompt and tools sent with a conversation never change under it (Claude's thinking
/// blocks are bound to them): file access changes nothing in the request, new custom
/// instructions start over.
#[test]
fn settings_never_change_a_conversations_prompt_or_tools() {
    let replies = (0..3).map(|_| (200, SSE, reply(&[text("Hi")], "end_turn"))).collect();
    let (url, seen, server) = mock(replies);
    let mut conv = Conversation::new();
    let mut b = Fake::default();
    let mut cfg = anthropic(&url);
    let _ = go(&cfg, &mut conv, &mut b, &no_stop());
    cfg.allow_files = true;
    let (_, events) = go(&cfg, &mut conv, &mut b, &no_stop());
    assert!(!events.iter().any(|e| matches!(e, Event::Notice(_))), "{events:?}");
    cfg.custom_instructions = "Units are millimetres.".into();
    let (_, events) = go(&cfg, &mut conv, &mut b, &no_stop());
    server.join().unwrap();
    assert_eq!(events.first(), Some(&Event::Notice(text::INSTRUCTIONS_CHANGED.into())));
    let seen = seen.lock().unwrap();
    let [first, second, third] = [&seen[0].1, &seen[1].1, &seen[2].1];
    assert_eq!((&first["tools"], &first["system"]), (&second["tools"], &second["system"]), "the same bytes");
    assert_eq!(second["messages"].as_array().map(Vec::len), Some(3), "the chat went on");
    assert_eq!(third["messages"].as_array().map(Vec::len), Some(1), "and started over");
    assert!(third["system"][0]["text"].as_str().unwrap().ends_with("Units are millimetres."));
    assert_eq!(first["tools"], third["tools"]);
}

/// A base64 PNG of `w` × `h` (its header only, which is all the size check reads).
fn png_header(w: u32, h: u32) -> String {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend(w.to_be_bytes());
    b.extend(h.to_be_bytes());
    b.extend([8, 6, 0, 0, 0]);
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    b.chunks(3)
        .flat_map(|c| {
            let n = c.iter().enumerate().fold(0u32, |n, (i, &x)| n | u32::from(x) << (16 - 8 * i));
            (0..4).map(move |k| if k <= c.len() { A[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' })
        })
        .collect()
}

/// An image Claude would refuse (a side over 2000 px, or past what one chat can carry) never
/// enters the conversation: one would fail every later request until a new chat.
#[test]
fn oversized_and_surplus_images_are_left_out() {
    assert_eq!(png_size(&png_header(1600, 8192)), Some((1600, 8192)));
    assert_eq!(png_size("iVBORw0KGgo="), None);
    let mut conv = Conversation::new();
    let image = |w, h| Output { ok: true, text: "rendered".into(), image: Some(png_header(w, h)) };
    let mut tall = image(1600, 8192);
    admit_image(&mut conv, &mut tall);
    assert_eq!(tall.image, None);
    assert!(tall.text.contains("1600 × 8192 px") && tall.text.contains("2000 or less"), "{}", tall.text);
    for _ in 0..MAX_CONVERSATION_IMAGES {
        let mut ok = image(1600, 1000);
        admit_image(&mut conv, &mut ok);
        assert!(ok.image.is_some());
    }
    let mut more = image(1600, 1000);
    admit_image(&mut conv, &mut more);
    assert!(more.image.is_none() && more.text.contains("new chat"), "{}", more.text);
    let mut huge = Output { ok: true, text: String::new(), image: Some("A".repeat(MAX_IMAGE_BYTES + 1)) };
    admit_image(&mut Conversation::new(), &mut huge);
    assert!(huge.image.is_none());
}

/// A call written as `tool {args}` mid-reply: the transcript shows the reply without it, in
/// English and in Korean (where a wrong byte offset would cut a character).
#[test]
fn text_around_a_written_call_streams_whole() {
    let chunks = |c: &[&str]| -> String {
        c.iter().map(|t| format!("data: {}\n\n", json!({"choices": [{"delta": {"content": t}}]}))).chain(["data: [DONE]\n\n".to_string()]).collect()
    };
    for (parts, expected) in [
        (["Drawing.\nexecute ", "{\"command\": \"circle\"}", "\nDone, the circle is drawn."], "Drawing.\n\nDone, the circle is drawn."),
        (["원을 그립니다. execute ", "{\"command\": \"circle\"}", " 원점에 원이 그려졌습니다."], "원을 그립니다.  원점에 원이 그려졌습니다."),
    ] {
        let (url, _, server) = mock(vec![(200, SSE, chunks(&parts)), (200, SSE, chunks(&["."]))]);
        let cfg = Config { provider: ProviderKind::Custom, base_url: url, model: "qwen".into(), ..Config::default() };
        let mut b = Fake::default();
        let (r, events) = go(&cfg, &mut Conversation::new(), &mut b, &no_stop());
        server.join().unwrap();
        assert_eq!(r, Ok(()));
        assert_eq!(b.calls.len(), 1);
        let first: String = events
            .iter()
            .take_while(|e| !matches!(e, Event::ToolCall { .. }))
            .filter_map(|e| if let Event::Text(t) = e { Some(t.as_str()) } else { None })
            .collect();
        assert_eq!(first, expected);
    }
}

/// A server that takes the request and says nothing (a local model reading a long prompt): Stop
/// ends the wait at once, and without Stop it isn't sent again.
#[test]
fn stop_ends_a_wait_for_a_silent_server() {
    let silent = || {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", l.local_addr().unwrap());
        let t = std::thread::spawn(move || {
            let mut held = Vec::new();
            l.set_nonblocking(true).unwrap();
            let until = std::time::Instant::now() + std::time::Duration::from_secs(8);
            while std::time::Instant::now() < until {
                if let Ok((s, _)) = l.accept() {
                    held.push(s);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            held.len()
        });
        (url, t)
    };
    let (url, _server) = silent();
    let cfg = Config { provider: ProviderKind::Custom, base_url: url, model: "qwen".into(), ..Config::default() };
    let stop = no_stop();
    let flag = Arc::clone(&stop);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        flag.store(true, Ordering::Relaxed);
    });
    let started = std::time::Instant::now();
    let mut conv = Conversation::new();
    let (r, events) = go(&cfg, &mut conv, &mut Fake::default(), &stop);
    assert!(started.elapsed() < std::time::Duration::from_millis(1500), "{:?}", started.elapsed());
    assert_eq!((r, events), (Ok(()), vec![Event::Notice(text::STOPPED.into())]));
    assert!(conv.is_empty());

    let (url, server) = silent();
    let cfg = Config { base_url: url, ..cfg };
    let (r, events) = go(&cfg, &mut Conversation::new(), &mut Fake::default(), &no_stop());
    assert!(r.as_ref().is_err_and(|e| e.contains("No answer")), "{r:?}");
    assert!(events.is_empty(), "not sent again: {events:?}");
    assert_eq!(server.join().unwrap(), 1, "one connection");
}

/// A redirect is not followed: the key would go to the other host.
#[test]
fn redirects_are_not_followed() {
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    let elsewhere = format!("http://{}/v1/messages", other.local_addr().unwrap());
    other.set_nonblocking(true).unwrap();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let (mut line, mut len) = (String::new(), 0);
        while r.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
            len = line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse().ok()).unwrap_or(len);
            line.clear();
        }
        // The whole request read, so closing doesn't reset the connection before the reply.
        r.read_exact(&mut vec![0; len]).unwrap();
        write!(s, "HTTP/1.1 307 Moved\r\nlocation: {elsewhere}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").unwrap();
    });
    let (r, _) = go(&anthropic(&url), &mut Conversation::new(), &mut Fake::default(), &no_stop());
    server.join().unwrap();
    assert!(r.as_ref().is_err_and(|e| e.contains("307")), "{r:?}");
    assert!(other.accept().is_err(), "nothing reached the other host");
}

/// A reply of `content` then structured `calls` ((name, arguments)), streamed.
fn openai_reply(content: &[&str], calls: &[(&str, &str)]) -> String {
    let mut chunks: Vec<Value> = content.iter().map(|t| json!({"choices": [{"delta": {"content": t}}]})).collect();
    let calls: Vec<Value> =
        calls.iter().enumerate().map(|(i, (name, args))| json!({"index": i, "function": {"name": name, "arguments": args}})).collect();
    let finish = if calls.is_empty() { "stop" } else { "tool_calls" };
    chunks.push(json!({"choices": [{"delta": {"tool_calls": calls}, "finish_reason": finish}]}));
    chunks.iter().map(|v| format!("data: {v}\n\n")).chain(["data: [DONE]\n\n".to_string()]).collect()
}

/// A local model made the box again in its next round (qwen2.5:7b): the second one isn't run and
/// the model is told why; looking again runs, and after an undo so does the box. A cloud model's
/// repeat runs.
#[test]
fn a_local_model_repeating_a_change_is_told_instead() {
    let boxed = "{\"command\":\"box\",\"params\":{\"corner1\":[0,0,0],\"corner2\":[600,400,750]}}";
    let again = "{\"params\":{\"corner2\":[600,400,750],\"corner1\":[0,0,0]},\"command\":\"box\"}";
    let render = "{\"view\":\"iso\"}";
    let replies = vec![
        (200, SSE, openai_reply(&[], &[("execute", boxed), ("render", render)])),
        (200, SSE, openai_reply(&["Rendering.\nis", "phere\n"], &[("execute", again), ("render", render), ("execute", "{\"command\":\"zoom\"}")])),
        (200, SSE, openai_reply(&[], &[("execute", "{\"command\":\"U\"}"), ("execute", boxed)])),
        (200, SSE, openai_reply(&["Done."], &[])),
    ];
    let (url, seen, server) = mock(replies);
    let cfg = Config { provider: ProviderKind::Custom, base_url: url, model: "qwen".into(), ..Config::default() };
    let (mut conv, mut b) = (Conversation::new(), Fake::default());
    let (r, events) = go(&cfg, &mut conv, &mut b, &no_stop());
    server.join().unwrap();
    assert_eq!(r, Ok(()));
    let commands: Vec<String> = b.calls.iter().map(|(m, p)| p["command"].as_str().map_or(m.clone(), |c| format!("{m} {c}"))).collect();
    assert_eq!(commands, ["engine.execute box", "ui.render", "ui.render", "engine.execute zoom", "engine.execute U", "engine.execute box"]);
    let told: Vec<&String> =
        events.iter().filter_map(|e| if let Event::ToolResult { ok: false, text, .. } = e { Some(text) } else { None }).collect();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].starts_with("Not run: this same call already ran") && told[0].contains("\"prompt\": \"Command:\""), "{}", told[0]);
    // The word piece before the calls never shows and isn't remembered.
    let shown: String = events.iter().filter_map(|e| if let Event::Text(t) = e { Some(t.as_str()) } else { None }).collect();
    assert_eq!(shown, "Rendering.\nDone.");
    assert_eq!(seen.lock().unwrap()[2].1["messages"][6]["content"], "Rendering.", "after the first round's results and image");

    // Claude repeating the same call means it.
    let call = |id: &str| reply(&[tool_use(id, "execute", &[boxed])], "tool_use");
    let (url, _, server) = mock(vec![(200, SSE, call("t1")), (200, SSE, call("t2")), (200, SSE, reply(&[text("Two boxes.")], "end_turn"))]);
    let mut b = Fake::default();
    let (r, _) = go(&anthropic(&url), &mut Conversation::new(), &mut b, &no_stop());
    server.join().unwrap();
    assert_eq!((r, b.calls.len()), (Ok(()), 2));
}

/// What counts as a repeat: a line that starts and ends a command, or a JSON command, that
/// changed the drawing and succeeded, until an undo or another drawing. A prompt's answers, a
/// refused or failed call, other arguments, looking and a new message may come again.
#[test]
fn repeats_are_changes_only() {
    let guard = Guard::new(&mut Fake::default(), true);
    let call = |name: &str, input: Value| Call { id: "1".into(), name: name.into(), input: Ok(input) };
    let typed = |text: &str| call("command_line", json!({ "text": text }));
    let state = |running: Option<&str>| Output { ok: true, text: json!({"prompt": "x", "running": running}).to_string(), image: None };
    let done = || Output { ok: true, text: "{\"handle\": \"2A\"}".into(), image: None };
    let mut r = Repeats::default();
    let circle = typed("circle 0,0 5");
    r.note(&guard, &circle, &state(None));
    assert!(r.earlier(&circle).is_some_and(|o| !o.ok && o.text.contains("\"running\":null")), "made twice");
    assert!(r.earlier(&typed("circle 0,0 6")).is_none() && r.earlier(&call("execute", json!({"text": "circle 0,0 5"}))).is_none());
    // While a command runs, typed lines answer its prompts.
    for (line, running) in [("line 0,0", Some("line")), ("@100,0", Some("line")), ("@100,0", Some("line")), ("", None)] {
        assert!(r.earlier(&typed(line)).is_none(), "{line}");
        r.note(&guard, &typed(line), &state(running));
    }
    r.note(&guard, &typed("rectang 0,0"), &state(Some("rectang")));
    assert!(r.earlier(&circle).is_none(), "an answer to RECTANG's prompt");
    r.note(&guard, &call("cancel", json!({})), &state(None));
    assert!(r.earlier(&circle).is_some());
    // A refused line, a failed call and looking are not changes; a cut result leaves the prompt unknown.
    let refused = Output { text: json!({"running": null, "error": "Unknown command"}).to_string(), ..state(None) };
    r.note(&guard, &typed("box 0,0,0 600,400,750"), &refused);
    let boxed = call("execute", json!({"command": "box", "params": {"corner2": [1, 1, 1]}}));
    r.note(&guard, &boxed, &Output::error("no"));
    for c in [call("execute", json!({"command": "zoom", "params": {"mode": "extents"}})), call("execute", json!({"command": "Z"})), typed("zoom e")] {
        r.note(&guard, &c, &done());
        assert!(r.earlier(&c).is_none(), "{c:?}");
    }
    assert!(r.earlier(&typed("box 0,0,0 600,400,750")).is_none() && r.earlier(&boxed).is_none());
    r.note(&guard, &boxed, &done());
    r.note(&guard, &typed("circle 9,9 1"), &Output { text: "{\"running\": null, \"out".into(), ..done() });
    assert!(r.earlier(&boxed).is_some() && r.earlier(&circle).is_none(), "a JSON call still counts while the prompt is unknown");
    // Undo (by any name) or another drawing: what ran before may run again.
    r.note(&guard, &call("cancel", json!({})), &state(None));
    for c in [call("execute", json!({"command": "_U"})), typed("undo 2"), call("new_drawing", json!({}))] {
        r.note(&guard, &boxed, &done());
        r.note(&guard, &c, &state(None));
        assert!(r.earlier(&boxed).is_none(), "{c:?}");
    }
    // UNDO at its prompt: the line that starts it, then the answer.
    r.note(&guard, &boxed, &done());
    r.note(&guard, &typed("undo"), &state(Some("undo")));
    r.note(&guard, &typed(""), &state(None));
    assert!(r.earlier(&boxed).is_none(), "the box was undone");
    // A setting set back runs: the latest call of a command replaces the earlier.
    let layer = |name: &str| call("execute", json!({"command": "layer.current", "params": {"name": name}}));
    for name in ["Walls", "Doors", "Walls"] {
        assert!(r.earlier(&layer(name)).is_none(), "{name}");
        r.note(&guard, &layer(name), &done());
    }
    assert!(r.earlier(&layer("Walls")).is_some() && r.earlier(&boxed).is_none());
}
