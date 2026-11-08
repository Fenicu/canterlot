import type { components } from '#lib/api/generated/openapi.js';
import { fmt } from '#lib/format/ru.js';
import { harnessLabel } from '#lib/format/labels.js';

export type SearchBody = components['schemas']['SearchBody'];
export type SearchPage = components['schemas']['SearchPage'];
export type SearchHit = components['schemas']['SearchHit'];
export type SearchSegment = components['schemas']['SearchSegment'];

export type Period = 'today' | 'week' | 'month' | 'all';
export type KindFilter = 'answers' | 'messages' | 'all';

/** Сессия, которой ограничен поиск: «Искать в этой сессии». */
export interface SessionScope {
	id: string;
	title: string;
}

/**
 * Состояние поиска, которое переживает переход к совпадению и назад.
 * Живёт в `history.state` или в памяти вкладки: ни в адресе, ни в localStorage.
 */
export interface SearchState {
	q: string;
	period: Period;
	kind: KindFilter;
	session: SessionScope | null;
}

export const MAX_QUERY = 200;
export const PAGE_SIZE = 20;

export const PERIODS: { value: Period; label: string }[] = [
	{ value: 'today', label: 'Сегодня' },
	{ value: 'week', label: 'Неделя' },
	{ value: 'month', label: 'Месяц' },
	{ value: 'all', label: 'Всё' }
];

export const KINDS: { value: KindFilter; label: string }[] = [
	{ value: 'answers', label: 'Ответы' },
	{ value: 'messages', label: 'Сообщения' },
	{ value: 'all', label: 'Всё' }
];

export function emptyState(session: SessionScope | null = null): SearchState {
	return { q: '', period: 'all', kind: 'all', session };
}

const DAY = 86_400_000;

/** Начало периода: сегодня с местной полуночи, неделя и месяц скользящие. */
export function periodSince(period: Period, now: number): string | undefined {
	if (period === 'all') return undefined;
	if (period === 'today') {
		const midnight = new Date(now);
		midnight.setHours(0, 0, 0, 0);
		return midnight.toISOString();
	}
	return new Date(now - (period === 'week' ? 7 : 30) * DAY).toISOString();
}

export function searchBody(state: SearchState, now: number, cursor?: string | null, limit = PAGE_SIZE): SearchBody {
	const body: SearchBody = { q: state.q.trim().slice(0, MAX_QUERY), limit };
	const since = periodSince(state.period, now);
	if (since) body.since = since;
	if (state.kind !== 'all') body.kind = state.kind;
	if (state.session) body.conversation_id = state.session.id;
	if (cursor) body.cursor = cursor;
	return body;
}

/** Подпись совпадения по происхождению: чей это текст. */
export function originLabel(hit: Pick<SearchHit, 'kind' | 'origin_kind'>): string {
	if (hit.kind === 'answer') return 'ответ';
	switch (hit.origin_kind) {
		case 'result':
			return 'итог';
		case 'message':
		case 'spawn':
			return 'сообщение сессии';
		default:
			return 'моё сообщение';
	}
}

export function totalLabel(total: number, capped: boolean): string {
	if (capped) return `Найдено больше ${total}`;
	return `Найдено ${total} ${fmt.plural(total, ['совпадение', 'совпадения', 'совпадений'])}`;
}

const SENTINELS = /[]/g;

/** Сниппет без пустых кусков и с одиночными пробелами: хаб уже убрал разметку, клиент только рисует. */
export function cleanSegments(segments: SearchSegment[]): SearchSegment[] {
	const out: SearchSegment[] = [];
	for (const segment of segments) {
		const text = segment.text.replace(SENTINELS, '').replace(/\s+/g, ' ');
		if (!text) continue;
		const last = out.at(-1);
		if (last && last.hit === segment.hit) last.text += text;
		else out.push({ text, hit: segment.hit });
	}
	if (out[0]) out[0].text = out[0].text.trimStart();
	const tail = out.at(-1);
	if (tail) tail.text = tail.text.trimEnd();
	return out.filter((segment) => segment.text.length > 0);
}

/**
 * Адрес совпадения: разговор и место в ленте. Запроса в адресе нет никогда —
 * перезагрузка или общий доступ к ссылке не должны унести его в журналы прокси.
 */
export function hitHref(hit: Pick<SearchHit, 'conversation_id' | 'block_id' | 'input_id' | 'jump_seq'>): string {
	const params = new URLSearchParams();
	if (hit.block_id) params.set('block', hit.block_id);
	else if (hit.input_id) params.set('input', hit.input_id);
	params.set('seq', String(hit.jump_seq));
	return `/c/${encodeURIComponent(hit.conversation_id)}?${params.toString()}`;
}

function isPeriod(value: unknown): value is Period {
	return value === 'today' || value === 'week' || value === 'month' || value === 'all';
}

function isKind(value: unknown): value is KindFilter {
	return value === 'answers' || value === 'messages' || value === 'all';
}

/** Состояние из записи истории: чужая или старая форма даёт пустой поиск, а не ошибку. */
export function readState(value: unknown): SearchState {
	if (!value || typeof value !== 'object') return emptyState();
	const raw = value as Record<string, unknown>;
	const session = raw.session as Record<string, unknown> | null | undefined;
	return {
		q: typeof raw.q === 'string' ? raw.q.slice(0, MAX_QUERY) : '',
		period: isPeriod(raw.period) ? raw.period : 'all',
		kind: isKind(raw.kind) ? raw.kind : 'all',
		session:
			session && typeof session.id === 'string'
				? { id: session.id, title: typeof session.title === 'string' ? session.title : '' }
				: null
	};
}

/** Хаб отдаёт идентификатор инструмента (`claude_code`): владельцу показывается его название. */
export function hitHarness(raw: string): string {
	return harnessLabel({ title: raw, provider: raw.split(/[_\-\s]/)[0] });
}
