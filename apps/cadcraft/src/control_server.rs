//! Loopback JSON-lines control server: one request per line, one reply per line.
//! This is the transport the MCP server (`cadcraft-cli mcp --connect`) wraps.
//!
//! `--control` starts one for the whole run; Options ▸ AI turns another on and off ([`Switch`]).
//!
//! Lines are bounded ([`MAX_LINE`]): a longer one is refused and the connection closed, without
//! buffering the rest. So is the number of connections served at once ([`MAX_CONNECTIONS`]).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use cadcraft_ui_egui::{CadApp, ControlRequest};
use serde_json::{Value, json};

/// Longest accepted request line (bytes, without the newline). Generous, so requests that carry
/// whole drawings or images as base64 still fit.
const MAX_LINE: usize = 256 * 1024 * 1024;
/// Connections served at once; further ones get an error line and are closed.
const MAX_CONNECTIONS: usize = 16;

/// A running control server. Dropping the handle leaves it running; [`ControlServer::stop`] ends it.
pub struct ControlServer {
    port: u16,
    stop: Arc<AtomicBool>,
    accept: JoinHandle<()>,
}

/// Listen on `127.0.0.1:port`; the app answers the requests that arrive on the receiver.
pub fn start(port: u16, ctx: egui::Context) -> std::io::Result<(ControlServer, Receiver<ControlRequest>)> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let (tx, rx) = channel::<ControlRequest>();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let accept = std::thread::Builder::new().name("control-accept".into()).spawn(move || accept_loop(listener, tx, ctx, &flag))?;
    log::info!("control server listening on 127.0.0.1:{port}");
    Ok((ControlServer { port, stop, accept }, rx))
}

impl ControlServer {
    /// Stop accepting and free the port. Open connections are answered with an error from their
    /// next request on, once the app has dropped the receiver.
    pub fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        // `accept` blocks: wake it with a connection of our own, then wait for the listener to go.
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        if TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok() {
            let _ = self.accept.join();
        }
        log::info!("control server on 127.0.0.1:{} stopped", self.port);
    }
}

/// The control server Options ▸ AI asks for ([`CadApp::wanted_control_port`]), started, moved and
/// stopped to match it. A port that can't be opened is tried again only when the setting changes.
#[derive(Default)]
pub struct Switch {
    wanted: Option<u16>,
    server: Option<ControlServer>,
}

impl Switch {
    pub fn reconcile(&mut self, cad: &mut CadApp, ctx: &egui::Context) {
        let wanted = cad.wanted_control_port();
        if wanted == self.wanted {
            return;
        }
        self.wanted = wanted;
        if let Some(server) = self.server.take() {
            server.stop();
            cad.detach_control();
        }
        cad.ai.mcp_status = match wanted {
            None => cadcraft_ui_egui::tl!("Off").to_string(),
            Some(port) => match start(port, ctx.clone()) {
                Ok((server, rx)) => {
                    cad.attach_control(rx);
                    self.server = Some(server);
                    cadcraft_ui_egui::tf!("Listening on 127.0.0.1:{port}", port = port)
                }
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => cadcraft_ui_egui::tf!("Port {port} is in use", port = port),
                Err(e) => {
                    log::warn!("control server on 127.0.0.1:{port}: {e}");
                    cadcraft_ui_egui::tf!("Port {port} could not be opened: {error}", port = port, error = e)
                }
            },
        };
    }
}

/// A connection's place among [`MAX_CONNECTIONS`], given back when the connection ends (also when
/// its thread can't be started, or panics).
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn accept_loop(listener: TcpListener, tx: Sender<ControlRequest>, ctx: egui::Context, stop: &AtomicBool) {
    let open = Arc::new(AtomicUsize::new(0));
    for mut stream in listener.incoming().flatten() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            let _ = writeln!(stream, "{}", json!({"ok": false, "error": "too many control connections"}));
            continue;
        }
        let slot = Slot(Arc::clone(&open));
        let tx = tx.clone();
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new().spawn(move || {
            let _slot = slot;
            serve(stream, tx, ctx);
        });
    }
}

/// Read one `\n`-terminated line of at most `max` bytes into `buf` (newline stripped).
/// `Ok(false)` at end of stream; `Err` when the line is too long or the read fails.
fn read_bounded_line(reader: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> Result<bool, String> {
    buf.clear();
    // `take` bounds the bytes buffered for one line: a peer can't make us allocate more than `max`.
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let n = reader.take(limit).read_until(b'\n', buf).map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(false);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        return Ok(true);
    }
    if buf.len() > max {
        return Err(format!("request line longer than {max} bytes; closing the connection"));
    }
    // Last line without a newline, then end of stream.
    Ok(true)
}

fn serve(stream: TcpStream, tx: Sender<ControlRequest>, ctx: egui::Context) {
    let Ok(read) = stream.try_clone() else { return };
    let mut out = stream;
    let mut reader = BufReader::new(read);
    let mut buf = Vec::new();
    loop {
        match read_bounded_line(&mut reader, &mut buf, MAX_LINE) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": e}));
                break;
            }
        }
        // Bytes that aren't UTF-8 end the connection, as `BufRead::lines` did.
        let Ok(line) = std::str::from_utf8(&buf) else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(line) {
            Ok(msg) => {
                let id = msg.get("id").cloned().unwrap_or(Value::Null);
                let method = msg.get("method").and_then(Value::as_str).unwrap_or("").to_string();
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                let (req, pending) = ControlRequest::new(method, params);
                if tx.send(req).is_err() {
                    let _ = writeln!(out, "{}", json!({"id": id, "ok": false, "error": "the control server was turned off"}));
                    break;
                }
                ctx.request_repaint();
                // On timeout the request is cancelled if the app hasn't started it, and the reply's
                // `state` says whether it can still have run.
                let mut r = pending.wait(Duration::from_secs(60));
                if let Some(o) = r.as_object_mut() {
                    o.insert("id".into(), id);
                }
                r
            }
            Err(e) => json!({"ok": false, "error": format!("bad JSON: {e}")}),
        };
        if writeln!(out, "{reply}").is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_read_up_to_the_limit() {
        let mut buf = Vec::new();
        let mut input: &[u8] = b"aaaaaaaaaa\nbb\r\ncc";
        assert_eq!(read_bounded_line(&mut input, &mut buf, 10), Ok(true));
        assert_eq!(buf, b"aaaaaaaaaa", "a line exactly at the limit is accepted");
        assert_eq!(read_bounded_line(&mut input, &mut buf, 10), Ok(true));
        assert_eq!(buf, b"bb", "CRLF is stripped");
        assert_eq!(read_bounded_line(&mut input, &mut buf, 10), Ok(true));
        assert_eq!(buf, b"cc", "a last line without a newline is still read");
        assert_eq!(read_bounded_line(&mut input, &mut buf, 10), Ok(false), "then end of stream");
    }

    #[test]
    fn an_overlong_line_is_refused_without_reading_the_rest() {
        let mut buf = Vec::new();
        let long = [b'x'; 100];
        let mut input: &[u8] = &long;
        assert!(read_bounded_line(&mut input, &mut buf, 10).is_err());
        assert!(buf.len() <= 11, "at most limit + 1 bytes are buffered, got {}", buf.len());
        assert_eq!(input.len(), 100 - 11, "the rest of the line is left unread");
    }

    /// The control server on a loopback port, with a stand-in UI that echoes each method.
    fn serve_echo() -> std::net::SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = channel::<ControlRequest>();
        std::thread::spawn(move || accept_loop(listener, tx, egui::Context::default(), &AtomicBool::new(false)));
        std::thread::spawn(move || {
            for req in rx {
                let _ = req.reply.send(json!({"ok": true, "method": req.method}));
            }
        });
        addr
    }

    #[test]
    fn requests_are_answered_with_their_ids() {
        let addr = serve_echo();
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(b"{\"id\":3,\"method\":\"ping\"}\r\n\n{\"id\":4,\"method\":\"pong\"}\n").unwrap();
        let mut r = BufReader::new(c);
        for (id, method) in [(3, "ping"), (4, "pong")] {
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let v: Value = serde_json::from_str(&line).unwrap();
            assert_eq!((v["id"].as_i64(), v["method"].as_str()), (Some(id), Some(method)), "{line}");
        }
    }

    fn free_port() -> u16 {
        TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
    }

    #[test]
    fn a_stopped_server_frees_its_port_and_turns_away_its_connections() {
        let port = free_port();
        let (server, rx) = start(port, egui::Context::default()).unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut replies = BufReader::new(c.try_clone().unwrap());
        let mut line = String::new();
        c.write_all(b"{\"id\":6,\"method\":\"ping\"}\n").unwrap();
        let req = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(req.begin());
        req.reply.send(json!({"ok": true})).unwrap();
        replies.read_line(&mut line).unwrap();
        assert!(line.contains("\"ok\":true"), "{line}");
        server.stop();
        drop(rx);
        // The connection made before the stop gets an error line instead of reaching the app.
        c.write_all(b"{\"id\":7,\"method\":\"ping\"}\n").unwrap();
        line.clear();
        replies.read_line(&mut line).unwrap();
        assert!(line.contains("turned off") && line.contains("\"id\":7"), "{line}");
        // The port can be opened again at once.
        let (again, _rx) = start(port, egui::Context::default()).unwrap();
        again.stop();
    }

    #[test]
    fn the_switch_follows_the_setting_and_does_not_retry_a_busy_port() {
        let ctx = egui::Context::default();
        let mut cad = CadApp::new(cadcraft_engine::Session::empty(), cadcraft_ui_egui::Services::default());
        let mut switch = Switch::default();
        let port = free_port();
        cad.ai.mcp_port = port;
        switch.reconcile(&mut cad, &ctx);
        assert!(cad.ai.mcp_status.is_empty() && switch.server.is_none(), "off stays off");

        cad.ai.mcp_enabled = true;
        switch.reconcile(&mut cad, &ctx);
        assert_eq!(cad.ai.mcp_status, format!("Listening on 127.0.0.1:{port}"));
        assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());

        // Moving to a busy port stops the old server and reports the new port as in use, once.
        let busy = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let busy_port = busy.local_addr().unwrap().port();
        cad.ai.mcp_port = busy_port;
        switch.reconcile(&mut cad, &ctx);
        assert_eq!(cad.ai.mcp_status, format!("Port {busy_port} is in use"));
        assert!(TcpListener::bind(("127.0.0.1", port)).is_ok(), "the old port is free again");
        drop(busy);
        switch.reconcile(&mut cad, &ctx);
        assert!(switch.server.is_none(), "not retried every frame");

        // Turning it off and on again retries.
        cad.ai.mcp_enabled = false;
        switch.reconcile(&mut cad, &ctx);
        assert_eq!(cad.ai.mcp_status, "Off");
        cad.ai.mcp_enabled = true;
        switch.reconcile(&mut cad, &ctx);
        assert_eq!(cad.ai.mcp_status, format!("Listening on 127.0.0.1:{busy_port}"));
        cad.ai.mcp_enabled = false;
        switch.reconcile(&mut cad, &ctx);
        assert!(TcpListener::bind(("127.0.0.1", busy_port)).is_ok());
    }

    #[test]
    fn extra_connections_are_refused() {
        let addr = serve_echo();
        // Idle connections are accepted in order and held open.
        let held: Vec<TcpStream> = (0..MAX_CONNECTIONS).map(|_| TcpStream::connect(addr).unwrap()).collect();
        let mut extra = TcpStream::connect(addr).unwrap();
        extra.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut line = String::new();
        BufReader::new(&mut extra).read_line(&mut line).unwrap();
        assert!(line.contains("too many control connections"), "{line}");
        drop(held);
    }
}
