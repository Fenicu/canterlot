//! База хаба во время обновления: дамп, восстановление в отдельную базу, обмен имён,
//! сверка миграций. Живая база `canterlot` никогда не удаляется: удалить можно только
//! служебную базу отката, а её имя проверяет конструктор `ScratchDb`.

use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::{
    error::CtlError,
    layout::Layout,
    ops::OpId,
    runner::{Cmd, Runner},
    stack::{cmd, compose, psql, run_ok, stdout},
};

pub const LIVE_DB: &str = "canterlot";

/// Имя служебной базы отката: `canterlot_restore_<op>` или `canterlot_failed_<op>`, где
/// `<op>` — 32 hex идентификатора операции. Другие имена, в том числе `canterlot`, сюда
/// не попадают, поэтому `drop_scratch` не может удалить живую базу.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScratchDb(String);

impl ScratchDb {
    fn of(kind: &str, op: &OpId) -> Result<Self, CtlError> {
        let hex: String = op.0.chars().filter(|c| *c != '-').collect();
        Self::parse(&format!("canterlot_{kind}_{}", hex.to_ascii_lowercase()))
    }

    pub fn restore(op: &OpId) -> Result<Self, CtlError> {
        Self::of("restore", op)
    }

    pub fn failed(op: &OpId) -> Result<Self, CtlError> {
        Self::of("failed", op)
    }

    pub fn parse(name: &str) -> Result<Self, CtlError> {
        let suffix = name
            .strip_prefix("canterlot_restore_")
            .or_else(|| name.strip_prefix("canterlot_failed_"));
        match suffix {
            Some(hex)
                if hex.len() == 32
                    && hex
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
            {
                Ok(Self(name.to_owned()))
            }
            _ => Err(CtlError::BadName(format!("not a scratch database: {name}"))),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Команда, у которой stdout или stdin перенаправлены в файл хоста. Дамп не проходит
/// через память утилиты; путь — отдельный аргумент `sh`, не часть текста скрипта.
fn redirect(file: &Path, input: bool, inner: Cmd) -> Cmd {
    let script = if input {
        "f=$1; shift; exec \"$@\" < \"$f\""
    } else {
        "f=$1; shift; umask 077; exec \"$@\" > \"$f\""
    };
    let mut args = vec![
        "-c".to_owned(),
        script.to_owned(),
        "sh".to_owned(),
        file.display().to_string(),
        inner.program,
    ];
    args.extend(inner.args);
    let mut out = cmd("sh", &[]);
    out.args = args;
    out
}

fn pg_tool(layout: &Layout, tool: &str, args: &[&str]) -> Cmd {
    let mut all = vec!["exec", "-T", "postgres", tool, "-U", "postgres"];
    all.extend_from_slice(args);
    compose(layout, &all)
}

/// Файл рядом с дампом, в который он пишется до проверки.
pub fn partial_path(dump: &Path) -> PathBuf {
    let mut name = dump.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    dump.with_file_name(name)
}

fn remove_if_exists(path: &Path) -> Result<(), CtlError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// `pg_dump -Fc` живой базы в `dump`: сначала в `.partial`, после проверки чтения —
/// переименование. Неудача удаляет недописанный файл, чтобы не держать занятое место.
pub async fn dump(runner: &dyn Runner, layout: &Layout, dump: &Path) -> Result<(), CtlError> {
    let partial = partial_path(dump);
    remove_if_exists(&partial)?;
    let inner = pg_tool(layout, "pg_dump", &["-Fc", "-d", LIVE_DB]);
    let result = async {
        run_ok(runner, "pg_dump", redirect(&partial, false, inner)).await?;
        verify_dump(runner, layout, &partial).await
    }
    .await;
    if let Err(err) = result {
        remove_if_exists(&partial)?;
        return Err(err);
    }
    fs::rename(&partial, dump)?;
    if let Some(parent) = dump.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Дамп читается целиком, и в его оглавлении есть таблица миграций.
pub async fn verify_dump(
    runner: &dyn Runner,
    layout: &Layout,
    dump: &Path,
) -> Result<(), CtlError> {
    let list = run_ok(
        runner,
        "pg_restore --list",
        redirect(dump, true, pg_tool(layout, "pg_restore", &["--list"])),
    )
    .await?;
    if !String::from_utf8_lossy(&list.stdout).contains("_sqlx_migrations") {
        return Err(CtlError::BadOutput(
            "dump table of contents has no _sqlx_migrations".into(),
        ));
    }
    run_ok(
        runner,
        "pg_restore read",
        redirect(
            dump,
            true,
            pg_tool(layout, "pg_restore", &["-f", "/dev/null"]),
        ),
    )
    .await?;
    Ok(())
}

/// Восстановление дампа в уже созданную служебную базу; живая база не затрагивается.
pub async fn restore_into(
    runner: &dyn Runner,
    layout: &Layout,
    dump: &Path,
    db: &ScratchDb,
) -> Result<(), CtlError> {
    run_ok(
        runner,
        "pg_restore",
        redirect(
            dump,
            true,
            pg_tool(
                layout,
                "pg_restore",
                &["--exit-on-error", "-d", db.as_str()],
            ),
        ),
    )
    .await?;
    Ok(())
}

pub async fn create_scratch(
    runner: &dyn Runner,
    layout: &Layout,
    db: &ScratchDb,
) -> Result<(), CtlError> {
    let sql = format!(
        "create database \"{}\" owner canterlot encoding 'UTF8' template template0;\n",
        db.as_str()
    );
    run_ok(
        runner,
        "psql create database",
        psql(layout, "postgres", &sql),
    )
    .await?;
    Ok(())
}

/// Удаляет только служебную базу отката (имя проверено типом).
pub async fn drop_scratch(
    runner: &dyn Runner,
    layout: &Layout,
    db: &ScratchDb,
) -> Result<(), CtlError> {
    let sql = format!("drop database if exists \"{}\";\n", db.as_str());
    run_ok(runner, "psql drop database", psql(layout, "postgres", &sql)).await?;
    Ok(())
}

/// Обмен имён одной транзакцией: живая база становится `failed`, восстановленная — `canterlot`.
/// Сбой посередине откатывает оба переименования.
pub async fn swap(
    runner: &dyn Runner,
    layout: &Layout,
    restored: &ScratchDb,
    failed: &ScratchDb,
) -> Result<(), CtlError> {
    let sql = format!(
        "begin;\nalter database \"{LIVE_DB}\" rename to \"{}\";\nalter database \"{}\" rename to \"{LIVE_DB}\";\ncommit;\n",
        failed.as_str(),
        restored.as_str()
    );
    run_ok(
        runner,
        "psql swap databases",
        psql(layout, "postgres", &sql),
    )
    .await?;
    Ok(())
}

/// oid баз с данными именами; отсутствующей базы в ответе нет.
pub async fn oids(
    runner: &dyn Runner,
    layout: &Layout,
    names: &[&str],
) -> Result<BTreeMap<String, u32>, CtlError> {
    // Имена — `canterlot` и проверенные служебные: кавычки в них невозможны.
    let list: Vec<String> = names.iter().map(|n| format!("'{n}'")).collect();
    let sql = format!(
        "select datname, oid from pg_database where datname in ({});\n",
        list.join(", ")
    );
    let out = run_ok(runner, "psql database oids", psql(layout, "postgres", &sql)).await?;
    let mut found = BTreeMap::new();
    for line in stdout(&out).lines().filter(|l| !l.trim().is_empty()) {
        let (name, oid) = line
            .split_once('|')
            .ok_or_else(|| CtlError::BadOutput("database oids".into()))?;
        let oid = oid
            .trim()
            .parse()
            .map_err(|_| CtlError::BadOutput("database oids".into()))?;
        found.insert(name.trim().to_owned(), oid);
    }
    Ok(found)
}

/// Строки `_sqlx_migrations`: «версия hex успех(0|1)», по возрастанию версии.
pub async fn applied_migrations(
    runner: &dyn Runner,
    layout: &Layout,
    database: &str,
) -> Result<Vec<String>, CtlError> {
    let sql = "select version::text || ' ' || encode(checksum, 'hex') || ' ' || success::int::text from _sqlx_migrations order by version;\n";
    let out = run_ok(runner, "psql migrations", psql(layout, database, sql)).await?;
    Ok(stdout(&out)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

pub async fn table_count(
    runner: &dyn Runner,
    layout: &Layout,
    database: &str,
) -> Result<u64, CtlError> {
    let out = run_ok(
        runner,
        "psql table count",
        psql(
            layout,
            database,
            "select count(*) from pg_tables where schemaname = 'public';\n",
        ),
    )
    .await?;
    stdout(&out)
        .parse()
        .map_err(|_| CtlError::BadOutput("table count".into()))
}

pub async fn database_size(runner: &dyn Runner, layout: &Layout) -> Result<u64, CtlError> {
    let out = run_ok(
        runner,
        "psql database size",
        psql(
            layout,
            "postgres",
            &format!("select pg_database_size('{LIVE_DB}');\n"),
        ),
    )
    .await?;
    stdout(&out)
        .parse()
        .map_err(|_| CtlError::BadOutput("database size".into()))
}

/// Свободное место файловой системы пути по `df`; `None`, если вывод не разобран.
pub async fn free_bytes(runner: &dyn Runner, path: &Path) -> Result<Option<u64>, CtlError> {
    let out = runner
        .run(cmd(
            "df",
            &["-B1", "--output=avail", &path.display().to_string()],
        ))
        .await?;
    if out.status != 0 {
        return Ok(None);
    }
    Ok(stdout(&out)
        .lines()
        .last()
        .and_then(|line| line.trim().parse().ok()))
}

#[derive(Deserialize)]
struct Manifest {
    migrations: Vec<ManifestEntry>,
}

#[derive(Deserialize)]
struct ManifestEntry {
    version: i64,
    checksum: String,
}

/// Применённые миграции базы должны быть началом списка целевого бинаря: те же версии по
/// порядку, те же контрольные суммы, все успешны. Иначе обновление не начинается.
pub fn compare_migrations(manifest_json: &str, applied: &[String]) -> Result<(), String> {
    let manifest: Manifest = serde_json::from_str(manifest_json.trim())
        .map_err(|err| format!("target migration manifest not understood: {err}"))?;
    let mut target = manifest.migrations;
    target.sort_by_key(|m| m.version);
    if target.is_empty() {
        return Err("target migration manifest is empty".into());
    }
    if applied.len() > target.len() {
        return Err(format!(
            "the database has {} migrations, the target binary only {}",
            applied.len(),
            target.len()
        ));
    }
    for (row, want) in applied.iter().zip(&target) {
        let parts: Vec<&str> = row.split(' ').collect();
        let [version, checksum, success] = parts.as_slice() else {
            return Err(format!("database migration row not understood: {row}"));
        };
        let version: i64 = version
            .parse()
            .map_err(|_| format!("database migration row not understood: {row}"))?;
        if *success != "1" {
            return Err(format!(
                "migration {version} is marked failed in the database"
            ));
        }
        if version != want.version {
            return Err(format!(
                "applied migrations are not a prefix of the target: database has {version} where the target expects {}",
                want.version
            ));
        }
        if !checksum.eq_ignore_ascii_case(&want.checksum) {
            return Err(format!(
                "migration {version} checksum differs from the target binary"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(rows: &[(i64, &str)]) -> String {
        let list: Vec<String> = rows
            .iter()
            .map(|(v, c)| format!("{{\"version\":{v},\"description\":\"m\",\"checksum\":\"{c}\"}}"))
            .collect();
        format!("{{\"migrations\":[{}]}}", list.join(","))
    }

    #[test]
    fn scratch_names_never_match_the_live_database() {
        let op = OpId("01900000-0000-7000-8000-0000000000ab".into());
        let restore = ScratchDb::restore(&op).unwrap();
        assert_eq!(
            restore.as_str(),
            "canterlot_restore_019000000000700080000000000000ab"
        );
        assert!(
            ScratchDb::failed(&op)
                .unwrap()
                .as_str()
                .starts_with("canterlot_failed_")
        );
        for bad in [
            "canterlot",
            "postgres",
            "canterlot_restore_",
            "canterlot_restore_x",
            "canterlot_failed_019000000000700080000000000000AB",
            "canterlot_restore_019000000000700080000000000000ab\"; drop",
        ] {
            assert!(ScratchDb::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn compare_requires_prefix_with_equal_checksums() {
        let m = manifest(&[(1, "aa"), (2, "bb"), (3, "cc")]);
        assert!(compare_migrations(&m, &["1 aa 1".into(), "2 bb 1".into()]).is_ok());
        assert!(compare_migrations(&m, &[]).is_ok());
        let failed = compare_migrations(&m, &["1 aa 1".into(), "2 bb 0".into()]);
        assert!(failed.unwrap_err().contains("failed"));
        let changed = compare_migrations(&m, &["1 aa 1".into(), "2 ff 1".into()]);
        assert!(changed.unwrap_err().contains("checksum"));
        let gap = compare_migrations(&m, &["1 aa 1".into(), "3 cc 1".into()]);
        assert!(gap.unwrap_err().contains("prefix"));
        let ahead =
            compare_migrations(&manifest(&[(1, "aa")]), &["1 aa 1".into(), "2 bb 1".into()]);
        assert!(ahead.is_err());
        assert!(compare_migrations("not json", &[]).is_err());
    }

    #[test]
    fn redirect_keeps_path_out_of_the_script() {
        let layout = Layout::new(PathBuf::from("/"), None);
        let c = redirect(
            Path::new("/var/lib/canterlot/backups/x.dump"),
            false,
            pg_tool(&layout, "pg_dump", &["-Fc"]),
        );
        assert_eq!(c.program, "sh");
        assert!(!c.args[1].contains("backups"));
        assert_eq!(c.args[3], "/var/lib/canterlot/backups/x.dump");
        assert_eq!(c.args[4], "docker");
    }
}
