import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedHistory } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const RUNNING = '.compact[data-running="true"]';

function liveExecution(api: FakeHubApi, id: string): void {
	const detail = api.details.get(id) as Record<string, unknown>;
	detail.execution = { state: 'running', generation: 1 };
}

function seed(titles: string[], entries = 3) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const ids = titles.map((title) => {
		const id = seedConversation(api, { title });
		seedHistory(api, id, entries);
		liveExecution(api, id);
		api.pushContext(id, 900_000, 1_000_000);
		return id;
	});
	return { hub, api, ids };
}

async function ready(page: Page) {
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
}

function indicator(page: Page) {
	return page.getByRole('button', { name: /^Контекст/ });
}

test('running_compaction_from_history_is_shown_on_fresh_load', async ({ page }) => {
	const { hub, api, ids: [id] } = seed(['Сессия Альфа'], 20);
	api.pushCompaction(id!);
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(`/c/${id}`);
	await ready(page);
	await expect(page.locator(RUNNING)).toHaveCount(1);
	await expect(page.locator(RUNNING)).toBeInViewport();
	await expect(indicator(page)).toHaveAttribute('data-compacting', 'true');
	await expect(indicator(page)).toContainText('Сжимает контекст');
	// Полный контекст уже сжимается: тревожный тон на это время снят.
	await expect(indicator(page)).toHaveAttribute('data-tone', 'accent');
});

test('compaction_started_while_away_is_shown_on_return', async ({ page }) => {
	const { hub, api, ids: [id] } = seed(['Сессия Альфа'], 20);
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto('/');
	const link = page.getByRole('link', { name: /Сессия Альфа/ }).first();
	await link.click();
	await ready(page);
	await expect(page.locator(RUNNING)).toHaveCount(0);
	await page.goBack();
	await link.waitFor();
	api.pushCompaction(id!);
	hub.flush();
	await link.click();
	await ready(page);
	await expect(page.locator(RUNNING)).toHaveCount(1);
	await expect(page.locator(RUNNING)).toBeInViewport();
});

test('compaction_started_in_a_side_chat_is_shown_after_switching_back', async ({ page }) => {
	await page.setViewportSize({ width: 1280, height: 800 });
	const { hub, api, ids: [a] } = seed(['Сессия Альфа', 'Сессия Бета'], 60);
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(`/c/${a}`);
	const chat = page.locator('[data-pane="chat"]');
	await expect(chat.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	const list = page.locator('[data-pane="list"]');
	await list.getByRole('link', { name: /Сессия Бета/ }).click();
	await expect(chat.getByRole('heading', { level: 1 })).toHaveText('Сессия Бета');
	api.pushCompaction(a!);
	hub.flush();
	await list.getByRole('link', { name: /Сессия Альфа/ }).click();
	await expect(chat.getByRole('heading', { level: 1 })).toHaveText('Сессия Альфа');
	await expect(chat.locator(RUNNING)).toHaveCount(1);
	await expect(chat.locator(RUNNING)).toBeInViewport();
});

test('compaction_cut_off_by_the_turn_end_does_not_spin_after_reopen', async ({ page }) => {
	const { hub, api, ids: [id] } = seed(['Сессия Альфа']);
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto('/');
	const link = page.getByRole('link', { name: /Сессия Альфа/ }).first();
	await link.click();
	await ready(page);
	api.pushCompaction(id!);
	hub.flush();
	await expect(page.locator(RUNNING)).toHaveCount(1);
	await page.goBack();
	await link.waitFor();
	api.closeOpenCompactions(id!);
	hub.flush();
	await link.click();
	await ready(page);
	await expect(page.getByText('Сжатие контекста не удалось')).toBeVisible();
	await expect(page.locator(RUNNING)).toHaveCount(0);
	await expect(indicator(page)).not.toHaveAttribute('data-compacting', 'true');

	await page.reload();
	await ready(page);
	await expect(page.getByText('Сжатие контекста не удалось')).toBeVisible();
	await expect(page.locator(RUNNING)).toHaveCount(0);
});

test('requested_compaction_shows_the_running_state_before_the_agent_starts', async ({ page }) => {
	const { hub, api, ids: [id] } = seed(['Сессия Альфа']);
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(`/c/${id}`);
	await ready(page);
	await expect(page.locator(RUNNING)).toHaveCount(0);
	await indicator(page).click();
	const details = page.locator('[data-details]');
	await details.getByRole('button', { name: 'Сжать контекст' }).click();
	await details.getByRole('button', { name: 'Точно сжать?' }).click();
	await expect(details).toBeHidden();
	// Воркер ещё не сообщил о старте: строка и индикатор уже говорят, что сжатие идёт.
	await expect(page.locator(RUNNING)).toHaveCount(1);
	await expect(indicator(page)).toHaveAttribute('data-compacting', 'true');

	const started = api.pushCompaction(id!);
	hub.flush();
	await expect(page.locator(RUNNING)).toHaveCount(1);
	api.finishCompaction(id!, (started.payload as { id: string }).id);
	hub.flush();
	await expect(page.getByText('Контекст сжат автоматически')).toBeVisible();
	await expect(page.locator(RUNNING)).toHaveCount(0);
	await expect(indicator(page)).not.toHaveAttribute('data-compacting', 'true');
	await expect(indicator(page)).toContainText('Контекст 90%');
	await expect(indicator(page)).toHaveAttribute('data-tone', 'danger');
});

test('requested_compaction_settles_when_a_fresh_snapshot_already_has_its_result', async ({ page }) => {
	const { hub, api, ids: [id] } = seed(['Сессия Альфа']);
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(`/c/${id}`);
	await ready(page);
	await indicator(page).click();
	const details = page.locator('[data-details]');
	await details.getByRole('button', { name: 'Сжать контекст' }).click();
	await details.getByRole('button', { name: 'Точно сжать?' }).click();
	await expect(page.locator(RUNNING)).toHaveCount(1);
	// События эпизода пропали в переподключении: итог приходит только снимком.
	hub.chaos.forceResync('cursor_too_old');
	api.pushCompaction(id!, { ok: true });
	hub.flush();
	await expect(page.getByText('Контекст сжат автоматически')).toBeVisible();
	await expect(page.locator(RUNNING)).toHaveCount(0);
	await expect(indicator(page)).not.toHaveAttribute('data-compacting', 'true');
});
