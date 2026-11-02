import { fmt } from '#lib/format/ru.js';

export interface CompactionFacts {
	trigger: string;
	ok?: boolean | null;
	pre_tokens?: number | null;
}

export const CONTEXT_WARN_PCT = 70;
export const CONTEXT_DANGER_PCT = 90;

export type ContextTone = 'accent' | 'warning' | 'danger';

/** Занятость контекста в процентах; `null` — окно неизвестно, процента нет. */
export function contextPercent(tokens: number, window: number | null | undefined): number | null {
	if (!window || window <= 0 || tokens < 0) return null;
	return Math.floor((tokens / window) * 100);
}

export function contextTone(percent: number | null): ContextTone {
	if (percent !== null && percent >= CONTEXT_DANGER_PCT) return 'danger';
	if (percent !== null && percent >= CONTEXT_WARN_PCT) return 'warning';
	return 'accent';
}

/** Короткая подпись индикатора: «Контекст 64%», без окна — токены. */
export function contextBadge(tokens: number, window: number | null | undefined): string {
	const percent = contextPercent(tokens, window);
	if (percent === null) return `Контекст ${fmt.tokensShort(tokens)}`;
	return `Контекст ${percent}%`;
}

/** Раскрытие по нажатию: «640 тыс. из 1 млн токенов», без окна — только токены. */
export function contextDetails(tokens: number, window: number | null | undefined): string {
	const used = fmt.tokensShort(tokens);
	if (!window || window <= 0) return `${used} токенов`;
	return `${used} из ${fmt.tokensShort(window)} токенов`;
}

/** Тихая строка finished-эпизода; цифр нет, если CLI их не дал. */
export function compactionLine(facts: CompactionFacts): string {
	if (facts.ok === false) return 'Сжатие контекста не удалось';
	const scope = facts.trigger === 'manual' ? 'вручную' : 'автоматически';
	const tokens =
		facts.pre_tokens !== null && facts.pre_tokens !== undefined && facts.pre_tokens > 0
			? `, было ${fmt.tokensShort(facts.pre_tokens)} токенов`
			: '';
	return `Контекст сжат ${scope}${tokens}`;
}

/** Строка идущего сжатия. */
export function compactionRunningLine(): string {
	return 'Сжимает контекст';
}
