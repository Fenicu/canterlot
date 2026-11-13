import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';
import { bootDictation, micButton } from './dictation-fixture';

test('dictation_block_shows_the_service_state_and_keeps_the_switch', async ({ page }) => {
  const { api } = await bootSecondary(page, '/settings?c=input', (api) => {
    api.hub.dictation.status = { available: false, reason: 'no_service', mode: null, engine: null };
  });
  const block = page.locator('[data-dictation-settings]');
  await expect(block).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Диктовка', level: 3 })).toBeVisible();
  await expect(page.locator('[data-dictation-status]')).toHaveText('Служба распознавания не подключена');

  const toggle = block.getByRole('switch', { name: /^Кнопка микрофона/ });
  await expect(toggle).toBeChecked();
  await toggle.uncheck();
  await expect.poll(() => page.evaluate(() => localStorage.getItem('canterlot.dictation'))).toBe('off');
  await page.reload();
  await expect(page.locator('[data-dictation-settings]').getByRole('switch', { name: /^Кнопка микрофона/ })).not.toBeChecked();

  api.hub.dictation.status = { available: false, reason: 'unhealthy', mode: null, engine: null };
  await page.reload();
  await expect(page.locator('[data-dictation-status]')).toHaveText('Служба распознавания не отвечает');
  api.hub.dictation.status = { available: false, reason: 'disabled', mode: null, engine: null };
  await page.reload();
  await expect(page.locator('[data-dictation-status]')).toHaveText('Диктовка выключена в настройках службы распознавания');
});

test('turning_the_switch_off_hides_the_mic_button_in_the_composer', async ({ page }) => {
  await bootSecondary(page, '/settings?c=input', (api) => {
    api.hub.dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
  });
  const block = page.locator('[data-dictation-settings]');
  await block.getByRole('switch', { name: /^Кнопка микрофона/ }).uncheck();
  await bootDictation(page, {
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
    }
  });
  await expect(micButton(page)).toHaveCount(0);
});

test('dictionary_editor_adds_frequent_pairs_and_saves_the_list', async ({ page }) => {
  const { api } = await bootSecondary(page, '/settings?c=input', (api) => {
    api.hub.dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
    api.hub.dictation.terms = [{ pattern: 'гит', replacement: 'git' }];
  });
  const block = page.locator('[data-dictation-settings]');
  await expect(block.locator('[data-dictation-term]')).toHaveCount(1);

  await block.getByRole('button', { name: 'Добавить частые' }).click();
  const list = block.locator('[data-dictation-frequent-list]');
  await expect(list).toBeVisible();
  await expect(list.getByRole('checkbox', { name: 'гит → git' })).toBeDisabled();
  await list.getByRole('checkbox', { name: 'апи → API' }).check();
  await list.getByRole('checkbox', { name: 'свелт → Svelte' }).check();
  await list.getByRole('button', { name: 'Добавить выбранные' }).click();
  await expect(block.locator('[data-dictation-term]')).toHaveCount(3);

  await block.getByRole('button', { name: 'Сохранить словарь' }).click();
  await expect
    .poll(() => api.hub.dictation.terms)
    .toEqual([
      { pattern: 'гит', replacement: 'git' },
      { pattern: 'апи', replacement: 'API' },
      { pattern: 'свелт', replacement: 'Svelte' }
    ]);
  await page.reload();
  await expect(page.locator('[data-dictation-term]')).toHaveCount(3);
  await expect(page.locator('[data-term-pattern]').nth(1)).toHaveValue('апи');
});

test('dictionary_editor_reports_validation_errors_in_russian', async ({ page }) => {
  const { api } = await bootSecondary(page, '/settings?c=input', (api) => {
    api.hub.dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
  });
  const block = page.locator('[data-dictation-settings]');
  await block.getByRole('button', { name: 'Добавить пару' }).click();
  await block.getByRole('button', { name: 'Сохранить словарь' }).click();
  await expect(block.getByRole('alert')).toHaveText('Заполните шаблон и замену');

  await block.locator('[data-term-pattern]').fill('апи');
  await block.locator('[data-term-replacement]').fill('API');
  await block.getByRole('button', { name: 'Добавить пару' }).click();
  await block.locator('[data-term-pattern]').nth(1).fill(' АПИ ');
  await block.locator('[data-term-replacement]').nth(1).fill('api');
  await block.getByRole('button', { name: 'Сохранить словарь' }).click();
  await expect(block.getByRole('alert')).toHaveText('Такой шаблон уже есть');

  // Ошибку проверки хаба показываем тем же русским текстом.
  await block.locator('[data-term-pattern]').nth(1).fill('мейн');
  await block.locator('[data-term-replacement]').nth(1).fill('main');
  api.hub.dictation.termsFailure = 'too_many_terms';
  await block.getByRole('button', { name: 'Сохранить словарь' }).click();
  await expect(block.getByRole('alert')).toHaveText('Не больше 200 пар');
});
