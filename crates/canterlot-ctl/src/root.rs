//! Повышение прав: утилита сама перезапускает себя через sudo, чтобы SUDO_USER
//! указывал на вызвавшего пользователя, от которого ставится воркер.

use std::{fs::File, os::unix::process::CommandExt, path::PathBuf, process::Command};

use crate::error::CtlError;

/// Окружение повышения прав; в тестах подменяется, чтобы не запускать sudo.
pub trait Elevate {
    fn is_root(&self) -> bool;
    fn interactive(&self) -> bool;
    fn self_exe(&self) -> Option<PathBuf>;
    /// Возврат означает, что sudo не заменил процесс.
    fn exec_sudo(&self, args: &[String]) -> Result<(), CtlError>;
}

pub struct SystemElevate;

impl Elevate for SystemElevate {
    fn is_root(&self) -> bool {
        rustix::process::geteuid().is_root()
    }
    fn interactive(&self) -> bool {
        File::options()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .is_ok()
    }
    fn self_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }
    fn exec_sudo(&self, args: &[String]) -> Result<(), CtlError> {
        let error = Command::new("sudo").args(args).exec();
        Err(CtlError::Io(error))
    }
}

/// Команда, с которой утилиту запустили: её имя попадает в подсказку про sudo.
fn command_of(argv: &[String]) -> String {
    argv.first()
        .map(String::as_str)
        .unwrap_or("install")
        .to_owned()
}

/// Повысить права до root, перезапустив себя с теми же аргументами.
pub fn ensure_root(argv: &[String]) -> Result<(), CtlError> {
    ensure_root_with(argv, &SystemElevate)
}

pub fn ensure_root_with(argv: &[String], env: &dyn Elevate) -> Result<(), CtlError> {
    if env.is_root() {
        return Ok(());
    }
    // Без терминала sudo не сможет спросить пароль: отказываемся до запуска.
    if !env.interactive() {
        return Err(CtlError::RootRequired(command_of(argv)));
    }
    let self_exe = env
        .self_exe()
        .ok_or_else(|| CtlError::RootRequired(command_of(argv)))?;
    // sudo сохраняет SUDO_USER: install берёт оттуда имя пользователя воркера.
    let mut sudo_args = vec!["--".to_owned(), self_exe.to_string_lossy().into_owned()];
    sudo_args.extend_from_slice(argv);
    env.exec_sudo(&sudo_args)
        .map_err(|_| CtlError::RootRequired(command_of(argv)))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    struct Recording {
        is_root: bool,
        interactive: bool,
        self_exe: Option<PathBuf>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl Elevate for Recording {
        fn is_root(&self) -> bool {
            self.is_root
        }
        fn interactive(&self) -> bool {
            self.interactive
        }
        fn self_exe(&self) -> Option<PathBuf> {
            self.self_exe.clone()
        }
        fn exec_sudo(&self, args: &[String]) -> Result<(), CtlError> {
            self.calls.borrow_mut().push(args.to_vec());
            Ok(())
        }
    }

    #[test]
    fn non_root_reexecs_same_argv_through_sudo() {
        let env = Recording {
            interactive: true,
            self_exe: Some(PathBuf::from("/opt/canterlot/ctl/1.2.3/canterlot")),
            ..Recording::default()
        };
        let argv = ["install", "--tls", "external", "--yes"].map(String::from);
        ensure_root_with(&argv, &env).unwrap();
        assert_eq!(
            env.calls.into_inner(),
            vec![vec![
                "--".to_owned(),
                "/opt/canterlot/ctl/1.2.3/canterlot".to_owned(),
                "install".into(),
                "--tls".into(),
                "external".into(),
                "--yes".into(),
            ]]
        );
    }

    #[test]
    fn root_needs_no_sudo() {
        let env = Recording {
            is_root: true,
            ..Recording::default()
        };
        ensure_root_with(&["status".into()], &env).unwrap();
        assert!(env.calls.borrow().is_empty());
    }

    #[test]
    fn non_interactive_or_missing_self_refuses() {
        let env = Recording::default();
        assert!(matches!(
            ensure_root_with(&[], &env),
            Err(CtlError::RootRequired(_))
        ));
        let env = Recording {
            interactive: true,
            ..Recording::default()
        };
        assert!(matches!(
            ensure_root_with(&[], &env),
            Err(CtlError::RootRequired(_))
        ));
        assert!(env.calls.borrow().is_empty());
    }
}
