//! Server-sent events, as OpenAI-compatible servers stream a chat reply
//! (`"stream": true`): lines of `data: {json}`, an empty line after each
//! event, and `data: [DONE]` at the end.
//!
//! Small enough to write here rather than take a dependency (GUARDRAILS §9).
//! Bytes are split into lines before they are decoded, so a character split
//! across two network chunks is put back together.

/// Turns chunks of a response body into the data of each complete event.
#[derive(Debug, Default)]
pub(crate) struct Decoder {
    /// The start of a line whose end hasn't arrived yet.
    line: Vec<u8>,
    /// The data lines of the event being read.
    data: Vec<String>,
    /// The event being read has at least one data line.
    has_data: bool,
}

/// The stream wasn't text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NotText;

impl Decoder {
    /// Takes the next chunk of the body, and returns the data of every event
    /// it completes, in order.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, NotText> {
        let mut events = Vec::new();
        let mut rest = chunk;
        while let Some(end) = rest.iter().position(|&b| b == b'\n') {
            self.line.extend_from_slice(&rest[..end]);
            rest = &rest[end + 1..];
            let line = std::mem::take(&mut self.line);
            if let Some(event) = self.line_done(&line)? {
                events.push(event);
            }
        }
        self.line.extend_from_slice(rest);
        Ok(events)
    }

    /// The body has ended: the data of an event that wasn't followed by an
    /// empty line, if any.
    pub(crate) fn finish(mut self) -> Result<Option<String>, NotText> {
        let line = std::mem::take(&mut self.line);
        if !line.is_empty()
            && let Some(event) = self.line_done(&line)?
        {
            return Ok(Some(event));
        }
        Ok(self.dispatch())
    }

    fn line_done(&mut self, line: &[u8]) -> Result<Option<String>, NotText> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return Ok(self.dispatch());
        }
        let line = std::str::from_utf8(line).map_err(|_| NotText)?;
        // Comments (": ping") and other fields (event, id, retry) carry
        // nothing Lax uses.
        if let Some(value) = line.strip_prefix("data") {
            let value = match value.strip_prefix(':') {
                Some(value) => value.strip_prefix(' ').unwrap_or(value),
                // "data" alone is an empty data line.
                None if value.is_empty() => "",
                None => return Ok(None),
            };
            self.data.push(value.to_string());
            self.has_data = true;
        }
        Ok(None)
    }

    fn dispatch(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.has_data) {
            return None;
        }
        Some(std::mem::take(&mut self.data).join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(chunks: &[&[u8]]) -> Vec<String> {
        let mut decoder = Decoder::default();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(decoder.push(chunk).unwrap());
        }
        events.extend(decoder.finish().unwrap());
        events
    }

    #[test]
    fn events_are_read_from_data_lines() {
        assert_eq!(
            all(&[b"data: {\"a\":1}\n\ndata: {\"a\":2}\n\ndata: [DONE]\n\n"]),
            ["{\"a\":1}", "{\"a\":2}", "[DONE]"]
        );
    }

    #[test]
    fn lines_and_characters_split_across_chunks_are_put_together() {
        let body = "data: {\"t\":\"Hej Åsa\"}\n\ndata: [DONE]\n\n".as_bytes();
        // Every split point, also inside "Å", which takes two bytes.
        for split in 0..body.len() {
            assert_eq!(
                all(&[&body[..split], &body[split..]]),
                ["{\"t\":\"Hej Åsa\"}", "[DONE]"],
                "split at {split}"
            );
        }
        // One byte at a time.
        let bytes: Vec<&[u8]> = body.chunks(1).collect();
        assert_eq!(all(&bytes), ["{\"t\":\"Hej Åsa\"}", "[DONE]"]);
    }

    #[test]
    fn crlf_comments_and_other_fields_are_handled() {
        assert_eq!(
            all(&[b": ping\r\nevent: message\r\nid: 7\r\ndata:{\"a\":1}\r\n\r\nretry: 10\n\n"]),
            ["{\"a\":1}"]
        );
    }

    #[test]
    fn several_data_lines_make_one_event() {
        assert_eq!(all(&[b"data: one\ndata: two\n\n"]), ["one\ntwo"]);
        assert_eq!(all(&[b"data\n\n"]), [""]);
        // A field that only starts like "data" isn't data.
        assert!(all(&[b"database: x\n\n"]).is_empty());
    }

    #[test]
    fn a_last_event_without_an_empty_line_still_counts() {
        assert_eq!(all(&[b"data: {\"a\":1}"]), ["{\"a\":1}"]);
        assert_eq!(all(&[b"data: {\"a\":1}\n"]), ["{\"a\":1}"]);
        assert!(all(&[b""]).is_empty());
    }

    #[test]
    fn bytes_that_are_not_text_are_refused() {
        let mut decoder = Decoder::default();
        assert_eq!(decoder.push(b"data: \xff\xfe\n\n"), Err(NotText));
    }
}
