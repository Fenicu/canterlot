//! Точки отказа для тестов приёма: имитация гибели процесса вокруг commit dispatch.
//! Без фичи `testkit` все проверки возвращают `false` и ничего не стоят.

use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Гибель до commit dispatch: транзакция откатывается, строка остаётся `claimed`.
    BeforeDispatchCommit,
    /// Гибель после commit dispatch, до ответа клиенту «Запустить сейчас».
    LostRunNowResponse,
    /// Сбой базы в начале dispatch, уже после предпроверки.
    DispatchBeginError,
    /// Выбранная маршрутом пара оказывается занятой уже под замками dispatch.
    BusyAfterRouting,
}

#[cfg(feature = "testkit")]
static ARMED: std::sync::Mutex<Vec<(Uuid, Fault)>> = std::sync::Mutex::new(Vec::new());

/// Взвести точку для расписания: сработает один раз.
#[cfg(feature = "testkit")]
pub fn arm(schedule_id: Uuid, fault: Fault) {
    ARMED.lock().expect("faults").push((schedule_id, fault));
}

/// Осталась ли точка взведённой (ещё не сработала).
#[cfg(feature = "testkit")]
pub fn armed(schedule_id: Uuid, fault: Fault) -> bool {
    ARMED
        .lock()
        .expect("faults")
        .iter()
        .any(|entry| *entry == (schedule_id, fault))
}

#[cfg(feature = "testkit")]
pub(crate) fn hit(schedule_id: Uuid, fault: Fault) -> bool {
    let mut armed = ARMED.lock().expect("faults");
    match armed
        .iter()
        .position(|entry| *entry == (schedule_id, fault))
    {
        Some(index) => {
            armed.remove(index);
            true
        }
        None => false,
    }
}

#[cfg(not(feature = "testkit"))]
#[inline]
pub(crate) fn hit(_schedule_id: Uuid, _fault: Fault) -> bool {
    false
}
