//! Хостовый ключ ed25519 для нового сервера: хаб создаёт его в памяти и кладёт в cloud-init,
//! чтобы первое же подключение сверяло отпечаток. На диск ключ не пишется.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use sha2::{Digest as _, Sha256};

const KEY_TYPE: &str = "ssh-ed25519";
const MAGIC: &[u8] = b"openssh-key-v1\0";
const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
const END: &str = "-----END OPENSSH PRIVATE KEY-----";
/// Длина строки base64 в файле ключа OpenSSH.
const LINE: usize = 70;
/// Размер блока шифра `none`: добивка приватной части идёт до кратного 8.
const BLOCK: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyError {
    /// У системы нет источника случайных чисел.
    Random,
    /// Библиотека подписи отвергла семя.
    Seed,
}

impl fmt::Display for HostKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Random => f.write_str("no system randomness for the host key"),
            Self::Seed => f.write_str("the host key seed was rejected"),
        }
    }
}

impl std::error::Error for HostKeyError {}

/// Пара хостового ключа. Закрытая часть не попадает ни в `Debug`, ни в логи.
#[derive(Clone, PartialEq, Eq)]
pub struct HostKey {
    private_openssh: String,
    public_line: String,
    public_raw: [u8; 32],
}

impl fmt::Debug for HostKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostKey")
            .field("public", &self.public_line)
            .field("private", &"<redacted>")
            .finish()
    }
}

impl HostKey {
    /// Новая пара из системного источника случайных чисел.
    pub fn generate() -> Result<Self, HostKeyError> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|_| HostKeyError::Random)?;
        let mut check = [0u8; 4];
        getrandom::fill(&mut check).map_err(|_| HostKeyError::Random)?;
        Self::from_seed(&seed, u32::from_be_bytes(check))
    }

    fn from_seed(seed: &[u8; 32], check: u32) -> Result<Self, HostKeyError> {
        let pair = Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| HostKeyError::Seed)?;
        let public_raw: [u8; 32] = pair
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| HostKeyError::Seed)?;

        let mut public_blob = Vec::new();
        put_string(&mut public_blob, KEY_TYPE.as_bytes());
        put_string(&mut public_blob, &public_raw);

        // Закрытая часть: проверочное число дважды, тип, открытый ключ, семя + открытый
        // ключ (так хранит ed25519 OpenSSH), пустой комментарий, добивка 1, 2, 3, …
        let mut private = Vec::new();
        private.extend_from_slice(&check.to_be_bytes());
        private.extend_from_slice(&check.to_be_bytes());
        put_string(&mut private, KEY_TYPE.as_bytes());
        put_string(&mut private, &public_raw);
        let mut secret = Vec::with_capacity(64);
        secret.extend_from_slice(seed);
        secret.extend_from_slice(&public_raw);
        put_string(&mut private, &secret);
        put_string(&mut private, b"");
        let mut pad = 1u8;
        while private.len() % BLOCK != 0 {
            private.push(pad);
            pad += 1;
        }

        let mut file = Vec::from(MAGIC);
        put_string(&mut file, b"none");
        put_string(&mut file, b"none");
        put_string(&mut file, b"");
        file.extend_from_slice(&1u32.to_be_bytes());
        put_string(&mut file, &public_blob);
        put_string(&mut file, &private);

        let encoded = STANDARD.encode(&file);
        let mut text = String::with_capacity(encoded.len() + encoded.len() / LINE + 100);
        text.push_str(BEGIN);
        text.push('\n');
        for chunk in encoded.as_bytes().chunks(LINE) {
            // Base64 — чистый ASCII, разбивка по байтам не рвёт символ.
            text.push_str(std::str::from_utf8(chunk).map_err(|_| HostKeyError::Seed)?);
            text.push('\n');
        }
        text.push_str(END);
        text.push('\n');

        Ok(Self {
            private_openssh: text,
            public_line: format!("{KEY_TYPE} {}", STANDARD.encode(&public_blob)),
            public_raw,
        })
    }

    /// Закрытый ключ в формате OpenSSH (`-----BEGIN OPENSSH PRIVATE KEY-----`), без шифрования.
    pub fn private_openssh(&self) -> &str {
        &self.private_openssh
    }

    /// Открытый ключ одной строкой: `ssh-ed25519 <base64>`.
    pub fn public_line(&self) -> &str {
        &self.public_line
    }

    /// Отпечаток `SHA256:<base64 без добивки>`, как печатает `ssh-keygen -l`.
    pub fn fingerprint_sha256(&self) -> String {
        let mut blob = Vec::new();
        put_string(&mut blob, KEY_TYPE.as_bytes());
        put_string(&mut blob, &self.public_raw);
        let digest = Sha256::digest(&blob);
        let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest);
        format!("SHA256:{encoded}")
    }
}

/// Строка формата SSH: длина `u32` big-endian и байты.
fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    // Все строки здесь короче 4 ГБ.
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn decode_private(text: &str) -> Vec<u8> {
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.first().copied(), Some(BEGIN));
        assert_eq!(lines.last().copied(), Some(END));
        let body: String = lines[1..lines.len() - 1].concat();
        for line in &lines[1..lines.len() - 1] {
            assert!(line.len() <= LINE, "base64 line too long: {}", line.len());
        }
        STANDARD.decode(body).unwrap()
    }

    fn take_u32(bytes: &mut &[u8]) -> u32 {
        let (head, rest) = bytes.split_at(4);
        *bytes = rest;
        u32::from_be_bytes(head.try_into().unwrap())
    }

    fn take_string<'a>(bytes: &mut &'a [u8]) -> &'a [u8] {
        let len = take_u32(bytes) as usize;
        let (head, rest) = bytes.split_at(len);
        *bytes = rest;
        head
    }

    #[test]
    fn private_key_is_an_unencrypted_openssh_ed25519_file() {
        let key = HostKey::generate().unwrap();
        let raw = decode_private(key.private_openssh());
        assert!(raw.starts_with(MAGIC));
        let mut rest = &raw[MAGIC.len()..];
        assert_eq!(take_string(&mut rest), b"none");
        assert_eq!(take_string(&mut rest), b"none");
        assert_eq!(take_string(&mut rest), b"");
        assert_eq!(take_u32(&mut rest), 1);
        let mut public = take_string(&mut rest);
        assert_eq!(take_string(&mut public), KEY_TYPE.as_bytes());
        let public_raw = take_string(&mut public);
        assert_eq!(public_raw.len(), 32);
        assert!(public.is_empty());
        let mut private = take_string(&mut rest);
        assert!(rest.is_empty());
        assert_eq!(private.len() % BLOCK, 0);
        let check1 = take_u32(&mut private);
        let check2 = take_u32(&mut private);
        assert_eq!(check1, check2);
        assert_eq!(take_string(&mut private), KEY_TYPE.as_bytes());
        assert_eq!(take_string(&mut private), public_raw);
        let secret = take_string(&mut private);
        assert_eq!(secret.len(), 64);
        assert_eq!(&secret[32..], public_raw);
        let _comment = take_string(&mut private);
        // Добивка 1, 2, 3, …
        for (i, byte) in private.iter().enumerate() {
            assert_eq!(usize::from(*byte), i + 1);
        }
    }

    #[test]
    fn public_line_carries_the_same_key() {
        let key = HostKey::generate().unwrap();
        let line = key.public_line();
        let (kind, body) = line.split_once(' ').unwrap();
        assert_eq!(kind, KEY_TYPE);
        assert!(!body.contains(' '));
        let blob = STANDARD.decode(body).unwrap();
        let mut view = &blob[..];
        assert_eq!(take_string(&mut view), KEY_TYPE.as_bytes());
        assert_eq!(take_string(&mut view), key.public_raw);
        assert!(view.is_empty());
        assert!(!line.contains('\n'));
    }

    #[test]
    fn two_keys_differ() {
        let a = HostKey::generate().unwrap();
        let b = HostKey::generate().unwrap();
        assert_ne!(a.public_line(), b.public_line());
        assert_ne!(a.private_openssh(), b.private_openssh());
    }

    #[test]
    fn debug_hides_the_private_part() {
        let key = HostKey::generate().unwrap();
        let shown = format!("{key:?}");
        let body: String = key
            .private_openssh()
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect();
        assert!(!shown.contains(&body[..40]));
        assert!(!shown.contains("PRIVATE KEY"));
        assert!(shown.contains("redacted"));
    }

    /// Настоящий `ssh-keygen` читает файл и сходится с нашими открытым ключом и отпечатком.
    #[test]
    fn ssh_keygen_accepts_the_key_and_agrees_on_the_public_part() {
        let key = HostKey::generate().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host_key");
        std::fs::write(&path, key.private_openssh()).unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let derived = Command::new("ssh-keygen")
            .args(["-y", "-f"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(derived.status.success(), "{derived:?}");
        let derived = String::from_utf8(derived.stdout).unwrap();
        assert_eq!(
            derived
                .trim()
                .split(' ')
                .take(2)
                .collect::<Vec<_>>()
                .join(" "),
            key.public_line()
        );

        let print = Command::new("ssh-keygen")
            .args(["-l", "-E", "sha256", "-f"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(print.status.success(), "{print:?}");
        let print = String::from_utf8(print.stdout).unwrap();
        assert_eq!(
            print.split_whitespace().nth(1),
            Some(key.fingerprint_sha256().as_str())
        );
    }
}
