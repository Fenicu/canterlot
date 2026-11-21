import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { seedSystemBubbles, type SystemBubbleGroup } from '../../testing/fixtures/system-bubbles';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function boot(page: Page, group: SystemBubbleGroup = 'all', opts: { fresh?: boolean } = {}) {
  const phone = test.info().project.name === 'phone';
  await page.setViewportSize({ width: phone ? 412 : 1280, height: 900 });
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Оркестратор' });
  const seed = seedSystemBubbles(api, id, group);
  // Пузырь, пришедший на глазах, ленте виден как «только что»: время записи не раньше четырёх секунд назад.
  if (opts.fresh) for (const item of api.histories.get(id) ?? []) if (item.kind === 'input') item.created_at = new Date(Date.now() + 3_600_000).toISOString();
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id, ...seed };
}

async function noOverflow(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
}

type Box = { x: number; y: number; width: number; height: number };

/** Зазоры по осям, где прямоугольники расходятся: касание и нахлёст дают 0 или меньше и отсеиваются. */
function clearGap(a: Box, b: Box) {
  return [
    Math.max(a.x, b.x) - Math.min(a.x + a.width, b.x + b.width),
    Math.max(a.y, b.y) - Math.min(a.y + a.height, b.y + b.height)
  ].filter((gap) => gap > 0);
}

async function tapTarget(page: Page) {
  return page.evaluate(() => parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--target')));
}

test('alarm_bubbles_carry_the_condition_note_targets_and_the_folded_text', async ({ page }) => {
  const { child } = await boot(page, 'alarm');
  const single = page.locator('article.system[data-event-kind="alarm"]').filter({ hasText: 'Будильник «gate»' }).first();
  await single.scrollIntoViewIfNeeded();
  await expect(single).toContainText('Пора');
  await expect(single).toContainText('Поставлен');
  await expect(single).toContainText('ждал');
  await expect(single.getByRole('button', { name: 'Показать заметку' })).toBeVisible();
  await single.getByRole('button', { name: 'Показать заметку' }).click();
  await expect(single.getByRole('button', { name: 'Скрыть заметку' })).toBeVisible();
  const fold = single.getByRole('button', { name: 'Текст для агента' });
  await expect(fold).toHaveAttribute('aria-expanded', 'false');
  await expect(single).not.toContainText('[canterlot]');
  await fold.click();
  await expect(fold).toHaveAttribute('aria-expanded', 'true');
  await expect(single.locator('[data-role="system-body"]')).toContainText('[canterlot] Alarm');

  const file = page.locator('article.system[data-event-kind="alarm"]').filter({ hasText: 'release' });
  await expect(file).toContainText('Появился файл /tmp/gate.rc (2 КиБ)');

  const timeout = page.locator('article.system[data-event-kind="alarm"]').filter({ hasText: 'watch' });
  await expect(timeout).toHaveAttribute('data-tone', 'warning');
  await expect(timeout).toContainText('Истёк срок ожидания');
  const link = timeout.getByRole('link', { name: 'Сборка' });
  await expect(link).toHaveAttribute('href', `/c/${child}`);
  await expect(timeout).toContainText('зависла');

  const batch = page.locator('article.system[data-event-kind="alarm"]').filter({ hasText: 'Сработало 2 будильника' });
  await expect(batch).toContainText('Будильник «gate»');
  await expect(batch).toContainText('Будильник «disk»');

  // Старый будильник без новых полей остаётся прежней тонкой строкой.
  await expect(page.locator('[data-role="alarm"]')).toContainText('Будильник «old»: сработал');
  await noOverflow(page);
});

test('the_alarm_note_and_agent_text_are_separate_tap_targets', async ({ page }) => {
  await boot(page, 'alarm');
  const card = page.locator('article.system[data-event-kind="alarm"]').filter({ hasText: 'Будильник «gate»' }).first();
  await card.scrollIntoViewIfNeeded();
  const note = card.getByRole('button', { name: 'Показать заметку' });
  const fold = card.getByRole('button', { name: 'Текст для агента' });
  await expect(note).toBeVisible();
  await expect(fold).toBeVisible();
  const [noteBox, foldBox] = [(await note.boundingBox())!, (await fold.boundingBox())!];
  // Обе кнопки остаются целями касания, но между ними виден зазор: иначе две заливки
  // сливаются в одну фигуру и читаются одной кнопкой.
  const gaps = clearGap(noteBox, foldBox);
  expect(gaps.length, 'кнопки не соприкасаются').toBeGreaterThan(0);
  expect(Math.min(...gaps), 'между кнопками виден зазор').toBeGreaterThanOrEqual(4);
  expect(Math.min(...gaps), 'зазор не раздувает карточку').toBeLessThanOrEqual(20);
  const target = await tapTarget(page);
  expect(noteBox.height).toBeGreaterThanOrEqual(target);
  expect(foldBox.height).toBeGreaterThanOrEqual(target);
  // Ни у одной кнопки нет своей заливки: как у ссылок карточки, она сливалась бы с соседней.
  for (const box of [note, fold]) {
    const alpha = await box.evaluate((node) => {
      const parts = (getComputedStyle(node).backgroundColor.match(/[\d.]+/g) ?? []).map(Number);
      return parts.length >= 4 ? parts[3]! : 1;
    });
    expect(alpha, 'у кнопки нет собственного фона').toBe(0);
  }
});

test('the_system_card_controls_do_not_merge_or_spread_out', async ({ page }) => {
  // Заметка, действие-ссылка и кнопка сжатия контекста — три вида соседних контролов.
  for (const group of ['alarm', 'machine', 'context'] as const) {
    await boot(page, group);
    const target = await tapTarget(page);
    await expect(page.locator('[data-role="system-controls"] > :nth-child(2)').first()).toBeVisible();
    let seen = 0;
    for (const card of await page.locator('article.system').all()) {
      const controls = card.locator('[data-role="system-controls"] > *');
      if ((await controls.count()) < 2) continue;
      seen += 1;
      const boxes = (await controls.evaluateAll((nodes) =>
        nodes.map((node) => {
          const rect = node.getBoundingClientRect();
          return { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
        })
      )) as Box[];
      for (const [index, box] of boxes.entries()) {
        expect(box.height, `высота контрола ${index}`).toBeGreaterThanOrEqual(target);
        const next = boxes[index + 1];
        if (!next) continue;
        const gaps = clearGap(box, next);
        expect(gaps.length, `${group}: контролы ${index} и ${index + 1} не соприкасаются`).toBeGreaterThan(0);
        expect(Math.min(...gaps), `${group}: зазор между ${index} и ${index + 1}`).toBeGreaterThanOrEqual(4);
        expect(Math.min(...gaps), `${group}: зазор между ${index} и ${index + 1}`).toBeLessThanOrEqual(20);
      }
    }
    expect(seen, `пара контролов в группе ${group}`).toBeGreaterThan(0);
  }
});

test('the_fold_chevron_stays_with_its_label_in_every_service_card', async ({ page }) => {
  // То же раскрытие стоит в будильнике, машине и контексте: стрелка должна читаться частью
  // подписи, а не разделителем между соседними действиями.
  for (const group of ['alarm', 'machine', 'context'] as const) {
    await boot(page, group);
    await expect(page.locator('article.system').filter({ has: page.locator('.text-toggle') }).first()).toBeVisible();
    let seen = 0;
    let compared = 0;
    for (const card of await page.locator('article.system').all()) {
      const toggle = card.locator('.text-toggle');
      if ((await toggle.count()) === 0) continue;
      const label = toggle.locator('.fold-label');
      const chevron = toggle.locator('.chevron');
      await expect(label).not.toBeEmpty();
      const index = await toggle.evaluate((node) => Array.prototype.indexOf.call(node.parentElement!.children, node));
      const controls = card.locator('[data-role="system-controls"] > *');
      const [toggleBox, labelBox, chevronBox] = (await Promise.all([
        toggle.boundingBox(),
        label.boundingBox(),
        chevron.boundingBox()
      ])) as Box[];
      expect(chevronBox.x, `${group}: стрелка после подписи`).toBeGreaterThanOrEqual(
        labelBox.x + labelBox.width - 1
      );
      const labelGap = chevronBox.x - (labelBox.x + labelBox.width);
      expect(labelGap, `${group}: подпись и стрелка рядом`).toBeLessThanOrEqual(12);
      if (index > 0) {
        const previous = (await controls.nth(index - 1).boundingBox())!;
        if (Math.abs(previous.y - toggleBox.y) < 4) {
          compared += 1;
          const controlGap = toggleBox.x - (previous.x + previous.width);
          expect(controlGap, `${group}: до соседнего контрола дальше, чем до стрелки`).toBeGreaterThan(
            labelGap + 4
          );
        }
      }
      seen += 1;
    }
    expect(seen, `раскрытие в группе ${group}`).toBeGreaterThan(0);
    expect(compared, `соседний контрол в группе ${group}`).toBeGreaterThan(0);
  }
});

/** Анимации значка пузыря по именам: Svelte прибавляет к имени ключевых кадров свой хеш. */
const iconMotion = (bubble: Locator) =>
  bubble.locator('[data-role="system-icon"]').evaluate((node) => node.getAnimations({ subtree: true }).map((item) => (item as CSSAnimation).animationName.replace(/^svelte-[a-z0-9]+-/, '')));

test('a_bubble_from_history_stands_drawn_and_nothing_moves', async ({ page }) => {
  await boot(page, 'alarm');
  const bubble = page.locator('article.system[data-event-kind="alarm"]').first();
  await expect(bubble.locator('svg.mark')).toHaveAttribute('data-state', 'dry');
  expect(await iconMotion(bubble)).toEqual([]);
});

test('a_bubble_that_arrives_live_is_inked_once_in_its_own_tone_then_dries', async ({ page }) => {
  // Росчерк идёт с монтирования пузыря и кончается за полсекунды: начало анимаций записывается с первого кадра страницы.
  await page.addInitScript(() => {
    const started: string[] = [];
    (window as unknown as { __started: string[] }).__started = started;
    document.addEventListener('animationstart', (event) => started.push((event as AnimationEvent).animationName), true);
  });
  await boot(page, 'alarm', { fresh: true });
  const bubble = page.locator('article.system[data-event-kind="alarm"]').first();
  await expect(bubble.locator('svg.mark')).toBeVisible();
  const started = () => page.evaluate(() => (window as unknown as { __started: string[] }).__started.map((name) => name.replace(/^svelte-[a-z0-9]+-/, '')));
  await expect.poll(started).toContain('mark-ink');
  // Дважды росчерк не идёт: после оседания цвета значок стоит написанным.
  await expect(bubble.locator('svg.mark')).toHaveAttribute('data-state', 'dry', { timeout: 10_000 });
  expect(await iconMotion(bubble)).toEqual([]);
});

test('the_ink_dries_into_the_tone_of_the_bubble_and_the_motion_off_fallback_is_a_drawn_icon', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await boot(page, 'all', { fresh: true });
  const bubble = page.locator('article.system').first();
  await expect(bubble.locator('svg.mark')).toHaveAttribute('data-state', 'dry');
  expect(await iconMotion(bubble)).toEqual([]);
  // Под любым тоном значок стоит в цвете заголовка пузыря: предупреждение остаётся предупреждением.
  for (const tinted of await page.locator('article.system[data-tone="warning"], article.system[data-tone="danger"]').all()) {
    const colors = await tinted.evaluate((node) => [getComputedStyle(node.querySelector('svg.mark')!).color, getComputedStyle(node.querySelector('.title')!).color]);
    expect(colors[0]).toBe(colors[1]);
  }
});

test('machine_bubbles_show_numbers_and_open_the_machines_screen', async ({ page }) => {
  await boot(page, 'machine');
  const disk = page.locator('article.system[data-event-kind="machine"]').filter({ hasText: 'Машина dev-vm: мало места' });
  await disk.scrollIntoViewIfNeeded();
  await expect(disk).toContainText('21,4 ГиБ (8 %)');
  await expect(disk).toContainText('/w/target');
  await expect(disk).toContainText('Баннер связи');

  const critical = page.locator('article.system[data-event-kind="machine"]').filter({ hasText: 'критически мало места' });
  await expect(critical).toHaveAttribute('data-tone', 'warning');

  const memory = page.locator('article.system[data-event-kind="machine"]').filter({ hasText: 'мало памяти' });
  await expect(memory).toHaveAttribute('data-tone', 'warning');
  await expect(memory).toContainText('2,9 ГиБ из 29,8 ГиБ');
  await expect(memory).toContainText('Toolkit');

  const leaked = page.locator('article.system[data-event-kind="machine"]').filter({ hasText: 'остались процессы' });
  await expect(leaked).toContainText('cargo');
  const lease = page.locator('article.system[data-event-kind="machine"]').filter({ hasText: 'аренда занята долго' });
  await expect(lease).toContainText('95 мин');

  // Старое предупреждение без details падает на прежний текстовый блок.
  await expect(page.locator('[data-event-kind="legacy"]')).toContainText('Сообщение хаба');

  await disk.getByRole('link', { name: 'Открыть машины' }).click();
  await expect(page).toHaveURL(/\/machines$/);
});

test('context_bubbles_offer_the_same_compaction_request_in_the_own_session', async ({ page }) => {
  await boot(page, 'context');
  const first = page.locator('article.system[data-event-kind="context"]').filter({ hasText: '85 %' });
  await first.scrollIntoViewIfNeeded();
  await expect(first).toContainText('Контекст заполнен на 85 %');
  await expect(first).toHaveAttribute('data-tone', 'neutral');
  const second = page.locator('article.system[data-event-kind="context"]').filter({ hasText: '92 %' });
  await expect(second).toHaveAttribute('data-tone', 'warning');
  const foreign = page.locator('article.system[data-event-kind="context"]').filter({ hasText: '88 %' });
  await expect(foreign.getByRole('button', { name: 'Сжать контекст' })).toHaveCount(0);

  let posts = 0;
  await page.route('**/api/v1/conversations/*/compact', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    posts += 1;
    await route.fulfill({
      status: 200,
      headers: { 'content-type': 'application/json' },
      json: { requested: { command_id: '00000000-0000-4000-8000-000000000099' } }
    });
  });
  await first.getByRole('button', { name: 'Сжать контекст' }).click();
  await expect.poll(() => posts).toBe(1);
});

test('limit_bubbles_name_the_provider_window_reset_and_the_stuck_delegate', async ({ page }) => {
  const { child } = await boot(page, 'limit');
  const resumed = page.locator('article.system[data-event-kind="limit_resumed"]');
  await resumed.scrollIntoViewIfNeeded();
  await expect(resumed).toContainText('Лимит Claude сброшен — продолжаю');
  await expect(resumed).toContainText('неделя');

  const paused = page.locator('article.system[data-event-kind="quota_paused"]');
  await expect(paused).toContainText('Помощник «Сборка» упёрся в лимит Claude (неделя)');
  await expect(paused).toContainText('сброс');
  await expect(paused.getByRole('link', { name: /Помощник «Сборка»/ })).toHaveAttribute('href', `/c/${child}`);

  const note = page.locator('article.system[data-event-kind="quota_note"]');
  await expect(note).toContainText('Лимит сброшен, но «Сборка» сам не продолжит');
  await expect(note).toContainText('Отправьте ему сообщение или остановите');
  await expect(note.locator('.action')).toHaveCount(0);
  await expect(note.getByRole('button', { name: 'Отправьте ему сообщение или остановите' })).toHaveCount(0);
  await expect(note.getByRole('link', { name: /Лимит сброшен/ })).toHaveAttribute('href', `/c/${child}`);
});

test('a_session_message_is_an_agent_bubble_with_a_bookmark_and_no_edit', async ({ page }) => {
  const { child } = await boot(page, 'message');
  const message = page.locator('[data-role="session-message"]').first();
  await message.scrollIntoViewIfNeeded();
  await expect(message).toContainText('Сессия «Сборка»');
  await expect(message).toContainText('Sonnet 5');
  await expect(message).not.toContainText('[canterlot]');
  await expect(message).toContainText('Проверь миграцию');
  await expect(message.locator('strong')).toHaveText('миграцию');
  await expect(message.locator('p code')).toHaveText('индексы');
  // Короткое сообщение не сворачивается: кнопка появляется только при переполнении высоты.
  await expect(message.getByRole('button', { name: 'Показать полностью' })).toHaveCount(0);
  await expect(message.getByRole('link', { name: 'Сессия «Сборка»' })).toHaveAttribute('href', `/c/${child}`);
  await expect(message.getByRole('button', { name: 'Изменить' })).toHaveCount(0);
  const bookmark = message.getByRole('button', { name: 'В избранное' });
  await expect(bookmark).toHaveAttribute('aria-pressed', 'false');
  const headerBox = (await message.locator('header').boundingBox())!;
  const toggleBox = (await bookmark.boundingBox())!;
  expect(toggleBox.y + toggleBox.height / 2).toBeGreaterThanOrEqual(headerBox.y);
  expect(toggleBox.y + toggleBox.height / 2).toBeLessThanOrEqual(headerBox.y + headerBox.height);
  await bookmark.click();
  await expect(message.getByRole('button', { name: 'Убрать из избранного' })).toHaveAttribute('aria-pressed', 'true');

  // Владелец и дальше видит свой пузырь с правкой, чужое сообщение — нет.
  await expect(page.locator('article.bubble').filter({ hasText: 'Моё сообщение' })).toBeVisible();
});

test('a_spawn_brief_renders_markdown_and_folds_by_height', async ({ page }) => {
  await boot(page, 'message');
  const brief = page.locator('[data-role="session-message"][data-origin="spawn"]');
  await brief.scrollIntoViewIfNeeded();
  await expect(brief).toContainText('Задание от сессии «Оркестратор»');
  // Разметка брифа — та же, что у ответа агента, а не сырой markdown.
  await expect(brief.locator('h2')).toHaveText('Goal');
  await expect(brief.locator('strong')).toHaveText('Your area:');
  await expect(brief.locator('p code').first()).toHaveText('timeline');
  await expect(brief.locator('li')).toHaveCount(2);
  await expect(brief).not.toContainText('## Goal');
  await expect(brief).not.toContainText('**Your area:**');

  const body = brief.locator('.body');
  const pre = brief.locator('pre');
  const toggle = brief.getByRole('button', { name: 'Показать полностью' });
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await expect(pre).toContainText('Шаг 18');
  // Забор не разрезан: блок кода один, прячет его высота тела карточки.
  await expect(pre).toHaveCount(1);
  const foldedBox = (await body.boundingBox())!;
  const foldedPre = (await pre.boundingBox())!;
  expect(foldedPre.y + foldedPre.height).toBeGreaterThan(foldedBox.y + foldedBox.height);

  await toggle.click();
  await expect(brief.getByRole('button', { name: 'Свернуть' })).toHaveAttribute('aria-expanded', 'true');
  const expandedBox = (await body.boundingBox())!;
  const expandedPre = (await pre.boundingBox())!;
  expect(expandedBox.height).toBeGreaterThan(foldedBox.height);
  expect(expandedPre.y + expandedPre.height).toBeLessThanOrEqual(expandedBox.y + expandedBox.height + 1);

  await brief.getByRole('button', { name: 'Свернуть' }).click();
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  expect((await body.boundingBox())!.height).toBeLessThanOrEqual(foldedBox.height + 1);
});

test('the_service_bubbles_do_not_overflow_320_and_412_and_have_distinct_names', async ({ page }) => {
  await boot(page, 'all');
  for (const width of [320, 412]) {
    await page.setViewportSize({ width, height: 900 });
    await noOverflow(page);
    for (const bubble of await page.locator('article.system').all()) {
      const names = await bubble.locator('a,button').evaluateAll((nodes) =>
        nodes.map((node) => (node.getAttribute('aria-label') || node.textContent || '').trim())
      );
      for (const name of names) expect(name, `пустое имя в ${width}px`).not.toBe('');
      expect(new Set(names).size, `повтор имени в ${width}px: ${names.join(', ')}`).toBe(names.length);
    }
  }
});
