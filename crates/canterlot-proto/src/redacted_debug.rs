use std::fmt;

use crate::worker::v1::{Command, DeliverInput, TextBlock, command};

impl fmt::Debug for TextBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextBlock")
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

impl fmt::Debug for DeliverInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeliverInput")
            .field("input_id", &self.input_id)
            .field("conversation_id", &self.conversation_id)
            .field("content", &self.content)
            .finish()
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Command")
            .field("command_id", &self.command_id)
            .field("dispatch_attempt", &self.dispatch_attempt)
            .field("execution_id", &self.execution_id)
            .field("execution_generation", &self.execution_generation)
            .field("kind", &self.kind)
            .finish()
    }
}

impl fmt::Debug for command::Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeliverInput(input) => f.debug_tuple("DeliverInput").field(input).finish(),
            Self::CancelInput(input) => f.debug_tuple("CancelInput").field(input).finish(),
            Self::WithdrawInput(input) => f.debug_tuple("WithdrawInput").field(input).finish(),
            Self::Interrupt(input) => f.debug_tuple("Interrupt").field(input).finish(),
            Self::StartExecution(input) => f.debug_tuple("StartExecution").field(input).finish(),
            Self::DeliverDecision(input) => f.debug_tuple("DeliverDecision").field(input).finish(),
            Self::StopExecution(input) => f.debug_tuple("StopExecution").field(input).finish(),
        }
    }
}

impl fmt::Debug for crate::worker::v1::SuggestWorkdirs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SuggestWorkdirs")
            .field("request_id", &self.request_id)
            .field("path_bytes", &self.path.len())
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::WorkdirSuggestions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkdirSuggestions")
            .field("request_id", &self.request_id)
            .field("directories", &self.directories.len())
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::BlockDelta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockDelta")
            .field("append_bytes", &self.append.len())
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::BlockFinal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockFinal")
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

impl fmt::Debug for crate::worker::ipc::Deliver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use crate::worker::v1::content_block;
        use prost::Message;

        let text_bytes: usize = match self
            .command
            .as_ref()
            .and_then(|command| command.kind.as_ref())
        {
            Some(command::Kind::DeliverInput(input)) => input
                .content
                .iter()
                .map(|block| match &block.block {
                    Some(content_block::Block::Text(text)) => text.text.len(),
                    Some(content_block::Block::Attachment(_)) | None => 0,
                })
                .sum(),
            _ => 0,
        };
        f.debug_struct("Deliver")
            .field(
                "command_bytes",
                &self.command.as_ref().map(Message::encoded_len).unwrap_or(0),
            )
            .field("text_bytes", &text_bytes)
            .finish()
    }
}

impl fmt::Debug for crate::worker::ipc::Attach {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Attach")
            .field("protocol_version", &self.protocol_version)
            .field("execution_id", &self.execution_id)
            .field("generation", &self.generation)
            .field("secret_bytes", &self.secret.len())
            .finish()
    }
}

impl fmt::Debug for crate::worker::ipc::Ownership {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ownership")
            .field("execution_id", &self.execution_id)
            .field("generation", &self.generation)
            .field("phase", &self.phase)
            .field("secret_sha256_bytes", &self.secret_sha256.len())
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::AttachmentRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentRef")
            .field("size", &self.size)
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::AttachmentChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentChunk")
            .field("data_bytes", &self.data.len())
            .finish()
    }
}

impl fmt::Debug for crate::worker::ipc::LocalFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalFile")
            .field("size", &self.size)
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::BlockStarted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockStarted")
            .field("block_id", &self.block_id)
            .field("rev", &self.rev)
            .field("agent_bytes", &self.agent.len())
            .field("model_bytes", &self.model.len())
            .field("tool_name_bytes", &self.tool_name.len())
            .field("summary_bytes", &self.summary.len())
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::PhaseChanged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PhaseChanged")
            .field("execution_id", &self.execution_id)
            .field("phase", &self.phase)
            .field("tool_name_bytes", &self.tool_name.len())
            .field("summary_bytes", &self.summary.len())
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::MediaChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaChunk")
            .field("part", &self.part)
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::media_chunk::Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Header(header) => f
                .debug_struct("Header")
                .field("media_id", &header.media_id)
                .field("size", &header.size)
                .finish_non_exhaustive(),
            Self::Data(data) => f
                .debug_struct("Data")
                .field("data_bytes", &data.len())
                .finish(),
        }
    }
}

impl fmt::Debug for crate::worker::v1::DeliverDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeliverDecision")
            .field("request_id", &self.request_id)
            .field("decision_id", &self.decision_id)
            .field("generation", &self.generation)
            .field("kind", &self.kind)
            .field("decision_payload_json", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::RequestOpened {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestOpened")
            .field("request_id", &self.request_id)
            .field("execution_id", &self.execution_id)
            .field("generation", &self.generation)
            .field("kind", &self.kind)
            .field("payload_json", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::DecisionApplied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecisionApplied")
            .field("request_id", &self.request_id)
            .field("decision_id", &self.decision_id)
            .field("proof_kind", &self.proof_kind)
            .field("proof_payload_json", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::Instructions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Instructions")
            .field("mode", &self.mode)
            .field("preset_id", &self.preset_id)
            .field("revision", &self.revision)
            .field("title_bytes", &self.title.len())
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::AgentCallRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentCallRequest")
            .field("execution_id", &self.execution_id)
            .field("generation", &self.generation)
            .field("call_id", &self.call_id)
            .field("tool", &self.tool)
            .field("arguments_bytes", &self.arguments_json.len())
            .finish()
    }
}

impl fmt::Debug for crate::worker::v1::AgentCallResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentCallResponse")
            .field("result_bytes", &self.result_json.len())
            .field("error_code", &self.error_code)
            .field("error_message_bytes", &self.error_message.len())
            .finish()
    }
}

impl fmt::Debug for crate::worker::ipc::AgentCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentCall")
            .field("call_id", &self.call_id)
            .field("tool", &self.tool)
            .field("arguments_bytes", &self.arguments_json.len())
            .finish()
    }
}

impl fmt::Debug for crate::worker::ipc::AgentCallResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentCallResult")
            .field("call_id", &self.call_id)
            .field("result_bytes", &self.result_json.len())
            .field("error_code", &self.error_code)
            .finish_non_exhaustive()
    }
}
