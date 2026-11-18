//! Приём и отправка заявленных запусков.

use crate::{
    http::AppState,
    schedules::{ScheduleError, ScheduleRun},
};
use uuid::Uuid;

/// Обработка одного `claimed`-запуска вне тика.
pub async fn admit(_scheduler: &crate::scheduler::Scheduler, _run_id: Uuid) {}

/// «Запустить сейчас»: same admission and dispatch, под ключом клиента.
pub async fn run_now(
    _state: &AppState,
    _owner_id: Uuid,
    _schedule_id: Uuid,
    _key: &str,
    _run_anyway: bool,
    _correlation_id: Uuid,
) -> Result<ScheduleRun, ScheduleError> {
    Err(ScheduleError::NotFound)
}
