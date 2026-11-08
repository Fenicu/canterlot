use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
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
    supervisor::ipc::{IpcError, read_message, validate_socket_path, write_message},
};

pub(super) struct Prepared {
    pub dir: PathBuf,
    pub secret: [u8; 32],
    pub socket: PathBuf,
}

pub(super) struct Linked {
    pub read: OwnedReadHalf,
    pub write: OwnedWriteHalf,
    pub attached: Attached,
}

pub(super) struct Spawned(Option<tokio::process::Child>);

impl Spawned {
    pub fn id(&self) -> Option<u32> {
        self.0.as_ref().and_then(tokio::process::Child::id)
    }

    pub async fn kill(&mut self) -> std::io::Result<()> {
        if let Some(child) = &mut self.0 {
            child.kill().await?;
        }
        Ok(())
    }

    pub fn detach(mut self) {
        if let Some(mut child) = self.0.take() {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.start_kill();
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
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
    runtime_dir: Option<&Path>,
) -> Result<Prepared, ExecError> {
    let dir = data_dir.join("executions").join(execution_id.to_string());
    let socket = match runtime_dir {
        Some(runtime) => runtime
            .join("canterlot")
            .join(format!("{execution_id}.sock")),
        None => dir.join("ipc.sock"),
    };
    validate_socket_path(&socket)?;
    if let Some(runtime) = runtime_dir {
        let path = runtime.join("canterlot");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_dir()
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.uid() != rustix::process::getuid().as_raw()
        {
            return Err(IpcError::Directory.into());
        }
    }
    let root = data_dir.join("executions");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)?;
    fs::File::open(data_dir)?.sync_all()?;
    let dir = root.join(execution_id.to_string());
    // Каталог без записи принадлежности остался от прерванной подготовки: процесса там нет.
    match fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::DirBuilder::new().mode(0o700).create(&dir)?;
    fs::File::open(&root)?.sync_all()?;
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
    let socket_spec = toml::Value::String(socket.to_string_lossy().into_owned()).to_string();
    let spec = format!("ipc_socket = {socket_spec}\n{spec}");
    private_file(&dir.join("spec.toml"), spec.as_bytes())?;
    fs::File::open(&dir)?.sync_all()?;
    Ok(Prepared {
        dir,
        secret,
        socket,
    })
}

fn build_jobs_env(configured: u32, inherited: Option<&OsStr>) -> Option<String> {
    inherited.is_none().then(|| configured.max(1).to_string())
}

/// Сопровождение запускается отдельным процессом без привязки к жизни этого; ждём его только ради zombie.
pub(super) fn spawn_supervisor(
    exe: &Path,
    dir: &Path,
    configured_build_jobs: u32,
    lease_socket: Option<&Path>,
) -> std::io::Result<Spawned> {
    let build_jobs = build_jobs_env(
        configured_build_jobs,
        std::env::var_os("CARGO_BUILD_JOBS").as_deref(),
    );
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(dir.join("supervisor.log"));
    // Повтор при ETXTBSY: обёртка супервизора пишется тестом прямо перед запуском.
    let child = crate::adapter::spawn_with_retry_sync(|| {
        let (stdout, stderr) = match &log {
            Ok(file) => (
                Stdio::from(file.try_clone()?),
                Stdio::from(file.try_clone()?),
            ),
            Err(_) => (Stdio::null(), Stdio::null()),
        };
        let mut command = tokio::process::Command::new(exe);
        command
            .arg("supervise")
            .arg("--exec-dir")
            .arg(dir)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .kill_on_drop(false);
        if let Some(value) = &build_jobs {
            command.env("CARGO_BUILD_JOBS", value);
        }
        // Сопровождение передаёт их CLI через `prepare_command` рядом с меткой группы.
        if let Some(socket) = lease_socket {
            command.env(crate::containment::LEASE_SOCKET_ENV, socket);
            command.env(crate::containment::WORKER_BIN_ENV, exe);
        }
        command.spawn()
    })?;
    Ok(Spawned(Some(child)))
}

pub(super) async fn connect_attach(
    socket: &Path,
    secret: &[u8],
    execution_id: Uuid,
    generation: u64,
    expected_pid: Option<u32>,
    wait: Option<Duration>,
) -> Result<Linked, ExecError> {
    let connect = async {
        loop {
            match UnixStream::connect(&socket).await {
                Ok(stream) => return Ok(stream),
                Err(_) if wait.is_some() => sleep(Duration::from_millis(20)).await,
                Err(error) => return Err(error),
            }
        }
    };
    let deadline = tokio::time::Instant::now() + wait.unwrap_or(Duration::from_secs(5));
    let stream = match wait {
        Some(limit) => timeout(limit, connect)
            .await
            .map_err(|_| ExecError::AttachTimeout)??,
        None => connect.await?,
    };
    let peer = stream.peer_cred()?;
    if peer.uid() != rustix::process::getuid().as_raw() {
        return Err(ExecError::Verify("ipc_uid"));
    }
    if let Some(pid) = expected_pid
        && peer.pid().and_then(|p| u32::try_from(p).ok()) != Some(pid)
    {
        return Err(ExecError::Verify("ipc_peer"));
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
    let reply: SupervisorToControl = tokio::time::timeout_at(deadline, read_message(&mut read))
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

#[cfg(test)]
mod tests {
    use super::{build_jobs_env, spawn_supervisor};

    #[test]
    fn cargo_build_jobs_defaults_but_never_overrides_the_parent() {
        assert_eq!(build_jobs_env(6, None).as_deref(), Some("6"));
        assert_eq!(build_jobs_env(6, Some("12".as_ref())), None);
    }

    #[tokio::test]
    async fn spawn_proceeds_with_null_stdio_when_supervisor_log_cannot_be_opened() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("supervisor.log")).unwrap();
        let child = spawn_supervisor(std::path::Path::new("/bin/true"), directory.path(), 1, None);
        assert!(child.is_ok(), "{:?}", child.err());
        drop(child);
    }
}
