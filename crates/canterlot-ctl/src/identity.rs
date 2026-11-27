//! Тождество базы: перед запуском хаба на перенесённой, восстановленной или повторно
//! используемой базе утилита сверяет её с тем, что записала раньше.

use serde::Deserialize;

use crate::{
    error::CtlError,
    layout::Layout,
    ops::DbIdentity,
    runner::Runner,
    stack::{psql, run_ok, stdout},
};

const IDENTITY_SQL: &str = "select (select system_identifier from pg_control_system()), \
     (select oid from pg_database where datname = current_database()), \
     installation_id, restore_generation, owner_id from installation;\n";

/// Сравнивает поля; в ошибке только имена различающихся полей, без значений.
pub fn check_identity(expected: &DbIdentity, got: &DbIdentity) -> Result<(), CtlError> {
    let mut differ = Vec::new();
    if expected.system_identifier != got.system_identifier {
        differ.push("system_identifier");
    }
    if expected.database_oid != got.database_oid {
        differ.push("database_oid");
    }
    if expected.installation_id != got.installation_id {
        differ.push("installation_id");
    }
    if expected.restore_generation != got.restore_generation {
        differ.push("restore_generation");
    }
    if expected.owner_id != got.owner_id {
        differ.push("owner_id");
    }
    if differ.is_empty() {
        return Ok(());
    }
    Err(CtlError::IdentityMismatch(format!(
        "{} differ; see docs/deploy/pilot.md §8.2 for recovery",
        differ.join(", ")
    )))
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    restore_in_progress: bool,
}

pub fn parse_identity(line: &str) -> Result<DbIdentity, CtlError> {
    let bad = || CtlError::BadOutput("database identity".into());
    let parts: Vec<&str> = line.trim().split('|').collect();
    let [system, oid, installation, generation, owner] = parts.as_slice() else {
        return Err(bad());
    };
    Ok(DbIdentity {
        system_identifier: (*system).to_owned(),
        database_oid: oid.parse().map_err(|_| bad())?,
        installation_id: installation.parse().map_err(|_| bad())?,
        restore_generation: generation.parse().map_err(|_| bad())?,
        owner_id: owner.parse().map_err(|_| bad())?,
    })
}

/// Читает тождество базы из работающего postgres и флаг восстановления из манифеста.
pub async fn db_identity(runner: &dyn Runner, layout: &Layout) -> Result<DbIdentity, CtlError> {
    let manifest = layout.manifest_dir().join("installation.toml");
    match std::fs::read_to_string(&manifest) {
        Ok(text) => {
            let parsed: Manifest = toml::from_str(&text)?;
            if parsed.restore_in_progress {
                return Err(CtlError::RestoreInProgress);
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(CtlError::Other(format!(
                "manifest_missing: {}",
                manifest.display()
            )));
        }
        Err(err) => return Err(err.into()),
    }
    db_identity_of(runner, layout, "canterlot").await
}

/// Тождество произвольной базы кластера без проверки манифеста: так откат читает
/// восстановленную копию до того, как она станет `canterlot`.
pub async fn db_identity_of(
    runner: &dyn Runner,
    layout: &Layout,
    database: &str,
) -> Result<DbIdentity, CtlError> {
    let out = run_ok(
        runner,
        "psql identity",
        psql(layout, database, IDENTITY_SQL),
    )
    .await?;
    parse_identity(&stdout(&out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn identity() -> DbIdentity {
        DbIdentity {
            system_identifier: "7300000000000000001".into(),
            database_oid: 16384,
            installation_id: Uuid::nil(),
            restore_generation: Uuid::nil(),
            owner_id: Uuid::nil(),
        }
    }

    #[test]
    fn check_identity_names_fields_without_values() {
        check_identity(&identity(), &identity()).unwrap();
        let mut other = identity();
        other.database_oid = 1;
        other.owner_id = Uuid::max();
        match check_identity(&identity(), &other) {
            Err(CtlError::IdentityMismatch(text)) => {
                assert!(text.starts_with("database_oid, owner_id differ"), "{text}");
                assert!(!text.contains("16384"));
                assert!(!text.contains(&Uuid::max().to_string()));
            }
            other => panic!("expected identity_mismatch, got {other:?}"),
        }
    }

    #[test]
    fn parse_identity_rejects_garbage() {
        let line = format!("1|2|{0}|{0}|{0}", Uuid::nil());
        assert_eq!(parse_identity(&line).unwrap().database_oid, 2);
        assert!(parse_identity("1|2|x").is_err());
        assert!(parse_identity("").is_err());
    }

    #[tokio::test]
    async fn db_identity_refuses_restore_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().to_path_buf(), None);
        std::fs::create_dir_all(layout.manifest_dir()).unwrap();
        std::fs::write(
            layout.manifest_dir().join("installation.toml"),
            "installation_id = \"x\"\nrestore_in_progress = true\n",
        )
        .unwrap();
        let runner = crate::runner::FakeRunner::new();
        assert!(matches!(
            db_identity(&runner, &layout).await,
            Err(CtlError::RestoreInProgress)
        ));
        assert!(runner.calls().is_empty());
    }
}
