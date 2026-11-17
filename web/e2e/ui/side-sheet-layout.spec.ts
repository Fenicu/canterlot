import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryBlock } from '../../testing/fake-hub/state';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d7';
const LONG_TITLE = 'Локальная LLM: замеры памяти при контексте 128k и совещания по четырём моделям';
const ANSWER =
  'Оценка:\n\n| Шаг | Время |\n| --- | --- |\n| Замеры памяти при контексте 128k | ~6 мин |\n| Совещания: 4 модели × 3 записи | ~40 мин |\n\n' +
  'Половина оставшегося времени уходит на 9B в режиме размышлений.\n\nСократить?';
const SHOTS = process.env.SIDE_SHOTS;

function answer(api: FakeHubApi, text: string, seq: number): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text, rev: 1, state: 'final' });
  return {
    kind: 'block',
    id,
    block_kind: 'text',
    content: text,
    content_bytes: text.length,
    content_truncated: false,
    created_seq: String(seq),
    created_at: '2026-11-17T18:05:00.000Z',
    rev: 1,
    state: 'final',
    agent: 'agent',
    model: 'opus',
    tool_name: null,
    summary: null,
    parent_block_id: null,
    input_command_id: null,
    line_count: 8
  } as HistoryBlock;
}

/** Родитель и его копия в стороне с одинаковой лентой: так основной чат служит эталоном. */
async function openSide(page: Page, baseURL: string, answers = 1, theme: 'dark' | 'light' = 'dark'): Promise<{ sheet: Locator; mainGap: number }> {
  await page.addInitScript((value) => localStorage.setItem('canterlot.theme', value), theme);
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const parent = seedConversation(api, { title: LONG_TITLE });
  api.details.set(parent, { ...(api.details.get(parent) as Record<string, unknown>), turn_recorded: true, side_of: null, execution: null });
  const feed = () => Array.from({ length: answers }, (_, i) => answer(api, ANSWER, i + 1));
  api.histories.set(parent, feed());
  api.bindConversation(parent);
  const create = api.createSideChat.bind(api);
  api.createSideChat = (id: string) => {
    const out = create(id);
    api.histories.set(out.conversation_id, feed());
    return out;
  };
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`${baseURL}/c/${parent}`);
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText('Сократить?').first()).toBeVisible();
  const mainGap = await gap(page.locator('.screen'));

  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await page.getByRole('menuitem', { name: 'Спросить в стороне' }).click();
  const sheet = page.locator('dialog[open], aside.side-panel').filter({ has: page.getByRole('heading', { name: /в стороне/ }) });
  await expect(sheet).toBeVisible({ timeout: 15_000 });
  await expect(sheet.getByText('Сократить?').last()).toBeVisible();
  await expect(sheet.getByRole('textbox', { name: 'Сообщение' })).toBeVisible();
  await sheet.evaluate((node) => Promise.all(node.getAnimations().map((animation) => animation.finished)));
  return { sheet, mainGap };
}

/** Зазор между низом последней карточки ленты и капсулой поля при ленте, прокрученной до конца. */
async function gap(scope: Locator): Promise<number> {
  return scope.evaluate(async (root) => {
    const feed = root.querySelector<HTMLElement>('.feed')!;
    feed.scrollTop = feed.scrollHeight;
    await new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
    const cards = [...root.querySelectorAll<HTMLElement>('.feed [data-row] article')];
    const last = cards.at(-1)!.getBoundingClientRect();
    const capsule = root.querySelector<HTMLElement>('.capsule')!.getBoundingClientRect();
    return capsule.top - last.bottom;
  });
}

async function shot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return;
  await page.screenshot({ path: `${SHOTS}/${name}-${test.info().project.name}.png` });
}

test.describe('side sheet layout', () => {
  test.beforeEach(async ({ page }, testInfo) => {
    if (testInfo.project.name === 'phone') await page.setViewportSize({ width: 360, height: 780 });
  });

  test('side_header_is_one_line_with_quiet_actions', async ({ page }, testInfo) => {
    const { sheet } = await openSide(page, testInfo.project.use.baseURL as string, 1, 'light');
    await shot(page, 'after-side-360-light');
    const heading = sheet.getByRole('heading', { name: /в стороне/ });
    const box = (await heading.boundingBox())!;
    const lineHeight = await heading.evaluate((node) => parseFloat(getComputedStyle(node).lineHeight));
    // Длинное имя сессии режется многоточием, а не переносится.
    expect(box.height).toBeLessThan(lineHeight * 1.5);
    expect(await heading.locator('.name').evaluate((node) => node.scrollWidth > node.clientWidth)).toBe(true);
    await expect(heading.locator('.marker')).toBeVisible();

    const restart = sheet.getByRole('button', { name: 'Начать заново' });
    const close = sheet.getByRole('button', { name: /^Закрыть/ });
    const restartBox = (await restart.boundingBox())!;
    expect(box.x + box.width).toBeLessThanOrEqual(restartBox.x + 0.5);
    for (const button of [restart, close]) {
      const b = (await button.boundingBox())!;
      expect(b.width).toBeGreaterThanOrEqual(44);
      expect(b.height).toBeGreaterThanOrEqual(44);
    }
    // Тихая кнопка без рамки-пилюли.
    expect(await restart.evaluate((node) => getComputedStyle(node).borderTopWidth)).toBe('0px');
  });

  test('side_feed_scrolls_below_header_not_under_it', async ({ page }, testInfo) => {
    const { sheet } = await openSide(page, testInfo.project.use.baseURL as string, 6);
    const header = (await sheet.locator('header').first().boundingBox())!;
    const feed = sheet.locator('.feed');
    const feedBox = (await feed.boundingBox())!;
    expect(feedBox.y).toBeGreaterThanOrEqual(header.y + header.height - 0.5);
    // Прокручивает лента со своими гаснущими краями, а не тело листа под шапкой.
    expect(await feed.evaluate((node) => node.scrollHeight > node.clientHeight)).toBe(true);
    expect(await feed.evaluate((node) => getComputedStyle(node).maskImage)).toContain('gradient');
    const scrollers = await sheet.evaluate((root) =>
      [...root.querySelectorAll<HTMLElement>('*')].filter((node) => {
        if (node.classList.contains('feed') || node.closest('.feed, .composer')) return false;
        const style = getComputedStyle(node);
        return /auto|scroll/.test(style.overflowY) && node.scrollHeight > node.clientHeight + 1;
      }).length
    );
    expect(scrollers).toBe(0);
  });

  test('side_composer_sits_right_below_last_answer', async ({ page }, testInfo) => {
    const { sheet, mainGap } = await openSide(page, testInfo.project.use.baseURL as string, 6);
    const sideGap = await gap(sheet);
    expect(Math.abs(sideGap - mainGap)).toBeLessThanOrEqual(4);
  });

  test('side_feed_has_no_bookmarks_and_its_own_placeholder', async ({ page }, testInfo) => {
    const { sheet } = await openSide(page, testInfo.project.use.baseURL as string);
    await expect(sheet.locator('[data-role="answer-footer"]')).toBeVisible();
    await expect(sheet.getByRole('button', { name: 'В избранное' })).toHaveCount(0);
    await expect(sheet.getByRole('textbox', { name: 'Сообщение' })).toHaveAttribute('placeholder', 'Вопрос в стороне…');
    await expect(page.locator('.screen').getByRole('textbox', { name: 'Сообщение' }).first()).toHaveAttribute('placeholder', 'Сообщение агенту…');
  });

  test('side_table_row_stays_inside_a_visible_card', async ({ page }, testInfo) => {
    const { sheet } = await openSide(page, testInfo.project.use.baseURL as string, 1, 'dark');
    await shot(page, 'after-side-360-dark');
    const measured = await sheet.evaluate((root) => {
      const row = root.querySelector<HTMLElement>('.prose tbody tr')!;
      const card = row.closest<HTMLElement>('article')!;
      const canvas = root.matches('dialog') ? root : root.closest<HTMLElement>('aside')!;
      const r = row.getBoundingClientRect();
      const c = card.getBoundingClientRect();
      return {
        cardBg: getComputedStyle(card).backgroundColor,
        canvasBg: getComputedStyle(canvas).backgroundColor,
        left: r.left - c.left,
        right: c.right - r.right
      };
    });
    // Подсветка строки лежит в карточке ответа, а карточка видна на фоне листа, как в основной ленте.
    expect(measured.cardBg).not.toBe(measured.canvasBg);
    expect(measured.left).toBeGreaterThanOrEqual(8);
    expect(measured.right).toBeGreaterThanOrEqual(8);
  });
});
