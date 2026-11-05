import type { Page } from '@playwright/test';
import type { HistoryBlock, HistoryInput, HistoryItem } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';
const NEWEST = 'Моё сообщение 300';

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
		line_count: text.split('\n').length
	};
}

function message(api: FakeHubApi, seq: number, text: string): HistoryInput {
	return {
		kind: 'input',
		id: api.id(),
		idempotency_key: `message-${seq}-${api.id()}`,
		state: 'accepted',
		content: [{ type: 'text', text }],
		attachments: [],
		created_seq: String(seq),
		feed_seq: String(seq),
		created_at: AT
	};
}

/** Длинная лента с высокими блоками: оценки строк заведомо не совпадают с их настоящей высотой. */
function seed(api: FakeHubApi, id: string, count = 300): void {
	const items: HistoryItem[] = [];
	for (let seq = 1; seq <= count; seq++) {
		if (seq % 25 === 0) {
			items.push(message(api, seq, `Моё сообщение ${seq}`));
		} else if (seq % 7 === 0) {
			const code = '```\n' + Array.from({ length: 14 }, (_, line) => `const value${line} = ${line};`).join('\n') + '\n```';
			items.push(block(api, seq, code));
		} else {
			items.push(block(api, seq, `Ответ ${seq}`));
		}
	}
	api.histories.set(id, items);
	api.bindConversation(id);
}

async function boot(page: Page) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const a = seedConversation(api, { title: 'Сессия Альфа' });
	const b = seedConversation(api, { title: 'Сессия Бета' });
	seed(api, a);
	seed(api, b, 40);
	await installChatRoutes(page, hub, api, OWNER);
	return { hub, api, a, b };
}

async function open(page: Page, id: string): Promise<void> {
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
}

/** Переход внутри приложения: тёплый разговор остаётся в памяти, а лента монтируется заново. */
async function spa(page: Page, href: string): Promise<void> {
	await page.locator('#main').evaluate((main, target) => {
		const link = document.createElement('a');
		link.href = target;
		link.textContent = target;
		main.append(link);
	}, href);
	await page.getByRole('link', { name: href, exact: true }).click();
}

async function expectAtNewest(page: Page): Promise<void> {
	const feed = page.locator('.feed');
	await expect
		.poll(() => feed.evaluate((el) => Math.round(el.scrollHeight - el.scrollTop - el.clientHeight)))
		.toBeLessThanOrEqual(2);
	await expect(page.getByText(NEWEST, { exact: false })).toBeInViewport();
	await expect(feed.locator('[data-row]').last()).toContainText(NEWEST);
}

test('first_open_lands_on_the_newest_message', async ({ page }) => {
	const { a } = await boot(page);
	await open(page, a);
	await expectAtNewest(page);
});

test('switch_a_b_a_returns_to_the_newest_message', async ({ page }) => {
	const { a, b } = await boot(page);
	await open(page, a);
	await expectAtNewest(page);
	await spa(page, `/c/${b}`);
	await expect(page).toHaveURL(new RegExp(`/c/${b}$`));
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await spa(page, `/c/${a}`);
	await expect(page).toHaveURL(new RegExp(`/c/${a}$`));
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await expectAtNewest(page);
});

test('reload_lands_on_the_newest_message', async ({ page }) => {
	const { a } = await boot(page);
	await open(page, a);
	await page.reload();
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await expectAtNewest(page);
});

test('reopening_after_a_message_jump_returns_to_the_newest_message', async ({ page }) => {
	const { a, b } = await boot(page);
	await open(page, a);
	await expectAtNewest(page);

	await page.getByRole('button', { name: 'Мои сообщения' }).click();
	const list = page.locator('[data-owner-list]');
	await expect(list).toBeVisible({ timeout: 15_000 });
	await expect(page.locator('[data-owner-seq="25"]')).toBeVisible({ timeout: 15_000 });
	await page.locator('[data-owner-seq="25"]').click();

	// Прыжок к якорю по-прежнему работает: видно выбранное сообщение, а не хвост.
	const jumped = page.locator('.bubble[data-jump]');
	await expect(jumped).toContainText('Моё сообщение 25', { timeout: 15_000 });
	await expect(jumped).toBeFocused();

	// Возврат в разговор из списка: якорь прошлого визита не должен переезжать на новый.
	await spa(page, `/c/${b}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await spa(page, `/c/${a}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

	await expectAtNewest(page);
});
