//! Маршрутизация: выбор исполнителя по политике владельца и текущим лимитам (C1).
//!
//! Чистая функция `route` идёт по кандидатам типа задачи по порядку и возвращает
//! первого прошедшего все фильтры (§5.2): уровень, канонизация через каталог машины
//! родителя, белый список, совместимость режима, лимиты провайдера (с резервом,
//! паузой и несвежими показаниями), параллельность, `exclude_workdirs`.
//! Никто не прошёл — ошибка `no_executor_available` с причинами по каждому
//! кандидату. Очередь запусков — C2: шов `queue_reason` уже здесь, но C1 всегда
//! отвечает ошибкой.

use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    Caller, ToolContext, ToolError, ToolErrorCode,
    whitelist::{self, WhitelistEntry},
};
use crate::limits_forecast::Forecast;

/// Имя встроенного пресета исполнителя: ищется по названию среди пресетов
/// владельца с назначением `instructions` (если нет — без пресета).
pub const EXECUTOR_PRESET_TITLE: &str = "Исполнитель (английский)";

const MAX_TASK_TYPES: usize = 32;
const MAX_CANDIDATES: usize = 8;
const MAX_KEY_CHARS: usize = 32;
const MAX_EXCLUDE_WORKDIRS: usize = 16;
pub(crate) const LEVELS: [&str; 4] = ["low", "medium", "high", "xhigh"];

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_CHARS
        && key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

// ---------------------------------------------------------------------------
// Политика
// ---------------------------------------------------------------------------

/// Пороги провайдера в процентах расхода.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ProviderThresholds {
    pub max_weekly_percent: Option<f64>,
    pub max_five_hour_percent: Option<f64>,
    pub max_monthly_percent: Option<f64>,
}

/// Кандидат типа задачи.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub harness: String,
    pub model: String,
    pub effort: Option<String>,
    pub levels: Option<Vec<String>>,
    pub preset: Option<String>,
    pub nudge: Option<Value>,
    #[serde(default)]
    pub exclude_workdirs: Vec<String>,
    pub watchdog_minutes: Option<i64>,
}

pub const MIN_EXPECTED_MINUTES: i64 = 5;
pub const MAX_EXPECTED_MINUTES: i64 = 1440;
pub const DEFAULT_EXPECTED_MINUTES: i64 = 60;

fn default_expected_minutes() -> i64 {
    DEFAULT_EXPECTED_MINUTES
}

/// Тип задачи: русское название для PWA и упорядоченные кандидаты.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskType {
    pub title: String,
    pub candidates: Vec<Candidate>,
    pub preset: Option<String>,
    pub isolated: Option<bool>,
    /// Ожидаемая длительность задачи в минутах: по ней прогноз решает, хватит ли лимита.
    #[serde(default = "default_expected_minutes")]
    pub expected_minutes: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnknownLimits {
    #[default]
    Allow,
    Skip,
}

/// Политика маршрутизации владельца. `None` в базе — встроенная политика.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Policy {
    pub task_types: BTreeMap<String, TaskType>,
    pub providers: BTreeMap<String, ProviderThresholds>,
    pub unlimited_models: Vec<String>,
    pub max_parallel: BTreeMap<String, i64>,
    pub unknown_limits: UnknownLimits,
}

fn harness_known(harness: &str) -> bool {
    canterlot_proto::harness::provider_of(harness).is_some()
}

fn check_percent(value: Option<f64>, what: &str) -> Result<(), String> {
    if let Some(percent) = value
        && !(0.0..=100.0).contains(&percent)
    {
        return Err(format!("{what} is out of range 0..=100"));
    }
    Ok(())
}

/// Проверка политики при сохранении (§5.6): известные харнессы, до 32 типов,
/// до 8 кандидатов, ключи `[a-z0-9_]{1,32}`. Пары вне белого списка сохраняются:
/// маршрут их пропустит.
pub fn validate(policy: &Policy) -> Result<(), String> {
    if policy.task_types.len() > MAX_TASK_TYPES {
        return Err(format!(
            "too many task types: {} > {MAX_TASK_TYPES}",
            policy.task_types.len()
        ));
    }
    for (key, task) in &policy.task_types {
        if !valid_key(key) {
            return Err(format!("bad task type key: {key:?}"));
        }
        if task.title.trim().is_empty() {
            return Err(format!("task type {key:?} needs a title"));
        }
        if !(MIN_EXPECTED_MINUTES..=MAX_EXPECTED_MINUTES).contains(&task.expected_minutes) {
            return Err(format!(
                "task type {key:?}: expected_minutes is out of range {MIN_EXPECTED_MINUTES}..={MAX_EXPECTED_MINUTES}"
            ));
        }
        if task.candidates.is_empty() || task.candidates.len() > MAX_CANDIDATES {
            return Err(format!(
                "task type {key:?} needs 1..={MAX_CANDIDATES} candidates"
            ));
        }
        for candidate in &task.candidates {
            validate_candidate(key, candidate)?;
        }
    }
    for (provider, thresholds) in &policy.providers {
        if !canterlot_proto::harness::harnesses_of(provider).is_empty()
            || matches!(provider.as_str(), "claude" | "codex" | "opencode")
        {
            // известный провайдер
        } else {
            return Err(format!("unknown provider: {provider:?}"));
        }
        check_percent(
            thresholds.max_weekly_percent,
            &format!("providers.{provider}.max_weekly_percent"),
        )?;
        check_percent(
            thresholds.max_five_hour_percent,
            &format!("providers.{provider}.max_five_hour_percent"),
        )?;
        check_percent(
            thresholds.max_monthly_percent,
            &format!("providers.{provider}.max_monthly_percent"),
        )?;
    }
    for (pair, limit) in &policy.max_parallel {
        if *limit < 1 {
            return Err(format!("max_parallel[{pair:?}] must be at least 1"));
        }
    }
    Ok(())
}

fn validate_candidate(task_key: &str, candidate: &Candidate) -> Result<(), String> {
    let what = format!("task type {task_key:?} candidate");
    if !harness_known(&candidate.harness) {
        return Err(format!("{what}: unknown harness {:?}", candidate.harness));
    }
    if candidate.model.trim().is_empty() || candidate.model.contains('\0') {
        return Err(format!("{what}: bad model"));
    }
    if let Some(effort) = &candidate.effort
        && (effort.trim().is_empty() || effort.contains('\0'))
    {
        return Err(format!("{what}: bad effort"));
    }
    if let Some(levels) = &candidate.levels {
        if levels.is_empty() {
            return Err(format!("{what}: empty levels"));
        }
        for level in levels {
            if !LEVELS.contains(&level.as_str()) {
                return Err(format!("{what}: bad level {level:?}"));
            }
        }
    }
    if let Some(preset) = &candidate.preset
        && (preset.trim().is_empty() || preset.contains('\0'))
    {
        return Err(format!("{what}: bad preset"));
    }
    if let Some(watchdog) = candidate.watchdog_minutes
        && !(5..=1440).contains(&watchdog)
    {
        return Err(format!("{what}: watchdog_minutes is out of range 5..=1440"));
    }
    if candidate.exclude_workdirs.len() > MAX_EXCLUDE_WORKDIRS {
        return Err(format!(
            "{what}: too many exclude_workdirs (max {MAX_EXCLUDE_WORKDIRS})"
        ));
    }
    for prefix in &candidate.exclude_workdirs {
        if !prefix.starts_with('/') || prefix.contains('\0') {
            return Err(format!("{what}: bad exclude_workdirs entry {prefix:?}"));
        }
    }
    super::nudge::validate_spawn_arg(&candidate.nudge).map_err(|_| format!("{what}: bad nudge"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Встроенная политика
// ---------------------------------------------------------------------------

fn candidate(
    harness: &str,
    model: &str,
    effort: Option<&str>,
    nudge: Value,
    watchdog_minutes: Option<i64>,
    exclude_work: bool,
) -> Candidate {
    Candidate {
        harness: harness.to_owned(),
        model: model.to_owned(),
        effort: effort.map(str::to_owned),
        levels: None,
        preset: None,
        nudge: Some(nudge),
        exclude_workdirs: if exclude_work {
            vec!["/home/fenicu/work".to_owned()]
        } else {
            Vec::new()
        },
        watchdog_minutes,
    }
}

/// Встроенная политика (§C1 п. 4): раскладка из опыта 02.11, модели как сегодня.
pub fn builtin() -> Policy {
    use serde_json::json;
    let deepseek_high_90 = || {
        candidate(
            "opencode",
            "opencode-go/deepseek-v4.1-flash",
            Some("high"),
            json!({"max_nudges": 2}),
            Some(90),
            false,
        )
    };
    let deepseek_high_120 = || {
        candidate(
            "opencode",
            "opencode-go/deepseek-v4.1-flash",
            Some("high"),
            json!({"max_nudges": 2}),
            Some(120),
            false,
        )
    };
    let muse_nudge = |max_nudges: i64, watchdog: i64| {
        candidate(
            "opencode",
            "opencode-go/muse-spark-1.3-contributor",
            None,
            json!({"max_nudges": max_nudges}),
            Some(watchdog),
            true,
        )
    };
    let muse_true = candidate(
        "opencode",
        "opencode-go/muse-spark-1.3-contributor",
        None,
        Value::Bool(true),
        Some(120),
        true,
    );
    let opus_high = candidate(
        "claude_code",
        "opus[1m]",
        Some("high"),
        Value::Bool(false),
        None,
        false,
    );
    let codex_high = candidate(
        "codex",
        "gpt-6.1-sol",
        Some("high"),
        Value::Bool(false),
        None,
        false,
    );
    let beam = Candidate {
        nudge: Some(Value::Bool(false)),
        watchdog_minutes: None,
        ..candidate(
            "opencode",
            "opencode-go/beam-free",
            None,
            Value::Bool(false),
            None,
            true,
        )
    };
    let mut task_types = BTreeMap::new();
    let mut task = |key: &str, title: &str, candidates: Vec<Candidate>, isolated: bool| {
        task_types.insert(
            key.to_owned(),
            TaskType {
                title: title.to_owned(),
                candidates,
                preset: None,
                isolated: Some(isolated),
                expected_minutes: DEFAULT_EXPECTED_MINUTES,
            },
        );
    };
    task(
        "bugfix",
        "Исправление по ясному брифу",
        vec![deepseek_high_90(), muse_true.clone(), opus_high.clone()],
        true,
    );
    task(
        "feature",
        "Реализация по брифу",
        vec![deepseek_high_120(), muse_true.clone(), opus_high.clone()],
        true,
    );
    task(
        "research",
        "Разбор неочевидной причины",
        vec![muse_nudge(3, 120), deepseek_high_120(), opus_high.clone()],
        true,
    );
    task(
        "mechanical",
        "Механика, тесты, мелкий интерфейс",
        vec![beam, deepseek_high_120()],
        true,
    );
    task(
        "review",
        "Взгляд со стороны",
        vec![codex_high, opus_high.clone()],
        false,
    );
    task(
        "ops",
        "Выкатка и скрипты прода",
        vec![deepseek_high_90(), opus_high],
        false,
    );
    Policy {
        task_types,
        providers: BTreeMap::from([
            (
                "claude".to_owned(),
                ProviderThresholds {
                    max_weekly_percent: Some(90.0),
                    max_five_hour_percent: Some(95.0),
                    max_monthly_percent: None,
                },
            ),
            (
                "codex".to_owned(),
                ProviderThresholds {
                    max_weekly_percent: Some(80.0),
                    max_five_hour_percent: Some(80.0),
                    max_monthly_percent: None,
                },
            ),
            (
                "opencode".to_owned(),
                ProviderThresholds {
                    max_weekly_percent: Some(95.0),
                    max_five_hour_percent: Some(95.0),
                    max_monthly_percent: Some(95.0),
                },
            ),
        ]),
        unlimited_models: vec!["opencode-go/beam-free".to_owned()],
        max_parallel: BTreeMap::from([
            ("opencode/opencode-go/deepseek-v4.1-flash".to_owned(), 4),
            (
                "opencode/opencode-go/muse-spark-1.3-contributor".to_owned(),
                2,
            ),
            ("codex/default".to_owned(), 1),
            ("codex/gpt-6.1-sol".to_owned(), 1),
            ("claude_code/opus".to_owned(), 2),
            ("claude_code/opus[1m]".to_owned(), 2),
        ]),
        unknown_limits: UnknownLimits::Allow,
    }
}

// ---------------------------------------------------------------------------
// Каталог машины родителя
// ---------------------------------------------------------------------------

/// Модель каталога: идентификатор и усилия, которые она знает.
#[derive(Clone, Debug)]
pub struct CatalogModel {
    pub id: String,
    pub efforts: Vec<String>,
}

/// Каталог одного харнесса на машине родителя.
#[derive(Clone, Debug, Default)]
pub struct HarnessCatalog {
    pub models: Vec<CatalogModel>,
    pub modes: Vec<String>,
}

impl HarnessCatalog {
    pub fn first_model(&self) -> Option<&str> {
        self.models.first().map(|model| model.id.as_str())
    }

    /// Канонизация модели: точный идентификатор, `default` → первая модель,
    /// затем совпадение по основе (`opus` ↔ `opus[1m]`), если оно однозначно.
    pub fn resolve_model(&self, requested: &str) -> Option<&CatalogModel> {
        if let Some(found) = self.models.iter().find(|model| model.id == requested) {
            return Some(found);
        }
        if requested == super::whitelist::DEFAULT_MODEL {
            return self.models.first();
        }
        let base = strip_variant(requested);
        let mut matches = self
            .models
            .iter()
            .filter(|model| strip_variant(&model.id) == base);
        let first = matches.next()?;
        if matches.next().is_none() {
            Some(first)
        } else {
            None
        }
    }

    pub fn has_mode(&self, mode: &str) -> bool {
        self.modes.iter().any(|known| known == mode)
    }
}

fn strip_variant(id: &str) -> &str {
    match id.find('[') {
        Some(n) if id.ends_with(']') => &id[..n],
        _ => id,
    }
}

/// Все харнессы машины родителя.
pub async fn load_catalogs(
    conn: &mut PgConnection,
    machine: Uuid,
) -> Result<HashMap<String, HarnessCatalog>, sqlx::Error> {
    let rows: Vec<(String, Value, Value)> = sqlx::query_as(
        "select harness,models,permission_modes from machine_harnesses where machine_id=$1",
    )
    .bind(machine)
    .fetch_all(conn)
    .await?;
    let mut out = HashMap::new();
    for (harness, models, modes) in rows {
        let models = models
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        Some(CatalogModel {
                            id: item["id"].as_str()?.to_owned(),
                            efforts: item["efforts"]
                                .as_array()
                                .map(|efforts| {
                                    efforts
                                        .iter()
                                        .filter_map(Value::as_str)
                                        .map(str::to_owned)
                                        .collect()
                                })
                                .unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let modes = modes
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        out.insert(harness, HarnessCatalog { models, modes });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Вход и решение
// ---------------------------------------------------------------------------

/// Запрос маршрута: тип, уровень и контекст родителя.
#[derive(Clone, Debug)]
pub struct RouteRequest {
    pub task_type: String,
    pub level: Option<String>,
    pub parent_harness: String,
    pub parent_mode: String,
    pub explicit_mode: Option<String>,
    pub workdir: String,
}

/// Показание окна лимита для маршрута.
#[derive(Clone, Debug)]
pub struct WindowReading {
    pub used_percent: f64,
    pub observed_at: DateTime<Utc>,
    pub resets_at: Option<DateTime<Utc>>,
    pub forecast: Option<Forecast>,
}

/// Лимиты провайдера для маршрута.
#[derive(Clone, Debug, Default)]
pub struct ProviderView {
    pub weekly: Option<WindowReading>,
    pub five_hour: Option<WindowReading>,
    pub monthly: Option<WindowReading>,
    pub reserve_active: bool,
    pub reserve_resets_at: Option<DateTime<Utc>>,
}

/// Кандидат в ответе: пара, вердикт и короткая английская причина.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Considered {
    pub harness: String,
    pub model: String,
    pub verdict: String,
    pub reason: String,
}

/// Решение маршрута: выбранная пара и полный разбор кандидатов.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct Decision {
    pub policy_revision: i64,
    pub task_type: String,
    pub level: Option<String>,
    pub harness: String,
    pub model: String,
    pub effort: String,
    pub preset: Option<String>,
    pub nudge: Value,
    pub watchdog_minutes: Option<i64>,
    /// Ожидаемая длительность задачи, по которой считался прогноз лимита.
    #[serde(default = "default_expected_minutes")]
    pub expected_minutes: i64,
    pub considered: Vec<Considered>,
}

// ---------------------------------------------------------------------------
// Канонизация путей для `exclude_workdirs`
// ---------------------------------------------------------------------------

/// Нормализация пути без обращения к файловой системе: `.`, `..`, повторы `/`,
/// висячий `/` в конце. Пустая нормализация — корень.
fn normalize_workdir(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        return "/".to_owned();
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    out
}

/// Первый префикс, под которым лежит рабочий каталог (сравнение по компонентам).
fn excluded_prefix<'a>(prefixes: &'a [String], workdir: &str) -> Option<&'a str> {
    let path = normalize_workdir(workdir);
    let here: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    prefixes.iter().find_map(|prefix| {
        let normalized = normalize_workdir(prefix);
        let want: Vec<&str> = normalized
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        if !want.is_empty() && here.starts_with(&want) {
            Some(prefix.as_str())
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Выбор
// ---------------------------------------------------------------------------

/// Ошибка маршрута: неизвестный тип или пустой выбор с причинами.
#[derive(Clone, Debug)]
pub enum RouteError {
    UnknownTask {
        available: Vec<String>,
    },
    NoExecutor {
        considered: Vec<Considered>,
        reset: Option<DateTime<Utc>>,
    },
}

/// Вход чистой функции выбора: политика, окружение и запрос одним значением.
pub struct RouteInputs<'a> {
    pub policy: &'a Policy,
    pub revision: i64,
    pub whitelist: &'a [WhitelistEntry],
    pub catalogs: &'a HashMap<String, HarnessCatalog>,
    pub limits: &'a HashMap<String, ProviderView>,
    pub live: &'a HashMap<(String, String), i64>,
    pub stale: Duration,
    pub now: DateTime<Utc>,
    pub request: &'a RouteRequest,
    /// «Запустить сейчас»: лимиты провайдеров не оцениваются вовсе.
    pub bypass_limits: bool,
}

/// Чистая функция выбора (§5.2 + §C1 пп. 5–7): уровень, канонизация через каталог,
/// белый список, совместимость режима, лимиты, параллельность, `exclude_workdirs`.
/// Первый прошедший кандидат выбран.
pub fn route(inputs: RouteInputs<'_>) -> Result<Decision, RouteError> {
    let RouteInputs {
        policy,
        revision,
        whitelist,
        catalogs,
        limits,
        live,
        stale,
        now,
        request,
        bypass_limits,
    } = inputs;
    let task = policy.task_types.get(&request.task_type).ok_or_else(|| {
        let mut available: Vec<String> = policy.task_types.keys().cloned().collect();
        available.sort();
        RouteError::UnknownTask { available }
    })?;
    let mut considered = Vec::with_capacity(task.candidates.len());
    let mut nearest_reset: Option<DateTime<Utc>> = None;
    for candidate in &task.candidates {
        // 1. Уровень.
        if let Some(level) = request.level.as_deref()
            && let Some(levels) = candidate.levels.as_deref()
            && !levels.iter().any(|known| known == level)
        {
            considered.push(skip(
                candidate,
                "level",
                format!("level {level} not in candidate levels"),
            ));
            continue;
        }
        // 2. Канонизация через каталог машины родителя.
        let Some(catalog) = catalogs.get(&candidate.harness) else {
            considered.push(skip(candidate, "skipped", "not available on this machine"));
            continue;
        };
        let resolved = match catalog.resolve_model(&candidate.model) {
            Some(model) => model,
            None => {
                considered.push(skip(candidate, "skipped", "not available on this machine"));
                continue;
            }
        };
        // Усилие: явное важнее; без него уровень, если модель его знает; иначе default.
        let effort = match candidate.effort.as_deref() {
            Some(effort) if resolved.efforts.iter().any(|known| known == effort) => {
                effort.to_owned()
            }
            Some(_) => {
                considered.push(skip(
                    candidate,
                    "skipped",
                    format!(
                        "effort {:?} not available for model {}",
                        candidate.effort, resolved.id
                    ),
                ));
                continue;
            }
            None => request
                .level
                .clone()
                .filter(|level| resolved.efforts.iter().any(|known| known == level))
                .unwrap_or_else(|| default_effort(&resolved.efforts)),
        };
        let first_model = catalog.first_model().unwrap_or_default();
        // 3. Белый список.
        if !whitelist::allows(whitelist, &candidate.harness, &resolved.id, first_model) {
            considered.push(skip(candidate, "skipped", "not in the owner's list"));
            continue;
        }
        // 4. Совместимость режима с харнессом.
        let mode = match request.explicit_mode.as_deref() {
            Some(mode) => mode.to_owned(),
            None => super::spawn::resolve_default_mode(
                &request.parent_harness,
                &request.parent_mode,
                &candidate.harness,
            ),
        };
        if !catalog.has_mode(&mode) {
            considered.push(skip(
                candidate,
                "skipped",
                format!(
                    "permission mode {mode} not available for {}",
                    candidate.harness
                ),
            ));
            continue;
        }
        // 5. Лимиты провайдера.
        let provider = canterlot_proto::harness::provider_of(&candidate.harness)
            .unwrap_or_default()
            .to_owned();
        let unlimited = policy.unlimited_models.iter().any(|entry| {
            entry == &resolved.id || *entry == format!("{}/{}", candidate.harness, resolved.id)
        });
        if !bypass_limits
            && !unlimited
            && let Some((verdict, reason)) = limits_verdict(
                policy,
                &provider,
                limits.get(&provider),
                stale,
                now,
                task.expected_minutes,
                &mut nearest_reset,
            )
        {
            considered.push(skip(candidate, verdict, reason));
            continue;
        }
        // 6. Параллельность.
        let pair = format!("{}/{}", candidate.harness, resolved.id);
        if let Some(limit) = policy.max_parallel.get(&pair) {
            let busy = live
                .get(&(candidate.harness.clone(), resolved.id.clone()))
                .copied()
                .unwrap_or(0);
            if busy >= *limit {
                considered.push(skip(
                    candidate,
                    "parallel_busy",
                    format!("{busy} of {limit} {} sessions busy", resolved.id),
                ));
                continue;
            }
        }
        // 7. Исключённые рабочие каталоги.
        if let Some(prefix) = excluded_prefix(&candidate.exclude_workdirs, &request.workdir) {
            considered.push(skip(
                candidate,
                "excluded_workdir",
                format!("excluded for workdir {prefix}"),
            ));
            continue;
        }
        considered.push(Considered {
            harness: candidate.harness.clone(),
            model: resolved.id.clone(),
            verdict: "chosen".to_owned(),
            reason: String::new(),
        });
        return Ok(Decision {
            policy_revision: revision,
            task_type: request.task_type.clone(),
            level: request.level.clone(),
            harness: candidate.harness.clone(),
            model: resolved.id.clone(),
            effort,
            preset: candidate.preset.clone().or_else(|| task.preset.clone()),
            nudge: candidate.nudge.clone().unwrap_or(Value::Bool(false)),
            watchdog_minutes: candidate.watchdog_minutes,
            expected_minutes: task.expected_minutes,
            considered,
        });
    }
    Err(RouteError::NoExecutor {
        considered,
        reset: nearest_reset,
    })
}

fn skip(candidate: &Candidate, verdict: &str, reason: impl Into<String>) -> Considered {
    Considered {
        harness: candidate.harness.clone(),
        model: candidate.model.clone(),
        verdict: verdict.to_owned(),
        reason: reason.into(),
    }
}

fn default_effort(efforts: &[String]) -> String {
    efforts
        .iter()
        .find(|effort| effort.as_str() == "medium")
        .or_else(|| efforts.first())
        .cloned()
        .unwrap_or_default()
}

fn window_name(key: &str) -> &str {
    key
}

#[allow(clippy::too_many_arguments)]
fn check_window(
    provider: &str,
    window: &str,
    threshold: Option<f64>,
    reading: Option<&WindowReading>,
    unknown: UnknownLimits,
    stale: Duration,
    now: DateTime<Utc>,
    expected_minutes: i64,
    nearest_reset: &mut Option<DateTime<Utc>>,
) -> Option<(&'static str, String)> {
    let threshold = threshold?;
    let Some(reading) = reading else {
        return match unknown {
            UnknownLimits::Allow => None,
            UnknownLimits::Skip => Some((
                "limits_unknown",
                format!("{provider} {window} limits unknown"),
            )),
        };
    };
    // Расход внутри окна не падает: превышение с известным будущим сбросом остаётся
    // превышением и после того, как показание перестало быть свежим.
    if reading.resets_at.is_some_and(|reset| reset > now) && reading.used_percent > threshold {
        note_reset(nearest_reset, reading, now);
        return Some((
            "limits_over",
            format!(
                "{provider} {} {}% > {}%",
                window_name(window),
                percent(reading.used_percent),
                percent(threshold)
            ),
        ));
    }
    let age = now.signed_duration_since(reading.observed_at);
    let reset_passed = reading.resets_at.is_some_and(|reset| reset <= now);
    if reset_passed || age.to_std().is_ok_and(|age| age > stale) {
        return match unknown {
            UnknownLimits::Allow => None,
            UnknownLimits::Skip => {
                note_reset(nearest_reset, reading, now);
                Some((
                    "limits_unknown",
                    if reset_passed {
                        format!(
                            "{} {} limits unknown (reset passed)",
                            provider,
                            window_name(window)
                        )
                    } else {
                        format!(
                            "{} {} limits unknown (observed {})",
                            provider,
                            window_name(window),
                            ago(age)
                        )
                    },
                ))
            }
        };
    }
    if reading.used_percent > threshold {
        return Some((
            "limits_over",
            format!(
                "{provider} {} {}% > {}%",
                window_name(window),
                percent(reading.used_percent),
                percent(threshold)
            ),
        ));
    }
    // Прогноз: окно не успеет сброситься до конца задачи, а темп пересечёт порог.
    if let Some(forecast) = reading.forecast.as_ref() {
        let task_end = now + chrono::TimeDelta::minutes(expected_minutes);
        let resets_before_end = reading.resets_at.is_some_and(|reset| reset <= task_end);
        if !resets_before_end {
            let pace = forecast.pace_permille_per_hour() / 10.0;
            let projected = reading.used_percent + pace * expected_minutes as f64 / 60.0;
            if projected > threshold {
                note_reset(nearest_reset, reading, now);
                return Some((
                    "projected_over_threshold",
                    format!(
                        "{} {}% now, projected {}% in {} min > {}%",
                        projected_window_name(window),
                        whole_percent(reading.used_percent),
                        whole_percent(projected),
                        expected_minutes,
                        whole_percent(threshold),
                    ),
                ));
            }
        }
    }
    None
}

/// Ближайший сброс среди окон, которые действительно блокируют кандидата: окно,
/// прошедшее порог, а не любое просмотренное.
fn note_reset(
    nearest_reset: &mut Option<DateTime<Utc>>,
    reading: &WindowReading,
    now: DateTime<Utc>,
) {
    if let Some(reset) = reading.resets_at
        && reset > now
        && nearest_reset.is_none_or(|known| reset < known)
    {
        *nearest_reset = Some(reset);
    }
}

/// Имя окна в причине прогноза: у пятичасового окна дефис, как в контракте PWA.
fn projected_window_name(window: &str) -> &str {
    match window {
        "five_hour" => "five-hour",
        other => other,
    }
}

fn whole_percent(value: f64) -> i64 {
    value.round() as i64
}

/// Проверка лимитов: пороги, резерв и пауза. Возвращает вердикт и причину пропуска.
fn limits_verdict(
    policy: &Policy,
    provider: &str,
    view: Option<&ProviderView>,
    stale: Duration,
    now: DateTime<Utc>,
    expected_minutes: i64,
    nearest_reset: &mut Option<DateTime<Utc>>,
) -> Option<(&'static str, String)> {
    let thresholds = policy.providers.get(provider);
    let view = view.cloned().unwrap_or_default();
    if view.reserve_active {
        let when = view
            .reserve_resets_at
            .map(|reset| format!(" until {}", reset.format("%H:%M UTC")))
            .unwrap_or_default();
        return Some(("quota_paused", format!("quota paused{when}")));
    }
    let unknown = policy.unknown_limits;
    if let Some(reason) = check_window(
        provider,
        "weekly",
        thresholds.and_then(|limits| limits.max_weekly_percent),
        view.weekly.as_ref(),
        unknown,
        stale,
        now,
        expected_minutes,
        nearest_reset,
    ) {
        return Some(reason);
    }
    if let Some(reason) = check_window(
        provider,
        "five_hour",
        thresholds.and_then(|limits| limits.max_five_hour_percent),
        view.five_hour.as_ref(),
        unknown,
        stale,
        now,
        expected_minutes,
        nearest_reset,
    ) {
        return Some(reason);
    }
    if let Some(reason) = check_window(
        provider,
        "monthly",
        thresholds.and_then(|limits| limits.max_monthly_percent),
        view.monthly.as_ref(),
        unknown,
        stale,
        now,
        expected_minutes,
        nearest_reset,
    ) {
        return Some(reason);
    }
    None
}

fn percent(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value:.1}")
    }
}

fn ago(age: chrono::TimeDelta) -> String {
    let minutes = age.num_minutes().max(0);
    if minutes < 60 {
        format!("{minutes} min ago")
    } else if minutes < 60 * 24 {
        format!("{} h ago", minutes / 60)
    } else {
        format!("{} d ago", minutes / (60 * 24))
    }
}

/// Шов очереди (C2): причина постановки — параллельность или лимиты со сбросом
/// не позже горизонта. Пара вне белого списка и неизвестный харнесс очередью не лечатся.
pub fn queue_reason(
    considered: &[Considered],
    reset: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    horizon: Duration,
) -> Option<&'static str> {
    let mut limits = false;
    for entry in considered {
        match entry.verdict.as_str() {
            "parallel_busy" => return Some("concurrency"),
            "limits_over" | "projected_over_threshold" | "quota_paused" => limits = true,
            _ => {}
        }
    }
    if limits
        && reset
            .is_some_and(|at| at <= now + chrono::TimeDelta::from_std(horizon).unwrap_or_default())
    {
        return Some("limits");
    }
    None
}

/// Вердикты, которые лечатся ожиданием: лимит, а не постоянная причина отказа.
pub fn blocked_by_limits(considered: &[Considered]) -> bool {
    considered.iter().any(|entry| {
        matches!(
            entry.verdict.as_str(),
            "limits_over" | "projected_over_threshold" | "quota_paused" | "limits_unknown"
        )
    })
}

/// Неизвестные лимиты при политике skip лечатся повтором: срока сброса у них нет,
/// поэтому `queue_reason` сам очередь не назначает.
pub fn retry_blocked_by_unknown_limits(considered: &[Considered]) -> bool {
    considered
        .iter()
        .any(|entry| entry.verdict == "limits_unknown")
}

// ---------------------------------------------------------------------------
// База: политика владельца, лимиты, занятость, пресеты
// ---------------------------------------------------------------------------

/// Политика владельца и её ревизия; `null` — встроенная политика.
pub async fn load_policy(
    conn: &mut PgConnection,
    owner: Uuid,
) -> Result<(Policy, i64), sqlx::Error> {
    let row: Option<(Option<Value>, i64)> =
        sqlx::query_as("select routing_policy,routing_policy_revision from owners where id=$1")
            .bind(owner)
            .fetch_optional(&mut *conn)
            .await?;
    let (stored, revision) = row.unwrap_or((None, 0));
    let policy = match stored {
        Some(value) => serde_json::from_value(value).unwrap_or_default(),
        None => builtin(),
    };
    Ok((policy, revision))
}

/// Сохранение политики владельцем: проверка, рост ревизии, событие.
pub async fn save_policy(
    conn: &mut PgConnection,
    owner: Uuid,
    policy: Option<&Policy>,
) -> Result<i64, String> {
    if let Some(policy) = policy {
        validate(policy)?;
    }
    let revision: i64 = sqlx::query_scalar(
        "update owners set routing_policy=$2,routing_policy_revision=routing_policy_revision+1
          where id=$1 returning routing_policy_revision",
    )
    .bind(owner)
    .bind(policy.map(|value| serde_json::to_value(value).unwrap_or(Value::Null)))
    .fetch_one(&mut *conn)
    .await
    .map_err(|error| error.to_string())?;
    Ok(revision)
}

/// Снимок лимитов провайдеров для маршрута.
pub async fn limits_view(
    conn: &mut PgConnection,
) -> Result<HashMap<String, ProviderView>, sqlx::Error> {
    let current = crate::limits::current(conn).await?;
    let mut out = HashMap::new();
    for provider in current {
        let window = |state: Option<crate::limits::WindowState>| {
            state.map(|reading| WindowReading {
                used_percent: f64::from(reading.used_permille) / 10.0,
                observed_at: reading.observed_at,
                resets_at: reading.resets_at,
                forecast: reading.forecast,
            })
        };
        out.insert(
            provider.provider.clone(),
            ProviderView {
                weekly: window(provider.weekly),
                five_hour: window(provider.five_hour),
                monthly: window(provider.monthly),
                reserve_active: provider.weekly_reserve_active,
                reserve_resets_at: provider.weekly_reserve_resets_at,
            },
        );
    }
    Ok(out)
}

/// Отпечаток занятых пар: живые делегированные сессии владельца с той же
/// разрешённой парой: `working`, `stalled`, `idle` с очередью, ожидающий толчок.
pub async fn live_counts(
    conn: &mut PgConnection,
    owner: Uuid,
) -> Result<HashMap<(String, String), i64>, sqlx::Error> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "select c.harness,c.model,count(*) from conversations c
          join conversation_derived d on d.conversation_id=c.id
          where c.owner_id=$1 and c.parent_conversation_id is not null and c.trashed_at is null
            and (d.activity_state in ('working','stalled')
              or (d.activity_state='idle' and d.queue_queued > 0)
              or exists(select 1 from nudge_rules r
                        where r.conversation_id=c.id and r.state='pending')
              -- Проекция отстаёт на мгновение: только что созданный делегат с ещё
              -- не принятым вводом уже занимает слот параллельности. Остановленная
              -- сессия с замершим вводом слот не держит: нужен живой процесс.
              or (exists(select 1 from executions e
                         where e.conversation_id=c.id
                           and e.state not in ('stopped','failed','owner_lost'))
                  and (exists(select 1 from inputs i
                              where i.conversation_id=c.id and i.feed_seq is null
                                and i.state in ('queued','dispatched','on_machine'))
                       or exists(select 1 from queue_state q
                                 where q.conversation_id=c.id and q.run_slot <> 'free'))))
          group by c.harness,c.model",
    )
    .bind(owner)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(harness, model, count)| ((harness, model), count))
        .collect())
}

/// Пресет политики по ссылке (`clean`, UUID) или названию; иначе пресет
/// «Исполнитель (английский)» по названию; если и его нет — `clean`.
pub async fn resolve_policy_preset(
    conn: &mut PgConnection,
    owner: Uuid,
    raw: Option<&str>,
) -> Result<String, sqlx::Error> {
    if let Some(raw) = raw.filter(|value| !value.trim().is_empty()) {
        if crate::presets::PresetRef::parse(raw).is_ok() {
            return Ok(raw.to_owned());
        }
        if let Some(id) = find_preset_by_title(conn, owner, raw).await? {
            return Ok(id);
        }
    }
    if let Some(id) = find_preset_by_title(conn, owner, EXECUTOR_PRESET_TITLE).await? {
        return Ok(id);
    }
    Ok("clean".to_owned())
}

async fn find_preset_by_title(
    conn: &mut PgConnection,
    owner: Uuid,
    title: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "select id::text from instruction_presets
          where owner_id=$1 and title=$2 and purpose='instructions' and deleted_at is null
          limit 1",
    )
    .bind(owner)
    .bind(title)
    .fetch_optional(conn)
    .await
}

/// Запас машины для предпросмотра: минимум свободного диска, нагрузка на ядро,
/// свободная память и свежесть метрик.
#[derive(Clone, Debug, Default, Serialize, ToSchema)]
pub struct MachineHeadroom {
    pub free_disk_gb: Option<f64>,
    pub load_per_core: Option<f64>,
    pub mem_available_gb: Option<f64>,
    pub observed_at: Option<DateTime<Utc>>,
    pub stale: bool,
}

pub async fn machine_headroom(
    conn: &mut PgConnection,
    machine: Uuid,
    stale_after: Duration,
) -> Result<MachineHeadroom, sqlx::Error> {
    const BYTES_PER_GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let row: Option<(i32, i32, i64, Value, DateTime<Utc>)> = sqlx::query_as(
        "select cpu_cores,load1_centi,mem_available_bytes,disks,observed_at
          from machine_metrics where machine_id=$1",
    )
    .bind(machine)
    .fetch_optional(conn)
    .await?;
    let Some((cores, load1, mem_available, disks, observed_at)) = row else {
        return Ok(MachineHeadroom {
            stale: true,
            ..MachineHeadroom::default()
        });
    };
    let free_disk_gb = disks
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|disk| disk["free_bytes"].as_i64())
                .map(|free| free as f64 / BYTES_PER_GB)
                .reduce(f64::min)
        })
        .unwrap_or(None);
    let load_per_core = (cores > 0).then(|| f64::from(load1) / 100.0 / f64::from(cores));
    let stale = Utc::now()
        .signed_duration_since(observed_at)
        .to_std()
        .is_ok_and(|age| age > stale_after);
    Ok(MachineHeadroom {
        free_disk_gb,
        load_per_core,
        mem_available_gb: Some(mem_available as f64 / BYTES_PER_GB),
        observed_at: Some(observed_at),
        stale,
    })
}

// ---------------------------------------------------------------------------
// Инструмент `route_preview`
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewArgs {
    task_type: Option<String>,
    level: Option<String>,
}

/// `routing.preview`: список типов без `task_type`, иначе решение и запас машины.
/// Описание инструмента — на стороне воркера (`route_preview` в MCP).
pub async fn preview(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: PreviewArgs = super::parse_arguments(arguments)?;
    if !context.routing_enabled {
        return Err(ToolError::with_message(
            ToolErrorCode::InvalidArguments,
            "Routing is disabled on this hub",
        ));
    }
    if let Some(level) = args.level.as_deref()
        && !LEVELS.contains(&level)
    {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }
    let mut tx = crate::db::begin(&context.pool).await?;
    let (policy, revision) = load_policy(&mut tx, caller.owner_id).await?;
    let Some(task_type) = args.task_type.as_deref() else {
        let mut types: Vec<Value> = policy
            .task_types
            .iter()
            .map(|(key, task)| serde_json::json!({"key": key, "title": task.title}))
            .collect();
        types.sort_by(|left, right| left["key"].as_str().cmp(&right["key"].as_str()));
        tx.commit().await?;
        return Ok(serde_json::json!({"task_types": types, "policy_revision": revision}));
    };
    let parent =
        super::access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    let catalogs = load_catalogs(&mut tx, caller.machine_id).await?;
    let list = whitelist::view(&mut tx, caller.owner_id).await?;
    let limits = limits_view(&mut tx).await?;
    let live = live_counts(&mut tx, caller.owner_id).await?;
    let place: (String,) = sqlx::query_as("select workdir from conversations where id=$1")
        .bind(caller.conversation_id)
        .fetch_one(&mut *tx)
        .await?;
    let request = RouteRequest {
        task_type: task_type.to_owned(),
        level: args.level.clone(),
        parent_harness: parent.harness.clone(),
        parent_mode: super::spawn::parent_mode(
            &parent.permission_mode,
            parent.pending_mode.as_deref(),
        )
        .to_owned(),
        explicit_mode: None,
        workdir: place.0,
    };
    let now = Utc::now();
    let headroom = machine_headroom(
        &mut tx,
        caller.machine_id,
        std::time::Duration::from_secs(120),
    )
    .await?;
    let guard = crate::session_tools::guard::check_in_tx(
        &mut tx,
        caller.owner_id,
        caller.machine_id,
        &context.guards,
        now,
    )
    .await?;
    let existing = policy.task_types.contains_key(task_type);
    match route(RouteInputs {
        policy: &policy,
        revision,
        whitelist: &list.entries,
        catalogs: &catalogs,
        limits: &limits,
        live: &live,
        stale: context.routing_stale,
        now,
        request: &request,
        bypass_limits: false,
    }) {
        Ok(decision) => {
            tx.commit().await?;
            Ok(serde_json::json!({
                "decision": decision,
                "machine": headroom,
                "guard": guard.to_value(),
                "hint": "Read-only preview; to start the session, call session_spawn with route.",
            }))
        }
        Err(RouteError::UnknownTask { available }) => {
            let _ = existing;
            Err(ToolError::with_message(
                ToolErrorCode::TaskTypeUnknown,
                format!(
                    "Unknown task type {task_type:?}. Available: {}",
                    available.join(", ")
                ),
            ))
        }
        Err(RouteError::NoExecutor { considered, reset }) => Err(ToolError::with_message(
            ToolErrorCode::NoExecutorAvailable,
            no_executor_message(&considered, reset),
        )),
    }
}

pub(crate) fn no_executor_message(
    considered: &[Considered],
    reset: Option<DateTime<Utc>>,
) -> String {
    let mut message = String::from("No executor available for this task type:");
    for entry in considered {
        message.push_str(&format!(" {}: {}; ", entry.harness, entry.reason));
    }
    if let Some(reset) = reset {
        message.push_str(&format!(
            "nearest limit reset at {}",
            reset.format("%H:%M UTC")
        ));
    }
    message.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog(models: &[(&str, &[&str])], modes: &[&str]) -> HarnessCatalog {
        HarnessCatalog {
            models: models
                .iter()
                .map(|(id, efforts)| CatalogModel {
                    id: id.to_string(),
                    efforts: efforts.iter().map(|effort| effort.to_string()).collect(),
                })
                .collect(),
            modes: modes.iter().map(|mode| mode.to_string()).collect(),
        }
    }

    fn catalogs() -> HashMap<String, HarnessCatalog> {
        HashMap::from([
            (
                "opencode".to_owned(),
                catalog(
                    &[
                        (
                            "opencode-go/deepseek-v4.1-flash",
                            &["low", "medium", "high"],
                        ),
                        (
                            "opencode-go/muse-spark-1.3-contributor",
                            &["low", "medium", "high"],
                        ),
                        ("opencode-go/beam-free", &["low", "medium"]),
                    ],
                    &["owner", "ask", "bypass", "read_only"],
                ),
            ),
            (
                "claude_code".to_owned(),
                catalog(
                    &[("opus[1m]", &["low", "medium", "high"])],
                    &["owner", "ask", "bypass", "read_only"],
                ),
            ),
            (
                "codex".to_owned(),
                catalog(
                    &[("gpt-6.1-sol", &["low", "medium", "high"])],
                    &["owner", "ask", "bypass", "read_only"],
                ),
            ),
        ])
    }

    fn entries() -> Vec<WhitelistEntry> {
        [
            "opencode/opencode-go/deepseek-v4.1-flash",
            "opencode/opencode-go/muse-spark-1.3-contributor",
            "opencode/opencode-go/beam-free",
            "claude_code/opus[1m]",
            "codex/gpt-6.1-sol",
        ]
        .iter()
        .map(|pair| {
            let (harness, model) = pair.split_once('/').unwrap();
            WhitelistEntry {
                harness: harness.to_owned(),
                model: model.to_owned(),
            }
        })
        .collect()
    }

    fn policy() -> Policy {
        let mut policy = builtin();
        // Тестовый белый список короче встроенного: первый кандидат каталога.
        policy.unlimited_models = vec!["opencode-go/beam-free".to_owned()];
        policy
    }

    fn request(task_type: &str) -> RouteRequest {
        RouteRequest {
            task_type: task_type.to_owned(),
            level: None,
            parent_harness: "opencode".to_owned(),
            parent_mode: "bypass".to_owned(),
            explicit_mode: None,
            workdir: "/tmp/w".to_owned(),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-05T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn reading(percent: f64, age_minutes: i64) -> WindowReading {
        WindowReading {
            used_percent: percent,
            observed_at: now() - chrono::TimeDelta::minutes(age_minutes),
            resets_at: Some(now() + chrono::TimeDelta::hours(4)),
            forecast: None,
        }
    }

    fn limits_with(weekly: f64) -> HashMap<String, ProviderView> {
        HashMap::from([(
            "opencode".to_owned(),
            ProviderView {
                weekly: Some(reading(weekly, 5)),
                ..ProviderView::default()
            },
        )])
    }

    #[test]
    fn builtin_policy_passes_validation() {
        assert!(validate(&builtin()).is_ok());
    }

    #[test]
    fn bypass_limits_ignores_unknown_and_over_thresholds() {
        let mut policy = policy();
        policy.unknown_limits = UnknownLimits::Skip;
        let empty = HashMap::new();
        let empty_live: HashMap<(String, String), i64> = HashMap::new();
        let req = request("bugfix");
        assert!(
            matches!(
                route(RouteInputs {
                    policy: &policy,
                    revision: 1,
                    whitelist: &entries(),
                    catalogs: &catalogs(),
                    limits: &empty,
                    live: &empty_live,
                    stale: Duration::from_secs(1800),
                    now: now(),
                    request: &req,
                    bypass_limits: false,
                }),
                Err(RouteError::NoExecutor { .. })
            ),
            "unknown limits skip the candidate without a bypass"
        );
        let decision = route(RouteInputs {
            policy: &policy,
            revision: 1,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &empty,
            live: &empty_live,
            stale: Duration::from_secs(1800),
            now: now(),
            request: &req,
            bypass_limits: true,
        })
        .unwrap();
        assert_eq!(decision.model, "opencode-go/deepseek-v4.1-flash");
    }

    #[test]
    fn first_passing_candidate_wins() {
        let decision = route(RouteInputs {
            policy: &policy(),
            revision: 3,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &HashMap::new(),
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("bugfix"),
            bypass_limits: false,
        })
        .unwrap();
        assert_eq!(decision.harness, "opencode");
        assert_eq!(decision.model, "opencode-go/deepseek-v4.1-flash");
        assert_eq!(decision.effort, "high");
        assert_eq!(decision.policy_revision, 3);
        assert_eq!(decision.watchdog_minutes, Some(90));
        assert_eq!(decision.expected_minutes, DEFAULT_EXPECTED_MINUTES);
        assert_eq!(decision.considered.len(), 1);
        assert_eq!(decision.considered[0].verdict, "chosen");
    }

    #[test]
    fn level_filters_and_sets_effort() {
        let mut policy = policy();
        let task = policy.task_types.get_mut("mechanical").unwrap();
        task.candidates[0].levels = Some(vec!["low".to_owned()]);
        let mut with_level = request("mechanical");
        with_level.level = Some("high".to_owned());
        let decision = route(RouteInputs {
            policy: &policy,
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &HashMap::new(),
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &with_level,
            bypass_limits: false,
        })
        .unwrap();
        // Beam пропущен по уровню; DeepSeek без effort берёт уровень как усилие.
        assert_eq!(decision.model, "opencode-go/deepseek-v4.1-flash");
        assert_eq!(decision.effort, "high");
        assert!(decision.considered[0].reason.contains("high"));
    }

    #[test]
    fn whitelist_skip_is_explained() {
        let decision = route(RouteInputs {
            policy: &policy(),
            revision: 0,
            whitelist: &[],
            catalogs: &catalogs(),
            limits: &HashMap::new(),
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("bugfix"),
            bypass_limits: false,
        });
        let error = decision.unwrap_err();
        let RouteError::NoExecutor { considered, .. } = error else {
            panic!("expected no executor");
        };
        assert!(considered.iter().all(|entry| entry.verdict == "skipped"));
        assert!(
            considered
                .iter()
                .all(|entry| entry.reason == "not in the owner's list")
        );
    }

    #[test]
    fn missing_harness_is_not_available() {
        let mut catalogs = catalogs();
        catalogs.remove("claude_code");
        let decision = route(RouteInputs {
            policy: &policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs,
            limits: &HashMap::new(),
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("review"),
            bypass_limits: false,
        })
        .unwrap();
        // claude_code нет в каталоге: выбран codex, а не отсутствующий opus.
        assert_eq!(decision.harness, "codex");
        assert_eq!(decision.model, "gpt-6.1-sol");
    }

    #[test]
    fn default_model_resolves_to_first() {
        let mut policy = Policy::default();
        policy.task_types.insert(
            "probe".to_owned(),
            TaskType {
                title: "Проба".to_owned(),
                candidates: vec![Candidate {
                    harness: "opencode".to_owned(),
                    model: "default".to_owned(),
                    effort: None,
                    levels: None,
                    preset: None,
                    nudge: None,
                    exclude_workdirs: vec![],
                    watchdog_minutes: None,
                }],
                preset: None,
                isolated: None,
                expected_minutes: DEFAULT_EXPECTED_MINUTES,
            },
        );
        let decision = route(RouteInputs {
            policy: &policy,
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &HashMap::new(),
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("probe"),
            bypass_limits: false,
        })
        .unwrap();
        assert_eq!(decision.model, "opencode-go/deepseek-v4.1-flash");
    }

    #[test]
    fn alias_resolves_both_ways() {
        let mut policy = Policy::default();
        policy.task_types.insert(
            "probe".to_owned(),
            TaskType {
                title: "Проба".to_owned(),
                candidates: vec![Candidate {
                    harness: "claude_code".to_owned(),
                    model: "opus".to_owned(),
                    effort: None,
                    levels: None,
                    preset: None,
                    nudge: None,
                    exclude_workdirs: vec![],
                    watchdog_minutes: None,
                }],
                preset: None,
                isolated: None,
                expected_minutes: DEFAULT_EXPECTED_MINUTES,
            },
        );
        let decision = route(RouteInputs {
            policy: &policy,
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &HashMap::new(),
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("probe"),
            bypass_limits: false,
        })
        .unwrap();
        assert_eq!(decision.model, "opus[1m]");
    }

    #[test]
    fn unknown_effort_skips_candidate() {
        let mut policy = Policy::default();
        policy.task_types.insert(
            "probe".to_owned(),
            TaskType {
                title: "Проба".to_owned(),
                candidates: vec![Candidate {
                    harness: "opencode".to_owned(),
                    model: "opencode-go/beam-free".to_owned(),
                    effort: Some("high".to_owned()),
                    levels: None,
                    preset: None,
                    nudge: None,
                    exclude_workdirs: vec![],
                    watchdog_minutes: None,
                }],
                preset: None,
                isolated: None,
                expected_minutes: DEFAULT_EXPECTED_MINUTES,
            },
        );
        // Beam свободен от лимитов, но усилие `high` он не знает.
        let error = route(RouteInputs {
            policy: &policy,
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &HashMap::new(),
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("probe"),
            bypass_limits: false,
        })
        .unwrap_err();
        let RouteError::NoExecutor { considered, .. } = error else {
            panic!("expected no executor");
        };
        assert!(considered[0].reason.contains("high"));
    }

    #[test]
    fn over_threshold_skips_with_numbers() {
        let decision = route(RouteInputs {
            policy: &policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &limits_with(97.0),
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("bugfix"),
            bypass_limits: false,
        })
        .unwrap();
        // DeepSeek и Muse упёрлись в лимит opencode; у claude показаний нет —
        // unknown allow, поэтому выбран opus, а сброс недели известен.
        assert_eq!(decision.harness, "claude_code");
        assert_eq!(decision.considered.len(), 3);
        assert!(
            decision
                .considered
                .iter()
                .any(|entry| entry.reason == "opencode weekly 97% > 95%")
        );
    }

    fn with_forecast(percent: f64, resets_in: chrono::TimeDelta, rate: f64) -> WindowReading {
        WindowReading {
            used_percent: percent,
            observed_at: now() - chrono::TimeDelta::minutes(5),
            resets_at: Some(now() + resets_in),
            forecast: Some(Forecast {
                rate_permille_per_hour: rate,
                recent_rate_permille_per_hour: None,
                exhausts_at: Some(now() + chrono::TimeDelta::hours(1)),
                before_reset: true,
            }),
        }
    }

    fn projected_policy() -> Policy {
        let mut policy = policy();
        let thresholds = policy.providers.get_mut("opencode").unwrap();
        thresholds.max_weekly_percent = Some(80.0);
        thresholds.max_five_hour_percent = None;
        thresholds.max_monthly_percent = None;
        policy
    }

    #[test]
    fn projected_over_threshold_skips_with_the_contract_reason() {
        let limits: HashMap<String, ProviderView> = HashMap::from([(
            "opencode".to_owned(),
            ProviderView {
                weekly: Some(with_forecast(72.0, chrono::TimeDelta::hours(4), 120.0)),
                ..ProviderView::default()
            },
        )]);
        let decision = route(RouteInputs {
            policy: &projected_policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &limits,
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("bugfix"),
            bypass_limits: false,
        })
        .unwrap();
        // Оба кандидата opencode пропущены прогнозом; выбран opus.
        assert_eq!(decision.harness, "claude_code");
        assert!(decision.considered.iter().any(|entry| {
            entry.verdict == "projected_over_threshold"
                && entry.reason == "weekly 72% now, projected 84% in 60 min > 80%"
        }));
    }

    #[test]
    fn a_reset_before_the_task_ends_does_not_skip() {
        let limits: HashMap<String, ProviderView> = HashMap::from([(
            "opencode".to_owned(),
            ProviderView {
                weekly: Some(with_forecast(72.0, chrono::TimeDelta::minutes(30), 120.0)),
                ..ProviderView::default()
            },
        )]);
        let decision = route(RouteInputs {
            policy: &projected_policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &limits,
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("bugfix"),
            bypass_limits: false,
        })
        .unwrap();
        assert_eq!(decision.harness, "opencode");
        assert_eq!(decision.considered[0].verdict, "chosen");
    }

    #[test]
    fn without_forecast_only_the_current_percent_counts() {
        let limits: HashMap<String, ProviderView> = HashMap::from([(
            "opencode".to_owned(),
            ProviderView {
                weekly: Some(reading(72.0, 5)),
                ..ProviderView::default()
            },
        )]);
        let decision = route(RouteInputs {
            policy: &projected_policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &limits,
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("bugfix"),
            bypass_limits: false,
        })
        .unwrap();
        assert_eq!(decision.harness, "opencode");
        assert_eq!(decision.considered[0].verdict, "chosen");
    }

    #[test]
    fn an_already_over_window_keeps_the_old_reason() {
        let limits: HashMap<String, ProviderView> = HashMap::from([(
            "opencode".to_owned(),
            ProviderView {
                weekly: Some(with_forecast(97.0, chrono::TimeDelta::hours(4), 120.0)),
                ..ProviderView::default()
            },
        )]);
        let decision = route(RouteInputs {
            policy: &policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &limits,
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("bugfix"),
            bypass_limits: false,
        })
        .unwrap();
        assert!(decision.considered.iter().any(|entry| {
            entry.verdict == "limits_over" && entry.reason == "opencode weekly 97% > 95%"
        }));
    }

    #[test]
    fn expected_minutes_defaults_and_validates() {
        let task: TaskType =
            serde_json::from_value(json!({"title": "Проба", "candidates": []})).unwrap();
        assert_eq!(task.expected_minutes, DEFAULT_EXPECTED_MINUTES);
        let mut policy = builtin();
        assert!(
            policy
                .task_types
                .values()
                .all(|task| task.expected_minutes == DEFAULT_EXPECTED_MINUTES)
        );
        policy
            .task_types
            .get_mut("bugfix")
            .unwrap()
            .expected_minutes = MIN_EXPECTED_MINUTES - 1;
        assert!(validate(&policy).is_err());
        policy
            .task_types
            .get_mut("bugfix")
            .unwrap()
            .expected_minutes = MAX_EXPECTED_MINUTES + 1;
        assert!(validate(&policy).is_err());
        policy
            .task_types
            .get_mut("bugfix")
            .unwrap()
            .expected_minutes = MIN_EXPECTED_MINUTES;
        assert!(validate(&policy).is_ok());
    }

    #[test]
    fn all_filtered_reports_reasons_and_reset() {
        let limits: HashMap<String, ProviderView> = HashMap::from([
            (
                "opencode".to_owned(),
                ProviderView {
                    weekly: Some(reading(99.0, 5)),
                    five_hour: Some(reading(99.0, 5)),
                    monthly: Some(reading(99.0, 5)),
                    ..ProviderView::default()
                },
            ),
            (
                "claude".to_owned(),
                ProviderView {
                    weekly: Some(reading(99.0, 5)),
                    five_hour: Some(reading(99.0, 5)),
                    monthly: None,
                    ..ProviderView::default()
                },
            ),
        ]);
        let error = route(RouteInputs {
            policy: &policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &limits,
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("bugfix"),
            bypass_limits: false,
        })
        .unwrap_err();
        let RouteError::NoExecutor { considered, reset } = error else {
            panic!("expected no executor");
        };
        assert_eq!(considered.len(), 3);
        assert!(
            considered
                .iter()
                .any(|entry| entry.reason.contains("weekly"))
        );
        assert!(reset.is_some());
        let message = no_executor_message(&considered, reset);
        assert!(message.contains("No executor available"));
    }

    #[test]
    fn unlimited_models_skip_limits() {
        let mut policy = Policy::default();
        policy.task_types.insert(
            "fast".to_owned(),
            TaskType {
                title: "Быстро".to_owned(),
                candidates: vec![Candidate {
                    harness: "opencode".to_owned(),
                    model: "opencode-go/beam-free".to_owned(),
                    effort: None,
                    levels: None,
                    preset: None,
                    nudge: None,
                    exclude_workdirs: vec![],
                    watchdog_minutes: None,
                }],
                preset: None,
                isolated: None,
                expected_minutes: DEFAULT_EXPECTED_MINUTES,
            },
        );
        policy.unlimited_models = vec!["opencode-go/beam-free".to_owned()];
        let decision = route(RouteInputs {
            policy: &policy,
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &limits_with(99.0),
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("fast"),
            bypass_limits: false,
        })
        .unwrap();
        assert_eq!(decision.model, "opencode-go/beam-free");
    }

    #[test]
    fn stale_limits_allow_and_skip() {
        let mut limits = HashMap::new();
        limits.insert(
            "opencode".to_owned(),
            ProviderView {
                weekly: Some(reading(10.0, 120)),
                ..ProviderView::default()
            },
        );
        // allow: кандидат проходит с пометкой (пустой отчёт: он первый и выбран).
        let decision = route(RouteInputs {
            policy: &policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &limits,
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("bugfix"),
            bypass_limits: false,
        })
        .unwrap();
        assert_eq!(decision.model, "opencode-go/deepseek-v4.1-flash");
        // skip: несвежий кандидат отсекается, выбирается следующий свежий.
        let mut strict = policy();
        strict.unknown_limits = UnknownLimits::Skip;
        let mut fresh = limits.clone();
        fresh.insert(
            "claude".to_owned(),
            ProviderView {
                weekly: Some(reading(10.0, 5)),
                five_hour: Some(reading(10.0, 5)),
                monthly: None,
                ..ProviderView::default()
            },
        );
        let error_or_next = route(RouteInputs {
            policy: &strict,
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &fresh,
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("bugfix"),
            bypass_limits: false,
        });
        let decision = error_or_next.unwrap();
        assert_ne!(decision.model, "opencode-go/deepseek-v4.1-flash");
        assert!(
            decision
                .considered
                .iter()
                .any(|entry| entry.reason.contains("observed"))
        );
    }

    #[test]
    fn a_reading_after_its_reset_is_unknown_not_over() {
        let past = WindowReading {
            used_percent: 96.0,
            observed_at: now() - chrono::TimeDelta::minutes(10),
            resets_at: Some(now() - chrono::TimeDelta::minutes(1)),
            forecast: None,
        };
        for (unknown, expected) in [
            (UnknownLimits::Allow, None),
            (UnknownLimits::Skip, Some("limits_unknown")),
        ] {
            let mut nearest_reset = None;
            let verdict = check_window(
                "opencode",
                "weekly",
                Some(95.0),
                Some(&past),
                unknown,
                Duration::from_secs(1800),
                now(),
                60,
                &mut nearest_reset,
            );
            assert_eq!(verdict.map(|(verdict, _)| verdict), expected, "{unknown:?}");
            assert_eq!(nearest_reset, None, "a past reset is not a waiting target");
        }
    }

    #[test]
    fn a_stale_exhausted_window_stays_over_until_its_reset() {
        let stale = WindowReading {
            used_percent: 97.0,
            observed_at: now() - chrono::TimeDelta::minutes(120),
            resets_at: Some(now() + chrono::TimeDelta::days(3)),
            forecast: None,
        };
        for unknown in [UnknownLimits::Allow, UnknownLimits::Skip] {
            let mut nearest_reset = None;
            let verdict = check_window(
                "claude",
                "weekly",
                Some(90.0),
                Some(&stale),
                unknown,
                Duration::from_secs(1800),
                now(),
                60,
                &mut nearest_reset,
            );
            assert_eq!(
                verdict.map(|(verdict, _)| verdict),
                Some("limits_over"),
                "{unknown:?}"
            );
        }
    }

    #[test]
    fn a_stale_reading_without_a_reset_follows_the_unknown_policy() {
        let stale = WindowReading {
            used_percent: 97.0,
            observed_at: now() - chrono::TimeDelta::minutes(120),
            resets_at: None,
            forecast: None,
        };
        for (unknown, expected) in [
            (UnknownLimits::Allow, None),
            (UnknownLimits::Skip, Some("limits_unknown")),
        ] {
            let mut nearest_reset = None;
            let verdict = check_window(
                "opencode",
                "weekly",
                Some(95.0),
                Some(&stale),
                unknown,
                Duration::from_secs(1800),
                now(),
                60,
                &mut nearest_reset,
            );
            assert_eq!(verdict.map(|(verdict, _)| verdict), expected, "{unknown:?}");
        }
    }

    fn single_candidate_policy() -> Policy {
        let mut policy = Policy::default();
        policy.task_types.insert(
            "one".to_owned(),
            TaskType {
                title: "Один".to_owned(),
                candidates: vec![Candidate {
                    harness: "opencode".to_owned(),
                    model: "opencode-go/deepseek-v4.1-flash".to_owned(),
                    effort: Some("high".to_owned()),
                    levels: None,
                    preset: None,
                    nudge: None,
                    exclude_workdirs: vec![],
                    watchdog_minutes: None,
                }],
                preset: None,
                isolated: None,
                expected_minutes: DEFAULT_EXPECTED_MINUTES,
            },
        );
        policy.providers.insert(
            "opencode".to_owned(),
            ProviderThresholds {
                max_weekly_percent: Some(95.0),
                max_five_hour_percent: Some(95.0),
                max_monthly_percent: Some(95.0),
            },
        );
        policy
    }

    fn window_at(percent: f64, reset: chrono::TimeDelta) -> WindowReading {
        WindowReading {
            used_percent: percent,
            observed_at: now() - chrono::TimeDelta::minutes(5),
            resets_at: Some(now() + reset),
            forecast: None,
        }
    }

    fn route_one(limits: &HashMap<String, ProviderView>) -> Result<Decision, RouteError> {
        route(RouteInputs {
            policy: &single_candidate_policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits,
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("one"),
            bypass_limits: false,
        })
    }

    #[test]
    fn only_the_blocking_window_sets_the_nearest_reset() {
        let limits = HashMap::from([(
            "opencode".to_owned(),
            ProviderView {
                weekly: Some(window_at(10.0, chrono::TimeDelta::hours(4))),
                five_hour: Some(window_at(10.0, chrono::TimeDelta::hours(1))),
                monthly: Some(window_at(96.0, chrono::TimeDelta::days(20))),
                ..ProviderView::default()
            },
        )]);
        let error = route_one(&limits).unwrap_err();
        let RouteError::NoExecutor { considered, reset } = error else {
            panic!("expected no executor");
        };
        assert_eq!(reset, Some(now() + chrono::TimeDelta::days(20)));
        assert!(
            considered
                .iter()
                .any(|entry| entry.verdict == "limits_over" && entry.reason.contains("monthly"))
        );
        assert_eq!(
            queue_reason(&considered, reset, now(), Duration::from_secs(6 * 3600)),
            None,
            "a reset beyond the horizon is not a reason to queue"
        );
    }

    #[test]
    fn a_blocking_five_hour_window_queues_with_its_own_reset() {
        let limits = HashMap::from([(
            "opencode".to_owned(),
            ProviderView {
                weekly: Some(window_at(10.0, chrono::TimeDelta::hours(4))),
                five_hour: Some(window_at(96.0, chrono::TimeDelta::hours(1))),
                ..ProviderView::default()
            },
        )]);
        let error = route_one(&limits).unwrap_err();
        let RouteError::NoExecutor { considered, reset } = error else {
            panic!("expected no executor");
        };
        let reset = reset.unwrap();
        assert_eq!(reset, now() + chrono::TimeDelta::hours(1));
        assert_eq!(
            queue_reason(&considered, Some(reset), now(), Duration::from_secs(6 * 3600)),
            Some("limits")
        );
        assert!(no_executor_message(&considered, Some(reset)).contains("13:00 UTC"));
    }

    #[test]
    fn reserve_pauses_with_time() {
        let limits: HashMap<String, ProviderView> = HashMap::from([(
            "opencode".to_owned(),
            ProviderView {
                reserve_active: true,
                reserve_resets_at: Some(now() + chrono::TimeDelta::hours(2)),
                ..ProviderView::default()
            },
        )]);
        let decision = route(RouteInputs {
            policy: &policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &limits,
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("bugfix"),
            bypass_limits: false,
        })
        .unwrap();
        // Первый два кандидата на паузе; выбран opus.
        assert_eq!(decision.harness, "claude_code");
        assert!(
            decision
                .considered
                .iter()
                .any(|entry| entry.reason.contains("quota paused"))
        );
    }

    #[test]
    fn parallelism_counts_matching_pairs() {
        let live: HashMap<(String, String), i64> = HashMap::from([(
            (
                "opencode".to_owned(),
                "opencode-go/deepseek-v4.1-flash".to_owned(),
            ),
            4,
        )]);
        let decision = route(RouteInputs {
            policy: &policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &HashMap::new(),
            live: &live,
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("bugfix"),
            bypass_limits: false,
        })
        .unwrap();
        // DeepSeek занят 4 из 4; Muse исключён workdir? Нет: workdir /tmp/w.
        assert_eq!(decision.model, "opencode-go/muse-spark-1.3-contributor");
        assert!(
            decision
                .considered
                .iter()
                .any(|entry| entry.reason.contains("sessions busy"))
        );
    }

    #[test]
    fn exclude_workdirs_matches_components() {
        let mut with_home = request("bugfix");
        with_home.workdir = "/home/fenicu/work/project".to_owned();
        let decision = route(RouteInputs {
            policy: &policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &HashMap::new(),
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &with_home,
            bypass_limits: false,
        })
        .unwrap();
        // Muse исключён каталогом; DeepSeek первый и проходит.
        assert_eq!(decision.model, "opencode-go/deepseek-v4.1-flash");
        assert!(decision.considered.len() == 1);
        // А под workshops-префиксом Muse проходит: не тот компонент.
        let mut shop = request("feature");
        shop.workdir = "/home/fenicu/workshop".to_owned();
        let live: HashMap<(String, String), i64> = HashMap::from([(
            (
                "opencode".to_owned(),
                "opencode-go/deepseek-v4.1-flash".to_owned(),
            ),
            4,
        )]);
        let decision = route(RouteInputs {
            policy: &policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &HashMap::new(),
            live: &live,
            stale: Duration::from_secs(1800),
            now: now(),
            request: &shop,
            bypass_limits: false,
        })
        .unwrap();
        assert_eq!(decision.model, "opencode-go/muse-spark-1.3-contributor");
    }

    #[test]
    fn trailing_slash_and_dots_canonicalize() {
        assert_eq!(normalize_workdir("/home/fenicu/work/"), "/home/fenicu/work");
        assert_eq!(
            normalize_workdir("/home/fenicu/work/../work/x"),
            "/home/fenicu/work/x"
        );
        assert!(excluded_prefix(&["/home/fenicu/work".to_owned()], "/home/fenicu/work/").is_some());
        assert!(excluded_prefix(&["/home/fenicu/work/".to_owned()], "/home/fenicu/work").is_some());
        assert!(
            excluded_prefix(&["/home/fenicu/work".to_owned()], "/home/fenicu/workshop").is_none()
        );
    }

    #[test]
    fn queue_seam_marks_concurrency_and_limits() {
        let busy = vec![Considered {
            harness: "codex".to_owned(),
            model: "gpt-6.1-sol".to_owned(),
            verdict: "parallel_busy".to_owned(),
            reason: "1 of 1 sessions busy".to_owned(),
        }];
        assert_eq!(
            queue_reason(&busy, None, now(), Duration::from_secs(6 * 3600)),
            Some("concurrency")
        );
        let limited = vec![Considered {
            harness: "codex".to_owned(),
            model: "gpt-6.1-sol".to_owned(),
            verdict: "limits_over".to_owned(),
            reason: "codex weekly 83% > 80%".to_owned(),
        }];
        assert_eq!(
            queue_reason(
                &limited,
                Some(now() + chrono::TimeDelta::hours(4)),
                now(),
                Duration::from_secs(6 * 3600)
            ),
            Some("limits")
        );
        let skipped = vec![Considered {
            harness: "codex".to_owned(),
            model: "gpt-6.1-sol".to_owned(),
            verdict: "skipped".to_owned(),
            reason: "not in the owner's list".to_owned(),
        }];
        assert_eq!(
            queue_reason(&skipped, None, now(), Duration::from_secs(6 * 3600)),
            None
        );
        // Сброс дальше горизонта — не очередь, а ошибка.
        assert_eq!(
            queue_reason(
                &limited,
                Some(now() + chrono::TimeDelta::hours(9)),
                now(),
                Duration::from_secs(6 * 3600)
            ),
            None
        );
        // Пауза квоты без срока сброса тоже не лечится очередью.
        assert_eq!(
            queue_reason(&limited, None, now(), Duration::from_secs(6 * 3600)),
            None
        );
    }

    #[test]
    fn unknown_task_lists_available() {
        let error = route(RouteInputs {
            policy: &policy(),
            revision: 0,
            whitelist: &entries(),
            catalogs: &catalogs(),
            limits: &HashMap::new(),
            live: &HashMap::new(),
            stale: Duration::from_secs(1800),
            now: now(),
            request: &request("nope"),
            bypass_limits: false,
        })
        .unwrap_err();
        let RouteError::UnknownTask { available } = error else {
            panic!("expected unknown task");
        };
        assert!(available.contains(&"bugfix".to_owned()));
    }

    #[test]
    fn validate_rejects_bad_policy() {
        let mut policy = builtin();
        policy.task_types.insert(
            "Bad Key".to_owned(),
            TaskType {
                title: "".to_owned(),
                candidates: vec![],
                preset: None,
                isolated: None,
                expected_minutes: DEFAULT_EXPECTED_MINUTES,
            },
        );
        assert!(validate(&policy).is_err());
        let mut policy = builtin();
        let task = policy.task_types.get_mut("bugfix").unwrap();
        task.candidates[0].harness = "nope".to_owned();
        assert!(validate(&policy).is_err());
        let mut policy = builtin();
        let task = policy.task_types.get_mut("bugfix").unwrap();
        task.candidates[0].watchdog_minutes = Some(4);
        assert!(validate(&policy).is_err());
        let mut policy = builtin();
        let task = policy.task_types.get_mut("bugfix").unwrap();
        task.candidates[0].exclude_workdirs = vec!["relative".to_owned()];
        assert!(validate(&policy).is_err());
        assert!(validate(&builtin()).is_ok());
        let _ = json!(null);
    }
}
