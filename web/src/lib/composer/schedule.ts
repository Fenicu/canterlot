/**
 * Быстрые варианты отправки позже.
 * Только точные сдвиги: «Через 15 минут» и «Через час».
 * Время на кнопке считает экран от server-corrected now в местной зоне.
 */
export function presets(now: number): { label: string; at: number }[] {
	return [
		{ label: 'Через 15 минут', at: now + 15 * 60 * 1000 },
		{ label: 'Через час', at: now + 60 * 60 * 1000 }
	];
}

/**
 * Валидация окна отложенного сообщения по правилам хаба:
 * now + 1 с <= run_at <= now + 366 дней.
 */
export function validateScheduleTime(at: number, now: number): 'ok' | 'too_soon' | 'too_far' {
	if (!Number.isFinite(at) || at < now + 1_000) {
		return 'too_soon';
	}
	const maxAllowed = now + 366 * 86_400_000;
	if (at > maxAllowed) {
		return 'too_far';
	}
	return 'ok';
}
