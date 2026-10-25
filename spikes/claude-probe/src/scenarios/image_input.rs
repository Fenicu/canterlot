//! Необязательная возможность: изображение во вводе (base64-блок `image`).

use std::time::Duration;

use serde_json::json;

use super::{Ctx, TURN_TIMEOUT, new_uuid, result_summary};
use crate::broker::deny_all;
use crate::config::Config;
use crate::protocol;

/// CRC-32 (IEEE) для чанков PNG.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Однотонный RGB PNG без сжатия (zlib stored block), до 64 КиБ сырых данных.
pub fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    // Строка: байт фильтра 0 и w пикселей.
    let mut row = vec![0u8];
    for _ in 0..w {
        row.extend_from_slice(&rgb);
    }
    let raw: Vec<u8> = (0..h).flat_map(|_| row.iter().copied()).collect();
    let len = u16::try_from(raw.len()).unwrap_or(u16::MAX);
    let mut z = vec![0x78, 0x01, 0x01];
    z.extend_from_slice(&len.to_le_bytes());
    z.extend_from_slice(&(!len).to_le_bytes());
    z.extend_from_slice(&raw[..usize::from(len)]);
    z.extend_from_slice(&adler32(&raw[..usize::from(len)]).to_be_bytes());

    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        png.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        png.extend_from_slice(&body);
        png.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    png
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(char::from(T[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let mut ctx = Ctx::new(
        cfg,
        "image_input",
        deny_all("image_input: no tool calls expected"),
    )
    .await?;
    let mut s = ctx.spawn(&[], &[])?;
    let png = solid_png(32, 32, [220, 20, 20]);
    let id = new_uuid();
    let msg = protocol::user_message_blocks(
        &id,
        json!([
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": base64(&png)}},
            {"type": "text", "text": "What is the dominant color of this image? Reply with exactly one lowercase English word."}
        ]),
    );
    ctx.step("turn: user message with a base64 PNG image block (32x32 solid red)");
    ctx.turns += 1;
    s.send_user_value(&id, &msg).await?;
    let r = s
        .wait_next_result(TURN_TIMEOUT)
        .await
        .map(|(_, r)| r)
        .unwrap_or_default();
    let answer = r["result"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let ok = ctx.check(
        "image_understood",
        answer.contains("red"),
        json!({"answer": answer, "png_bytes": png.len(), "result": result_summary(&r), "delivery": s.delivery_note(&id)}),
    );
    s.p.close_stdin();
    s.pump_for(Duration::from_secs(3)).await;
    let _ = s.p.wait_exit(Duration::from_secs(20)).await;
    ctx.finish(
        if ok { "PASS" } else { "FAIL" },
        if ok {
            "base64 image block in a stream-json user message was understood"
        } else {
            "see check notes"
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_known_values() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected);
        }
    }

    #[test]
    fn png_has_signature_and_valid_chunk_crcs() {
        let png = solid_png(2, 2, [255, 0, 0]);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        let mut i = 8;
        let mut kinds = Vec::new();
        while i < png.len() {
            let len = u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]) as usize;
            let body = &png[i + 4..i + 8 + len];
            let crc = u32::from_be_bytes([
                png[i + 8 + len],
                png[i + 9 + len],
                png[i + 10 + len],
                png[i + 11 + len],
            ]);
            assert_eq!(crc32(body), crc);
            kinds.push(String::from_utf8_lossy(&body[..4]).into_owned());
            i += 12 + len;
        }
        assert_eq!(kinds, vec!["IHDR", "IDAT", "IEND"]);
    }
}
