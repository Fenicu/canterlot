//! Разбор потока `GET /api/event`. Сервер пишет `data: <JSON>` и пустую строку,
//! комментарий `: heartbeat` каждые 15 с; `Last-Event-ID` не поддерживается.
//! Куски режутся где угодно, в том числе внутри кириллической буквы, поэтому байты
//! копятся до конца строки.

use serde_json::Value;
use std::{fmt, time::Duration};
use tokio::sync::mpsc;

/// Самый крупный законный кадр — картинка в результате инструмента: 20 МиБ в base64.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
/// Сервер шлёт пульс раз в 15 с: три пропуска значат зависшую связь.
const SILENCE_LIMIT: Duration = Duration::from_secs(45);

#[derive(PartialEq, Eq)]
pub enum Frame {
    Data(Vec<u8>),
    Oversized,
}

// Кадры несут текст разговора: в отладке только размер.
impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Data(bytes) => f.debug_tuple("Data").field(&bytes.len()).finish(),
            Self::Oversized => f.write_str("Oversized"),
        }
    }
}

pub struct Parser {
    cap: usize,
    line: Vec<u8>,
    data: Vec<u8>,
    has_data: bool,
    oversized: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self::with_cap(MAX_FRAME_BYTES)
    }
}

impl Parser {
    pub fn with_cap(cap: usize) -> Self {
        Self {
            cap,
            line: Vec::new(),
            data: Vec::new(),
            has_data: false,
            oversized: false,
        }
    }

    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<Frame>) {
        let mut rest = chunk;
        while !rest.is_empty() {
            match rest.iter().position(|&byte| byte == b'\n') {
                Some(end) => {
                    self.extend_line(&rest[..end]);
                    let line = std::mem::take(&mut self.line);
                    self.finish_line(&line, out);
                    rest = &rest[end + 1..];
                }
                None => {
                    self.extend_line(rest);
                    return;
                }
            }
        }
    }

    fn extend_line(&mut self, bytes: &[u8]) {
        if self.oversized {
            return;
        }
        if self.data.len() + self.line.len() + bytes.len() > self.cap {
            self.line = Vec::new();
            self.data = Vec::new();
            self.oversized = true;
            return;
        }
        self.line.extend_from_slice(bytes);
    }

    fn finish_line(&mut self, line: &[u8], out: &mut Vec<Frame>) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            if self.oversized {
                out.push(Frame::Oversized);
                self.oversized = false;
            } else if self.has_data {
                out.push(Frame::Data(std::mem::take(&mut self.data)));
            }
            self.has_data = false;
            return;
        }
        let Some(value) = line.strip_prefix(b"data:") else {
            return;
        };
        let value = value.strip_prefix(b" ").unwrap_or(value);
        if self.oversized {
            return;
        }
        if self.has_data {
            self.data.push(b'\n');
        }
        self.data.extend_from_slice(value);
        self.has_data = true;
    }
}

pub enum StreamItem {
    Event(Value),
    /// Кадр не разобран; `required` — по байтам видно, что это конец периода работы.
    Undecodable {
        required: bool,
    },
    Oversized,
    Ended,
}

impl fmt::Debug for StreamItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Event(event) => f
                .debug_struct("Event")
                .field("type", &event.get("type"))
                .finish_non_exhaustive(),
            Self::Undecodable { required } => f
                .debug_struct("Undecodable")
                .field("required", required)
                .finish(),
            Self::Oversized => f.write_str("Oversized"),
            Self::Ended => f.write_str("Ended"),
        }
    }
}

fn decode(frame: Frame) -> StreamItem {
    let bytes = match frame {
        Frame::Data(bytes) => bytes,
        Frame::Oversized => return StreamItem::Oversized,
    };
    match super::http::decode_json(&bytes) {
        Ok(value) if value.is_object() => StreamItem::Event(value),
        _ => StreamItem::Undecodable {
            required: bytes
                .windows(b"session.execution.".len())
                .any(|window| window == b"session.execution."),
        },
    }
}

/// Читает поток отдельной задачей: разбор крупных кадров не задерживает цикл
/// супервизора, а отмена `next_event` ничего не теряет.
pub async fn pump(mut response: reqwest::Response, items: mpsc::Sender<StreamItem>) {
    let mut parser = Parser::default();
    let mut frames = Vec::new();
    while let Ok(Ok(Some(chunk))) = tokio::time::timeout(SILENCE_LIMIT, response.chunk()).await {
        parser.push(&chunk, &mut frames);
        for frame in frames.drain(..) {
            if items.send(decode(frame)).await.is_err() {
                return;
            }
        }
    }
    let _ = items.send(StreamItem::Ended).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(chunks: &[&[u8]]) -> Vec<Frame> {
        let mut parser = Parser::default();
        let mut out = Vec::new();
        for chunk in chunks {
            parser.push(chunk, &mut out);
        }
        out
    }

    #[test]
    fn real_stream_prefix_gives_two_frames() {
        let wire =
            b"data: {\"type\":\"server.connected\"}\n\n: heartbeat\n\ndata: {\"type\":\"x\"}\n\n";
        assert_eq!(
            frames(&[wire]),
            vec![
                Frame::Data(br#"{"type":"server.connected"}"#.to_vec()),
                Frame::Data(br#"{"type":"x"}"#.to_vec())
            ]
        );
    }

    #[test]
    fn chunk_may_split_a_cyrillic_letter_anywhere() {
        let wire = "data: {\"text\":\"Привет\"}\n\n".as_bytes();
        for split in 1..wire.len() {
            let got = frames(&[&wire[..split], &wire[split..]]);
            let Frame::Data(bytes) = &got[0] else {
                panic!("{got:?}");
            };
            let value: Value = serde_json::from_slice(bytes).unwrap();
            assert_eq!(value["text"], "Привет", "split at {split}");
        }
    }

    #[test]
    fn crlf_and_multiline_data_follow_the_sse_rules() {
        assert_eq!(
            frames(&[b"id: 1\r\nevent: x\r\ndata:a\r\ndata: b\r\n\r\n"]),
            vec![Frame::Data(b"a\nb".to_vec())]
        );
    }

    #[test]
    fn comment_only_block_gives_no_frame() {
        assert!(frames(&[b": heartbeat\n\n\n\n"]).is_empty());
    }

    #[test]
    fn oversized_frame_is_dropped_and_the_next_one_survives() {
        let mut parser = Parser::with_cap(16);
        let mut out = Vec::new();
        parser.push(b"data: ", &mut out);
        parser.push(&[b'x'; 64], &mut out);
        parser.push(&[b'y'; 64], &mut out);
        parser.push(b"\n\ndata: ok\n\n", &mut out);
        assert_eq!(out, vec![Frame::Oversized, Frame::Data(b"ok".to_vec())]);
    }

    #[test]
    fn debug_shows_no_conversation_text() {
        let frame = Frame::Data("секрет".as_bytes().to_vec());
        assert!(!format!("{frame:?}").contains("секрет"));
        let item = decode(Frame::Data(
            r#"{"type":"session.text.delta","data":{"delta":"секрет"}}"#
                .as_bytes()
                .to_vec(),
        ));
        let shown = format!("{item:?}");
        assert!(shown.contains("session.text.delta"), "{shown}");
        assert!(!shown.contains("секрет"), "{shown}");
    }

    #[test]
    fn half_of_an_emoji_does_not_lose_the_frame() {
        let item = decode(Frame::Data(
            br#"{"type":"session.tool.success","data":{"text":"a\ud83d"}}"#.to_vec(),
        ));
        let StreamItem::Event(event) = item else {
            panic!("{item:?}");
        };
        assert_eq!(event["data"]["text"], "a\u{fffd}");
    }

    #[test]
    fn broken_frames_say_whether_a_terminal_event_was_lost() {
        assert!(matches!(
            decode(Frame::Data(b"{\"type\":\"session.execution.succ".to_vec())),
            StreamItem::Undecodable { required: true }
        ));
        assert!(matches!(
            decode(Frame::Data(b"[1]".to_vec())),
            StreamItem::Undecodable { required: false }
        ));
    }
}
