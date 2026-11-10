import { describe, expect, test } from 'vitest';
import {
	BookmarksListStore,
	ConversationBookmarks,
	type BookmarkApi,
	type BookmarksListApi
} from '../../../src/lib/bookmarks/store.svelte';
import type { BookmarkChange, BookmarkView } from '../../../src/lib/bookmarks/model';

const CONVERSATION = '00000000-0000-4000-8000-0000000000c1';

function entry(n: number, patch: Partial<BookmarkView> = {}): BookmarkView {
	return {
		id: `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`,
		conversation_id: CONVERSATION,
		conversation_title: 'Миграция поиска',
		target_kind: 'block',
		target_id: `00000000-0000-4000-8000-${String(100 + n).padStart(12, '0')}`,
		created_at: new Date(2026, 10, 8, 10, n).toISOString(),
		target_created_at: new Date(2026, 10, 7, 10, n).toISOString(),
		kind: 'answer',
		origin_kind: null,
		block_id: `00000000-0000-4000-8000-${String(100 + n).padStart(12, '0')}`,
		input_id: null,
		seq: n,
		jump_seq: n,
		harness: 'claude_code',
		model: 'opus-5.5',
		archived: false,
		role: 'agent',
		agent: 'orchestrator',
		excerpt: `ответ ${n}`,
		...patch
	};
}

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (reason: unknown) => void;
	const promise = new Promise<T>((ok, fail) => {
		resolve = ok;
		reject = fail;
	});
	return { promise, resolve, reject };
}

function fakeApi(patch: Partial<BookmarkApi> = {}): BookmarkApi {
	return {
		listConversation: async () => [],
		put: async (_conversation, _kind, targetId) => entry(1, { target_id: targetId }),
		remove: async () => undefined,
		...patch
	};
}

describe('conversation bookmarks', () => {
	test('load_fills_the_targets_and_the_timeline_order', async () => {
		const model = new ConversationBookmarks(
			CONVERSATION,
			fakeApi({ listConversation: async () => [entry(4), entry(2), entry(3)] })
		);
		await model.load();
		expect(model.loaded).toBe(true);
		expect(model.count).toBe(3);
		expect(model.list.map((item) => item.seq)).toEqual([2, 3, 4]);
		expect(model.has('block', entry(4).target_id)).toBe(true);
		expect(model.has('input', entry(4).target_id)).toBe(false);
	});

	test('add_flips_at_once_and_keeps_the_server_entry', async () => {
		const pending = deferred<BookmarkView>();
		const model = new ConversationBookmarks(CONVERSATION, fakeApi({ put: () => pending.promise }));
		const done = model.toggle('block', entry(5).target_id);
		expect(model.has('block', entry(5).target_id)).toBe(true);
		expect(model.count).toBe(1);
		pending.resolve(entry(5));
		await done;
		expect(model.count).toBe(1);
		expect(model.list[0]?.excerpt).toBe('ответ 5');
	});

	test('a_failed_add_flips_back_and_names_the_error', async () => {
		const model = new ConversationBookmarks(
			CONVERSATION,
			fakeApi({
				put: async () => {
					throw new Error('Сбой закладки');
				}
			})
		);
		await model.toggle('block', entry(5).target_id);
		expect(model.has('block', entry(5).target_id)).toBe(false);
		expect(model.count).toBe(0);
		expect(model.toggleError).toBe('Сбой закладки');
	});

	test('a_failed_remove_puts_the_row_back', async () => {
		const model = new ConversationBookmarks(
			CONVERSATION,
			fakeApi({
				listConversation: async () => [entry(5)],
				remove: async () => {
					throw new Error('Сбой удаления');
				}
			})
		);
		await model.load();
		await model.remove('block', entry(5).target_id);
		expect(model.has('block', entry(5).target_id)).toBe(true);
		expect(model.list).toHaveLength(1);
		expect(model.toggleError).toBe('Сбой удаления');
	});

	test('rapid_taps_end_in_the_last_state_and_run_one_request_after_another', async () => {
		const first = deferred<BookmarkView>();
		const calls: string[] = [];
		const model = new ConversationBookmarks(
			CONVERSATION,
			fakeApi({
				put: async () => {
					calls.push('put');
					return first.promise;
				},
				remove: async () => {
					calls.push('delete');
				}
			})
		);
		const target = entry(5).target_id;
		const add = model.toggle('block', target);
		// Первый запрос уже ушёл к хабу: второй встаёт за ним, а не отменяет его.
		await Promise.resolve();
		const remove = model.toggle('block', target);
		expect(model.has('block', target)).toBe(false);
		first.resolve(entry(5));
		await Promise.all([add, remove]);
		expect(calls).toEqual(['put', 'delete']);
		expect(model.has('block', target)).toBe(false);
		expect(model.count).toBe(0);
	});

	test('an_event_with_string_seq_updates_the_glyph_and_does_not_double_an_own_change', () => {
		const model = new ConversationBookmarks(CONVERSATION, fakeApi());
		const view = entry(7);
		const change = {
			conversation_id: CONVERSATION,
			target_kind: 'block',
			target_id: view.target_id,
			bookmarked: true,
			bookmark: { ...view, seq: '7', jump_seq: '7' }
		} as unknown as BookmarkChange;
		model.apply(change);
		model.apply(change);
		expect(model.has('block', view.target_id)).toBe(true);
		expect(model.list).toHaveLength(1);
		expect(model.list[0]?.seq).toBe(7);
		const removed = { ...change, bookmarked: false, bookmark: undefined } as unknown as BookmarkChange;
		model.apply(removed);
		expect(model.has('block', view.target_id)).toBe(false);
		expect(model.list).toHaveLength(0);
	});
});

describe('all bookmarks list', () => {
	function pageApi(pages: Record<string, BookmarkView[]>): BookmarksListApi & { calls: (string | undefined)[] } {
		const calls: (string | undefined)[] = [];
		return {
			calls,
			async list(query) {
				calls.push(query.cursor);
				const items = pages[query.cursor ?? 'first'] ?? [];
				return { bookmarks: items, next_cursor: query.cursor ? null : 'next' };
			},
			async remove() {
				return undefined;
			}
		};
	}

	test('a_new_bookmark_goes_on_top_only_until_the_list_is_paged', async () => {
		const api = pageApi({ first: [entry(2), entry(1)], next: [entry(0)] });
		const store = new BookmarksListStore(api, () => () => undefined);
		await store.run();
		expect(store.items.map((item) => item.seq)).toEqual([2, 1]);
		const added = entry(9);
		store.apply({
			conversation_id: CONVERSATION,
			target_kind: 'block',
			target_id: added.target_id,
			bookmarked: true,
			bookmark: { ...added, seq: '9', jump_seq: '9' } as never
		});
		expect(store.items[0]?.id).toBe(added.id);
		await store.more();
		expect(store.items.map((item) => item.seq)).toEqual([9, 2, 1, 0]);
		const later = entry(10);
		store.apply({
			conversation_id: CONVERSATION,
			target_kind: 'block',
			target_id: later.target_id,
			bookmarked: true,
			bookmark: { ...later, seq: '10', jump_seq: '10' } as never
		});
		expect(store.items.some((item) => item.id === later.id)).toBe(false);
		store.apply({
			conversation_id: CONVERSATION,
			target_kind: 'block',
			target_id: added.target_id,
			bookmarked: false
		});
		expect(store.items.map((item) => item.seq)).toEqual([2, 1, 0]);
	});

	test('removing_a_row_calls_the_api_and_restores_it_on_failure', async () => {
		let fail = true;
		let deleted = 0;
		const api: BookmarksListApi = {
			list: async () => ({ bookmarks: [entry(1)], next_cursor: null }),
			remove: async () => {
				if (fail) throw new Error('Сбой удаления');
				deleted += 1;
			}
		};
		const store = new BookmarksListStore(api, () => () => undefined);
		await store.run();
		await store.remove(entry(1));
		expect(store.items).toHaveLength(1);
		expect(store.actionError).toBe('Сбой удаления');
		fail = false;
		await store.remove(entry(1));
		expect(store.items).toHaveLength(0);
		expect(deleted).toBe(1);
	});
});
