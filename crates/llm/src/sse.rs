//! Incremental Server-Sent Events parser (the `text/event-stream` format of the HTML Living
//! Standard, §9.2): feed it arbitrary byte chunks, get complete events out.
//!
//! Lines end in `\n`, `\r\n` or `\r`; `:` lines are comments; `event:` and `data:` fields are
//! collected and an event is dispatched at a blank line. Hostile input is bounded: a line longer
//! than [`MAX_LINE`], an event with more than [`MAX_LINE`] bytes of data, or a stream longer than
//! [`MAX_TOTAL`] fails with [`LlmError::TooLarge`]. Invalid UTF-8 is replaced, never fatal here.

use crate::LlmError;

/// Longest accepted line (and event data), in bytes.
pub const MAX_LINE: usize = 8 << 20;
/// Longest accepted stream, in bytes.
pub const MAX_TOTAL: usize = 64 << 20;

/// One dispatched event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field (empty when absent).
    pub event: String,
    /// The `data:` lines joined with `\n`.
    pub data: String,
}

/// The incremental parser.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    /// Bytes of `buf` already scanned for a line end.
    scanned: usize,
    event: String,
    data: String,
    has_data: bool,
    total: usize,
    started: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk; returns the events it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, LlmError> {
        self.total = self.total.saturating_add(chunk.len());
        if self.total > MAX_TOTAL {
            return Err(LlmError::TooLarge);
        }
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut start = 0;
        let mut i = self.scanned;
        while let Some(&b) = self.buf.get(i) {
            if b == b'\n' || b == b'\r' {
                let mut next = i + 1;
                if b == b'\r' {
                    match self.buf.get(next) {
                        Some(b'\n') => next += 1,
                        Some(_) => {}
                        // A trailing `\r` may be the first half of `\r\n`: wait for more input.
                        None => break,
                    }
                }
                let line = self.buf.get(start..i).unwrap_or_default().to_vec();
                self.line(&line, &mut out)?;
                start = next;
                i = next;
            } else {
                i += 1;
            }
        }
        self.buf.drain(..start.min(self.buf.len()));
        self.scanned = i.saturating_sub(start);
        if self.buf.len() > MAX_LINE {
            return Err(LlmError::TooLarge);
        }
        Ok(out)
    }

    /// End of stream: process a final unterminated line and dispatch a pending event.
    pub fn finish(&mut self) -> Result<Vec<SseEvent>, LlmError> {
        let mut out = Vec::new();
        let mut rest = std::mem::take(&mut self.buf);
        if rest.last() == Some(&b'\r') {
            rest.pop();
        }
        self.scanned = 0;
        if !rest.is_empty() {
            self.line(&rest, &mut out)?;
        }
        self.dispatch(&mut out);
        Ok(out)
    }

    fn line(&mut self, mut line: &[u8], out: &mut Vec<SseEvent>) -> Result<(), LlmError> {
        if !self.started {
            self.started = true;
            line = line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line);
        }
        if line.is_empty() {
            self.dispatch(out);
            return Ok(());
        }
        if line.first() == Some(&b':') {
            return Ok(());
        }
        let (field, value) = match line.iter().position(|&b| b == b':') {
            Some(p) => {
                let v = line.get(p + 1..).unwrap_or_default();
                (line.get(..p).unwrap_or_default(), v.strip_prefix(b" ").unwrap_or(v))
            }
            None => (line, &[][..]),
        };
        match field {
            b"event" => self.event = String::from_utf8_lossy(value).into_owned(),
            b"data" => {
                if self.data.len().saturating_add(value.len()) > MAX_LINE {
                    return Err(LlmError::TooLarge);
                }
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(&String::from_utf8_lossy(value));
                self.has_data = true;
            }
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        let event = std::mem::take(&mut self.event);
        if self.has_data {
            self.has_data = false;
            out.push(SseEvent { event, data: std::mem::take(&mut self.data) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut p = SseParser::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(p.push(c).unwrap());
        }
        out.extend(p.finish().unwrap());
        out
    }

    #[test]
    fn parses_events_with_any_line_endings() {
        let s = b"\xEF\xBB\xBF: comment\nevent: a\ndata: 1\ndata:2\n\r\nevent: b\rdata: x\r\rdata: tail";
        let ev = parse_all(&[s]);
        assert_eq!(
            ev,
            vec![
                SseEvent { event: "a".into(), data: "1\n2".into() },
                SseEvent { event: "b".into(), data: "x".into() },
                SseEvent { event: String::new(), data: "tail".into() },
            ]
        );
        for cut in 0..=s.len() {
            assert_eq!(parse_all(&[&s[..cut], &s[cut..]]), ev, "cut at {cut}");
        }
        let bytes: Vec<&[u8]> = s.chunks(1).collect();
        assert_eq!(parse_all(&bytes), ev);
    }

    #[test]
    fn event_without_data_is_not_dispatched() {
        assert!(parse_all(&[b"event: ping\n\n"]).is_empty());
    }

    #[test]
    fn giant_lines_and_streams_are_rejected() {
        let mut p = SseParser::new();
        let chunk = vec![b'a'; 1 << 20];
        let mut err = None;
        for _ in 0..10 {
            if let Err(e) = p.push(&chunk) {
                err = Some(e);
                break;
            }
        }
        assert_eq!(err, Some(LlmError::TooLarge));

        let mut p = SseParser::new();
        let mut line = b"data: ".to_vec();
        line.extend(vec![b'x'; 1 << 20]);
        line.push(b'\n');
        let mut err = None;
        for _ in 0..80 {
            if let Err(e) = p.push(&line) {
                err = Some(e);
                break;
            }
        }
        assert_eq!(err, Some(LlmError::TooLarge));
    }
}
