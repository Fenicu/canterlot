import type { Locator, Page } from '@playwright/test';
import type { HistoryInput } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { seedTaigaCards } from '../../testing/fixtures/taiga-cards';
import { bootDictation, startDictation } from './dictation-fixture';
import { screenshotPng, settled, viewer } from './image-viewer-fixture';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000e1';
const AT = '2026-11-16T10:00:00.000Z';
const WIDTHS = [360, 390, 412];
const phoneOnly = (info: { project: { name: string } }) => test.skip(info.project.name !== 'phone');

async function theme(page: Page, width: number): Promise<void> {
  await page.addInitScript((value) => localStorage.setItem('canterlot.theme', value), width === 390 ? 'light' : 'dark');
}

async function scrollFeedToTop(page: Page): Promise<void> {
  await page.locator('.feed').evaluate((node) => {
    node.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 }));
    node.scrollTop = 0;
  });
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
}

/** Число строк, которые текст занимает внутри элемента. */

test.describe('F-09 owner bubble with several images', () => {
  const image = (id: string, n: number) => ({
    attachment_id: id,
    name: `screenshot-${n}.png`,
    mime: 'image/png',
    size: 120_000,
    sha256: '',
    image: true
  });

  async function boot(page: Page): Promise<void> {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const id = seedConversation(api, { title: 'Снимки' });
    const many: HistoryInput = {
      kind: 'input',
      id: api.id(),
      idempotency_key: 'f09-many',
      state: 'accepted',
      content: [{ type: 'text', text: 'Три экрана и протокол' }],
      attachments: [
        image('f09-1', 1),
        image('f09-2', 2),
        image('f09-3', 3),
        { attachment_id: 'f09-doc', name: 'протокол-встречи-2026-11-16.md', mime: 'text/markdown', size: 4096, sha256: '', image: false }
      ],
      created_seq: '1',
      feed_seq: '1',
      created_at: AT
    } as HistoryInput;
    const one: HistoryInput = {
      kind: 'input',
      id: api.id(),
      idempotency_key: 'f09-one',
      state: 'accepted',
      content: [{ type: 'text', text: 'Один экран' }],
      attachments: [image('f09-4', 4)],
      created_seq: '2',
      feed_seq: '2',
      created_at: AT
    } as HistoryInput;
    api.histories.set(id, [many, one]);
    api.bindConversation(id);
    await installChatRoutes(page, hub, api, OWNER);
    await page.route('**/api/v1/attachments/*/content*', (route) => route.fulfill({ contentType: 'image/png', body: screenshotPng(720, 450, 210) }));
    await page.route('**/api/v1/attachments/*/preview*', (route) => route.fulfill({ contentType: 'image/png', body: screenshotPng(180, 112, 210) }));
    await page.goto(`/c/${id}`);
    await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('article.bubble a.thumb')).toHaveCount(4);
    await scrollFeedToTop(page);
  }

  for (const width of WIDTHS) {
    test(`several_thumbnails_share_rows_at_${width}`, async ({ page }, info) => {
      phoneOnly(info);
      await page.setViewportSize({ width, height: 892 });
      await theme(page, width);
      await boot(page);
      const many = page.locator('article.bubble').filter({ hasText: 'Три экрана и протокол' });
      const thumbs = many.locator('a.thumb');
      await expect(thumbs).toHaveCount(3);
      // Две миниатюры делят строку, а не встают одна под другой.
      const perRow = await thumbs.evaluateAll((nodes) => {
        const tops = nodes.map((node) => Math.round(node.getBoundingClientRect().top));
        const rows = new Map<number, number>();
        for (const top of tops) rows.set(top, (rows.get(top) ?? 0) + 1);
        return [...rows.values()];
      });
      expect(perRow, JSON.stringify(perRow)).toEqual([2, 1]);
      for (const box of await thumbs.evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect().width))) {
        expect(box).toBeLessThanOrEqual(160.5);
        expect(box).toBeGreaterThan(120);
      }
      // Одна картинка по-прежнему 160 px.
      const single = page.locator('article.bubble').filter({ hasText: 'Один экран' }).locator('a.thumb');
      const singleWidth = await single.evaluate((node) => node.getBoundingClientRect().width);
      expect(singleWidth).toBeGreaterThan(159.5);
      expect(singleWidth).toBeLessThan(161);
      // Файл рядом с картинками сохраняет свою плитку во всю ширину, а не половину строки.
      const file = many.locator('li').filter({ has: page.locator('.document-tile') }).first();
      await expect(file).toHaveCount(1);
      const sizes = await many.locator('ul.files').evaluate((list) => ({
        list: list.getBoundingClientRect().width,
        file: list.querySelector('li:has(.document-tile)')!.getBoundingClientRect().width
      }));
      expect(sizes.file).toBeGreaterThan(sizes.list * 0.6);
      // Просмотрщик открывается касанием миниатюры.
      if (width === 412) {
        await thumbs.first().click();
        await expect(viewer(page)).toBeVisible({ timeout: 10_000 });
        await settled(page);
        await page.keyboard.press('Escape');
        await expect(viewer(page)).toHaveCount(0);
      }
    });
  }
});

test.describe('F-10 session sheet trash row', () => {
  async function openSheet(page: Page): Promise<Locator> {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    const ids = seedMockupSessions(api);
    await installChatRoutes(page, hub, api, ids.pinned);
    await page.goto('/sessions');
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await page.getByRole('button', { name: /^Действия:/ }).first().click();
    await expect(page.getByRole('dialog').first()).toBeVisible();
    return page.getByRole('dialog').first();
  }

  async function measure(row: Locator) {
    return row.evaluate((el) => {
      const label = el.querySelector('span')!;
      const range = document.createRange();
      range.selectNodeContents(label);
      const style = getComputedStyle(el);
      return {
        labelLines: new Set([...range.getClientRects()].filter((rect) => rect.width > 0).map((rect) => Math.round(rect.top))).size,
        height: el.getBoundingClientRect().height,
        radius: parseFloat(style.borderTopLeftRadius)
      };
    });
  }

  for (const width of WIDTHS) {
    test(`trash_label_stays_on_one_line_at_${width}`, async ({ page }, info) => {
      phoneOnly(info);
      await page.setViewportSize({ width, height: 892 });
      await theme(page, width);
      const dialog = await openSheet(page);
      const trash = dialog.getByRole('button', { name: /^Переместить в корзину/ });
      const archive = dialog.getByRole('button', { name: /^В архив/ });
      const trashBox = await measure(trash);
      const archiveBox = await measure(archive);
      expect(trashBox.labelLines).toBe(1);
      expect(archiveBox.labelLines).toBe(1);
      expect(Math.abs(trashBox.height - archiveBox.height)).toBeLessThanOrEqual(1);
      expect(trashBox.radius).toBeLessThan(999);
      expect(archiveBox.radius).toBeLessThan(999);
    });
  }

  test('desktop_popover_trash_label_stays_on_one_line', async ({ page }, info) => {
    test.skip(info.project.name === 'phone');
    await theme(page, 412);
    const dialog = await openSheet(page);
    const trash = dialog.getByRole('button', { name: /^Переместить в корзину/ });
    const archive = dialog.getByRole('button', { name: /^В архив/ });
    const trashBox = await measure(trash);
    const archiveBox = await measure(archive);
    expect(trashBox.labelLines).toBe(1);
    expect(trashBox.height).toBeCloseTo(archiveBox.height, 0);
    expect(trashBox.radius).toBeLessThan(999);
  });
});

test.describe('F-13 folded Taiga comment', () => {
  async function reveal(page: Page, target: Locator): Promise<Locator> {
    const frame = () => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
    for (let step = 0; step < 60; step++) {
      if ((await target.count()) > 0) {
        try {
          await target.first().scrollIntoViewIfNeeded({ timeout: 2_000 });
          await frame();
          if ((await target.count()) > 0) return target.first();
        } catch {
          // Строку сняли с окна, пока до неё крутили.
        }
      }
      await page.locator('.feed').evaluate((node) => {
        node.scrollTop += node.clientHeight * 0.6;
      });
      await frame();
    }
    await expect(target.first()).toBeAttached();
    return target.first();
  }

  for (const width of WIDTHS) {
    test(`folded_comment_shows_six_whole_lines_at_${width}`, async ({ page }, info) => {
      phoneOnly(info);
      await page.setViewportSize({ width, height: 892 });
      await theme(page, width);
      const hub = new FakeHub();
      const api = new FakeHubApi(hub);
      const seed = seedTaigaCards(api);
      await installChatRoutes(page, hub, api, OWNER);
      await page.goto(`/c/${seed.id}`);
      await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 20_000 });
      await expect(page.locator('.feed [data-row]').first()).toBeVisible({ timeout: 15_000 });
      const card = await reveal(page, page.locator(`[data-key="block:${seed.blocks.comment}"] .feed-card`));
      const quote = card.locator('[data-role="taiga-comment"]');
      await expect(quote).toBeVisible();

      const box = await quote.evaluate((el) => {
        const clamped = el.querySelector(':scope > .quote-text.clamped') as HTMLElement | null;
        if (!clamped) return null;
        const style = getComputedStyle(clamped);
        const rect = clamped.getBoundingClientRect();
        const range = document.createRange();
        range.selectNodeContents(clamped);
        const rects = [...range.getClientRects()].filter((line) => line.width > 0);
        const padBottom = parseFloat(style.paddingBottom);
        const contentBottom = rect.bottom - padBottom;
        const inside = rects.filter((line) => line.top < contentBottom - 0.5);
        return {
          padTop: parseFloat(style.paddingTop),
          padBottom,
          lineHeight: parseFloat(style.lineHeight),
          clientHeight: clamped.clientHeight,
          contentBottom,
          insideBottom: inside.length > 0 ? Math.max(...inside.map((line) => line.bottom)) : 0,
          hiddenTop: rects.filter((line) => line.top >= contentBottom - 0.5).length > 0 ? Math.min(...rects.filter((line) => line.top >= contentBottom - 0.5).map((line) => line.top)) : null,
          lines: new Set(rects.map((line) => Math.round(line.top))).size
        };
      });
      expect(box).not.toBeNull();
      // Рамка обрезки идёт по строке сетки: у самого элемента с зажимом нет отступов блока.
      expect(box!.padTop).toBe(0);
      expect(box!.padBottom).toBe(0);
      expect(box!.lines).toBeGreaterThanOrEqual(6);
      // Ровно шесть целых строк: высота кратна межстрочному, ни одна видимая строка не выходит за край.
      expect(Math.abs(box!.clientHeight - box!.lineHeight * 6)).toBeLessThanOrEqual(1);
      expect(box!.insideBottom).toBeLessThanOrEqual(box!.contentBottom + 0.5);

      // Раскрытие по-прежнему показывает весь текст.
      await card.locator('[data-role="taiga-comment-toggle"]').click();
      await expect(quote).not.toHaveClass(/clamped/);
      await expect(quote).toContainText('Проверка на 320 px прошла');
    });
  }
});

test.describe('F-14 dictation panel while listening', () => {
  const COMMITTED =
    'Открой ветку и проверь маршрутизацию кандидатов: сначала прогони тесты хаба, потом тесты воркера, потом собери клиент и посмотри, что лента не прыгает при раскрытии рассуждения. После этого обнови историю изменений и напиши мне, что получилось, коротко и по делу. ';
  const TENTATIVE = 'И не забудь про скриншоты на телефоне';

  async function boot(page: Page, texts: { afterMs: number; committed: string; tentative: string }[]) {
    await bootDictation(page, {
      pref: 'on',
      configure: (dictation) => {
        dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
        dictation.script = { texts };
      }
    });
    await startDictation(page);
  }

  for (const width of WIDTHS) {
    test(`newest_words_stay_in_view_at_${width}`, async ({ page }, info) => {
      phoneOnly(info);
      await page.setViewportSize({ width, height: 892 });
      await theme(page, width);
      await boot(page, [{ afterMs: 20, committed: COMMITTED, tentative: TENTATIVE }]);
      const transcript = page.locator('[data-dictation-transcript]');
      await expect(page.locator('[data-dictation-tentative]')).toBeVisible({ timeout: 10_000 });
      await expect(page.locator('[data-dictation-tentative]')).toHaveText(TENTATIVE);
      // Хвост доезжает после кадра с новым текстом.
      await page.waitForTimeout(300);
      const box = await transcript.evaluate((el) => {
        const style = getComputedStyle(el);
        const tentative = el.querySelector('[data-dictation-tentative]');
        const rect = el.getBoundingClientRect();
        return {
          scrollTop: el.scrollTop,
          scrollHeight: el.scrollHeight,
          clientHeight: el.clientHeight,
          padTop: parseFloat(style.paddingTop),
          padBottom: parseFloat(style.paddingBottom),
          lineHeight: parseFloat(style.lineHeight),
          contentBottom: rect.bottom - parseFloat(style.paddingBottom),
          tentativeBottom: tentative ? tentative.getBoundingClientRect().bottom : null
        };
      });
      expect(box.scrollTop).toBeGreaterThan(0);
      expect(box.tentativeBottom).not.toBeNull();
      expect(box.tentativeBottom!).toBeLessThanOrEqual(box.contentBottom + 1);
      // В поле влезают целые строки: высота содержимого кратна межстрочному.
      const lines = (box.clientHeight - box.padTop - box.padBottom) / box.lineHeight;
      expect(Math.abs(lines - Math.round(lines))).toBeLessThan(0.02);
      expect(box.scrollHeight).toBeGreaterThan(box.clientHeight);
    });
  }

  test('owner_scrolling_up_stops_the_follow', async ({ page }, info) => {
    phoneOnly(info);
    await page.setViewportSize({ width: 412, height: 892 });
    await theme(page, 412);
    await boot(page, [
      { afterMs: 20, committed: COMMITTED, tentative: TENTATIVE },
      { afterMs: 1_600, committed: `${COMMITTED}${COMMITTED}`, tentative: 'Ещё одна новая фраза' }
    ]);
    const transcript = page.locator('[data-dictation-transcript]');
    await expect(page.locator('[data-dictation-tentative]')).toHaveText(TENTATIVE, { timeout: 10_000 });
    await page.waitForTimeout(300);
    expect(await transcript.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
    // Владелец ушёл вверх: новые слова больше не тянут поле вниз.
    await transcript.evaluate((el) => {
      el.scrollTop = 0;
      el.dispatchEvent(new Event('scroll'));
    });
    await expect(page.locator('[data-dictation-tentative]')).toHaveText('Ещё одна новая фраза');
    await page.waitForTimeout(200);
    expect(await transcript.evaluate((el) => el.scrollTop)).toBe(0);
  });
});

test.describe('F-17 routing candidate rows', () => {
  for (const width of WIDTHS) {
    test(`name_and_meta_do_not_break_mid_word_at_${width}`, async ({ page }, info) => {
      phoneOnly(info);
      await page.setViewportSize({ width, height: 892 });
      await theme(page, width);
      const hub = new FakeHub();
      const api = new FakeHubApi(hub);
      const ids = seedMockupSessions(api);
      await installChatRoutes(page, hub, api, ids.pinned);
      await page.goto('/settings/routing');
      await expect(page.locator('[data-routing-editor][data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
      const candidate = page.locator('[data-task-key="bugfix"] li.candidate').first();
      const name = candidate.locator('.name');
      await expect(name).toBeVisible();

      const words = await name.evaluate((el) => {
        const found: { text: string; top: number; left: number; right: number }[] = [];
        const broken: string[] = [];
        const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
        for (let node = walker.nextNode(); node; node = walker.nextNode()) {
          const text = node.nodeValue ?? '';
          for (const match of text.matchAll(/\S+/g)) {
            const range = document.createRange();
            range.setStart(node, match.index!);
            range.setEnd(node, match.index! + match[0].length);
            const rects = [...range.getClientRects()].filter((rect) => rect.width > 0);
            const tops = new Set(rects.map((rect) => Math.round(rect.top)));
            if (tops.size > 1) broken.push(`${match[0]} ×${tops.size}`);
            for (const rect of rects) found.push({ text: match[0], top: Math.round(rect.top), left: rect.left, right: rect.right });
          }
        }
        // Точка-разделитель не должна оставаться последней на строке.
        const dangling = found
          .filter((word) => word.text === '·')
          .filter((dot) => !found.some((word) => word.text !== '·' && word.top === dot.top && word.left > dot.right - 1));
        return { broken, dangling: dangling.length, rows: new Set(found.map((word) => word.top)).size };
      });
      // Разрыв слова виден как несколько строковых прямоугольников у одного слова.
      expect(words.broken, JSON.stringify(words)).toEqual([]);
      expect(words.dangling, JSON.stringify(words)).toBe(0);
      // Имя с метаданными занимает не больше трёх строк: имя и мета.
      expect(words.rows).toBeLessThanOrEqual(3);
      // Кнопки не выходят за строку.
      const row = await candidate.locator('.row').evaluate((el) => {
        const box = el.getBoundingClientRect();
        return {
          width: box.width,
          scrollWidth: el.scrollWidth,
          clientWidth: el.clientWidth,
          buttons: [...el.querySelectorAll('button')].map((button) => {
            const rect = button.getBoundingClientRect();
            return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom, label: button.getAttribute('aria-label') };
          })
        };
      });
      expect(row.buttons.length).toBeGreaterThanOrEqual(3);
      expect(row.scrollWidth).toBeLessThanOrEqual(row.clientWidth + 1);
      for (const button of row.buttons) {
        expect(button.left).toBeGreaterThanOrEqual(-0.5);
        expect(button.right).toBeLessThanOrEqual(width + 0.5);
      }
    });
  }
});
