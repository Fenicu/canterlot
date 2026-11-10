import { describe, expect, test } from 'vitest';
import {
	bookmarkAnchor,
	bookmarkExcerpt,
	bookmarkHref,
	bookmarkStamp,
	bookmarkWho,
	cardBookmarked,
	normalizeBookmark,
	timelineOrder,
	type BookmarkView
} from '../../../src/lib/bookmarks/model';

const NOW = new Date(2026, 10, 8, 15, 30).getTime();
const MESSAGE_AT = new Date(2026, 10, 7, 9, 15).toISOString();

function entry(patch: Partial<BookmarkView> = {}): BookmarkView {
	return {
		id: '00000000-0000-4000-8000-000000000001',
		conversation_id: '00000000-0000-4000-8000-0000000000c1',
		conversation_title: 'Миграция поиска',
		target_kind: 'block',
		target_id: '00000000-0000-4000-8000-0000000000b2',
		created_at: '2026-11-08T15:00:00.000Z',
		target_created_at: MESSAGE_AT,
		kind: 'answer',
		origin_kind: null,
		block_id: '00000000-0000-4000-8000-0000000000b2',
		input_id: null,
		seq: 7,
		jump_seq: 7,
		harness: 'claude_code',
		model: 'opus-5.5',
		archived: false,
		role: 'agent',
		agent: 'orchestrator',
		excerpt: 'Последний абзац ответа',
		...patch
	};
}

describe('bookmark anchor', () => {
	test('takes_the_last_top_level_text_block_and_ignores_nested_ones', () => {
		const first = { id: 'first', kind: 'text', parentId: null };
		const tool = { id: 'tool', kind: 'tool_call', parentId: null };
		const last = { id: 'last', kind: 'text', parentId: null };
		const nested = { id: 'nested', kind: 'text', parentId: 'tool' };
		expect(bookmarkAnchor([first, tool, nested, last])).toBe(last);
		expect(bookmarkAnchor([tool, nested, first])).toBe(first);
	});

	test('falls_back_to_the_first_top_level_block_when_the_card_has_no_text', () => {
		const tool = { id: 'tool', kind: 'tool_call', parentId: null };
		const result = { id: 'result', kind: 'tool_result', parentId: null };
		expect(bookmarkAnchor([tool, result])).toBe(tool);
		expect(bookmarkAnchor([{ id: 'nested', kind: 'text', parentId: 'tool' }])).toBeNull();
		expect(bookmarkAnchor([])).toBeNull();
	});

	test('a_card_counts_as_marked_when_any_top_level_block_is_marked', () => {
		const blocks = [
			{ id: 'first', parentId: null },
			{ id: 'last', parentId: null },
			{ id: 'nested', parentId: 'first' }
		];
		expect(cardBookmarked(blocks, (id) => id === 'last')).toBe(true);
		expect(cardBookmarked(blocks, (id) => id === 'nested')).toBe(false);
	});
});

describe('bookmark row labels', () => {
	test('owner_is_you_and_agent_uses_the_timeline_title', () => {
		expect(bookmarkWho(entry({ role: 'owner', agent: null }))).toBe('Вы');
		expect(bookmarkWho(entry({ role: 'agent', agent: 'orchestrator' }))).toBe('Claude');
		expect(bookmarkWho(entry({ role: 'agent', agent: 'reviewer' }))).toBe('Reviewer');
	});

	test('an_empty_excerpt_says_there_is_no_text', () => {
		expect(bookmarkExcerpt(entry({ excerpt: '  ' }))).toBe('Без текста');
		expect(bookmarkExcerpt(entry({ excerpt: ' ответ ' }))).toBe('ответ');
	});

	test('date_and_time_are_taken_from_the_message_not_from_the_bookmark', () => {
		expect(bookmarkStamp(entry(), NOW)).toBe('7 ноября, 09:15');
		expect(bookmarkStamp(entry({ target_created_at: 'not-a-date' }), NOW)).toBe('');
	});

	test('href_reuses_the_search_jump_address', () => {
		expect(bookmarkHref(entry())).toBe(
			'/c/00000000-0000-4000-8000-0000000000c1?block=00000000-0000-4000-8000-0000000000b2&seq=7'
		);
		expect(
			bookmarkHref(
				entry({
					kind: 'message',
					target_kind: 'input',
					block_id: null,
					input_id: 'in-1',
					jump_seq: 11
				})
			)
		).toBe('/c/00000000-0000-4000-8000-0000000000c1?input=in-1&seq=11');
	});
});

describe('bookmark event shape', () => {
	test('string_sequence_from_the_event_becomes_a_number', () => {
		const view = entry({ seq: 3, jump_seq: 3 });
		const normal = normalizeBookmark({
			...view,
			seq: '12' as unknown as number,
			jump_seq: '12' as unknown as number
		});
		expect(normal.seq).toBe(12);
		expect(normal.jump_seq).toBe(12);
	});

	test('timeline_order_is_by_place_in_the_feed', () => {
		expect(timelineOrder([entry({ target_id: 'b', seq: 9 }), entry({ target_id: 'a', seq: 2 })]).map((item) => item.target_id)).toEqual(['a', 'b']);
	});
});
