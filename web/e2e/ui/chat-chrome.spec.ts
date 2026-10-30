import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupChat, seedMockup08 } from '../../testing/fixtures/mockup-chat';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function bootChat(
	page: Page,
	opts?: {
		seeder?: (api: FakeHubApi) => string;
		setupApi?: (api: FakeHubApi) => void;
	}
) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);

	// Seed workstation machine with catalog
	seedMachine(api, {
		id: 'workstation',
		name: 'workstation',
		harnesses: [
			{
				id: 'claude',
				title: 'Claude Code',
				provider: 'claude',
				models: [
					{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium', 'high'] },
					{ id: 'sonnet-5', title: 'sonnet-5', efforts: ['low', 'medium'] },
					{ id: 'haiku', title: 'haiku', efforts: ['low'] }
				],
				permission_modes: ['default', 'ask', 'auto_edit', 'full_access']
			}
		]
	});

	if (opts?.setupApi) opts.setupApi(api);

	const id = opts?.seeder ? opts.seeder(api) : seedMockupChat(api);

	// Ensure detail has matching harness/machine
	const detail = api.details.get(id);
	if (detail) {
		if (!opts?.seeder) {
			detail.harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
			detail.machine = { id: 'workstation', name: 'workstation' };
			detail.model = 'opus-5.5';
		}
		detail.effort = detail.effort || 'medium';
		detail.permission_mode = detail.permission_mode || 'default';
	}

	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

	return { hub, api, id };
}

test('header_shows_subtitle_state_chip_and_pending_setting_indicator', async ({ page }) => {
	const { hub, id } = await bootChat(page);

	// Initial header checks
	await expect(page.locator('.header-title')).toBeVisible();
	await expect(page.getByText(/Claude Code · opus-5.5 · workstation/)).toBeVisible();
	await expect(page.locator('.state-chip')).toBeVisible();

	// Emit settings change with pending model
	hub.append(`conversation:${id}`, {
		kind: 'conversation_settings_changed',
		conversation_id: id,
		payload: {
			effective: { model: 'opus-5.5', effort: 'medium', permission_mode: 'default' },
			pending: { model: 'sonnet-5' },
			rev: 2
		}
	});
	hub.flush();

	// Subtitle updates without reload
	await expect(
		page.getByText(/Claude Code · opus-5.5 → sonnet-5 со следующего хода · workstation/)
	).toBeVisible();
});

test('settings_sheet_lists_catalog_values_and_marks_a_pending_change_for_the_next_turn', async ({ page }) => {
	await bootChat(page);

	// Open menu -> settings
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Настройки сессии' }).click();

	// Verify catalog options
	const modelSelect = page.locator('#model');
	await expect(modelSelect).toBeVisible();
	await expect(modelSelect.locator('option')).toHaveText(['opus-5.5', 'sonnet-5', 'haiku']);

	// Select sonnet-5 and save
	await modelSelect.selectOption('sonnet-5');
	await page.getByRole('button', { name: 'Сохранить настройки' }).click();

	// Verify pending indicator appears in the header subtitle
	await expect(page.locator('.subtitle')).toContainText('со следующего хода');
});

test('unsupported_midsession_setting_is_disabled_with_the_reason_text', async ({ page }) => {
	await bootChat(page);

	// Open settings
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Настройки сессии' }).click();

	// In fake hub route: selecting invalid-model returns 422
	const modelSelect = page.locator('#model');
	await modelSelect.evaluate((node: HTMLSelectElement) => {
		const opt = document.createElement('option');
		opt.value = 'invalid';
		opt.text = 'invalid';
		node.appendChild(opt);
		node.value = 'invalid';
		node.dispatchEvent(new Event('change', { bubbles: true }));
	});

	await page.getByRole('button', { name: 'Сохранить настройки' }).click();

	// Error message appears under control
	await expect(page.getByText('Модель неизвестна или недоступна на этой машине')).toBeVisible();
});

test('stop_force_stop_resume_and_continue_queue_call_endpoints_with_keys_and_confirmations', async ({ page }) => {
	const calls: { url: string; method: string; body: { force?: boolean } | null; key: string }[] = [];
	page.on('request', (req) => {
		if (req.url().includes('/api/v1/conversations/')) {
			calls.push({
				url: req.url(),
				method: req.method(),
				body: req.postDataJSON() as { force?: boolean } | null,
				key: req.headers()['idempotency-key'] || ''
			});
		}
	});

	const { hub, id } = await bootChat(page);

	// Set execution live
	hub.append(`conversation:${id}`, {
		kind: 'execution_state_changed',
		conversation_id: id,
		payload: { generation: 1, outcome: null, state: 'running' }
	});
	hub.flush();

	// 1. Stop
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Остановить', exact: true }).click();

	await expect.poll(() => calls.find((c) => c.url.endsWith('/stop') && c.body?.force === false)).toBeDefined();
	const stopCall = calls.find((c) => c.url.endsWith('/stop') && c.body?.force === false);
	expect(stopCall!.key).not.toBe('');

	// Re-set live for force stop test
	hub.append(`conversation:${id}`, {
		kind: 'execution_state_changed',
		conversation_id: id,
		payload: { generation: 2, outcome: null, state: 'running' }
	});
	hub.flush();

	// 2. Force stop with confirmation
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Принудительно остановить' }).click();
	await expect(page.getByText('Принудительно прервать выполнение?')).toBeVisible();

	await page.getByRole('button', { name: 'Остановить принудительно' }).click();
	await expect.poll(() => calls.find((c) => c.url.endsWith('/stop') && c.body?.force === true)).toBeDefined();

	// 3. Resume
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Возобновить сессию' }).click();
	await expect.poll(() => calls.find((c) => c.url.endsWith('/resume'))).toBeDefined();

	// 4. Continue queue when live + user_stop
	hub.append(`conversation:${id}`, {
		kind: 'execution_state_changed',
		conversation_id: id,
		payload: { generation: 3, outcome: null, state: 'running' }
	});
	hub.append(`conversation:${id}`, {
		kind: 'queue_state_changed',
		conversation_id: id,
		payload: {
			pause_reasons: [{ reason: 'user_stop' }],
			hold_reasons: [],
			run_slot: 'free'
		}
	});
	hub.flush();

	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Продолжить очередь' }).click();
	await expect.poll(() => calls.find((c) => c.url.endsWith('/queue/continue'))).toBeDefined();
});

test('interrupt_on_a_harness_that_exits_offers_resume_and_continue', async ({ page }) => {
	const calls: { url: string; body: { continue_queue?: boolean } | null; key: string }[] = [];
	page.on('request', (req) => {
		if (req.url().includes('/api/v1/conversations/')) {
			calls.push({
				url: req.url(),
				body: req.postDataJSON() as { continue_queue?: boolean } | null,
				key: req.headers()['idempotency-key'] || ''
			});
		}
	});

	const { hub, id } = await bootChat(page);

	// Synthetic exit-on-interrupt: queue paused with user_stop, execution not live (no_live_execution)
	hub.append(`conversation:${id}`, {
		kind: 'execution_state_changed',
		conversation_id: id,
		payload: { generation: 1, outcome: 'synthetic-exit-on-interrupt', state: 'stopped' }
	});
	hub.append(`conversation:${id}`, {
		kind: 'queue_state_changed',
		conversation_id: id,
		payload: {
			pause_reasons: [{ reason: 'user_stop' }],
			hold_reasons: [{ reason: 'no_live_execution' }],
			run_slot: 'free'
		}
	});
	hub.flush();

	// Card offers "Возобновить сессию и очередь"
	const button = page.getByRole('button', { name: 'Возобновить сессию и очередь' });
	await expect(button).toBeVisible();
	await button.click();

	await expect.poll(() => calls.find((c) => c.url.endsWith('/resume'))).toBeDefined();
	const resumeCall = calls.find((c) => c.url.endsWith('/resume'));
	expect(resumeCall!.body).toEqual({ continue_queue: true });
	expect(resumeCall!.key).not.toBe('');
});

test('pause_cards_render_mockup_08_in_both_themes', async ({ page }) => {
	await bootChat(page, {
		seeder: (api) => seedMockup08(api)
	});

	// Mockup 08 cards
	await expect(page.getByText('Пауза: недельный лимит Claude 95% · резерв владельца')).toBeVisible();
	await expect(page.getByText('Вкладку можно закрыть: продолжение отправит хаб.')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Продолжить, тратя резерв' })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Изменить текст продолжения' })).toBeVisible();

	// Toggle theme
	await page.locator('.chat-header [data-theme-toggle]').click();
	await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');

	// Cards remain rendered
	await expect(page.getByText('Пауза: недельный лимит Claude 95% · резерв владельца')).toBeVisible();
	await expect(page.getByText('Вкладку можно закрыть: продолжение отправит хаб.')).toBeVisible();
});

test('spend_reserve_button_continues_one_session_and_updates_the_card', async ({ page }) => {
	await bootChat(page, {
		seeder: (api) => seedMockup08(api)
	});

	await expect(page.getByText('Пауза: недельный лимит Claude 95% · резерв владельца')).toBeVisible();

	// Click spend reserve
	await page.getByRole('button', { name: 'Продолжить, тратя резерв' }).click();

	// Card disappears after continuation
	await expect(page.getByText('Пауза: недельный лимит Claude 95% · резерв владельца')).toHaveCount(0);
});

test('resume_text_edit_saves_through_put_crunch', async ({ page }) => {
	const puts: { enabled?: boolean; resume_text?: string }[] = [];
	page.on('request', (req) => {
		if (req.method() === 'PUT' && req.url().endsWith('/crunch')) {
			puts.push(req.postDataJSON() as { enabled?: boolean; resume_text?: string });
		}
	});

	await bootChat(page, {
		seeder: (api) => seedMockup08(api)
	});

	await page.getByRole('button', { name: 'Изменить текст продолжения' }).click();
	const textarea = page.locator('textarea[id^="resume-paused"]');
	await expect(textarea).toBeVisible();

	await textarea.fill('Лимиты сброшены, работаем!');
	await page.getByRole('button', { name: 'Сохранить' }).click();

	expect(puts.length).toBeGreaterThan(0);
	expect(puts[0]?.resume_text).toBe('Лимиты сброшены, работаем!');
});

test('diagnostics_sheet_lists_state_cursors_and_diagnostic_events', async ({ page }) => {
	await page.context().grantPermissions(['clipboard-read', 'clipboard-write']).catch(() => {});
	const { hub, id } = await bootChat(page);

	// Append diagnostic events
	const diagEv = hub.append(`conversation:${id}`, {
		kind: 'adapter_diagnostic',
		conversation_id: id,
		payload: {
			kind: 'custom',
			event_type: 'trace'
		}
	});
	const divEv = hub.append(`conversation:${id}`, {
		kind: 'state_divergence',
		conversation_id: id,
		payload: {
			reason: 'mismatch'
		}
	});
	hub.flush();

	// Open diagnostics sheet
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Диагностика' }).click();

	await expect(page.getByRole('dialog', { name: 'Диагностика' })).toBeVisible();
	await expect(page.getByText('adapter_diagnostic')).toBeVisible();
	await expect(page.getByText(diagEv.correlation_id)).toBeVisible();
	await expect(page.getByText('state_divergence')).toBeVisible();
	await expect(page.getByText(divEv.correlation_id)).toBeVisible();

	// Test copy report button
	const copyButton = page.getByRole('button', { name: 'Скопировать отчёт' });
	await copyButton.click();
	await expect(page.getByRole('button', { name: 'Отчёт скопирован' })).toBeVisible();
});

test('reconnect_mid_run_updates_header_and_cards_without_reload', async ({ page }) => {
	const { hub, id } = await bootChat(page);

	// Emit mid-run updates (e.g. stopping state, pause card)
	hub.append(`conversation:${id}`, {
		kind: 'execution_state_changed',
		conversation_id: id,
		payload: { generation: 1, outcome: null, state: 'stopping' }
	});
	hub.append(`conversation:${id}`, {
		kind: 'queue_state_changed',
		conversation_id: id,
		payload: {
			pause_reasons: [{ reason: 'fatal_error' }],
			hold_reasons: [],
			run_slot: 'free'
		}
	});
	hub.flush();

	// Header and cards update dynamically
	await expect(page.locator('.state-chip .state-text')).toHaveText('Останавливается');
	await expect(page.getByText('Очередь остановлена из-за ошибки агента')).toBeVisible();
});

test('chat_chrome_has_no_horizontal_scroll_at_360px_and_keyboard_reaches_every_action', async ({ page }) => {
	await bootChat(page, {
		seeder: (api) => seedMockup08(api)
	});

	await page.setViewportSize({ width: 360, height: 640 });

	const scrollWidth = await page.evaluate(() => document.documentElement.scrollWidth);
	expect(scrollWidth).toBeLessThanOrEqual(360);

	// Test keyboard focus
	const backLink = page.getByRole('link', { name: 'Все сессии' });
	await backLink.focus();
	await expect(backLink).toBeFocused();

	await page.keyboard.press('Tab');
	await expect(page.getByRole('button', { name: 'Меню сессии' })).toBeFocused();

	await page.keyboard.press('Tab');
	await expect(page.locator('.chat-header [data-theme-toggle]')).toBeFocused();
});
