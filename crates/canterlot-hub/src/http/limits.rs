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
/// Сброс лимитов, подаренный провайдером (сбросы Codex): когда сгорит.
#[derive(Serialize, ToSchema)]
pub struct ResetView {
    /// Заголовок от провайдера; `null` — провайдер его не назвал.
    pub title: Option<String>,
    /// Когда сброс сгорит; `null` — не сгорает.
    pub expires_at: Option<String>,
}

/// Сбросы лимитов провайдера с самой свежей машины владельца.
#[derive(Serialize, ToSchema)]
pub struct CreditsView {
    /// Сколько сбросов доступно; `null` — провайдер их ещё не называл.
    pub reset_count: Option<u32>,
    /// Список может быть короче счёта: провайдер называет не все.
    pub resets: Vec<ResetView>,
    pub observed_at: String,
}

impl From<limits::Credits> for CreditsView {
    fn from(c: limits::Credits) -> Self {
        Self {
            reset_count: c.reset_count,
            resets: c
                .resets
                .into_iter()
                .map(|reset| ResetView {
                    title: (!reset.title.is_empty()).then_some(reset.title),
                    expires_at: reset.expires_at.map(time),
                })
                .collect(),
            observed_at: time(c.observed_at),
        }
    }
}
/// Вход провайдера на одной машине учётной записи.
#[derive(Serialize, ToSchema)]
pub struct MachineCredentialView {
    pub machine_id: uuid::Uuid,
    pub machine_name: String,
    /// `null` — дата неизвестна (API-ключ, старый CLI).
    pub expires_at: Option<String>,
}

/// Сбросы лимитов, о которых сообщила одна машина учётной записи.
#[derive(Serialize, ToSchema)]
pub struct MachineCreditsView {
    pub machine_id: uuid::Uuid,
    pub machine_name: String,
    #[serde(flatten)]
    pub credits: CreditsView,
}
#[derive(Serialize, ToSchema)]
pub struct ProviderView {
    pub provider: String,
    /// Учётная запись провайдера (`account` харнесса в worker.toml); `owner` по умолчанию.
    /// У провайдера на нескольких подписках строк по одной на каждую.
    pub account: String,
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
    /// Самый ранний конец входа провайдера на машинах этой учётной записи; `null` — дата
    /// неизвестна (API-ключ, старый CLI) или у провайдера такого входа нет. Сводное поле для
    /// клиентов, которые ещё не читают `machine_credentials`.
    pub credential_expires_at: Option<String>,
    /// Сбросы лимитов Codex с самой свежей машины учётной записи; `null` — провайдер о них
    /// молчит. Сводное поле для клиентов, которые ещё не читают `machine_credits`.
    pub credits: Option<CreditsView>,
    /// Вход по машинам: у каждой машины он свой и в одну дату не сливается.
    pub machine_credentials: Vec<MachineCredentialView>,
    /// Сбросы лимитов по машинам.
    pub machine_credits: Vec<MachineCreditsView>,
    /// Публичный статус провайдера со страницы statuspage; `null`, пока хаб её не опрашивал
    /// или провайдер без такой страницы (OpenCode).
    pub status: Option<crate::provider_status::ProviderStatusView>,
}
impl From<limits::ProviderLimits> for ProviderView {
    fn from(p: limits::ProviderLimits) -> Self {
        Self {
            provider: p.provider,
            account: p.account,
            weekly: p.weekly.map(window),
            five_hour: p.five_hour.map(window),
            monthly: p.monthly.map(window),
            weekly_fable: p.weekly_fable.map(window),
            weekly_reserve_active: p.weekly_reserve_active,
            weekly_reserve_resets_at: p.weekly_reserve_resets_at.map(time),
            five_hour_absent: p.five_hour_absent,
            data_confidence: p.data_confidence,
            credential_expires_at: p.credential_expires_at.map(time),
            credits: p.credits.map(CreditsView::from),
            machine_credentials: p
                .machine_credentials
                .into_iter()
                .map(|c| MachineCredentialView {
                    machine_id: c.machine_id,
                    machine_name: c.machine_name,
                    expires_at: c.expires_at.map(time),
                })
                .collect(),
            machine_credits: p
                .machine_credits
                .into_iter()
                .map(|c| MachineCreditsView {
                    machine_id: c.machine_id,
                    machine_name: c.machine_name,
                    credits: CreditsView::from(c.credits),
                })
                .collect(),
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
    /// Те же просьбы по учётным записям: у провайдера их может быть несколько.
    pub requested_accounts: Vec<RequestedAccount>,
    pub retry_after_secs: u64,
}
#[derive(Serialize, ToSchema)]
pub struct RequestedAccount {
    pub provider: String,
    pub account: String,
}
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HistoryParams {
    pub provider: Option<String>,
    /// Только эта учётная запись провайдера; без параметра — все.
    pub account: Option<String>,
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
    pub account: String,
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
        || p.account
            .as_deref()
            .is_some_and(|a| !canterlot_proto::limits::valid_account(a))
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
    // Ряды по парам (провайдер, учётная запись): подписки разных учётных записей не смешиваются.
    let pairs: Vec<(String, String)> = sqlx::query_as(
        "select distinct h.provider, h.account from machine_harnesses h join machines m on m.id=h.machine_id
          where m.owner_id=$1 and ($2::text is null or h.provider=$2) and ($3::text is null or h.account=$3)
          order by h.provider, h.account",
    )
    .bind(o.owner_id)
    .bind(p.provider.as_deref())
    .bind(p.account.as_deref())
    .fetch_all(&s.pool)
    .await?;
    let pairs = if pairs.is_empty() {
        // Запрошенный провайдер без харнесса в каталоге: ряд по `owner`, как раньше.
        p.provider
            .clone()
            .map(|provider| {
                vec![(
                    provider,
                    p.account
                        .clone()
                        .unwrap_or_else(crate::model::owner_account),
                )]
            })
            .unwrap_or_default()
    } else {
        pairs
    };
    let mut series = Vec::new();
    for (provider, account) in pairs {
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
        let five_hour = points(
            limits::series(
                &s.pool,
                &provider,
                &account,
                limits::Window::FiveHour,
                from,
                to,
            )
            .await?,
        );
        let weekly = points(
            limits::series(
                &s.pool,
                &provider,
                &account,
                limits::Window::Weekly,
                from,
                to,
            )
            .await?,
        );
        let weekly_fable = points(
            limits::series(
                &s.pool,
                &provider,
                &account,
                limits::Window::WeeklyFable,
                from,
                to,
            )
            .await?,
        );
        series.push(ProviderSeries {
            provider,
            account,
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
    let pairs: Vec<(String, String)> = sqlx::query_as(
        "select distinct provider, account from machine_harnesses where provider in ('claude','codex','opencode') order by provider, account",
    )
    .fetch_all(&s.pool)
    .await?;
    let now = Utc::now();
    let (mut requested, mut requested_accounts, mut throttled) = (Vec::new(), Vec::new(), false);
    for (provider, account) in pairs {
        match request_poll_outcome(
            &s.pool,
            &s.registry,
            &provider,
            &account,
            REFRESH_MIN_INTERVAL,
            true,
            now,
        )
        .await?
        {
            PollRequest::Sent => {
                if !requested.contains(&provider) {
                    requested.push(provider.clone());
                }
                requested_accounts.push(RequestedAccount { provider, account });
            }
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
            requested_accounts,
            retry_after_secs: REFRESH_MIN_INTERVAL.as_secs(),
        }),
    ))
}
