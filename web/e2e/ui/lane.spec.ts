import type { Page } from '@playwright/test';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi, seedConversation, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { FIRST_DIALOG, seedFirstDialog } from '../../testing/fixtures/first-dialog';
import { seedMockup05, seedMockup11 } from '../../testing/fixtures/mockup-chat';
import { expect, test } from './fixtures';

async function bootLane(page: Page) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);

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
					{ id: 'haiku', title: 'haiku', efforts: ['low'] }
				],
				permission_modes: ['owner', 'ask', 'bypass', 'read_only']
			}
		]
	});

	const { id, rootBlockId } = seedMockup05(api);
	await installChatRoutes(page, hub, api);

	return { hub, api, id, rootBlockId };
}

test('lane_uses_the_same_renderer_as_the_main_chat', async ({ page }) => {
	const { id, rootBlockId } = await bootLane(page);

	await page.goto(`/c/${id}/lane/${rootBlockId}`);
	await expect(page.locator('.lane-screen[data-ready]')).toBeVisible();

	// Timeline structure: answer cards, details tool blocks
	await expect(page.locator('.lane-timeline-container .feed')).toBeVisible();
	await expect(page.locator('.card').first()).toBeVisible();
	await expect(page.locator('.tool').first()).toBeVisible();
});

test('lane_has_no_composer_and_explains_that_the_result_returns', async ({ page }) => {
	const { id, rootBlockId } = await bootLane(page);

	await page.goto(`/c/${id}/lane/${rootBlockId}`);
	await expect(page.locator('.lane-screen[data-ready]')).toBeVisible();

	// Verify no composer is rendered in lane
	await expect(page.locator('form.composer')).toHaveCount(0);
	await expect(page.locator('textarea[aria-label="Сообщение агенту"]')).toHaveCount(0);

	// Verify result returns message and link
	await expect(page.getByText('Результат вернётся в основную сессию')).toBeVisible();
	await expect(page.getByRole('link', { name: 'К основной сессии' })).toBeVisible();
});

test('desktop_delegation_panel_lists_nodes_with_state_labels_and_selects_a_lane', async ({ page }) => {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);

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
					{ id: 'haiku', title: 'haiku', efforts: ['low'] }
				],
				permission_modes: ['owner', 'ask', 'bypass', 'read_only']
			}
		]
	});

	const { id } = seedMockup11(api);
	await installChatRoutes(page, hub, api);

	await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-layout="desktop"]')).toBeVisible();

	// Open delegation panel
	await page.getByRole('button', { name: 'Делегирование' }).click();
	const panel = page.locator('[data-pane="lane"]');
	await expect(panel).toBeVisible();

	// Verify tree root and nodes
	await expect(panel.getByText('Восстановление потока')).toBeVisible();
	await expect(panel.getByText('Explore').first()).toBeVisible();
	await expect(panel.getByText('Читает')).toBeVisible();
	await expect(panel.getByText('Codex')).toBeVisible();
	await expect(panel.getByText('Ждёт')).toBeVisible();

	// Click node to open its lane in pane
	await panel.getByText('Explore').first().click();
	await expect(panel.locator('.selected-lane')).toBeVisible();
	await expect(panel.locator('.selected-lane .live-line')).toBeVisible();
});

test('delegation_shows_a_background_subagent_as_working_until_its_task_ends', async ({ page }) => {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedFirstDialog(api);
	await installChatRoutes(page, hub, api);
	const rootBlockId = FIRST_DIALOG.find((item) => item.kind === 'block' && item.tool_name === 'Agent')?.id ?? '';
	const detail = api.details.get(id);
	if (detail) {
		detail.harness = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };
		detail.execution = { state: 'running' };
	}
	const task = {
		task_id: 'a1',
		kind: 'agent',
		block_id: rootBlockId,
		parent_task_id: null,
		description: 'фоном',
		started_at: '2026-10-28T12:00:00.000Z'
	};
	api.pushBackground(id, [{ ...task, state: 'running', ended_at: null }]);
	await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-layout="desktop"]')).toBeVisible();
	await page.getByRole('button', { name: 'Делегирование' }).click();
	const node = page.locator('[data-pane="lane"] .tree-node').first();
	await expect(node.getByText('Работает')).toBeVisible();

	await page.goto(`/c/${id}/lane/${rootBlockId}`);
	await expect(page.locator('.live-dot')).toHaveClass(/active/);

	api.pushBackground(id, [{ ...task, state: 'stopped', ended_at: '2026-10-28T12:02:00.000Z' }]);
	await expect(page.locator('.live-line')).toContainText('Прервано');
	await expect(page.locator('.live-dot')).not.toHaveClass(/active/);
});

test('desktop_pane_opens_and_closes_without_losing_chat_scroll', async ({ page }) => {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);

	seedMachine(api, {
		id: 'workstation',
		name: 'workstation',
		harnesses: [
			{
				id: 'claude',
				title: 'Claude Code',
				provider: 'claude',
				models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low'] }],
				permission_modes: ['owner']
			}
		]
	});

	const { id } = seedMockup11(api);
	await installChatRoutes(page, hub, api);

	await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto(`/c/${id}`);

	const stream = page.locator('.feed');
	await expect(stream).toBeVisible();

	// Замер до открытия панели: лента доезжает до низа, пока строки домериваются.
	await expect
		.poll(() => stream.evaluate((node) => Math.round(node.scrollHeight - node.scrollTop - node.clientHeight)))
		.toBeLessThanOrEqual(2);
	const initialScroll = await stream.evaluate((node) => node.scrollTop);

	// Open lane pane
	await page.getByRole('button', { name: 'Делегирование' }).click();
	const panel = page.locator('[data-pane="lane"]');
	await expect(panel).toBeVisible();

	// Close lane pane using close button
	await panel.getByRole('button', { name: 'Закрыть боковую панель' }).click();
	await expect(panel).toHaveCount(0);

	// Замер после закрытия панели: строки перемериваются под прежнюю ширину не в том же кадре.
	await expect.poll(() => stream.evaluate((node) => node.scrollTop)).toBe(initialScroll);
});

test('finished_lane_reads_as_done_with_named_steps_and_the_report_open', async ({ page }) => {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedFirstDialog(api);
	await installChatRoutes(page, hub, api);
	const agent = FIRST_DIALOG.find((item) => item.kind === 'block' && item.tool_name === 'Agent');
	await page.goto(`/c/${id}/lane/${agent?.id}`);
	await expect(page.locator('.lane-screen[data-ready]')).toBeVisible();
	await expect(page.locator('.tool .name').first()).toHaveText('Шаг');
	await expect(page.locator('.tool .summary').first()).toHaveText('/tmp/canterlot-check/note.txt');
	await expect(page.locator('.report h3')).toHaveText('Итог');
	await expect(page.locator('.report')).toContainText('проверка кириллицы');
	await expect(page.locator('[role="log"]')).not.toContainText('Subagent hand-back');
	await expect(page.locator('.lane-screen')).not.toContainText('Результат инструмента');
	await expect(page.locator('.lane-screen')).not.toContainText('Результат вернётся');
	await expect(page.locator('.lane-screen')).not.toContainText('ркестратор');
	await expect(page.locator('.path')).toContainText('Claude');
	await expect(page.locator('.live-line')).toContainText('Готово');
	await expect(page.locator('.live-dot')).not.toHaveClass(/active/);
	await expect(page.locator('.lane-screen .mark')).not.toHaveClass(/running/);
});

const MINUTE = 60_000;

function agentCall(api: FakeHubApi, name: string, run: string, startMin: number, endMin: number | null, state: 'final' | 'streaming' | 'incomplete', seq: number): HistoryBlock[] {
	const callId = api.id();
	const childId = api.id();
	const at = (min: number): string => new Date(Date.now() - min * MINUTE).toISOString();
	const base = { kind: 'block' as const, content_truncated: false, rev: 1, model: 'haiku', summary: null, line_count: 1, input_command_id: run };
	api.blocks.set(callId, { text: name, rev: 1, state });
	api.blocks.set(childId, { text: 'шаг', rev: 1, state: 'final' });
	return [
		{ ...base, id: callId, block_kind: 'tool_call', content: name, content_bytes: name.length, created_seq: String(seq), created_at: at(startMin), state, agent: 'claude', tool_name: 'Agent', parent_block_id: null, summary: name, started_at: at(startMin), finished_at: endMin === null ? null : at(endMin) },
		{ ...base, id: childId, block_kind: 'text', content: 'шаг', content_bytes: 6, created_seq: String(seq + 1), created_at: at(startMin), state: 'final', agent: name, tool_name: null, parent_block_id: callId }
	];
}

async function bootCrowdedDelegation(page: Page, title: string) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedConversation(api, { title, model: 'opus-5.5' });
	const blocks: HistoryBlock[] = [];
	let seq = 1;
	for (let i = 0; i < 14; i++) {
		const state = i === 3 ? 'incomplete' : 'final';
		blocks.push(...agentCall(api, `старый-${i}`, 'run-1', 120 - i, 100 - i, state, seq));
		seq += 2;
	}
	blocks.push(...agentCall(api, 'свежий', 'run-2', 6, 4, 'final', seq));
	seq += 2;
	blocks.push(...agentCall(api, 'идёт', 'run-2', 5, null, 'streaming', seq));
	api.histories.set(id, blocks);
	api.bindConversation(id);
	await installChatRoutes(page, hub, api);
	return { id };
}

test('delegation_collapses_old_subagents_into_a_row_and_opens_each_lane', async ({ page, isMobile }) => {
	const { id } = await bootCrowdedDelegation(page, 'Восстановление потока после сна');
	if (!isMobile) await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	if (isMobile) await page.getByRole('button', { name: /^Делегирование/ }).click();
	else await page.locator('.topbar').getByRole('button', { name: 'Делегирование' }).click();
	const scope = isMobile ? page.getByRole('dialog', { name: 'Делегирование' }) : page.locator('[data-pane="lane"]');
	await expect(scope).toBeVisible();

	const names = scope.locator('.tree-node:not(.older-toggle) .subagent-name');
	await expect(names).toHaveText(['идёт', 'свежий']);
	const fresh = scope.locator('.tree-node').filter({ hasText: 'свежий' });
	await expect(fresh.locator('.worked')).toHaveText('2 мин');
	await expect(scope.locator('.tree-node').filter({ hasText: 'идёт' }).locator('.worked')).toHaveCount(0);
	await page.screenshot({ path: `test-results/delegation-worked-${test.info().project.name}.png` });
	const toggle = scope.getByRole('button', { name: /^Завершённые/ });
	await expect(toggle).toHaveText('Завершённые · 14, прервано 1');
	await expect(toggle).toHaveAttribute('aria-expanded', 'false');
	// Прерванное — тем же цветом, что и в ленте, а не цветом ошибки.
	const warning = await page.evaluate(() => {
		const probe = document.createElement('i');
		probe.style.color = 'var(--warning)';
		document.body.append(probe);
		const value = getComputedStyle(probe).color;
		probe.remove();
		return value;
	});
	await expect(toggle.locator('.warning')).toHaveCSS('color', warning);

	await toggle.click();
	await expect(toggle).toHaveAttribute('aria-expanded', 'true');
	const older = scope.locator('.older .subagent-name');
	await expect(older).toHaveCount(14);
	await expect(older.first()).toHaveText('старый-13');
	await expect(older.last()).toHaveText('старый-0');
	await expect(scope.locator('.older').getByText('Прервано')).toHaveCount(1);
	await expect(scope.locator('.older').getByText('Прервано')).toHaveCSS('color', warning);

	if (isMobile) {
		await scope.locator('.tree-node').filter({ hasText: 'идёт' }).click();
		await expect(page).toHaveURL(/\/c\/[^/]+\/lane\/[^/]+$/);
		await expect(page.locator('.lane-screen[data-ready]')).toBeVisible();
	} else {
		await scope.locator('.tree-node').filter({ hasText: 'старый-2' }).click();
		await expect(scope.locator('.selected-lane')).toBeVisible();
	}
});

test('a_quiet_delegation_has_no_collapsed_row', async ({ page, isMobile }) => {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedConversation(api, { title: 'Один субагент', model: 'opus-5.5' });
	api.histories.set(id, agentCall(api, 'идёт', 'run-1', 2, null, 'streaming', 1));
	api.bindConversation(id);
	await installChatRoutes(page, hub, api);
	if (!isMobile) await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	if (isMobile) await page.getByRole('button', { name: 'Делегирование, работает 1' }).click();
	else await page.locator('.topbar').getByRole('button', { name: 'Делегирование' }).click();
	const scope = isMobile ? page.getByRole('dialog', { name: 'Делегирование' }) : page.locator('[data-pane="lane"]');
	await expect(scope.locator('.subagent-name')).toHaveText(['идёт']);
	await expect(scope.getByRole('button', { name: /^Завершённые/ })).toHaveCount(0);
});

test('phone_header_keeps_the_delegation_entry_with_a_long_title_at_384px', async ({ page, isMobile }) => {
	test.skip(!isMobile, 'только телефон');
	const { id } = await bootCrowdedDelegation(page, 'Очень длинное название сессии, которое никак не помещается в шапку телефона целиком');
	await page.setViewportSize({ width: 384, height: 800 });
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	const entry = page.getByRole('button', { name: 'Делегирование, работает 1' });
	await expect(entry).toBeVisible();
	await expect(entry.locator('.running-count')).toHaveText('1');
	const boxes = await page.evaluate(() => {
		const rect = (selector: string) => document.querySelector(selector)?.getBoundingClientRect();
		const title = rect('.header-title')!;
		const actions = rect('.header-actions')!;
		return { titleRight: title.right, titleWidth: title.width, actionsLeft: actions.left, actionsRight: actions.right, scroll: document.documentElement.scrollWidth, width: window.innerWidth };
	});
	expect(boxes.titleRight).toBeLessThanOrEqual(boxes.actionsLeft);
	expect(boxes.titleWidth).toBeGreaterThan(120);
	expect(boxes.actionsRight).toBeLessThanOrEqual(boxes.width);
	expect(boxes.scroll).toBeLessThanOrEqual(boxes.width);
	await page.screenshot({ path: 'test-results/delegation-header-384.png' });
});

test('phone_has_no_delegation_entry_without_subagents', async ({ page, isMobile }) => {
	test.skip(!isMobile, 'только телефон');
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedConversation(api, { title: 'Без делегирования', model: 'opus-5.5' });
	await installChatRoutes(page, hub, api);
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await expect(page.getByRole('button', { name: /^Делегирование/ })).toHaveCount(0);
});

test('lane_of_an_old_subagent_outside_the_loaded_window_shows_its_steps', async ({ page }) => {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedConversation(api, { title: 'Старый субагент', model: 'opus-5.5' });
	const base = {
		kind: 'block' as const,
		content: '',
		content_bytes: 0,
		content_truncated: false,
		created_at: '2026-11-01T09:00:00.000Z',
		rev: 1,
		state: 'final',
		line_count: 1,
		input_command_id: null
	};
	const call: HistoryBlock = { ...base, id: 'old-agent', block_kind: 'tool_call', created_seq: '1', agent: 'orchestrator', model: 'opus-5.5', tool_name: 'Agent', summary: 'Старая разведка', parent_block_id: null };
	const lane: HistoryBlock[] = Array.from({ length: 6 }, (_, index) => ({
		...base,
		id: `old-step-${index}`,
		block_kind: 'tool_call' as const,
		created_seq: String(2 + index),
		agent: 'general-purpose',
		model: 'sonnet-5.5',
		tool_name: 'Bash',
		summary: `старый шаг ${index}`,
		parent_block_id: 'old-agent'
	}));
	const filler: HistoryBlock[] = Array.from({ length: 150 }, (_, index) => ({
		...base,
		id: `later-${index}`,
		block_kind: 'text' as const,
		content: `позже ${index}`,
		created_seq: String(100 + index),
		agent: 'orchestrator',
		model: 'opus-5.5',
		tool_name: null,
		summary: null,
		parent_block_id: null
	}));
	api.histories.set(id, [call, ...lane, ...filler]);
	for (const block of [call, ...lane, ...filler]) api.blocks.set(block.id, { text: block.content, rev: block.rev, state: block.state });
	api.bindConversation(id);
	await installChatRoutes(page, hub, api);

	await page.goto(`/c/${id}/lane/old-agent`);
	await expect(page.locator('.lane-screen[data-ready]')).toBeVisible();
	await expect(page.locator('.tool .summary').filter({ hasText: 'старый шаг 5' })).toBeVisible();
	await expect(page.locator('.header-title')).toHaveText('Старая разведка');
	await expect(page.locator('.subtitle')).toContainText('general-purpose');
	await expect(page.locator('.live-line')).toContainText('Готово');
});

test('feed_and_delegation_panel_name_an_unfinished_subagent_the_same_way', async ({ page, isMobile }) => {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedConversation(api, { title: 'Один субагент', model: 'opus-5.5', harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' } } as never);
	api.histories.set(id, agentCall(api, 'Explore', 'run-1', 10, 8, 'final', 1));
	api.bindConversation(id);
	await installChatRoutes(page, hub, api);
	if (!isMobile) await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	const feed = page.locator('[role="log"] .sub .status');
	await expect(feed).toHaveAttribute('data-state', 'stopped');
	await expect(feed).toHaveText(/^Прервано/);
	if (isMobile) await page.getByRole('button', { name: /^Делегирование/ }).click();
	else await page.locator('.topbar').getByRole('button', { name: /Делегирование/ }).click();
	const scope = isMobile ? page.getByRole('dialog', { name: 'Делегирование' }) : page.locator('[data-pane="lane"]');
	const node = scope.locator('.tree-node').filter({ hasText: 'Explore' });
	await expect(node.locator('.warning')).toHaveText('Прервано');
	expect(await feed.evaluate((el) => getComputedStyle(el).color)).toBe(await node.locator('.warning').evaluate((el) => getComputedStyle(el).color));
});
