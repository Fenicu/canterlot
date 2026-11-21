import type { Locator, Page } from '@playwright/test';
import { changeJob } from '../../testing/fake-hub';
import { OUTPUTS_DIR, seedRunnerJobCards, type RunnerJobsSeed } from '../../testing/fixtures/runner-job-cards';
import { expect, test } from './fixtures';
import { cardOf, reveal } from './job-feed';
import { bootSecondary } from './secondary-fixture';

/** Лента с вызовами `job_run machine:auto` и экран машин на одном фейковом хабе. */
async function boot(page: Page, path?: string) {
  let seed!: RunnerJobsSeed;
  const booted = await bootSecondary(page, path ?? '/machines', (api) => { seed = seedRunnerJobCards(api); });
  if (!path) {
    await page.goto(`/c/${seed.id}`);
    await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('.feed [data-row]').first()).toBeVisible({ timeout: 15_000 });
  }
  return { ...booted, seed };
}

const sheet = (page: Page) => page.locator('[data-job-sheet]');
const state = (card: Locator) => card.locator('[data-role="card-state"]');
const meta = (card: Locator) => card.locator('[data-role="job-meta"]');
const pen = (card: Locator) => card.locator('[data-role="job-stage"] .write');
const penAnimations = (card: Locator) => card.locator('[data-role="job-stage"] .write i').evaluateAll((nodes) => nodes.map((node) => getComputedStyle(node).animationName));

async function openMachine(page: Page, name: string) {
  await page.locator('main').getByRole('radio', { name, exact: true }).check();
  await expect(page.locator('main').getByRole('heading', { name, exact: true })).toBeVisible();
}

test('a_runner_job_card_follows_the_job_from_waiting_to_collected_outputs', async ({ page }) => {
  const { hub, api, seed } = await boot(page, undefined);
  const card = await reveal(page, cardOf(page, seed.blocks.flow!));
  await expect(card).toContainText('Запустила команду: Полная проверка');
  await expect(card).not.toContainText('на auto');
  await expect(state(card)).toHaveText('ждёт раннер');
  await expect(meta(card)).toContainText('project @ 1a2b3c4');
  await expect(pen(card)).toHaveCount(0);

  changeJob(hub, api, seed.flow.id, { state: 'staging', machine_id: seed.runner.id, machine_name: seed.runner.name });
  await expect(state(card)).toHaveText('подвозит код');
  await expect(meta(card)).toContainText('runner-a');
  await expect(meta(card)).toContainText('project @ 1a2b3c4');
  await expect(pen(card)).toHaveCount(1);
  expect((await penAnimations(card)).every((name) => name !== 'none')).toBe(true);

  changeJob(hub, api, seed.flow.id, { state: 'running', workdir: '/srv/runner/jobs/work/flow', started_at: new Date().toISOString() });
  await expect(state(card)).toHaveText('идёт');
  await expect(meta(card)).toContainText('/srv/runner/jobs/work/flow');
  await expect(pen(card)).toHaveCount(0);

  changeJob(hub, api, seed.flow.id, { state: 'collecting' });
  await expect(state(card)).toHaveText('собирает результаты');
  await expect(pen(card)).toHaveCount(1);
  await expect(card.locator('[data-role="job-outputs"]')).toHaveCount(0);

  changeJob(hub, api, seed.flow.id, { state: 'succeeded', exit_code: 0, duration_ms: 75_000, outputs_dir: OUTPUTS_DIR }, 'all steps passed\n');
  await expect(state(card)).toHaveText('готово');
  await expect(pen(card)).toHaveCount(0);
  await expect(card.locator('[data-role="job-outputs-dir"]')).toHaveText(OUTPUTS_DIR);
  await expect(card.locator('[data-role="job-outputs"]').getByRole('button', { name: 'Скопировать путь к результатам' })).toBeVisible();
  await expect(card.locator('[data-role="job-outputs-note"]')).toHaveCount(0);
});

test('a_failed_staging_says_why_and_a_collected_job_shows_its_outputs_dir', async ({ page }) => {
  const { seed } = await boot(page, undefined);
  const failed = await reveal(page, cardOf(page, seed.blocks.stagingFailed!));
  await expect(failed.locator('[data-role="card-state"]')).toHaveText('ошибка');
  await expect(meta(failed)).toContainText('runner-a');
  await expect(meta(failed)).toContainText('не удалось подвезти код: git push failed');
  await expect(failed.locator('[data-role="job-outputs"]')).toHaveCount(0);
  const collected = await reveal(page, cardOf(page, seed.blocks.collected!));
  await expect(collected.locator('[data-role="job-outputs-dir"]')).toHaveText(OUTPUTS_DIR);
});

test('outputs_not_collected_come_with_the_note_and_no_path', async ({ page }) => {
  const { seed } = await boot(page, undefined);
  const card = await reveal(page, cardOf(page, seed.blocks.notCollected!));
  await expect(card.locator('[data-role="card-state"]')).toHaveText('готово');
  await expect(card.locator('[data-role="job-outputs-note"]')).toHaveText('Результаты не собраны: collection timed out');
  await expect(card.locator('[data-role="job-outputs-dir"]')).toHaveCount(0);
});

test('without_motion_the_pen_of_a_stage_stands_written', async ({ page }) => {
  const { hub, api, seed } = await boot(page, undefined);
  const card = await reveal(page, cardOf(page, seed.blocks.flow!));
  await page.emulateMedia({ reducedMotion: 'reduce' });
  changeJob(hub, api, seed.flow.id, { state: 'staging', machine_id: seed.runner.id, machine_name: seed.runner.name });
  await expect(state(card)).toHaveText('подвозит код');
  await expect(pen(card)).toHaveCount(1);
  expect(await penAnimations(card)).toEqual(['none', 'none', 'none']);
  changeJob(hub, api, seed.flow.id, { state: 'collecting', workdir: '/srv/runner/jobs/work/flow' });
  await expect(state(card)).toHaveText('собирает результаты');
  expect(await penAnimations(card)).toEqual(['none', 'none', 'none']);
});

test('the_runners_list_card_shows_name_availability_and_slots', async ({ page }) => {
  const { seed } = await boot(page, undefined);
  const card = await reveal(page, cardOf(page, seed.blocks.runners!));
  await expect(card).toContainText('Смотрела раннеры: 2 · 2 на связи · ждут 1');
  const own = card.locator('[data-runner-row="carrier-a"]');
  await expect(own.locator('[data-role="runner-slots"]')).toContainText('0/2');
  const row = card.locator('[data-runner-row="runner-a"]');
  await expect(row.locator('[data-role="runner-state"]')).toHaveText('на связи');
  await expect(row.locator('[data-role="runner-slots"]')).toContainText('1/4');
});

test('the_sheet_names_the_runner_the_source_and_the_outputs', async ({ page }) => {
  const { hub, api, seed } = await boot(page, undefined);
  await page.goto(`/machines?job=${seed.flow.id}`);
  const panel = sheet(page);
  await expect(panel.locator('[data-role="job-state"]')).toHaveText('ждёт раннер');
  await expect(panel.locator('[data-role="job-machine"]')).toHaveText('ждёт раннер');
  await expect(panel.locator('[data-role="job-source"]')).toHaveText('project @ 1a2b3c4');
  await expect(panel.locator('[data-role="job-labels"]')).toHaveText('build-host');
  await expect(panel.getByRole('button', { name: 'Отменить', exact: true })).toBeVisible();
  changeJob(hub, api, seed.flow.id, { state: 'staging', machine_id: seed.runner.id, machine_name: seed.runner.name });
  await expect(panel.locator('[data-role="job-state"]')).toHaveText('подвозит код');
  await expect(panel.locator('[data-role="job-machine"]')).toHaveText('runner-a');
  await expect(panel.locator('[data-role="job-stage"] .write')).toHaveCount(1);
  changeJob(hub, api, seed.flow.id, { state: 'succeeded', exit_code: 0, duration_ms: 1000, outputs_dir: OUTPUTS_DIR });
  await expect(panel.locator('[data-role="job-state"]')).toHaveText('готово');
  await expect(panel.locator('[data-role="job-outputs-dir"]')).toHaveText(OUTPUTS_DIR);
  await expect(panel.locator('[data-role="job-stage"]')).toHaveCount(0);
  await page.goto(`/machines?job=${seed.notCollected.id}`);
  await expect(sheet(page).locator('[data-role="job-outputs-note"]')).toHaveText('Результаты не собраны: collection timed out');
  await page.goto(`/machines?job=${seed.stagingFailed.id}`);
  await expect(sheet(page).locator('[data-role="job-reason"]')).toHaveText('не удалось подвезти код: git push failed');
});

test('the_runner_card_lists_its_jobs_and_the_carrier_does_not_double_list_them', async ({ page }) => {
  const { hub, api, seed } = await boot(page, '/machines');
  await openMachine(page, 'carrier-a');
  const section = page.locator('[data-machine-jobs]');
  // Несущая показывает ничьи задачи: раннер ещё не выбран.
  const waiting = section.locator(`[data-job="${seed.flow.id}"]`);
  await expect(waiting).toContainText('ждёт раннер');
  await expect(waiting).toContainText('project @ 1a2b3c4');
  await expect(waiting).toContainText('метки: build-host');
  // Задачи её раннеров лежат на карточке раннера.
  await expect(section.locator(`[data-job="${seed.collected.id}"]`)).toHaveCount(0);
  await expect(section.locator(`[data-job="${seed.stagingFailed.id}"]`)).toHaveCount(0);

  await openMachine(page, 'runner-a');
  await expect(section.locator(`[data-job="${seed.collected.id}"]`)).toContainText('готово');
  await expect(section.locator(`[data-job="${seed.stagingFailed.id}"]`)).toContainText('ошибка');
  await expect(section.locator(`[data-job="${seed.flow.id}"]`)).toHaveCount(0);

  // Раннер выбран: задача уходит с несущей на карточку раннера и идёт по состояниям.
  changeJob(hub, api, seed.flow.id, { state: 'staging', machine_id: seed.runner.id, machine_name: seed.runner.name });
  const row = section.locator(`[data-job="${seed.flow.id}"]`);
  await expect(row).toContainText('подвозит код');
  await expect(row.locator('[data-job-mark] .write')).toHaveCount(1);
  changeJob(hub, api, seed.flow.id, { state: 'collecting' });
  await expect(row).toContainText('собирает результаты');
  await expect(row.locator('[data-job-mark] .write')).toHaveCount(1);
  changeJob(hub, api, seed.flow.id, { state: 'succeeded', exit_code: 0, duration_ms: 5000, outputs_dir: OUTPUTS_DIR });
  await expect(row).toContainText('готово');
  await expect(row.locator('.write')).toHaveCount(0);
  await openMachine(page, 'carrier-a');
  await expect(section.locator(`[data-job="${seed.flow.id}"]`)).toHaveCount(0);
  await expect(section.getByRole('heading', { name: 'Задачи', exact: true })).toBeVisible();
});

test('a_job_waiting_for_a_runner_opens_from_the_carrier_card', async ({ page }) => {
  const { seed } = await boot(page, '/machines');
  await openMachine(page, 'carrier-a');
  await page.locator(`[data-machine-jobs] [data-job="${seed.flow.id}"]`).click();
  await expect(sheet(page).locator('[data-role="job-state"]')).toHaveText('ждёт раннер');
});

test('runner_job_phone_layout_does_not_overflow', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 800 });
  const { seed } = await boot(page, undefined);
  const card = await reveal(page, cardOf(page, seed.blocks.collected!));
  const fits = (locator: Locator) => locator.evaluate((node) => { const box = node.getBoundingClientRect(); return box.right <= document.documentElement.clientWidth + 1 && box.left >= -1; });
  expect(await fits(card)).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
  await page.goto(`/machines?job=${seed.collected.id}`);
  await expect(sheet(page).locator('[data-role="job-outputs-dir"]')).toBeVisible();
  expect(await sheet(page).evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
});
