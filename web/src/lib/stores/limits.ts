import { fmt } from '$lib/format/ru';

export interface LimitWindow {
	used_permille: number;
	resets_at: string | null;
	observed_at: string;
}

export interface ProviderLimits {
	provider: string;
	five_hour?: LimitWindow | null;
	weekly?: LimitWindow | null;
	weekly_reserve_active: boolean;
	weekly_reserve_resets_at?: string | null;
}

export interface LimitThresholds {
	stale_after_secs: number;
	warn_pcts: number[];
	weekly_reserve_pct: number;
}

export interface LimitView {
	fiveHour: string;
	weekly: string;
	tone: 'default' | 'warning' | 'reserve';
	resets: { fiveHour: string; weekly: string };
	stale: boolean;
}

function windowText(window: LimitWindow | null | undefined, now: number): { pct: string; reset: string; stale: boolean; hot: boolean } {
	if (!window) return { pct: '0%', reset: '', stale: false, hot: false };
	const observed = Date.parse(window.observed_at);
	return {
		pct: fmt.pct(window.used_permille),
		reset: window.resets_at ? fmt.resetAt(Date.parse(window.resets_at), now) : '',
		stale: Number.isNaN(observed) ? true : now - observed < 0 ? false : true,
		hot: window.used_permille / 10 >= 0
	};
}

export function limitView(provider: ProviderLimits, now: number, cfg: LimitThresholds): LimitView {
	const five = windowText(provider.five_hour, now);
	const week = windowText(provider.weekly, now);
	const warn = Math.min(...cfg.warn_pcts);
	const hot = (value: LimitWindow | null | undefined): boolean =>
		!!value && value.used_permille / 10 >= warn;
	const staleMs = cfg.stale_after_secs * 1000;
	const age = (window: LimitWindow | null | undefined): boolean => {
		if (!window) return false;
		const observed = Date.parse(window.observed_at);
		return Number.isNaN(observed) || now - observed > staleMs;
	};
	let tone: LimitView['tone'] = 'default';
	if (provider.weekly_reserve_active) tone = 'reserve';
	else if (hot(provider.five_hour) || hot(provider.weekly)) tone = 'warning';
	return {
		fiveHour: five.pct,
		weekly: week.pct,
		tone,
		resets: { fiveHour: five.reset, weekly: week.reset },
		stale: age(provider.five_hour) || age(provider.weekly)
	};
}
