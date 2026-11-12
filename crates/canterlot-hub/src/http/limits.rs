use super::{
    AppState,
    auth::Owner,
    error::{ApiError, ErrorCode},
    extract::ApiQuery,
};
use crate::{conversations::time, limits};
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(current))
        .routes(routes!(history))
        .routes(routes!(refresh))
}
/// Прогноз окна: средний и недавний темп, ожидаемое исчерпание.
#[derive(Serialize, ToSchema)]
pub struct ForecastView {
    /// Средний темп по приросту расхода между снимками окна, промилле в час.
    pub rate_permille_per_hour: i64,
    /// Темп за последний час (пятичасовое окно) или шесть часов (неделя, месяц).
    pub recent_rate_permille_per_hour: Option<i64>,
    /// Когда окно кончится при среднем темпе; `null`, если темп нулевой.
    pub exhausts_at: Option<String>,
    /// Исчерпание наступит раньше сброса окна.
    pub before_reset: bool,
}
impl From<limits::Forecast> for ForecastView {
    fn from(f: limits::Forecast) -> Self {
        Self {
            rate_permille_per_hour: f.rate_permille_per_hour.round() as i64,
            recent_rate_permille_per_hour: f
                .recent_rate_permille_per_hour
                .map(|rate| rate.round() as i64),
            exhausts_at: f.exhausts_at.map(time),
            before_reset: f.before_reset,
        }
    }
}
#[derive(Serialize, ToSchema)]
pub struct WindowView {
    pub used_permille: u16,
    pub used_pct: f64,
    pub resets_at: Option<String>,
    pub observed_at: String,
    /// Прогноз по темпу; поля нет, когда данных мало.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forecast: Option<ForecastView>,
}
fn window(w: limits::WindowState) -> WindowView {
    WindowView {
        used_permille: w.used_permille,
        used_pct: f64::from(w.used_permille) / 10.0,
        resets_at: w.resets_at.map(time),
        observed_at: time(w.observed_at),
        forecast: w.forecast.map(ForecastView::from),
    }
}
#[derive(Serialize, ToSchema)]
pub struct ProviderView {
    pub provider: String,
    pub weekly: Option<WindowView>,
    pub five_hour: Option<WindowView>,
    pub monthly: Option<WindowView>,
    pub weekly_fable: Option<WindowView>,
    pub weekly_reserve_active: bool,
    /// Сброс недели, в окне которой включился резерв.
    pub weekly_reserve_resets_at: Option<String>,
    /// У провайдера нет пятичасового окна (например, план без этого лимита): счётчик скрывается.
    pub five_hour_absent: bool,
    /// `estimated`, когда CodexBar вывел расход из локальных журналов, иначе `exact`.
    pub data_confidence: limits::DataConfidence,
    /// Публичный статус провайдера со страницы statuspage; `null`, пока хаб её не опрашивал
    /// или провайдер без такой страницы (OpenCode, Antigravity).
    pub status: Option<crate::provider_status::ProviderStatusView>,
}
impl From<limits::ProviderLimits> for ProviderView {
    fn from(p: limits::ProviderLimits) -> Self {
        Self {
            provider: p.provider,
            weekly: p.weekly.map(window),
            five_hour: p.five_hour.map(window),
            monthly: p.monthly.map(window),
            weekly_fable: p.weekly_fable.map(window),
            weekly_reserve_active: p.weekly_reserve_active,
            weekly_reserve_resets_at: p.weekly_reserve_resets_at.map(time),
            five_hour_absent: p.five_hour_absent,
            data_confidence: p.data_confidence,
            status: p
                .status
                .as_ref()
                .map(crate::provider_status::ProviderStatusView::from),
        }
    }
}
/// Раньше этого срока повторную просьбу об опросе лимитов провайдера хаб не отправляет.
const REFRESH_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Serialize, ToSchema)]
pub struct RefreshView {
    /// Провайдеры, чьим машинам отправлена просьба об опросе.
    pub requested: Vec<String>,
    pub retry_after_secs: u64,
}
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HistoryParams {
    pub provider: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub tz: Option<String>,
}
#[derive(Serialize, ToSchema)]
pub struct Point {
    pub observed_at: String,
    pub used_permille: u16,
    pub used_pct: f64,
}
#[derive(Serialize, ToSchema)]
pub struct ProviderSeries {
    pub provider: String,
    pub five_hour: Vec<Point>,
    pub weekly: Vec<Point>,
    pub weekly_fable: Vec<Point>,
}
#[derive(Serialize, ToSchema)]
pub struct History {
    pub usage: Vec<limits::UsageDay>,
    pub series: Vec<ProviderSeries>,
    /// Медианы скорости ответа по моделям; Codex не передаёт времени генерации и сюда не попадает.
    pub speed: Vec<crate::speed::ModelSpeedSummary>,
}
#[utoipa::path(get,path="/api/v1/limits",tag="limits",responses((status=200,description="Текущие лимиты провайдеров, чьи харнессы есть в каталоге машин владельца",body=Vec<ProviderView>)))]
async fn current(State(s): State<AppState>, o: Owner) -> Result<Json<Vec<ProviderView>>, ApiError> {
    Ok(Json(
        limits::shown(&mut *s.pool.acquire().await?, o.owner_id)
            .await?
            .into_iter()
            .map(ProviderView::from)
            .collect(),
    ))
}
#[utoipa::path(get,path="/api/v1/limits/history",tag="limits",params(HistoryParams),responses((status=200,description="Расход и история лимитов",body=History)))]
async fn history(
    State(s): State<AppState>,
    o: Owner,
    ApiQuery(p): ApiQuery<HistoryParams>,
) -> Result<Json<History>, ApiError> {
    let to = p.to.unwrap_or_else(Utc::now);
    let from = p
        .from
        .unwrap_or(to - TimeDelta::days(i64::from(s.limits.history_days)));
    if from >= to
        || to - from > TimeDelta::days(366)
        || p.provider
            .as_ref()
            .is_some_and(|p| !limits::known_provider(p))
    {
        return Err(ApiError::new(ErrorCode::InvalidRequest));
    }
    let usage = limits::usage_by_day(
        &s.pool,
        o.owner_id,
        p.provider.as_deref(),
        from,
        to,
        p.tz.as_deref().unwrap_or("UTC"),
    )
    .await
    .map_err(super::conversations::query_error)?;
    let speed = crate::speed::summary(
        &mut *s.pool.acquire().await?,
        o.owner_id,
        p.provider.as_deref(),
        from,
        to,
        p.tz.as_deref().unwrap_or("UTC"),
    )
    .await?;
    let providers = if let Some(provider) = p.provider {
        vec![provider]
    } else {
        sqlx::query_scalar("select distinct provider from machine_harnesses h join machines m on m.id=h.machine_id where m.owner_id=$1 order by provider").bind(o.owner_id).fetch_all(&s.pool).await?
    };
    let mut series = Vec::new();
    for provider in providers {
        let points = |items: Vec<(DateTime<Utc>, u16)>| {
            items
                .into_iter()
                .map(|(at, used)| Point {
                    observed_at: time(at),
                    used_permille: used,
                    used_pct: f64::from(used) / 10.0,
                })
                .collect()
        };
        let five_hour =
            points(limits::series(&s.pool, &provider, limits::Window::FiveHour, from, to).await?);
        let weekly =
            points(limits::series(&s.pool, &provider, limits::Window::Weekly, from, to).await?);
        let weekly_fable = points(
            limits::series(&s.pool, &provider, limits::Window::WeeklyFable, from, to).await?,
        );
        series.push(ProviderSeries {
            provider,
            five_hour,
            weekly,
            weekly_fable,
        });
    }
    Ok(Json(History {
        usage,
        series,
        speed,
    }))
}
#[utoipa::path(post,path="/api/v1/limits/refresh",tag="limits",responses((status=202,description="Просьба об опросе отправлена воркерам; новые показания придут событием limits_changed",body=RefreshView),(status=429,description="Опрос просили слишком недавно",body=super::error::ErrorEnvelope),(status=409,description="Нет доступного воркера с лимитами",body=super::error::ErrorEnvelope)))]
async fn refresh(
    State(s): State<AppState>,
    _o: Owner,
) -> Result<(StatusCode, Json<RefreshView>), ApiError> {
    use crate::crunch::{PollRequest, request_poll_outcome};
    let providers: Vec<String> = sqlx::query_scalar(
        "select distinct provider from machine_harnesses where provider in ('claude','codex','opencode','antigravity') order by provider",
    )
    .fetch_all(&s.pool)
    .await?;
    let now = Utc::now();
    let (mut requested, mut throttled) = (Vec::new(), false);
    for provider in providers {
        match request_poll_outcome(
            &s.pool,
            &s.registry,
            &provider,
            REFRESH_MIN_INTERVAL,
            true,
            now,
        )
        .await?
        {
            PollRequest::Sent => requested.push(provider),
            PollRequest::Throttled => throttled = true,
            PollRequest::NoMachine => {}
        }
    }
    if requested.is_empty() {
        return Err(ApiError::new(if throttled {
            ErrorCode::TooManyRequests
        } else {
            ErrorCode::MachineUnavailable
        }));
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(RefreshView {
            requested,
            retry_after_secs: REFRESH_MIN_INTERVAL.as_secs(),
        }),
    ))
}
