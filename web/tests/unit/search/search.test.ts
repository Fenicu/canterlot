import { afterEach, describe, expect, test } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import { ApiError } from '../../../src/lib/api/http';
import HitSnippet from '../../../src/lib/search/HitSnippet.svelte';
import {
	cleanSegments,
	emptyState,
	hitHref,
	originLabel,
	periodSince,
	readState,
	searchBody,
	totalLabel,
	type SearchBody,
	type SearchHit,
	type SearchPage
} from '../../../src/lib/search/model';
import { SearchStore } from '../../../src/lib/search/store.svelte';

const NOW = Date.parse('2026-11-08T15:30:00.000Z');

function hit(n: number, patch: Partial<SearchHit> = {}): SearchHit {
	return {
		conversation_id: '00000000-0000-4000-8000-00000000c001',
		conversation_title: 'Сессия',
		harness: 'claude_code',
		model: 'opus',
		archived: false,
		kind: 'answer',
		origin_kind: null,
		block_id: `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`,
		input_id: null,
		seq: n,
		jump_seq: n,
		created_at: new Date(NOW - n * 60_000).toISOString(),
		snippet: [{ text: `ответ ${n}`, hit: false }],
		...patch
	};
}

function pages(all: SearchHit[], size: number) {
	const calls: SearchBody[] = [];
	const fetch = async (body: SearchBody): Promise<SearchPage> => {
		calls.push(body);
		const offset = body.cursor ? Number(body.cursor) : 0;
		const next = offset + size < all.length ? String(offset + size) : null;
		return { hits: all.slice(offset, offset + size), next_cursor: next, total: all.length, total_capped: false };
	};
	return { calls, fetch };
}

afterEach(() => {
	document.body.textContent = '';
});

describe('search model', () => {
	test('period_starts_at_local_midnight_for_today_and_slides_for_week_and_month', () => {
		const midnight = new Date(NOW);
		midnight.setHours(0, 0, 0, 0);
		expect(periodSince('today', NOW)).toBe(midnight.toISOString());
		expect(periodSince('week', NOW)).toBe(new Date(NOW - 7 * 86_400_000).toISOString());
		expect(periodSince('month', NOW)).toBe(new Date(NOW - 30 * 86_400_000).toISOString());
		expect(periodSince('all', NOW)).toBeUndefined();
	});

	test('body_carries_filters_and_cursor_and_trims_the_query', () => {
		const body = searchBody({ q: '  отчёт плана ', period: 'week', kind: 'messages', session: { id: 'c1', title: 'Т' } }, NOW, 'next');
		expect(body).toEqual({
			q: 'отчёт плана',
			limit: 20,
			since: new Date(NOW - 7 * 86_400_000).toISOString(),
			kind: 'messages',
			conversation_id: 'c1',
			cursor: 'next'
		});
		expect(searchBody(emptyState(), NOW)).toEqual({ q: '', limit: 20 });
		expect(searchBody({ ...emptyState(), q: 'x'.repeat(300) }, NOW).q).toHaveLength(200);
	});

	test('labels_follow_origin_kind_and_totals_use_plurals_and_the_cap', () => {
		expect(originLabel({ kind: 'answer', origin_kind: null })).toBe('ответ');
		expect(originLabel({ kind: 'message', origin_kind: 'owner' })).toBe('моё сообщение');
		expect(originLabel({ kind: 'message', origin_kind: null })).toBe('моё сообщение');
		expect(originLabel({ kind: 'message', origin_kind: 'message' })).toBe('сообщение сессии');
		expect(originLabel({ kind: 'message', origin_kind: 'spawn' })).toBe('сообщение сессии');
		expect(originLabel({ kind: 'message', origin_kind: 'result' })).toBe('итог');
		expect(totalLabel(1, false)).toBe('Найдено 1 совпадение');
		expect(totalLabel(3, false)).toBe('Найдено 3 совпадения');
		expect(totalLabel(11, false)).toBe('Найдено 11 совпадений');
		expect(totalLabel(200, true)).toBe('Найдено больше 200');
	});

	test('segments_drop_sentinels_collapse_whitespace_and_merge_neighbours', () => {
		expect(
			cleanSegments([
				{ text: '  начало\n\n  ', hit: false },
				{ text: 'отчёт', hit: true },
				{ text: '', hit: false },
				{ text: 'а', hit: true },
				{ text: ' хвост  ', hit: false }
			])
		).toEqual([
			{ text: 'начало ', hit: false },
			{ text: 'отчёта', hit: true },
			{ text: ' хвост', hit: false }
		]);
	});

	test('hit_link_points_at_the_jump_place_and_never_carries_the_query', () => {
		const q = 'секретный запрос';
		const block = hitHref(hit(7, { jump_seq: 3 }));
		expect(block).toBe('/c/00000000-0000-4000-8000-00000000c001?block=00000000-0000-4000-8000-000000000007&seq=3');
		const input = hitHref(hit(8, { kind: 'message', block_id: null, input_id: 'in-1', origin_kind: 'owner' }));
		expect(input).toBe('/c/00000000-0000-4000-8000-00000000c001?input=in-1&seq=8');
		for (const href of [block, input]) {
			expect(new URL(href, 'https://hub.test').searchParams.has('q')).toBe(false);
			expect(decodeURIComponent(href)).not.toContain(q);
		}
	});

	test('history_state_is_read_defensively', () => {
		expect(readState(undefined)).toEqual(emptyState());
		expect(readState({ q: 'план', period: 'week', kind: 'answers', session: { id: 'c1', title: 'Т' } })).toEqual({
			q: 'план',
			period: 'week',
			kind: 'answers',
			session: { id: 'c1', title: 'Т' }
		});
		expect(readState({ q: 5, period: 'year', kind: 'tools', session: { title: 'нет id' } })).toEqual(emptyState());
	});
});

describe('search store', () => {
	test('empty_query_clears_without_a_request', async () => {
		const { calls, fetch } = pages([hit(1)], 20);
		const store = new SearchStore(fetch, emptyState(), () => NOW);
		await store.run();
		expect(calls).toHaveLength(0);
		expect(store.status).toBe('idle');
	});

	test('cursor_pages_append_until_the_cursor_ends', async () => {
		const all = Array.from({ length: 45 }, (_, index) => hit(index + 1));
		const { calls, fetch } = pages(all, 20);
		const store = new SearchStore(fetch, { ...emptyState(), q: 'ответ' }, () => NOW);
		await store.run();
		expect(store.status).toBe('ready');
		expect(store.hits).toHaveLength(20);
		expect(store.total).toBe(45);
		expect(store.hasMore).toBe(true);
		await Promise.all([store.more(), store.more()]);
		expect(calls.map((call) => call.cursor ?? null)).toEqual([null, '20']);
		expect(store.hits).toHaveLength(40);
		await store.more();
		expect(store.hits).toHaveLength(45);
		expect(store.hasMore).toBe(false);
		await store.more();
		expect(calls).toHaveLength(3);
		expect(new Set(store.hits.map((item) => item.block_id)).size).toBe(45);
	});

	test('a_newer_query_wins_over_a_slow_older_answer', async () => {
		const resolvers: ((page: SearchPage) => void)[] = [];
		const store = new SearchStore(
			(body) => new Promise<SearchPage>((resolve) => resolvers.push((page) => resolve({ ...page, hits: page.hits.map((item) => ({ ...item, conversation_title: body.q })) }))),
			{ ...emptyState(), q: 'первый' },
			() => NOW
		);
		const first = store.run();
		store.q = 'второй';
		const second = store.run();
		resolvers[1]!({ hits: [hit(2)], next_cursor: null, total: 1, total_capped: false });
		await second;
		resolvers[0]!({ hits: [hit(1)], next_cursor: null, total: 1, total_capped: false });
		await first;
		expect(store.hits.map((item) => item.conversation_title)).toEqual(['второй']);
	});

	test('hub_timeout_is_its_own_state_and_other_failures_keep_the_message', async () => {
		const timeout = new SearchStore(
			async () => {
				throw new ApiError(503, 'internal', 'timeout', 'browser', 'never', '00000000-0000-4000-8000-0000000000e5');
			},
			{ ...emptyState(), q: 'план' },
			() => NOW
		);
		await timeout.run();
		expect(timeout.status).toBe('error');
		expect(timeout.failure?.kind).toBe('timeout');
		expect(timeout.failure?.message).toContain('Уточните запрос');

		const invalid = new SearchStore(
			async () => {
				throw new ApiError(400, 'invalid_request', 'bad', 'browser', 'never', '00000000-0000-4000-8000-0000000000e6');
			},
			{ ...emptyState(), q: 'план' },
			() => NOW
		);
		await invalid.run();
		expect(invalid.failure).toEqual({ kind: 'failed', message: 'Некорректный запрос' });
	});

	test('a_failed_next_page_keeps_the_hits_and_can_retry', async () => {
		let fail = false;
		const all = Array.from({ length: 30 }, (_, index) => hit(index + 1));
		const { fetch } = pages(all, 20);
		const store = new SearchStore(
			async (body) => {
				if (fail) throw new Error('сеть');
				return fetch(body);
			},
			{ ...emptyState(), q: 'ответ' },
			() => NOW
		);
		await store.run();
		fail = true;
		await store.more();
		expect(store.hits).toHaveLength(20);
		expect(store.moreFailure?.message).toBe('сеть');
		fail = false;
		await store.more();
		expect(store.hits).toHaveLength(30);
		expect(store.moreFailure).toBeNull();
	});
});

describe('snippet rendering', () => {
	test('hits_render_as_mark_and_text_is_never_html', () => {
		const target = document.createElement('div');
		document.body.append(target);
		const app = mount(HitSnippet, {
			target,
			props: {
				segments: [
					{ text: 'перед <b>жирным</b> ', hit: false },
					{ text: 'отчёт', hit: true },
					{ text: ' после ', hit: false },
					{ text: 'плана', hit: true }
				]
			}
		});
		flushSync();
		const node = target.querySelector('[data-search-snippet]')!;
		expect([...node.querySelectorAll('mark')].map((mark) => mark.textContent)).toEqual(['отчёт', 'плана']);
		expect(node.querySelector('b')).toBeNull();
		expect(node.textContent).toBe('перед <b>жирным</b> отчёт после плана');
		unmount(app);
	});
});
