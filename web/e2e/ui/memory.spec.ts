import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine, seedPresets } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupChat } from '../../testing/fixtures/mockup-chat';
import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';

const LIST = '[data-memory-list]';

async function open(page: Page, patch?: (api: FakeHubApi) => void) {
  const booted = await bootSecondary(page, '/settings?c=memory', (api) => {
    api.seedMemory({ domain: 'work', subject: 'Сборка воркера', body: 'cargo build -p canterlot-worker' });
    api.seedMemory({ domain: 'work', subject: 'Порт стенда', body: 'Стенд слушает 5432' });
    api.seedMemory({ domain: 'personal', subject: 'Рецепт борща', body: 'Свёкла, капуста, говядина' });
    patch?.(api);
  });
  await expect(page.locator('[data-memory]')).toBeVisible({ timeout: 15_000 });
  return booted;
}

const rowTitles = (page: Page) => page.locator(`${LIST} [data-memory-row] strong`);

test('memory_screen_switches_domains_without_mixing_lists', async ({ page }) => {
  const { api } = await open(page);
  const domains = page.getByRole('radiogroup', { name: 'Домен памяти' });
  await expect(domains.getByRole('radio', { name: 'Работа' })).toBeChecked();
  await expect(rowTitles(page)).toHaveText(['Порт стенда', 'Сборка воркера']);
  await expect(page.locator('[data-memory-list="work"]')).toBeVisible();

  await domains.getByRole('radio', { name: 'Личное' }).check();
  await expect(page.locator('[data-memory-list="personal"]')).toBeVisible();
  await expect(rowTitles(page)).toHaveText(['Рецепт борща']);
  await expect(page.getByText('Сборка воркера')).toHaveCount(0);

  await domains.getByRole('radio', { name: 'Работа' }).check();
  await expect(rowTitles(page)).toHaveText(['Порт стенда', 'Сборка воркера']);
  await expect(page.getByText('Рецепт борща')).toHaveCount(0);
  // Каждый запрос списка назвал домен: экран не просит «всё сразу».
  const lists = api.memoryCalls.filter((call) => call.method === 'GET' && call.id === null);
  expect(lists.length).toBeGreaterThanOrEqual(3);
  expect(lists.every((call) => call.domain === 'work' || call.domain === 'personal')).toBe(true);
  expect(lists.map((call) => call.domain)).toEqual(expect.arrayContaining(['work', 'personal']));

  // Строка пишет свой статус словом.
  await expect(page.locator('[data-memory-row]').first()).toContainText('Активна');
});

test('edit_entry_saves_and_shows_new_version', async ({ page }) => {
  const { api } = await open(page);
  await page.locator('[data-memory-row]', { hasText: 'Порт стенда' }).click();
  const sheet = page.getByRole('dialog', { name: 'Запись' });
  await expect(sheet.locator('[data-role="memory-version"]')).toHaveText('Версия 1');
  await sheet.getByRole('textbox', { name: 'Текст' }).fill('Стенд слушает 5433');
  await sheet.getByRole('button', { name: 'Сохранить', exact: true }).click();

  await expect(sheet.locator('[data-role="memory-notice"]')).toHaveText('Сохранено, версия 2');
  await expect(sheet.locator('[data-role="memory-version"]')).toHaveText('Версия 2');
  const saved = [...api.memoryEntries.values()].find((row) => row.subject === 'Порт стенда')!;
  expect(saved).toMatchObject({ version: 2, body: 'Стенд слушает 5433', domain: 'work' });

  // Новая запись: создаётся в открытом домене и появляется в списке.
  await sheet.getByRole('button', { name: 'Закрыть' }).click();
  await page.getByRole('button', { name: 'Новая запись' }).click();
  await expect(page.getByRole('dialog', { name: 'Новая запись' })).toBeVisible();
  // После создания лист становится листом записи, поэтому дальше он без имени.
  const fresh = page.getByRole('dialog');
  await fresh.getByRole('textbox', { name: 'Тема' }).fill('Ветка выпуска');
  await fresh.getByRole('textbox', { name: 'Текст' }).fill('Выпуск идёт из int/143');
  await fresh.getByRole('button', { name: 'Создать' }).click();
  await expect(fresh.locator('[data-role="memory-notice"]')).toHaveText('Запись создана');
  await fresh.getByRole('button', { name: 'Закрыть' }).click();
  await expect(rowTitles(page)).toContainText(['Ветка выпуска']);
  expect([...api.memoryEntries.values()].find((row) => row.subject === 'Ветка выпуска')?.domain).toBe('work');
});

test('version_conflict_shows_reload_prompt', async ({ page }) => {
  const { api } = await open(page);
  await page.locator('[data-memory-row]', { hasText: 'Порт стенда' }).click();
  const sheet = page.getByRole('dialog', { name: 'Запись' });
  await sheet.getByRole('textbox', { name: 'Текст' }).fill('Моя правка');

  // Пока лист открыт, запись меняют с другого устройства.
  const row = [...api.memoryEntries.values()].find((entry) => entry.subject === 'Порт стенда')!;
  api.updateMemory(row.id, { domain: 'work', expected_version: 1, body: 'Правка с другого устройства' });

  await sheet.getByRole('button', { name: 'Сохранить', exact: true }).click();
  const prompt = sheet.locator('[data-role="memory-conflict"]');
  await expect(prompt).toContainText('Запись изменена в другом месте');
  await expect(prompt).toContainText('версия 2');
  expect(api.memoryEntries.get(row.id)?.body).toBe('Правка с другого устройства');

  await prompt.getByRole('button', { name: 'Загрузить актуальную версию' }).click();
  await expect(sheet.getByRole('textbox', { name: 'Текст' })).toHaveValue('Правка с другого устройства');
  await expect(sheet.locator('[data-role="memory-version"]')).toHaveText('Версия 2');
  await expect(prompt).toHaveCount(0);
});

test('secret_warning_asks_before_saving_anyway', async ({ page }) => {
  const { api } = await open(page);
  await page.getByRole('button', { name: 'Новая запись' }).click();
  await expect(page.getByRole('dialog', { name: 'Новая запись' })).toBeVisible();
  const sheet = page.getByRole('dialog');
  await sheet.getByRole('textbox', { name: 'Тема' }).fill('Доступ к API');
  await sheet.getByRole('textbox', { name: 'Текст' }).fill('ключ sk-abcdefghijklmnop1234');
  await sheet.getByRole('button', { name: 'Создать' }).click();
  const warning = sheet.locator('[data-role="memory-secret"]');
  await expect(warning).toContainText('Текст похож на секрет');
  expect([...api.memoryEntries.values()].some((row) => row.subject === 'Доступ к API')).toBe(false);
  await warning.getByRole('button', { name: 'Сохранить всё равно' }).click();
  await expect(sheet.locator('[data-role="memory-notice"]')).toHaveText('Запись создана');
});

test('archive_restore_and_hard_delete_with_confirmation', async ({ page }) => {
  const { api } = await open(page);
  await page.locator('[data-memory-row]', { hasText: 'Сборка воркера' }).click();
  let sheet = page.getByRole('dialog', { name: 'Запись' });
  await sheet.getByRole('button', { name: 'В архив' }).click();
  await expect(sheet).toHaveCount(0);
  await expect(rowTitles(page)).toHaveText(['Порт стенда']);

  const view = page.getByRole('radiogroup', { name: 'Показать записи' });
  await view.getByRole('radio', { name: 'В архиве' }).check();
  await expect(rowTitles(page)).toHaveText(['Сборка воркера']);
  await expect(page.locator('[data-memory-row]').first()).toContainText('В архиве');

  await page.locator('[data-memory-row]', { hasText: 'Сборка воркера' }).click();
  sheet = page.getByRole('dialog', { name: 'Запись' });
  await sheet.getByRole('button', { name: 'Вернуть из архива' }).click();
  await expect(page.getByText('В архиве записей нет')).toBeVisible();
  await view.getByRole('radio', { name: 'Активные' }).check();
  // Возвращённая запись обновлена позже остальных и встаёт первой.
  await expect(rowTitles(page)).toHaveText(['Сборка воркера', 'Порт стенда']);

  // Удаление навсегда: сначала подтверждение, и пока оно не нажато, запись на месте.
  await page.locator('[data-memory-row]', { hasText: 'Порт стенда' }).click();
  sheet = page.getByRole('dialog', { name: 'Запись' });
  await sheet.getByRole('button', { name: 'Удалить навсегда' }).click();
  const confirm = sheet.getByRole('group', { name: 'Удалить навсегда' });
  await expect(confirm).toContainText('без возможности вернуть');
  expect([...api.memoryEntries.values()].some((row) => row.subject === 'Порт стенда')).toBe(true);
  await confirm.getByRole('button', { name: 'Отмена' }).click();
  await expect(confirm).toHaveCount(0);
  await sheet.getByRole('button', { name: 'Удалить навсегда' }).click();
  await sheet.getByRole('group', { name: 'Удалить навсегда' }).getByRole('button', { name: 'Удалить навсегда' }).click();
  await expect(sheet).toHaveCount(0);
  await expect(rowTitles(page)).toHaveText(['Сборка воркера']);
  expect([...api.memoryEntries.values()].some((row) => row.subject === 'Порт стенда')).toBe(false);
});

test('session_info_shows_memory_domain_or_no_memory', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, {
    id: 'workstation',
    name: 'workstation',
    harnesses: [{ id: 'claude', title: 'Claude Code', provider: 'claude', models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium'] }], permission_modes: ['owner', 'ask'] }]
  });
  seedPresets(api);
  const id = seedMockupChat(api);
  const detail = api.details.get(id)!;
  detail.harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
  detail.machine = { id: 'workstation', name: 'workstation' };
  detail.model = 'opus-5.5';
  detail.effort = 'medium';
  detail.permission_mode = 'owner';
  await installChatRoutes(page, hub, api, '00000000-0000-4000-8000-0000000000d1');

  const memoryRow = async () => {
    await page.goto(`/c/${id}`);
    await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
    await page.getByRole('button', { name: 'Меню сессии' }).click();
    await page.getByRole('menuitem', { name: 'Настройки сессии' }).click();
    return page.locator('[data-role="session-memory"]');
  };

  Object.assign(detail, { memory_domain: 'personal', memory_project: '/h/home/alpha', memory_conflict: false });
  await expect(await memoryRow()).toContainText('Память: Личное');
  await expect(page.locator('[data-role="session-memory"] code')).toHaveText('/h/home/alpha');

  Object.assign(detail, { memory_domain: 'work' });
  await expect(await memoryRow()).toContainText('Память: Работа');

  Object.assign(detail, { memory_domain: null, memory_project: null, memory_conflict: false });
  await expect(await memoryRow()).toContainText('Память: нет памяти');

  Object.assign(detail, { memory_domain: null, memory_conflict: true });
  await expect(await memoryRow()).toContainText('Память: конфликт домена');
  await expect(page.locator('[data-role="session-memory"] code')).toHaveCount(0);
});

const ALPHA = '/h/work/alpha';
const BETA = '/h/work/beta';

test('filters_by_project_and_kind', async ({ page }) => {
  const { api } = await open(page, (hub) => {
    hub.seedMemory({ domain: 'work', subject: 'Альфа: ловушка', kind: 'gotcha', level: 'project', project: ALPHA, project_label: 'alpha', updated_by: { kind: 'agent', harness: 'claude_code' } });
    hub.seedMemory({ domain: 'work', subject: 'Бета: решение', kind: 'decision', level: 'project', project: BETA, project_label: 'beta' });
    hub.seedMemory({ domain: 'work', subject: 'Общая ловушка', kind: 'gotcha' });
  });
  const project = page.getByRole('combobox', { name: 'Проект' });
  const kind = page.getByRole('combobox', { name: 'Вид записей' });
  await expect(project.locator('option')).toHaveText(['Все проекты', 'alpha', 'beta']);

  await project.selectOption(ALPHA);
  await expect(rowTitles(page)).toHaveText(['Альфа: ловушка']);
  // Список проектов не сужается вместе с выборкой.
  await expect(project.locator('option')).toHaveText(['Все проекты', 'alpha', 'beta']);
  expect(api.memoryQueries.at(-1)).toMatchObject({ domain: 'work', project: ALPHA });

  await project.selectOption('');
  await kind.selectOption('gotcha');
  await expect(rowTitles(page)).toHaveText(['Общая ловушка', 'Альфа: ловушка']);
  await expect(page.getByText('Бета: решение')).toHaveCount(0);
  expect(api.memoryQueries.at(-1)).toMatchObject({ kind: 'gotcha' });
  expect(api.memoryQueries.at(-1)?.project).toBeUndefined();

  await project.selectOption(BETA);
  await expect(page.locator('[data-memory-row]')).toHaveCount(0);
  await expect(page.locator('[data-role="memory-empty"]')).toBeVisible();

  // Смена домена сбрасывает фильтры: личный список не прячется за чужим проектом.
  await page.getByRole('radiogroup', { name: 'Домен памяти' }).getByRole('radio', { name: 'Личное' }).check();
  await expect(rowTitles(page)).toHaveText(['Рецепт борща']);
  await expect(project.locator('option')).toHaveText(['Все проекты']);

  // Строка называет, кем и когда запись менялась.
  await page.getByRole('radiogroup', { name: 'Домен памяти' }).getByRole('radio', { name: 'Работа' }).check();
  await expect(page.locator('[data-memory-row]', { hasText: 'Альфа: ловушка' })).toContainText('Изменено агентом (claude_code)');
  await expect(page.locator('[data-memory-row]', { hasText: 'Общая ловушка' })).toContainText('Изменено владельцем');
});

test('level_change_saves_with_version', async ({ page }) => {
  const { api } = await open(page, (hub) => { hub.seedMemory({ domain: 'work', subject: 'Правило сборки', body: 'Собирать из корня' }); });
  const row = () => [...api.memoryEntries.values()].find((entry) => entry.subject === 'Правило сборки')!;
  await page.locator('[data-memory-row]', { hasText: 'Правило сборки' }).click();
  const sheet = page.getByRole('dialog', { name: 'Запись' });
  const level = sheet.getByRole('combobox', { name: 'Уровень' });
  await expect(level).toHaveValue('domain');

  await level.selectOption('project');
  await expect(sheet.getByRole('button', { name: 'Сохранить', exact: true })).toBeDisabled();
  await sheet.getByRole('textbox', { name: 'Каталог проекта' }).fill(ALPHA);
  await sheet.getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(sheet.locator('[data-role="memory-version"]')).toHaveText('Версия 2');
  expect(row()).toMatchObject({ level: 'project', project: ALPHA, version: 2 });
  await expect(sheet.getByRole('textbox', { name: 'Каталог проекта' })).toHaveCount(0);
  await expect(level).toHaveValue('project');

  await level.selectOption('domain');
  await sheet.getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(sheet.locator('[data-role="memory-version"]')).toHaveText('Версия 3');
  expect(row()).toMatchObject({ level: 'domain', project: null, version: 3 });

  // Устаревшая версия при смене уровня даёт тот же запрос перезагрузки.
  api.updateMemory(row().id, { domain: 'work', expected_version: 3, body: 'Правка с другого устройства' });
  await level.selectOption('project');
  await sheet.getByRole('textbox', { name: 'Каталог проекта' }).fill(BETA);
  await sheet.getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(sheet.locator('[data-role="memory-conflict"]')).toContainText('версия 4');
  expect(row().level).toBe('domain');
});

test('revision_view_and_restore', async ({ page }) => {
  const { api } = await open(page, (hub) => {
    const seeded = hub.seedMemory({ domain: 'work', subject: 'Адрес стенда', body: 'Первый текст записи' });
    hub.updateMemory(seeded.id, { domain: 'work', expected_version: 1, body: 'Второй текст записи' });
    hub.updateMemory(seeded.id, { domain: 'work', expected_version: 2, body: 'Третий текст записи' });
  });
  const entry = () => [...api.memoryEntries.values()].find((row) => row.subject === 'Адрес стенда')!;

  await page.locator('[data-memory-row]', { hasText: 'Адрес стенда' }).click();
  const sheet = page.getByRole('dialog', { name: 'Запись' });
  await expect(sheet.locator('[data-role="memory-version"]')).toHaveText('Версия 3');
  await sheet.getByText('Прошлые версии').click();
  const list = sheet.locator('[data-role="memory-revisions"]');
  await expect(list.getByRole('button')).toHaveCount(2);

  await list.getByRole('button', { name: /Версия 1/ }).click();
  const view = sheet.locator('[data-role="memory-revision-body"]');
  await expect(view).toHaveText('Первый текст записи');
  await sheet.getByRole('button', { name: 'Вернуть эту версию' }).click();
  await expect(sheet.locator('[data-role="memory-version"]')).toHaveText('Версия 4');
  await expect(sheet.getByRole('textbox', { name: 'Текст' })).toHaveValue('Первый текст записи');
  expect(entry()).toMatchObject({ version: 4, body: 'Первый текст записи' });
  // Новая версия видна в списке прошлых, прежняя третья тоже.
  await expect(list.getByRole('button')).toHaveCount(3);

  // Версию открыли, пока запись менялась в другом месте: перезапись отклонена, текст не потерян.
  await list.getByRole('button', { name: /Версия 2/ }).click();
  api.updateMemory(entry().id, { domain: 'work', expected_version: 4, body: 'Правка с другого устройства' });
  await sheet.getByRole('button', { name: 'Вернуть эту версию' }).click();
  const prompt = sheet.locator('[data-role="memory-conflict"]');
  await expect(prompt).toContainText('версия 5');
  expect(entry().body).toBe('Правка с другого устройства');
  await prompt.getByRole('button', { name: 'Загрузить актуальную версию' }).click();
  await expect(sheet.getByRole('textbox', { name: 'Текст' })).toHaveValue('Правка с другого устройства');
});

test('revision_restore_brings_back_subject_kind_and_level', async ({ page }) => {
  const { api } = await open(page, (hub) => {
    const seeded = hub.seedMemory({ domain: 'work', subject: 'Старая тема', kind: 'gotcha', level: 'project', project: ALPHA, project_label: 'alpha', body: 'Старый текст' });
    hub.updateMemory(seeded.id, { domain: 'work', expected_version: 1, subject: 'Новая тема', kind: 'decision', level: 'domain', body: 'Новый текст' });
  });
  const entry = () => [...api.memoryEntries.values()].find((row) => row.body === 'Старый текст' || row.body === 'Новый текст')!;
  expect(entry()).toMatchObject({ subject: 'Новая тема', kind: 'decision', level: 'domain', project: null });

  await page.locator('[data-memory-row]', { hasText: 'Новая тема' }).click();
  const sheet = page.getByRole('dialog', { name: 'Запись' });
  await sheet.getByText('Прошлые версии').click();
  await sheet.locator('[data-role="memory-revisions"]').getByRole('button', { name: /Версия 1/ }).click();
  await expect(sheet.locator('[data-role="memory-revision"]')).toContainText('тема, вид, уровень, срок и текст');
  await sheet.getByRole('button', { name: 'Вернуть эту версию' }).click();

  await expect(sheet.locator('[data-role="memory-version"]')).toHaveText('Версия 3');
  await expect(sheet.getByRole('textbox', { name: 'Тема' })).toHaveValue('Старая тема');
  await expect(sheet.getByRole('combobox', { name: 'Вид' })).toHaveValue('gotcha');
  await expect(sheet.getByRole('combobox', { name: 'Уровень' })).toHaveValue('project');
  await expect(sheet.getByRole('textbox', { name: 'Текст' })).toHaveValue('Старый текст');
  expect(entry()).toMatchObject({ subject: 'Старая тема', kind: 'gotcha', level: 'project', project: ALPHA, body: 'Старый текст', version: 3 });
});

test('secret_warning_on_revision_restore_replays_that_restore', async ({ page }) => {
  const secret = 'ключ sk-abcdefghijklmnop1234 для вызова';
  const { api } = await open(page, (hub) => {
    // Старая версия сохранена владельцем поверх предупреждения, текущая чистая.
    const seeded = hub.seedMemory({ domain: 'work', subject: 'Доступ к API', body: secret });
    hub.updateMemory(seeded.id, { domain: 'work', expected_version: 1, body: 'Чистый текст' });
  });
  const entry = () => [...api.memoryEntries.values()].find((row) => row.subject === 'Доступ к API')!;
  await page.locator('[data-memory-row]', { hasText: 'Доступ к API' }).click();
  const sheet = page.getByRole('dialog', { name: 'Запись' });
  // Несохранённая правка в редакторе не должна подменить восстановление.
  await sheet.getByRole('textbox', { name: 'Текст' }).fill('Несохранённая правка редактора');
  await sheet.getByText('Прошлые версии').click();
  await sheet.locator('[data-role="memory-revisions"]').getByRole('button', { name: /Версия 1/ }).click();
  await sheet.getByRole('button', { name: 'Вернуть эту версию' }).click();

  const warning = sheet.locator('[data-role="memory-secret"]');
  await expect(warning).toContainText('Текст похож на секрет');
  expect(entry()).toMatchObject({ version: 2, body: 'Чистый текст' });
  await warning.getByRole('button', { name: 'Сохранить всё равно' }).click();

  await expect(sheet.locator('[data-role="memory-version"]')).toHaveText('Версия 3');
  expect(entry()).toMatchObject({ version: 3, body: secret });
  await expect(sheet.getByRole('textbox', { name: 'Текст' })).toHaveValue(secret);
});
