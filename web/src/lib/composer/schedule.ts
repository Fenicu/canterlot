/**
 * Быстрые варианты отправки позже.
 * «Через 15 минут», «Через час», «Сегодня вечером», «Завтра утром».
 */
export function presets(now: number): { label: string; at: number }[] {
	const min15 = now + 15 * 60 * 1000;
	const hour1 = now + 60 * 60 * 1000;

	// «Сегодня вечером»: 18:00 местного времени. Если 18:00 уже прошло, переносится на завтра 18:00
	const evening = new Date(now);
	evening.setHours(18, 0, 0, 0);
	if (evening.getTime() <= now) {
		evening.setDate(evening.getDate() + 1);
	}

	// «Завтра утром»: 09:00 местного времени следующего дня
	const morning = new Date(now);
	morning.setDate(morning.getDate() + 1);
	morning.setHours(9, 0, 0, 0);

	return [
		{ label: 'Через 15 минут', at: min15 },
		{ label: 'Через час', at: hour1 },
		{ label: 'Сегодня вечером', at: evening.getTime() },
		{ label: 'Завтра утром', at: morning.getTime() }
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
