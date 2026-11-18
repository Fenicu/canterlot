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
        .contains(&(schedule_id, fault))
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

/// Управляемая остановка приёмки: заявка уже у хаба, а приёмка ещё не началась
/// (стоит в очереди на семафор). Тест держит затвор и отпускает его сам.
#[cfg(feature = "testkit")]
static STALLS: std::sync::Mutex<Vec<(Uuid, std::sync::Arc<tokio::sync::Notify>)>> =
    std::sync::Mutex::new(Vec::new());

/// Взвести остановку приёмки расписания; приёмка ждёт `notify_one` на затворе.
#[cfg(feature = "testkit")]
pub fn arm_stall(schedule_id: Uuid) -> std::sync::Arc<tokio::sync::Notify> {
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    STALLS
        .lock()
        .expect("stalls")
        .push((schedule_id, gate.clone()));
    gate
}

#[cfg(feature = "testkit")]
pub(crate) async fn stall(schedule_id: Uuid) {
    let gate = {
        let mut stalls = STALLS.lock().expect("stalls");
        stalls
            .iter()
            .position(|(id, _)| *id == schedule_id)
            .map(|index| stalls.remove(index).1)
    };
    if let Some(gate) = gate {
        gate.notified().await;
    }
}

#[cfg(not(feature = "testkit"))]
#[inline]
pub(crate) async fn stall(_schedule_id: Uuid) {}

#[cfg(not(feature = "testkit"))]
#[inline]
pub(crate) fn hit(_schedule_id: Uuid, _fault: Fault) -> bool {
    false
}
