import type { Locator, Page } from '@playwright/test';
import { changeJob, seedJob } from '../../testing/fake-hub';
import { DONE_LOG, LONG_TITLE, seedJobCards, type JobCardsSeed } from '../../testing/fixtures/job-cards';
import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';

/** Лента с вызовами `job_*` и экран машин на одном фейковом хабе. */
async function boot(page: Page, path?: string) {
  let seed!: JobCardsSeed;
  const booted = await bootSecondary(page, path ?? '/machines', (api) => { seed = seedJobCards(api); });
  if (!path) {
    // Без пути: лента оркестратора с вызовами `job_*`.
    await page.goto(`/c/${seed.id}`);
    await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('.feed [data-row]').first()).toBeVisible({ timeout: 15_000 });
  }
  return { ...booted, seed };
}

const feed = (page: Page) => page.locator('.feed');
const sheet = (page: Page) => page.locator('[data-job-sheet]');

/** Лента виртуальная: строка далеко от низа не смонтирована, пока до неё не докрутили. */
async function reveal(page: Page, target: Locator): Promise<Locator> {
  const settle = () => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  await feed(page).evaluate((node) => {
    node.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 }));
    node.scrollTop = 0;
  });
  await settle();
  for (let step = 0; step < 40; step++) {
    if ((await target.count()) > 0) {
      try {
        await target.first().scrollIntoViewIfNeeded({ timeout: 2_000 });
        await settle();
        if ((await target.count()) > 0) return target.first();
      } catch {
        // Строку сняли с окна, пока до неё крутили: ищем дальше.
      }
    }
    await feed(page).evaluate((node) => { node.scrollTop += node.clientHeight * 0.6; });
    await settle();
  }
  await expect(target.first()).toBeAttached();
  return target.first();
}

const cardOf = (page: Page, blockId: string) => page.locator(`[data-key="block:${blockId}"] .feed-card`).first();

test('a_job_run_card_opens_the_sheet_with_the_tail_and_the_exit_code', async ({ page }) => {
  const { seed } = await boot(page, undefined);
  const card = await reveal(page, cardOf(page, seed.blocks.jobDone!));
  await expect(card).toContainText('Запустила команду на machine-b: Сборка веба');
  await expect(card.locator('[data-role="card-state"]')).toHaveText('готово');
  await expect(card.locator('[data-role="job-meta"]')).toContainText('код 0');
  await expect(card.locator('[data-role="job-meta"]')).toContainText('4 с');
  // Раскрытие показывает хвост вывода, ссылка ведёт в лист задачи.
  await card.getByRole('button', { name: /Показать вывод/ }).click();
  await expect(card).toContainText('built 12 files');
  await card.locator('[data-role="job-open"]').click();
  await expect(page).toHaveURL(new RegExp(`/machines\\?job=${seed.done.id}`));
  await expect(sheet(page)).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Сборка веба' })).toBeVisible();
  await expect(sheet(page).locator('[data-role="job-machine"]')).toHaveText('machine-b');
  await expect(sheet(page).locator('[data-role="job-workdir"]')).toHaveText('/h/u/proj');
  await expect(sheet(page).locator('[data-role="job-state"]')).toHaveText('готово');
  await expect(sheet(page).locator('[data-role="job-exit"]')).toHaveText('0');
  await expect(sheet(page).locator('[data-role="job-duration"]')).toHaveText('4 с');
  await expect(sheet(page).locator('[data-role="job-log"]')).toHaveText(DONE_LOG.trimEnd());
});

test('a_running_card_follows_job_changed_and_ends_with_an_exit_code', async ({ page }) => {
  const { hub, api, seed } = await boot(page, undefined);
  const card = await reveal(page, cardOf(page, seed.blocks.jobRunning!));
  await expect(card.locator('[data-role="card-state"]')).toHaveText('идёт');
  changeJob(hub, api, seed.running.id, { state: 'failed', exit_code: 3, duration_ms: 9000 }, 'running 40 tests\nnot ok 7 - parser\n');
  await expect(card.locator('[data-role="card-state"]')).toHaveText('код 3');
  await expect(card.locator('[data-role="job-meta"]')).toContainText('9 с');
});

test('a_job_can_be_canceled_from_the_sheet_after_a_confirmation', async ({ page }) => {
  const { hub, api, seed } = await boot(page, undefined);
  await page.goto(`/machines?job=${seed.running.id}`);
  await expect(sheet(page).locator('[data-role="job-state"]')).toHaveText('идёт');
  await expect(sheet(page).locator('[data-role="job-log"]')).toContainText('running 40 tests');
  // Дальше задача пишет вывод: хвост в листе обновляется без перезагрузки.
  changeJob(hub, api, seed.running.id, {}, 'running 40 tests\nok 1 - parser\nok 2 - lexer\n');
  await expect(sheet(page).locator('[data-role="job-log"]')).toContainText('ok 2 - lexer');
  await sheet(page).getByRole('button', { name: 'Отменить', exact: true }).click();
  const confirm = page.getByRole('dialog', { name: 'Отменить задачу?' });
  await expect(confirm).toBeVisible();
  // Отказ от отмены ничего не посылает хабу.
  await confirm.getByRole('button', { name: 'Отмена' }).click();
  await expect(confirm).toBeHidden();
  expect(api.jobCancelCalls).toEqual([]);
  await sheet(page).getByRole('button', { name: 'Отменить', exact: true }).click();
  await page.getByRole('dialog', { name: 'Отменить задачу?' }).getByRole('button', { name: 'Отменить задачу' }).click();
  await expect.poll(() => api.jobCancelCalls.length).toBe(1);
  expect(api.jobCancelCalls[0]!.id).toBe(seed.running.id);
  await expect(sheet(page).locator('[data-role="job-state"]')).toHaveText('отменяется');
  // Воркер подтвердил: событие хаба закрывает задачу, кнопки отмены больше нет.
  changeJob(hub, api, seed.running.id, { state: 'canceled', end_reason: 'canceled_by_owner', duration_ms: 3000 });
  await expect(sheet(page).locator('[data-role="job-state"]')).toHaveText('отменена');
  await expect(sheet(page).locator('[data-role="job-reason"]')).toHaveText('отменена владельцем');
  await expect(sheet(page).getByRole('button', { name: 'Отменить', exact: true })).toHaveCount(0);
});

test('the_sheet_follows_a_running_job_to_its_end', async ({ page }) => {
  const { hub, api, seed } = await boot(page, undefined);
  await page.goto(`/machines?job=${seed.running.id}`);
  await expect(sheet(page).locator('[data-role="job-state"]')).toHaveText('идёт');
  changeJob(hub, api, seed.running.id, { state: 'succeeded', exit_code: 0, duration_ms: 12_000 }, 'running 40 tests\nall 40 passed\n');
  await expect(sheet(page).locator('[data-role="job-state"]')).toHaveText('готово');
  await expect(sheet(page).locator('[data-role="job-exit"]')).toHaveText('0');
  await expect(sheet(page).locator('[data-role="job-duration"]')).toHaveText('12 с');
  await expect(sheet(page).locator('[data-role="job-log"]')).toContainText('all 40 passed');
  await expect(sheet(page).getByRole('button', { name: 'Отменить', exact: true })).toHaveCount(0);
  // После конца экран читает журнал целиком, с начала.
  await expect.poll(() => api.jobLogCalls.some((call) => call.offset === '0')).toBe(true);
});

test('an_expired_full_log_says_so_and_keeps_the_tail', async ({ page }) => {
  const { api, seed } = await boot(page, undefined);
  api.jobLogsExpired.add(seed.done.id);
  await page.goto(`/machines?job=${seed.done.id}`);
  await expect(sheet(page).locator('[data-role="job-expired"]')).toHaveText('Полный журнал удалён по сроку, остался хвост');
  await expect(sheet(page).locator('[data-role="job-log"]')).toContainText('built 12 files');
});

test('a_remote_spawn_shows_the_machine_on_the_delegate_card', async ({ page }) => {
  const { seed } = await boot(page, undefined);
  const card = await reveal(page, cardOf(page, seed.blocks.spawn!));
  await expect(card.locator('[data-role="spawn-meta"]')).toContainText('машина machine-b');
});

test('a_refused_job_run_reads_in_words', async ({ page }) => {
  const { seed } = await boot(page, undefined);
  const card = await reveal(page, cardOf(page, seed.blocks.jobRefused!));
  await expect(card).toContainText('Машина не принимает работу с других машин');
});

test('the_machine_jobs_section_lists_running_and_recent_jobs_and_opens_the_sheet', async ({ page }) => {
  const { seed } = await boot(page, '/machines');
  await page.locator('main').getByRole('radio', { name: 'machine-b', exact: true }).check();
  const section = page.locator('[data-machine-jobs]');
  await expect(section.getByRole('heading', { name: 'Задачи', exact: true })).toBeVisible();
  await expect(section.getByRole('heading', { name: 'Идут сейчас' })).toBeVisible();
  const running = section.locator(`[data-job="${seed.running.id}"]`);
  await expect(running).toContainText('Прогон тестов');
  await expect(running).toContainText('идёт');
  await expect(section.locator(`[data-job="${seed.done.id}"]`)).toContainText('готово');
  await running.click();
  await expect(sheet(page)).toBeVisible();
  await expect(sheet(page).locator('[data-role="job-state"]')).toHaveText('идёт');
});

test('running_jobs_in_the_machine_section_carry_the_pen_and_finished_ones_do_not', async ({ page }) => {
  const { seed, api, hub } = await boot(page, '/machines');
  await page.locator('main').getByRole('radio', { name: 'machine-b', exact: true }).check();
  const section = page.locator('[data-machine-jobs]');
  const running = section.locator(`[data-job="${seed.running.id}"]`);
  await expect(running).toContainText('идёт');
  await expect(running.locator('[data-job-mark] .write')).toHaveCount(1);
  expect(await running.locator('.write i').first().evaluate((node) => getComputedStyle(node).animationName)).not.toBe('none');
  await expect(section.locator(`[data-job="${seed.done.id}"]`).locator('.write')).toHaveCount(0);
  // Задача кончилась: перо уходит вместе со словом «идёт».
  changeJob(hub, api, seed.running.id, { state: 'succeeded', exit_code: 0, duration_ms: 4000 });
  await expect(running.locator('.write')).toHaveCount(0);
  // Без движения перо стоит написанным.
  await page.emulateMedia({ reducedMotion: 'reduce' });
  const other = seedJob(api, seed.machine, { state: 'running', title: 'Сборка образа', shell: 'make image', created_at: new Date().toISOString() });
  await page.goto('/machines');
  await page.locator('main').getByRole('radio', { name: 'machine-b', exact: true }).check();
  const row = section.locator(`[data-job="${other.id}"]`);
  await expect(row.locator('.write')).toHaveCount(1);
  expect(await row.locator('.write i').evaluateAll((nodes) => nodes.map((node) => getComputedStyle(node).animationName))).toEqual(['none', 'none', 'none']);
});

test('a_running_job_on_a_machine_without_a_link_does_not_write', async ({ page }) => {
  const { seed, api } = await boot(page, '/machines');
  const stale = seedJob(api, seed.machine, { state: 'running', title: 'Зависший прогон', shell: 'sleep 99', machine_offline: true, created_at: new Date().toISOString() });
  await page.goto('/machines');
  await page.locator('main').getByRole('radio', { name: 'machine-b', exact: true }).check();
  const row = page.locator(`[data-machine-jobs] [data-job="${stale.id}"]`);
  await expect(row).toContainText('машина не на связи');
  await expect(row.locator('.write')).toHaveCount(0);
});

test('the_machine_jobs_section_is_empty_for_a_machine_without_jobs_and_shows_a_lost_job_in_words', async ({ page }) => {
  const { api, seed } = await boot(page, '/machines');
  await expect(page.locator('main').getByRole('heading', { name: 'workstation' })).toBeVisible();
  await expect(page.locator('[data-machine-jobs] [data-role="jobs-empty"]')).toHaveText('На этой машине нет задач');
  const lost = seedJob(api, seed.machine, { state: 'lost', title: 'Выкладка', shell: 'make deploy', created_at: new Date().toISOString() });
  await page.goto('/machines');
  await page.locator('main').getByRole('radio', { name: 'machine-b', exact: true }).check();
  const row = page.locator(`[data-machine-jobs] [data-job="${lost.id}"]`);
  await expect(row).toContainText('потеряна');
  await expect(row).toContainText('связь с машиной потеряна');
});

test('long_titles_and_commands_do_not_overflow_on_the_phone', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 800 });
  const { seed } = await boot(page, undefined);
  const card = await reveal(page, cardOf(page, seed.blocks.jobLong!));
  await expect(card).toContainText(LONG_TITLE.slice(0, 20));
  await card.getByRole('button', { name: /Показать вывод/ }).click();
  const fits = (locator: Locator) => locator.evaluate((node) => { const box = node.getBoundingClientRect(); return box.right <= document.documentElement.clientWidth + 1 && box.left >= -1; });
  expect(await fits(card)).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
  await page.goto(`/machines?job=${seed.long.id}`);
  await expect(sheet(page)).toBeVisible();
  await expect(sheet(page).locator('[data-role="job-state"]')).toHaveText('готово');
  expect(await sheet(page).evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
  await page.keyboard.press('Escape');
  await expect(sheet(page)).toBeHidden();
  await page.locator('main').getByRole('radio', { name: 'machine-b', exact: true }).check();
  const row = page.locator(`[data-machine-jobs] [data-job="${seed.long.id}"]`);
  await expect(row).toBeVisible();
  expect(await fits(row)).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});
