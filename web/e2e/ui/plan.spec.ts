import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { REPORT_TEXT, seedPlan } from '../../testing/fixtures/plan';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function boot(page: Page, path: (seed: ReturnType<typeof seedPlan>) => string, ready = '[data-ready]') {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const seed = seedPlan(api);
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(path(seed));
	await expect(page.locator(ready).first()).toBeVisible({ timeout: 15_000 });
	return { hub, api, seed };
}

/** Вкладка «План» в листе делегирования (телефон) или в панели стола. */
function planTab(page: Page) {
	return page.locator('[data-role="plan-tab"]');
}

test('plan_strip_shows_review_and_blocked_and_opens_the_plan_tab', async ({ page }) => {
	const { api, seed } = await boot(page, (s) => `/c/${s.orchestrator}`);
	const strip = page.locator('[data-role="plan-strip"]');
	await expect(strip).toHaveText('План: 1 заблокирована · 1 на проверке');
	await expect(strip).toHaveAttribute('data-tone', 'warning');

	await strip.getByRole('button').click();
	await expect(planTab(page)).toBeVisible();
	await expect(page.getByRole('radio', { name: 'План' })).toBeChecked();

	// Группы идут в порядке внимания, закрытые свёрнуты.
	const groups = planTab(page).locator('[data-plan-group]');
	await expect(groups).toHaveCount(6);
	expect(await groups.evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-plan-group')))).toEqual([
		'in_progress',
		'in_review',
		'blocked',
		'waiting',
		'done',
		'cancelled'
	]);
	const done = planTab(page).locator('[data-plan-group="done"]');
	await expect(done.getByRole('button', { name: 'Готово · 2' })).toHaveAttribute('aria-expanded', 'false');
	await expect(done.locator('[data-plan-task]')).toHaveCount(0);
	await expect(planTab(page).locator('[data-plan-group="cancelled"] [data-plan-task]')).toHaveCount(0);
	await done.getByRole('button', { name: 'Готово · 2' }).click();
	await expect(done.locator('[data-plan-task]')).toHaveCount(2);

	// Строка задачи: номер и название, слово состояния, исполнитель с моделью и число коммитов.
	const first = planTab(page).locator('[data-plan-task="1"]');
	await expect(first.getByRole('button').first()).toContainText('#1 Таблица plan_tasks и миграция');
	await expect(first.locator('[data-role="plan-state"]')).toHaveText('в работе');
	await expect(first.locator('[data-role="plan-executor"]')).toContainText('Миграция plan_tasks');
	await expect(first.locator('[data-role="plan-executor"]')).toContainText('opus-5.5');
	await expect(first.locator('[data-role="plan-commits-count"]')).toContainText('1 коммит');
	await expect(first.getByRole('link', { name: 'Миграция plan_tasks' })).toHaveAttribute('href', `/c/${seed.executors.working}`);

	// Закрытие обеих задач снимает полосу; новая задача на проверке возвращает её без предупреждения.
	await page.keyboard.press('Escape');
	const [, review, blocked] = seed.tasks;
	api.pushPlanTask({ ...blocked!, state: 'done' });
	api.pushPlanTask({ ...review!, state: 'done' });
	await expect(strip).toHaveCount(0);
	api.pushPlanTask({ ...seed.tasks[0]!, state: 'in_review' });
	await expect(strip).toHaveText('План: 1 на проверке');
	await expect(strip).toHaveAttribute('data-tone', 'default');
});

test('expanded_task_lists_commits_and_report_opens_the_executor_chat_at_the_block', async ({ page }) => {
	const { api, seed } = await boot(page, (s) => `/c/${s.orchestrator}`);
	await page.locator('[data-role="plan-strip"] button').click();
	const task = planTab(page).locator('[data-plan-task="2"]');
	await task.getByRole('button', { name: /#2 Хранилище плана/ }).click();
	const body = task.locator('[data-role="plan-task-body"]');
	await expect(body).toBeVisible();
	await expect(body).toContainText('Сессия исполнителя закончила ход');
	await expect(body).toContainText('Снимок заменяет набор задач целиком');
	await expect(body).toContainText('3 коммита · 14 файлов изменено');
	await expect(body.locator('[data-role="plan-commits"] li')).toHaveCount(3);
	await expect(body.locator('[data-role="plan-commits"] li').first()).toHaveText('a1b2c3d Добавить хранилище плана с ревизиями');

	const blocked = planTab(page).locator('[data-plan-task="3"]');
	await blocked.getByRole('button', { name: /#3/ }).click();
	await expect(blocked.locator('[data-role="plan-blocker"]')).toContainText('Нужен общий ответ гейта толчка');
	await expect(blocked.locator('[data-role="plan-executor-state"]')).toContainText('упала');
	expect(api.planReads).toContain(seed.orchestrator);

	// Место отчёта приходит с планом: лента встаёт окном вокруг seq, без перебора страниц истории.
	const historyCalls: URL[] = [];
	page.on('request', (request) => {
		if (request.url().includes(`/conversations/${seed.executors.review}/history`)) historyCalls.push(new URL(request.url()));
	});
	await body.getByRole('link', { name: 'Отчёт' }).click();
	await expect(page).toHaveURL(new RegExp(`/c/${seed.executors.review}\\?block=${seed.reportBlockId}&seq=${seed.reportSeq}$`));
	const block = page.locator(`[data-block="${seed.reportBlockId}"]`);
	await expect(block).toHaveText(REPORT_TEXT, { timeout: 10_000 });
	await expect(block).toBeInViewport();
	await expect(page.locator('[data-row][data-jump]')).toContainText(REPORT_TEXT);
	expect(historyCalls.some((url) => url.searchParams.get('around') === String(seed.reportSeq))).toBe(true);
	expect(historyCalls.some((url) => url.searchParams.has('before'))).toBe(false);
});

/** Раскрыть задачу #2 в плане оркестратора и пойти по ссылке «Отчёт». */
async function openReport(page: Page) {
	await page.locator('[data-role="plan-strip"] button').click();
	const task = planTab(page).locator('[data-plan-task="2"]');
	await task.getByRole('button', { name: /#2 Хранилище плана/ }).click();
	// Ссылка появляется с подробностями задачи, они приходят отдельным запросом; их признак — коммиты.
	const body = task.locator('[data-role="plan-task-body"]');
	await expect(body.locator('[data-role="plan-commits"] li')).toHaveCount(3);
	await body.getByRole('link', { name: 'Отчёт' }).click();
	await expect(page).toHaveURL(/\?block=/);
}

const historyOf = (id: string) => (url: URL) => url.pathname.endsWith(`/conversations/${id}/history`);

test('report_jump_does_not_page_the_tail_window_it_is_about_to_replace', async ({ page }) => {
	const { seed } = await boot(page, (s) => `/c/${s.orchestrator}`);
	// Окно вокруг отчёта в пути: лента ещё держит хвост, а прыжок уже снял прижатие к низу.
	let release!: () => void;
	const held = new Promise<void>((resolve) => (release = resolve));
	let asked!: () => void;
	const around = new Promise<void>((resolve) => (asked = resolve));
	await page.route(historyOf(seed.executors.review), async (route) => {
		if (new URL(route.request().url()).searchParams.has('around')) {
			asked();
			await held;
		}
		await route.fallback();
	});
	const historyCalls: URL[] = [];
	page.on('request', (request) => {
		if (historyOf(seed.executors.review)(new URL(request.url()))) historyCalls.push(new URL(request.url()));
	});
	await openReport(page);
	await around;
	const feed = page.locator('.feed[data-loaded="100"]');
	await expect(feed).toBeVisible();
	// Верх хвоста на экране: и прокрутка к краю, и наблюдатель верхнего края просят старшую страницу.
	await feed.evaluate(async (el) => {
		const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));
		el.scrollTop = 600;
		await frame();
		el.scrollTop = 0;
		await frame();
		await frame();
	});
	release();
	const block = page.locator(`[data-block="${seed.reportBlockId}"]`);
	await expect(block).toHaveText(REPORT_TEXT, { timeout: 10_000 });
	await expect(block).toBeInViewport();
	await expect(page.locator('[data-row][data-jump]')).toContainText(REPORT_TEXT);
	await expect(page.locator('.feed')).not.toHaveAttribute('data-paging');
	expect(historyCalls.some((url) => url.searchParams.has('around'))).toBe(true);
	expect(historyCalls.filter((url) => url.searchParams.has('before')).map(String)).toEqual([]);
});

test('report_at_the_top_edge_pulls_the_older_page_once_placed_and_stays_in_place', async ({ page }) => {
	const { api, seed } = await boot(page, (s) => `/c/${s.orchestrator}`);
	// Окно вокруг отчёта начинается с него самого, а история старше есть: цель встаёт у верхнего края.
	await page.route(
		(url) => historyOf(seed.executors.review)(url) && url.searchParams.has('around'),
		async (route) => {
			const around = new URL(route.request().url()).searchParams.get('around')!;
			await route.fulfill({ json: await api.history(seed.executors.review, { around, limit: 7 }) });
		}
	);
	const older: URL[] = [];
	page.on('request', (request) => {
		const url = new URL(request.url());
		if (historyOf(seed.executors.review)(url) && url.searchParams.has('before')) older.push(url);
	});
	await openReport(page);
	const feed = page.locator('.feed');
	const block = page.locator(`[data-block="${seed.reportBlockId}"]`);
	await expect(block).toHaveText(REPORT_TEXT, { timeout: 10_000 });
	// Прыжок держал край окна, пока ставил цель; на месте старшая страница приходит один раз.
	await expect.poll(() => older.length).toBe(1);
	// Короткое окно касается и нижнего края: младшая страница ложится под цель и её не двигает.
	await expect(feed).not.toHaveAttribute('data-paging');
	await expect(block).toBeInViewport();
	const row = page.locator('[data-row]').filter({ has: block });
	const offset = await row.evaluate((node) => {
		const host = node.closest<HTMLElement>('.feed')!;
		return node.getBoundingClientRect().top - host.getBoundingClientRect().top - (Number.parseFloat(getComputedStyle(host).paddingTop) || 0);
	});
	expect(Math.abs(offset)).toBeLessThanOrEqual(2);
	expect(older[0]!.searchParams.get('before')).toBe(String(seed.reportSeq));
});

test('report_link_without_a_seq_still_finds_the_block_by_paging', async ({ page }) => {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const seed = seedPlan(api);
	const detail = api.planDetails.get(seed.tasks[1]!.id)!;
	api.planDetails.set(seed.tasks[1]!.id, { ...detail, report_seq: null });
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(`/c/${seed.orchestrator}`);
	await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
	await openReport(page);
	await expect(page).toHaveURL(new RegExp(`/c/${seed.executors.review}\\?block=${seed.reportBlockId}$`));
	const block = page.locator(`[data-block="${seed.reportBlockId}"]`);
	await expect(block).toHaveText(REPORT_TEXT, { timeout: 10_000 });
	await expect(block).toBeInViewport();
});

test('deleted_orchestrator_drops_its_plan_and_deleted_executor_reads_as_removed', async ({ page }) => {
	const { api, seed } = await boot(page, () => '/sessions', '[data-list-ready]');
	const badge = page.locator(`[data-session="${seed.orchestrator}"] [data-role="plan-badge"]`);
	await expect(badge).toContainText('План · 2/6');

	await page.getByRole('button', { name: 'Ещё' }).click();
	await page.getByRole('menuitem', { name: 'Планы' }).click();
	await expect(page).toHaveURL(/\/plan$/);
	const entry = page.locator(`[data-plan-orchestrator="${seed.orchestrator}"]`);
	await expect(entry).toContainText('Доска плана');
	await expect(entry).toContainText('1 в работе · 1 на проверке · 1 заблокирована · 1 ждёт · 2 готово · 1 отменена');

	await entry.getByRole('link').click();
	await expect(page).toHaveURL(new RegExp(`/plan/${seed.orchestrator}$`));
	const task = page.locator('[data-plan-task="3"]');
	await task.getByRole('button', { name: /#3/ }).click();
	await expect(task.locator('[data-role="plan-executor"]')).toContainText('Отбор подписок');
	api.deleteConversation(seed.executors.failed);
	await expect(task.locator('[data-role="plan-executor"]')).toHaveText('исполнитель удалён');
	await expect(task.locator('[data-role="plan-executor-state"]')).toHaveText('исполнитель удалён');

	api.deleteConversation(seed.orchestrator);
	await expect(page.locator('[data-role="plan-gone"]')).toBeVisible();
	await expect(page.locator('[data-plan-task]')).toHaveCount(0);
	await page.goto('/plan');
	await expect(page.locator('[data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
	await expect(page.locator('[data-plan-orchestrator]')).toHaveCount(0);
	await expect(page.getByText('Планов пока нет')).toBeVisible();
});

test('snapshot_replaces_the_plan_instead_of_merging', async ({ page }) => {
	const { hub, api, seed } = await boot(page, (s) => `/plan/${s.orchestrator}`, '[data-secondary-ready]');
	await expect(page.locator('[data-plan-task="1"]')).toBeVisible();
	await expect(page.locator('[data-plan-group="in_review"]')).toBeVisible();
	// Хаб потерял две задачи без событий: новый снимок убирает их и у клиента.
	api.planTasks.delete(seed.tasks[0]!.id);
	api.planTasks.delete(seed.tasks[1]!.id);
	hub.chaos.forceResync('cursor_too_old');
	await expect(page.locator('[data-plan-task="1"]')).toHaveCount(0);
	await expect(page.locator('[data-plan-group="in_review"]')).toHaveCount(0);
	await expect(page.locator('[data-plan-task="3"]')).toBeVisible();
});

test('delegation_panel_switches_between_tree_and_plan', async ({ page }) => {
	const { seed } = await boot(page, (s) => `/c/${s.orchestrator}`);
	await page.getByRole('button', { name: /^Делегирование/ }).first().click();
	const tree = page.getByRole('radio', { name: 'Делегирование' });
	await expect(tree).toBeChecked();
	await expect(page.locator(`.tree a[href="/c/${seed.executors.working}"]`)).toBeVisible();
	await page.locator('[data-role="delegation-tabs"] label', { hasText: 'План' }).click();
	await expect(planTab(page).locator('[data-plan-group="in_progress"]')).toBeVisible();
	await page.locator('[data-role="delegation-tabs"] label', { hasText: 'Делегирование' }).click();
	await expect(planTab(page)).toHaveCount(0);
});
