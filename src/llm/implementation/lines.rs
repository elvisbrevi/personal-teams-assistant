//! A CLI's JSON stream read one line at a time in bounded memory. A line can carry megabytes
//! of tool-result payload (an image read is base64, twice): the run needs its event type,
//! session, result and prompts, never those bodies, so a long string's content is dropped
//! while the line is read and the rest of the event is parsed as usual.
use serde_json::Value;

/// A string longer than this is read as empty. Every line that fitted the old whole-line
/// limit reads exactly as before.
const STRING: usize = 256_000;
/// The rest of a line, its long strings dropped, is kept up to this; past it the event is
/// skipped.
const KEPT: usize = 1_000_000;
/// A line longer than this is skipped whole, however little of it would be kept.
const LINE: usize = 64 << 20;
/// A line up to this that is not JSON is noise and ignored, as before; a longer one is a
/// skipped event.
const NOISE: usize = 256_000;

/// What one line of the stream was.
#[derive(Debug, PartialEq)]
pub(super) enum Line {
    Event(Value),
    /// A short line that is not JSON.
    Ignored,
    /// An event that could not be read within the bounds; nothing of it is kept.
    Skipped,
}

pub(super) struct Lines {
    /// The line so far, without the content of strings longer than `STRING`.
    kept: Vec<u8>,
    /// The content of the string being read, while it fits `STRING`.
    string: Vec<u8>,
    in_string: bool,
    escaped: bool,
    /// The current string passed `STRING`: its content is being dropped.
    long: bool,
    /// The line passed `KEPT` or `LINE`.
    over: bool,
    length: usize,
    /// The current line dropped the content of a string longer than `STRING`.
    dropping: bool,
    /// The last line read did.
    dropped: bool,
}
impl Lines {
    pub(super) fn new() -> Self {
        Self {
            kept: Vec::new(),
            string: Vec::new(),
            in_string: false,
            escaped: false,
            long: false,
            over: false,
            length: 0,
            dropping: false,
            dropped: false,
        }
    }
    /// Whether the last line read had a string read as empty because it was too long: a
    /// report that reads empty from such a line was over the bound, not missing.
    pub(super) fn dropped(&self) -> bool {
        self.dropped
    }
    /// Reads `bytes` up to the end of a line: how many it used and, at a line's end, the line.
    pub(super) fn feed(&mut self, bytes: &[u8]) -> (usize, Option<Line>) {
        for (at, &byte) in bytes.iter().enumerate() {
            // A newline never appears inside a JSON string: it always ends the line.
            if byte == b'\n' {
                return (at + 1, Some(self.end()));
            }
            self.byte(byte);
        }
        (bytes.len(), None)
    }
    /// The end of the stream: the last line if it had no newline.
    pub(super) fn finish(&mut self) -> Option<Line> {
        (self.length > 0).then(|| self.end())
    }
    fn byte(&mut self, byte: u8) {
        self.length += 1;
        if self.length > LINE {
            self.over = true;
        }
        if self.over {
            return;
        }
        if !self.in_string {
            self.keep(byte);
            if byte == b'"' {
                self.in_string = true;
                self.long = false;
                self.string.clear();
            }
            return;
        }
        if self.escaped {
            self.escaped = false;
        } else if byte == b'\\' {
            self.escaped = true;
        } else if byte == b'"' {
            self.in_string = false;
            if !self.long {
                if self.kept.len() + self.string.len() < KEPT {
                    self.kept.extend_from_slice(&self.string);
                } else {
                    self.over = true;
                }
            }
            self.keep(byte);
            return;
        }
        if self.long {
            return;
        }
        if self.string.len() < STRING {
            self.string.push(byte);
        } else {
            self.long = true;
            self.dropping = true;
            self.string.clear();
        }
    }
    fn keep(&mut self, byte: u8) {
        if self.kept.len() < KEPT {
            self.kept.push(byte);
        } else {
            self.over = true;
        }
    }
    fn end(&mut self) -> Line {
        // A line that ends inside a string lacks its closing quote and never parses.
        let line = if self.over {
            Line::Skipped
        } else {
            match serde_json::from_slice::<Value>(&self.kept) {
                Ok(value) => Line::Event(value),
                Err(_) if self.length <= NOISE => Line::Ignored,
                Err(_) => Line::Skipped,
            }
        };
        self.kept.clear();
        self.string.clear();
        self.in_string = false;
        self.escaped = false;
        self.long = false;
        self.over = false;
        self.length = 0;
        self.dropped = std::mem::take(&mut self.dropping);
        line
    }
    #[cfg(test)]
    fn retained(&self) -> usize {
        self.kept.capacity() + self.string.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(lines: &mut Lines, stream: &[u8], chunk: usize) -> Vec<Line> {
        let mut read = Vec::new();
        for mut bytes in stream.chunks(chunk) {
            while !bytes.is_empty() {
                let (used, line) = lines.feed(bytes);
                bytes = &bytes[used..];
                read.extend(line);
            }
        }
        read.extend(lines.finish());
        read
    }
    fn image_line(size: usize) -> Vec<u8> {
        let image = "A".repeat(size);
        let mut line = serde_json::to_vec(&json!({"type":"user","session_id":"s-1","message":{"role":"user","content":[{"type":"tool_result","content":[{"type":"image","source":{"type":"base64","data":image}}]}]},"tool_use_result":{"file":{"base64":image}}})).unwrap();
        line.push(b'\n');
        line
    }

    #[test]
    fn an_oversized_line_is_read_without_its_payload_in_bounded_memory() {
        let mut stream =
            b"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s-1\"}\n".to_vec();
        stream.extend(image_line(150_000));
        stream.extend(image_line(2_500_000));
        stream.extend(
            b"{\"type\":\"result\",\"is_error\":false,\"result\":\"Done.\\nBranch: feature/x\"}\n",
        );
        let mut lines = Lines::new();
        let read = read(&mut lines, &stream, 8192);
        assert_eq!(read.len(), 4);
        assert_eq!(
            read[0],
            Line::Event(json!({"type":"system","subtype":"init","session_id":"s-1"}))
        );
        // The 300 KB line's strings fit the string bound and are kept; the 5 MB line's are not.
        for (line, kept) in read[1..3].iter().zip([150_000, 0]) {
            let Line::Event(event) = line else {
                panic!("{line:?}")
            };
            assert_eq!(event["type"], "user");
            assert_eq!(event["session_id"], "s-1");
            let image = event["tool_use_result"]["file"]["base64"].as_str().unwrap();
            assert_eq!(image.len(), kept);
        }
        assert_eq!(
            read[3],
            Line::Event(
                json!({"type":"result","is_error":false,"result":"Done.\nBranch: feature/x"})
            )
        );
        assert!(!lines.dropped());
        // The 5 MB line was never held whole: what is retained stays within the kept and
        // string bounds (the 300 KB line is kept whole), far below the line.
        assert!(lines.retained() < KEPT + 2 * STRING, "{}", lines.retained());
        assert!(lines.retained() < 5_000_000 / 4, "{}", lines.retained());
    }

    #[test]
    fn a_line_says_whether_it_dropped_a_long_string() {
        let mut lines = Lines::new();
        let mut line =
            serde_json::to_vec(&json!({"type":"result","result":"R".repeat(300_000)})).unwrap();
        line.push(b'\n');
        let (_, read) = lines.feed(&line);
        assert_eq!(
            read,
            Some(Line::Event(json!({"type":"result","result":""})))
        );
        assert!(lines.dropped());
        let (_, read) = lines.feed(b"{\"type\":\"result\",\"result\":\"Done.\"}\n");
        assert!(matches!(read, Some(Line::Event(_))));
        assert!(!lines.dropped());
    }

    #[test]
    fn lines_that_fit_the_old_limit_read_as_before() {
        let text = "\u{e9}\"\\ \u{1F600}".repeat(10_000);
        let event =
            json!({"type":"result","result":text,"structured_output":{"items":[1,2.5,null,true]}});
        let mut line = serde_json::to_vec(&event).unwrap();
        line.extend(b"\nnot json\n\n");
        let read = read(&mut Lines::new(), &line, 7);
        assert_eq!(read, [Line::Event(event), Line::Ignored, Line::Ignored]);
    }

    #[test]
    fn an_event_beyond_every_bound_is_skipped_and_the_next_one_is_read() {
        // Too much kept after dropping long strings, an oversized line that is not JSON, and a
        // truncated last line.
        let many = serde_json::to_vec(&json!({"type":"user","parts":vec!["x".repeat(200_000); 8]}))
            .unwrap();
        let mut stream = many;
        stream.push(b'\n');
        stream.extend(b"{\"type\":\"user\",\"broken\":");
        stream.extend(vec![b'7'; 300_000]);
        stream.extend(
            b"]\n{\"type\":\"result\",\"result\":\"Done.\"}\n{\"type\":\"user\",\"data\":\"",
        );
        stream.extend(vec![b'A'; 300_000]);
        let read = read(&mut Lines::new(), &stream, 65_536);
        assert_eq!(
            read,
            [
                Line::Skipped,
                Line::Skipped,
                Line::Event(json!({"type":"result","result":"Done."})),
                Line::Skipped
            ]
        );
    }
}
