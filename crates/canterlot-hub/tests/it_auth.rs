//! Группа `it_auth`: вход и доступ, учётные данные и секреты, настройки, лимиты и провайдеры,
//! поверхности владельца, память и ядро HTTP.
//!
//! Модули остаются на своих местах в `tests/it/`, корень только объявляет их долю. Так правка
//! одного модуля пересобирает эту группу, а не общий стоблочный бинарник `it`.

#![forbid(unsafe_code)]

// Поддержка общая для всех бинарников: в этой группе часть её не нужна.
#[allow(dead_code)]
#[path = "it/support/mod.rs"]
mod support;

#[path = "it/auth_devices.rs"]
mod auth_devices;
#[path = "it/auth_logs.rs"]
mod auth_logs;
#[path = "it/auth_passkeys.rs"]
mod auth_passkeys;
#[path = "it/auth_password.rs"]
mod auth_password;
#[path = "it/auth_qr.rs"]
mod auth_qr;
#[path = "it/auth_recovery.rs"]
mod auth_recovery;
#[path = "it/auth_sessions.rs"]
mod auth_sessions;
#[path = "it/auth_stepup.rs"]
mod auth_stepup;
#[path = "it/auth_ws.rs"]
mod auth_ws;
#[path = "it/bookmarks.rs"]
mod bookmarks;
#[path = "it/config.rs"]
mod config;
#[path = "it/contract_coverage.rs"]
mod contract_coverage;
#[path = "it/credential_pushes.rs"]
mod credential_pushes;
#[path = "it/credentials.rs"]
mod credentials;
#[path = "it/enroll.rs"]
mod enroll;
#[path = "it/goals.rs"]
mod goals;
#[path = "it/http_core.rs"]
mod http_core;
#[path = "it/http_requests.rs"]
mod http_requests;
#[path = "it/images.rs"]
mod images;
#[path = "it/in_tx.rs"]
mod in_tx;
#[path = "it/installation.rs"]
mod installation;
#[path = "it/limit_settings.rs"]
mod limit_settings;
#[path = "it/limits_absent.rs"]
mod limits_absent;
#[path = "it/limits_fable.rs"]
mod limits_fable;
#[path = "it/limits_forecast.rs"]
mod limits_forecast;
#[path = "it/limits_offered.rs"]
mod limits_offered;
#[path = "it/limits_refresh.rs"]
mod limits_refresh;
#[path = "it/limits_report.rs"]
mod limits_report;
#[path = "it/media.rs"]
mod media;
#[path = "it/memory_http.rs"]
mod memory_http;
#[path = "it/memory_import.rs"]
mod memory_import;
#[path = "it/memory_scope.rs"]
mod memory_scope;
#[path = "it/memory_settings.rs"]
mod memory_settings;
#[path = "it/memory_tools.rs"]
mod memory_tools;
#[path = "it/message_snippets.rs"]
mod message_snippets;
#[path = "it/model_resolution.rs"]
mod model_resolution;
#[path = "it/notifications_muted.rs"]
mod notifications_muted;
#[path = "it/owner_inputs.rs"]
mod owner_inputs;
#[path = "it/owner_instructions.rs"]
mod owner_instructions;
#[path = "it/owner_push.rs"]
mod owner_push;
#[path = "it/owner_reclaim.rs"]
mod owner_reclaim;
#[path = "it/presence.rs"]
mod presence;
#[path = "it/presets.rs"]
mod presets;
#[path = "it/prompt_suggestions.rs"]
mod prompt_suggestions;
#[path = "it/provider_credits.rs"]
mod provider_credits;
#[path = "it/provider_status.rs"]
mod provider_status;
#[path = "it/requests_domain.rs"]
mod requests_domain;
#[path = "it/requests_integration.rs"]
mod requests_integration;
#[path = "it/schema.rs"]
mod schema;
#[path = "it/secret_requests.rs"]
mod secret_requests;
#[path = "it/security.rs"]
mod security;
#[path = "it/service_notes.rs"]
mod service_notes;
#[path = "it/settings.rs"]
mod settings;
#[path = "it/settings_change.rs"]
mod settings_change;
#[path = "it/updates_e2e.rs"]
mod updates_e2e;
#[path = "it/version.rs"]
mod version;
