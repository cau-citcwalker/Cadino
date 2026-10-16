//! Anthropic's Messages API: the request, the streamed reply assembled into content blocks, and
//! the tool results.

use std::io::{BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::http::{self, Failure};
use crate::{Call, Config, Conversation, Event, Next, Output, ProviderKind, sse, text};

const VERSION: &str = "2023-06-01";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// Models with adaptive thinking and `output_config.effort`.
fn adaptive(model: &str) -> bool {
    const PREFIXES: [&str; 9] = [
        "claude-opus-5",
        "claude-opus-4-6",
        "claude-opus-4-7",
        "claude-opus-4-8",
        "claude-sonnet-5",
        "claude-sonnet-4-6",
        "claude-haiku-5",
        "claude-fable-5",
        "claude-mythos-5",
    ];
    PREFIXES.iter().any(|p| model.starts_with(p))
}

/// Whether a refusal should be retried on another model server-side (`fallbacks: "default"`):
/// models that have it, on Anthropic's own API.
fn falls_back(cfg: &Config, model: &str) -> bool {
    matches!(model, "claude-opus-5-5" | "claude-opus-5" | "claude-fable-5-1" | "claude-sonnet-5-5")
        && cfg.base() == ProviderKind::Anthropic.default_base_url()
}

pub(crate) fn headers(key: &str, fallback: bool) -> Vec<(&'static str, String)> {
    let mut h = vec![("x-api-key", key.to_string()), ("anthropic-version", VERSION.to_string())];
    if fallback {
        h.push(("anthropic-beta", FALLBACK_BETA.to_string()));
    }
    h
}

fn body(cfg: &Config, model: &str, conv: &Conversation, tools: &[Value], fallback: bool) -> Value {
    let tools: Vec<Value> = tools
        .iter()
        .map(|t| json!({"name": t["name"], "description": t["description"], "input_schema": t["inputSchema"], "eager_input_streaming": true}))
        .collect();
    let thinking = adaptive(model);
    let mut b = json!({
        "model": model,
        "max_tokens": cfg.max_tokens,
        "stream": true,
        "system": [{"type": "text", "text": conv.system}],
        "tools": tools,
        // Automatic caching of the stable prefix (tools, system, earlier turns).
        "cache_control": {"type": "ephemeral"},
        "messages": conv.messages,
        "thinking": thinking.then(|| json!({"type": "adaptive", "display": "summarized"})),
        "output_config": thinking.then(|| json!({"effort": cfg.effort})),
        "fallbacks": fallback.then_some("default"),
    });
    if let Value::Object(o) = &mut b {
        o.retain(|_, v| !v.is_null());
    }
    b
}

/// One request: stream the reply, append it to `conv`, and say what comes next.
pub(crate) fn turn(
    cfg: &Config,
    model: &str,
    conv: &mut Conversation,
    tools: &[Value],
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(Event),
) -> Result<Next, String> {
    let fallback = falls_back(cfg, model);
    let url = format!("{}/v1/messages", cfg.base());
    let body = body(cfg, model, conv, tools, fallback).to_string();
    let headers = headers(cfg.api_key.trim(), fallback);
    let reply = http::retrying(cancel, on_event, |on_event| read(http::check(http::send(Some(&body), &url, &headers)?)?.body, cancel, on_event));
    let m = match reply {
        Ok(m) => m,
        Err(Failure::Cancelled) => return Ok(Next::Cancelled),
        Err(f) => return Err(f.message()),
    };
    on_event(Event::TurnDone { stop_reason: m.stop_reason.clone(), usage: m.usage.clone() });
    if m.stop_reason == "refusal" {
        // Not appended: the declined turn is not part of the conversation.
        let d = &m.stop_details;
        let why: Vec<&str> = [d["category"].as_str(), d["explanation"].as_str()].into_iter().flatten().collect();
        on_event(Event::Error(if why.is_empty() { text::DECLINED.into() } else { text::DECLINED_WHY.replace("{reason}", &why.join(": ")) }));
        return Ok(Next::Done);
    }
    let (content, calls) = finish(m.blocks);
    if !content.is_empty() {
        conv.messages.push(json!({"role": "assistant", "content": content}));
    }
    Ok(match m.stop_reason.as_str() {
        "tool_use" if !calls.is_empty() => Next::Tools(calls),
        "pause_turn" if calls.is_empty() => Next::Again,
        "max_tokens" => {
            on_event(Event::Notice(text::MAX_TOKENS.replace("{max_tokens}", &cfg.max_tokens.to_string())));
            if calls.is_empty() {
                Next::Done
            } else {
                Next::Refuse(calls, "Not run: the reply was cut off at max_tokens before this call was complete.".into())
            }
        }
        stop if !calls.is_empty() => Next::Refuse(calls, format!("Not run: the reply ended with `{stop}`.")),
        _ => Next::Done,
    })
}

/// A content block as it streams in: its index, the block, and its tool input JSON so far.
#[derive(Debug)]
struct Block {
    index: u64,
    v: Value,
    json: String,
}

/// A streamed message.
#[derive(Debug, Default)]
struct Message {
    blocks: Vec<Block>,
    stop_reason: String,
    stop_details: Value,
    usage: Value,
}

/// Read a streamed message until `message_stop`. A failure before the first content block is a
/// retry; after it, the partial message is lost and the failure is fatal.
fn read(body: impl Read, cancel: &AtomicBool, on_event: &mut dyn FnMut(Event)) -> Result<Message, Failure> {
    let mut events = sse::Reader::new(BufReader::new(body));
    let mut m = Message::default();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Failure::Cancelled);
        }
        let started = !m.blocks.is_empty();
        let lost = |msg: String| if started { Failure::fatal(msg) } else { Failure::Retry { msg, after: None } };
        let ev = match events.next() {
            Ok(Some(ev)) => ev,
            // Stop pressed while the transport waited for the next bytes.
            Err(_) if cancel.load(Ordering::Relaxed) => return Err(Failure::Cancelled),
            Ok(None) => return Err(lost("The connection closed before the reply was complete".into())),
            Err(e) => return Err(lost(format!("Reading the reply: {e}"))),
        };
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
            log::warn!("anthropic: skipped an event that is not JSON ({} bytes)", ev.data.len());
            continue;
        };
        match v["type"].as_str().unwrap_or_default() {
            "message_start" => m.usage = v["message"]["usage"].clone(),
            "content_block_start" => {
                let index = v["index"].as_u64().unwrap_or(m.blocks.len() as u64);
                m.blocks.push(Block { index, v: v["content_block"].clone(), json: String::new() });
            }
            "content_block_delta" => {
                if let Some(b) = m.blocks.iter_mut().rev().find(|b| Some(b.index) == v["index"].as_u64()) {
                    apply(b, &v["delta"], on_event);
                }
            }
            "message_delta" => {
                m.stop_reason = v["delta"]["stop_reason"].as_str().unwrap_or_default().to_string();
                m.stop_details = v["delta"]["stop_details"].clone();
                if let (Some(u), Some(d)) = (m.usage.as_object_mut(), v["usage"].as_object()) {
                    u.extend(d.iter().filter(|(_, x)| !x.is_null()).map(|(k, x)| (k.clone(), x.clone())));
                } else if v["usage"].is_object() {
                    m.usage = v["usage"].clone();
                }
            }
            "message_stop" => return Ok(m),
            "error" => {
                let e = &v["error"];
                return Err(lost(format!("{}: {}", e["type"].as_str().unwrap_or("error"), e["message"].as_str().unwrap_or_default())));
            }
            // ping, content_block_stop, and events added later.
            _ => {}
        }
    }
}

/// Apply a `content_block_delta` to its block.
fn apply(b: &mut Block, d: &Value, on_event: &mut dyn FnMut(Event)) {
    let s = |k: &str| d[k].as_str().unwrap_or_default().to_string();
    let Some(o) = b.v.as_object_mut() else { return };
    let mut append = |key: &str, more: &str| {
        if let Value::String(t) = o.entry(key).or_insert_with(|| json!("")) {
            t.push_str(more);
        }
    };
    match d["type"].as_str().unwrap_or_default() {
        "text_delta" => {
            let t = s("text");
            append("text", &t);
            on_event(Event::Text(t));
        }
        "thinking_delta" => {
            let t = s("thinking");
            append("thinking", &t);
            on_event(Event::Thinking(t));
        }
        "signature_delta" => {
            o.insert("signature".into(), json!(s("signature")));
        }
        "input_json_delta" => b.json.push_str(&s("partial_json")),
        "citations_delta" => {
            if let Value::Array(c) = o.entry("citations").or_insert_with(|| json!([])) {
                c.push(d["citation"].clone());
            }
        }
        _ => {}
    }
}

/// The finished content to append (tool inputs parsed, a fallback's declined partial pruned) and
/// its tool calls. A tool input that is not a JSON object is kept as `{}` in the content and comes
/// back as the call's `Err` raw text.
fn finish(blocks: Vec<Block>) -> (Vec<Value>, Vec<Call>) {
    let mut bad = Vec::new();
    let content = blocks
        .into_iter()
        .map(|mut b| {
            if b.v["type"] == "tool_use"
                && !b.json.is_empty()
                && let Some(o) = b.v.as_object_mut()
            {
                let input = match serde_json::from_str::<Value>(&b.json) {
                    Ok(v @ Value::Object(_)) => v,
                    _ => {
                        bad.push((o.get("id").and_then(Value::as_str).unwrap_or_default().to_string(), b.json));
                        json!({})
                    }
                };
                o.insert("input".into(), input);
            }
            b.v
        })
        .collect();
    let content = prune_fallback(content);
    let calls = content
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| {
            let id = b["id"].as_str().unwrap_or_default().to_string();
            let input = match bad.iter().find(|(i, _)| *i == id) {
                Some((_, raw)) => Err(raw.clone()),
                None => Ok(b["input"].clone()),
            };
            Call { id, name: b["name"].as_str().unwrap_or_default().to_string(), input }
        })
        .collect();
    (content, calls)
}

/// After a server-side fallback the content starts with the declined model's partial: before the
/// last `fallback` block only its text is echoed back, and the `fallback` markers are dropped.
fn prune_fallback(content: Vec<Value>) -> Vec<Value> {
    let Some(last) = content.iter().rposition(|b| b["type"] == "fallback") else { return content };
    content.into_iter().enumerate().filter(|(i, b)| b["type"] != "fallback" && (*i > last || b["type"] == "text")).map(|(_, b)| b).collect()
}

/// One user message with a `tool_result` per call.
pub(crate) fn results(calls: &[Call], outs: Vec<Output>) -> Value {
    let blocks: Vec<Value> = calls
        .iter()
        .zip(outs)
        .map(|(c, o)| {
            let mut content = Vec::new();
            if !o.text.is_empty() || o.image.is_none() {
                content.push(json!({"type": "text", "text": if o.text.is_empty() { "(no output)" } else { o.text.as_str() }}));
            }
            if let Some(data) = o.image {
                content.push(json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": data}}));
            }
            json!({"type": "tool_result", "tool_use_id": c.id, "content": content, "is_error": !o.ok})
        })
        .collect();
    json!({"role": "user", "content": blocks})
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A stream of `events` as the API sends them.
    pub(crate) fn sse(events: &[Value]) -> String {
        events.iter().map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap_or_default())).collect()
    }

    /// A reply streaming `blocks` (each a start block and its deltas), then `stop`.
    pub(crate) fn reply(blocks: &[(Value, Vec<Value>)], stop: &str) -> String {
        let mut ev = vec![json!({"type": "message_start", "message": {"usage": {"input_tokens": 10, "cache_read_input_tokens": 0}}})];
        for (i, (start, deltas)) in blocks.iter().enumerate() {
            ev.push(json!({"type": "content_block_start", "index": i, "content_block": start}));
            ev.extend(deltas.iter().map(|d| json!({"type": "content_block_delta", "index": i, "delta": d})));
            ev.push(json!({"type": "content_block_stop", "index": i}));
        }
        ev.push(json!({"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 7}}));
        ev.push(json!({"type": "message_stop"}));
        sse(&ev)
    }

    pub(crate) fn tool_use(id: &str, name: &str, parts: &[&str]) -> (Value, Vec<Value>) {
        (
            json!({"type": "tool_use", "id": id, "name": name, "input": {}}),
            parts.iter().map(|p| json!({"type": "input_json_delta", "partial_json": p})).collect(),
        )
    }

    fn assemble(stream: &str) -> (Message, Vec<Event>) {
        let mut events = Vec::new();
        let m = read(stream.as_bytes(), &AtomicBool::new(false), &mut |e| events.push(e)).unwrap();
        (m, events)
    }

    #[test]
    fn blocks_assemble_with_thinking_signature_and_tool_input() {
        let stream = reply(
            &[
                (
                    json!({"type": "thinking", "thinking": "", "signature": ""}),
                    vec![
                        json!({"type": "thinking_delta", "thinking": "Plan: "}),
                        json!({"type": "thinking_delta", "thinking": "a circle."}),
                        json!({"type": "signature_delta", "signature": "c2ln"}),
                    ],
                ),
                (json!({"type": "text", "text": ""}), vec![json!({"type": "text_delta", "text": "Drawing."})]),
                tool_use("toolu_1", "command_line", &["{\"text\": \"circ", "le 0,0 5\"}"]),
                (json!({"type": "server_thing", "x": 1}), vec![]),
            ],
            "tool_use",
        );
        let (m, events) = assemble(&stream);
        assert_eq!(m.stop_reason, "tool_use");
        assert_eq!(m.usage, json!({"input_tokens": 10, "cache_read_input_tokens": 0, "output_tokens": 7}));
        assert_eq!(events, vec![Event::Thinking("Plan: ".into()), Event::Thinking("a circle.".into()), Event::Text("Drawing.".into())]);
        let (content, calls) = finish(m.blocks);
        assert_eq!(content[0], json!({"type": "thinking", "thinking": "Plan: a circle.", "signature": "c2ln"}));
        assert_eq!(content[1], json!({"type": "text", "text": "Drawing."}));
        assert_eq!(content[2]["input"], json!({"text": "circle 0,0 5"}));
        assert_eq!(content[3], json!({"type": "server_thing", "x": 1}), "unknown blocks are kept verbatim");
        assert_eq!(calls, vec![Call { id: "toolu_1".into(), name: "command_line".into(), input: Ok(json!({"text": "circle 0,0 5"})) }]);
    }

    #[test]
    fn invalid_tool_json_is_kept_raw_and_the_block_gets_an_empty_input() {
        let stream =
            reply(&[tool_use("a", "command_line", &["{\"text\": \"x"]), tool_use("b", "cancel", &[]), tool_use("c", "render", &["[1]"])], "tool_use");
        let (content, calls) = finish(assemble(&stream).0.blocks);
        assert_eq!(calls[0].input, Err("{\"text\": \"x".into()));
        assert_eq!(calls[1].input, Ok(json!({})), "no input deltas = no arguments");
        assert_eq!(calls[2].input, Err("[1]".into()), "an input must be an object");
        assert!(content.iter().all(|b| b["input"] == json!({})));
    }

    #[test]
    fn stream_errors_retry_before_content_and_fail_after() {
        let overloaded = json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}});
        let before = sse(&[json!({"type": "message_start", "message": {}}), overloaded.clone()]);
        let r = read(before.as_bytes(), &AtomicBool::new(false), &mut |_| {});
        assert!(matches!(r, Err(Failure::Retry { ref msg, .. }) if msg.contains("overloaded_error")), "{r:?}");
        let after = sse(&[json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}), overloaded]);
        assert!(matches!(read(after.as_bytes(), &AtomicBool::new(false), &mut |_| {}), Err(Failure::Fatal { .. })));
        let cut = sse(&[json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})]);
        assert!(matches!(read(cut.as_bytes(), &AtomicBool::new(false), &mut |_| {}), Err(Failure::Fatal { .. })), "closed mid-reply");
    }

    #[test]
    fn a_fallback_prunes_the_declined_partial() {
        let content = vec![
            json!({"type": "thinking", "thinking": "a", "signature": "s"}),
            json!({"type": "text", "text": "partial"}),
            json!({"type": "tool_use", "id": "x", "name": "cancel", "input": {}}),
            json!({"type": "fallback", "from": {"model": "claude-opus-5-5"}, "to": {"model": "claude-opus-5"}}),
            json!({"type": "redacted_thinking", "data": "r"}),
            json!({"type": "fallback", "from": {"model": "claude-opus-5"}, "to": {"model": "claude-opus-4-8"}}),
            json!({"type": "thinking", "thinking": "b", "signature": "t"}),
            json!({"type": "text", "text": "final"}),
            json!({"type": "tool_use", "id": "y", "name": "cancel", "input": {}}),
        ];
        let kept: Vec<Value> = prune_fallback(content.clone());
        assert_eq!(kept, vec![content[1].clone(), content[6].clone(), content[7].clone(), content[8].clone()]);
        let blocks = content.into_iter().enumerate().map(|(i, v)| Block { index: i as u64, v, json: String::new() }).collect();
        let (_, calls) = finish(blocks);
        assert_eq!(calls.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["y"], "the declined model's tool call never runs");
    }

    #[test]
    fn request_shape_follows_the_model() {
        let cfg = Config::default();
        let conv = Conversation::default();
        let tools = vec![json!({"name": "cancel", "description": "Esc", "inputSchema": {"type": "object"}})];
        let b = body(&cfg, "claude-opus-5-5", &conv, &tools, true);
        assert_eq!(b["thinking"], json!({"type": "adaptive", "display": "summarized"}));
        assert_eq!(b["output_config"], json!({"effort": "high"}));
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["tools"][0], json!({"name": "cancel", "description": "Esc", "input_schema": {"type": "object"}, "eager_input_streaming": true}));
        assert!(falls_back(&cfg, "claude-sonnet-5-5") && !falls_back(&cfg, "claude-haiku-5-5"));
        let old = body(&cfg, "claude-haiku-4-5", &conv, &tools, false);
        for k in ["thinking", "output_config", "fallbacks", "temperature", "top_p"] {
            assert!(old.get(k).is_none(), "{k}: {old}");
        }
        let proxy = Config { base_url: "https://proxy.example/".into(), ..Config::default() };
        assert!(!falls_back(&proxy, "claude-opus-5-5"), "fallbacks only on Anthropic's own API");
    }
}
