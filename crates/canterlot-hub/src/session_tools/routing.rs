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

/// Тип задачи: русское название для PWA и упорядоченные кандидаты.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskType {
    pub title: String,
    pub candidates: Vec<Candidate>,
    pub preset: Option<String>,
    pub isolated: Option<bool>,
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
        exclude_workdirs: exclude_work
            .then(|| vec!["/home/fenicu/work".to_owned()])
            .unwrap_or_default(),
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
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
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

/// Чистая функция выбора (§5.2 + §C1 пп. 5–7): уровень, канонизация через каталог,
/// белый список, совместимость режима, лимиты, параллельность, `exclude_workdirs`.
/// Первый прошедший кандидат выбран.
pub fn route(
    policy: &Policy,
    revision: i64,
    whitelist: &[WhitelistEntry],
    catalogs: &HashMap<String, HarnessCatalog>,
    limits: &HashMap<String, ProviderView>,
    live: &HashMap<(String, String), i64>,
    stale: Duration,
    now: DateTime<Utc>,
    request: &RouteRequest,
) -> Result<Decision, RouteError> {
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
        if !unlimited
            && let Some((verdict, reason)) = limits_verdict(
                policy,
                &provider,
                limits.get(&provider),
                stale,
                now,
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

fn check_window(
    provider: &str,
    window: &str,
    threshold: Option<f64>,
    reading: Option<&WindowReading>,
    unknown: UnknownLimits,
    stale: Duration,
    now: DateTime<Utc>,
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
    if let Some(reset) = reading.resets_at
        && reset > now
        && nearest_reset.is_none_or(|known| reset < known)
    {
        *nearest_reset = Some(reset);
    }
    let age = now.signed_duration_since(reading.observed_at);
    if age.to_std().is_ok_and(|age| age > stale) {
        return match unknown {
            UnknownLimits::Allow => None,
            UnknownLimits::Skip => Some((
                "limits_unknown",
                format!(
                    "{} {} limits unknown (observed {})",
                    provider,
                    window_name(window),
                    ago(age)
                ),
            )),
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
    None
}

/// Проверка лимитов: пороги, резерв и пауза. Возвращает вердикт и причину пропуска.
fn limits_verdict(
    policy: &Policy,
    provider: &str,
    view: Option<&ProviderView>,
    stale: Duration,
    now: DateTime<Utc>,
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
/// не позже горизонта. C1 всегда отвечает ошибкой, но решение уже размечено.
pub fn queue_reason(
    considered: &[Considered],
    now: DateTime<Utc>,
    horizon: Duration,
) -> Option<&'static str> {
    let mut limits = false;
    for entry in considered {
        match entry.verdict.as_str() {
            "parallel_busy" => return Some("concurrency"),
            "limits_over" | "quota_paused" => limits = true,
            _ => {}
        }
    }
    if limits {
        let _ = (now, horizon);
        return Some("limits");
    }
    None
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
          where c.owner_id=$1 and c.parent_conversation_id is not null
            and (d.activity_state in ('working','stalled')
              or (d.activity_state='idle' and d.queue_queued > 0)
              or exists(select 1 from nudge_rules r
                        where r.conversation_id=c.id and r.state='pending'))
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

fn preview_description() -> &'static str {
    "Show which executor the owner's routing policy would pick right now for a task type, with the reason for every candidate, current limits and machine headroom. Without task_type, lists the task types. Read-only; to start the session, call session_spawn with route."
}

/// `routing.preview`: список типов без `task_type`, иначе решение и запас машины.
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
    let existing = policy.task_types.contains_key(task_type);
    match route(
        &policy,
        revision,
        &list.entries,
        &catalogs,
        &limits,
        &live,
        context.routing_stale,
        now,
        &request,
    ) {
        Ok(decision) => {
            tx.commit().await?;
            Ok(serde_json::json!({
                "decision": decision,
                "machine": headroom,
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

fn no_executor_message(considered: &[Considered], reset: Option<DateTime<Utc>>) -> String {
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

pub fn description() -> &'static str {
    preview_description()
}
