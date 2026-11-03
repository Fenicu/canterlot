import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine, seedPresets } from '../../testing/fake-hub';
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
				permission_modes: ['owner', 'ask', 'bypass', 'read_only']
			}
		]
	});

	if (opts?.setupApi) opts.setupApi(api);
	seedPresets(api);

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
		detail.permission_mode = detail.permission_mode || 'owner';
	}

	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	// Снимок фальшивого хаба берёт курсор по голове журнала, а данные из засеянного состояния: событие,
	// добавленное до снимка, оказалось бы за курсором и не пришло бы. Тесты добавляют события после подписки.
	await expect.poll(() => hub.subscribed(`conversation:${id}`), { timeout: 15_000 }).toBe(true);

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
			effective: { model: 'opus-5.5', effort: 'medium', permission_mode: 'owner' },
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

test('phone_chat_touch_scroll_does_not_move_document_and_header_and_composer_stay_in_viewport', async ({ page }) => {
	test.skip(test.info().project.name !== 'phone', 'проверка телефонной раскладки');
	await bootChat(page);
	await page.evaluate(() => {
		const tall = document.createElement('div');
		tall.style.cssText = 'position:absolute;top:0;left:0;width:1px;height:300vh';
		document.body.append(tall);
	});
	const cdp = await page.context().newCDPSession(page);
	const size = page.viewportSize()!;
	const x = Math.round(size.width / 2);
	const from = Math.round(size.height * 0.75);
	for (let swipe = 0; swipe < 6; swipe += 1) {
		await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y: from }] });
		for (let move = 1; move <= 10; move += 1) {
			await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: from - move * 40 }] });
		}
		await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
	}
	await page.waitForTimeout(300);
	const state = await page.evaluate(() => ({
		scrollTop: (document.scrollingElement as HTMLElement).scrollTop,
		innerHeight: window.innerHeight,
		header: document.querySelector('.chat-header')!.getBoundingClientRect(),
		composer: document.querySelector('[data-composer]')!.getBoundingClientRect()
	}));
	expect(state.scrollTop).toBe(0);
	expect(state.header.top).toBeGreaterThanOrEqual(0);
	expect(state.composer.bottom).toBeLessThanOrEqual(state.innerHeight);
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

test('settings_sheet_keeps_a_session_model_that_left_the_catalog', async ({ page }) => {
	const seeded = await bootChat(page);
	const detail = seeded.api.details.get(seeded.id);
	if (detail) detail.model = 'opus';
	await page.reload();
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await expect(page.locator('.subtitle')).toContainText('opus');

	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Настройки сессии' }).click();

	const modelSelect = page.locator('#model');
	await expect(modelSelect).toHaveValue('opus');
	await expect(modelSelect.locator('option')).toHaveText(['opus-5.5', 'sonnet-5', 'haiku', 'opus']);
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
	await page.getByRole('menuitem', { name: 'Запустить' }).click();
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

test('stuck_session_card_asks_for_compaction_and_shows_its_progress', async ({ page }) => {
	const posted: string[] = [];
	page.on('request', (req) => {
		if (req.method() === 'POST') posted.push(req.url());
	});
	const { hub, id } = await bootChat(page);
	hub.append(`conversation:${id}`, {
		kind: 'execution_state_changed',
		conversation_id: id,
		payload: { generation: 3, outcome: null, state: 'running' }
	});
	hub.append(`conversation:${id}`, {
		kind: 'queue_state_changed',
		conversation_id: id,
		payload: {
			pause_reasons: [{ reason: 'session_stuck' }],
			hold_reasons: [],
			run_slot: 'free'
		}
	});
	hub.flush();

	const card = page.locator('.pause-card[data-tone="danger"]');
	await expect(card).toContainText('один и тот же отказ провайдера подряд');
	await expect(card).toContainText('Причина отказа неизвестна');
	await card.getByRole('button', { name: 'Сжать контекст' }).click();
	await expect.poll(() => posted.filter((url) => url.endsWith('/compact')).length).toBe(1);
	await expect(card).toContainText('Контекст сжимается');
	await expect(card.getByRole('button')).toHaveCount(0);

	hub.append(`conversation:${id}`, {
		kind: 'queue_state_changed',
		conversation_id: id,
		payload: { pause_reasons: [], hold_reasons: [], run_slot: 'free' }
	});
	hub.flush();
	await expect(card).toHaveCount(0);
});

test('failed_start_offers_one_action_everywhere_and_it_clears_the_pause', async ({ page }) => {
	const resumes: { continue_queue?: boolean }[] = [];
	page.on('request', (req) => {
		if (req.method() === 'POST' && req.url().endsWith('/resume')) {
			resumes.push(req.postDataJSON() as { continue_queue?: boolean });
		}
	});
	const { hub, id } = await bootChat(page);
	hub.append(`conversation:${id}`, {
		kind: 'execution_state_changed',
		conversation_id: id,
		payload: { generation: 1, outcome: 'start_rejected', state: 'failed' }
	});
	hub.append(`conversation:${id}`, {
		kind: 'queue_state_changed',
		conversation_id: id,
		payload: {
			pause_reasons: [{ reason: 'fatal_error' }],
			hold_reasons: [{ reason: 'no_live_execution' }],
			run_slot: 'free'
		}
	});
	hub.flush();

	const card = page.getByRole('region', { name: 'Уведомления о состоянии очереди' });
	await expect(card.getByText('Очередь остановлена из-за ошибки агента')).toBeVisible();
	await expect(card.getByText('Сессия не запущена')).toHaveCount(0);

	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await expect(page.getByRole('menuitem', { name: 'Запустить' })).toHaveCount(1);
	await expect(page.getByRole('menuitem', { name: /Возобновить/ })).toHaveCount(0);
	await page.getByRole('button', { name: 'Меню сессии' }).click();

	await card.getByRole('button', { name: 'Запустить', exact: true }).click();
	await expect.poll(() => resumes.at(-1)?.continue_queue).toBe(true);
});

test('a_session_with_a_recorded_turn_offers_resume_instead_of_start', async ({ page }) => {
	const { hub, id } = await bootChat(page, {
		seeder: (api) => {
			const seeded = seedMockupChat(api);
			const detail = api.details.get(seeded);
			if (detail) detail.turn_recorded = true;
			return seeded;
		}
	});
	hub.append(`conversation:${id}`, {
		kind: 'execution_state_changed',
		conversation_id: id,
		payload: { generation: 1, outcome: 'user_stop', state: 'stopped' }
	});
	hub.append(`conversation:${id}`, {
		kind: 'queue_state_changed',
		conversation_id: id,
		payload: {
			pause_reasons: [],
			hold_reasons: [{ reason: 'no_live_execution' }],
			run_slot: 'free'
		}
	});
	hub.flush();
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await expect(page.getByRole('menuitem', { name: 'Возобновить' })).toHaveCount(1);
	await expect(page.getByRole('menuitem', { name: 'Запустить' })).toHaveCount(0);
	await expect(
		page.getByRole('region', { name: 'Уведомления о состоянии очереди' }).getByRole('button', { name: 'Возобновить', exact: true })
	).toBeVisible();
});

test('session_settings_show_permission_modes_by_label_never_raw', async ({ page }) => {
	const { hub, id } = await bootChat(page);
	hub.append(`conversation:${id}`, {
		kind: 'conversation_settings_changed',
		conversation_id: id,
		payload: {
			effective: { model: 'opus-5.5', effort: 'medium', permission_mode: 'owner' },
			pending: {},
			rev: 2
		}
	});
	hub.flush();
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Настройки сессии' }).click();
	const select = page.locator('select#permission');
	await expect(select).toBeVisible();
	const options = (await select.locator('option').allTextContents()).map((text) => text.trim());
	expect(options).toEqual(['Как в настройках CLI', 'Спрашивать', 'Без подтверждений', 'Только чтение']);
	await expect(page.getByText('Сейчас:').filter({ hasText: 'Как в настройках CLI' })).toBeVisible();
	await expect(page.getByText(/~\/\.claude\/settings\.json/)).toBeVisible();
	await select.selectOption('bypass');
	await expect(page.getByText(/выполняет всё без подтверждений/)).toBeVisible();
	await expect(page.getByText(/^(owner|bypass|read_only)$/)).toHaveCount(0);
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

	// Карточка предлагает единое действие
	const button = page.getByRole('button', { name: 'Запустить', exact: true });
	await expect(button).toBeVisible();
	await button.click();

	await expect.poll(() => calls.find((c) => c.url.endsWith('/resume'))).toBeDefined();
	const resumeCall = calls.find((c) => c.url.endsWith('/resume'));
	expect(resumeCall!.body).toEqual({ continue_queue: true });
	expect(resumeCall!.key).not.toBe('');
});

test('settings_sheet_changes_preset_from_next_turn_and_warns_about_restart', async ({ page }) => {
	const bodies: { instruction_profile?: string; key: string }[] = [];
	page.on('request', (request) => {
		if (request.method() === 'PATCH' && new URL(request.url()).pathname.endsWith('/settings')) {
			bodies.push({ ...(request.postDataJSON() as { instruction_profile?: string }), key: request.headers()['idempotency-key'] ?? '' });
		}
	});
	const { hub, id } = await bootChat(page);
	hub.append(`conversation:${id}`, {
		kind: 'conversation_settings_changed',
		conversation_id: id,
		payload: {
			effective: {
				model: 'opus-5.5',
				effort: 'medium',
				permission_mode: 'owner',
				instruction_profile: 'clean',
				instruction_preset: { id: 'clean', kind: 'clean', title: 'Без инструкций', revision: null, current_revision: null, deleted: false }
			},
			pending: {},
			rev: 2
		}
	});
	hub.flush();

	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Настройки сессии' }).click();
	await expect(page.getByText('Сейчас: Без инструкций')).toBeVisible();
	await expect(page.getByText('Смена инструкций перезапускает агента между ходами с сохранением истории. Пока идёт фоновая работа, смена ждёт её конца: задачи не останавливаются.')).toBeVisible();
	await page.getByRole('combobox', { name: 'Новые инструкции' }).selectOption('seed-executor');
	await page.getByRole('button', { name: 'Сохранить настройки' }).click();
	await expect.poll(() => bodies.find((body) => body.instruction_profile === 'seed-executor')).toBeDefined();
	expect(bodies.at(-1)?.key).not.toBe('');
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Настройки сессии' }).click();
	await expect(page.getByText('Без инструкций → Исполнитель (английский) со следующего хода')).toBeVisible();
});

test('apply_new_version_after_edit', async ({ page }) => {
	const bodies: { instruction_profile?: string; key: string }[] = [];
	page.on('request', (request) => {
		if (request.method() === 'PATCH' && new URL(request.url()).pathname.endsWith('/settings')) {
			bodies.push({ ...(request.postDataJSON() as { instruction_profile?: string }), key: request.headers()['idempotency-key'] ?? '' });
		}
	});
	const { hub, id } = await bootChat(page);
	hub.append(`conversation:${id}`, {
		kind: 'conversation_settings_changed',
		conversation_id: id,
		payload: {
			effective: {
				model: 'opus-5.5',
				effort: 'medium',
				permission_mode: 'owner',
				instruction_profile: 'seed-executor',
				instruction_preset: { id: 'seed-executor', kind: 'custom', title: 'Исполнитель (английский)', revision: 1, current_revision: 2, deleted: false }
			},
			pending: {},
			rev: 2
		}
	});
	hub.flush();

	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Настройки сессии' }).click();
	await expect(page.getByText('Инструкции изменены после применения')).toBeVisible();
	await page.getByRole('button', { name: 'Применить новую версию' }).click();
	await expect.poll(() => bodies.find((body) => body.instruction_profile === 'seed-executor')).toBeDefined();
});

test('deleted_preset_is_shown_as_the_sessions_copy', async ({ page }) => {
	const { hub, id } = await bootChat(page);
	hub.append(`conversation:${id}`, {
		kind: 'conversation_settings_changed',
		conversation_id: id,
		payload: {
			effective: {
				model: 'opus-5.5',
				effort: 'medium',
				permission_mode: 'owner',
				instruction_profile: 'seed-executor',
				instruction_preset: { id: 'seed-executor', kind: 'custom', title: 'Исполнитель (английский)', revision: 1, current_revision: null, deleted: true }
			},
			pending: {},
			rev: 2
		}
	});
	hub.flush();

	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Настройки сессии' }).click();
	await expect(page.getByText(/сессия работает со своей копией/)).toBeVisible();
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
	await expect(page.getByText('Сообщение адаптера')).toBeVisible();
	await expect(page.getByText(diagEv.correlation_id)).toBeVisible();
	await expect(page.getByText('Расхождение состояния')).toBeVisible();
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

test('session_dropdown_closes_outside_keeps_inside_and_toggles', async ({ page, isMobile }) => {
	await bootChat(page);
	const toggle = page.getByRole('button', { name: 'Меню сессии' });
	const menu = page.getByRole('menu', { name: 'Действия над сессией' });
	const outside = (): Promise<void> => (isMobile ? page.locator('.feed').tap({ position: { x: 8, y: 8 } }) : page.locator('.feed').click({ position: { x: 8, y: 8 } }));
	await toggle.click();
	await expect(menu).toBeVisible();
	await outside();
	await expect(menu).toHaveCount(0);
	await toggle.click();
	await toggle.click();
	await expect(menu).toHaveCount(0);
	await toggle.click();
	await menu.locator('.menu-divider').dispatchEvent('pointerdown');
	await expect(menu).toBeVisible();
	await page.keyboard.press('Escape');
	await expect(menu).toHaveCount(0);
	await expect(toggle).toBeFocused();
});
