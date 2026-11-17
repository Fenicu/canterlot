/** Подпись сессии для вкладки: тот же текст, что в шапке чата, но короткий. */
export const TAB_TITLE_FALLBACK = 'Сессия';
export const TAB_TITLE_LIMIT = 60;

/**
 * Обрезает название сессии до вкладки: многоточие входит в предел, поэтому
 * длинная подпись не вытесняет из заголовка « · Canterlot».
 */
export function trimTabTitle(title?: string | null): string {
	const text = (title ?? '').trim() || TAB_TITLE_FALLBACK;
	const chars = Array.from(text);
	if (chars.length <= TAB_TITLE_LIMIT) return text;
	return `${chars.slice(0, TAB_TITLE_LIMIT - 1).join('').trimEnd()}…`;
}

export function sessionTabTitle(title?: string | null): string {
	return `${trimTabTitle(title)} · Canterlot`;
}

export function laneTabTitle(title?: string | null): string {
	return `Субагент · ${trimTabTitle(title)} · Canterlot`;
}
