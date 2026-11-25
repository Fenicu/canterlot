//! Группа `it_cloud`: облако и машины, раннеры и задачи, расписания и их прогоны, запуск
//! и управление исполнением.
//!
//! Модули остаются на своих местах в `tests/it/`, корень только объявляет их долю. Так правка
//! одного модуля пересобирает эту группу, а не общий стоблочный бинарник `it`.

#![forbid(unsafe_code)]

// Поддержка общая для всех бинарников: в этой группе часть её не нужна.
#[allow(dead_code)]
#[path = "it/support/mod.rs"]
mod support;

#[path = "it/auth_geo.rs"]
mod auth_geo;
#[path = "it/auth_pushes.rs"]
mod auth_pushes;
#[path = "it/cloud_budget.rs"]
mod cloud_budget;
#[path = "it/cloud_http.rs"]
mod cloud_http;
#[path = "it/cloud_windows.rs"]
mod cloud_windows;
#[path = "it/cloud_yandex.rs"]
mod cloud_yandex;
#[path = "it/control.rs"]
mod control;
#[path = "it/delete_running_job.rs"]
mod delete_running_job;
#[path = "it/dispatch.rs"]
mod dispatch;
#[path = "it/execution_control.rs"]
mod execution_control;
#[path = "it/executions.rs"]
mod executions;
#[path = "it/inplace_jobs.rs"]
mod inplace_jobs;
#[path = "it/machine_disk.rs"]
mod machine_disk;
#[path = "it/machine_info.rs"]
mod machine_info;
#[path = "it/machine_lease.rs"]
mod machine_lease;
#[path = "it/machine_outage.rs"]
mod machine_outage;
#[path = "it/machine_updates.rs"]
mod machine_updates;
#[path = "it/machines.rs"]
mod machines;
#[path = "it/remote_spawn.rs"]
mod remote_spawn;
#[path = "it/reserve_settings.rs"]
mod reserve_settings;
#[path = "it/routine_reports.rs"]
mod routine_reports;
#[path = "it/routing.rs"]
mod routing;
#[path = "it/runner_jobs.rs"]
mod runner_jobs;
#[path = "it/runners_grpc.rs"]
mod runners_grpc;
#[path = "it/runners_http.rs"]
mod runners_http;
#[path = "it/runners_hub.rs"]
mod runners_hub;
#[path = "it/schedule_failure_push.rs"]
mod schedule_failure_push;
#[path = "it/scheduled_runs.rs"]
mod scheduled_runs;
#[path = "it/scheduled_runs_pipeline.rs"]
mod scheduled_runs_pipeline;
#[path = "it/scheduler.rs"]
mod scheduler;
#[path = "it/spawn_queue.rs"]
mod spawn_queue;
#[path = "it/worker_terminal_outcomes.rs"]
mod worker_terminal_outcomes;
