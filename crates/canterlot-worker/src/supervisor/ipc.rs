use std::{
    fs, io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

use canterlot_proto::limits::IPC_MAX_MESSAGE_BYTES;
use prost::Message;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{
        UnixListener, UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
};

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("ipc I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("ipc message exceeds the limit")]
    TooLarge,
    #[error("invalid ipc message")]
    Decode,
    #[error("ipc directory must be private and owned by the current user")]
    Directory,
}

pub async fn read_message<M: Message + Default, R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<M, IpcError> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > IPC_MAX_MESSAGE_BYTES {
        return Err(IpcError::TooLarge);
    }
    let mut buffer = vec![0_u8; length];
    reader.read_exact(&mut buffer).await?;
    M::decode(buffer.as_slice()).map_err(|_| IpcError::Decode)
}

pub async fn write_message<M: Message, W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &M,
) -> Result<(), IpcError> {
    let length = message.encoded_len();
    if length > IPC_MAX_MESSAGE_BYTES {
        return Err(IpcError::TooLarge);
    }
    let mut buffer = Vec::with_capacity(4 + length);
    buffer.extend_from_slice(
        &u32::try_from(length)
            .map_err(|_| IpcError::TooLarge)?
            .to_be_bytes(),
    );
    message
        .encode(&mut buffer)
        .map_err(|_| IpcError::TooLarge)?;
    writer.write_all(&buffer).await?;
    writer.flush().await?;
    Ok(())
}

pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && std::hint::black_box(
            left.iter()
                .zip(right)
                .fold(0_u8, |difference, (a, b)| difference | (a ^ b)),
        ) == 0
}

pub struct IpcServer {
    listener: UnixListener,
    secret: Vec<u8>,
    uid: u32,
}

pub struct IpcConnection {
    stream: UnixStream,
}

impl IpcConnection {
    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        self.stream.into_split()
    }
}

impl IpcServer {
    pub async fn bind(exec_dir: &Path, secret: Vec<u8>) -> Result<IpcServer, IpcError> {
        Self::bind_for_uid(exec_dir, secret, rustix::process::getuid().as_raw()).await
    }

    /// Отдельный uid нужен только для проверки отказа чужому пользователю.
    pub async fn bind_for_uid(
        exec_dir: &Path,
        secret: Vec<u8>,
        uid: u32,
    ) -> Result<IpcServer, IpcError> {
        let metadata = fs::metadata(exec_dir)?;
        if metadata.permissions().mode() & 0o077 != 0
            || metadata.uid() != rustix::process::getuid().as_raw()
        {
            return Err(IpcError::Directory);
        }
        let socket = exec_dir.join("ipc.sock");
        match fs::remove_file(&socket) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        Ok(IpcServer {
            listener,
            secret,
            uid,
        })
    }

    pub fn secret_matches(&self, candidate: &[u8]) -> bool {
        constant_time_eq(&self.secret, candidate)
    }

    /// Соединения других пользователей закрываются молча; ожидание продолжается.
    pub async fn accept(&self) -> Result<IpcConnection, IpcError> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            match stream.peer_cred() {
                Ok(credentials) if credentials.uid() == self.uid => {
                    return Ok(IpcConnection { stream });
                }
                _ => drop(stream),
            }
        }
    }
}
