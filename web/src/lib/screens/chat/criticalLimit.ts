import { limitView, type LimitThresholds, type MeterView, type ProviderLimits } from '#lib/stores/limits.js';

/** Строка шапки чата о лимите провайдера сессии, который вот-вот кончится. */
export interface CriticalLimit {
	/** «Лимит 5 ч: осталось 3% · сброс через 1 ч 10 мин». */
	text: string;
}

const WINDOW_NAME: Record<'five' | 'week' | 'fable' | 'month', string> = {
	five: 'Лимит 5 ч',
	week: 'Недельный лимит',
	fable: 'Лимит Fable',
	month: 'Месячный лимит'
};

/**
 * Окно провайдера сессии на тревожном пороге (`danger_remaining_pct`): то, у которого
 * остаток меньше всего. Жёлтый порог и чужие провайдеры в чате не показываются.
 */
export function criticalLimit(
	providers: readonly ProviderLimits[],
	provider: string,
	now: number,
	cfg: LimitThresholds
): CriticalLimit | null {
	let worst: { name: string; meter: MeterView } | null = null;
	for (const entry of providers) {
		if (entry.provider !== provider) continue;
		const view = limitView(entry, now, cfg);
		const windows = [
			['five', view.five],
			['week', view.week],
			['fable', view.fable],
			['month', view.month]
		] as const;
		for (const [key, meter] of windows) {
			if (!meter || meter.tone !== 'danger') continue;
			if (!worst || meter.fill < worst.meter.fill) worst = { name: WINDOW_NAME[key], meter };
		}
	}
	if (!worst) return null;
	const until = worst.meter.reset;
	// «через 1 ч 10 мин»; последняя минута приходит без «через».
	const reset = !until ? '' : until.startsWith('через') ? ` · сброс ${until}` : ' · сброс в течение минуты';
	return { text: `${worst.name}: осталось ${worst.meter.text}${reset}` };
}
