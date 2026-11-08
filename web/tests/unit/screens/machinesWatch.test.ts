import { describe, expect, it } from 'vitest';
import { FakeHub, FakeHubApi, machineWatchSeed } from '../../../testing/fake-hub';
import { generalWarnings, hotspotRows, leakedSummary, memoryLowText, shortPath, sizeGb, unleasedText } from '#lib/screens/machines/model.js';

const NOW = Date.parse('2026-11-08T12:00:00.000Z');

describe('machines screen: watchdog report', () => {
	const seed = machineWatchSeed(new FakeHubApi(new FakeHub()), NOW);

	it('orders_caches_by_size_and_names_delegate_worktrees', () => {
		const rows = hotspotRows(seed.watch.hotspots, NOW);
		expect(rows.map(row => row.size)).toEqual(['77 ГБ', '12 ГБ', '1,2 ГБ', '0,4 ГБ']);
		expect(rows[0]).toMatchObject({ name: '/home/owner/myprojects/canterlot-2a/target', path: null, meta: 'из них incremental 31 ГБ · запись 2 ч назад' });
		expect(rows[1]).toMatchObject({ name: 'Рабочая копия «Баннер связи»', path: '…/01a1b8e2-659f-715f-aad1-bf53d98aefdd/target', meta: 'из них incremental 4,3 ГБ · запись 5 ч назад' });
		expect(rows[2]!.meta).toBe('запись 1 д назад');
		expect(hotspotRows(seed.watch.hotspots, NOW, 2)).toHaveLength(2);
	});

	it('summarises_leaked_processes_by_command', () => {
		const summary = leakedSummary(seed.watch.leaked)!;
		expect(summary.title).toBe('3 процесса пережили свои сессии');
		expect(summary.meta).toBe('cargo 2, node 1 · занимают 1,4 ГиБ');
		expect(summary.rows).toHaveLength(2);
		expect(summary.rows[0]!.name).toBe('Баннер связи');
		expect(summary.rows[0]!.processes.map(item => item.text)).toEqual(['cargo · pid 48211 · 3 ч · 600 МиБ', 'cargo · pid 48390 · 3 ч · 380 МиБ']);
		expect(summary.rows[1]!.name).toBe('Запуск d4a7c3f9');
		expect(leakedSummary([])).toBeNull();
		expect(leakedSummary(seed.watch.leaked.slice(0, 1))!.title).toBe('1 процесс пережил свою сессию');
	});

	it('uses_russian_plurals_and_hides_watch_warnings_from_the_general_list', () => {
		expect(unleasedText(0)).toBeNull();
		expect(unleasedText(1)).toBe('Без очереди: 1 сборка');
		expect(unleasedText(2)).toBe('Без очереди: 2 сборки');
		expect(unleasedText(5)).toBe('Без очереди: 5 сборок');
		expect(unleasedText(21)).toBe('Без очереди: 21 сборка');
		expect(memoryLowText(seed.mem_available_bytes)).toBe('Мало памяти: 2,1 ГиБ свободно');
		expect(memoryLowText(undefined)).toBe('Мало памяти');
		expect(generalWarnings(['disk_usage_high', 'memory_low', 'processes_leaked'])).toEqual(['Мало свободного места на диске']);
		expect(sizeGb(9_960_000_000)).toBe('10 ГБ');
		expect(shortPath('/a/b')).toBe('/a/b');
	});
});
