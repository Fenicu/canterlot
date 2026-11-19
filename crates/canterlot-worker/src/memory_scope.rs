//! Канонические корни рабочих проектов для общей памяти: воркер сам разворачивает `~` и
//! симлинки, а хаб по ним классифицирует рабочий каталог разговора.

use std::path::{Path, PathBuf};

/// Каноничные корни: `~` раскрыт относительно `home`, симлинки разрешены, пути без
/// существующего каталога отброшены, повторы убраны.
pub fn canonical_roots(roots: &[String], home: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for root in roots {
        let expanded = if root == "~" {
            home.to_path_buf()
        } else if let Some(rest) = root.strip_prefix("~/") {
            home.join(rest)
        } else {
            PathBuf::from(root)
        };
        if !expanded.is_absolute() {
            continue;
        }
        if let Ok(real) = std::fs::canonicalize(&expanded)
            && real.is_dir()
            && !out.contains(&real)
        {
            out.push(real);
        }
    }
    out.sort();
    out
}

/// Realpath рабочего каталога; `None`, если его нет.
pub fn canonical_workdir(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn canonical_roots_resolve_symlinks_and_drop_missing() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("h");
        std::fs::create_dir_all(home.join("work")).unwrap();
        std::fs::create_dir_all(home.join("real")).unwrap();
        symlink(home.join("real"), home.join("link")).unwrap();
        let roots = vec![
            "~/work".to_string(),
            "~/link".to_string(),
            "~/missing".to_string(),
            "relative/dir".to_string(),
            home.join("work").to_string_lossy().into_owned(),
        ];
        let got = canonical_roots(&roots, &home);
        let real_home = std::fs::canonicalize(&home).unwrap();
        assert_eq!(got, vec![real_home.join("real"), real_home.join("work")]);
    }

    #[test]
    fn canonical_workdir_resolves_symlink_and_none_for_missing() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(base.join("work/p")).unwrap();
        symlink(base.join("work/p"), base.join("alias")).unwrap();
        assert_eq!(
            canonical_workdir(&base.join("alias")),
            Some(base.join("work/p"))
        );
        assert_eq!(canonical_workdir(&base.join("nope")), None);
    }
}
