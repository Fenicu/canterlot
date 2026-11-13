//! Словарь замен терминов диктовки: владелец пишет «мейн», служба слышит «мейн»,
//! в поле ввода приходит `main`.
//!
//! Применение — один проход слева направо, без повторного разбора вставленного
//! текста. Порядок правил — от самого длинного шаблона к короткому.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use utoipa::ToSchema;

/// Больше двухсот пар владелец не заводит.
pub const MAX_TERMS: usize = 200;
/// Предел шаблона и замены в скалярных значениях Unicode.
pub const MAX_TERM_CHARS: usize = 100;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Term {
    pub pattern: String,
    pub replacement: String,
}

/// Пары — личные данные владельца: в `Debug` только длины.
impl std::fmt::Debug for Term {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Term")
            .field("pattern_chars", &self.pattern.chars().count())
            .field("replacement_chars", &self.replacement.chars().count())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TermsBody {
    pub terms: Vec<Term>,
}

impl std::fmt::Debug for TermsBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermsBody")
            .field("terms", &self.terms.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermError {
    TooMany,
    Empty,
    TooLong,
    Duplicate,
}

/// Проверка присланного списка: обрезка пробелов и коды отказа для PWA.
pub fn validate(terms: Vec<Term>) -> Result<Vec<Term>, TermError> {
    if terms.len() > MAX_TERMS {
        return Err(TermError::TooMany);
    }
    let mut seen = HashSet::new();
    let mut stored = Vec::with_capacity(terms.len());
    for term in terms {
        let pattern = term.pattern.trim().to_owned();
        let replacement = term.replacement.trim().to_owned();
        if pattern.is_empty() || replacement.is_empty() {
            return Err(TermError::Empty);
        }
        if pattern.chars().count() > MAX_TERM_CHARS || replacement.chars().count() > MAX_TERM_CHARS
        {
            return Err(TermError::TooLong);
        }
        if !seen.insert(normalised(&pattern)) {
            return Err(TermError::Duplicate);
        }
        stored.push(Term {
            pattern,
            replacement,
        });
    }
    Ok(stored)
}

/// Ключ повтора: NFC, нижний регистр, ё как е, пробельные серии в один пробел.
fn normalised(pattern: &str) -> String {
    let folded: String = pattern
        .nfc()
        .collect::<String>()
        .chars()
        .map(fold)
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Простое посимвольное приведение для сравнения: ё и Ё равны е.
fn fold(c: char) -> char {
    match c {
        'ё' | 'Ё' => 'е',
        other => other.to_lowercase().next().unwrap_or(other),
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token {
    Char(char),
    Space,
}

#[derive(Clone, PartialEq, Eq)]
struct Rule {
    tokens: Vec<Token>,
    /// Длина шаблона в скалярных значениях: длинные правила идут первыми.
    length: usize,
    replacement: String,
    starts_word: bool,
    ends_word: bool,
}

/// Снимок списка на момент `start`: правки посреди диктовки её не меняют.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Dictionary {
    rules: Vec<Rule>,
}

impl std::fmt::Debug for Dictionary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dictionary")
            .field("rules", &self.rules.len())
            .finish()
    }
}

impl Dictionary {
    pub fn new(terms: &[Term]) -> Self {
        let mut rules: Vec<Rule> = terms.iter().filter_map(compile).collect();
        // Сортировка устойчива: при равной длине побеждает порядок владельца.
        rules.sort_by(|a, b| b.length.cmp(&a.length));
        Self { rules }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Один проход по тексту: вставленная замена дальше не просматривается.
    pub fn apply(&self, text: &str) -> String {
        if self.rules.is_empty() && text.is_ascii() {
            return text.to_owned();
        }
        let text: String = text.nfc().collect();
        let chars: Vec<char> = text.chars().collect();
        let folded: Vec<char> = chars.iter().copied().map(fold).collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < chars.len() {
            match self.matching(&chars, &folded, i) {
                Some((end, rule)) => {
                    out.push_str(&rule.replacement);
                    i = end;
                }
                None => {
                    out.push(chars[i]);
                    i += 1;
                }
            }
        }
        out
    }

    /// Пара «окончательный текст / хвост» после замены.
    pub fn apply_update(&self, committed: &str, tentative: &str) -> (String, String) {
        let committed = self.apply(committed);
        if tentative.is_empty() {
            return (committed, String::new());
        }
        if committed.is_empty() {
            return (committed, self.apply(tentative));
        }
        let joined = self.apply(&format!("{committed} {tentative}"));
        let prefix = format!("{committed} ");
        match joined.strip_prefix(&prefix) {
            Some(rest) => (committed, rest.to_owned()),
            None => (committed, self.apply(tentative)),
        }
    }

    fn matching(&self, chars: &[char], folded: &[char], start: usize) -> Option<(usize, &Rule)> {
        self.rules.iter().find_map(|rule| {
            if rule.starts_word && start > 0 && is_word(chars[start - 1]) {
                return None;
            }
            let mut at = start;
            for token in &rule.tokens {
                match token {
                    Token::Char(expected) => {
                        if folded.get(at) != Some(expected) {
                            return None;
                        }
                        at += 1;
                    }
                    Token::Space => {
                        if !chars.get(at).is_some_and(|c| c.is_whitespace()) {
                            return None;
                        }
                        while chars.get(at).is_some_and(|c| c.is_whitespace()) {
                            at += 1;
                        }
                    }
                }
            }
            if rule.ends_word && chars.get(at).is_some_and(|c| is_word(*c)) {
                return None;
            }
            Some((at, rule))
        })
    }
}

fn compile(term: &Term) -> Option<Rule> {
    let pattern: String = term.pattern.nfc().collect();
    let mut tokens = Vec::new();
    let mut length = 0;
    for c in pattern.chars() {
        if c.is_whitespace() {
            if !matches!(tokens.last(), Some(Token::Space)) {
                tokens.push(Token::Space);
            }
        } else {
            tokens.push(Token::Char(fold(c)));
            length += 1;
        }
    }
    while matches!(tokens.first(), Some(Token::Space)) {
        tokens.remove(0);
    }
    while matches!(tokens.last(), Some(Token::Space)) {
        tokens.pop();
    }
    if tokens.is_empty() {
        return None;
    }
    let starts_word = matches!(tokens.first(), Some(Token::Char(c)) if is_word(*c));
    let ends_word = matches!(tokens.last(), Some(Token::Char(c)) if is_word(*c));
    Some(Rule {
        tokens,
        length,
        replacement: term.replacement.clone(),
        starts_word,
        ends_word,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dictionary(pairs: &[(&str, &str)]) -> Dictionary {
        Dictionary::new(
            &pairs
                .iter()
                .map(|(pattern, replacement)| Term {
                    pattern: (*pattern).into(),
                    replacement: (*replacement).into(),
                })
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn multi_word_pattern_is_replaced() {
        let d = dictionary(&[("сессион спаун", "session_spawn")]);
        assert_eq!(
            d.apply("нужен сессион спаун срочно"),
            "нужен session_spawn срочно"
        );
    }

    #[test]
    fn whitespace_run_in_pattern_matches_any_run() {
        let d = dictionary(&[("сессион спаун", "session_spawn")]);
        assert_eq!(d.apply("сессион\t\n спаун"), "session_spawn");
    }

    #[test]
    fn case_and_yo_are_ignored() {
        let d = dictionary(&[("мейн", "main"), ("Ёлка", "tree")]);
        assert_eq!(d.apply("Мейн и МЕЙН и мейн"), "main и main и main");
        assert_eq!(d.apply("ёлка и ЕЛКА"), "tree и tree");
        // Текст после NFC: «и» + бревис — это «й».
        assert_eq!(d.apply("меи\u{0306}н"), "main");
    }

    #[test]
    fn word_boundaries_are_respected_for_cyrillic_and_latin() {
        let d = dictionary(&[("мейн", "main")]);
        assert_eq!(d.apply("мейн"), "main");
        assert_eq!(d.apply("мейн-фрейм"), "main-фрейм");
        assert_eq!(d.apply("подмейн"), "подмейн");
        assert_eq!(d.apply("mainмейн"), "mainмейн");
        assert_eq!(d.apply("мейн2"), "мейн2");
        assert_eq!(d.apply("2мейн"), "2мейн");
        assert_eq!(d.apply("_мейн_"), "_мейн_");
    }

    #[test]
    fn punctuation_adjacent_to_a_match_is_kept() {
        let d = dictionary(&[("мейн", "main")]);
        assert_eq!(d.apply("«мейн», (мейн)."), "«main», (main).");
        assert_eq!(d.apply("мейн!"), "main!");
    }

    #[test]
    fn longest_pattern_wins_and_the_list_breaks_ties() {
        let d = dictionary(&[("мейн", "main"), ("мейн ветка", "main_branch")]);
        assert_eq!(d.apply("мейн ветка"), "main_branch");
        assert_eq!(d.apply("мейн"), "main");
        let d = dictionary(&[("сессион", "session"), ("сессион спаун", "session_spawn")]);
        assert_eq!(d.apply("сессион спаун"), "session_spawn");
    }

    #[test]
    fn replacement_is_not_rescanned() {
        let d = dictionary(&[("раз", "два"), ("два", "три")]);
        assert_eq!(d.apply("раз и два"), "два и три");
    }

    #[test]
    fn empty_dictionary_keeps_text_but_normalises_it() {
        let d = dictionary(&[]);
        assert_eq!(d.apply("просто текст"), "просто текст");
        assert_eq!(d.apply("меи\u{0306}н"), "мейн");
    }

    #[test]
    fn committed_and_tentative_are_applied_separately() {
        let d = dictionary(&[("мейн", "main")]);
        let (committed, tentative) = d.apply_update("нужен мейн", "и ветка");
        assert_eq!(committed, "нужен main");
        assert_eq!(tentative, "и ветка");
    }

    #[test]
    fn pattern_spanning_the_boundary_waits_until_the_boundary_moves() {
        let d = dictionary(&[("сессион спаун", "session_spawn")]);
        let (committed, tentative) = d.apply_update("нужен сессион", "спаун");
        assert_eq!(committed, "нужен сессион");
        assert_eq!(tentative, "спаун");
        // Хвост не начинается с уже заменённого committed — он применяется сам.
        let d = dictionary(&[("мейн", "main"), ("мейн ветка", "main_branch")]);
        let (committed, tentative) = d.apply_update("мейн", "ветка");
        assert_eq!(committed, "main");
        assert_eq!(tentative, "ветка");
    }

    #[test]
    fn boundary_fallbacks_when_joined_does_not_start_with_committed() {
        let d = dictionary(&[("сессион", "session")]);
        let (committed, tentative) = d.apply_update("сессион", "дальше");
        assert_eq!(committed, "session");
        assert_eq!(tentative, "дальше");
        assert_eq!(d.apply_update("", "сессион"), ("".into(), "session".into()));
        assert_eq!(d.apply_update("мейн", ""), ("мейн".into(), "".into()));
    }

    #[test]
    fn validation_trims_and_keeps_order() {
        let terms = validate(vec![
            Term {
                pattern: "  мейн  ".into(),
                replacement: " main ".into(),
            },
            Term {
                pattern: "ветка".into(),
                replacement: "branch".into(),
            },
        ])
        .unwrap();
        assert_eq!(terms[0].pattern, "мейн");
        assert_eq!(terms[0].replacement, "main");
        assert_eq!(terms[1].pattern, "ветка");
    }

    #[test]
    fn validation_rejects_an_empty_side() {
        for (pattern, replacement) in [("   ", "main"), ("мейн", "\t")] {
            assert_eq!(
                validate(vec![Term {
                    pattern: pattern.into(),
                    replacement: replacement.into(),
                }])
                .unwrap_err(),
                TermError::Empty
            );
        }
    }

    #[test]
    fn validation_rejects_more_than_two_hundred_terms() {
        let terms = (0..=MAX_TERMS)
            .map(|i| Term {
                pattern: format!("слово{i}"),
                replacement: "x".into(),
            })
            .collect();
        assert_eq!(validate(terms).unwrap_err(), TermError::TooMany);
    }

    #[test]
    fn validation_rejects_long_sides_by_scalar_values() {
        let long: String = std::iter::repeat_n('я', MAX_TERM_CHARS + 1).collect();
        let ok: String = std::iter::repeat_n('я', MAX_TERM_CHARS).collect();
        assert_eq!(
            validate(vec![Term {
                pattern: long.clone(),
                replacement: "x".into(),
            }])
            .unwrap_err(),
            TermError::TooLong
        );
        assert_eq!(
            validate(vec![Term {
                pattern: "x".into(),
                replacement: long,
            }])
            .unwrap_err(),
            TermError::TooLong
        );
        assert!(
            validate(vec![Term {
                pattern: ok,
                replacement: "x".into(),
            }])
            .is_ok()
        );
    }

    #[test]
    fn validation_rejects_normalised_duplicates() {
        for (first, second) in [
            ("Мейн", "мейн"),
            ("Ёлка", "елка"),
            ("сессион  спаун", "сессион спаун"),
        ] {
            assert_eq!(
                validate(vec![
                    Term {
                        pattern: first.into(),
                        replacement: "a".into()
                    },
                    Term {
                        pattern: second.into(),
                        replacement: "b".into()
                    },
                ])
                .unwrap_err(),
                TermError::Duplicate,
                "{first} / {second}"
            );
        }
    }
}
