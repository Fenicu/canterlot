//! Группа `it_sessions`: сессии и разговоры, лента и приём, история и блоки, gRPC и WSS.
//!
//! Модули остаются на своих местах в `tests/it/`, корень только объявляет их долю. Так правка
//! одного модуля пересобирает эту группу, а не общий стоблочный бинарник `it`.

#![forbid(unsafe_code)]

// Поддержка общая для всех бинарников: в этой группе часть её не нужна.
#[allow(dead_code)]
#[path = "it/support/mod.rs"]
mod support;

#[path = "it/activity.rs"]
mod activity;
#[path = "it/blocks.rs"]
mod blocks;
#[path = "it/compaction.rs"]
mod compaction;
#[path = "it/composer_drafts.rs"]
mod composer_drafts;
#[path = "it/conversation_delete.rs"]
mod conversation_delete;
#[path = "it/conversation_trash.rs"]
mod conversation_trash;
#[path = "it/conversations_api.rs"]
mod conversations_api;
#[path = "it/derived.rs"]
mod derived;
#[path = "it/edit_input.rs"]
mod edit_input;
#[path = "it/event_schema.rs"]
mod event_schema;
#[path = "it/events.rs"]
mod events;
#[path = "it/forwarded_attachment_delete.rs"]
mod forwarded_attachment_delete;
#[path = "it/grpc_attachments.rs"]
mod grpc_attachments;
#[path = "it/grpc_session.rs"]
mod grpc_session;
#[path = "it/history.rs"]
mod history;
#[path = "it/history_search.rs"]
mod history_search;
#[path = "it/ingest.rs"]
mod ingest;
#[path = "it/session_accept.rs"]
mod session_accept;
#[path = "it/session_api.rs"]
mod session_api;
#[path = "it/session_domains.rs"]
mod session_domains;
#[path = "it/session_send.rs"]
mod session_send;
#[path = "it/session_spawn.rs"]
mod session_spawn;
#[path = "it/session_subs.rs"]
mod session_subs;
#[path = "it/session_summary.rs"]
mod session_summary;
#[path = "it/session_tools_read.rs"]
mod session_tools_read;
#[path = "it/session_usage.rs"]
mod session_usage;
#[path = "it/session_wait.rs"]
mod session_wait;
#[path = "it/side_chat.rs"]
mod side_chat;
#[path = "it/snapshot_boundary.rs"]
mod snapshot_boundary;
#[path = "it/submit.rs"]
mod submit;
#[path = "it/todos_integration.rs"]
mod todos_integration;
#[cfg(all(feature = "testkit", feature = "fault-injection"))]
#[path = "it/ws.rs"]
mod ws;
