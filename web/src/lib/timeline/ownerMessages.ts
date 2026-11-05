import { originLabel } from '#lib/timeline/origin.js';
import type { Row } from '#lib/timeline/rows.js';
import type { InputOrigin } from '#lib/stores/inputs.js';

/** Первая непустая строка сообщения без ведущих и хвостовых пробелов. */
export function firstLine(text: string): string {
	for (const line of text.split(/\r?\n/)) {
		const trimmed = line.trim();
		if (trimmed) return trimmed;
	}
	return '';
}

/** Строка списка для сообщения без текста: вложение говорит само за себя. */
export function messagePreview(text: string, attachments = 0): string {
	const line = firstLine(text);
	if (line) return line;
	return attachments > 0 ? 'Вложение' : 'Без текста';
}

/** Страницы хаба приходят от новых к старым: в списке порядок всегда хронологический. */
export function sortOwnerMessages<T extends { seq: string }>(items: T[]): T[] {
	return [...items].sort((a, b) => {
		const left = BigInt(a.seq);
		const right = BigInt(b.seq);
		return left < right ? -1 : left > right ? 1 : 0;
	});
}

/**
 * Подпись происхождения в списке: служебные вводы названы так же тихо,
 * как их называет лента, но будильник не притворяется чужой сессией.
 */
export function ownerMessageOrigin(origin: InputOrigin | null | undefined): string | null {
	if (!origin) return null;
	if (origin.kind === 'alarm') return 'Будильник';
	if (origin.kind === 'spawn') return 'Запуск сессии';
	return originLabel(origin);
}

/** Ряд ленты, в котором стоит это сообщение: по нему считается место прокрутки. */
export function rowIndexOfInput(rows: Row[], inputId: string): number {
	return rows.findIndex((row) =>
		row.entries.some((entry) => entry.kind === 'input' && entry.input.id === inputId)
	);
}
