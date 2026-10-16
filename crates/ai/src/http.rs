//! HTTP for the providers: one request, the retry rules, and the reply as a reader.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::Event;

pub(crate) const DESKTOP_ONLY: &str = "The AI assistant needs the desktop app";
/// Whole (non-streamed) replies are read up to this size.
const MAX_BODY: u64 = 16 << 20;
/// Sends after the first, while nothing of the reply was streamed.
const RETRIES: u32 = 3;
/// The first backoff; it doubles (2, 4, 8 s).
const BACKOFF_MS: u64 = if cfg!(test) { 5 } else { 2000 };

/// Why a request failed.
#[derive(Debug, PartialEq)]
pub(crate) enum Failure {
    /// Worth sending again (overload, rate limit, a lost connection); `after` = the server's
    /// `retry-after` in seconds.
    Retry {
        msg: String,
        after: Option<u64>,
    },
    /// Not worth sending again; `status` = the HTTP status, 0 when there was none.
    Fatal {
        status: u16,
        msg: String,
    },
    Cancelled,
}

impl Failure {
    pub(crate) fn fatal(msg: impl Into<String>) -> Self {
        Failure::Fatal { status: 0, msg: msg.into() }
    }
    pub(crate) fn message(self) -> String {
        match self {
            Failure::Retry { msg, .. } | Failure::Fatal { msg, .. } => msg,
            Failure::Cancelled => "Stopped".into(),
        }
    }
}

/// A reply: its status, whether it is a whole JSON document (not a stream), and the body.
pub(crate) struct Reply {
    pub status: u16,
    pub json: bool,
    retry_after: Option<u64>,
    pub body: Box<dyn Read + Send>,
}

/// Run `attempt` until it succeeds, retrying [`Failure::Retry`] up to three times after 2, 4 and
/// 8 s (or the server's `retry-after`, at most a minute). `attempt` gets `on_event` to stream with.
pub(crate) fn retrying<T>(
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(Event),
    mut attempt: impl FnMut(&mut dyn FnMut(Event)) -> Result<T, Failure>,
) -> Result<T, Failure> {
    let mut n = 0;
    loop {
        match attempt(on_event) {
            Err(Failure::Retry { msg, after }) if n < RETRIES => {
                let ms = after.map_or(BACKOFF_MS << n, |s| s.min(60) * 1000);
                n += 1;
                on_event(Event::Notice(format!("{msg}. Trying again in {} s ({n}/{RETRIES})", ms.div_ceil(1000))));
                let until = Instant::now() + Duration::from_millis(ms);
                while let Some(left) = until.checked_duration_since(Instant::now()) {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(Failure::Cancelled);
                    }
                    std::thread::sleep(left.min(Duration::from_millis(100)));
                }
            }
            r => return r,
        }
    }
}

/// The reply when it succeeded; otherwise the API's error message, a retry for timeouts,
/// conflicts, rate limits and server errors.
pub(crate) fn check(r: Reply) -> Result<Reply, Failure> {
    if (200..300).contains(&r.status) {
        return Ok(r);
    }
    let mut text = String::new();
    // Best effort: the status alone still explains the failure.
    let _ = r.body.take(64 << 10).read_to_string(&mut text);
    let v: Value = serde_json::from_str(&text).unwrap_or_default();
    let api = v["error"]["message"].as_str().or(v["error"].as_str()).or(v["message"].as_str()).map(str::to_string);
    let msg = format!("HTTP {}: {}", r.status, api.unwrap_or_else(|| text.trim().chars().take(300).collect()));
    Err(match r.status {
        408 | 409 | 429 | 500 | 502 | 503 | 504 | 529 => Failure::Retry { msg, after: r.retry_after },
        401 => Failure::Fatal { status: 401, msg: format!("{}. Check the API key in Options ▸ AI.", msg.trim_end_matches('.')) },
        status => Failure::Fatal { status, msg },
    })
}

/// A whole JSON reply.
pub(crate) fn read_json(body: impl Read) -> Result<Value, Failure> {
    let mut s = String::new();
    body.take(MAX_BODY).read_to_string(&mut s).map_err(|e| Failure::Retry { msg: format!("Reading the reply: {e}"), after: None })?;
    serde_json::from_str(&s).map_err(|e| Failure::fatal(format!("Unexpected reply ({e}): {}", s.chars().take(200).collect::<String>())))
}

/// GET `url` and read its JSON (once, no retries).
pub(crate) fn get_json(url: &str, headers: &[(&'static str, String)]) -> Result<Value, Failure> {
    read_json(check(send(None, url, headers)?)?.body)
}

/// POST `body` (JSON) to `url`, or GET it when `body` is `None`. Any status comes back as a
/// [`Reply`]; a failure to connect or send is a retry.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn send(body: Option<&str>, url: &str, headers: &[(&'static str, String)]) -> Result<Reply, Failure> {
    native::send(body, url, headers)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn send(_body: Option<&str>, _url: &str, _headers: &[(&'static str, String)]) -> Result<Reply, Failure> {
    Err(Failure::fatal(DESKTOP_ONLY))
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::sync::LazyLock;
    use std::time::Duration;

    use ureq::unversioned::resolver::DefaultResolver;
    use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport, time};
    use ureq::{Agent, Timeout};

    use super::{Failure, Reply};

    const CONNECT: Duration = Duration::from_secs(15);
    /// The longest wait for the next bytes of a reply.
    const READ: Duration = Duration::from_secs(180);

    static AGENT: LazyLock<Agent> = LazyLock::new(|| {
        let config = Agent::config_builder().http_status_as_error(false).timeout_connect(Some(CONNECT)).build();
        Agent::with_parts(config, PerRead(DefaultConnector::new()), DefaultResolver::default())
    });

    /// ureq times a reply body as a whole; this caps each wait for bytes instead, so a stalled
    /// stream ends while a long, live one goes on.
    #[derive(Debug)]
    struct PerRead(DefaultConnector);

    impl Connector for PerRead {
        type Out = Capped;
        fn connect(&self, details: &ConnectionDetails, chained: Option<()>) -> Result<Option<Capped>, ureq::Error> {
            Ok(self.0.connect(details, chained)?.map(Capped))
        }
    }

    #[derive(Debug)]
    struct Capped(Box<dyn Transport>);

    impl Transport for Capped {
        fn buffers(&mut self) -> &mut dyn Buffers {
            self.0.buffers()
        }
        fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
            self.0.transmit_output(amount, timeout)
        }
        fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
            let cap: time::Duration = READ.into();
            self.0.await_input(if timeout.after > cap { NextTimeout { after: cap, reason: Timeout::RecvBody } } else { timeout })
        }
        fn is_open(&mut self) -> bool {
            self.0.is_open()
        }
        fn is_tls(&self) -> bool {
            self.0.is_tls()
        }
    }

    pub(super) fn send(body: Option<&str>, url: &str, headers: &[(&'static str, String)]) -> Result<Reply, Failure> {
        let sent = match body {
            Some(body) => {
                let mut req = AGENT.post(url);
                for (k, v) in headers {
                    req = req.header(*k, v.as_str());
                }
                req.content_type("application/json").send(body)
            }
            None => {
                let mut req = AGENT.get(url);
                for (k, v) in headers {
                    req = req.header(*k, v.as_str());
                }
                req.call()
            }
        };
        let resp = sent.map_err(|e| match e {
            ureq::Error::Io(_) | ureq::Error::Timeout(_) | ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => {
                Failure::Retry { msg: format!("Cannot reach {url} ({e})"), after: None }
            }
            e => Failure::fatal(format!("{url}: {e}")),
        })?;
        let header = |k: &str| resp.headers().get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
        let retry_after = header("retry-after").and_then(|s| s.trim().parse::<u64>().ok());
        let json = header("content-type").is_some_and(|c| c.contains("json"));
        let status = resp.status().as_u16();
        Ok(Reply { status, json, retry_after, body: Box::new(resp.into_body().into_reader()) })
    }
}
