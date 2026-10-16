//! Server-sent events (the `text/event-stream` format both providers stream in).

use std::io::{BufRead, Read};

/// The most one event may hold (a hostile server could otherwise send one endless line).
pub(crate) const MAX_EVENT: usize = 16 << 20;

/// One event: its `event:` name (empty when none) and its `data:` lines joined by newlines.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Event {
    pub event: String,
    pub data: String,
}

/// Reads events from a stream: `\n` or `\r\n` lines, `field: value` pairs, `:` comments, and a
/// blank line ending each event.
pub(crate) struct Reader<R> {
    r: R,
    line: Vec<u8>,
}

impl<R: BufRead> Reader<R> {
    pub(crate) fn new(r: R) -> Self {
        Self { r, line: Vec::new() }
    }

    /// The next event with data; `None` at the end of the stream. A last event without its blank
    /// line still counts.
    pub(crate) fn next(&mut self) -> std::io::Result<Option<Event>> {
        let mut ev = Event::default();
        let mut has_data = false;
        loop {
            self.line.clear();
            let n = (&mut self.r).take(MAX_EVENT as u64 + 1).read_until(b'\n', &mut self.line)?;
            if n == 0 {
                return Ok(has_data.then_some(ev));
            }
            if self.line.len() + ev.data.len() > MAX_EVENT {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "an event of more than 16 MiB"));
            }
            let line = self.line.strip_suffix(b"\n").unwrap_or(&self.line);
            let line = String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line));
            if line.is_empty() {
                if has_data {
                    return Ok(Some(ev));
                }
                ev.event.clear();
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((&line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => ev.event = value.to_string(),
                "data" => {
                    if has_data {
                        ev.data.push('\n');
                    }
                    ev.data.push_str(value);
                    has_data = true;
                }
                // Comments (`:` lines, empty field), `id`, `retry`, unknown fields.
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(s: &[u8]) -> Vec<Event> {
        let mut r = Reader::new(s);
        std::iter::from_fn(|| r.next().unwrap()).collect()
    }

    fn ev(event: &str, data: &str) -> Event {
        Event { event: event.into(), data: data.into() }
    }

    #[test]
    fn lines_fields_comments_and_dispatch() {
        let s = b": comment\r\nevent: ping\r\ndata: {}\r\n\r\nevent: delta\ndata:a\ndata:  b\nid: 7\nretry: 5\n\n\nevent: unused\n\ndata\ndata: last";
        assert_eq!(all(s), [ev("ping", "{}"), ev("delta", "a\n b"), ev("", "\nlast")]);
        // An event name without data is not dispatched and does not leak into the next event.
        assert_eq!(all(b"event: x\n\ndata: y\n\n"), [ev("", "y")]);
        assert_eq!(all(b""), []);
    }

    #[test]
    fn an_endless_event_is_an_error() {
        let mut big = b"data: ".to_vec();
        big.resize(MAX_EVENT + 10, b'x');
        assert!(Reader::new(big.as_slice()).next().is_err());
        let lines = format!("data: {}\n", "x".repeat(99)).repeat(MAX_EVENT / 100 + 2);
        assert!(Reader::new(lines.as_bytes()).next().is_err(), "many lines of one event count together");
    }
}
