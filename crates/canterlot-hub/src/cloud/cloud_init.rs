//! Скрипт `#cloud-config` для новой облачной машины раннера.
//!
//! Он ставит хостовый ключ, который создал хаб (первое подключение сверяет отпечаток), docker,
//! git и rsync, заводит пользователя `runner` и каталог раннера и в самом конце пишет маркер
//! готовности. Токена canterlot и бинарников здесь нет: код и токен подвозит носитель после
//! подключения.

use std::fmt;

/// Предел размера пользовательских данных, в байтах UTF-8: скрипт заведомо меньше.
pub const MAX_BYTES: usize = 65_535;

/// Куда cloud-init кладёт маркер готовности. Носитель ждёт именно этот файл.
pub const READY_MARKER: &str = "/var/lib/canterlot-runner/ready";

/// Пакеты, без которых раннер не работает.
const PACKAGES: &str = "docker.io git rsync sudo";

/// Сколько дополнительных пакетов принимает карточка.
pub const MAX_PREPARE_PACKAGES: usize = 40;

/// Предел размера скрипта подготовки из карточки, в байтах UTF-8.
pub const MAX_PREPARE_SCRIPT_BYTES: usize = 16_384;

/// Имя пакета apt: первый символ — строчная латинская буква или цифра, дальше те же, `+`,
/// `.` и `-`; не длиннее 63 символов. Набор взят из аргумента карточки и проверяется ещё раз
/// здесь: строка попадает в команду установки без кавычек.
pub fn valid_package(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() || b.is_ascii_digit() => {}
        _ => return false,
    }
    name.len() <= 63
        && bytes.all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'+' | b'.' | b'-')
        })
}

/// Порт обратного SOCKS5 на облачном раннере. Должен совпадать с `EGRESS_SOCKS_PORT`
/// воркера (`crates/canterlot-worker/src/carrier/ssh.rs`).
pub const EGRESS_SOCKS_PORT: u16 = 1080;

/// Файл с переменными прокси для задач; воркер читает его как `[jobs] env_file`.
pub const EGRESS_ENV_FILE: &str = "/etc/canterlot/egress.env";

/// Drop-in демона docker: он тянет образы и слои через тот же SOCKS5.
const DOCKER_PROXY_DROPIN: &str = "/etc/systemd/system/docker.service.d/canterlot-proxy.conf";

const READY_SCRIPT: &str = "/usr/local/sbin/canterlot-runner-ready.sh";

/// Исходные данные скрипта. Ключи — строки в обычном виде, без ссылок на файлы.
#[derive(Clone, PartialEq, Eq)]
pub struct CloudInitParams {
    /// Закрытый хостовый ключ в формате OpenSSH.
    pub host_key_ed25519_private: String,
    /// Открытая часть того же ключа: `ssh-ed25519 <base64>`.
    pub host_key_ed25519_public: String,
    /// Ключ носителя для входа пользователем `runner`.
    pub runner_authorized_key: String,
    /// Корневой каталог раннера: абсолютный путь, его владелец `runner`.
    pub root_dir: String,
    /// Дополнительные пакеты из одобренной карточки: ставятся тем же циклом, что и базовые.
    pub packages: Vec<String>,
}

impl fmt::Debug for CloudInitParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudInitParams")
            .field("host_key_ed25519_private", &"<redacted>")
            .field("host_key_ed25519_public", &self.host_key_ed25519_public)
            .field("runner_authorized_key", &self.runner_authorized_key)
            .field("root_dir", &self.root_dir)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudInitError {
    /// Хостовый закрытый ключ не похож на файл OpenSSH.
    InvalidPrivateKey,
    /// Открытая строка ключа пуста, многострочна или содержит лишнее.
    InvalidPublicKey(&'static str),
    /// Путь не абсолютный или содержит символы вне безопасного набора.
    InvalidRootDir,
    /// Дополнительный пакет карточки — не имя пакета apt или их слишком много.
    InvalidPackages(&'static str),
    /// Результат длиннее предела провайдера.
    TooLarge { bytes: usize },
}

impl fmt::Display for CloudInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPrivateKey => f.write_str("host private key is not an OpenSSH key file"),
            Self::InvalidPublicKey(which) => write!(f, "{which} is not a single-line public key"),
            Self::InvalidRootDir => f.write_str("root_dir must be a safe absolute path"),
            Self::InvalidPackages(why) => write!(f, "prepare packages are not safe: {why}"),
            Self::TooLarge { bytes } => {
                write!(f, "cloud-init is {bytes} bytes, the limit is {MAX_BYTES}")
            }
        }
    }
}

impl std::error::Error for CloudInitError {}

fn is_base64(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// Закрытый ключ: ровно файл OpenSSH, строки только base64. Так в YAML-блок не попадёт
/// посторонний ключ или команда.
fn check_private_key(text: &str) -> Result<Vec<&str>, CloudInitError> {
    let lines: Vec<&str> = text.lines().collect();
    let [first, body @ .., last] = lines.as_slice() else {
        return Err(CloudInitError::InvalidPrivateKey);
    };
    let framed = *first == "-----BEGIN OPENSSH PRIVATE KEY-----"
        && *last == "-----END OPENSSH PRIVATE KEY-----";
    if !framed || body.is_empty() || !body.iter().all(|line| is_base64(line) && line.len() <= 200) {
        return Err(CloudInitError::InvalidPrivateKey);
    }
    Ok(lines)
}

/// Открытый ключ одной строкой: тип, base64, необязательный комментарий. Набор символов
/// исключает кавычки, двоеточия, пробелы внутри слов и решётку, поэтому значение безопасно
/// и в одинарных кавычках YAML, и в `authorized_keys`.
fn check_public_key<'a>(line: &'a str, which: &'static str) -> Result<&'a str, CloudInitError> {
    let bad = CloudInitError::InvalidPublicKey(which);
    let mut words = line.split(' ');
    let kind = words.next().unwrap_or_default();
    let kind_ok =
        (kind.starts_with("ssh-") || kind.starts_with("ecdsa-") || kind.starts_with("sk-"))
            && kind
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'@' | b'.'));
    if !kind_ok || !words.next().is_some_and(is_base64) {
        return Err(bad);
    }
    let comment_ok = |word: &str| {
        !word.is_empty()
            && word.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@' | b'+' | b'=')
            })
    };
    match (words.next(), words.next()) {
        (None, _) => Ok(line),
        (Some(comment), None) if comment_ok(comment) => Ok(line),
        _ => Err(bad),
    }
}

/// Открытый ключ входа для конфигурации хаба: те же требования, что у cloud-init.
pub fn validate_public_key(line: &str) -> bool {
    check_public_key(line, "runner_public_key").is_ok()
}

fn check_root_dir(path: &str) -> Result<(), CloudInitError> {
    let safe = path.len() > 1
        && path.len() <= 200
        && path.starts_with('/')
        && !path.contains("//")
        && !path.split('/').any(|part| part == "..")
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'));
    if safe {
        Ok(())
    } else {
        Err(CloudInitError::InvalidRootDir)
    }
}

pub fn render(params: &CloudInitParams) -> Result<String, CloudInitError> {
    let private = check_private_key(&params.host_key_ed25519_private)?;
    let host_public = check_public_key(&params.host_key_ed25519_public, "host_key_ed25519_public")?;
    let authorized = check_public_key(&params.runner_authorized_key, "runner_authorized_key")?;
    check_root_dir(&params.root_dir)?;
    // Пакеты карточки проверяются ещё раз: строка идёт в команду установки без кавычек, а
    // между карточкой и рендером могла пройти замена окна или правка строки в базе.
    if params.packages.len() > MAX_PREPARE_PACKAGES {
        return Err(CloudInitError::InvalidPackages("too many packages"));
    }
    let mut install = String::from(PACKAGES);
    for package in &params.packages {
        if !valid_package(package) {
            return Err(CloudInitError::InvalidPackages("bad package name"));
        }
        install.push(' ');
        install.push_str(package);
    }
    let root = &params.root_dir;

    let mut out = String::new();
    out.push_str("#cloud-config\n");
    out.push_str("# Generated by the hub for one runner window.\n");
    out.push_str("ssh_deletekeys: true\n");
    out.push_str("ssh_genkeytypes: [ed25519]\n");
    out.push_str("ssh_keys:\n  ed25519_private: |\n");
    for line in private {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!("  ed25519_public: '{host_public}'\n"));
    out.push_str("groups:\n  - docker\n");
    out.push_str("users:\n");
    out.push_str("  - name: runner\n");
    out.push_str("    gecos: canterlot runner\n");
    out.push_str("    shell: /bin/bash\n");
    out.push_str("    groups: [docker]\n");
    out.push_str("    sudo: ALL=(ALL) NOPASSWD:ALL\n");
    out.push_str("    lock_passwd: true\n");
    out.push_str(&format!(
        "    ssh_authorized_keys:\n      - '{authorized}'\n"
    ));
    out.push_str("write_files:\n");
    out.push_str("  - path: /etc/ssh/sshd_config.d/10-canterlot-hostkey.conf\n");
    out.push_str("    permissions: '0644'\n");
    out.push_str("    content: |\n");
    out.push_str("      HostKey /etc/ssh/ssh_host_ed25519_key\n");
    // Из облака заблокированы static.crates.io, github.com, CDN Docker Hub и зеркала apt.
    // Обратный SOCKS5 поднимает носитель в своём ssh-пробросе, поэтому обе записи должны
    // лежать на диске до установки docker.io и первого выхода задач в сеть.
    out.push_str(&format!("  - path: {DOCKER_PROXY_DROPIN}\n"));
    out.push_str("    permissions: '0644'\n");
    out.push_str("    content: |\n");
    out.push_str("      [Service]\n");
    out.push_str(&format!(
        "      Environment=\"HTTP_PROXY=socks5://127.0.0.1:{EGRESS_SOCKS_PORT}\" \"HTTPS_PROXY=socks5://127.0.0.1:{EGRESS_SOCKS_PORT}\" \"NO_PROXY=localhost,127.0.0.1,::1\"\n"
    ));
    out.push_str(&format!("  - path: {EGRESS_ENV_FILE}\n"));
    out.push_str("    permissions: '0644'\n");
    out.push_str("    content: |\n");
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        out.push_str(&format!(
            "      {name}=socks5h://127.0.0.1:{EGRESS_SOCKS_PORT}\n"
        ));
    }
    for name in ["NO_PROXY", "no_proxy"] {
        out.push_str(&format!("      {name}=localhost,127.0.0.1,::1\n"));
    }
    out.push_str(&format!("  - path: {READY_SCRIPT}\n"));
    out.push_str("    permissions: '0755'\n");
    out.push_str("    content: |\n");
    // Маркер готовности — последняя команда скрипта, и только после проверки всего нужного.
    for line in [
        "#!/bin/sh".to_owned(),
        "set -eu".to_owned(),
        "docker info >/dev/null".to_owned(),
        "git --version >/dev/null".to_owned(),
        "rsync --version >/dev/null".to_owned(),
        "id runner >/dev/null".to_owned(),
        format!("[ \"$(stat -c %U {root})\" = runner ]"),
        "mkdir -p /var/lib/canterlot-runner".to_owned(),
        format!("date -u +%Y-%m-%dT%H:%M:%SZ > {READY_MARKER}"),
    ] {
        out.push_str("      ");
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str("runcmd:\n");
    out.push_str("  - [sh, -c, 'rm -f /etc/ssh/ssh_host_rsa_key* /etc/ssh/ssh_host_ecdsa_key* /etc/ssh/ssh_host_dsa_key*']\n");
    out.push_str("  - [systemctl, restart, ssh]\n");
    // Пакеты ставятся не модулем `packages` (он падает при сбое зеркала и не повторяется), а
    // циклами, которые ждут сеть и зеркало. Маркер готовности пишется после них.
    out.push_str("  - [sh, -c, 'until apt-get update; do sleep 10; done']\n");
    out.push_str(&format!(
        "  - [sh, -c, 'until DEBIAN_FRONTEND=noninteractive apt-get install -y {install}; do sleep 10; apt-get update || true; done']\n"
    ));
    out.push_str("  - [systemctl, enable, --now, docker]\n");
    out.push_str(&format!("  - [mkdir, -p, {root}]\n"));
    out.push_str(&format!("  - [chown, 'runner:runner', {root}]\n"));
    out.push_str(&format!("  - [sh, {READY_SCRIPT}]\n"));

    if out.len() > MAX_BYTES {
        return Err(CloudInitError::TooLarge { bytes: out.len() });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::host_key::HostKey;

    fn params() -> CloudInitParams {
        let key = HostKey::generate().unwrap();
        CloudInitParams {
            host_key_ed25519_private: key.private_openssh().to_owned(),
            host_key_ed25519_public: key.public_line().to_owned(),
            runner_authorized_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAMPLE0000 carrier"
                .to_owned(),
            root_dir: "/srv/runner".to_owned(),
            packages: Vec::new(),
        }
    }

    fn render_ok(p: &CloudInitParams) -> String {
        render(p).unwrap()
    }

    #[test]
    fn starts_with_the_cloud_config_header() {
        assert!(render_ok(&params()).starts_with("#cloud-config\n"));
    }

    #[test]
    fn installs_the_given_host_key_and_drops_the_others() {
        let p = params();
        let text = render_ok(&p);
        assert!(text.contains("ssh_deletekeys: true"));
        assert!(text.contains("ssh_genkeytypes: [ed25519]"));
        assert!(text.contains("ssh_keys:\n  ed25519_private: |\n"));
        // Каждая строка закрытого ключа лежит в блоке с отступом.
        for line in p.host_key_ed25519_private.lines() {
            assert!(text.contains(&format!("\n    {line}\n")), "missing {line}");
        }
        assert!(text.contains(&format!(
            "  ed25519_public: '{}'\n",
            p.host_key_ed25519_public
        )));
        for other in ["rsa", "ecdsa", "dsa"] {
            assert!(text.contains(&format!("/etc/ssh/ssh_host_{other}_key*")));
        }
    }

    #[test]
    fn installs_docker_git_rsync_in_retry_loops_that_wait_for_the_network() {
        let text = render_ok(&params());
        for package in ["docker.io", "git", "rsync"] {
            assert!(text.contains(package), "no {package}");
        }
        // Без сети модуль `packages` падает и не повторяется: его нет, ставят циклы.
        assert!(!text.contains("\npackages:") && !text.contains("package_update"));
        let runcmd = text.split("\nruncmd:\n").nth(1).unwrap();
        let update = runcmd
            .find("until apt-get update; do sleep 10; done")
            .expect("apt-get update retried until it succeeds");
        let install = runcmd
            .find(
                "until DEBIAN_FRONTEND=noninteractive apt-get install -y docker.io git rsync sudo",
            )
            .expect("the install retried until it succeeds");
        let docker = runcmd.find("enable, --now, docker").unwrap();
        let ready = runcmd.find(READY_SCRIPT).unwrap();
        assert!(update < install && install < docker && docker < ready);
    }

    #[test]
    fn installs_the_card_packages_in_the_same_retry_loop_as_the_base_ones() {
        let mut p = params();
        p.packages = vec!["build-essential".to_owned(), "libssl-dev".to_owned()];
        let text = render_ok(&p);
        let install = "until DEBIAN_FRONTEND=noninteractive apt-get install -y \
                       docker.io git rsync sudo build-essential libssl-dev; do sleep 10; \
                       apt-get update || true; done";
        assert!(
            text.contains(install),
            "extra packages are not in the install loop"
        );
    }

    #[test]
    fn rejects_packages_that_are_not_apt_names() {
        for bad in [
            "",
            "openssl libssl-dev",
            "$(id)",
            "-x",
            "OpenSSL",
            "a/b",
            &"a".repeat(64),
        ] {
            let mut p = params();
            p.packages = vec![bad.to_owned()];
            assert_eq!(
                render(&p),
                Err(CloudInitError::InvalidPackages("bad package name")),
                "{bad:?}"
            );
        }
        let mut p = params();
        p.packages = (0..=MAX_PREPARE_PACKAGES)
            .map(|i| format!("p{i}"))
            .collect();
        assert_eq!(
            render(&p),
            Err(CloudInitError::InvalidPackages("too many packages"))
        );
    }

    #[test]
    fn the_package_name_rule_is_exact() {
        for good in ["a", "docker.io", "gcc-12", "libc++", "python3.12", "7zip"] {
            assert!(valid_package(good), "{good}");
        }
        for bad in ["", "A", "a b", "a;b", "a_b", "a\nb", "-a", ".a"] {
            assert!(!valid_package(bad), "{bad:?}");
        }
    }

    #[test]
    fn proxies_docker_and_the_jobs_before_docker_starts() {
        let text = render_ok(&params());
        let dropin = "  - path: /etc/systemd/system/docker.service.d/canterlot-proxy.conf\n    permissions: '0644'\n    content: |\n      [Service]\n      Environment=\"HTTP_PROXY=socks5://127.0.0.1:1080\" \"HTTPS_PROXY=socks5://127.0.0.1:1080\" \"NO_PROXY=localhost,127.0.0.1,::1\"\n";
        assert!(text.contains(dropin), "docker drop-in missing or wrong");
        let mut env = String::from(
            "  - path: /etc/canterlot/egress.env\n    permissions: '0644'\n    content: |\n",
        );
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            env.push_str(&format!("      {name}=socks5h://127.0.0.1:1080\n"));
        }
        for name in ["NO_PROXY", "no_proxy"] {
            env.push_str(&format!("      {name}=localhost,127.0.0.1,::1\n"));
        }
        assert!(text.contains(&env), "egress.env missing or wrong");
        // Оба файла лежат на диске до установки docker.io: демон стартует уже с прокси.
        let install = text.find("apt-get install -y docker.io").unwrap();
        let dropin_at = text.find(DOCKER_PROXY_DROPIN).unwrap();
        let env_at = text.find(EGRESS_ENV_FILE).unwrap();
        assert!(dropin_at < install && env_at < install);
    }

    #[test]
    fn creates_the_runner_user_in_docker_with_sudo_and_the_key() {
        let p = params();
        let text = render_ok(&p);
        assert!(text.contains("\ngroups:\n  - docker\n"));
        assert!(text.contains("  - name: runner\n"));
        assert!(text.contains("    groups: [docker]\n"));
        assert!(text.contains("    sudo: ALL=(ALL) NOPASSWD:ALL\n"));
        assert!(text.contains("    lock_passwd: true\n"));
        assert!(text.contains(&format!(
            "    ssh_authorized_keys:\n      - '{}'\n",
            p.runner_authorized_key
        )));
    }

    #[test]
    fn creates_the_root_dir_owned_by_runner() {
        let text = render_ok(&params());
        assert!(text.contains("  - [mkdir, -p, /srv/runner]\n"));
        assert!(text.contains("  - [chown, 'runner:runner', /srv/runner]\n"));
    }

    #[test]
    fn ready_marker_is_written_last() {
        let text = render_ok(&params());
        let runcmd = text.split("\nruncmd:\n").nth(1).expect("runcmd section");
        let last = runcmd.trim_end().lines().last().unwrap();
        assert_eq!(last, format!("  - [sh, {READY_SCRIPT}]"));
        // runcmd — последняя секция документа, после неё ничего нет.
        assert!(!runcmd.contains("\n\n") && !runcmd.lines().any(|l| !l.starts_with("  - ")));
        // Скрипт готовности пишет маркер последней командой и проверяет остальное до неё.
        let script_start = text.find("#!/bin/sh").unwrap();
        let script = &text[script_start..text.find("\nruncmd:").unwrap()];
        let marker_line = script
            .lines()
            .position(|l| l.contains(READY_MARKER))
            .expect("marker line");
        let lines: Vec<_> = script.lines().collect();
        assert!(lines[marker_line + 1..].iter().all(|l| l.trim().is_empty()));
        for check in [
            "docker info",
            "git --version",
            "rsync --version",
            "id runner",
        ] {
            assert!(
                lines[..marker_line].iter().any(|l| l.contains(check)),
                "{check}"
            );
        }
        assert!(script.contains("set -eu"));
    }

    #[test]
    fn carries_no_token_and_no_binary() {
        let text = render_ok(&params()).to_lowercase();
        for forbidden in [
            "token",
            "bearer",
            "curl",
            "wget",
            "http://",
            "https://",
            "\u{7f}elf",
            "\0",
        ] {
            assert!(!text.contains(forbidden), "found {forbidden}");
        }
        assert!(text.is_ascii());
    }

    #[test]
    fn rejects_unsafe_root_dirs() {
        for bad in [
            "",
            "/",
            "relative/dir",
            "/srv/../etc",
            "/srv/run ner",
            "/srv/runner\nrm -rf /",
            "/srv/runner;id",
            "/srv/'x'",
            "/srv/$HOME",
        ] {
            let mut p = params();
            p.root_dir = bad.to_owned();
            assert_eq!(render(&p), Err(CloudInitError::InvalidRootDir), "{bad:?}");
        }
    }

    #[test]
    fn rejects_malformed_keys() {
        let mut p = params();
        p.host_key_ed25519_private = "not a key".to_owned();
        assert_eq!(render(&p), Err(CloudInitError::InvalidPrivateKey));

        let mut p = params();
        p.host_key_ed25519_private.push_str("injected: yes\n");
        assert_eq!(render(&p), Err(CloudInitError::InvalidPrivateKey));

        let mut p = params();
        p.host_key_ed25519_public = "ssh-ed25519 AAAA\nruncmd: [x]".to_owned();
        assert!(matches!(
            render(&p),
            Err(CloudInitError::InvalidPublicKey(_))
        ));

        let mut p = params();
        p.runner_authorized_key = "ssh-ed25519 AAAA' ; rm -rf /; '".to_owned();
        assert!(matches!(
            render(&p),
            Err(CloudInitError::InvalidPublicKey(_))
        ));

        let mut p = params();
        p.runner_authorized_key = String::new();
        assert!(matches!(
            render(&p),
            Err(CloudInitError::InvalidPublicKey(_))
        ));
    }

    #[test]
    fn leaves_the_route_and_the_resolver_to_the_provider() {
        // У машины свой публичный адрес и сеть провайдера: маршрут и резолвер не трогаются.
        let text = render_ok(&params());
        for gone in ["ip route", "resolv.conf", "nameserver"] {
            assert!(!text.contains(gone), "{gone} is still in the script");
        }
        let runcmd = text.split("\nruncmd:\n").nth(1).unwrap();
        assert!(runcmd.contains("until apt-get update"));
    }

    #[test]
    fn size_limit_is_exactly_65535_bytes() {
        let mut p = params();
        let base = render_ok(&p).len();
        assert!(base < MAX_BYTES);
        let pad = "A".repeat(MAX_BYTES - base);
        p.runner_authorized_key.push_str(&pad);
        // Лишний хвост ключа вместо комментария: в вывод он попадает один раз.
        let at_limit = render(&p).unwrap();
        assert_eq!(at_limit.len(), MAX_BYTES);
        p.runner_authorized_key.push('A');
        assert_eq!(
            render(&p),
            Err(CloudInitError::TooLarge {
                bytes: MAX_BYTES + 1
            })
        );
    }

    #[test]
    fn debug_hides_the_private_key() {
        let p = params();
        let shown = format!("{p:?}");
        assert!(!shown.contains("OPENSSH"));
        assert!(shown.contains("redacted"));
    }
}
