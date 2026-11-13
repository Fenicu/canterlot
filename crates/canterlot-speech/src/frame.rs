//! Кадры протокола: вид (1 байт), длина (u32 big-endian), полезная нагрузка.
//! Тот же формат идёт между хабом и службой и между службой и движком.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const KIND_JSON: u8 = 0x01;
pub const KIND_AUDIO: u8 = 0x02;

/// Предел JSON от хаба.
pub const MAX_CLIENT_JSON: usize = 4096;
/// Предел звукового кадра от хаба.
pub const MAX_AUDIO_FRAME: usize = 8192;
/// Предел JSON от службы хабу.
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
            Frame::Json(b) => write!(f, "Json({} bytes)", b.len()),
            Frame::Audio(b) => write!(f, "Audio({} bytes)", b.len()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("соединение закрыто")]
    Closed,
    #[error("неизвестный вид кадра {0:#04x}")]
    UnknownKind(u8),
    #[error("кадр длиной {len} больше предела {max}")]
    TooLarge { len: usize, max: usize },
    #[error("ввод-вывод: {0}")]
    Io(#[from] std::io::Error),
}

/// Пределы на чтение: JSON и звук отдельно.
#[derive(Debug, Clone, Copy)]
pub struct ReadLimits {
    pub json: usize,
    pub audio: usize,
}

impl ReadLimits {
    pub const CLIENT: ReadLimits = ReadLimits {
        json: MAX_CLIENT_JSON,
        audio: MAX_AUDIO_FRAME,
    };
    pub const SERVICE: ReadLimits = ReadLimits {
        json: MAX_SERVICE_JSON,
        audio: 0,
    };
    /// Канал с движком: задания несут сегменты до десятков секунд звука.
    pub const ENGINE: ReadLimits = ReadLimits {
        json: 16 << 20,
        audio: 64 << 20,
    };
}

pub async fn read_frame<R: AsyncRead + Unpin>(
    r: &mut R,
    limits: ReadLimits,
) -> Result<Frame, FrameError> {
    let mut header = [0u8; 5];
    match r.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(FrameError::Closed),
        Err(e) => return Err(e.into()),
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
    r.read_exact(&mut payload).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            FrameError::Closed
        } else {
            e.into()
        }
    })?;
    Ok(if header[0] == KIND_JSON {
        Frame::Json(payload)
    } else {
        Frame::Audio(payload)
    })
}

pub fn encode(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    kind: u8,
    payload: &[u8],
) -> std::io::Result<()> {
    w.write_all(&encode(kind, payload)).await?;
    w.flush().await
}

pub async fn write_json<W: AsyncWrite + Unpin>(
    w: &mut W,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    write_frame(w, KIND_JSON, &bytes).await
}

/// PCM s16le в отсчёты.
pub fn pcm_from_bytes(bytes: &[u8]) -> Vec<i16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c))
        .collect()
}

pub fn pcm_to_bytes(pcm: &[i16]) -> Vec<u8> {
    pcm.iter().flat_map(|s| s.to_le_bytes()).collect()
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

        let big = encode(KIND_AUDIO, &vec![0; MAX_AUDIO_FRAME + 2]);
        assert!(matches!(
            read_frame(&mut &big[..], ReadLimits::CLIENT).await,
            Err(FrameError::TooLarge { .. })
        ));
        let odd = [7u8, 0, 0, 0, 0];
        assert!(matches!(
            read_frame(&mut &odd[..], ReadLimits::CLIENT).await,
            Err(FrameError::UnknownKind(7))
        ));
    }

    #[test]
    fn debug_hides_payload() {
        let f = Frame::Json(b"secret words".to_vec());
        assert_eq!(format!("{f:?}"), "Json(12 bytes)");
    }
}
