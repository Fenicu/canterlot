//! Деревья PWA на хосте: каталоги выпусков, слияние старых чанков, ссылка `build` и флаг
//! обслуживания, который читает Caddy (`/srv/canterlot-web/maintenance`).

use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{cli::Version, error::CtlError, fsx, layout::Layout, release};

/// Сколько прежних выпусков отдают свои неизменяемые чанки новому дереву.
pub const MERGE_FROM: usize = 3;

pub fn maintenance_flag(layout: &Layout) -> PathBuf {
    layout.web_parent().join("maintenance")
}

/// Пока файл есть, Caddy отвечает 503 на `/api/*`, статика раздаётся дальше.
pub fn set_maintenance(layout: &Layout) -> Result<(), CtlError> {
    fsx::write_atomic_mode(&maintenance_flag(layout), b"maintenance\n", 0o644)
}

pub fn clear_maintenance(layout: &Layout) -> Result<(), CtlError> {
    // remove_file удаляет саму ссылку, если её подложили, и не идёт по ней.
    match fs::remove_file(maintenance_flag(layout)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Версии каталогов `web/releases/` по возрастанию; посторонние имена пропускаются.
pub fn web_versions(layout: &Layout) -> Result<Vec<Version>, CtlError> {
    versions_in(&layout.web_parent().join("releases"))
}

pub fn versions_in(dir: &Path) -> Result<Vec<Version>, CtlError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str()
            && let Ok(version) = Version::parse(name)
        {
            found.push(version);
        }
    }
    found.sort_by_key(Version::key);
    Ok(found)
}

/// Распаковка во временный каталог рядом и переименование: каталог выпуска либо полный,
/// либо его нет, поэтому повтор после сбоя просто берёт готовый.
pub fn extract_once(archive: &Path, dest: &Path) -> Result<(), CtlError> {
    if dest.exists() {
        return Ok(());
    }
    let parent = dest.parent().unwrap_or_else(|| Path::new("/"));
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".extract-{}", Uuid::now_v7()));
    fs::create_dir(&tmp)?;
    if let Err(err) = release::extract_tar_gz(archive, &tmp) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(err);
    }
    fs::rename(&tmp, dest)?;
    Ok(())
}

/// Дописывает в `target/_app/immutable` файлы прежних выпусков, которых в нём нет, вместе
/// со сжатыми соседями (`.br`, `.gz`, `.zst`): открытая вкладка старой версии ещё грузит
/// свои чанки. Существующие файлы не заменяются, ссылки не копируются и не обходятся.
pub fn merge_chunks(target: &Path, sources: &[PathBuf]) -> Result<u64, CtlError> {
    let mut copied = 0;
    for source in sources {
        let from = source.join("_app/immutable");
        match fs::symlink_metadata(&from) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => continue,
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => return Err(err.into()),
        }
        copied += merge_dir(target, &from, &target.join("_app/immutable"))?;
    }
    Ok(copied)
}

fn merge_dir(target: &Path, from: &Path, to: &Path) -> Result<u64, CtlError> {
    let mut copied = 0;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if kind.is_dir() {
            copied += merge_dir(target, &entry.path(), &dest)?;
        } else if kind.is_file() {
            match fs::symlink_metadata(&dest) {
                Ok(_) => continue,
                Err(err) if err.kind() == ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
            fsx::ensure_dir(target, to, 0o755)?;
            // Через временный файл: оборванная копия не выглядит готовой при повторе.
            let tmp = to.join(format!(".merge-{}.tmp", Uuid::now_v7()));
            fs::copy(entry.path(), &tmp)?;
            fs::rename(&tmp, &dest)?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// Ссылка `build` переключается на `releases/<v>` заменой через rename: в каждый момент
/// она указывает либо на старое дерево, либо на новое. Настоящий каталог на её месте — отказ.
pub fn flip_build(layout: &Layout, v: &Version) -> Result<(), CtlError> {
    let link = layout.web_parent().join("build");
    match fs::symlink_metadata(&link) {
        Ok(meta) if !meta.file_type().is_symlink() => {
            return Err(CtlError::Other(format!(
                "not_a_symlink: {}",
                link.display()
            )));
        }
        _ => {}
    }
    fsx::replace_symlink(&Path::new("releases").join(v.as_str()), &link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn chunk_merge_keeps_old_immutable_and_compressed_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("1.2.3");
        let older = dir.path().join("1.2.2");
        let new = dir.path().join("1.3.0");
        write(&old.join("_app/immutable/chunks/a.js"), "old a");
        write(&old.join("_app/immutable/chunks/a.js.br"), "old a br");
        write(&old.join("_app/immutable/chunks/a.js.gz"), "old a gz");
        write(&old.join("_app/immutable/chunks/a.js.zst"), "old a zst");
        write(&old.join("_app/immutable/shared.js"), "old shared");
        write(&older.join("_app/immutable/entry/b.js"), "older b");
        write(&old.join("200.html"), "old html");
        write(&new.join("_app/immutable/shared.js"), "new shared");
        write(&new.join("200.html"), "new html");
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, "secret").unwrap();
        symlink(&outside, old.join("_app/immutable/link.js")).unwrap();

        let copied = merge_chunks(&new, &[old.clone(), older.clone()]).unwrap();
        assert_eq!(copied, 5);
        let read = |p: &str| fs::read_to_string(new.join(p)).unwrap();
        assert_eq!(read("_app/immutable/chunks/a.js"), "old a");
        assert_eq!(read("_app/immutable/chunks/a.js.br"), "old a br");
        assert_eq!(read("_app/immutable/chunks/a.js.gz"), "old a gz");
        assert_eq!(read("_app/immutable/chunks/a.js.zst"), "old a zst");
        assert_eq!(read("_app/immutable/entry/b.js"), "older b");
        // Свои файлы нового выпуска не заменяются, вне immutable ничего не переносится.
        assert_eq!(read("_app/immutable/shared.js"), "new shared");
        assert_eq!(read("200.html"), "new html");
        assert!(fs::symlink_metadata(new.join("_app/immutable/link.js")).is_err());
        // Повтор ничего не меняет.
        assert_eq!(merge_chunks(&new, &[old, older]).unwrap(), 0);
    }

    #[test]
    fn build_link_flip_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().to_path_buf(), None);
        let releases = layout.web_parent().join("releases");
        fs::create_dir_all(releases.join("1.2.3")).unwrap();
        fs::create_dir_all(releases.join("1.3.0")).unwrap();
        let link = layout.web_parent().join("build");
        symlink("releases/1.2.3", &link).unwrap();
        let before = fs::symlink_metadata(&link).unwrap();
        assert!(before.file_type().is_symlink());

        flip_build(&layout, &v("1.3.0")).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("releases/1.3.0"));
        // Ссылка относительная и заменена целиком; временных ссылок не осталось.
        let leftovers: Vec<_> = fs::read_dir(layout.web_parent())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // Настоящий каталог на месте ссылки не заменяется.
        fs::remove_file(&link).unwrap();
        fs::create_dir(&link).unwrap();
        assert!(flip_build(&layout, &v("1.2.3")).is_err());
        assert!(fs::symlink_metadata(&link).unwrap().is_dir());
    }

    #[test]
    fn web_versions_sort_numerically() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().to_path_buf(), None);
        for name in ["1.9.0", "1.10.0", "1.2.3", "junk"] {
            fs::create_dir_all(layout.web_parent().join("releases").join(name)).unwrap();
        }
        let got: Vec<String> = web_versions(&layout)
            .unwrap()
            .iter()
            .map(|v| v.to_string())
            .collect();
        assert_eq!(got, ["1.2.3", "1.9.0", "1.10.0"]);
    }

    #[test]
    fn maintenance_flag_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().to_path_buf(), None);
        fs::create_dir_all(layout.web_parent()).unwrap();
        set_maintenance(&layout).unwrap();
        assert!(maintenance_flag(&layout).is_file());
        clear_maintenance(&layout).unwrap();
        clear_maintenance(&layout).unwrap();
        assert!(!maintenance_flag(&layout).exists());
    }
}
