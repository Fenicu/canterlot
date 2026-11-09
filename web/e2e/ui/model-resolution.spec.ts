import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedMachine, seedPresets } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const MACHINE = '00000000-0000-4000-8000-0000000000e7';

/** Псевдоним `fable[1m]` на машине уже перешёл с Fable 5.1 на Fable 5.2. */
async function boot(page: Page) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const changed = new Date(Date.now() - 3 * 3600_000).toISOString();
	seedMachine(api, {
		id: MACHINE,
		name: 'workstation',
		harnesses: [
			{
				id: 'claude_code',
				title: 'Claude Code',
				provider: 'claude',
				models: [
					{
						id: 'fable[1m]',
						title: 'Fable 1M',
						efforts: ['low', 'medium', 'high'],
						resolved: { model: 'claude-fable-5-2', observed_at: changed, previous: 'claude-fable-5-1', changed_at: changed }
					},
					{
						id: 'opus[1m]',
						title: 'Opus 1M',
						efforts: ['low', 'medium', 'high'],
						resolved: { model: 'claude-opus-5-5', observed_at: changed, previous: null, changed_at: null }
					},
					{ id: 'haiku', title: 'Haiku', efforts: ['low'] }
				],
				permission_modes: ['owner', 'ask', 'bypass', 'read_only']
			}
		]
	} as never);
	api.machines.get(MACHINE)!.capabilities = ['pilot_v1'];
	seedPresets(api);
	const id = seedConversation(api, {
		title: 'Новая версия модели',
		machine: { id: MACHINE, name: 'workstation' },
		harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' },
		model: 'fable[1m]',
		model_resolved: 'claude-fable-5-2'
	} as never);
	await installChatRoutes(page, hub, api, OWNER);
	return { hub, api, id };
}

test('session_row_and_header_name_the_concrete_version_behind_the_alias', async ({ page }) => {
	const { hub, id } = await boot(page);
	await page.goto('/sessions');
	await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
	const meta = page.locator(`[data-session="${id}"] .meta`);
	await expect(meta).toHaveText('Claude Code · Fable 5.2 1M · workstation');
	await expect(meta).toHaveAttribute('title', 'fable[1m] → claude-fable-5-2');

	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	const subtitle = page.locator('.subtitle');
	await expect(subtitle).toHaveText('Claude Code · Fable 5.2 1M · workstation');
	await expect(subtitle).toHaveAttribute('data-model-alias', 'fable[1m] → claude-fable-5-2');

	// Новая версия приходит с первым блоком хода: шапка меняется без перечитывания.
	await expect.poll(() => hub.subscribed(`conversation:${id}`), { timeout: 15_000 }).toBe(true);
	hub.append(`conversation:${id}`, {
		kind: 'derived_changed',
		conversation_id: id,
		payload: {
			conversation_id: id,
			activity: { state: 'idle', evidence: {}, since: new Date().toISOString() },
			unread_count: 0,
			queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
			last_activity_at: null,
			settings: { model: 'fable[1m]', model_resolved: 'claude-fable-5-3', effort: 'medium', permission_mode: 'owner' },
			crunch: { enabled: false }
		}
	});
	hub.flush();
	await expect(subtitle).toHaveText('Claude Code · Fable 5.3 1M · workstation');
});

test('model_picker_shows_what_each_alias_runs_now_and_a_one_time_change_notice', async ({ page }) => {
	const { id } = await boot(page);
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Настройки сессии' }).click();

	const model = page.locator('#model');
	await expect(model.locator('option')).toHaveText(['Fable 1M — сейчас Fable 5.2', 'Opus 1M — сейчас Opus 5.5', 'Haiku']);
	await expect(page.locator('[data-session-model]')).toHaveText('В этой сессии: Fable 5.2 1M (fable[1m] → claude-fable-5-2)');
	await expect(page.locator('[data-model-change]')).toHaveText('fable[1m] теперь Fable 5.2, было Fable 5.1');
	await expect(page.locator('[data-model-resolution]')).toHaveCount(0);
	// Другой псевдоним сессия ещё не пробовала: подсказка называет наблюдение на машине.
	await model.selectOption('opus[1m]');
	await expect(page.locator('[data-model-resolution]')).toContainText('opus[1m] → claude-opus-5-5 (по ходу ');
	await model.selectOption('fable[1m]');
	if (process.env.MODEL_RESOLUTION_SHOT) await page.screenshot({ path: `${process.env.MODEL_RESOLUTION_SHOT}-${test.info().project.name}.png` });

	// Уведомление разовое: при следующем открытии настроек его нет.
	await page.reload();
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Настройки сессии' }).click();
	await expect(page.locator('[data-session-model]')).toBeVisible();
	await expect(page.locator('[data-model-change]')).toHaveCount(0);
});

test('new_session_picker_names_the_current_version_of_each_alias', async ({ page }) => {
	await boot(page);
	await page.goto('/new');
	await page.getByRole('combobox', { name: 'Машина' }).selectOption({ label: 'workstation' });
	await page.getByRole('radio', { name: 'Claude Code' }).check();
	const model = page.getByRole('combobox', { name: 'Модель' });
	await model.selectOption('fable[1m]');
	await expect(model.locator('option:checked')).toHaveText('Fable 1M — сейчас Fable 5.2');
	await expect(page.locator('[data-model-resolution]')).toContainText('fable[1m] → claude-fable-5-2');
});
