//! Кадры протокола хаб–служба речи (§1.1 `docs/design/dictation.md`):
//! вид (1 байт), длина (u32 big-endian), полезная нагрузка.
//!
//! Своя реализация, без зависимости от крейта `canterlot-speech`: тот тянет
//! нативную сборку transcribe.cpp.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const KIND_JSON: u8 = 0x01;
pub const KIND_AUDIO: u8 = 0x02;

/// Предел JSON-кадра от хаба службе.
pub const MAX_CLIENT_JSON: usize = 4096;
/// Предел звукового кадра от хаба службе.
pub const MAX_AUDIO_FRAME: usize = 8192;
/// Предел JSON-кадра от службы хабу.
pub const MAX_SERVICE_JSON: usize = 262_144;

#[derive(Clone, PartialEq, Eq)]
pub enum Frame {
    Json(Vec<u8>),
    Audio(Vec<u8>),
}

// Содержимое кадров не печатается: в них звук и текст.
impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Frame::Json(bytes) => write!(f, "Json({} bytes)", bytes.len()),
            Frame::Audio(bytes) => write!(f, "Audio({} bytes)", bytes.len()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("connection closed")]
    Closed,
    #[error("unknown frame kind {0:#04x}")]
    UnknownKind(u8),
    #[error("frame of {len} bytes exceeds {max}")]
    TooLarge { len: usize, max: usize },
    #[error("frame i/o failed")]
    Io(#[from] std::io::Error),
}

/// Пределы на чтение: JSON и звук отдельно; `0` запрещает вид целиком.
#[derive(Debug, Clone, Copy)]
pub struct ReadLimits {
    pub json: usize,
    pub audio: usize,
}

impl ReadLimits {
    /// Что служба вправе прочитать от хаба (и что хаб пишет службе).
    pub const CLIENT: ReadLimits = ReadLimits {
        json: MAX_CLIENT_JSON,
        audio: MAX_AUDIO_FRAME,
    };
    /// Что хаб вправе прочитать от службы: звука от неё не бывает.
    pub const PROTOCOL: ReadLimits = ReadLimits {
        json: MAX_SERVICE_JSON,
        audio: 0,
    };
}

pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    limits: ReadLimits,
) -> Result<Frame, FrameError> {
    let mut header = [0u8; 5];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(FrameError::Closed);
        }
        Err(error) => return Err(error.into()),
    }
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    let max = match header[0] {
        KIND_JSON => limits.json,
        KIND_AUDIO => limits.audio,
        other => return Err(FrameError::UnknownKind(other)),
    };
    if len > max {
        return Err(FrameError::TooLarge { len, max });
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            FrameError::Closed
        } else {
            error.into()
        }
    })?;
    Ok(match header[0] {
        KIND_JSON => Frame::Json(payload),
        _ => Frame::Audio(payload),
    })
}

pub fn encode(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub async fn write_json<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    write_frame(writer, KIND_JSON, &bytes).await
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    kind: u8,
    payload: &[u8],
) -> std::io::Result<()> {
    writer.write_all(&encode(kind, payload)).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip_and_limits() {
        let mut buf = encode(KIND_JSON, br#"{"type":"finish"}"#);
        buf.extend(encode(KIND_AUDIO, &[1, 0, 2, 0]));
        let mut r = &buf[..];
        assert_eq!(
            read_frame(&mut r, ReadLimits::CLIENT).await.unwrap(),
            Frame::Json(br#"{"type":"finish"}"#.to_vec())
        );
        assert_eq!(
            read_frame(&mut r, ReadLimits::CLIENT).await.unwrap(),
            Frame::Audio(vec![1, 0, 2, 0])
        );
        assert!(matches!(
            read_frame(&mut r, ReadLimits::CLIENT).await,
            Err(FrameError::Closed)
        ));

        let big = encode(KIND_JSON, &vec![b' '; MAX_SERVICE_JSON + 1]);
        assert!(matches!(
            read_frame(&mut &big[..], ReadLimits::PROTOCOL).await,
            Err(FrameError::TooLarge { .. })
        ));
        // Звука от службы не бывает вовсе.
        let audio = encode(KIND_AUDIO, &[1, 0]);
        assert!(matches!(
            read_frame(&mut &audio[..], ReadLimits::PROTOCOL).await,
            Err(FrameError::TooLarge { .. })
        ));
        let odd = [7u8, 0, 0, 0, 0];
        assert!(matches!(
            read_frame(&mut &odd[..], ReadLimits::PROTOCOL).await,
            Err(FrameError::UnknownKind(7))
        ));
        let huge = [KIND_JSON, 0xff, 0xff, 0xff, 0xff];
        assert!(matches!(
            read_frame(&mut &huge[..], ReadLimits::PROTOCOL).await,
            Err(FrameError::TooLarge { .. })
        ));
    }

    #[test]
    fn debug_hides_payload() {
        let f = Frame::Json(b"secret words".to_vec());
        assert_eq!(format!("{f:?}"), "Json(12 bytes)");
        let f = Frame::Audio(vec![0; 4]);
        assert_eq!(format!("{f:?}"), "Audio(4 bytes)");
    }
}
