import { fmt } from '#lib/format/ru.js';
import { modelLabel } from '#lib/format/labels.js';

/** Расход одной модели в ходу: форма `ModelUsageView` хаба. */
export interface CostUsage {
	model: string;
	agent?: string;
	input_tokens: number;
	output_tokens: number;
	cache_read_tokens: number;
	cache_write_tokens: number;
	/** Микродолларов; пусто — цена неизвестна, а не ноль. */
	cost_micro_usd?: number | null;
	cost_source?: string | null;
}

const UNKNOWN = '≈ $ —';

/** Сумма в микродолларах по-русски: «≈ $0,62», «< $0,01», «бесплатно», неизвестная — «≈ $ —». */
export function costMoney(micro: number | null | undefined): string {
	if (micro == null) return UNKNOWN;
	if (micro === 0) return 'бесплатно';
	if (micro < 10_000) return '< $0,01';
	const cents = Math.round(micro / 10_000);
	const dollars = Math.floor(cents / 100);
	const rest = String(cents % 100).padStart(2, '0');
	return `≈ $${fmt.number(dollars)},${rest}`;
}

/** Доля кеша во входе, целые проценты; без входа её нет. */
export function cacheShare(usage: Pick<CostUsage, 'input_tokens' | 'cache_read_tokens' | 'cache_write_tokens'>): number | null {
	const input = usage.input_tokens + usage.cache_read_tokens + usage.cache_write_tokens;
	return input > 0 ? Math.round((usage.cache_read_tokens / input) * 100) : null;
}

/** Доля недельного лимита: общая квота, поэтому всегда с «≈». Нулевой сдвиг не показывается. */
export function weekShare(permille: number | null | undefined): string | null {
	if (permille == null || permille <= 0) return null;
	const percent = permille / 10;
	const text = Number.isInteger(percent) ? String(percent) : percent.toFixed(1).replace('.', ',');
	return `≈ ${text}% недели`;
}

const MAIN_AGENTS = new Set(['orchestrator', 'main']);

interface Merged extends CostUsage {
	main: boolean;
	cost_micro_usd: number | null;
}

/** Одна модель — одна строка, даже если её звали несколько агентов; одна неизвестная цена делает неизвестной сумму. */
function mergeByModel(usage: CostUsage[]): Merged[] {
	const byModel = new Map<string, Merged>();
	for (const line of usage) {
		const known = byModel.get(line.model);
		const cost = line.cost_micro_usd ?? null;
		if (!known) {
			byModel.set(line.model, { ...line, main: MAIN_AGENTS.has(line.agent ?? ''), cost_micro_usd: cost });
			continue;
		}
		known.input_tokens += line.input_tokens;
		known.output_tokens += line.output_tokens;
		known.cache_read_tokens += line.cache_read_tokens;
		known.cache_write_tokens += line.cache_write_tokens;
		known.cost_micro_usd = known.cost_micro_usd == null || cost == null ? null : known.cost_micro_usd + cost;
		known.main ||= MAIN_AGENTS.has(line.agent ?? '');
	}
	return [...byModel.values()];
}

const total = (line: CostUsage): number => line.input_tokens + line.output_tokens + line.cache_read_tokens + line.cache_write_tokens;

function tokensText(n: number): string {
	return n < 1000 ? `${n} ${fmt.plural(n, ['токен', 'токена', 'токенов'])}` : `${fmt.tokensShort(n)} токенов`;
}

/**
 * Строка под ответом: «Opus 5.5 · 48 тыс. токенов (кеш 96%) · ≈ $0,62 · ≈ 1,4% недели».
 * Главная модель идёт первой своими числами, остальные — «+ Haiku 5.5 ≈ $0,01» или счётом.
 */
export function costLine(usage: CostUsage[], limit: { weekly_delta_permille: number } | null | undefined): string {
	const models = mergeByModel(usage).filter((line) => total(line) > 0 || line.cost_micro_usd != null);
	if (models.length === 0) return '';
	models.sort((a, b) => Number(b.main) - Number(a.main) || total(b) - total(a));
	const [main, ...extra] = models;
	if (!main) return '';
	const cache = cacheShare(main);
	const tokens = cache == null ? tokensText(total(main)) : `${tokensText(total(main))} (кеш ${cache}%)`;
	let money = costMoney(main.cost_micro_usd);
	if (extra.length === 1 && extra[0]) {
		money += ` + ${modelLabel(extra[0].model)} ${costMoney(extra[0].cost_micro_usd)}`;
	} else if (extra.length > 1) {
		const unknown = extra.some((line) => line.cost_micro_usd == null);
		const sum = extra.reduce((acc, line) => acc + (line.cost_micro_usd ?? 0), 0);
		money += ` + ${extra.length} ${fmt.plural(extra.length, ['модель', 'модели', 'моделей'])} ${unknown ? UNKNOWN : costMoney(sum)}`;
	}
	return [modelLabel(main.model), tokens, money, weekShare(limit?.weekly_delta_permille)].filter((part): part is string => !!part).join(' · ');
}

interface CostTotals {
	cost_micro_usd: number;
	unknown_cost_turns: number;
}

/** Итог панели сведений: деньги за сессию с делегатами и число ходов, которым цену назвать нечем. */
export function sessionCostLine(session: CostTotals, delegates: CostTotals): { money: string; unknown: string | null } {
	const micro = session.cost_micro_usd + delegates.cost_micro_usd;
	const unknown = session.unknown_cost_turns + delegates.unknown_cost_turns;
	return {
		money: `${costMoney(micro)} за сессию с делегатами`,
		unknown: unknown > 0 ? `${unknown} ${fmt.plural(unknown, ['ход', 'хода', 'ходов'])} без цены` : null
	};
}
