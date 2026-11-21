import type { Locator, Page } from '@playwright/test';
import { icons } from '../../src/lib/ui/icons';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { hubText, setActivity, streamCall } from '../../testing/fixtures/feed-cards';
import { LONG_SUBJECT, seedMemoryCards, type MemoryCardsSeed } from '../../testing/fixtures/memory-cards';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000c2';

async function boot(page: Page): Promise<{ hub: FakeHub; api: FakeHubApi; seed: MemoryCardsSeed }> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const seed = seedMemoryCards(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${seed.id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.feed [data-row]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, seed };
}

const feed = (page: Page) => page.locator('.feed');
const card = (page: Page, id: string) => page.locator(`[data-key="block:${id}"] .feed-card`).first();

/** Лента виртуальная: строка далеко от низа не смонтирована, пока до неё не докрутили. */
async function reveal(page: Page, target: Locator): Promise<Locator> {
  const settle = () => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  await feed(page).evaluate((node) => {
    node.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 }));
    node.scrollTop = 0;
  });
  await settle();
  for (let step = 0; step < 60; step++) {
    if ((await target.count()) > 0) {
      try {
        await target.first().scrollIntoViewIfNeeded({ timeout: 2_000 });
        await settle();
        if ((await target.count()) > 0) return target.first();
      } catch {
        // Строку сняли с окна, пока до неё крутили: ищем дальше.
      }
    }
    // Чтение и поиск памяти уходят в свёрнутые шаги хода: раскрываем встреченную группу.
    const folded = feed(page).locator('button.head[aria-expanded="false"]', { hasText: /\d+ шаг/ });
    if ((await folded.count()) > 0) {
      await folded.first().click();
      await settle();
      continue;
    }
    await feed(page).evaluate((node) => {
      node.scrollTop += node.clientHeight * 0.6;
    });
    await settle();
  }
  await expect(target.first()).toBeAttached();
  return target.first();
}

const line = (found: Locator) => found.locator('.head .hit');
/**
 * Раскрывает карточку за шеврон: на узком экране середина строки занята ссылкой на запись.
 * Точка клика лежит на середине высоты, у шеврона. Угол кнопки (8, 8) скруглён радиусом карточки
 * в 22 px и проходит попадание лишь на пару пикселей: сдвиг строки между нажатием и отпусканием
 * под нагрузкой отдавал отпускание шапке, и клика по кнопке не выходило.
 */
async function toggle(found: Locator): Promise<void> {
  const hit = line(found);
  const box = await hit.boundingBox();
  await hit.click({ position: { x: 20, y: (box?.height ?? 44) / 2 } });
}

test('every_memory_card_reads_its_line_and_word', async ({ page }) => {
  test.setTimeout(90_000);
  const { seed } = await boot(page);
  const expected: [string, string, string, string][] = [
    ['search', 'canterlot.memory_search', 'Искала в памяти: «тестовая база»', '3 записи'],
    ['searchEmpty', 'canterlot.memory_search', 'Искала в памяти: «квантовая гравитация»', 'ничего'],
    ['searchNew', 'canterlot.memory_search', 'Новые записи памяти', '1 запись'],
    ['get', 'canterlot.memory_get', 'Прочитала запись: Порт тестовой базы', ''],
    ['write', 'canterlot.memory_write', 'Запомнила: Не писать эмодзи в карточках', ''],
    ['writeSecret', 'canterlot.memory_write', 'Отказ: похоже на секрет', 'ошибка'],
    ['writeExists', 'canterlot.memory_write', 'Уже есть запись с такой темой', 'ошибка'],
    ['writeRate', 'canterlot.memory_write', 'Лимит записей в час', 'ошибка'],
    ['update', 'canterlot.memory_update', 'Обновила запись: Порт тестовой базы', ''],
    ['updateConflict', 'canterlot.memory_update', 'Отказ: запись уже изменили (версия 3)', 'ошибка'],
    ['delete', 'canterlot.memory_delete', 'Убрала в архив: Порт тестовой базы', ''],
    ['deleteUnknown', 'canterlot.memory_delete', 'Отказ: записи нет', 'ошибка']
  ];
  for (const [name, id, text, word] of expected) {
    const found = await reveal(page, card(page, seed.blocks[name]!));
    await expect(found, name).toHaveAttribute('data-card', id);
    await expect(line(found), name).toHaveAttribute('aria-label', new RegExp(`^${text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\. Показать `));
    if (word) await expect(found.locator('[data-role="card-state"]'), name).toHaveText(word);
  }
  // Правка называет версии справа.
  const update = await reveal(page, card(page, seed.blocks.update!));
  await expect(update.locator('[data-role="card-state"]')).toHaveText('версия 2 → 3');
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
});

test('a_search_card_opens_and_lists_the_rows_with_kind_level_and_stale_mark', async ({ page }) => {
  const { seed } = await boot(page);
  const found = await reveal(page, card(page, seed.blocks.search!));
  await toggle(found);
  const rows = found.locator('[data-role="memory-rows"] li');
  await expect(rows).toHaveCount(3);
  await expect(rows.nth(0)).toContainText('грабли Порт тестовой базы');
  await expect(rows.nth(0)).toContainText('canterlot');
  await expect(rows.nth(0).locator('[data-role="memory-stale"]')).toHaveCount(0);
  await expect(rows.nth(1)).toContainText('справка Сборка воркера');
  await expect(rows.nth(1)).toContainText('весь домен');
  await expect(rows.nth(1).locator('[data-role="memory-stale"]')).toHaveText('устарела');
  await expect(rows.nth(2)).toContainText('о владельце');
});

test('a_search_row_opens_that_entry_on_the_memory_screen', async ({ page }) => {
  const { seed } = await boot(page);
  const found = await reveal(page, card(page, seed.blocks.search!));
  await toggle(found);
  await found.locator(`[data-memory-row="${seed.entries.port.id}"]`).click();
  await expect(page).toHaveURL(new RegExp(`/settings\\?c=memory&entry=${seed.entries.port.id}`));
  const sheet = page.getByRole('dialog', { name: 'Запись' });
  await expect(sheet).toBeVisible({ timeout: 15_000 });
  await expect(sheet.getByRole('textbox', { name: 'Тема' })).toHaveValue('Порт тестовой базы');
});

test('a_link_to_a_personal_entry_without_a_domain_switches_to_it', async ({ page }) => {
  const { seed } = await boot(page);
  await page.goto(`/settings?c=memory&entry=${seed.entries.recipe.id}`);
  const sheet = page.getByRole('dialog', { name: 'Запись' });
  await expect(sheet).toBeVisible({ timeout: 15_000 });
  await expect(sheet.getByRole('textbox', { name: 'Тема' })).toHaveValue('Рецепт борща');
  await expect(page.getByRole('radiogroup', { name: 'Домен памяти' }).getByRole('radio', { name: 'Личное' })).toBeChecked();
});

test('a_link_to_an_unknown_entry_shows_the_list_with_a_notice', async ({ page }) => {
  await boot(page);
  await page.goto('/settings?c=memory&entry=00000000-0000-4000-8000-0000000000ff');
  await expect(page.locator('[data-role="memory-link-notice"]')).toContainText('Записи нет');
  await expect(page.getByRole('dialog', { name: 'Запись' })).toHaveCount(0);
  await expect(page.locator('[data-memory-list]')).toBeVisible();
});

test('a_get_card_shows_the_body_as_markdown_with_kind_level_version_and_expiry', async ({ page }) => {
  const { seed } = await boot(page);
  const found = await reveal(page, card(page, seed.blocks.get!));
  await toggle(found);
  await expect(found.locator('[data-role="memory-meta"]')).toHaveText('грабли · canterlot · версия 2 · до 01.12');
  await expect(found.locator('[data-role="memory-body"] strong')).toHaveText('Порт');
  await expect(found.locator('[data-role="memory-body"] code')).toHaveText('55883');
});

test('a_write_card_names_kind_and_level_and_opens_the_body', async ({ page }) => {
  const { seed } = await boot(page);
  const found = await reveal(page, card(page, seed.blocks.write!));
  await expect(found.locator('[data-role="memory-meta"]')).toHaveText('урок · весь домен');
  await toggle(found);
  await expect(found.locator('[data-role="memory-body"] li')).toHaveCount(2);
});

test('an_existing_subject_refusal_links_the_existing_entry', async ({ page }) => {
  const { seed } = await boot(page);
  const found = await reveal(page, card(page, seed.blocks.writeExists!));
  await expect(found.locator('.head a')).toHaveAttribute('href', new RegExp(`/settings\\?c=memory&entry=${seed.entries.port.id}`));
});

test('an_update_card_lists_the_changed_fields_and_the_new_text', async ({ page }) => {
  const { seed } = await boot(page);
  const found = await reveal(page, card(page, seed.blocks.update!));
  await expect(found.locator('[data-role="memory-changes"]')).toHaveText('Изменено: срок, текст');
  await toggle(found);
  await expect(found.locator('[data-role="memory-details"]')).toHaveText('Срок снят');
  await expect(found.locator('[data-role="memory-body"] code')).toHaveText('55884');
});

/** Анимации значка карточки по именам: Svelte прибавляет к имени ключевых кадров свой хеш. */
async function markMotion(found: Locator): Promise<string[]> {
  return found.locator('[data-role="card-icon"]').evaluate((node) => node.getAnimations({ subtree: true }).map((item) => (item as CSSAnimation).animationName.replace(/^svelte-[a-z0-9]+-/, '')));
}
const mark = (found: Locator) => found.locator('[data-role="card-icon"] svg.mark');

test('a_memory_card_leads_with_the_brain_and_its_chevron_sits_at_the_right_edge', async ({ page }) => {
  const { seed } = await boot(page);
  const found = await reveal(page, card(page, seed.blocks.search!));
  const icon = await found.locator('[data-role="card-icon"]').boundingBox();
  const text = await found.locator('.head .line, .head .lead').first().boundingBox();
  const word = await found.locator('[data-role="card-state"]').boundingBox();
  const chevron = await found.locator('.head .chevron').boundingBox();
  const head = await found.locator('.head').boundingBox();
  expect(icon && text && word && chevron && head).toBeTruthy();
  expect(icon!.x).toBeLessThan(text!.x);
  expect(word!.x + word!.width).toBeLessThanOrEqual(chevron!.x + 0.5);
  expect(head!.x + head!.width - (chevron!.x + chevron!.width)).toBeLessThan(40);
  await expect(found.locator('[data-role="card-icon"] path.ink')).toHaveAttribute('d', icons.brain);
  // Значок стоит в слоте шеврона: колонка текста у карточек со значком и без него одна.
  await expect(found.locator('.head .chevron')).toHaveCSS('order', '5');
});

test('a_card_from_history_stands_drawn_and_a_refused_one_stays_a_muted_sketch', async ({ page }) => {
  const { seed } = await boot(page);
  const written = await reveal(page, card(page, seed.blocks.write!));
  await expect(mark(written)).toHaveAttribute('data-state', 'dry');
  expect(await markMotion(written)).toEqual([]);
  const refused = await reveal(page, card(page, seed.blocks.writeSecret!));
  await expect(mark(refused)).toHaveAttribute('data-state', 'refused');
  // Ничего не записано: чернил нет, контур приглушён и не дышит.
  await expect(refused.locator('svg.mark path.ink')).toHaveCSS('opacity', '0');
  expect(Number(await refused.locator('svg.mark path.ghost').evaluate((node) => getComputedStyle(node).opacity))).toBeGreaterThan(0);
  expect(await markMotion(refused)).toEqual([]);
});

test.describe('a live memory call', () => {
  const WRITE = { kind: 'feedback', subject: 'Значки только из Lucide', body: 'Из лицензированного набора.', level: 'project' };
  const written = (api: FakeHubApi) => hubText({ id: api.id(), domain: 'work', level: 'project', project_label: 'canterlot', version: 1 });

  async function working(page: Page) {
    const booted = await boot(page);
    // Вызов без ответа идёт, только пока сессия занята: состояние приходит событием хаба, как в жизни.
    setActivity(booted.hub, booted.api, booted.seed.id, 'working');
    return booted;
  }

  test('sketches_and_breathes_while_it_runs_then_is_inked_once_and_dries', async ({ page }) => {
    const { hub, api, seed } = await working(page);
    // Масштаб движения растягивает росчерк: замер не гонится с его концом.
    await page.evaluate(() => document.documentElement.style.setProperty('--motion-scale', '8'));
    const fresh = streamCall(hub, api, seed.id, seed.run, 'mcp__canterlot__memory_write', WRITE);
    const found = card(page, fresh.call);
    await expect(found).toHaveAttribute('data-state', 'running');
    await expect(mark(found)).toHaveAttribute('data-state', 'sketch');
    await expect(found.locator('[data-role="card-state"]')).toHaveText('идёт');
    expect(await markMotion(found)).toContain('mark-breathe');
    streamCall(hub, api, seed.id, seed.run, 'mcp__canterlot__memory_write', null, { blockId: fresh.call, resultId: fresh.result, result: written(api) });
    await expect(found).toHaveAttribute('data-state', 'done');
    await expect(mark(found)).toHaveAttribute('data-state', 'ink');
    expect(await markMotion(found)).toContain('mark-ink');
    expect(await markMotion(found)).not.toContain('mark-breathe');
    // Слово «идёт» ушло сменой подписи и покинуло документ.
    await expect(found.locator('[data-role="card-state"]')).toHaveCount(0, { timeout: 15_000 });
    // Росчерк идёт один раз: потом значок стоит написанным и ничего не крутится.
    await expect(mark(found)).toHaveAttribute('data-state', 'dry', { timeout: 30_000 });
    expect(await markMotion(found)).toEqual([]);
  });

  test('a_refusal_that_arrives_live_stays_a_muted_sketch_and_says_so_in_words', async ({ page }) => {
    const { hub, api, seed } = await working(page);
    const fresh = streamCall(hub, api, seed.id, seed.run, 'mcp__canterlot__memory_write', WRITE);
    const found = card(page, fresh.call);
    await expect(mark(found)).toHaveAttribute('data-state', 'sketch');
    streamCall(hub, api, seed.id, seed.run, 'mcp__canterlot__memory_write', null, { blockId: fresh.call, resultId: fresh.result, result: 'looks_like_secret: The text looks like a secret {"class":"assignment"}', failed: true });
    await expect(found).toHaveAttribute('data-state', 'error');
    await expect(mark(found)).toHaveAttribute('data-state', 'refused');
    await expect(found.locator('[data-role="card-state"]')).toHaveText('ошибка');
    expect(await markMotion(found)).not.toContain('mark-ink');
  });

  test('reduced_motion_leaves_a_still_outline_while_running_and_a_finished_icon_after', async ({ page }) => {
    await page.emulateMedia({ reducedMotion: 'reduce' });
    const { hub, api, seed } = await working(page);
    const fresh = streamCall(hub, api, seed.id, seed.run, 'mcp__canterlot__memory_write', WRITE);
    const found = card(page, fresh.call);
    await expect(mark(found)).toHaveAttribute('data-state', 'sketch');
    await expect(mark(found)).toHaveAttribute('data-motion', 'off');
    expect(await markMotion(found)).toEqual([]);
    expect(Number(await found.locator('svg.mark path.ghost').evaluate((node) => getComputedStyle(node).opacity))).toBeGreaterThan(0);
    streamCall(hub, api, seed.id, seed.run, 'mcp__canterlot__memory_write', null, { blockId: fresh.call, resultId: fresh.result, result: written(api) });
    await expect(found).toHaveAttribute('data-state', 'done');
    // Без движения нечего прописывать: значок сразу стоит написанным, слово заменено без перехода.
    await expect(mark(found)).toHaveAttribute('data-state', 'dry');
    expect(await markMotion(found)).toEqual([]);
    await expect(found.locator('[data-role="card-state"]')).toHaveCount(0);
  });

  test('motion_scale_zero_is_the_same_static_fallback', async ({ page }) => {
    const { hub, api, seed } = await working(page);
    await page.evaluate(() => document.documentElement.style.setProperty('--motion-scale', '0'));
    const fresh = streamCall(hub, api, seed.id, seed.run, 'mcp__canterlot__memory_write', WRITE);
    const found = card(page, fresh.call);
    await expect(mark(found)).toHaveAttribute('data-state', 'sketch');
    expect(await markMotion(found)).toEqual([]);
    streamCall(hub, api, seed.id, seed.run, 'mcp__canterlot__memory_write', null, { blockId: fresh.call, resultId: fresh.result, result: written(api) });
    await expect(mark(found)).toHaveAttribute('data-state', 'dry');
    expect(await markMotion(found)).toEqual([]);
    await expect(found.locator('[data-role="card-state"]')).toHaveCount(0);
  });
});

test('a_long_subject_and_a_long_row_stay_inside_the_bubble', async ({ page }) => {
  const { seed } = await boot(page);
  for (const name of ['writeLong', 'search']) {
    const found = await reveal(page, card(page, seed.blocks[name]!));
    if (name === 'search') await toggle(found);
    const box = await found.boundingBox();
    const viewport = page.viewportSize()!;
    expect(box!.x + box!.width, name).toBeLessThanOrEqual(viewport.width);
    const wide = await found.evaluate((node) => node.scrollWidth - node.clientWidth);
    expect(wide, name).toBeLessThanOrEqual(0);
  }
  await expect(page.getByText(LONG_SUBJECT).first()).toBeAttached();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
});
