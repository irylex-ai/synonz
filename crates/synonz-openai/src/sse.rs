//! Incremental SSE (server-sent events) parsing for OpenAI-compatible
//! streaming responses.
//!
//! Deliberately minimal: OpenAI-compatible streams are `data:`-line based.
//! Comment lines (`:`-prefixed) and other SSE fields are ignored; the
//! literal payload `[DONE]` is passed through for the caller to interpret.
//!
//! The parser is byte-safe across chunk boundaries: incomplete lines stay
//! buffered until a full line (terminated by `\n`) arrives.

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
            // Other SSE fields (event/id/retry/comments) are not used by
            // OpenAI-compatible streams and are ignored.
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
    fn single_chunk_multiple_events() {
        let events = payloads(&[&b"data: {\"a\":1}\ndata: {\"b\":2}\n\n"[..]]);
        assert_eq!(
            events,
            vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]
        );
    }

    #[test]
    fn event_split_across_chunks() {
        let events = payloads(&[b"data: {\"frag", b"ment\": tru", b"e}\n\n"]);
        assert_eq!(events, vec!["{\"fragment\": true}".to_string()]);
    }

    #[test]
    fn done_sentinel_passes_through() {
        let events = payloads(&[&b"data: {\"x\":1}\ndata: [DONE]\n"[..]]);
        assert_eq!(events, vec!["{\"x\":1}".to_string(), "[DONE]".to_string()]);
    }

    #[test]
    fn comments_and_non_data_fields_are_ignored() {
        let events = payloads(&[&b": keepalive\nevent: foo\nid: 1\ndata: {\"ok\":true}\n\n"[..]]);
        assert_eq!(events, vec!["{\"ok\":true}".to_string()]);
    }

    #[test]
    fn no_space_after_colon_is_handled() {
        let events = payloads(&[&b"data:{\"no\":\"space\"}\n\n"[..]]);
        assert_eq!(events, vec!["{\"no\":\"space\"}".to_string()]);
    }
}
