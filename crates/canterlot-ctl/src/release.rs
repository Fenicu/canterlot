use std::{
    collections::BTreeMap,
    fmt::Write as FmtWrite,
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
};

use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

use crate::{cli::Version, error::CtlError};

/// Источник релизов: две базовые ссылки и репозиторий, из которого берутся теги.
#[derive(Clone, Debug)]
pub struct ReleaseSource {
    pub api_base: Url,
    pub download_base: Url,
    pub repo: String,
}

impl Default for ReleaseSource {
    fn default() -> Self {
        Self {
            api_base: Url::parse("https://api.github.com/").expect("fixed url"),
            download_base: Url::parse("https://github.com/").expect("fixed url"),
            repo: "Fenicu/canterlot".into(),
        }
    }
}

impl ReleaseSource {
    fn latest_url(&self) -> Result<Url, CtlError> {
        Ok(self
            .api_base
            .join(&format!("repos/{}/releases/latest", self.repo))?)
    }

    fn asset_url(&self, tag: &str, name: &str) -> Result<Url, CtlError> {
        Ok(self
            .download_base
            .join(&format!("{}/releases/download/{}/{}", self.repo, tag, name))?)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Assets {
    pub hub: String,
    pub worker: String,
    pub web: String,
    pub csp: String,
    pub ctl: String,
    pub install: String,
    pub sums: String,
}

impl Assets {
    pub fn names(&self) -> [&str; 7] {
        [
            &self.hub,
            &self.worker,
            &self.web,
            &self.csp,
            &self.ctl,
            &self.install,
            &self.sums,
        ]
    }
}

pub fn asset_names(v: &Version) -> Assets {
    let x = v.as_str();
    Assets {
        hub: format!("canterlot-hub-{x}-x86_64-linux-gnu.tar.gz"),
        worker: format!("canterlot-worker-{x}-x86_64-linux-gnu.tar.gz"),
        web: format!("canterlot-web-{x}.tar.gz"),
        csp: format!("canterlot-csp-{x}.txt"),
        ctl: format!("canterlot-{x}-x86_64-linux.tar.gz"),
        install: "install.sh".into(),
        sums: "SHA256SUMS".into(),
    }
}

#[derive(Clone, Debug)]
pub struct Staged {
    pub version: Version,
    pub dir: PathBuf,
    pub files: BTreeMap<String, PathBuf>,
}

#[derive(Deserialize)]
struct Latest {
    tag_name: String,
}

pub async fn resolve(src: &ReleaseSource, want: Option<&Version>) -> Result<Version, CtlError> {
    if let Some(version) = want {
        return Ok(version.clone());
    }
    let client = reqwest::Client::new();
    let latest: Latest = client
        .get(src.latest_url()?)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let tag = latest
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&latest.tag_name);
    Version::parse(tag)
}

/// Имя актива должно быть обычным именем файла: каталоги и `..` отсюда не проходят.
fn safe_name(name: &str) -> Result<&str, CtlError> {
    let path = Path::new(name);
    if name.is_empty()
        || matches!(name, "." | "..")
        || path.components().count() != 1
        || path.file_name().and_then(|part| part.to_str()) != Some(name)
    {
        return Err(CtlError::UnsafeArchive(name.into()));
    }
    Ok(name)
}

pub async fn fetch(
    src: &ReleaseSource,
    v: &Version,
    assets: &[&str],
    into: &Path,
) -> Result<Staged, CtlError> {
    fs::create_dir_all(into)?;
    let dir = into.join(format!(".stage-{}", Uuid::now_v7()));
    fs::create_dir(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    let client = reqwest::Client::new();
    // Все файлы берутся из одного заранее разрешённого тега, даже если «последний» изменился.
    let tag = format!("v{}", v.as_str());
    let mut files = BTreeMap::new();
    for &name in assets {
        let name = safe_name(name)?;
        let url = src.asset_url(&tag, name)?;
        let bytes = client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let path = dir.join(name);
        fs::write(&path, &bytes)?;
        files.insert(name.to_string(), path);
    }
    // SHA256SUMS не может содержать собственную контрольную сумму.
    let check: Vec<&str> = assets
        .iter()
        .copied()
        .filter(|name| *name != "SHA256SUMS")
        .collect();
    verify(&dir, &dir.join("SHA256SUMS"), &check)?;
    Ok(Staged {
        version: v.clone(),
        dir,
        files,
    })
}

/// Версия каталога выпуска: строка `version=` в `RELEASE` (формат `ключ=значение`, как его
/// пишет build-release.sh), иначе имя единственного архива хаба в каталоге.
pub fn dir_version(dir: &Path) -> Result<Version, CtlError> {
    match fs::read_to_string(dir.join("RELEASE")) {
        Ok(text) => {
            for line in text.lines() {
                if let Some((key, value)) = line.split_once('=')
                    && key.trim() == "version"
                {
                    return Version::parse(value.trim().trim_matches('"'));
                }
            }
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let mut found = Vec::new();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        // Машине воркера нужен только его архив: версия читается и из его имени.
        if let Some(rest) = name
            .strip_prefix("canterlot-hub-")
            .or_else(|| name.strip_prefix("canterlot-worker-"))
            && let Some(version) = rest.strip_suffix("-x86_64-linux-gnu.tar.gz")
        {
            let version = Version::parse(version)?;
            if !found.contains(&version) {
                found.push(version);
            }
        }
    }
    match found.as_slice() {
        [one] => Ok(one.clone()),
        _ => Err(CtlError::AssetMissing("version".into())),
    }
}

pub fn from_dir(dir: &Path, assets: &[&str]) -> Result<Staged, CtlError> {
    let check: Vec<&str> = assets
        .iter()
        .copied()
        .filter(|name| *name != "SHA256SUMS")
        .collect();
    verify(dir, &dir.join("SHA256SUMS"), &check)?;
    let mut files = BTreeMap::new();
    for &name in assets {
        let name = safe_name(name)?;
        let path = dir.join(name);
        if !path.is_file() {
            return Err(CtlError::AssetMissing(name.into()));
        }
        files.insert(name.to_string(), path);
    }
    Ok(Staged {
        version: dir_version(dir)?,
        dir: dir.to_path_buf(),
        files,
    })
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Для каждого имени должна быть ровно одна подходящая строка с совпадающим хэшем.
pub fn verify(dir: &Path, sums: &Path, names: &[&str]) -> Result<(), CtlError> {
    let text = match fs::read_to_string(sums) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Err(CtlError::ChecksumMissing(sums.display().to_string()));
        }
        Err(err) => return Err(err.into()),
    };
    let mut expected = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let hash = parts.next().unwrap_or_default();
        let raw = parts.next().unwrap_or_default().trim();
        let name = raw.strip_prefix('*').unwrap_or(raw);
        if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            expected.insert(name.to_string(), hash.to_ascii_lowercase());
        }
    }
    for &name in names {
        let name = safe_name(name)?;
        let want = expected
            .get(name)
            .ok_or_else(|| CtlError::ChecksumMissing(name.into()))?;
        let bytes = fs::read(dir.join(name))?;
        let got = hex(&Sha256::digest(&bytes));
        if got != *want {
            return Err(CtlError::ChecksumMismatch(name.into()));
        }
    }
    Ok(())
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

fn absolute(path: &Path) -> Result<PathBuf, CtlError> {
    if path.is_absolute() {
        return Ok(normalize(path));
    }
    Ok(normalize(&std::env::current_dir()?.join(path)))
}

fn unsafe_path(path: &Path) -> bool {
    path.as_os_str().is_empty()
        || path.components().any(|part| {
            matches!(
                part,
                Component::RootDir | Component::ParentDir | Component::Prefix(_)
            )
        })
}

/// Распаковывает архив, не допуская выход за `dest` через пути или символические ссылки.
pub fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<(), CtlError> {
    fs::create_dir_all(dest)?;
    let root = absolute(dest)?;
    let file = fs::File::open(archive)?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let entries = archive.entries()?;
    for entry in entries {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if unsafe_path(&path) {
            return Err(CtlError::UnsafeArchive(path.display().to_string()));
        }
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            let link = entry.link_name()?.ok_or_else(|| {
                CtlError::UnsafeArchive(format!("link without target: {}", path.display()))
            })?;
            if unsafe_path(&link) {
                return Err(CtlError::UnsafeArchive(format!(
                    "link {} -> {}",
                    path.display(),
                    link.display()
                )));
            }
            let base = path.parent().unwrap_or_else(|| Path::new(""));
            if !normalize(&root.join(base).join(&link)).starts_with(&root) {
                return Err(CtlError::UnsafeArchive(format!(
                    "link {} -> {}",
                    path.display(),
                    link.display()
                )));
            }
        }
        entry
            .unpack_in(&root)
            .map_err(|err| CtlError::UnsafeArchive(err.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            let _ = write!(out, "{b:02x}");
        }
        out
    }

    #[test]
    fn asset_names_follow_release_contract() {
        let v = Version::parse("1.2.3").unwrap();
        let a = asset_names(&v);
        assert_eq!(a.hub, "canterlot-hub-1.2.3-x86_64-linux-gnu.tar.gz");
        assert_eq!(a.worker, "canterlot-worker-1.2.3-x86_64-linux-gnu.tar.gz");
        assert_eq!(a.web, "canterlot-web-1.2.3.tar.gz");
        assert_eq!(a.csp, "canterlot-csp-1.2.3.txt");
        assert_eq!(a.ctl, "canterlot-1.2.3-x86_64-linux.tar.gz");
        assert_eq!(a.install, "install.sh");
        assert_eq!(a.sums, "SHA256SUMS");
    }

    #[test]
    fn dir_version_reads_key_value_release_and_falls_back_to_hub_archive() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path()
                .join("canterlot-hub-1.4.0-x86_64-linux-gnu.tar.gz"),
            b"",
        )
        .unwrap();
        assert_eq!(dir_version(dir.path()).unwrap().as_str(), "1.4.0");
        fs::write(
            dir.path().join("RELEASE"),
            "sha=0123456789ab\nversion=1.4.0\nsource=0000000000000000000000000000000000000000\n",
        )
        .unwrap();
        assert_eq!(dir_version(dir.path()).unwrap().as_str(), "1.4.0");
        fs::write(dir.path().join("RELEASE"), "sha=0123456789ab\n").unwrap();
        assert_eq!(dir_version(dir.path()).unwrap().as_str(), "1.4.0");
    }

    #[test]
    fn hex_of_known_bytes() {
        assert_eq!(hex(b"\x00\xff"), "00ff");
    }
}
