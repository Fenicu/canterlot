//! Табличные случаи на общей базе и общем приложении.
//!
//! Подготовка теста хаба (клон базы, установка, роутер) стоит дороже большинства самих
//! проверок, поэтому независимые случаи идут подряд на одной подготовке. Каждый случай
//! заводит своих владельцев, машины и разговоры и проверяет только их; общие очереди,
//! которые обходит планировщик, чистит шаг `before`.

/// Метка случая: если случай паникует, в выводе теста остаётся его имя.
pub struct Case(pub &'static str);

impl Drop for Case {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("case failed: {}", self.0);
        }
    }
}

/// Итог случая: `()` или `Result`, ошибка которого валит тест с именем случая.
pub trait Outcome {
    fn check(self, case: &str);
}

impl Outcome for () {
    fn check(self, _case: &str) {}
}

impl<E: std::fmt::Debug> Outcome for Result<(), E> {
    fn check(self, case: &str) {
        if let Err(error) = self {
            panic!("case {case} failed: {error:?}");
        }
    }
}

/// `run_cases!(pool.clone(); a, b)` вызывает `a(pool.clone()).await`, затем `b`.
/// `run_cases!(pool.clone(), before = clean; a, b)` перед каждым случаем ждёт
/// `clean(pool.clone())`. Упавший случай называет себя в выводе.
macro_rules! run_cases {
    ($fx:expr; $($case:ident),+ $(,)?) => {
        $({
            let _case = $crate::support::cases::Case(stringify!($case));
            $crate::support::cases::Outcome::check($case($fx).await, stringify!($case));
        })+
    };
    ($fx:expr, before = $before:path; $($case:ident),+ $(,)?) => {
        $({
            let _case = $crate::support::cases::Case(stringify!($case));
            $before($fx).await;
            $crate::support::cases::Outcome::check($case($fx).await, stringify!($case));
        })+
    };
}
pub(crate) use run_cases;

/// Число кругов вероятностного теста: полное в профиле nextest `stress`, короткое в
/// обычном гейте. `CANTERLOT_STRESS_ROUNDS` перекрывает оба значения.
pub fn stress_rounds(full: u32, gate: u32) -> u32 {
    if let Some(rounds) = std::env::var("CANTERLOT_STRESS_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        return rounds;
    }
    if std::env::var("NEXTEST_PROFILE").as_deref() == Ok("stress") {
        full
    } else {
        gate
    }
}
