//! Incremental SSE (server-sent events) parsing for the Anthropic Messages
//! API streaming responses.
//!
//! Deliberately minimal: only `data:` payloads are captured; `event:` and
//! other fields are ignored (the event type arrives inside the JSON
//! payload). Byte-safe across chunk boundaries: incomplete lines stay
//! buffered until a full line arrives.
//!
//! This parser is intentionally duplicated from `synonz-openai`: a
//! ~60-line, well-understood parser does not justify a shared crate yet
//! (ADR-0001: abstractions only for demonstrated repeated patterns). If a
//! third adapter appears, promote it to a shared location.

/// Incremental parser turning raw byte chunks into SSE `data:` payloads.
#[derive(Default)]
pub(crate) struct SseParser {
    buffer: Vec<u8>,
}

impl SseParser {
    pub(crate) fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Feeds raw bytes and returns every completed `data:` payload in order.
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(position) = self.buffer.iter().position(|&byte| byte == b'\n') {
            let line_bytes: Vec<u8> = self.buffer.drain(..=position).collect();
            let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]);
            let line = line.trim_end_matches('\r');
            if let Some(payload) = line.strip_prefix("data:") {
                events.push(payload.trim_start().to_string());
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payloads(chunks: &[&[u8]]) -> Vec<String> {
        let mut parser = SseParser::new();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(parser.feed(chunk));
        }
        events
    }

    #[test]
    fn captures_only_data_lines() {
        let events =
            payloads(&[&b"event: message_start\ndata: {\"type\":\"message_start\"}\n\n"[..]]);
        assert_eq!(events, vec!["{\"type\":\"message_start\"}".to_string()]);
    }

    #[test]
    fn event_split_across_chunks() {
        let events = payloads(&[b"data: {\"frag", b"ment\": 1}\n\n"]);
        assert_eq!(events, vec!["{\"fragment\": 1}".to_string()]);
    }
}
