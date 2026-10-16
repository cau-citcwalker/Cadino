//! OpenAI's Chat Completions API, also spoken by local servers (Ollama, LM Studio, llama.cpp,
//! vLLM): the request, the streamed reply with its tool calls, and the tool results.

use std::io::{BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::http::{self, Failure};
use crate::{Call, Config, Conversation, Event, Next, Output, ProviderKind, sse, text};

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
    let mut messages = vec![json!({"role": "system", "content": conv.system})];
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
    let custom = cfg.provider == ProviderKind::Custom;
    // A local model without vision refuses the tools' images: drop them once and go on.
    let mut vision_retry = custom && !conv.no_images && conv.messages.iter().any(has_image);
    let reply = loop {
        let body = body(cfg, model, conv, tools).to_string();
        let r = http::retrying(cancel, on_event, |on_event| {
            let r = http::check(http::send(Some(&body), &url, &headers)?)?;
            if r.json { whole(r.body, custom, on_event) } else { read(r.body, custom, cancel, on_event) }
        });
        match r {
            Err(Failure::Fatal { status: 400, msg })
                if vision_retry && ["image", "vision", "multimodal"].iter().any(|w| msg.to_lowercase().contains(w)) =>
            {
                vision_retry = false;
                conv.no_images = true;
                on_event(Event::Notice(text::NO_IMAGES.into()));
            }
            r => break r,
        }
    };
    let mut m = match reply {
        Ok(m) => m,
        Err(Failure::Cancelled) => return Ok(Next::Cancelled),
        Err(f) => return Err(cfg.failure(f)),
    };
    // A local model whose chat template lost its tool format writes the calls as text.
    let shown = m.text.get(..m.shown).unwrap_or_default().to_string();
    if custom && m.calls.is_empty() {
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        (m.text, m.calls) = text_calls(&m.text, &names);
    }
    // The rest of the text as it is now: what was shown stays a prefix of it, since the calls
    // are taken from text held back; were it not, what follows their common part.
    let common: usize = shown.chars().zip(m.text.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a.len_utf8()).sum();
    if let Some(rest) = m.text.get(common..).filter(|r| !r.is_empty()) {
        on_event(Event::Text(rest.to_string()));
    }
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
            on_event(Event::Notice(text::LENGTH_LIMIT.into()));
            if calls.is_empty() { Next::Done } else { Next::Refuse(calls, "Not run: the reply was cut off before this call was complete.".into()) }
        }
        "content_filter" => {
            on_event(Event::Error(text::CONTENT_FILTER.into()));
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
    /// Hold back text that may be tool calls written as text ([`text_calls`]).
    hold: bool,
    /// How much of `text` went out as [`Event::Text`].
    shown: usize,
}

/// Read `data: {json}` chunks until `data: [DONE]`. A failure before any content is a retry.
fn read(body: impl Read, hold: bool, cancel: &AtomicBool, on_event: &mut dyn FnMut(Event)) -> Result<Message, Failure> {
    let mut events = sse::Reader::new(BufReader::new(body));
    let mut m = Message { hold, ..Message::default() };
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Failure::Cancelled);
        }
        let started = m.started;
        let lost = |msg: String| if started { Failure::fatal(msg) } else { Failure::Retry { msg, after: None } };
        let ev = match events.next() {
            Ok(Some(ev)) => ev,
            // Stop pressed while the transport waited for the next bytes.
            Err(_) if cancel.load(Ordering::Relaxed) => return Err(Failure::Cancelled),
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
fn whole(body: impl Read, hold: bool, on_event: &mut dyn FnMut(Event)) -> Result<Message, Failure> {
    let v = http::read_json(body)?;
    let mut m = Message { hold, ..Message::default() };
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
        let safe = if m.hold { shown_len(&m.text) } else { m.text.len() };
        if let Some(more) = m.text.get(m.shown..safe).filter(|s| !s.is_empty()) {
            on_event(Event::Text(more.to_string()));
            m.shown = safe;
        }
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

/// Where tool calls written as text may start: Hermes/Qwen's tag, a code fence, a JSON object.
const CALL_MARKS: [&str; 3] = ["<tool_call>", "```", "{"];

/// How much of streamed `text` can be shown before it is known whether it holds tool calls: the
/// words before the first of [`CALL_MARKS`], or before a part of one at the end, but not the
/// last of them, which may be a tool's name (`execute {…}`, or glued to a stray word piece).
fn shown_len(text: &str) -> usize {
    // The marks are ASCII, so a matching tail starts on a character boundary.
    let partial = || CALL_MARKS.iter().flat_map(|m| (1..m.len()).filter(|&k| text.ends_with(&m[..k]))).max().unwrap_or(0);
    let end = CALL_MARKS.iter().filter_map(|m| text.find(m)).min().unwrap_or_else(|| text.len() - partial());
    let head = text.get(..end).unwrap_or_default().trim_end();
    head.char_indices().rfind(|(_, c)| c.is_whitespace()).map_or(0, |(i, c)| i + c.len_utf8())
}

/// A call written as text: `{"name": tool in known, "arguments" | "parameters": {...}}`.
fn text_call(v: &Value, known: &[&str]) -> Option<ToolCall> {
    let name = v["name"].as_str().filter(|n| known.contains(n))?;
    let args = match v.get("arguments").or_else(|| v.get("parameters"))? {
        Value::Null => String::new(),
        Value::String(a) => a.clone(),
        a => a.to_string(),
    };
    Some(ToolCall { name: name.to_string(), args, ..ToolCall::default() })
}

/// Tool calls a local model wrote as text because its server lost the tool format (a chat
/// template cut off with the start of an overflowing prompt, a garbled first token): JSON call
/// objects, bare, fenced or in Hermes/Qwen `<tool_call>` tags, and `tool {arguments}` as the
/// prompts write calls. Returns the text without them (and their tags or fences), and the
/// calls; an object naming an unknown tool stays text.
fn text_calls(text: &str, known: &[&str]) -> (String, Vec<ToolCall>) {
    const OPEN: [&str; 3] = ["<tool_call>", "```json", "```"];
    const CLOSE: [&str; 2] = ["</tool_call>", "```"];
    // ponytail: a reply this long isn't scanned (a JSON parse per `{`); written calls are short.
    if text.len() > 64 << 10 {
        return (text.to_string(), Vec::new());
    }
    let (mut shown, mut calls, mut done, mut from) = (String::new(), Vec::new(), 0, 0);
    while let Some(start) = text.get(from..).and_then(|r| r.find('{')).map(|i| from + i) {
        let mut values = serde_json::Deserializer::from_str(text.get(start..).unwrap_or_default()).into_iter::<Value>();
        let v = values.next().and_then(Result::ok).filter(|_| calls.len() < MAX_CALLS).unwrap_or_default();
        let before = text.get(done..start).unwrap_or_default();
        let head = before.trim_end();
        // A tool's name right before the object: `execute {"command": …}`. It may be glued to a
        // stray word piece: Ollama 0.18 sometimes decodes Qwen's `<tool_call>` token as one
        // (`ronicsexecute {…}`). ponytail: any word ending in a tool's name counts.
        let named = known.iter().find_map(|n| head.strip_suffix(n));
        let (c, kept) = match (text_call(&v, known), named) {
            (Some(c), _) => (c, OPEN.iter().find_map(|m| head.strip_suffix(m)).unwrap_or(before)),
            (None, Some(b)) if v.is_object() => {
                let name = head.get(b.len()..).unwrap_or_default().to_string();
                (ToolCall { name, args: v.to_string(), ..ToolCall::default() }, b)
            }
            _ => {
                from = start + 1;
                continue;
            }
        };
        shown.push_str(kept);
        let after = text.get(start + values.byte_offset()..).unwrap_or_default();
        let rest = CLOSE.iter().find_map(|m| after.trim_start().strip_prefix(m)).unwrap_or(after);
        calls.push(c);
        done = text.len() - rest.len();
        from = done;
    }
    shown.push_str(text.get(done..).unwrap_or_default());
    if calls.is_empty() { (text.to_string(), calls) } else { (shown.trim_end().to_string(), calls) }
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
    fn text_calls_take_known_tools_only() {
        let known = ["execute", "render"];
        let calls = |text: &str| {
            let (shown, calls) = text_calls(text, &known);
            (shown, calls.into_iter().map(|c| (c.name, c.args)).collect::<Vec<_>>())
        };
        let fenced = "Rendering.\n```json\n{\"name\": \"render\", \"parameters\": {\"view\": \"iso\"}}\n```\n";
        assert_eq!(calls(fenced), ("Rendering.".into(), vec![("render".into(), "{\"view\":\"iso\"}".into())]));
        assert_eq!(calls(" {\"name\": \"execute\", \"arguments\": \"{}\"}"), (String::new(), vec![("execute".into(), "{}".into())]));
        let two = "<tool_call>{\"name\": \"render\", \"arguments\": {}}</tool_call>\n<tool_call>{\"name\": \"execute\", \"arguments\": {\"command\": \"zoom\"}}";
        assert_eq!(calls(two), (String::new(), vec![("render".into(), "{}".into()), ("execute".into(), "{\"command\":\"zoom\"}".into())]));
        // Ollama sometimes garbles the first token, here the opening tag.
        let garbled = "leton\n{\"name\": \"execute\", \"arguments\": {\"command\": \"circle\"}}\n</tool_call>";
        assert_eq!(calls(garbled), ("leton".into(), vec![("execute".into(), "{\"command\":\"circle\"}".into())]));
        // As the prompts write calls (and the server's chat template doesn't).
        let named = "Drawing it.\nexecute {\"command\": \"circle\", \"params\": {\"radius\": 5}}";
        assert_eq!(calls(named), ("Drawing it.".into(), vec![("execute".into(), "{\"command\":\"circle\",\"params\":{\"radius\":5}}".into())]));
        assert_eq!(calls("ronicsrender {}").1, vec![("render".to_string(), "{}".to_string())]);
        assert_eq!(calls("render it: {\"a\": 1}").1, vec![], "the name must be right before the object");
        // Unknown tools, calls without arguments and other code stay text.
        let text = "Use <tool_call>{\"name\": \"draw_circle\", \"arguments\": {}}</tool_call> or {\"name\": \"render\"},\n```\nline 0,0 1,1\n```";
        assert_eq!(calls(text), (text.to_string(), vec![]));
    }

    #[test]
    fn text_that_may_be_a_call_is_held_back() {
        assert_eq!(shown_len("I am drawing <tool_call>{"), "I am ".len());
        assert_eq!(shown_len("I am drawing <tool"), "I am ".len());
        assert_eq!(shown_len("Here it is: ``"), "Here it ".len());
        assert_eq!(shown_len("  {\"name\""), 0);
        // The word before a call may be its tool's name; the last word may become one.
        assert_eq!(shown_len("Drawing.\nexecute {\"command\""), "Drawing.\n".len());
        assert_eq!(shown_len("Drawing.\nexecu"), "Drawing.\n".len());
        assert_eq!(shown_len("Plain <b>text</b> here"), "Plain <b>text</b> ".len());
        assert_eq!(shown_len("원을 그립니다. 원점"), "원을 그립니다. ".len());
        let mut m = Message { hold: true, ..Message::default() };
        let mut events = Vec::new();
        for t in ["Hi there <tool", "_call>{}", "</tool_call>"] {
            apply(&mut m, &json!({"choices": [{"delta": {"content": t}}]}), &mut |e| events.push(e));
        }
        assert_eq!((events, m.shown), (vec![Event::Text("Hi ".into())], 3));
    }

    #[test]
    fn a_whole_completion_is_read_too() {
        let v = json!({"choices": [{"message": {"content": "Hi", "tool_calls": [{"id": "x", "function": {"name": "cancel", "arguments": {}}}]}, "finish_reason": "tool_calls"}]});
        let m = whole(v.to_string().as_bytes(), false, &mut |_| {}).unwrap();
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
