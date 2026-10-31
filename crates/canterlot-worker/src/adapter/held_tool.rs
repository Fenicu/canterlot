//! Удержание начала вызова инструмента до решения о пометке «Разрешено правилами CLI».
//! Вызов ждёт запроса владельцу (без пометки), начала работы или завершения без запроса
//! (пометка по политике). События этого блока ждут вместе с ним.

use super::AdapterEvent;
use std::collections::VecDeque;
use uuid::Uuid;

struct HeldTool {
    item_id: String,
    block_id: Uuid,
    events: Vec<AdapterEvent>,
}

#[derive(Default)]
pub struct HeldTools {
    held: Vec<HeldTool>,
}

impl HeldTools {
    pub fn hold(&mut self, item_id: String, block_id: Uuid, event: AdapterEvent) {
        self.held.push(HeldTool {
            item_id,
            block_id,
            events: vec![event],
        });
    }

    pub fn item_of_block(&self, block_id: Uuid) -> Option<&str> {
        self.held
            .iter()
            .find(|held| held.block_id == block_id)
            .map(|held| held.item_id.as_str())
    }

    /// Событие удержанного блока остаётся с ним, остальные возвращаются вызывающему.
    pub fn capture(&mut self, event: AdapterEvent) -> Option<AdapterEvent> {
        let block = match &event {
            AdapterEvent::BlockDelta { block_id, .. }
            | AdapterEvent::BlockFinal { block_id, .. }
            | AdapterEvent::Media { block_id, .. } => Some(*block_id),
            _ => None,
        };
        match block.and_then(|block| self.held.iter_mut().find(|held| held.block_id == block)) {
            Some(held) => {
                held.events.push(event);
                None
            }
            None => Some(event),
        }
    }

    pub fn release(
        &mut self,
        item_id: &str,
        mark: Option<&'static str>,
        out: &mut VecDeque<AdapterEvent>,
    ) {
        if let Some(position) = self.held.iter().position(|held| held.item_id == item_id) {
            Self::flush(self.held.remove(position), mark, out);
        }
    }

    /// Ход кончился без решения: вызовы уходят без пометки.
    pub fn release_all(&mut self, out: &mut VecDeque<AdapterEvent>) {
        for held in self.held.drain(..) {
            Self::flush(held, None, out);
        }
    }

    fn flush(held: HeldTool, mark: Option<&'static str>, out: &mut VecDeque<AdapterEvent>) {
        for mut event in held.events {
            if let AdapterEvent::BlockStarted { approved_by, .. } = &mut event {
                *approved_by = mark.map(str::to_owned);
            }
            out.push_back(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::BlockKind;

    fn started(block_id: Uuid) -> AdapterEvent {
        AdapterEvent::BlockStarted {
            block_id,
            kind: BlockKind::ToolCall,
            input_command_id: None,
            parent: None,
            agent: String::new(),
            model: String::new(),
            tool_name: None,
            summary: None,
            approved_by: None,
        }
    }

    fn delta(block_id: Uuid) -> AdapterEvent {
        AdapterEvent::BlockDelta {
            block_id,
            append: "x".into(),
        }
    }

    fn mark_of(event: &AdapterEvent) -> Option<&str> {
        match event {
            AdapterEvent::BlockStarted { approved_by, .. } => approved_by.as_deref(),
            _ => None,
        }
    }

    #[test]
    fn events_of_a_held_block_wait_and_others_pass() {
        let block = Uuid::now_v7();
        let mut held = HeldTools::default();
        held.hold("item".into(), block, started(block));
        assert!(held.capture(delta(block)).is_none());
        assert!(held.capture(delta(Uuid::now_v7())).is_some());
        assert_eq!(held.item_of_block(block), Some("item"));
        let mut out = VecDeque::new();
        held.release("other", Some("cli_policy"), &mut out);
        assert!(out.is_empty());
        held.release("item", Some("cli_policy"), &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(mark_of(&out[0]), Some("cli_policy"));
        assert!(matches!(out[1], AdapterEvent::BlockDelta { .. }));
        assert_eq!(held.item_of_block(block), None);
    }

    #[test]
    fn end_of_turn_releases_everything_without_a_mark() {
        let (first, second) = (Uuid::now_v7(), Uuid::now_v7());
        let mut held = HeldTools::default();
        held.hold("a".into(), first, started(first));
        held.hold("b".into(), second, started(second));
        let mut out = VecDeque::new();
        held.release_all(&mut out);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|event| mark_of(event).is_none()));
    }
}
