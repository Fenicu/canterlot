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
