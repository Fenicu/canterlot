use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use canterlot_proto::{
    limits::IPC_PROTOCOL_VERSION,
    worker::ipc::{
        Attach, Attached, ControlToSupervisor, SupervisorToControl, control_to_supervisor as down,
        supervisor_to_control as up,
    },
};
use tokio::{
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    time::{sleep, timeout},
};
use uuid::Uuid;

use super::ExecError;
use crate::{
    config::SupervisorTimers,
    supervisor::ipc::{read_message, write_message},
};

pub(super) struct Prepared {
    pub dir: PathBuf,
    pub secret: [u8; 32],
}

pub(super) struct Linked {
    pub read: OwnedReadHalf,
    pub write: OwnedWriteHalf,
    pub attached: Attached,
}

fn private_file(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(content)?;
    file.sync_all()
}

fn millis(value: Duration) -> String {
    format!("\"{}ms\"", value.as_millis())
}

pub(super) fn prepare(
    data_dir: &Path,
    execution_id: Uuid,
    generation: u64,
    conversation_id: Uuid,
    timers: &SupervisorTimers,
) -> std::io::Result<Prepared> {
    let root = data_dir.join("executions");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)?;
    let dir = root.join(execution_id.to_string());
    // Каталог без записи принадлежности остался от прерванной подготовки: процесса там нет.
    match fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let secret: [u8; 32] = rand::random();
    private_file(&dir.join("secret"), &secret)?;
    let spec = format!(
        "execution_id = \"{execution_id}\"\ngeneration = {generation}\nconversation_id = \"{conversation_id}\"\nprotocol_version = {IPC_PROTOCOL_VERSION}\n\n[timers]\nattach_wait = {}\nready_deadline = {}\ntransmit_deadline = {}\nstop_grace = {}\nstop_retry = {}\n",
        millis(timers.attach_wait),
        millis(timers.ready_deadline),
        millis(timers.transmit_deadline),
        millis(timers.stop_grace),
        millis(timers.stop_retry),
    );
    private_file(&dir.join("spec.toml"), spec.as_bytes())?;
    Ok(Prepared { dir, secret })
}

/// Сопровождение запускается отдельным процессом без привязки к жизни этого; ждём его только ради zombie.
pub(super) fn spawn_supervisor(exe: &Path, dir: &Path) -> std::io::Result<u32> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(dir.join("supervisor.log"))?;
    let mut child = tokio::process::Command::new(exe)
        .arg("supervise")
        .arg("--exec-dir")
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(false)
        .spawn()?;
    let pid = child.id().ok_or(std::io::ErrorKind::NotFound)?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(pid)
}

pub(super) async fn connect_attach(
    dir: &Path,
    secret: &[u8],
    execution_id: Uuid,
    generation: u64,
    expected_pid: Option<u32>,
    wait: Option<Duration>,
) -> Result<Linked, ExecError> {
    let socket = dir.join("ipc.sock");
    let connect = async {
        loop {
            match UnixStream::connect(&socket).await {
                Ok(stream) => return Ok(stream),
                Err(_) if wait.is_some() => sleep(Duration::from_millis(20)).await,
                Err(error) => return Err(error),
            }
        }
    };
    let stream = match wait {
        Some(limit) => timeout(limit, connect)
            .await
            .map_err(|_| ExecError::AttachTimeout)??,
        None => connect.await?,
    };
    if let Some(pid) = expected_pid {
        let peer = stream.peer_cred()?;
        if peer.pid().and_then(|p| u32::try_from(p).ok()) != Some(pid) {
            return Err(ExecError::Verify("ipc_peer"));
        }
    }
    let (mut read, mut write) = stream.into_split();
    let attach = ControlToSupervisor {
        msg: Some(down::Msg::Attach(Attach {
            protocol_version: IPC_PROTOCOL_VERSION,
            secret: secret.to_vec(),
            execution_id: execution_id.to_string(),
            generation,
        })),
    };
    write_message(&mut write, &attach).await?;
    let limit = wait.unwrap_or(Duration::from_secs(5));
    let reply: SupervisorToControl = timeout(limit, read_message(&mut read))
        .await
        .map_err(|_| ExecError::AttachTimeout)??;
    let Some(up::Msg::Attached(attached)) = reply.msg else {
        return Err(ExecError::Verify("attach_reply"));
    };
    Ok(Linked {
        read,
        write,
        attached,
    })
}
