//! Команды `canterlot-hub auth …` на хосте хаба: им нужен доступ к конфигу и базе,
//! поэтому они остаются и после первого входа (отклонение от ТЗ §15, ADR-0005).

use std::io::{BufRead, Write};

use sqlx::PgPool;
use uuid::Uuid;

use super::{
    RevokeReason, label, password,
    recovery::{self, DEFAULT_MINUTES, MAX_MINUTES},
    sessions,
};
use crate::{config::HubConfig, notify};

/// Ссылка восстановления. Без `push` печатается; с `push` уходит только в Gotify
/// владельца, а напечатано лишь «sent».
pub async fn recovery_link(
    pool: &PgPool,
    config: &HubConfig,
    owner_id: Uuid,
    minutes: Option<u32>,
    push: bool,
) -> Result<String, String> {
    let minutes = minutes.unwrap_or(DEFAULT_MINUTES);
    if !(1..=MAX_MINUTES).contains(&minutes) {
        return Err("minutes_out_of_range".into());
    }
    if config.http.is_none() {
        return Err("http_not_configured".into());
    }
    let sender = if push {
        let gotify = config
            .notify
            .gotify
            .as_ref()
            .ok_or("notify_not_configured")?;
        Some(notify::Sender::new(gotify)?)
    } else {
        None
    };
    let token = recovery::issue(pool, owner_id, minutes)
        .await
        .map_err(|_| "db_error")?;
    let link = format!("{}/recover#{token}", config.auth.origin());
    let Some(sender) = sender else {
        return Ok(link);
    };
    let message = notify::Message {
        title: "Вход в canterlot".into(),
        body: format!("Ссылка для входа действует {minutes} мин. и открывается один раз: {link}"),
        priority: notify::PRIORITY_SIGN_IN,
        click_url: link,
    };
    sender.send_now(&message).await?;
    Ok("sent".into())
}

/// Пароль из TTY (без эха, дважды) или одной строкой из stdin.
pub fn read_password(from_stdin: bool) -> Result<String, String> {
    if from_stdin {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|_| "stdin_error")?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_owned());
    }
    let first = prompt_hidden("Новый пароль: ")?;
    let second = prompt_hidden("Повторите пароль: ")?;
    if first != second {
        return Err("passwords_differ".into());
    }
    Ok(first)
}

fn prompt_hidden(prompt: &str) -> Result<String, String> {
    use rustix::termios::{LocalModes, OptionalActions, isatty, tcgetattr, tcsetattr};
    let stdin = std::io::stdin();
    if !isatty(&stdin) {
        return Err("tty_required".into());
    }
    let saved = tcgetattr(&stdin).map_err(|_| "tty_error")?;
    let mut quiet = saved.clone();
    quiet.local_modes.remove(LocalModes::ECHO);
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    tcsetattr(&stdin, OptionalActions::Now, &quiet).map_err(|_| "tty_error")?;
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    let _ = tcsetattr(&stdin, OptionalActions::Now, &saved);
    eprintln!();
    read.map_err(|_| "stdin_error")?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

/// Задаёт пароль; с `revoke` завершает все сеансы (причина `server`).
pub async fn set_password(
    pool: &PgPool,
    owner_id: Uuid,
    new_password: String,
    revoke: bool,
) -> Result<String, String> {
    if !password::acceptable(&new_password) {
        return Err("password_length".into());
    }
    let phc = password::Passwords::default().hash(new_password).await?;
    let mut conn = pool.acquire().await.map_err(|_| "db_error")?;
    password::store(&mut conn, owner_id, &phc)
        .await
        .map_err(|_| "db_error")?;
    drop(conn);
    if !revoke {
        return Ok("password_set".into());
    }
    let revoked = sessions::revoke_all_except(pool, owner_id, None, RevokeReason::Server)
        .await
        .map_err(|_| "db_error")?;
    Ok(format!("password_set revoked={}", revoked.len()))
}

/// Живые сеансы, по строке на сеанс: id, способ, создан, последняя активность, адрес, подпись.
pub async fn list_sessions(pool: &PgPool, owner_id: Uuid) -> Result<String, String> {
    let rows = sessions::list(pool, owner_id, None)
        .await
        .map_err(|_| "db_error")?;
    let mut out = String::new();
    for row in rows {
        let title = row
            .name
            .clone()
            .or_else(|| label(row.user_agent.as_deref(), row.client_hints.as_ref()))
            .unwrap_or_else(|| "-".into());
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            row.id,
            row.method,
            row.created_at.format("%Y-%m-%dT%H:%M:%SZ"),
            row.last_seen_at.format("%Y-%m-%dT%H:%M:%SZ"),
            row.last_ip.as_deref().unwrap_or("-"),
            title.replace(['\t', '\n'], " "),
        ));
    }
    Ok(out)
}

/// Аварийная кнопка: завершает все сеансы (причина `server`).
pub async fn revoke_all(pool: &PgPool, owner_id: Uuid) -> Result<String, String> {
    let revoked = sessions::revoke_all_except(pool, owner_id, None, RevokeReason::Server)
        .await
        .map_err(|_| "db_error")?;
    Ok(format!("revoked={}", revoked.len()))
}
