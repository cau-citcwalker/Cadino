//! OpenAI's Chat Completions API, also spoken by local servers (Ollama, LM Studio, llama.cpp,
//! vLLM): the request, the streamed reply with its tool calls, and the tool results.

use std::io::{BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::http::{self, Failure};
use crate::{Call, Config, Conversation, Event, Next, Output, ProviderKind, sse};

/// Tool calls kept from one reply (a hostile server could send endless indexes).
const MAX_CALLS: usize = 128;

pub(crate) fn headers(key: &str) -> Vec<(&'static str, String)> {
    if key.is_empty() { Vec::new() } else { vec![("authorization", format!("Bearer {key}"))] }
}

/// Whether a message carries images (the one that follows tool results with images).
fn has_image(m: &Value) -> bool {
    m["content"].as_array().is_some_and(|parts| parts.iter().any(|p| p["type"] == "image_url"))
}

fn body(cfg: &Config, model: &str, conv: &Conversation, tools: &[Value]) -> Value {
    let mut messages = vec![json!({"role": "system", "content": cfg.system()})];
    messages.extend(conv.messages.iter().filter(|m| !(conv.no_images && has_image(m))).cloned());
    let tools: Vec<Value> = tools
        .iter()
        .map(|t| json!({"type": "function", "function": {"name": t["name"], "description": t["description"], "parameters": t["inputSchema"]}}))
        .collect();
    let usage = cfg.provider == ProviderKind::OpenAi && cfg.base() == ProviderKind::OpenAi.default_base_url();
    let mut b = json!({"model": model, "messages": messages, "tools": tools, "stream": true, "stream_options": usage.then(|| json!({"include_usage": true}))});
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
    let url = format!("{}/chat/completions", cfg.base());
    let headers = headers(cfg.api_key.trim());
    // A local model without vision refuses the tools' images: drop them once and go on.
    let mut vision_retry = cfg.provider == ProviderKind::Custom && !conv.no_images && conv.messages.iter().any(has_image);
    let reply = loop {
        let body = body(cfg, model, conv, tools).to_string();
        let r = http::retrying(cancel, on_event, |on_event| {
            let r = http::check(http::send(Some(&body), &url, &headers)?)?;
            if r.json { whole(r.body, on_event) } else { read(r.body, cancel, on_event) }
        });
        match r {
            Err(Failure::Fatal { status: 400, msg })
                if vision_retry && ["image", "vision", "multimodal"].iter().any(|w| msg.to_lowercase().contains(w)) =>
            {
                vision_retry = false;
                conv.no_images = true;
                on_event(Event::Notice("This model does not take images; the assistant goes on without them.".into()));
            }
            r => break r,
        }
    };
    let m = match reply {
        Ok(m) => m,
        Err(Failure::Cancelled) => return Ok(Next::Cancelled),
        Err(f) => return Err(f.message()),
    };
    let u = &m.usage;
    let usage = json!({"input_tokens": u["prompt_tokens"], "output_tokens": u["completion_tokens"], "cache_read_input_tokens": u["prompt_tokens_details"]["cached_tokens"], "cache_creation_input_tokens": null});
    on_event(Event::TurnDone { stop_reason: m.finish.clone(), usage });
    let first = conv.messages.len();
    let mut calls = Vec::with_capacity(m.calls.len());
    let mut tool_calls = Vec::with_capacity(m.calls.len());
    for (i, c) in m.calls.into_iter().enumerate() {
        let id = if c.id.is_empty() { format!("call_{}", first + i) } else { c.id };
        let input = match serde_json::from_str::<Value>(&c.args) {
            _ if c.args.trim().is_empty() => Ok(json!({})),
            Ok(v @ Value::Object(_)) => Ok(v),
            _ => Err(c.args.clone()),
        };
        // An unreadable call goes back as `{}`: some servers parse the history's arguments.
        let args = if input.is_ok() && !c.args.trim().is_empty() { c.args } else { "{}".into() };
        tool_calls.push(json!({"id": id, "type": "function", "function": {"name": c.name, "arguments": args}}));
        calls.push(Call { id, name: c.name, input });
    }
    let mut msg = json!({"role": "assistant", "content": if m.text.is_empty() && !calls.is_empty() { Value::Null } else { json!(m.text) }});
    if let (false, Some(o)) = (tool_calls.is_empty(), msg.as_object_mut()) {
        o.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    conv.messages.push(msg);
    Ok(match m.finish.as_str() {
        "length" => {
            on_event(Event::Notice("The reply reached the model's length limit.".into()));
            if calls.is_empty() { Next::Done } else { Next::Refuse(calls, "Not run: the reply was cut off before this call was complete.".into()) }
        }
        "content_filter" => {
            on_event(Event::Error("The provider's content filter stopped the reply.".into()));
            if calls.is_empty() { Next::Done } else { Next::Refuse(calls, "Not run: the content filter stopped the reply.".into()) }
        }
        _ if !calls.is_empty() => Next::Tools(calls),
        _ => Next::Done,
    })
}

/// A tool call as it streams in.
#[derive(Debug, Default)]
struct ToolCall {
    index: u64,
    id: String,
    name: String,
    args: String,
}

/// A streamed (or whole) completion.
#[derive(Debug, Default)]
struct Message {
    text: String,
    calls: Vec<ToolCall>,
    finish: String,
    usage: Value,
    started: bool,
}

/// Read `data: {json}` chunks until `data: [DONE]`. A failure before any content is a retry.
fn read(body: impl Read, cancel: &AtomicBool, on_event: &mut dyn FnMut(Event)) -> Result<Message, Failure> {
    let mut events = sse::Reader::new(BufReader::new(body));
    let mut m = Message::default();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Failure::Cancelled);
        }
        let started = m.started;
        let lost = |msg: String| if started { Failure::fatal(msg) } else { Failure::Retry { msg, after: None } };
        let ev = match events.next() {
            Ok(Some(ev)) => ev,
            // Some servers end without `[DONE]`.
            Ok(None) if !m.finish.is_empty() => return Ok(m),
            Ok(None) => return Err(lost("The connection closed before the reply was complete".into())),
            Err(e) => return Err(lost(format!("Reading the reply: {e}"))),
        };
        if ev.data.trim() == "[DONE]" {
            return Ok(m);
        }
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
            log::warn!("openai: skipped a chunk that is not JSON ({} bytes)", ev.data.len());
            continue;
        };
        if !v["error"].is_null() {
            let e = &v["error"];
            return Err(lost(e["message"].as_str().or(e.as_str()).unwrap_or("error").to_string()));
        }
        apply(&mut m, &v, on_event);
    }
}

/// A server that answered with one JSON completion instead of a stream.
fn whole(body: impl Read, on_event: &mut dyn FnMut(Event)) -> Result<Message, Failure> {
    let v = http::read_json(body)?;
    let mut m = Message::default();
    apply(&mut m, &v, on_event);
    Ok(m)
}

/// Apply one chunk (`choices[0].delta`) or a whole completion (`choices[0].message`).
fn apply(m: &mut Message, v: &Value, on_event: &mut dyn FnMut(Event)) {
    if v["usage"].is_object() {
        m.usage = v["usage"].clone();
    }
    let choice = &v["choices"][0];
    let d = if choice["delta"].is_object() { &choice["delta"] } else { &choice["message"] };
    if let Some(t) = d["content"].as_str().filter(|t| !t.is_empty()) {
        m.text.push_str(t);
        m.started = true;
        on_event(Event::Text(t.to_string()));
    }
    if let Some(t) = d["reasoning_content"].as_str().or(d["reasoning"].as_str()).filter(|t| !t.is_empty()) {
        m.started = true;
        on_event(Event::Thinking(t.to_string()));
    }
    for (i, tc) in d["tool_calls"].as_array().into_iter().flatten().enumerate() {
        let index = tc["index"].as_u64().unwrap_or(i as u64);
        let pos = match m.calls.iter().position(|c| c.index == index) {
            Some(p) => p,
            None if m.calls.len() < MAX_CALLS => {
                m.calls.push(ToolCall { index, ..ToolCall::default() });
                m.calls.len() - 1
            }
            None => continue,
        };
        let Some(c) = m.calls.get_mut(pos) else { continue };
        if let Some(id) = tc["id"].as_str().filter(|s| !s.is_empty()) {
            c.id = id.to_string();
        }
        let f = &tc["function"];
        if let Some(name) = f["name"].as_str().filter(|s| !s.is_empty()) {
            c.name = name.to_string();
        }
        match &f["arguments"] {
            Value::String(a) => c.args.push_str(a),
            // Some servers send the arguments as an object.
            a @ Value::Object(_) => c.args = a.to_string(),
            _ => {}
        }
        m.started = true;
    }
    if let Some(f) = choice["finish_reason"].as_str() {
        m.finish = f.to_string();
    }
}

/// A `tool` message per call, then one user message with the images the tools returned.
pub(crate) fn push_results(conv: &mut Conversation, calls: &[Call], outs: Vec<Output>) {
    let mut images = Vec::new();
    for (c, o) in calls.iter().zip(outs) {
        let mut text = if o.ok { o.text } else { format!("Error: {}", o.text) };
        if let Some(data) = o.image {
            if conv.no_images {
                text.push_str("\n(An image was made but is left out: this model does not take images.)");
            } else {
                text.push_str("\n(The image follows in the next message.)");
                images.push(json!({"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{data}")}}));
            }
        }
        let text = text.trim();
        conv.messages.push(json!({"role": "tool", "tool_call_id": c.id, "content": if text.is_empty() { "(no output)" } else { text }}));
    }
    if !images.is_empty() {
        let mut content = vec![json!({"type": "text", "text": "Images returned by the tool calls above:"})];
        content.extend(images);
        conv.messages.push(json!({"role": "user", "content": content}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_calls_accumulate_by_index() {
        let chunks = [
            json!({"choices": [{"delta": {"role": "assistant", "content": "Two "}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_a", "type": "function", "function": {"name": "command_line", "arguments": ""}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 1, "id": "call_b", "function": {"name": "render", "arguments": "{\"view\""}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"text\": \"circle"}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": " 0,0 5\"}"}}, {"index": 1, "function": {"arguments": ": \"iso\"}"}}]}}]}),
            json!({"choices": [{"delta": {"reasoning_content": "hmm"}, "finish_reason": "tool_calls"}]}),
            json!({"choices": [], "usage": {"prompt_tokens": 3, "completion_tokens": 4}}),
        ];
        let mut m = Message::default();
        let mut events = Vec::new();
        for c in &chunks {
            apply(&mut m, c, &mut |e| events.push(e));
        }
        assert_eq!((m.text.as_str(), m.finish.as_str()), ("Two ", "tool_calls"));
        let got: Vec<(&str, &str, &str)> = m.calls.iter().map(|c| (c.id.as_str(), c.name.as_str(), c.args.as_str())).collect();
        assert_eq!(got, [("call_a", "command_line", "{\"text\": \"circle 0,0 5\"}"), ("call_b", "render", "{\"view\": \"iso\"}")]);
        assert_eq!(events, [Event::Text("Two ".into()), Event::Thinking("hmm".into())]);
        assert_eq!(m.usage["completion_tokens"], 4);
    }

    #[test]
    fn a_whole_completion_is_read_too() {
        let v = json!({"choices": [{"message": {"content": "Hi", "tool_calls": [{"id": "x", "function": {"name": "cancel", "arguments": {}}}]}, "finish_reason": "tool_calls"}]});
        let m = whole(v.to_string().as_bytes(), &mut |_| {}).unwrap();
        assert_eq!((m.text.as_str(), m.calls.len(), m.calls[0].args.as_str()), ("Hi", 1, "{}"));
    }

    #[test]
    fn images_follow_the_tool_messages_and_are_left_out_after_a_refusal() {
        let calls = [
            Call { id: "a".into(), name: "render".into(), input: Ok(json!({})) },
            Call { id: "b".into(), name: "cancel".into(), input: Ok(json!({})) },
        ];
        let outs = || vec![Output { ok: true, text: String::new(), image: Some("UE5H".into()) }, Output::error("no")];
        let mut conv = Conversation::default();
        push_results(&mut conv, &calls, outs());
        assert_eq!(conv.messages.len(), 3);
        assert_eq!(conv.messages[1], json!({"role": "tool", "tool_call_id": "b", "content": "Error: no"}));
        assert_eq!(conv.messages[2]["content"][1]["image_url"]["url"], "data:image/png;base64,UE5H");
        conv.no_images = true;
        let cfg = Config { provider: ProviderKind::Custom, ..Config::default() };
        assert_eq!(body(&cfg, "m", &conv, &[])["messages"].as_array().map(Vec::len), Some(3), "system + two tool messages");
        assert!(body(&cfg, "m", &conv, &[]).get("stream_options").is_none());
    }
}
