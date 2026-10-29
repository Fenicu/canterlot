import { fmt } from '$lib/format/ru';
import { limitView, type LimitThresholds, type ProviderLimits } from '$lib/stores/limits';

export interface LimitStrip {
	summary: string;
	details: string;
	tone: 'default' | 'warning' | 'reserve';
	staleNote: string;
	crunchLabel: string | null;
}

export function limitStrip(
	providers: ProviderLimits[],
	now: number,
	cfg: LimitThresholds,
	opts: { prefer?: string; crunch?: boolean } = {}
): LimitStrip {
	const ordered = [...providers].sort((a, b) => rank(a.provider, opts.prefer) - rank(b.provider, opts.prefer));
	const views = ordered.map((provider) => ({ provider, view: limitView(provider, now, cfg) }));
	const summary = views
		.map(({ provider, view }) => `${label(provider.provider)} 5ч ${view.fiveHour} · неделя ${view.weekly}`)
		.join('  ');
	const details = views
		.map(({ provider }) => {
			const five = provider.five_hour?.resets_at ? fmt.clock(Date.parse(provider.five_hour.resets_at)) : '';
			const weekAt = provider.weekly?.resets_at ? Date.parse(provider.weekly.resets_at) : Number.NaN;
			const week = Number.isFinite(weekAt) ? `${weekday(weekAt)}, ${fmt.clock(weekAt)}` : '';
			return `${label(provider.provider)}: 5ч до ${five} · неделя до ${week}`;
		})
		.join('\n');
	let tone: LimitStrip['tone'] = 'default';
	if (views.some((item) => item.view.tone === 'reserve')) tone = 'reserve';
	else if (views.some((item) => item.view.tone === 'warning')) tone = 'warning';
	const staleAt = views
		.map((item) => observed(item.provider))
		.filter((stamp) => Number.isFinite(stamp) && now - stamp > cfg.stale_after_secs * 1000)
		.sort((a, b) => a - b)[0];
	const reserve = views.some((item) => item.provider.weekly_reserve_active);
	return {
		summary,
		details,
		tone,
		staleNote: staleAt !== undefined ? `данные от ${fmt.clock(staleAt)}` : '',
		crunchLabel: opts.crunch && reserve ? 'Кранч · недельный резерв' : null
	};
}

function rank(provider: string, prefer?: string): number {
	if (prefer && provider === prefer) return 0;
	if (provider === 'claude') return 1;
	if (provider === 'codex') return 2;
	return 3;
}

function label(provider: string): string {
	if (provider === 'claude') return 'Claude';
	if (provider === 'codex') return 'Codex';
	return provider;
}

function weekday(ts: number): string {
	return new Intl.DateTimeFormat('ru-RU', { weekday: 'short' }).format(ts).replace('.', '').toLocaleLowerCase('ru-RU');
}

function observed(provider: ProviderLimits): number {
	const stamps = [provider.five_hour?.observed_at, provider.weekly?.observed_at]
		.map((value) => (value ? Date.parse(value) : Number.NaN))
		.filter((value) => Number.isFinite(value));
	return stamps.length ? Math.min(...stamps) : Number.NaN;
}
