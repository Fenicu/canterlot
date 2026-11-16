//! Кто из устройств владельца сейчас смотрит чат сессии.
//!
//! PWA присылает отметку «чат открыт и виден» и повторяет её, пока экран виден;
//! отметка живёт [`TTL`] и гаснет сама, если страница замерла или её убили.
//! Карта держится в памяти процесса: это подсказка для отправки пушей, а не данные,
//! которые должны пережить перезапуск хаба.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::time::Instant;
use uuid::Uuid;

/// Сколько живёт одна отметка. PWA повторяет её каждые 20 секунд.
pub const TTL: Duration = Duration::from_secs(45);

/// `conversation_id → (ключ устройства → до какого времени видно)`.
#[derive(Clone, Default)]
pub struct Presence {
    entries: Arc<Mutex<HashMap<Uuid, HashMap<String, Instant>>>>,
}

impl Presence {
    /// Отметка устройства: видимая продлевает жизнь записи, скрытая убирает её сразу.
    pub fn report(&self, conversation_id: Uuid, device: &str, visible: bool, now: Instant) {
        let mut entries = self.lock();
        if visible {
            entries
                .entry(conversation_id)
                .or_default()
                .insert(device.to_owned(), now + TTL);
        } else if let Some(devices) = entries.get_mut(&conversation_id) {
            devices.remove(device);
            if devices.is_empty() {
                entries.remove(&conversation_id);
            }
        }
        prune(&mut entries, now);
    }

    /// Чат виден, если хотя бы одно устройство отметилось не раньше `TTL` назад.
    pub fn is_visible(&self, conversation_id: Uuid, now: Instant) -> bool {
        let mut entries = self.lock();
        prune(&mut entries, now);
        entries
            .get(&conversation_id)
            .is_some_and(|devices| devices.values().any(|until| *until > now))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, HashMap<String, Instant>>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn prune(entries: &mut HashMap<Uuid, HashMap<String, Instant>>, now: Instant) {
    entries.retain(|_, devices| {
        devices.retain(|_, until| *until > now);
        !devices.is_empty()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> Uuid {
        Uuid::now_v7()
    }

    #[test]
    fn a_visible_chat_is_seen_until_exactly_its_ttl() {
        let presence = Presence::default();
        let conversation = id();
        let now = Instant::now();
        presence.report(conversation, "phone", true, now);
        assert!(presence.is_visible(conversation, now));
        assert!(presence.is_visible(conversation, now + TTL - Duration::from_secs(1)));
        assert!(!presence.is_visible(conversation, now + TTL));
    }

    #[test]
    fn a_hidden_device_stops_mattering_but_another_device_keeps_the_chat_visible() {
        let presence = Presence::default();
        let conversation = id();
        let now = Instant::now();
        presence.report(conversation, "phone", true, now);
        presence.report(conversation, "laptop", true, now);
        presence.report(conversation, "phone", false, now);
        assert!(presence.is_visible(conversation, now));
        presence.report(conversation, "laptop", false, now);
        assert!(!presence.is_visible(conversation, now));
    }

    #[test]
    fn presence_of_one_chat_does_not_leak_to_another() {
        let presence = Presence::default();
        let visible = id();
        let other = id();
        let now = Instant::now();
        presence.report(visible, "phone", true, now);
        assert!(!presence.is_visible(other, now));
    }
}
