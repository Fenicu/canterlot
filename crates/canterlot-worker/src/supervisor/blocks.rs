use canterlot_proto::limits::MAX_BLOCK_DELTA_BYTES;
use sha2::{Digest, Sha256};

/// Открытый блок: ревизия и то, что хаб уже получил дельтами.
pub(super) struct OpenBlock {
    pub(super) rev: u64,
    streamed: usize,
    digest: Sha256,
}

impl OpenBlock {
    pub(super) fn new() -> Self {
        Self {
            rev: 1,
            streamed: 0,
            digest: Sha256::new(),
        }
    }

    /// Дельта ушла в журнал.
    pub(super) fn sent(&mut self, chunk: &str) {
        self.rev += 1;
        self.streamed += chunk.len();
        self.digest.update(chunk.as_bytes());
    }

    /// Часть финала, которой хаб ещё не видел. Финал крупнее порога хаб собирает только из
    /// дельт, а адаптеры, которым CLI отдаёт результат одной записью, их не присылают.
    /// Если ушедшее не совпало с началом финала, дописывать нечего: хаб увидит расхождение сумм.
    pub(super) fn missing_tail<'a>(&self, text: &'a str) -> &'a str {
        let Some(tail) = text.get(self.streamed..) else {
            return "";
        };
        if Sha256::digest(&text.as_bytes()[..self.streamed]) == self.digest.clone().finalize() {
            tail
        } else {
            ""
        }
    }
}

/// Куски текста не крупнее `MAX_BLOCK_DELTA_BYTES`, каждый режется по границе символа.
pub(super) fn delta_chunks(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut end = rest.len().min(MAX_BLOCK_DELTA_BYTES);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, tail) = rest.split_at(end);
        rest = tail;
        Some(chunk)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streamed(parts: &[&str]) -> OpenBlock {
        let mut block = OpenBlock::new();
        for part in parts {
            block.sent(part);
        }
        block
    }

    #[test]
    fn chunks_stay_within_the_limit_and_on_character_boundaries() {
        for text in [
            "a".repeat(3 * MAX_BLOCK_DELTA_BYTES + 1),
            format!("x{}", "я".repeat(40_000)),
            format!("ab{}", "😀".repeat(20_000)),
            "я".repeat(MAX_BLOCK_DELTA_BYTES / 2),
        ] {
            let chunks: Vec<&str> = delta_chunks(&text).collect();
            assert!(
                chunks
                    .iter()
                    .all(|c| !c.is_empty() && c.len() <= MAX_BLOCK_DELTA_BYTES)
            );
            assert_eq!(chunks.concat(), text);
        }
        assert_eq!(delta_chunks("").count(), 0);
    }

    #[test]
    fn whole_text_is_missing_when_nothing_was_streamed() {
        let text = "я".repeat(50_000);
        assert_eq!(OpenBlock::new().missing_tail(&text), text);
    }

    #[test]
    fn only_the_tail_after_a_matching_prefix_is_missing() {
        let text = format!("{}{}", "я".repeat(30_000), "ж".repeat(30_000));
        let block = streamed(&["я".repeat(10_000).as_str(), "я".repeat(20_000).as_str()]);
        assert_eq!(block.missing_tail(&text), "ж".repeat(30_000));
        assert_eq!(block.rev, 3);
        assert_eq!(streamed(&[&text]).missing_tail(&text), "");
    }

    #[test]
    fn diverging_or_longer_stream_gets_no_tail() {
        let text = "я".repeat(50_000);
        assert_eq!(streamed(&[&"ж".repeat(10_000)]).missing_tail(&text), "");
        assert_eq!(streamed(&[&"я".repeat(60_000)]).missing_tail(&text), "");
        // Граница ушедшего приходится внутрь символа финала.
        assert_eq!(streamed(&["x"]).missing_tail(&text), "");
    }
}
