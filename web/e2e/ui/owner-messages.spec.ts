import type { Page } from '@playwright/test';
import type { HistoryBlock, HistoryInput, HistoryItem } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = new Date().toISOString();

function block(api: FakeHubApi, seq: number, text: string): HistoryBlock {
	const id = api.id();
	api.blocks.set(id, { text, rev: 1, state: 'final' });
	return {
		kind: 'block',
		id,
		block_kind: 'text',
		content: text,
		content_bytes: text.length,
		content_truncated: false,
		created_seq: String(seq),
		created_at: AT,
		rev: 1,
		state: 'final',
		agent: 'agent',
		model: 'opus',
		tool_name: null,
		summary: null,
		parent_block_id: null,
		input_command_id: null,
		line_count: 1
	};
}

function message(
	api: FakeHubApi,
	seq: number,
	text: string,
	extra: Partial<HistoryInput> = {}
): HistoryInput {
	return {
		kind: 'input',
		id: api.id(),
		idempotency_key: `message-${seq}-${api.id()}`,
		state: 'accepted',
		content: [{ type: 'text', text }],
		attachments: [],
		created_seq: String(seq),
		feed_seq: String(seq),
		created_at: AT,
		retry_of: null,
		origin: null,
		revision: 1,
		...extra
	};
}

function seedMessages(api: FakeHubApi, id: string, count: number, prefix = 'Моё сообщение'): HistoryInput[] {
	const items: HistoryItem[] = [];
	const messages: HistoryInput[] = [];
	for (let seq = 1; seq <= count; seq++) {
		const item = message(api, seq, `${prefix} ${seq}\nподробности`, {
			created_at: new Date(Date.now() - (count - seq) * 60_000).toISOString()
		});
		messages.push(item);
		items.push(item);
	}
	api.histories.set(id, items);
	api.bindConversation(id);
	return messages;
}

function seedLongChat(api: FakeHubApi, id: string, blocks = 3000, every = 25): HistoryInput[] {
	const items: HistoryItem[] = [];
	const messages: HistoryInput[] = [];
	for (let seq = 1; seq <= blocks; seq++) {
		if (seq % every === 0) {
			const item = message(api, seq, `Моё сообщение ${messages.length + 1}\nдетали`, {
				created_at: new Date(Date.now() - (blocks - seq) * 1000).toISOString()
			});
			messages.push(item);
			items.push(item);
		} else {
			items.push(block(api, seq, `Ответ ${seq}`));
		}
	}
	api.histories.set(id, items);
	api.bindConversation(id);
	return messages;
}

async function boot(page: Page, items?: (api: FakeHubApi, id: string) => unknown) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedConversation(api);
	const result = items?.(api, id);
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await expect(page.getByRole('textbox', { name: 'Сообщение' })).toBeVisible();
	return { hub, api, id, result };
}

async function openMessages(page: Page): Promise<void> {
	await page.getByRole('button', { name: 'Мои сообщения' }).click();
	await expect(page.locator('[data-owner-list], [data-owner-empty]')).toBeVisible();
}

test('owner_messages_list_opens_scrolled_to_the_newest_with_origins_and_times', async ({ page }) => {
	await boot(page, (api, id) => {
		const now = Date.now();
		const items: HistoryItem[] = [];
		for (let n = 1; n <= 16; n++) {
			items.push(
				message(api, n, `Сообщение ${n}`, {
					created_at: new Date(now - (20 - n) * 3600_000).toISOString()
				})
			);
		}
		items.push(
			message(api, 17, 'От оркестратора', {
				created_at: new Date(now - 5 * 60_000).toISOString(),
				origin: {
					kind: 'message',
					from: '00000000-0000-4000-8000-000000000123',
					title: 'Оркестратор',
					model: 'sonnet-5',
					chain_depth: 1
				}
			}),
			message(api, 18, 'Сработал будильник', {
				created_at: new Date(now - 4 * 60_000).toISOString(),
				origin: { kind: 'alarm', chain_depth: 0 }
			}),
			message(api, 19, 'С картинкой', {
				created_at: new Date(now - 3 * 60_000).toISOString(),
				content: [
					{ type: 'text', text: 'С картинкой' },
					{
						type: 'attachment',
						attachment_id: '00000000-0000-4000-8000-000000000321',
						name: 'shot.png',
						mime: 'image/png',
						size: 10,
						sha256: 'x',
						image: true
					}
				],
				attachments: [
					{
						attachment_id: '00000000-0000-4000-8000-000000000321',
						name: 'shot.png',
						mime: 'image/png',
						size: 10,
						sha256: 'x',
						image: true
					}
				]
			}),
			message(api, 20, 'Последнее', { created_at: new Date(now - 60_000).toISOString() })
		);
		api.histories.set(id, items);
		api.bindConversation(id);
	});

	await openMessages(page);
	await expect(page.getByText(/^19 сообщений$/)).toBeVisible();

	const list = page.locator('[data-owner-list]');
	await expect(list).toBeVisible();
	await expect(list.getByText('Последнее')).toBeVisible();
	await expect(list.getByText('От сессии Оркестратор · модель sonnet-5')).toBeVisible();
	// Будильники и события хаба не опорные точки владельца: хаб их не отдаёт.
	await expect(list.getByText('Сработал будильник')).toHaveCount(0);
	await expect(list.getByText('Вложений: 1')).toHaveCount(1);
	expect(await list.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
});

test('owner_messages_list_has_an_empty_state', async ({ page }) => {
	await boot(page);
	await openMessages(page);
	await expect(page.locator('[data-owner-empty]')).toHaveText('Сообщений пока нет');
});

test('jump_to_an_early_message_loads_only_its_window_and_pauses_following', async ({ page }) => {
	test.setTimeout(120_000);
	const { result } = await boot(page, (api, id) => seedLongChat(api, id, 3000, 25));
	const messages = result as HistoryInput[];
	const first = messages[0]!;
	await openMessages(page);
	const list = page.locator('[data-owner-list]');
	await list.evaluate((el) => {
		el.scrollTop = 0;
	});
	const item = page.locator(`[data-owner-seq="${first.feed_seq}"]`);
	await expect(item).toBeVisible({ timeout: 10_000 });
	await item.click();

	const bubble = page.locator('.bubble[data-jump]');
	await expect(bubble).toContainText('Моё сообщение 1', { timeout: 10_000 });
	await expect(bubble).toBeFocused();
	await expect.poll(async () => Number(await page.locator('.feed').getAttribute('data-loaded'))).toBeLessThan(500);
	await expect(page.getByRole('button', { name: 'К последним сообщениям' })).toBeVisible();
	await expect(page.locator('.feed')).not.toHaveAttribute('data-pinned', '');
	expect(await page.locator('[data-row]').count()).toBeLessThan(80);

	await page.getByRole('button', { name: 'К последним сообщениям' }).click();
	await expect(page.getByRole('button', { name: 'К последним сообщениям' })).toBeHidden();
	await expect(page.getByText('Моё сообщение 120')).toBeVisible();
});

test('owner_messages_list_is_virtualized_with_thousands_of_messages', async ({ page }) => {
	test.setTimeout(120_000);
	await boot(page, (api, id) => seedMessages(api, id, 3000));
	await openMessages(page);
	const list = page.locator('[data-owner-list]');
	await expect(list).toBeVisible();
	await expect.poll(async () => page.locator('[data-owner-message]').count()).toBeLessThan(100);
	await expect(list.getByText('Моё сообщение 3000')).toBeVisible();
	expect(await list.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
});

test('owner_messages_refresh_when_the_owner_sends_while_the_panel_is_open', async ({ page }, info) => {
	test.skip(info.project.name !== 'desktop', 'на телефоне лист перекрывает поле ввода');
	const { api, id } = await boot(page, (api, id) => seedMessages(api, id, 3));
	await openMessages(page);
	const items = api.histories.get(id)!;
	const extra = message(api, 10, 'Новое на глазах', { created_at: new Date().toISOString() });
	items.push(extra);
	api.histories.set(id, items);

	await page.getByRole('textbox', { name: 'Сообщение' }).fill('Новое на глазах');
	await page.getByRole('button', { name: 'Отправить', exact: true }).click();
	await expect(page.locator('[data-owner-list]').getByText('Новое на глазах')).toBeVisible({ timeout: 10_000 });
	await expect(page.getByText(/^4 сообщения$/)).toBeVisible();
});

test('owner_messages_jump_long_task_stays_within_budget_and_is_visible_fast', async ({ page, browserName }) => {
	test.skip(browserName !== 'chromium', 'chromium only');
	test.setTimeout(180_000);
	const { result } = await boot(page, (api, id) => seedLongChat(api, id, 3200, 25));
	const first = (result as HistoryInput[])[0]!;
	const session = await page.context().newCDPSession(page);
	await session.send('Emulation.setCPUThrottlingRate', { rate: 4 });
	await openMessages(page);
	await page.locator('[data-owner-list]').evaluate((el) => {
		el.scrollTop = 0;
	});
	const item = page.locator(`[data-owner-seq="${first.feed_seq}"]`);
	await expect(item).toBeVisible({ timeout: 10_000 });
	await page.evaluate(() => {
		const tasks: { start: number; duration: number }[] = [];
		new PerformanceObserver((list) => {
			for (const entry of list.getEntries()) tasks.push({ start: entry.startTime, duration: entry.duration });
		}).observe({ entryTypes: ['longtask'] });
		(window as unknown as { __tasks?: { start: number; duration: number }[] }).__tasks = tasks;
	});
	const started = Date.now();
	await item.click();
	await expect(page.locator('.bubble[data-jump]')).toContainText('Моё сообщение 1', { timeout: 10_000 });
	const visibleMs = Date.now() - started;
	const tasks = await page.evaluate(
		() => (window as unknown as { __tasks?: { start: number; duration: number }[] }).__tasks ?? []
	);
	const longest = Math.max(0, ...tasks.map((task) => task.duration));
	// Процессор замедлен вчетверо: 250 мс здесь — около 60 мс на настоящем, а запас гасит гонку с соседними тестами гейта.
	expect(longest).toBeLessThan(250);
	expect(visibleMs).toBeLessThan(1000);
	process.stdout.write(
		`owner-messages jump: longest task ${Math.round(longest)} ms, visible in ${visibleMs} ms\n`
	);
});
