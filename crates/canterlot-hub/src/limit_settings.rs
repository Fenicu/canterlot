//! Пороги лимитов, которые владелец меняет из PWA: окраска остатка и недельный резерв.
//!
//! Значения хранятся строкой на владельца; строки нет — действуют умолчания из `[limits]`
//! `hub.toml`. Все проценты — остаток, а не расход: «жёлтым, когда осталось меньше 20».

use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::config::LimitsConfig;

/// Остаток ниже этого порога окрашивает полосу в жёлтый.
pub const WARN_REMAINING_DEFAULT: u16 = 20;
/// Остаток ниже этого порога окрашивает полосу в красный.
pub const DANGER_REMAINING_DEFAULT: u16 = 5;
/// Больше 99 не имеет смысла: 100% остатка не бывает после первого же расхода.
pub const MAX_REMAINING_PCT: u16 = 99;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LimitSettings {
    /// Жёлтый цвет, когда остаток меньше этого процента.
    pub warn_remaining_pct: u16,
    /// Красный цвет, когда остаток меньше этого процента.
    pub danger_remaining_pct: u16,
    /// Сколько недельного остатка оставить себе; `0` — без резерва.
    pub reserve_remaining_pct: u16,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct LimitSettingsView {
    pub settings: LimitSettings,
    pub is_default: bool,
    pub default_settings: LimitSettings,
}

/// Умолчания из конфигурации хаба. `weekly_reserve_pct` — расход, поэтому резерв
/// считается как `100 - weekly_reserve_pct`; `warn_pcts` больше не участвует.
pub fn default_settings(config: &LimitsConfig) -> LimitSettings {
    LimitSettings {
        warn_remaining_pct: WARN_REMAINING_DEFAULT,
        danger_remaining_pct: DANGER_REMAINING_DEFAULT,
        reserve_remaining_pct: 100u16
            .saturating_sub(config.weekly_reserve_pct)
            .min(MAX_REMAINING_PCT),
    }
}

impl LimitSettings {
    pub fn valid(self) -> bool {
        self.warn_remaining_pct <= MAX_REMAINING_PCT
            && self.danger_remaining_pct <= MAX_REMAINING_PCT
            && self.reserve_remaining_pct <= MAX_REMAINING_PCT
            && self.danger_remaining_pct <= self.warn_remaining_pct
    }

    /// Порог расхода недели, с которого включается резерв. `None` — резерва нет.
    pub fn reserve_threshold_permille(self) -> Option<i32> {
        if self.reserve_remaining_pct == 0 {
            None
        } else {
            Some(i32::from(100 - self.reserve_remaining_pct) * 10)
        }
    }

    /// Совместимый с клиентами расход в процентах: `0` резерва — это 100% расхода.
    pub fn reserve_used_pct(self) -> u16 {
        100 - self.reserve_remaining_pct
    }
}

async fn stored(
    conn: &mut PgConnection,
    owner_id: Uuid,
) -> Result<Option<LimitSettings>, sqlx::Error> {
    let row: Option<(i16, i16, i16)> = sqlx::query_as(
        "select warn_remaining_pct, danger_remaining_pct, reserve_remaining_pct
           from limit_settings where owner_id=$1",
    )
    .bind(owner_id)
    .fetch_optional(conn)
    .await?;
    row.map(|(warn, danger, reserve)| {
        Ok::<_, sqlx::Error>(LimitSettings {
            warn_remaining_pct: u16::try_from(warn)
                .map_err(|_| sqlx::Error::Decode("invalid limit settings".into()))?,
            danger_remaining_pct: u16::try_from(danger)
                .map_err(|_| sqlx::Error::Decode("invalid limit settings".into()))?,
            reserve_remaining_pct: u16::try_from(reserve)
                .map_err(|_| sqlx::Error::Decode("invalid limit settings".into()))?,
        })
    })
    .transpose()
}

/// Действующие значения: сохранённая строка или умолчания конфигурации.
pub async fn effective(
    conn: &mut PgConnection,
    owner_id: Uuid,
    config: &LimitsConfig,
) -> Result<LimitSettings, sqlx::Error> {
    Ok(stored(conn, owner_id)
        .await?
        .unwrap_or_else(|| default_settings(config)))
}

pub async fn view(
    conn: &mut PgConnection,
    owner_id: Uuid,
    config: &LimitsConfig,
) -> Result<LimitSettingsView, sqlx::Error> {
    let defaults = default_settings(config);
    let (settings, is_default) = match stored(conn, owner_id).await? {
        Some(settings) => (settings, false),
        None => (defaults, true),
    };
    Ok(LimitSettingsView {
        settings,
        is_default,
        default_settings: defaults,
    })
}

/// `None` возвращает умолчания: строка удаляется, а не перезаписывается их копией.
pub async fn replace(
    conn: &mut PgConnection,
    owner_id: Uuid,
    settings: Option<LimitSettings>,
) -> Result<(), sqlx::Error> {
    match settings {
        Some(settings) => {
            sqlx::query(
                "insert into limit_settings(owner_id, warn_remaining_pct, danger_remaining_pct,
                                            reserve_remaining_pct, updated_at)
                 values($1,$2,$3,$4,clock_timestamp())
                 on conflict(owner_id) do update set
                   warn_remaining_pct=excluded.warn_remaining_pct,
                   danger_remaining_pct=excluded.danger_remaining_pct,
                   reserve_remaining_pct=excluded.reserve_remaining_pct,
                   updated_at=excluded.updated_at",
            )
            .bind(owner_id)
            .bind(i16::try_from(settings.warn_remaining_pct).expect("validated percent"))
            .bind(i16::try_from(settings.danger_remaining_pct).expect("validated percent"))
            .bind(i16::try_from(settings.reserve_remaining_pct).expect("validated percent"))
            .execute(conn)
            .await?;
        }
        None => {
            sqlx::query("delete from limit_settings where owner_id=$1")
                .bind(owner_id)
                .execute(conn)
                .await?;
        }
    }
    Ok(())
}
