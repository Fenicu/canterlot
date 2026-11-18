import { describe, expect, it } from 'vitest';
import {
	foldModels,
	isVersionKind,
	updateCommand,
	updateHarness,
	updateText,
	updateTone,
	type UpdateItem
} from '../../src/lib/updates/text.js';
import {
	dismissKey,
	isNewer,
	newestDismissible,
	readDismiss,
	shouldShowBanner,
	unseenUpdates,
	writeDismiss,
	type DismissMarker,
	type DismissStorage
} from '../../src/lib/updates/dismiss.js';

const item = (kind: string, payload: Record<string, unknown>, extras: Partial<UpdateItem> = {}): UpdateItem => ({
	id: '01a10000-0000-7000-8000-000000000001',
	kind,
	machine: 'workstation',
	harness: 'codex',
	created_at: '2026-11-18T10:00:00.000Z',
	payload,
	...extras
});

function memory(): DismissStorage & { map: Map<string, string> } {
	const map = new Map<string, string>();
	return {
		map,
		getItem: (key) => map.get(key) ?? null,
		setItem: (key, value) => { map.set(key, value); }
	};
}

describe('текст ленты обновлений', () => {
	it('доступная версия называет харнесс, выпуск и установленную версию', () => {
		expect(updateText(item('version_available', {
			installed_version: '0.169', latest_version: '0.170', min_version: '0.150'
		}))).toBe('Доступна Codex 0.170 (установлена 0.169)');
	});

	it('ниже минимума называет установленную и нужную версии', () => {
		expect(updateText(item('below_minimum', {
			installed_version: '2.2.4', latest_version: '2.3.0', min_version: '2.2.6'
		}, { harness: 'opencode' }))).toBe('Ниже поддерживаемой версии: OpenCode 2.2.4, нужна 2.2.6');
	});

	it('самообновление показывает старую и новую версии стрелкой', () => {
		expect(updateText(item('cli_updated', { from: '2.1.311', to: '2.1.330' }, { harness: 'claude_code' })))
			.toBe('Claude Code обновился сам: 2.1.311 → 2.1.330');
	});

	it('добавленные и убранные модели перечисляются одной строкой', () => {
		expect(updateText(item('models_added', { models: ['claude-fable-5-5', 'gpt-6.1-sol'] })))
			.toBe('Добавлены модели: claude-fable-5-5, gpt-6.1-sol');
		expect(updateText(item('models_removed', { models: ['gpt-6.1-sol'] })))
			.toBe('Убраны модели: gpt-6.1-sol');
	});

	it('длинный список моделей сворачивается до пяти и счётчика', () => {
		const models = ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'];
		expect(foldModels(models)).toBe('a, b, c, d, e и ещё 3');
		expect(foldModels(models.slice(0, 5))).toBe('a, b, c, d, e');
		expect(foldModels([])).toBe('—');
		expect(updateText(item('models_added', { models }))).toBe('Добавлены модели: a, b, c, d, e и ещё 3');
	});

	it('неизвестный вид даёт спокойную русскую строку без сырого идентификатора', () => {
		expect(updateText(item('something_new', {}))).toBe('Новое обновление');
	});

	it('имена харнессов совпадают с названиями в PWA', () => {
		expect(updateHarness('claude_code')).toBe('Claude Code');
		expect(updateHarness('claude')).toBe('Claude Code');
		expect(updateHarness('codex')).toBe('Codex');
		expect(updateHarness('opencode')).toBe('OpenCode');
		expect(updateHarness('unknown')).toBe('unknown');
	});

	it('команда обновления берётся из поля ответа и из полезной нагрузки живого события', () => {
		expect(updateCommand(item('cli_updated', { update_command: 'claude update' }))).toBe('claude update');
		expect(updateCommand(item('cli_updated', {}, { update_command: 'npm i -g @openai/codex@latest' })))
			.toBe('npm i -g @openai/codex@latest');
		expect(updateCommand(item('models_added', { models: ['a'] }))).toBe('');
	});

	it('тревожный тон только у версии ниже минимума', () => {
		expect(updateTone('below_minimum')).toBe('warning');
		expect(updateTone('version_available')).toBe('neutral');
		expect(updateTone('cli_updated')).toBe('neutral');
		expect(updateTone('models_added')).toBe('neutral');
		expect(isVersionKind('version_available')).toBe(true);
		expect(isVersionKind('cli_updated')).toBe(true);
		expect(isVersionKind('models_added')).toBe(false);
	});
});

describe('правило закрытия баннера', () => {
	const older = item('version_available', {}, { id: 'a', created_at: '2026-11-18T09:00:00.000Z' });
	const newest = item('models_added', {}, { id: 'b', created_at: '2026-11-18T10:00:00.000Z' });
	const newer = item('cli_updated', {}, { id: 'c', created_at: '2026-11-18T11:00:00.000Z' });

	it('без отметки баннер показывается', () => {
		expect(shouldShowBanner([newest], null)).toBe(true);
		expect(unseenUpdates([newest, older], null)).toHaveLength(2);
	});

	it('после закрытия старые записи не возвращают баннер, а новая запись возвращает', () => {
		const dismissed: DismissMarker = newestDismissible([newest, older])!;
		expect(dismissed).toEqual({ id: 'b', created_at: '2026-11-18T10:00:00.000Z' });
		expect(shouldShowBanner([newest, older], dismissed)).toBe(false);
		expect(unseenUpdates([newer, newest, older], dismissed).map((entry) => entry.id)).toEqual(['c']);
		expect(shouldShowBanner([newer, newest, older], dismissed)).toBe(true);
	});

	it('равное время различается по идентификатору, а не по порядку в массиве', () => {
		const sameTime = item('models_removed', {}, { id: 'd', created_at: newest.created_at });
		const marker: DismissMarker = { id: 'b', created_at: newest.created_at };
		expect(isNewer(sameTime, marker)).toBe(true);
		expect(isNewer(item('models_removed', {}, { id: 'a', created_at: newest.created_at }), marker)).toBe(false);
		expect(isNewer(newest, marker)).toBe(false);
	});

	it('отметка хранится отдельно для каждого владельца и переживает перезагрузку', () => {
		const storage = memory();
		const marker = newestDismissible([newest])!;
		writeDismiss('owner-1', storage, marker);
		expect(storage.map.size).toBe(1);
		expect(readDismiss('owner-1', storage)).toEqual(marker);
		expect(readDismiss('owner-2', storage)).toBeNull();
		expect(dismissKey('owner-1')).toBe('canterlot.updates.dismissed.owner-1');
	});

	it('битая или неполная запись в хранилище не ломает правило', () => {
		const storage = memory();
		storage.setItem(dismissKey('owner-1'), '{not json');
		expect(readDismiss('owner-1', storage)).toBeNull();
		storage.setItem(dismissKey('owner-1'), JSON.stringify({ id: 'only-id' }));
		expect(readDismiss('owner-1', storage)).toBeNull();
		expect(shouldShowBanner([newest], readDismiss('owner-1', storage))).toBe(true);
	});
});
