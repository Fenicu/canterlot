//! Табличные случаи на общей базе и общем приложении.
//!
//! Подготовка теста хаба (клон базы, установка, роутер) стоит дороже большинства самих
//! проверок, поэтому независимые случаи идут подряд на одной подготовке. Каждый случай
//! заводит своих владельцев, машины и разговоры и проверяет только их; общие очереди,
//! которые обходит планировщик, чистит шаг `before`.

/// Метка случая: если случай паникует, в выводе теста остаётся его имя.
pub struct Case(pub &'static str);

impl Drop for Case {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("case failed: {}", self.0);
        }
    }
}

/// Итог случая: `()` или `Result`, ошибка которого валит тест с именем случая.
pub trait Outcome {
    fn check(self, case: &str);
}

impl Outcome for () {
    fn check(self, _case: &str) {}
}

impl<E: std::fmt::Debug> Outcome for Result<(), E> {
    fn check(self, case: &str) {
        if let Err(error) = self {
            panic!("case {case} failed: {error:?}");
        }
    }
}

/// `run_cases!(pool.clone(); a, b)` вызывает `a(pool.clone()).await`, затем `b`.
/// `run_cases!(pool.clone(), before = clean; a, b)` перед каждым случаем ждёт
/// `clean(pool.clone())`. Упавший случай называет себя в выводе.
// Поддержка общая для всех бинарников хаба, а таблицы есть не в каждом.
#[allow(unused_macros)]
macro_rules! run_cases {
    ($fx:expr; $($case:ident),+ $(,)?) => {
        $({
            let _case = $crate::support::cases::Case(stringify!($case));
            $crate::support::cases::Outcome::check($case($fx).await, stringify!($case));
        })+
    };
    ($fx:expr, before = $before:path; $($case:ident),+ $(,)?) => {
        $({
            let _case = $crate::support::cases::Case(stringify!($case));
            $before($fx).await;
            $crate::support::cases::Outcome::check($case($fx).await, stringify!($case));
        })+
    };
}
#[allow(unused_imports)]
pub(crate) use run_cases;

/// Число кругов вероятностного теста: полное в профиле nextest `stress`, короткое в
/// обычном гейте. `CANTERLOT_STRESS_ROUNDS` перекрывает оба значения.
pub fn stress_rounds(full: u32, gate: u32) -> u32 {
    if let Some(rounds) = std::env::var("CANTERLOT_STRESS_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        return rounds;
    }
    if std::env::var("NEXTEST_PROFILE").as_deref() == Ok("stress") {
        full
    } else {
        gate
    }
}

/// Состояние базы после подготовки таблицы: перед случаем `restore` возвращает базу к
/// нему, как будто случай получил свой клон. Дешевле клона: удаление и вставка идут
/// одной функцией `_snapshot.restore()` в базе, а роутер и зависимости приложения
/// остаются.
///
/// Память процесса (реестр машин, кэши приложения) остаётся от прошлого случая: её
/// таблица возвращает сама. Схему снимок тоже не возвращает: случай, который ставит
/// триггер или ограничение, снимает его сам.
pub struct Snapshot;

impl Snapshot {
    pub async fn take(pool: &sqlx::PgPool) -> Self {
        use std::collections::{BTreeMap, BTreeSet};
        let tables: Vec<String> = sqlx::query_scalar(
            "select tablename::text from pg_tables
              where schemaname = 'public' and tablename <> '_sqlx_migrations'
              order by 1",
        )
        .fetch_all(pool)
        .await
        .expect("snapshot tables");
        let edges: Vec<(String, String)> = sqlx::query_as(
            "select c.relname::text, p.relname::text from pg_constraint k
               join pg_class c on c.oid = k.conrelid
               join pg_class p on p.oid = k.confrelid
               join pg_namespace n on n.oid = c.relnamespace
              where k.contype = 'f' and n.nspname = 'public' and k.conrelid <> k.confrelid",
        )
        .fetch_all(pool)
        .await
        .expect("snapshot foreign keys");
        let mut children: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (child, parent) in &edges {
            children.entry(parent).or_default().insert(child);
        }
        // Порядок «дети раньше родителей»: удаление идёт по нему, вставка — обратно.
        fn visit<'a>(
            table: &'a str,
            children: &BTreeMap<&'a str, BTreeSet<&'a str>>,
            seen: &mut BTreeSet<&'a str>,
            order: &mut Vec<&'a str>,
        ) {
            if !seen.insert(table) {
                return;
            }
            for child in children.get(table).into_iter().flatten() {
                visit(child, children, seen, order);
            }
            order.push(table);
        }
        let mut seen = BTreeSet::new();
        let mut order = Vec::new();
        for table in &tables {
            visit(table, &children, &mut seen, &mut order);
        }
        sqlx::query("create schema _snapshot")
            .execute(pool)
            .await
            .expect("snapshot schema");
        let mut restore = String::new();
        for table in &order {
            restore.push_str(&format!("delete from public.\"{table}\";\n"));
        }
        for table in order.iter().rev() {
            let rows: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "select exists(select 1 from public.\"{table}\")"
            )))
            .fetch_one(pool)
            .await
            .expect("snapshot rows");
            if !rows {
                continue;
            }
            let columns: Vec<String> = sqlx::query_scalar(
                "select format('%I', column_name) from information_schema.columns
                  where table_schema = 'public' and table_name = $1 and is_generated = 'NEVER'
                  order by ordinal_position",
            )
            .bind(table)
            .fetch_all(pool)
            .await
            .expect("snapshot columns");
            let columns = columns.join(", ");
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "create table _snapshot.\"{table}\" as select {columns} from public.\"{table}\""
            )))
            .execute(pool)
            .await
            .expect("snapshot copy");
            restore.push_str(&format!(
                "insert into public.\"{table}\" ({columns}) overriding system value \
                 select {columns} from _snapshot.\"{table}\";\n"
            ));
        }
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "create function _snapshot.restore() returns void language plpgsql as $snapshot$\n\
             begin\n{restore}end\n$snapshot$"
        )))
        .execute(pool)
        .await
        .expect("snapshot function");
        Self
    }

    /// Возвращает базу к снимку одной транзакцией.
    pub async fn restore(&self, pool: &sqlx::PgPool) {
        restore(pool.clone()).await;
    }
}

/// Шаг `before` таблицы на голом пуле: база возвращается к снимку `Snapshot::take`.
pub async fn restore(pool: sqlx::PgPool) {
    sqlx::query("select _snapshot.restore()")
        .execute(&pool)
        .await
        .expect("snapshot restore");
}
