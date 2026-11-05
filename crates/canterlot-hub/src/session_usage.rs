//! Расход токенов за всю сессию. Считается по строкам `runs` и `run_model_usage`:
//! одна строка хода может сменить модель, поэтому модели складываются отдельно,
//! а число ходов считается по разговору. Делегаты — живые потомки по
//! `parent_conversation_id` плюс свёрнутые строки удалённых потомков.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool, types::Json};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ModelUsageView {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UsageTotalsView {
    pub turns: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub models: Vec<ModelUsageView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SessionUsageView {
    /// Собственный расход сессии: субагенты внутри CLI уже входят в её ходы.
    pub session: UsageTotalsView,
    /// Все делегаты в поддереве: живые потомки и свёрнутые после удаления.
    pub delegates: UsageTotalsView,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Totals {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
}

impl Totals {
    fn add(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
    }

    fn add_view(&mut self, view: &ModelUsageView) {
        self.add(Self {
            input: view.input_tokens,
            output: view.output_tokens,
            cache_read: view.cache_read_tokens,
            cache_write: view.cache_write_tokens,
        });
    }

    fn weight(self) -> i64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }
}

fn collect(models: &mut BTreeMap<String, Totals>, model: &str, totals: Totals) {
    models.entry(model.to_owned()).or_default().add(totals);
}

fn ordered(models: BTreeMap<String, Totals>) -> Vec<ModelUsageView> {
    let mut rows: Vec<(ModelUsageView, i64)> = models
        .into_iter()
        .map(|(model, totals)| {
            (
                ModelUsageView {
                    model,
                    input_tokens: totals.input,
                    output_tokens: totals.output,
                    cache_read_tokens: totals.cache_read,
                    cache_write_tokens: totals.cache_write,
                },
                totals.weight(),
            )
        })
        .collect();
    rows.sort_by(|(left, left_weight), (right, right_weight)| {
        right_weight
            .cmp(left_weight)
            .then_with(|| left.model.cmp(&right.model))
    });
    rows.into_iter().map(|(row, _)| row).collect()
}

fn totals_view(turns: i64, models: BTreeMap<String, Totals>) -> UsageTotalsView {
    let models = ordered(models);
    let mut sum = Totals::default();
    for row in &models {
        sum.add_view(row);
    }
    UsageTotalsView {
        turns,
        input_tokens: sum.input,
        output_tokens: sum.output,
        cache_read_tokens: sum.cache_read,
        cache_write_tokens: sum.cache_write,
        models,
    }
}

/// Собственные ходы и модели разговора.
async fn own(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<(i64, BTreeMap<String, Totals>), sqlx::Error> {
    let turns: i64 =
        sqlx::query_scalar("select count(*)::bigint from runs where conversation_id=$1")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    let rows: Vec<(String, i64, i64, i64, i64)> = sqlx::query_as(
        "select u.model,
                sum(u.input_tokens)::bigint, sum(u.output_tokens)::bigint,
                sum(u.cache_read_tokens)::bigint, sum(u.cache_write_tokens)::bigint
           from runs r join run_model_usage u using(input_command_id)
          where r.conversation_id=$1
          group by u.model",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let mut models = BTreeMap::new();
    for (model, input, output, cache_read, cache_write) in rows {
        collect(
            &mut models,
            &model,
            Totals {
                input,
                output,
                cache_read,
                cache_write,
            },
        );
    }
    Ok((turns, models))
}

/// Живые потомки любой глубины и их ходы.
async fn live_descendants(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<(i64, BTreeMap<String, Totals>), sqlx::Error> {
    let turns: i64 = sqlx::query_scalar(
        "with recursive tree as (
             select id from conversations where id=$1
             union all
             select c.id from conversations c join tree t on c.parent_conversation_id=t.id
         )
         select count(*)::bigint from runs r join tree t on r.conversation_id=t.id
          where t.id<>$1",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    let rows: Vec<(String, i64, i64, i64, i64)> = sqlx::query_as(
        "with recursive tree as (
             select id from conversations where id=$1
             union all
             select c.id from conversations c join tree t on c.parent_conversation_id=t.id
         )
         select u.model,
                sum(u.input_tokens)::bigint, sum(u.output_tokens)::bigint,
                sum(u.cache_read_tokens)::bigint, sum(u.cache_write_tokens)::bigint
           from runs r
           join run_model_usage u using(input_command_id)
           join tree t on t.id=r.conversation_id
          where t.id<>$1
          group by u.model",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let mut models = BTreeMap::new();
    for (model, input, output, cache_read, cache_write) in rows {
        collect(
            &mut models,
            &model,
            Totals {
                input,
                output,
                cache_read,
                cache_write,
            },
        );
    }
    Ok((turns, models))
}

/// Свёрнутые строки, привязанные к сессии и её живым потомкам. Строка потомка
/// содержит расходы уже удалённых детей, поэтому обход идёт по живому дереву.
async fn folded(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<(i64, BTreeMap<String, Totals>), sqlx::Error> {
    let rows: Vec<(i64, Json<Vec<ModelUsageView>>)> = sqlx::query_as(
        "with recursive tree as (
             select id from conversations where id=$1
             union all
             select c.id from conversations c join tree t on c.parent_conversation_id=t.id
         )
         select turns, models from conversation_usage_rollup
          where parent_id in (select id from tree)",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let mut turns: i64 = 0;
    let mut models = BTreeMap::new();
    for (count, Json(list)) in rows {
        turns = turns.saturating_add(count);
        for row in list {
            collect(
                &mut models,
                &row.model,
                Totals {
                    input: row.input_tokens,
                    output: row.output_tokens,
                    cache_read: row.cache_read_tokens,
                    cache_write: row.cache_write_tokens,
                },
            );
        }
    }
    Ok((turns, models))
}

/// Расход сессии и её делегатов для панели сведений. `None` — разговора нет или он чужой.
pub async fn for_conversation(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
) -> Result<Option<SessionUsageView>, sqlx::Error> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            let known: bool = sqlx::query_scalar(
                "select exists(select 1 from conversations where id=$1 and owner_id=$2 and trashed_at is null)",
            )
            .bind(id)
            .bind(owner_id)
            .fetch_one(&mut *conn)
            .await?;
            if !known {
                return Ok(None);
            }
            let (turns, own_models) = own(&mut *conn, id).await?;
            let (mut delegate_turns, mut delegate_models) =
                live_descendants(&mut *conn, id).await?;
            let (folded_turns, folded_models) = folded(&mut *conn, id).await?;
            delegate_turns = delegate_turns.saturating_add(folded_turns);
            for (model, totals) in folded_models {
                delegate_models.entry(model).or_default().add(totals);
            }
            Ok(Some(SessionUsageView {
                session: totals_view(turns, own_models),
                delegates: totals_view(delegate_turns, delegate_models),
            }))
        })
    })
    .await
}

/// Сворачивает расходы удаляемой сессии на её родителя. Вызывается в транзакции
/// удаления до очистки строк и идемпотентен по ключу `(parent_id, source_id)`;
/// у корневой сессии сворачивать некуда, её строки убирает каскад.
pub async fn fold_in_tx(conn: &mut PgConnection, id: Uuid) -> Result<(), sqlx::Error> {
    let parent: Option<Option<Uuid>> =
        sqlx::query_scalar("select parent_conversation_id from conversations where id=$1")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(Some(parent_id)) = parent else {
        return Ok(());
    };
    let (mut turns, mut models) = own(&mut *conn, id).await?;
    let rows: Vec<(i64, Json<Vec<ModelUsageView>>)> =
        sqlx::query_as("select turns, models from conversation_usage_rollup where parent_id=$1")
            .bind(id)
            .fetch_all(&mut *conn)
            .await?;
    for (count, Json(list)) in rows {
        turns = turns.saturating_add(count);
        for row in list {
            collect(
                &mut models,
                &row.model,
                Totals {
                    input: row.input_tokens,
                    output: row.output_tokens,
                    cache_read: row.cache_read_tokens,
                    cache_write: row.cache_write_tokens,
                },
            );
        }
    }
    sqlx::query(
        "insert into conversation_usage_rollup(parent_id,source_id,turns,models)
         values($1,$2,$3,$4) on conflict(parent_id,source_id) do nothing",
    )
    .bind(parent_id)
    .bind(id)
    .bind(turns)
    .bind(Json(ordered(models)))
    .execute(&mut *conn)
    .await?;
    Ok(())
}
