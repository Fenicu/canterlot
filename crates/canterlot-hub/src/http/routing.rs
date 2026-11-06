//! Политика маршрутизации владельца: чтение, правка и предпросмотр выбора.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::{ApiError, ErrorCode, ErrorDetails},
    extract::ApiJson,
};
use crate::{
    events::{self, NewEvent, Scope},
    idempotency,
    session_tools::routing::{self, Considered, Decision, MachineHeadroom, Policy},
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(read, replace))
        .routes(routes!(preview))
}

/// Политика владельца: `null` — встроенная; ответ всегда несёт и встроенную.
#[derive(Serialize, ToSchema)]
pub struct RoutingPolicyView {
    pub policy: Option<Policy>,
    pub revision: i64,
    pub builtin: Policy,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutingPolicyBody {
    /// `null` сбрасывает политику ко встроенной.
    #[schema(required = true)]
    pub policy: Option<Policy>,
    pub revision: i64,
}

fn policy_view(stored: Option<Policy>, revision: i64) -> RoutingPolicyView {
    RoutingPolicyView {
        policy: stored,
        revision,
        builtin: routing::builtin(),
    }
}

#[utoipa::path(get,path="/api/v1/routing-policy",tag="routing",responses((status=200,description="Политика маршрутизации владельца",body=RoutingPolicyView)))]
async fn read(State(s): State<AppState>, o: Owner) -> Result<Json<RoutingPolicyView>, ApiError> {
    let mut conn = s.pool.acquire().await?;
    let stored: Option<serde_json::Value> =
        sqlx::query_scalar("select routing_policy from owners where id=$1")
            .bind(o.owner_id)
            .fetch_one(&mut *conn)
            .await?;
    let revision: i64 =
        sqlx::query_scalar("select routing_policy_revision from owners where id=$1")
            .bind(o.owner_id)
            .fetch_one(&mut *conn)
            .await?;
    let policy = match stored {
        Some(value) => {
            Some(serde_json::from_value(value).map_err(|_| ApiError::new(ErrorCode::Internal))?)
        }
        None => None,
    };
    Ok(Json(policy_view(policy, revision)))
}

#[utoipa::path(put,path="/api/v1/routing-policy",tag="routing",params(("Idempotency-Key"=String,Header)),request_body=RoutingPolicyBody,responses((status=200,description="Политика сохранена",body=RoutingPolicyView),(status=409,description="Устаревшая ревизия",body=super::error::ErrorEnvelope)))]
async fn replace(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
    ApiJson(b): ApiJson<RoutingPolicyBody>,
) -> Result<Json<RoutingPolicyView>, ApiError> {
    if let Some(policy) = b.policy.as_ref()
        && let Err(message) = routing::validate(policy)
    {
        return Err(
            ApiError::new(ErrorCode::RoutingPolicyInvalid).with_details(ErrorDetails {
                reason: Some(message),
                ..Default::default()
            }),
        );
    }
    let owner = o.owner_id;
    let policy = b.policy.clone();
    let revision = b.revision;
    let outcome = idempotency::run_in_tx(
        &s.pool,
        owner,
        &key,
        "put_routing_policy",
        &b,
        None,
        move |conn| {
            let policy = policy.clone();
            Box::pin(async move {
                let current: i64 =
                    sqlx::query_scalar("select routing_policy_revision from owners where id=$1")
                        .bind(owner)
                        .fetch_one(&mut *conn)
                        .await?;
                if current != revision {
                    return Err(ApiError::new(ErrorCode::RoutingPolicyConflict));
                }
                let next = routing::save_policy(conn, owner, policy.as_ref())
                    .await
                    .map_err(|message| {
                        ApiError::new(ErrorCode::RoutingPolicyInvalid).with_details(ErrorDetails {
                            reason: Some(message),
                            ..Default::default()
                        })
                    })?;
                events::append(
                    conn,
                    NewEvent {
                        scope: Scope::Conversations { owner_id: owner },
                        kind: "routing_policy_changed",
                        conversation_id: None,
                        execution_id: None,
                        payload: serde_json::json!({"revision": next}),
                        correlation_id: correlation,
                    },
                )
                .await?;
                Ok::<_, ApiError>(next)
            })
        },
    )
    .await?;
    let mut conn = s.pool.acquire().await?;
    let stored: Option<serde_json::Value> =
        sqlx::query_scalar("select routing_policy from owners where id=$1")
            .bind(owner)
            .fetch_one(&mut *conn)
            .await?;
    let policy = match stored {
        Some(value) => {
            Some(serde_json::from_value(value).map_err(|_| ApiError::new(ErrorCode::Internal))?)
        }
        None => None,
    };
    Ok(Json(policy_view(policy, outcome.value)))
}

/// Предпросмотр выбора для типа задачи; `workdir` переопределяет каталог родителя.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutingPreviewBody {
    pub task_type: String,
    pub level: Option<String>,
    pub workdir: Option<String>,
}

/// Решение маршрута и запас машины; та же форма, что у `route_preview`.
#[derive(Serialize, ToSchema)]
pub struct RoutingPreviewView {
    pub decision: Decision,
    pub machine: MachineHeadroom,
    pub guard: crate::session_tools::guard::GuardVerdict,
    pub considered: Vec<Considered>,
}

#[utoipa::path(post,path="/api/v1/routing-policy/preview",tag="routing",request_body=RoutingPreviewBody,responses((status=200,description="Выбор маршрута и запас машины",body=RoutingPreviewView)))]
async fn preview(
    State(s): State<AppState>,
    o: Owner,
    ApiJson(b): ApiJson<RoutingPreviewBody>,
) -> Result<Json<RoutingPreviewView>, ApiError> {
    if b.task_type.trim().is_empty()
        || b.task_type.contains('\0')
        || b.level
            .as_deref()
            .is_some_and(|level| !routing::LEVELS.contains(&level))
    {
        return Err(ApiError::new(ErrorCode::InvalidRequest));
    }
    let mut conn = s.pool.acquire().await?;
    let (policy, revision) = routing::load_policy(&mut conn, o.owner_id).await?;
    if !policy.task_types.contains_key(&b.task_type) {
        let mut types: Vec<&str> = policy.task_types.keys().map(String::as_str).collect();
        types.sort_unstable();
        return Err(ApiError::new(ErrorCode::TaskTypeUnknown));
    }
    // Машина вызова: последняя активная сессия владельца, затем свежие метрики.
    let machine: Option<Uuid> = sqlx::query_scalar(
        "select machine_id from conversations where owner_id=$1
          order by last_activity_at desc nulls last, created_at desc limit 1",
    )
    .bind(o.owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    let machine: Option<Uuid> = match machine {
        Some(machine) => Some(machine),
        None => {
            sqlx::query_scalar(
                "select machine_id from machine_metrics order by observed_at desc limit 1",
            )
            .fetch_optional(&mut *conn)
            .await?
        }
    };
    let Some(machine) = machine else {
        return Err(ApiError::new(ErrorCode::MachineUnavailable));
    };
    let catalogs = routing::load_catalogs(&mut conn, machine).await?;
    let list = crate::session_tools::whitelist::view(&mut conn, o.owner_id).await?;
    let limits = routing::limits_view(&mut conn).await?;
    let live = routing::live_counts(&mut conn, o.owner_id).await?;
    let workdir = match b.workdir {
        Some(workdir) => workdir,
        None => sqlx::query_scalar(
            "select workdir from conversations where id in (
                select id from conversations where owner_id=$1
                order by last_activity_at desc nulls last limit 1)",
        )
        .bind(o.owner_id)
        .fetch_optional(&mut *conn)
        .await?
        .unwrap_or_default(),
    };
    let parent: Option<(String, String, Option<String>)> = sqlx::query_as(
        "select harness,permission_mode,pending_settings->>'permission_mode' as pending_mode
          from conversations where owner_id=$1
          order by last_activity_at desc nulls last limit 1",
    )
    .bind(o.owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    let (parent_harness, parent_mode, pending_mode) =
        parent.unwrap_or_else(|| ("opencode".to_owned(), "owner".to_owned(), None));
    let request = routing::RouteRequest {
        task_type: b.task_type.clone(),
        level: b.level.clone(),
        parent_harness,
        parent_mode: crate::session_tools::spawn::parent_mode(
            &parent_mode,
            pending_mode.as_deref(),
        )
        .to_owned(),
        explicit_mode: None,
        workdir,
    };
    let now = chrono::Utc::now();
    let headroom =
        routing::machine_headroom(&mut conn, machine, std::time::Duration::from_secs(120)).await?;
    let guard =
        crate::session_tools::guard::check_in_tx(&mut conn, o.owner_id, machine, &s.guards, now)
            .await?;
    match routing::route(routing::RouteInputs {
        policy: &policy,
        revision,
        whitelist: &list.entries,
        catalogs: &catalogs,
        limits: &limits,
        live: &live,
        stale: s.routing_stale,
        now,
        request: &request,
        bypass_limits: false,
    }) {
        Ok(decision) => {
            let considered = decision.considered.clone();
            Ok(Json(RoutingPreviewView {
                decision,
                machine: headroom,
                guard,
                considered,
            }))
        }
        Err(routing::RouteError::UnknownTask { .. }) => {
            Err(ApiError::new(ErrorCode::TaskTypeUnknown))
        }
        Err(routing::RouteError::NoExecutor { considered, reset }) => Err(ApiError::new(
            ErrorCode::NoExecutorAvailable,
        )
        .with_details(ErrorDetails {
            reason: Some(routing::no_executor_message(&considered, reset)),
            ..Default::default()
        })),
    }
}
