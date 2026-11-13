/** Словарь замен диктовки: пары «шаблон → замена», частые пары и проверка перед сохранением. */
import type { components } from "#lib/api/generated/openapi.js";

export type DictationTerm = components["schemas"]["Term"];

export const TERM_LIMIT = 200;
export const TERM_MAX_LENGTH = 100;

export type TermErrorCode = "too_many_terms" | "term_empty" | "term_too_long" | "duplicate_term";

const TERM_ERRORS: Record<TermErrorCode, string> = {
  too_many_terms: `Не больше ${TERM_LIMIT} пар`,
  term_empty: "Заполните шаблон и замену",
  term_too_long: `Шаблон и замена не длиннее ${TERM_MAX_LENGTH} символов`,
  duplicate_term: "Такой шаблон уже есть",
};

export function termErrorText(code: string): string | null {
  return code in TERM_ERRORS ? TERM_ERRORS[code as TermErrorCode] : null;
}

/** Общие пары для кнопки «Добавить частые»: правятся в списке, а не зашиты в распознавание. */
export const FREQUENT_TERMS: DictationTerm[] = [
  { pattern: "мейн", replacement: "main" },
  { pattern: "жесон", replacement: "JSON" },
  { pattern: "джейсон", replacement: "JSON" },
  { pattern: "апи", replacement: "API" },
  { pattern: "плейврайт", replacement: "Playwright" },
  { pattern: "плейрайт", replacement: "Playwright" },
  { pattern: "дипсик", replacement: "DeepSeek" },
  { pattern: "опус", replacement: "Opus" },
  { pattern: "некстест", replacement: "nextest" },
  { pattern: "гит", replacement: "git" },
  { pattern: "гитхаб", replacement: "GitHub" },
  { pattern: "докер", replacement: "Docker" },
  { pattern: "раст", replacement: "Rust" },
  { pattern: "свелт", replacement: "Svelte" },
];

/** Ключ шаблона так, как его сравнивает хаб: регистр, ё/е и лишние пробелы не важны. */
export function patternKey(pattern: string): string {
  return pattern.trim().replace(/\s+/g, " ").toLowerCase().replace(/ё/g, "е");
}

/** Первая ошибка списка или null: тем же порядком проверяет хаб. */
export function validateTerms(terms: DictationTerm[]): TermErrorCode | null {
  if (terms.length > TERM_LIMIT) return "too_many_terms";
  const seen = new Set<string>();
  for (const term of terms) {
    const pattern = term.pattern.trim();
    const replacement = term.replacement.trim();
    if (!pattern || !replacement) return "term_empty";
    if (pattern.length > TERM_MAX_LENGTH || replacement.length > TERM_MAX_LENGTH) return "term_too_long";
    const key = patternKey(pattern);
    if (seen.has(key)) return "duplicate_term";
    seen.add(key);
  }
  return null;
}

export function missingFrequent(terms: DictationTerm[]): { term: DictationTerm; missing: boolean }[] {
  const seen = new Set(terms.map((term) => patternKey(term.pattern)));
  return FREQUENT_TERMS.map((term) => ({ term, missing: !seen.has(patternKey(term.pattern)) }));
}

export type AddTermResult = { ok: true; terms: DictationTerm[] } | { ok: false; error: string };

export function addTerm(terms: DictationTerm[], term: DictationTerm): AddTermResult {
  if (terms.some((item) => patternKey(item.pattern) === patternKey(term.pattern))) {
    return { ok: false, error: TERM_ERRORS.duplicate_term };
  }
  return { ok: true, terms: [...terms, term] };
}
