//! Канонизация модели по каталогу машины: общий разбор для `session_spawn`
//! и маршрута владельца. Агент называет модель как привык, а дальше идёт
//! канонический идентификатор каталога.

/// Модель каталога для разбора: идентификатор и название.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ModelRef<'a> {
    pub(crate) id: &'a str,
    pub(crate) title: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolveError {
    /// Совпало несколько моделей: идентификаторы в порядке каталога.
    Ambiguous(Vec<String>),
    Unknown,
}

/// Сколько идентификаторов перечислять в ошибке неизвестной модели.
const MAX_LISTED_MODELS: usize = 40;

/// Индекс модели в каталоге. `None` и `default` — первая модель каталога.
pub(crate) fn resolve_model(
    requested: Option<&str>,
    models: &[ModelRef<'_>],
) -> Result<usize, ResolveError> {
    let Some(requested) = requested.filter(|name| *name != super::whitelist::DEFAULT_MODEL) else {
        return first(models);
    };
    let wanted = tokens(requested);
    // 1. Точный идентификатор каталога.
    if let Some(index) = models.iter().position(|model| model.id == requested) {
        return Ok(index);
    }
    // 2. Нормализованное равенство с идентификатором, его хвостом без провайдера и названием.
    if let Some(hits) = unique(models, |model| {
        [model.id, bare(model.id)]
            .map(tokens)
            .into_iter()
            .chain(model.title.map(tokens))
            .any(|candidate| candidate == wanted)
    })? {
        return Ok(hits);
    }
    // 3. Семейство: без `claude`, `1m` и числовых версий.
    let family = family(&wanted);
    if !family.is_empty()
        && let Some(hits) = unique(models, |model| {
            family_of(model).any(|candidate| candidate == family)
        })?
    {
        return Ok(hits);
    }
    // 4. Единственное продолжение по префиксу токенов.
    if !wanted.is_empty()
        && let Some(hits) = unique(models, |model| {
            [bare(model.id), model.title.unwrap_or_default()]
                .map(tokens)
                .into_iter()
                .any(|candidate| candidate.starts_with(&wanted))
        })?
    {
        return Ok(hits);
    }
    Err(ResolveError::Unknown)
}

fn first(models: &[ModelRef<'_>]) -> Result<usize, ResolveError> {
    if models.is_empty() {
        Err(ResolveError::Unknown)
    } else {
        Ok(0)
    }
}

/// Единственное попадание ступени: `None` — ни одного, ошибка — несколько.
fn unique(
    models: &[ModelRef<'_>],
    matches: impl Fn(&ModelRef<'_>) -> bool,
) -> Result<Option<usize>, ResolveError> {
    let hits: Vec<usize> = models
        .iter()
        .enumerate()
        .filter(|(_, model)| matches(model))
        .map(|(index, _)| index)
        .collect();
    match hits.as_slice() {
        [] => Ok(None),
        [index] => Ok(Some(*index)),
        _ => Err(ResolveError::Ambiguous(
            hits.iter()
                .map(|index| models[*index].id.to_owned())
                .collect(),
        )),
    }
}

/// Идентификатор без префикса провайдера: текст после последнего `/`.
fn bare(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

/// Токены названия: нижний регистр, разделители — пробелы и знаки `-_. /[]`.
fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| c.is_whitespace() || matches!(c, '-' | '_' | '.' | '/' | '[' | ']'))
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Семейство модели: без ведущего `claude`, без `1m` и без чисто числовых токенов.
fn family(tokens: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(tokens.len());
    for (index, token) in tokens.iter().enumerate() {
        if index == 0 && token == "claude" {
            continue;
        }
        if token == "1m" || token.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        out.push(token.clone());
    }
    out
}

/// Семейство кандидата по идентификатору без провайдера и по названию.
fn family_of(model: &ModelRef<'_>) -> impl Iterator<Item = Vec<String>> {
    [Some(bare(model.id)), model.title]
        .into_iter()
        .flatten()
        .map(tokens)
        .map(|tokens| family(&tokens))
}

pub(crate) fn unknown_message(requested: &str, harness: &str, models: &[ModelRef<'_>]) -> String {
    let ids: Vec<&str> = models
        .iter()
        .take(MAX_LISTED_MODELS)
        .map(|model| model.id)
        .collect();
    let mut message = format!(
        "Unknown model {requested:?} for {harness}. Available: {}",
        ids.join(", ")
    );
    if models.len() > MAX_LISTED_MODELS {
        message.push_str(&format!(", and {} more", models.len() - MAX_LISTED_MODELS));
    }
    message
}

pub(crate) fn ambiguous_message(requested: &str, harness: &str, ids: &[String]) -> String {
    format!(
        "Model {requested:?} matches several models of {harness}: {}. Pass one of these ids.",
        ids.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: &[(&str, &str)] = &[
        ("opus[1m]", "Opus 1M"),
        ("sonnet[1m]", "Sonnet 1M"),
        ("fable[1m]", "Fable 1M"),
        ("haiku", "Haiku"),
    ];

    const OPENCODE: &[(&str, &str)] = &[
        ("opencode-go/deepseek-v4.1-flash", "DeepSeek V4.1 Flash"),
        ("opencode-go/deepseek-v3.2", "DeepSeek V3.2"),
        (
            "opencode-go/muse-spark-1.3-contributor",
            "Muse Spark 1.3 Contributor",
        ),
    ];

    const CODEX: &[(&str, &str)] = &[("gpt-6.1-sol", "GPT 6.1 Sol")];

    fn entries<'a>(list: &'a [(&'a str, &'a str)]) -> Vec<ModelRef<'a>> {
        list.iter()
            .map(|(id, title)| ModelRef {
                id,
                title: Some(title),
            })
            .collect()
    }

    fn id(requested: Option<&str>, list: &[(&str, &str)]) -> Result<String, ResolveError> {
        let models = entries(list);
        resolve_model(requested, &models).map(|index| list[index].0.to_owned())
    }

    #[test]
    fn claude_catalog_accepts_human_names() {
        let cases = [
            ("opus[1m]", "opus[1m]"),
            ("Opus 1M", "opus[1m]"),
            ("opus 1m", "opus[1m]"),
            ("opus", "opus[1m]"),
            ("Opus 5.5", "opus[1m]"),
            ("claude-opus-5-5", "opus[1m]"),
            ("sonnet", "sonnet[1m]"),
            ("Sonnet 1M", "sonnet[1m]"),
            ("claude-sonnet-5-5", "sonnet[1m]"),
            ("Fable", "fable[1m]"),
            ("fable", "fable[1m]"),
            ("Fable 5.2", "fable[1m]"),
            ("claude-fable-5-2", "fable[1m]"),
            ("fable[1m]", "fable[1m]"),
            ("Claude Fable", "fable[1m]"),
            ("Haiku", "haiku"),
            ("haiku", "haiku"),
            ("claude-haiku-5-5", "haiku"),
        ];
        for (requested, expected) in cases {
            assert_eq!(
                id(Some(requested), CLAUDE).as_deref(),
                Ok(expected),
                "{requested}"
            );
        }
    }

    #[test]
    fn unknown_name_is_not_resolved() {
        assert_eq!(id(Some("gemini"), CLAUDE), Err(ResolveError::Unknown));
    }

    #[test]
    fn default_and_omitted_pick_the_first_model() {
        assert_eq!(id(None, CLAUDE).as_deref(), Ok("opus[1m]"));
        assert_eq!(id(Some("default"), CLAUDE).as_deref(), Ok("opus[1m]"));
        assert_eq!(id(None, &[]), Err(ResolveError::Unknown));
        assert_eq!(id(Some("default"), &[]), Err(ResolveError::Unknown));
    }

    #[test]
    fn opencode_names_and_ambiguous_prefixes() {
        assert_eq!(
            id(Some("deepseek-v4.1-flash"), OPENCODE).as_deref(),
            Ok("opencode-go/deepseek-v4.1-flash")
        );
        assert_eq!(
            id(Some("DeepSeek V4.1 Flash"), OPENCODE).as_deref(),
            Ok("opencode-go/deepseek-v4.1-flash")
        );
        assert_eq!(
            id(Some("muse"), OPENCODE).as_deref(),
            Ok("opencode-go/muse-spark-1.3-contributor")
        );
        assert_eq!(
            id(Some("deepseek"), OPENCODE),
            Err(ResolveError::Ambiguous(vec![
                "opencode-go/deepseek-v4.1-flash".to_owned(),
                "opencode-go/deepseek-v3.2".to_owned(),
            ]))
        );
    }

    #[test]
    fn codex_title_resolves() {
        assert_eq!(id(Some("GPT 6.1 Sol"), CODEX).as_deref(), Ok("gpt-6.1-sol"));
        assert_eq!(id(Some("gpt"), CODEX).as_deref(), Ok("gpt-6.1-sol"));
    }

    #[test]
    fn unknown_message_lists_catalog_ids_and_caps_the_list() {
        let many: Vec<(String, String)> = (0..45)
            .map(|n| (format!("m{n}"), format!("Model {n}")))
            .collect();
        let models: Vec<ModelRef<'_>> = many
            .iter()
            .map(|(id, title)| ModelRef {
                id,
                title: Some(title),
            })
            .collect();
        let message = unknown_message("gemini", "codex", &models);
        assert_eq!(
            message,
            format!(
                "Unknown model \"gemini\" for codex. Available: {}, and 5 more",
                (0..40)
                    .map(|n| format!("m{n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        );
    }

    #[test]
    fn ambiguous_message_names_the_ids() {
        assert_eq!(
            ambiguous_message(
                "deepseek",
                "opencode",
                &[
                    "opencode-go/deepseek-v4.1-flash".to_owned(),
                    "opencode-go/deepseek-v3.2".to_owned(),
                ]
            ),
            "Model \"deepseek\" matches several models of opencode: \
             opencode-go/deepseek-v4.1-flash, opencode-go/deepseek-v3.2. Pass one of these ids."
        );
    }
}
