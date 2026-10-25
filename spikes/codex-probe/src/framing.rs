//! Разбиение байтового потока stdout на строки JSONL.
//!
//! Режем только по `\n` (0x0A): этот байт не встречается внутри многобайтовых
//! последовательностей UTF-8, поэтому символ, разорванный между двумя чтениями,
//! собирается без потерь (AT-B-06).

use std::fmt;

/// Ошибка фрейминга: строка превысила лимит или не является UTF-8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    LineTooLong { limit: usize },
    InvalidUtf8 { len: usize },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::LineTooLong { limit } => write!(f, "line exceeds {limit} bytes"),
            FrameError::InvalidUtf8 { len } => write!(f, "line of {len} bytes is not valid UTF-8"),
        }
    }
}

impl std::error::Error for FrameError {}

pub struct LineFramer {
    buf: Vec<u8>,
    max_line: usize,
    /// Режим пропуска хвоста слишком длинной строки до ближайшего `\n`.
    discarding: bool,
}

impl LineFramer {
    pub fn new(max_line: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_line,
            discarding: false,
        }
    }

    /// Принимает очередной кусок байтов и возвращает все завершённые строки.
    /// Пустые строки пропускаются, завершающий `\r` отрезается.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Result<String, FrameError>> {
        let mut out = Vec::new();
        let mut rest = chunk;
        while let Some(pos) = rest.iter().position(|&b| b == b'\n') {
            let (head, tail) = rest.split_at(pos);
            rest = &tail[1..];
            if self.discarding {
                self.discarding = false;
                self.buf.clear();
                continue;
            }
            if self.buf.len() + head.len() > self.max_line {
                self.buf.clear();
                out.push(Err(FrameError::LineTooLong {
                    limit: self.max_line,
                }));
                continue;
            }
            self.buf.extend_from_slice(head);
            let line = std::mem::take(&mut self.buf);
            if let Some(item) = Self::decode(line) {
                out.push(item);
            }
        }
        if !rest.is_empty() && !self.discarding {
            if self.buf.len() + rest.len() > self.max_line {
                self.buf.clear();
                self.discarding = true;
                out.push(Err(FrameError::LineTooLong {
                    limit: self.max_line,
                }));
            } else {
                self.buf.extend_from_slice(rest);
            }
        }
        out
    }

    /// Остаток после EOF: незавершённая строка без `\n`.
    pub fn finish(&mut self) -> Option<Result<String, FrameError>> {
        if self.discarding {
            self.discarding = false;
            self.buf.clear();
            return None;
        }
        let line = std::mem::take(&mut self.buf);
        Self::decode(line)
    }

    fn decode(mut line: Vec<u8>) -> Option<Result<String, FrameError>> {
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            return None;
        }
        let len = line.len();
        Some(String::from_utf8(line).map_err(|_| FrameError::InvalidUtf8 { len }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_lines(v: Vec<Result<String, FrameError>>) -> Vec<String> {
        v.into_iter().map(|r| r.expect("valid line")).collect()
    }

    #[test]
    fn splits_multiple_lines_in_one_chunk() {
        let mut f = LineFramer::new(1024);
        let got = ok_lines(f.push(b"{\"a\":1}\n{\"b\":2}\n{\"c\""));
        assert_eq!(got, vec!["{\"a\":1}", "{\"b\":2}"]);
        let got = ok_lines(f.push(b":3}\n"));
        assert_eq!(got, vec!["{\"c\":3}"]);
        assert!(f.finish().is_none());
    }

    #[test]
    fn multibyte_split_at_every_byte_boundary() {
        // кириллица, эмодзи с ZWJ и флаг: 2-, 3- и 4-байтовые последовательности
        let text = "{\"delta\":\"Привет, мир 👩‍💻 🇷🇺 ✓\"}\n";
        let bytes = text.as_bytes();
        for cut in 1..bytes.len() {
            let mut f = LineFramer::new(1024);
            let mut got = ok_lines(f.push(&bytes[..cut]));
            got.extend(ok_lines(f.push(&bytes[cut..])));
            assert_eq!(got, vec![text.trim_end().to_string()], "cut at {cut}");
            assert!(!got[0].contains('\u{FFFD}'));
        }
    }

    #[test]
    fn multibyte_fed_one_byte_at_a_time() {
        let text = "{\"t\":\"日本語テキスト 🎉\"}\n{\"t\":\"ok\"}\n";
        let mut f = LineFramer::new(1024);
        let mut got = Vec::new();
        for b in text.as_bytes() {
            got.extend(ok_lines(f.push(std::slice::from_ref(b))));
        }
        assert_eq!(got, vec!["{\"t\":\"日本語テキスト 🎉\"}", "{\"t\":\"ok\"}"]);
    }

    #[test]
    fn strips_crlf_and_skips_empty_lines() {
        let mut f = LineFramer::new(1024);
        let got = ok_lines(f.push(b"\n\r\n{\"x\":1}\r\n\n"));
        assert_eq!(got, vec!["{\"x\":1}"]);
    }

    #[test]
    fn trailing_partial_line_is_returned_on_finish() {
        let mut f = LineFramer::new(1024);
        assert!(f.push(b"{\"tail\":true}").is_empty());
        assert_eq!(f.finish(), Some(Ok("{\"tail\":true}".to_string())));
    }

    #[test]
    fn invalid_utf8_is_reported_not_replaced() {
        let mut f = LineFramer::new(1024);
        let got = f.push(b"\xff\xfe\n{\"ok\":1}\n");
        assert_eq!(got[0], Err(FrameError::InvalidUtf8 { len: 2 }));
        assert_eq!(got[1], Ok("{\"ok\":1}".to_string()));
    }

    #[test]
    fn overlong_line_is_dropped_and_stream_recovers() {
        let mut f = LineFramer::new(8);
        let got = f.push(b"0123456789");
        assert_eq!(got, vec![Err(FrameError::LineTooLong { limit: 8 })]);
        // хвост длинной строки пропускается до перевода строки
        let got = f.push(b"abc\n{\"a\":1}\n");
        assert_eq!(got, vec![Ok("{\"a\":1}".to_string())]);
    }
}
