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

test('new_schedule_defaults_to_the_owner_timezone_and_keeps_saved_zones', async ({ page }) => {
  const { api } = await open(page, (hub) => { hub.scheduleOwnerTimezone = 'Asia/Yekaterinburg'; });
  await openNew(page);
  const editor = page.locator('[data-schedule-editor]');
  await expect(editor.getByRole('textbox', { name: 'Часовой пояс' })).toHaveValue('Asia/Yekaterinburg');
  await expect(editor.locator('[data-role="schedule-preview"]')).toContainText('Asia/Yekaterinburg');

  await editor.getByRole('textbox', { name: 'Название' }).fill('Поясной');
  await editor.getByRole('combobox', { name: 'Машина' }).selectOption({ label: 'workstation' });
  await fillWorkdir(page, '/srv/canterlot');
  await editor.getByRole('combobox', { name: 'Тип задачи' }).selectOption('review');
  await editor.getByRole('textbox', { name: 'Подсказка' }).fill('Проверка пояса');
  await editor.getByRole('button', { name: 'Сохранить' }).click();
  await expect(page.locator('[data-schedule-detail]')).toBeVisible();
  expect([...api.schedules.values()][0]!.timezone).toBe('Asia/Yekaterinburg');
  await expect(page.locator('[data-schedule-detail]')).toContainText('(пояс Asia/Yekaterinburg)');

  await page.getByRole('button', { name: 'Изменить' }).click();
  await expect(page.locator('[data-schedule-editor]').getByRole('textbox', { name: 'Часовой пояс' })).toHaveValue('Asia/Yekaterinburg');
});

test('existing_session_schedule_saves_a_session_target', async ({ page }) => {
  const { api, ids } = await open(page);
  await openNew(page);
  const editor = page.locator('[data-schedule-editor]');

  await editor.getByRole('textbox', { name: 'Название' }).fill('Проверка сессии');
  await editor.getByRole('radiogroup', { name: 'Цель запуска' }).getByRole('radio', { name: 'Сообщение в сессию' }).click();
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

test('session_report_schedule_saves_a_report_target_with_a_hint', async ({ page }) => {
  const { api, ids } = await open(page);
  await openNew(page);
  const editor = page.locator('[data-schedule-editor]');

  await editor.getByRole('textbox', { name: 'Название' }).fill('Стендап');
  const target = editor.getByRole('radiogroup', { name: 'Цель запуска' });
  await expect(target.locator('label')).toHaveText(['Новая сессия', 'Сообщение в сессию', 'Отчёт из сессии']);
  await target.getByRole('radio', { name: 'Отчёт из сессии' }).click();
  await expect(editor.getByRole('combobox', { name: 'Машина' })).toHaveCount(0);
  await editor.getByRole('combobox', { name: 'Сессия' }).selectOption(ids.pinned);
  await editor.getByRole('textbox', { name: 'Подсказка' }).fill('Вчера сделано / Сегодня в планах / Блокеры');
  await expect(editor).toContainText('Копия сессии только читает её историю и отвечает на этот промпт; ответ придёт карточкой в ленту сессии.');
  await expect(editor.getByRole('textbox', { name: 'Заголовок нового сеанса' })).toHaveCount(0);
  await editor.getByRole('textbox', { name: 'Заголовок карточки' }).fill('{name}');
  await editor.getByRole('button', { name: 'Сохранить' }).click();

  const detail = page.locator('[data-schedule-detail]');
  await expect(detail).toBeVisible();
  const saved = [...api.schedules.values()][0]!;
  expect(saved.target).toEqual({ kind: 'session_report', conversation_id: ids.pinned });
  expect(saved.title_template).toBe('{name}');
  await expect(detail).toContainText('Отчёт из сессии');
});

test('session_report_picker_leaves_out_side_copies', async ({ page }) => {
  const { api, ids } = await open(page, (hub) => {
    const row = [...hub.list.values()][0]!;
    hub.list.set('00000000-0000-4000-8000-0000000000f1', { ...row, id: '00000000-0000-4000-8000-0000000000f1', title: 'Копия-читатель', side_of: row.id } as typeof row);
  });
  await openNew(page);
  const editor = page.locator('[data-schedule-editor]');
  await editor.getByRole('radiogroup', { name: 'Цель запуска' }).getByRole('radio', { name: 'Отчёт из сессии' }).click();
  await expect(editor.getByRole('combobox', { name: 'Сессия' }).locator('option', { hasText: 'Копия-читатель' })).toHaveCount(0);
  await expect(editor.getByRole('combobox', { name: 'Сессия' }).locator(`option[value="${ids.pinned}"]`)).toHaveCount(1);
  expect(api.schedules.size).toBe(0);
});

test('session_report_target_error_from_the_hub_shows_at_the_target_field', async ({ page }) => {
  const { api, ids } = await open(page, (hub) => { hub.scheduleTargetError = 'Машина сессии не умеет читать историю копией'; });
  await openNew(page);
  const editor = page.locator('[data-schedule-editor]');
  await editor.getByRole('textbox', { name: 'Название' }).fill('Стендап');
  await editor.getByRole('radiogroup', { name: 'Цель запуска' }).getByRole('radio', { name: 'Отчёт из сессии' }).click();
  await editor.getByRole('combobox', { name: 'Сессия' }).selectOption(ids.pinned);
  await editor.getByRole('textbox', { name: 'Подсказка' }).fill('Итоги');
  await editor.getByRole('button', { name: 'Сохранить' }).click();

  const select = editor.getByRole('combobox', { name: 'Сессия' });
  await expect(select).toHaveAttribute('aria-invalid', 'true');
  await expect(select).toHaveAccessibleDescription('Машина сессии не умеет читать историю копией');
  await expect(editor).toBeVisible();
  expect(api.schedules.size).toBe(0);
  // Ошибка стоит у поля цели, а не в общей строке над формой.
  await expect(editor.locator('> .error')).toHaveCount(0);
  await select.selectOption('');
  await expect(select).not.toHaveAttribute('aria-invalid', 'true');
});

test('list_and_detail_show_the_target_kind_and_session_name', async ({ page }) => {
  const { api, ids } = await open(page);
  const report = seedSchedule(api, { name: 'Стендап', target: { kind: 'session_report', conversation_id: ids.pinned } });
  const message = seedSchedule(api, { name: 'Пинг', target: { kind: 'existing_session', conversation_id: ids.pinned } });
  const created = seedSchedule(api, { name: 'Разбор' });
  await page.reload();
  const sessionTitle = api.list.get(ids.pinned)!.title;
  await expect(page.locator(`[data-schedule-row="${report.id}"]`)).toContainText(`Отчёт из сессии · ${sessionTitle}`);
  await expect(page.locator(`[data-schedule-row="${message.id}"]`)).toContainText(`Сообщение в сессию · ${sessionTitle}`);
  await expect(page.locator(`[data-schedule-row="${created.id}"]`)).toContainText('Новая сессия');
  await page.locator(`[data-schedule-row="${report.id}"]`).getByRole('link', { name: 'Открыть' }).click();
  await expect(page.locator('[data-schedule-detail]')).toContainText('Отчёт из сессии');
  await expect(page.locator('[data-schedule-detail]')).toContainText(sessionTitle);
});

test('history_of_a_report_run_shows_the_kept_text_collapsed_until_expanded', async ({ page }) => {
  const { api, ids } = await open(page);
  const schedule = seedSchedule(api, { name: 'Стендап', target: { kind: 'session_report', conversation_id: ids.pinned } });
  seedScheduleRun(api, schedule.id, { detail: { report: { state: 'ready', text: '## Вчера сделано\n- Тесты рутин' } } });
  seedScheduleRun(api, schedule.id, { state: 'started', detail: { report: { state: 'failed' } } });
  await page.reload();
  await page.locator(LIST).getByRole('link', { name: 'Открыть' }).first().click();
  const history = page.locator('[data-schedule-history]');
  await expect(history.locator('[data-run-row]')).toHaveCount(2);
  const toggles = history.getByRole('button', { name: 'Показать отчёт' });
  await expect(toggles).toHaveCount(1);
  await expect(history.locator('[data-role="run-report"]')).toHaveCount(0);
  await toggles.click();
  await expect(history.locator('[data-role="run-report"]').getByRole('heading', { name: 'Вчера сделано' })).toBeVisible();
  await expect(history.getByRole('button', { name: 'Скрыть отчёт' })).toBeVisible();
  await history.getByRole('button', { name: 'Скрыть отчёт' }).click();
  await expect(history.locator('[data-role="run-report"]')).toHaveCount(0);
});

test('session_report_editor_fits_a_phone', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 740 });
  await open(page);
  await openNew(page);
  const editor = page.locator('[data-schedule-editor]');
  await editor.getByRole('radiogroup', { name: 'Цель запуска' }).getByRole('radio', { name: 'Отчёт из сессии' }).click();
  await expect(editor.getByRole('combobox', { name: 'Сессия' })).toBeVisible();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  const radios = await editor.getByRole('radiogroup', { name: 'Цель запуска' }).evaluate((group) => {
    const box = group.getBoundingClientRect();
    return { right: box.right, inside: [...group.querySelectorAll('label, [role="radio"]')].every((item) => item.getBoundingClientRect().right <= box.right + 1) };
  });
  expect(radios.inside).toBe(true);
  expect(radios.right).toBeLessThanOrEqual(360);
});
