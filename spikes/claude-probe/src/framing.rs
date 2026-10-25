//! Разбиение байтового потока на строки NDJSON.
//!
//! UTF-8 декодируется только у полной строки, поэтому многобайтовый символ,
//! разрезанный между чтениями, не превращается в символы замены (AT-B-06).

#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Line(Vec<u8>),
    /// Строка длиннее лимита: содержимое отброшено, сохранён только размер.
    Oversized {
        dropped: usize,
    },
}

pub struct LineFramer {
    buf: Vec<u8>,
    max_line: usize,
    dropping: Option<usize>,
}

impl LineFramer {
    pub fn new(max_line: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_line,
            dropping: None,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<Frame> {
        let mut out = Vec::new();
        let mut rest = chunk;
        while !rest.is_empty() {
            match rest.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    self.append(&rest[..pos]);
                    out.push(self.take_frame());
                    rest = &rest[pos + 1..];
                }
                None => {
                    self.append(rest);
                    rest = &[];
                }
            }
        }
        out
    }

    /// Хвост без завершающего перевода строки (при EOF).
    pub fn finish(&mut self) -> Option<Frame> {
        if self.buf.is_empty() && self.dropping.is_none() {
            return None;
        }
        Some(self.take_frame())
    }

    fn append(&mut self, part: &[u8]) {
        if let Some(dropped) = self.dropping.as_mut() {
            *dropped += part.len();
            return;
        }
        if self.buf.len() + part.len() > self.max_line {
            self.dropping = Some(self.buf.len() + part.len());
            self.buf.clear();
            return;
        }
        self.buf.extend_from_slice(part);
    }

    fn take_frame(&mut self) -> Frame {
        if let Some(dropped) = self.dropping.take() {
            return Frame::Oversized { dropped };
        }
        let mut line = std::mem::take(&mut self.buf);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Frame::Line(line)
    }
}

/// Строгое декодирование: при ошибке возвращаем lossy-текст как Err.
pub fn decode_line(line: &[u8]) -> Result<String, String> {
    match std::str::from_utf8(line) {
        Ok(s) => Ok(s.to_owned()),
        Err(_) => Err(String::from_utf8_lossy(line).into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(frames: Vec<Frame>) -> Vec<String> {
        frames
            .into_iter()
            .map(|f| match f {
                Frame::Line(b) => decode_line(&b).expect("valid utf-8"),
                Frame::Oversized { dropped } => format!("<oversized {dropped}>"),
            })
            .collect()
    }

    #[test]
    fn multibyte_split_at_every_byte_boundary() {
        let text = "{\"t\":\"привет 🦄 ok\"}\n{\"t\":\"второй — ✓\"}\n";
        let bytes = text.as_bytes();
        for split in 1..bytes.len() {
            let mut framer = LineFramer::new(1024);
            let mut out = framer.push(&bytes[..split]);
            out.extend(framer.push(&bytes[split..]));
            assert!(framer.finish().is_none());
            let got = lines(out);
            assert_eq!(
                got,
                vec!["{\"t\":\"привет 🦄 ok\"}", "{\"t\":\"второй — ✓\"}"],
                "split at {split}"
            );
            assert!(got.iter().all(|l| !l.contains('\u{FFFD}')));
        }
    }

    #[test]
    fn byte_by_byte_feed() {
        let text = "🦄🦄\nабв\n";
        let mut framer = LineFramer::new(64);
        let mut out = Vec::new();
        for b in text.as_bytes() {
            out.extend(framer.push(std::slice::from_ref(b)));
        }
        assert_eq!(lines(out), vec!["🦄🦄", "абв"]);
    }

    #[test]
    fn several_lines_in_one_chunk_and_crlf() {
        let mut framer = LineFramer::new(64);
        let out = framer.push(b"a\r\nb\n\nc");
        assert_eq!(lines(out), vec!["a", "b", ""]);
        assert_eq!(framer.finish(), Some(Frame::Line(b"c".to_vec())));
        assert_eq!(framer.finish(), None);
    }

    #[test]
    fn oversized_line_is_dropped_and_stream_recovers() {
        let mut framer = LineFramer::new(4);
        let mut out = framer.push(b"abc");
        out.extend(framer.push(b"defgh"));
        out.extend(framer.push(b"ij\nok\n"));
        assert_eq!(lines(out), vec!["<oversized 10>", "ok"]);
    }

    #[test]
    fn invalid_utf8_is_reported_not_hidden() {
        let err = decode_line(&[0x7b, 0xff, 0x7d]).unwrap_err();
        assert!(err.contains('\u{FFFD}'));
    }
}
