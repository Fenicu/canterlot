pub mod auth;
pub mod fake_timeweb;
pub mod fake_yandex;
pub mod mmdb;
#[cfg(feature = "fault-injection")]
pub mod settings_process_guard;
pub mod task3;
pub mod task5;
pub mod task7;
pub mod task9;

/// Ждёт, пока фоновый проектор тестового хаба начнёт работу, и возвращает момент его
/// старта. Выборки метрик раньше него окно наблюдения не засчитывает, а выборка из будущего
/// относительно часов фонового пересчёта ему невидима и сбросила бы окно: поэтому тест берёт
/// отметки сразу после старта, но уже в прошлом.
pub async fn projector_running(pool: &sqlx::PgPool) -> chrono::DateTime<chrono::Utc> {
    let database = canterlot_hub::activity::database_of(pool);
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Some(started) = canterlot_hub::activity::projector_started(&database)
                && chrono::Utc::now() > started + chrono::TimeDelta::milliseconds(100)
            {
                return started;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the activity projector of the test hub must start")
}
