use prost::Message;

use crate::worker::v1::{Command, JournalRecords, command, content_block};

pub const MAX_ATTACHMENT_BYTES: u64 = 104_857_600;
pub const MAX_ATTACHMENTS_PER_INPUT: usize = 10;
pub const MAX_ATTACHMENT_NAME_BYTES: usize = 255;
pub const MAX_METRICS_EXECUTIONS: usize = 256;
/// Картинка, на которую смотрел агент: крупнее показывается только путь и размер.
pub const MAX_MEDIA_BYTES: u64 = 20_971_520;
/// Файл, который агент отдаёт владельцу: не больше 50 МиБ.
pub const MAX_SHARE_BYTES: u64 = 52_428_800;
pub const MAX_SHARE_TITLE_BYTES: usize = 512;
pub const MAX_SHARE_NOTE_BYTES: usize = 4096;
pub const MEDIA_MIMES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];
/// Текст пресета инструкций агента: данные уровня сообщения, в лимит меты не входит.
pub const MAX_PRESET_TEXT_BYTES: usize = 65_536;
/// Аргументы одного вызова инструмента агента по каналу данных.
pub const MAX_AGENT_CALL_BYTES: usize = 262_144;

/// Рабочая папка: входит в ключ `machine_workdirs`, длиннее не принимается и подсказка.
pub const MAX_WORKDIR_BYTES: usize = 2048;
pub const MAX_WORKDIR_SUGGESTIONS: usize = 30;
/// Историй Claude Code в одном ответе формы.
pub const MAX_CLAUDE_HISTORIES: usize = 50;
/// Путь пробы: данные уровня сообщения, длиннее не принимается и не отвечается.
pub const MAX_PROBE_PATH_BYTES: usize = 4096;
/// Столько байт начала файла проба отдаёт, когда просят больше.
pub const MAX_PROBE_HEAD_BYTES: u32 = 4096;

/// Крупных каталогов сборки в одном отчёте сторожа.
pub const MAX_WATCH_HOTSPOTS: usize = 15;
/// Переживших исполнение процессов в одном отчёте сторожа.
pub const MAX_WATCH_LEAKED: usize = 64;
pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 1_048_576;
pub const MAX_INPUT_TEXT_BYTES: usize = 262_144;
pub const MAX_INPUT_META_BYTES: usize = 65_536;
pub const MAX_EVENT_BATCH_BYTES: usize = 524_288;
pub const MAX_FILE_CHUNK_BYTES: usize = 131_072;
pub const IPC_MAX_MESSAGE_BYTES: usize = 1_048_576;
pub const IPC_PROTOCOL_VERSION: u32 = 1;
/// Сопровождение пропускает неизвестное сообщение IPC и держит связь.
pub const IPC_CAP_IGNORE_UNKNOWN: &str = "ignore_unknown_v1";
/// Сопровождение принимает `CompactRequest`.
pub const IPC_CAP_COMPACT: &str = "compact_v1";
/// Сопровождение принимает `Deliver` с командой `SteerInput`.
pub const IPC_CAP_STEER: &str = "steer_v1";
/// Всё, что объявляет эта сборка в `Attached.ipc_capabilities`.
pub const IPC_CAPABILITIES: &[&str] = &[IPC_CAP_IGNORE_UNKNOWN, IPC_CAP_COMPACT, IPC_CAP_STEER];
pub const MAX_SJ_BATCH_RECORDS: usize = 256;
pub const MAX_BLOCK_DELTA_BYTES: usize = 16_384;
pub const BLOCK_FINAL_INLINE_MAX_BYTES: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitError {
    InputText { actual: usize },
    InputMeta { actual: usize },
    InstructionsText { actual: usize },
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
        if let Some(command::Kind::StartExecution(start)) = &cmd.kind
            && let Some(instructions) = &start.instructions
            && instructions.text.len() > MAX_PRESET_TEXT_BYTES
        {
            return Err(LimitError::InstructionsText {
                actual: instructions.text.len(),
            });
        }
        return Ok(());
    };
    let text_bytes = input
        .content
        .iter()
        .map(|block| match &block.block {
            Some(content_block::Block::Text(text)) => text.text.len(),
            Some(content_block::Block::Attachment(_)) | None => 0,
        })
        .sum();
    if text_bytes > MAX_INPUT_TEXT_BYTES {
        return Err(LimitError::InputText { actual: text_bytes });
    }
    let instructions_bytes = input
        .settings
        .as_ref()
        .and_then(|settings| settings.instructions.as_ref())
        .map(|instructions| instructions.text.len())
        .unwrap_or(0);
    if instructions_bytes > MAX_PRESET_TEXT_BYTES {
        return Err(LimitError::InstructionsText {
            actual: instructions_bytes,
        });
    }

    let meta_bytes = message_bytes - text_bytes - instructions_bytes;
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

pub fn check_ipc_message<M: Message>(message: &M) -> Result<(), LimitError> {
    let actual = message.encoded_len();
    if actual > IPC_MAX_MESSAGE_BYTES {
        return Err(LimitError::Message { actual });
    }

    Ok(())
}
