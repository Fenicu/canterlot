//! Спека на диске переживает релиз, который её записал: сопровождение живёт дольше воркера.

use canterlot_worker::{
    config::{self, ConfigErrorCategory},
    supervisor::{PersistedSpec, SupervisorSpec},
};
use std::path::Path;
use uuid::Uuid;

const B644882: &str = include_str!("fixtures/spec/b644882ac458.toml");
const R1DBDD18: &str = include_str!("fixtures/spec/1dbdd180ffce.toml");
/// Новые первыми: спеку читает сопровождение своего релиза, и читает строго.
const FROZEN: [&str; 2] = [R1DBDD18, B644882];

#[test]
fn frozen_specs_load() {
    for frozen in FROZEN {
        let spec: PersistedSpec = toml::from_str(frozen).unwrap();
        assert_eq!(
            spec.execution_id,
            Uuid::parse_str("01a193a4-8249-7024-ba2e-000000000001").unwrap()
        );
        assert_eq!(spec.generation, 1);
        assert_eq!(
            spec.ipc_socket.as_deref(),
            Some(Path::new(
                "/run/user/1000/canterlot/01a193a4-8249-7024-ba2e-000000000001.sock"
            ))
        );
    }
}

#[test]
fn keys_unknown_to_this_release_are_ignored_at_any_depth() {
    let text = format!(
        "future_key = 1\n{B644882}\n[future_table]\nkey = true\n\n[claude_code.future]\nkey = 1\n"
    );
    toml::from_str::<PersistedSpec>(&text).unwrap();
}

#[test]
fn strict_spec_refuses_the_retired_key_that_caused_owner_lost() {
    let error = toml::from_str::<SupervisorSpec>(B644882).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unknown field `credentials_path`")
            || error
                .to_string()
                .contains("unknown field `oauth_poll_interval_s`"),
        "{error}"
    );
}

#[test]
fn worker_toml_stays_strict_about_retired_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.toml");
    for key in ["oauth_poll_interval_s = 600", "credentials_path = \"/x\""] {
        std::fs::write(
            &path,
            format!(
                "hub_url = \"https://hub.example\"\nca_cert = \"/ca\"\ntoken_file = \"/token\"\n\
                 machine_id = \"{id}\"\ninstallation_id = \"{id}\"\n\
                 restore_generation = \"{id}\"\ndata_dir = \"/data\"\n\n[claude_code]\n{key}\n",
                id = Uuid::now_v7()
            ),
        )
        .unwrap();
        let error = config::load(&path).unwrap_err();
        assert_eq!(error.category, ConfigErrorCategory::Parse, "{key}");
    }
}

// Настоящий бинарник старого релиза: CANTERLOT_OLD_RELEASE_BIN, без переменной тест пропускается.
#[tokio::test]
async fn new_control_side_speaks_to_the_old_release_binary() {
    use canterlot_proto::{
        limits::IPC_PROTOCOL_VERSION,
        worker::{
            ipc::{
                Attach, ControlToSupervisor, ShipFrom, SjAck, SupervisorToControl,
                control_to_supervisor as down, supervisor_to_control as up,
            },
            v1::record_body::Body,
        },
    };
    use canterlot_worker::supervisor::ipc::{read_message, write_message};
    use std::os::unix::fs::PermissionsExt;
    use tokio::{net::UnixStream, time::timeout};

    let Some(binary) = std::env::var_os("CANTERLOT_OLD_RELEASE_BIN") else {
        return;
    };
    const WAIT: std::time::Duration = std::time::Duration::from_secs(10);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("execution");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (execution, socket) = (Uuid::now_v7(), dir.join("ipc.sock"));
    let secret = [7u8; 32];
    std::fs::write(dir.join("secret"), secret).unwrap();
    std::fs::set_permissions(dir.join("secret"), std::fs::Permissions::from_mode(0o600)).unwrap();
    // Строгая спека чужого релиза завершает сопровождение до сокета: это не провал проверки,
    // провал только если бинарник не принял ни одну.
    let mut accepted = None;
    for frozen in FROZEN {
        let mut spec: toml::Table = toml::from_str(frozen).unwrap();
        spec.insert("execution_id".into(), execution.to_string().into());
        spec.insert(
            "ipc_socket".into(),
            socket.to_str().unwrap().to_owned().into(),
        );
        std::fs::write(dir.join("spec.toml"), toml::to_string(&spec).unwrap()).unwrap();
        let mut child = tokio::process::Command::new(&binary)
            .args(["supervise", "--exec-dir"])
            .arg(&dir)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let listening = timeout(WAIT, async {
            loop {
                if socket.exists() {
                    return true;
                }
                if child.try_wait().unwrap().is_some() {
                    return false;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        if listening {
            accepted = Some(child);
            break;
        }
    }
    let mut child = accepted.expect("the release accepts one of the frozen specs");

    for attempt in 0..2 {
        let stream = timeout(WAIT, async {
            loop {
                match UnixStream::connect(&socket).await {
                    Ok(stream) => return stream,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
                }
            }
        })
        .await
        .unwrap();
        let (mut read, mut write) = stream.into_split();
        let attach = ControlToSupervisor {
            msg: Some(down::Msg::Attach(Attach {
                protocol_version: IPC_PROTOCOL_VERSION,
                secret: secret.to_vec(),
                execution_id: execution.to_string(),
                generation: 1,
            })),
        };
        write_message(&mut write, &attach).await.unwrap();
        let reply: SupervisorToControl = timeout(WAIT, read_message(&mut read))
            .await
            .unwrap()
            .unwrap();
        let Some(up::Msg::Attached(attached)) = reply.msg else {
            panic!("no attached reply");
        };
        assert_eq!(attached.protocol_version, IPC_PROTOCOL_VERSION);
        assert_eq!(attached.pid, child.id().unwrap());
        assert_eq!(attached.acked_sj_seq, attempt);
        let ship = ControlToSupervisor {
            msg: Some(down::Msg::ShipFrom(ShipFrom {
                committed_seq: attempt,
            })),
        };
        write_message(&mut write, &ship).await.unwrap();
        if attempt == 0 {
            let batch: SupervisorToControl = timeout(WAIT, read_message(&mut read))
                .await
                .unwrap()
                .unwrap();
            let Some(up::Msg::SjBatch(batch)) = batch.msg else {
                panic!("no journal batch");
            };
            let records = batch.records;
            assert!(matches!(
                records[0].body.as_ref().and_then(|body| body.body.as_ref()),
                Some(Body::JournalOpened(_))
            ));
            let ack = ControlToSupervisor {
                msg: Some(down::Msg::SjAck(SjAck { up_to_seq: 1 })),
            };
            write_message(&mut write, &ack).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    child.start_kill().unwrap();
    child.wait().await.unwrap();
}
