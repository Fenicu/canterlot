//! Тестовая база: клон шаблона с миграциями хаба.
//!
//! `#[sqlx::test]` на каждый тест создаёт пустую базу и прогоняет все миграции. Здесь
//! миграции применяются один раз в шаблон, а тест получает `CREATE DATABASE … TEMPLATE`.
//!
//! - Шаблон называется по хэшу набора миграций (версия, описание, контрольная сумма), так
//!   что изменённая миграция даёт новый шаблон. Шаблон живёт между прогонами.
//! - Первый тест строит шаблон под advisory-замком; nextest запускает каждый тест своим
//!   процессом, остальные ждут замок и видят готовый шаблон. Готовность — флаг
//!   `datistemplate`: его ставит строитель после миграций, недостроенный шаблон упавшего
//!   процесса следующий строитель пересоздаёт.
//! - Клон называется по тесту, как у `sqlx::test`: остаток после убитого процесса удаляет
//!   следующий прогон того же теста.
//! - Клон удаляется, когда завершается поток теста: и после успеха, и после паники. К этому
//!   моменту `#[tokio::test]` уже уничтожил свой рантайм и закрыл все соединения теста, а
//!   libtest ждёт поток, прежде чем сообщить итог. Удалять раньше, из `Drop` внутри
//!   рантайма, нельзя: заблокированный рантайм оставляет недоделанное подключение пула в
//!   аутентификации, а `DROP DATABASE` ждёт ProcSignalBarrier от всех процессов сервера,
//!   в том числе от него. Так весь сервер вставал на `authentication_timeout` (60 с).
//! - Строитель нового шаблона удаляет старые: остаются текущий и один предыдущий.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::future::Future;
use std::panic::Location;
use std::str::FromStr as _;
use std::time::Duration;

use sha2::{Digest as _, Sha256};
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection as _, Executor as _, PgConnection, PgPool};

/// Восемь байт `cnltmplt`: замок строителей шаблона во всех тестовых процессах.
const LOCK_KEY: i64 = i64::from_be_bytes(*b"cnltmplt");
const TEMPLATE_PREFIX: &str = "canterlot_tpl_";
const CLONE_PREFIX: &str = "canterlot_test_";
/// Повторы `CREATE DATABASE`: шаблон удалили между проверкой и клоном или к нему ещё
/// подключён строитель.
const CLONE_ATTEMPTS: u32 = 20;

/// Клоны, созданные в этом потоке. Деструктор потока удаляет их.
struct Clones(RefCell<Vec<(String, PgConnectOptions)>>);

impl Drop for Clones {
    fn drop(&mut self) {
        for (name, master) in self.0.take() {
            drop_clone(name, master);
        }
    }
}

thread_local! {
    static CLONES: Clones = const { Clones(RefCell::new(Vec::new())) };
}

/// Записывает клон к удалению при завершении потока. Если поток уже завершается, удаляет
/// сразу: рантайма теста тогда уже нет.
fn remove_at_thread_exit(name: String, master: PgConnectOptions) {
    let mut item = Some((name, master));
    let _ = CLONES.try_with(|clones| clones.0.borrow_mut().extend(item.take()));
    if let Some((name, master)) = item {
        drop_clone(name, master);
    }
}

/// Удаляет клон своим потоком со своим рантаймом: деструктор потока не может опираться
/// на рантайм теста. FORCE закрывает соединения, если пул всё же пережил рантайм.
fn drop_clone(name: String, master: PgConnectOptions) {
    let dropped = std::thread::spawn({
        let name = name.clone();
        move || -> Result<(), sqlx::Error> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async move {
                let mut conn = PgConnection::connect_with(&master).await?;
                conn.execute(sqlx::AssertSqlSafe(format!(
                    "drop database if exists {} with (force)",
                    ident(&name)
                )))
                .await?;
                conn.close().await
            })
        }
    })
    .join();
    match dropped {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("failed to delete test database {name:?}: {error}"),
        Err(_) => eprintln!("failed to delete test database {name:?}: cleanup thread panicked"),
    }
}

/// База для теста: `let pool = testkit::db().await;` первой строкой `#[tokio::test]`.
///
/// Замена `#[sqlx::test(migrator = …)]`: схема та же, что даёт `crate::db::MIGRATOR`,
/// пул такой же, как у `sqlx::test` (до 5 соединений, простой соединения 1 с). База живёт
/// до конца потока теста.
#[track_caller]
pub fn db() -> impl Future<Output = PgPool> {
    let identity = test_identity(Location::caller());
    async move {
        let setup = Setup::from_env(&crate::db::MIGRATOR);
        let name = clone_name(&identity);
        setup
            .open(&name)
            .await
            .unwrap_or_else(|error| panic!("test database {name} for {identity}: {error}"))
    }
}

/// Имя теста для имени клона: бинарник без хэша cargo, поток libtest (имя теста) и место
/// вызова `db()`.
fn test_identity(location: &Location<'_>) -> String {
    let binary = std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .map(|name| strip_cargo_hash(&name).to_owned())
        .unwrap_or_default();
    let thread = std::thread::current();
    format!(
        "{binary}::{}@{}:{}:{}",
        thread.name().unwrap_or_default(),
        location.file(),
        location.line(),
        location.column()
    )
}

/// `it_sessions-0123456789abcdef` → `it_sessions`: хэш меняется с каждой сборкой.
fn strip_cargo_hash(name: &str) -> &str {
    match name.rsplit_once('-') {
        Some((stem, hash)) if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
            stem
        }
        _ => name,
    }
}

fn clone_name(identity: &str) -> String {
    format!(
        "{CLONE_PREFIX}{}",
        &hex(&Sha256::digest(identity.as_bytes()))[..40]
    )
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Хэш набора миграций: версия, описание, тип, `no_tx` и контрольная сумма каждой.
fn migrations_hash(migrator: &Migrator) -> String {
    let mut hasher = Sha256::new();
    for migration in migrator.iter() {
        hasher.update(migration.version.to_be_bytes());
        hasher.update(migration.description.as_bytes());
        hasher.update([0]);
        hasher.update(format!("{:?}", migration.migration_type).as_bytes());
        hasher.update([u8::from(migration.no_tx)]);
        hasher.update(&migration.checksum[..]);
    }
    hex(&hasher.finalize())[..32].to_owned()
}

/// Параметры шаблона. Свои тесты помощника берут другой префикс и замок, чтобы не трогать
/// настоящий шаблон.
pub(super) struct Setup<'m> {
    master: PgConnectOptions,
    migrator: &'m Migrator,
    prefix: String,
    lock_key: i64,
}

impl<'m> Setup<'m> {
    fn from_env(migrator: &'m Migrator) -> Self {
        // Как у sqlx::test: служебная база из DATABASE_URL создаёт и удаляет тестовые.
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let master = PgConnectOptions::from_str(&url).expect("failed to parse DATABASE_URL");
        Self {
            master,
            migrator,
            prefix: TEMPLATE_PREFIX.to_owned(),
            lock_key: LOCK_KEY,
        }
    }

    fn template(&self) -> String {
        let name = format!("{}{}", self.prefix, migrations_hash(self.migrator));
        // PostgreSQL молча обрезает имя до 63 байт: разные наборы совпали бы.
        assert!(name.len() <= 63, "template name too long: {name}");
        name
    }

    /// Клон `name` из шаблона и пул к нему.
    async fn open(&self, name: &str) -> Result<PgPool, sqlx::Error> {
        let template = self.template();
        let mut conn = PgConnection::connect_with(&self.master).await?;
        let mut attempt = 0;
        loop {
            self.ensure_template(&mut conn, &template).await?;
            // Остаток прошлого прогона того же теста (процесс убит до drop), как у sqlx::test.
            conn.execute(sqlx::AssertSqlSafe(format!(
                "drop database if exists {}",
                ident(name)
            )))
            .await?;
            let created = conn
                .execute(sqlx::AssertSqlSafe(format!(
                    "create database {} template {}",
                    ident(name),
                    ident(&template)
                )))
                .await;
            match created {
                Ok(_) => break,
                Err(error) if attempt < CLONE_ATTEMPTS && retryable(&error) => {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(50 * u64::from(attempt))).await;
                }
                Err(error) => return Err(error),
            }
        }
        let _ = conn.close().await;
        remove_at_thread_exit(name.to_owned(), self.master.clone());
        PgPoolOptions::new()
            .max_connections(5)
            .idle_timeout(Some(Duration::from_secs(1)))
            .connect_with(self.master.clone().database(name))
            .await
    }

    /// Готовит шаблон, если его нет. `true`, если шаблон построил этот вызов.
    async fn ensure_template(
        &self,
        conn: &mut PgConnection,
        template: &str,
    ) -> Result<bool, sqlx::Error> {
        if template_ready(conn, template).await? {
            return Ok(false);
        }
        sqlx::query("select pg_advisory_lock($1)")
            .bind(self.lock_key)
            .execute(&mut *conn)
            .await?;
        let built = self.build_locked(conn, template).await;
        let unlocked = sqlx::query("select pg_advisory_unlock($1)")
            .bind(self.lock_key)
            .execute(&mut *conn)
            .await;
        let built = built?;
        unlocked?;
        Ok(built)
    }

    async fn build_locked(
        &self,
        conn: &mut PgConnection,
        template: &str,
    ) -> Result<bool, sqlx::Error> {
        // Пока ждали замок, шаблон мог построить другой процесс.
        if template_ready(conn, template).await? {
            return Ok(false);
        }
        // Недостроенный шаблон упавшего строителя: флага нет, строим заново.
        conn.execute(sqlx::AssertSqlSafe(format!(
            "drop database if exists {} with (force)",
            ident(template)
        )))
        .await?;
        conn.execute(sqlx::AssertSqlSafe(format!(
            "create database {}",
            ident(template)
        )))
        .await?;
        let mut target =
            PgConnection::connect_with(&self.master.clone().database(template)).await?;
        self.migrator
            .run(&mut target)
            .await
            .map_err(|error| sqlx::Error::Migrate(Box::new(error)))?;
        target.close().await?;
        conn.execute(sqlx::AssertSqlSafe(format!(
            "alter database {} with is_template true allow_connections false",
            ident(template)
        )))
        .await?;
        self.drop_stale(conn, template).await?;
        Ok(true)
    }

    /// Оставляет текущий шаблон и один предыдущий, остальные с этим префиксом удаляет.
    /// Порядок задаёт время постройки в `_canterlot_test.templates`.
    async fn drop_stale(&self, conn: &mut PgConnection, current: &str) -> Result<(), sqlx::Error> {
        // Схема общая для всех наборов миграций, а замок постройки у каждого свой:
        // `if not exists` при одновременном создании даёт 23505, поэтому свой замок.
        conn.execute(
            "select pg_advisory_xact_lock(7161679214410133360);
             create schema if not exists _canterlot_test;
             create table if not exists _canterlot_test.templates (
                 name text primary key,
                 built_at timestamptz not null default clock_timestamp()
             );",
        )
        .await?;
        sqlx::query(
            "insert into _canterlot_test.templates(name) values($1)
             on conflict(name) do update set built_at = clock_timestamp()",
        )
        .bind(current)
        .execute(&mut *conn)
        .await?;
        let others: Vec<String> = sqlx::query_scalar(
            "select d.datname::text from pg_database d
               left join _canterlot_test.templates t on t.name = d.datname
              where starts_with(d.datname, $1) and d.datname <> $2
              order by t.built_at desc nulls last, d.oid desc",
        )
        .bind(&self.prefix)
        .bind(current)
        .fetch_all(&mut *conn)
        .await?;
        for stale in others.iter().skip(1) {
            // Клон из удаляемого шаблона держит его замок: drop дождётся конца клона,
            // а тест того набора миграций перестроит шаблон по 3D000.
            if let Err(error) = drop_template(conn, stale).await {
                eprintln!("failed to drop stale test template {stale}: {error}");
            }
        }
        sqlx::query(
            "delete from _canterlot_test.templates t
              where not exists (select 1 from pg_database d where d.datname = t.name)",
        )
        .execute(&mut *conn)
        .await?;
        Ok(())
    }
}

/// Снимает флаг шаблона и удаляет базу. `DROP DATABASE` не идёт в одной строке с другими
/// командами: несколько команд в простом запросе — неявная транзакция.
async fn drop_template(conn: &mut PgConnection, name: &str) -> Result<(), sqlx::Error> {
    conn.execute(sqlx::AssertSqlSafe(format!(
        "alter database {} with is_template false",
        ident(name)
    )))
    .await?;
    conn.execute(sqlx::AssertSqlSafe(format!(
        "drop database if exists {}",
        ident(name)
    )))
    .await?;
    Ok(())
}

async fn template_ready(conn: &mut PgConnection, template: &str) -> Result<bool, sqlx::Error> {
    let ready: Option<bool> =
        sqlx::query_scalar("select datistemplate from pg_database where datname = $1")
            .bind(template)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(ready == Some(true))
}

/// 3D000: шаблон удалили (его набор миграций устарел для другого прогона) — следующий
/// круг построит его снова. 55006: к шаблону ещё подключён строитель.
fn retryable(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .is_some_and(|code| code == "3D000" || code == "55006")
}

/// Имена баз собраны из префикса и hex, кавычки только на случай префикса с заглавными.
fn ident(name: &str) -> String {
    debug_assert!(!name.contains('"'));
    format!("\"{name}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    /// Свой префикс и замок на процесс: тесты помощника не трогают настоящий шаблон
    /// и друг друга.
    fn self_test_setup<'m>(migrator: &'m Migrator, tag: &str) -> Setup<'m> {
        let mut setup = Setup::from_env(migrator);
        setup.prefix = format!("tplself_{tag}_{}_", std::process::id());
        setup.lock_key = LOCK_KEY ^ i64::from(std::process::id());
        setup
    }

    fn first_migrations(count: usize) -> Migrator {
        Migrator {
            migrations: Cow::Owned(
                crate::db::MIGRATOR
                    .iter()
                    .take(count)
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
            ..Migrator::DEFAULT
        }
    }

    async fn master(setup: &Setup<'_>) -> PgConnection {
        PgConnection::connect_with(&setup.master).await.unwrap()
    }

    async fn databases_with_prefix(conn: &mut PgConnection, prefix: &str) -> Vec<String> {
        sqlx::query_scalar(
            "select datname::text from pg_database where starts_with(datname, $1) order by 1",
        )
        .bind(prefix)
        .fetch_all(&mut *conn)
        .await
        .unwrap()
    }

    async fn drop_prefix(setup: &Setup<'_>) {
        let mut conn = master(setup).await;
        for name in databases_with_prefix(&mut conn, &setup.prefix).await {
            drop_template(&mut conn, &name).await.unwrap();
        }
    }

    /// Имя клона для своих тестов: вне префикса шаблонов, иначе его снесёт уборка старых.
    fn self_test_clone(setup: &Setup<'_>) -> String {
        clone_name(&format!("self-test::{}", setup.prefix))
    }

    async fn exists(conn: &mut PgConnection, name: &str) -> bool {
        sqlx::query_scalar("select exists(select 1 from pg_database where datname = $1)")
            .bind(name)
            .fetch_one(&mut *conn)
            .await
            .unwrap()
    }

    async fn applied(pool: &PgPool) -> i64 {
        sqlx::query_scalar("select count(*) from _sqlx_migrations")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Тело теста в своём потоке со своим рантаймом, как у `#[tokio::test]`: возвращает
    /// имя его базы и признак паники.
    fn run_in_test_thread(panic: bool) -> (String, bool) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let joined = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let pool = db().await;
                let name: String = sqlx::query_scalar("select current_database()::text")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                // Пул жив в фоновой задаче, как у теста, отдавшего его хабу: тело кончается,
                // пока задача держит соединения и ходит в базу.
                let (busy, is_busy) = tokio::sync::oneshot::channel();
                tokio::spawn(async move {
                    let mut busy = Some(busy);
                    loop {
                        let _ = sqlx::query("select 1").execute(&pool).await;
                        if let Some(busy) = busy.take() {
                            let _ = busy.send(());
                        }
                    }
                });
                is_busy.await.unwrap();
                sender.send(name).unwrap();
                assert!(!panic, "test body panics");
            })
        })
        .join();
        (receiver.recv().unwrap(), joined.is_err())
    }

    #[test]
    fn names_fit_postgres_and_follow_the_migration_set() {
        let name = clone_name("it_sessions::sessions::a@tests/it/sessions.rs:1:1");
        assert!(name.len() <= 63, "{name}");
        assert_ne!(
            name,
            clone_name("it_sessions::sessions::b@tests/it/sessions.rs:1:1")
        );
        assert_eq!(
            strip_cargo_hash("it_sessions-0123456789abcdef"),
            "it_sessions"
        );
        assert_eq!(
            strip_cargo_hash("canterlot_hub-0123456789abcdef"),
            "canterlot_hub"
        );
        assert_eq!(strip_cargo_hash("it"), "it");

        let full = migrations_hash(&crate::db::MIGRATOR);
        assert_eq!(full, migrations_hash(&crate::db::MIGRATOR));
        assert_ne!(full, migrations_hash(&first_migrations(3)));
        let mut changed = first_migrations(3);
        changed.migrations.to_mut()[2].checksum = Cow::Owned(vec![0; 48]);
        assert_ne!(
            migrations_hash(&first_migrations(3)),
            migrations_hash(&changed),
            "a changed migration gets a new template"
        );
        assert!(Setup::from_env(&crate::db::MIGRATOR).template().len() <= 63);
    }

    #[tokio::test]
    async fn racing_first_tests_build_the_template_once() {
        let migrator = first_migrations(20);
        let setup = self_test_setup(&migrator, "race");
        let template = setup.template();
        let racers = (0..8).map(|_| async {
            let mut conn = master(&setup).await;
            setup.ensure_template(&mut conn, &template).await.unwrap()
        });
        let built = futures_util::future::join_all(racers).await;
        assert_eq!(built.iter().filter(|built| **built).count(), 1, "{built:?}");

        let mut conn = master(&setup).await;
        assert!(template_ready(&mut conn, &template).await.unwrap());
        assert!(!setup.ensure_template(&mut conn, &template).await.unwrap());
        drop_prefix(&setup).await;
    }

    #[tokio::test]
    async fn half_built_template_is_rebuilt() {
        let migrator = first_migrations(5);
        let setup = self_test_setup(&migrator, "half");
        let template = setup.template();
        let mut conn = master(&setup).await;
        // Процесс умер после create database, до миграций и флага шаблона.
        conn.execute(sqlx::AssertSqlSafe(format!(
            "create database {}",
            ident(&template)
        )))
        .await
        .unwrap();
        assert!(setup.ensure_template(&mut conn, &template).await.unwrap());
        let pool = setup.open(&self_test_clone(&setup)).await.unwrap();
        assert_eq!(applied(&pool).await, 5);
        pool.close().await;
        drop_prefix(&setup).await;
    }

    #[tokio::test]
    async fn clones_are_isolated_and_carry_every_migration() {
        let first = db().await;
        let second = db().await;
        for pool in [&first, &second] {
            assert_eq!(
                applied(pool).await,
                crate::db::MIGRATOR.iter().count() as i64
            );
        }
        sqlx::query("create table only_in_first(id int)")
            .execute(&first)
            .await
            .unwrap();
        let in_second: Option<String> =
            sqlx::query_scalar("select to_regclass('public.only_in_first')::text")
                .fetch_one(&second)
                .await
                .unwrap();
        assert_eq!(in_second, None);
    }

    #[tokio::test]
    async fn the_clone_is_removed_after_the_test_and_after_a_panic() {
        let setup = Setup::from_env(&crate::db::MIGRATOR);
        let mut conn = master(&setup).await;
        for panic in [false, true] {
            let (name, panicked) = run_in_test_thread(panic);
            assert_eq!(panicked, panic);
            assert!(name.starts_with(CLONE_PREFIX), "{name}");
            assert!(!exists(&mut conn, &name).await, "{name} outlived its test");
        }
    }

    #[tokio::test]
    async fn leftover_clone_of_a_killed_run_is_replaced() {
        let setup = Setup::from_env(&crate::db::MIGRATOR);
        let name = clone_name("self-test::leftover");
        let mut conn = master(&setup).await;
        conn.execute(sqlx::AssertSqlSafe(format!(
            "drop database if exists {} with (force)",
            ident(&name)
        )))
        .await
        .unwrap();
        conn.execute(sqlx::AssertSqlSafe(format!(
            "create database {}",
            ident(&name)
        )))
        .await
        .unwrap();
        let pool = setup.open(&name).await.unwrap();
        assert!(applied(&pool).await > 0);
    }

    #[tokio::test]
    async fn new_template_keeps_one_previous_and_drops_older() {
        let one = first_migrations(1);
        let two = first_migrations(2);
        let three = first_migrations(3);
        let setups: Vec<Setup<'_>> = [&one, &two, &three]
            .iter()
            .map(|migrator| self_test_setup(migrator, "stale"))
            .collect();
        let names: Vec<String> = setups.iter().map(Setup::template).collect();
        let prefix = setups[0].prefix.clone();
        let mut conn = master(&setups[0]).await;

        for (setup, name) in setups.iter().zip(&names) {
            assert!(setup.ensure_template(&mut conn, name).await.unwrap());
        }
        let mut expected = vec![names[1].clone(), names[2].clone()];
        expected.sort();
        assert_eq!(databases_with_prefix(&mut conn, &prefix).await, expected);

        // Возврат к первому набору: остаются он и последний из других (третий).
        assert!(
            setups[0]
                .ensure_template(&mut conn, &names[0])
                .await
                .unwrap()
        );
        let mut expected = vec![names[0].clone(), names[2].clone()];
        expected.sort();
        assert_eq!(databases_with_prefix(&mut conn, &prefix).await, expected);

        // Шаблон удалил прогон с другим набором миграций: следующий клон строит его заново.
        drop_template(&mut conn, &names[2]).await.unwrap();
        let pool = setups[2].open(&self_test_clone(&setups[2])).await.unwrap();
        assert_eq!(applied(&pool).await, 3);
        pool.close().await;
        drop_prefix(&setups[0]).await;
    }
}
