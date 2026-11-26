//! Настоящий непривилегированный `sshd` на свободном порту loopback: временные ключи хоста и
//! клиента, вход только по ключу, разрешённая пересылка Unix-сокетов. Сеанс получает свой
//! `HOME` и `XDG_RUNTIME_DIR` во временном каталоге, поэтому воркер раннера, запущенный
//! через него, не касается ни домашнего каталога, ни рабочих сокетов хозяина теста.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use tempfile::TempDir;

pub const ALIAS: &str = "runner-a";

/// Кладёт бинарник воркера в корень раннера заранее, жёсткой ссылкой (или копией на другой ФС):
/// носитель видит совпавший sha и не гонит по SSH 80 МБ. Для тестов, где установка не проверяется.
#[allow(dead_code)]
pub fn preinstall_worker(root: &Path, worker: &Path) {
    let target = root.join("bin/canterlot-worker");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::hard_link(worker, &target)
        .or_else(|_| fs::copy(worker, &target).map(drop))
        .unwrap();
}

pub struct TestSshd {
    pub dir: TempDir,
    /// `ssh_config` с псевдонимом [`ALIAS`]; передаётся носителю как `-F`.
    pub ssh_config: PathBuf,
    port: u16,
    host_key_public: PathBuf,
    client_key: PathBuf,
    user: String,
    child: Option<Child>,
}

fn run(program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .unwrap_or_else(|error| panic!("{program}: {error}"));
    assert!(status.success(), "{program} {args:?}");
}

impl TestSshd {
    pub fn start() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("sshd-")
            .tempdir_in("/tmp")
            .unwrap();
        let base = dir.path();
        let host_key = base.join("host_key");
        let client_key = base.join("client_key");
        for key in [&host_key, &client_key] {
            run(
                "ssh-keygen",
                &["-q", "-t", "ed25519", "-N", "", "-f", key.to_str().unwrap()],
            );
        }
        fs::copy(base.join("client_key.pub"), base.join("authorized_keys")).unwrap();
        let home = base.join("home");
        let run_dir = base.join("run");
        for path in [&home, &run_dir] {
            fs::create_dir(path).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&run_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let user = String::from_utf8(
            Command::new("id")
                .arg("-un")
                .output()
                .expect("id -un")
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let sshd_config = base.join("sshd_config");
        fs::write(
            &sshd_config,
            format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {host}\n\
                 AuthorizedKeysFile {authorized}\nPasswordAuthentication no\n\
                 KbdInteractiveAuthentication no\nPubkeyAuthentication yes\nUsePAM no\n\
                 StrictModes no\nPidFile none\nAllowStreamLocalForwarding yes\n\
                 AllowTcpForwarding yes\nLogLevel INFO\n\
                 SetEnv HOME={home} XDG_RUNTIME_DIR={run_dir}\n",
                host = host_key.display(),
                authorized = base.join("authorized_keys").display(),
                home = home.display(),
                run_dir = run_dir.display(),
            ),
        )
        .unwrap();
        fs::write(
            base.join("ssh_config"),
            format!(
                "Host {ALIAS}\n  HostName 127.0.0.1\n  Port {port}\n  User {user}\n  \
                 IdentityFile {key}\n  IdentitiesOnly yes\n  StrictHostKeyChecking no\n  \
                 UserKnownHostsFile /dev/null\n  LogLevel ERROR\n",
                key = client_key.display(),
            ),
        )
        .unwrap();
        let mut log = fs::File::create(base.join("sshd.log")).unwrap();
        let mut child = Command::new("/usr/sbin/sshd")
            .args(["-D", "-e", "-f"])
            .arg(&sshd_config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start sshd");
        // Готовность — строка «Server listening» в его журнале, без опроса порта.
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        loop {
            let mut line = String::new();
            let read = stderr.read_line(&mut line).expect("sshd log");
            assert!(read > 0, "sshd exited before it started listening");
            writeln!(log, "{}", line.trim_end()).unwrap();
            if line.contains("Server listening") {
                break;
            }
        }
        // Остальной журнал уходит в файл, чтобы канал не заполнился; поток кончается вместе с sshd.
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut stderr, &mut log);
        });
        Self {
            ssh_config: base.join("ssh_config"),
            port,
            host_key_public: base.join("host_key.pub"),
            client_key,
            user,
            dir,
            child: Some(child),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Случайный высокий порт: в тесте облачного подключения он стоит вместо DNAT-порта роутера.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Открытая строка хостового ключа `ssh-ed25519 <base64>` для pinning'а.
    pub fn host_key_public(&self) -> String {
        fs::read_to_string(&self.host_key_public)
            .unwrap()
            .trim()
            .to_owned()
    }

    /// Путь к закрытому ключу клиента: в тесте заменяет `[runners].ssh_key`.
    pub fn client_key(&self) -> &Path {
        &self.client_key
    }

    /// Имя пользователя, под которым пускает тестовый `sshd`.
    pub fn user(&self) -> &str {
        &self.user
    }
}

impl Drop for TestSshd {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
