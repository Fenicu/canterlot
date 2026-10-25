use prost::Message;

use crate::worker::v1::{Command, JournalRecords, command, content_block};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 1_048_576;
pub const MAX_INPUT_TEXT_BYTES: usize = 262_144;
pub const MAX_INPUT_META_BYTES: usize = 65_536;
pub const MAX_EVENT_BATCH_BYTES: usize = 524_288;
pub const MAX_FILE_CHUNK_BYTES: usize = 131_072;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitError {
    InputText { actual: usize },
    InputMeta { actual: usize },
    Message { actual: usize },
    EventBatch { actual: usize },
}

pub fn check_deliver_input(cmd: &Command) -> Result<(), LimitError> {
    let message_bytes = cmd.encoded_len();
    if message_bytes > MAX_MESSAGE_BYTES {
        return Err(LimitError::Message {
            actual: message_bytes,
        });
    }

    let Some(command::Kind::DeliverInput(input)) = &cmd.kind else {
        return Ok(());
    };
    let text_bytes = input
        .content
        .iter()
        .map(|block| match &block.block {
            Some(content_block::Block::Text(text)) => text.text.len(),
            None => 0,
        })
        .sum();
    if text_bytes > MAX_INPUT_TEXT_BYTES {
        return Err(LimitError::InputText { actual: text_bytes });
    }

    let meta_bytes = message_bytes - text_bytes;
    if meta_bytes > MAX_INPUT_META_BYTES {
        return Err(LimitError::InputMeta { actual: meta_bytes });
    }

    Ok(())
}

pub fn check_record_batch(batch: &JournalRecords) -> Result<(), LimitError> {
    let actual = batch.encoded_len();
    if actual > MAX_EVENT_BATCH_BYTES {
        return Err(LimitError::EventBatch { actual });
    }

    Ok(())
}
