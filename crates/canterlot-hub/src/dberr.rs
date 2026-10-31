use std::fmt;

/// Всё, что из ошибки базы данных разрешено попадать в журнал и в ответ:
/// SQLSTATE, имя ограничения и грубый класс. Текст драйвера (DETAIL несёт значения строк)
/// не читается никогда.
#[derive(Clone, PartialEq, Eq)]
pub struct DbErrorInfo {
    pub sqlstate: Option<String>,
    pub constraint: Option<String>,
    pub class: &'static str,
}

pub const CLASS_UNAVAILABLE: &str = "unavailable";
pub const CLASS_CONSTRAINT: &str = "constraint";
pub const CLASS_INTERNAL: &str = "internal";

pub fn describe(error: &sqlx::Error) -> DbErrorInfo {
    if let sqlx::Error::Database(database) = error {
        let sqlstate = database.code().map(|code| code.into_owned());
        let class = match sqlstate.as_deref() {
            Some(code) if is_transient(code) => CLASS_UNAVAILABLE,
            Some(code) if code.starts_with("23") => CLASS_CONSTRAINT,
            _ => CLASS_INTERNAL,
        };
        return DbErrorInfo {
            sqlstate,
            constraint: database.constraint().map(str::to_owned),
            class,
        };
    }
    let class = match error {
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed
        | sqlx::Error::BeginFailed => CLASS_UNAVAILABLE,
        _ => CLASS_INTERNAL,
    };
    DbErrorInfo {
        sqlstate: None,
        constraint: None,
        class,
    }
}

/// Состояния, которые проходят сами: потеря связи, нехватка ресурсов, остановка сервера,
/// сериализация, взаимоблокировка, таймаут блокировки и отмена запроса.
fn is_transient(sqlstate: &str) -> bool {
    sqlstate.starts_with("08")
        || sqlstate.starts_with("53")
        || matches!(
            sqlstate,
            "40001" | "40P01" | "55P03" | "57014" | "57P01" | "57P02" | "57P03"
        )
}

impl DbErrorInfo {
    pub fn is_unavailable(&self) -> bool {
        self.class == CLASS_UNAVAILABLE
    }

    /// База отвергла само значение: класс 22 (данные) и 54 (предел строки индекса).
    /// Повтор того же значения даст тот же отказ.
    pub fn is_rejected_value(&self) -> bool {
        self.sqlstate
            .as_deref()
            .is_some_and(|code| code.starts_with("22") || code.starts_with("54"))
    }
}

impl fmt::Debug for DbErrorInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for DbErrorInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "sqlstate={} constraint={} class={}",
            self.sqlstate.as_deref().unwrap_or("-"),
            self.constraint.as_deref().unwrap_or("-"),
            self.class
        )
    }
}
