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
            Self::StopExecution(input) => f.debug_tuple("StopExecution").field(input).finish(),
        }
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
                    None => 0,
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
