import type { Page } from '@playwright/test';
import { expect, test, fillWorkdir } from './fixtures';
import { bootSecondary } from './secondary-fixture';
import { seedSchedule, seedScheduleRun } from '../../testing/fixtures/schedules';

const LIST = '[data-schedules-list]';

async function open(page: Page, patch?: (api: import('../../testing/fake-hub').FakeHubApi) => void) {
  const booted = await bootSecondary(page, '/settings?c=schedules', patch);
  await expect(page.locator('[data-schedules]')).toBeVisible({ timeout: 15_000 });
  return booted;
}

async function openNew(page: Page) {
  await page.getByRole('link', { name: 'Новое расписание' }).click();
  await expect(page.locator('[data-schedule-editor]')).toBeVisible();
}

test('new_session_schedule_previews_fires_and_saves_with_a_route', async ({ page }) => {
  const { api, ids } = await open(page);
  await openNew(page);
  const editor = page.locator('[data-schedule-editor]');

  await editor.getByRole('textbox', { name: 'Название' }).fill('Утренний разбор');
  await editor.getByRole('radiogroup', { name: 'Шаблон расписания' }).getByRole('radio', { name: 'По будням' }).click();
  await expect(editor.getByRole('textbox', { name: 'Cron (5 полей)' })).toHaveValue('0 9 * * 1-5');
  const preview = editor.locator('[data-role="schedule-preview"]');
  await preview.getByRole('button', { name: 'Показать' }).click();
  await expect(preview.locator('li')).toHaveCount(5);

  await editor.getByRole('combobox', { name: 'Машина' }).selectOption({ label: 'workstation' });
  await fillWorkdir(page, '/srv/canterlot');
  await editor.getByRole('combobox', { name: 'Тип задачи' }).selectOption('review');
  await editor.getByRole('textbox', { name: 'Подсказка' }).fill('Разбери коммиты за вчера');
  await editor.getByRole('button', { name: 'Сохранить' }).click();

  await expect(page.locator('[data-schedule-detail]')).toBeVisible();
  expect(api.schedules.size).toBe(1);
  const saved = [...api.schedules.values()][0]!;
  expect(saved.name).toBe('Утренний разбор');
  expect(saved.cron).toBe('0 9 * * 1-5');
  expect(saved.target).toMatchObject({ kind: 'new_session', machine_id: ids.machine, workdir: '/srv/canterlot', route: { task_type: 'review' } });
  expect(api.scheduleKeys).toHaveLength(1);
});

test('existing_session_schedule_saves_a_session_target', async ({ page }) => {
  const { api, ids } = await open(page);
  await openNew(page);
  const editor = page.locator('[data-schedule-editor]');

  await editor.getByRole('textbox', { name: 'Название' }).fill('Проверка сессии');
  await editor.getByRole('radiogroup', { name: 'Цель запуска' }).getByRole('radio', { name: 'Существующая сессия' }).click();
  await editor.getByRole('combobox', { name: 'Сессия' }).selectOption(ids.pinned);
  await editor.getByRole('textbox', { name: 'Подсказка' }).fill('Собери итоги');
  await editor.getByRole('button', { name: 'Сохранить' }).click();

  await expect(page.locator('[data-schedule-detail]')).toBeVisible();
  expect([...api.schedules.values()][0]!.target).toEqual({ kind: 'existing_session', conversation_id: ids.pinned });
});

test('pause_resume_and_live_updates_on_the_list', async ({ page }) => {
  const { api } = await open(page);
  const schedule = seedSchedule(api);
  await page.reload();
  const row = page.locator(`[data-schedule-row="${schedule.id}"]`);
  await expect(row).toBeVisible();
  const toggle = row.getByRole('switch', { name: `Включено: ${schedule.name}` });
  await expect(toggle).toBeChecked();

  await toggle.click();
  await expect(row.getByRole('switch', { name: `Включено: ${schedule.name}` })).not.toBeChecked();
  expect(api.schedules.get(schedule.id)!.enabled).toBe(false);

  await row.getByRole('switch', { name: `Включено: ${schedule.name}` }).click();
  await expect(row.getByRole('switch', { name: `Включено: ${schedule.name}` })).toBeChecked();
  expect(api.schedules.get(schedule.id)!.enabled).toBe(true);

  // Событие хаба меняет список без перезагрузки страницы.
  api.setScheduleEnabled(schedule.id, false, 'live-key');
  await expect(row.getByRole('switch', { name: `Включено: ${schedule.name}` })).not.toBeChecked();
});

test('run_now_keeps_one_run_when_the_response_is_lost', async ({ page }) => {
  const { api } = await open(page);
  const schedule = seedSchedule(api);
  await page.reload();
  await page.locator(LIST).getByRole('link', { name: 'Открыть' }).first().click();
  const detail = page.locator('[data-schedule-detail]');
  await expect(detail).toBeVisible();

  let aborted = false;
  await page.route('**/api/v1/schedules/*/run', async (route) => {
    if (!aborted) { aborted = true; await route.abort('failed'); return; }
    await route.fallback();
  });
  await detail.getByRole('button', { name: 'Запустить сейчас' }).click();
  await expect(detail.getByRole('status')).toContainText('Не удалось запустить');
  await detail.getByRole('button', { name: 'Запустить сейчас' }).click();
  await expect(detail.getByRole('status')).toContainText('Запуск принят');
  expect(api.scheduleRuns.get(schedule.id)).toHaveLength(1);

  await detail.getByRole('button', { name: 'Запустить в любом случае' }).click();
  await expect(detail.getByRole('status')).toContainText('Запущено без проверок');
  expect((api.scheduleRuns.get(schedule.id) ?? [])[0]!.detail).toMatchObject({ run_anyway: true });
});

test('history_shows_outcomes_delay_precheck_and_session_links', async ({ page }) => {
  const { api, ids } = await open(page);
  const schedule = seedSchedule(api);
  seedScheduleRun(api, schedule.id, { state: 'skipped_precheck', detail: { precheck_exit_code: 3, precheck_tail: 'нет прав\nдела не было' }, delay_ms: null });
  seedScheduleRun(api, schedule.id, { state: 'failed', detail: { reason: 'машина не ответила' }, session_id: ids.pinned, delay_ms: 5200 });
  await page.reload();
  await page.locator(LIST).getByRole('link', { name: 'Открыть' }).first().click();
  const history = page.locator('[data-schedule-history]');
  await expect(history.locator('[data-run-row]')).toHaveCount(2);
  await expect(history).toContainText('Пропущено: предпроверка');
  await expect(history).toContainText('Предпроверка: код 3');
  await expect(history).toContainText('дела не было');
  await expect(history).toContainText('Ошибка');
  await expect(history).toContainText('Задержка: 5 с');
  await expect(history.getByRole('link', { name: 'Открыть сессию' })).toHaveCount(1);
});

test('stale_revision_shows_the_standard_conflict_and_can_refresh', async ({ page }) => {
  const { api, ids } = await open(page);
  const schedule = seedSchedule(api, { target: { kind: 'new_session', machine_id: ids.machine, workdir: '/srv/canterlot', route: { task_type: 'review' } } });
  await page.reload();
  await page.locator(LIST).getByRole('link', { name: 'Открыть' }).first().click();
  await page.locator('[data-schedule-detail]').getByRole('button', { name: 'Изменить' }).click();
  const editor = page.locator('[data-schedule-editor]');
  await expect(editor).toBeVisible();

  await editor.getByRole('textbox', { name: 'Название' }).fill('Новое название');
  api.schedules.set(schedule.id, { ...schedule, revision: schedule.revision + 1 });
  await editor.getByRole('button', { name: 'Сохранить' }).click();
  await expect(editor.getByRole('alert')).toContainText('Расписание изменено с другого устройства');

  await editor.getByRole('button', { name: 'Обновить' }).click();
  await expect(editor.getByRole('alert')).toHaveCount(0);
  await expect(editor.getByRole('textbox', { name: 'Название' })).toHaveValue(schedule.name);
});
