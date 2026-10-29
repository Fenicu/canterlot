import { describe, expect, it } from 'vitest';
import { buildSections, tabCounts, type Group } from '$lib/screens/sessions/grouping';
import type { ListRow } from '$lib/stores/list.svelte';

function row(patch: Partial<ListRow> & { id: string; title: string }): ListRow {
	return {
		title_source: 'user',
		color: null,
		pinned: false,
		archived: false,
		groups: [],
		machine: { id: 'm', name: 'workstation' },
		harness: { id: 'claude', title: 'Claude Code', provider: 'claude' },
		workdir: '/work',
		created_at: '2026-10-28T12:00:00.000Z',
		model: 'opus',
		effort: 'medium',
		permission_mode: 'default',
		activity: { state: 'idle' },
		unread_count: 0,
		queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
		crunch: { enabled: false },
		last_activity_at: null,
		...patch
	};
}

const groups: Group[] = [
	{ id: 'canterlot', name: 'canterlot', position: 0 },
	{ id: 'review', name: 'На проверку', position: 1 },
	{ id: 'books', name: 'bookshelf', position: 2 }
];

describe('sessions/grouping', () => {
	it('sections_put_pinned_first_and_show_each_session_once_in_the_all_view', () => {
		const items = [
			row({ id: 'p1', title: 'Восстановление потока', pinned: true, groups: ['canterlot'] }),
			row({ id: 'p2', title: 'Очередь сообщений', pinned: true }),
			row({ id: 'c1', title: 'Граница', groups: ['canterlot'] }),
			row({ id: 'c2', title: 'Воркер', groups: ['canterlot'] }),
			row({ id: 'c3', title: 'Адаптер', groups: ['canterlot', 'review'] }),
			row({ id: 'r1', title: 'Доставка', groups: ['review'] }),
			row({ id: 'b1', title: 'Поиск', groups: ['books'] }),
			row({ id: 'b2', title: 'Каталог', groups: ['books'] }),
			row({ id: 'b3', title: 'Обложка', groups: ['books'] }),
			row({ id: 'b4', title: 'Глава', groups: ['books'] })
		];
		const sections = buildSections(items, groups, { kind: 'all' }, new Set());
		expect(sections.map((section) => section.items.length)).toEqual([2, 3, 1, 4]);
		expect(sections.reduce((sum, section) => sum + section.count, 0)).toBe(10);
		expect(new Set(sections.flatMap((section) => section.items.map((item) => item.id))).size).toBe(10);
		expect(sections[0]?.items.map((item) => item.id)).toEqual(['p1', 'p2']);
		expect(sections[1]?.title).toBe('canterlot');
	});

	it('group_tab_lists_all_members_including_pinned', () => {
		const items = [
			row({ id: 'p1', title: 'Закреп', pinned: true, groups: ['canterlot'] }),
			row({ id: 'c1', title: 'Обычная', groups: ['canterlot'] }),
			row({ id: 'other', title: 'Чужая', groups: ['books'] })
		];
		const [section] = buildSections(items, groups, { kind: 'group', id: 'canterlot' }, new Set());
		expect(section?.items.map((item) => item.id)).toEqual(['p1', 'c1']);
	});

	it('counts_come_from_the_snapshot_and_events_not_the_visible_slice', () => {
		const tabs = tabCounts(
			{ all: 10, pinned: 2, archived: 4, groups: [{ id: 'canterlot', count: 3 }] },
			[{ id: 'canterlot', name: 'canterlot' }]
		);
		expect(tabs.map((tab) => tab.count)).toEqual([10, 2, 3, 4]);
		expect(tabs.map((tab) => tab.label)).toEqual(['Все', 'Закреплённые', 'canterlot', 'Архив']);
		const visible = buildSections([row({ id: 'only', title: 'одна' })], groups, { kind: 'all' }, new Set());
		expect(visible[0]?.count).toBe(1);
		expect(tabs[0]?.count).toBe(10);
	});
});
