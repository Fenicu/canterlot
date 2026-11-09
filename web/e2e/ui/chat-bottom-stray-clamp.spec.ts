import type { Page } from '@playwright/test';
import type { HistoryBlock, HistoryInput, HistoryItem } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedFeedCards, streamCall, type FeedCardsSeed } from '../../testing/fixtures/feed-cards';
import { expect, test } from './fixtures';

// Открытый чат вставал выше низа и там оставался. Пока лента прижата, хвост на миг становится
// короче: окно строк перестраивается, карточка сменяет вид, текст пересобирается. Если раскладку
// в этот миг снимает любой код или сам браузер, scrollTop прижимается к меньшему пределу, а к
// событию прокрутки высота уже прежняя или больше. Контроллер читал такой подъём как прокрутку
// владельца и отпускал низ насовсем: зазор оставался, последняя строка уходила под поле ввода.
const OWNER = '00000000-0000-4000-8000-0000000000e1';
const AT = '2026-11-09T09:00:00.000Z';
const TAIL = 'Последняя строка ответа';

function answer(n: number): string {
  const parts = [`**Итог ${n}.** ` + 'Сверка выкатки и журналов воркера. '.repeat(8)];
  for (let i = 0; i < 6; i++) parts.push(`- **Пункт ${i}.** ` + 'Подробности шага с пояснением. '.repeat(5));
  parts.push('```ts', ...Array.from({ length: 10 }, (_, i) => `const v${i} = f(${i});`), '```');
  parts.push('| файл | строки |\n| --- | --- |\n| a.ts | 12 |');
  return parts.join('\n\n');
}

function text(api: FakeHubApi, body: string): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text: body, rev: 1, state: 'final' });
  return { kind: 'block', id, block_kind: 'text', content: body, content_bytes: body.length, content_truncated: false, created_seq: '0', created_at: AT, rev: 1, state: 'final', agent: 'orchestrator', model: 'opus', tool_name: null, summary: null, parent_block_id: null, input_command_id: null, line_count: 1 } as HistoryBlock;
}

/** Долгий оркестратор: старые ответы, карточки инструментов и длинный последний ответ. */
function seed(api: FakeHubApi): FeedCardsSeed {
  const s = seedFeedCards(api);
  const old: HistoryItem[] = [];
  for (let turn = 0; turn < 60; turn++) {
    old.push({ kind: 'input', id: api.id(), idempotency_key: `old-${turn}`, state: 'accepted', content: [{ type: 'text', text: `Вопрос ${turn}` }], attachments: [], created_seq: '0', feed_seq: '0', created_at: AT } as HistoryInput);
    old.push(text(api, turn % 2 ? answer(turn) : 'Короткий ответ. '.repeat(12)));
  }
  const all = [...old, ...api.histories.get(s.id)!, text(api, `${answer(999)}\n\n${TAIL}`)];
  all.forEach((item, index) => {
    item.created_seq = String(index - all.length);
    if (item.kind === 'input') item.feed_seq = item.created_seq;
  });
  api.histories.set(s.id, all);
  api.details.get(s.id)!.activity = { state: 'working', evidence: { phase: 'generating' }, since: new Date().toISOString() };
  (api.details.get(s.id) as Record<string, unknown>).execution = { state: 'running', generation: 1 };
  api.bindConversation(s.id);
  return s;
}

/** Зазор до низа после того, как лента успокоилась: несколько кадров подряд без изменений. */
async function settledGap(page: Page): Promise<number> {
  return page.locator('.feed').evaluate(async (el) => {
    const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));
    let last = -1;
    let still = 0;
    for (let i = 0; i < 600 && still < 20; i++) {
      await frame();
      const gap = el.scrollHeight - el.scrollTop - el.clientHeight;
      still = Math.abs(gap - last) < 0.5 ? still + 1 : 0;
      last = gap;
    }
    return last;
  });
}

/**
 * Хвост на миг короче, раскладку снимают посреди правки, потом хвост возвращается или растёт.
 * Так выглядит перестройка окна строк или смена вида карточки, когда раскладку читает чужой код.
 */
async function strayClamp(page: Page, grow: boolean): Promise<void> {
  await page.locator('.feed').evaluate((el, grow) => {
    const rows = el.querySelectorAll<HTMLElement>('[data-row]');
    const inner = rows[rows.length - 1]!.firstElementChild as HTMLElement;
    inner.style.display = 'none';
    void el.scrollHeight;
    void el.scrollTop;
    inner.style.display = '';
    if (grow) inner.append(Object.assign(document.createElement('p'), { textContent: 'Новая строка хвоста' }));
  }, grow);
}

async function strayClampKeepsBottom(page: Page, grow: boolean, throttle: boolean): Promise<void> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const s = seed(api);
  await installChatRoutes(page, hub, api, OWNER);
  if (throttle) {
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });
  }
  await page.goto(`/c/${s.id}`);
  const feed = page.locator('.feed');
  await expect(feed.locator('[data-row]').first()).toBeVisible({ timeout: 15_000 });
  expect(await settledGap(page)).toBeLessThanOrEqual(1);
  for (let round = 0; round < 3; round++) {
    await strayClamp(page, grow);
    // Ход идёт дальше: новый вызов приходит, пока событие прокрутки ещё в пути.
    streamCall(hub, api, s.id, s.run, 'Bash', { command: `npm test -- ${round}` }, { result: 'ok\n'.repeat(round + 1) });
    expect(await settledGap(page), `round ${round}`).toBeLessThanOrEqual(1);
    await expect(feed).toHaveAttribute('data-pinned', '');
  }
  // Нижний край последней строки виден над полем ввода.
  const edge = await page.evaluate(() => {
    const rows = document.querySelectorAll<HTMLElement>('.feed [data-row]');
    const composer = document.querySelector<HTMLElement>('[data-composer]')!;
    return composer.getBoundingClientRect().top - rows[rows.length - 1]!.getBoundingClientRect().bottom;
  });
  expect(edge).toBeGreaterThanOrEqual(0);
}

for (const grow of [false, true]) {
  test(`a_stray_clamp_while_pinned_keeps_the_very_bottom${grow ? '_and_new_tail' : ''}`, async ({ page }, info) => {
    test.setTimeout(90_000);
    await strayClampKeepsBottom(page, grow, info.project.name === 'phone');
  });
}

// Телефон владельца: Galaxy S25 Ultra, Chrome. Дробный масштаб 3.5 даёт дробный scrollTop.
test.describe('galaxy_s25_ultra', () => {
  // Firefox не умеет isMobile: контекст не создаётся, поэтому пропуск решается до него.
  test.skip(({ browserName }) => browserName !== 'chromium', 'Chrome на Android');
  test.use({ viewport: { width: 412, height: 915 }, deviceScaleFactor: 3.5, isMobile: true, hasTouch: true });
  for (const grow of [false, true]) {
    test(`a_stray_clamp_on_the_owners_phone_keeps_the_very_bottom${grow ? '_and_new_tail' : ''}`, async ({ page }) => {
      test.setTimeout(90_000);
      await strayClampKeepsBottom(page, grow, true);
    });
  }
});

test('the_owner_scrolling_up_after_a_stray_clamp_still_releases_the_bottom', async ({ page }, info) => {
  test.setTimeout(90_000);
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const s = seed(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${s.id}`);
  const feed = page.locator('.feed');
  await expect(feed.locator('[data-row]').first()).toBeVisible({ timeout: 15_000 });
  await settledGap(page);
  await strayClamp(page, true);
  expect(await settledGap(page)).toBeLessThanOrEqual(1);
  const box = (await feed.boundingBox())!;
  if (info.project.name === 'phone') {
    const cdp = await page.context().newCDPSession(page);
    const x = box.x + box.width / 2;
    let y = box.y + 150;
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] });
    for (let step = 0; step < 20; step++) {
      y += 20;
      await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y }] });
    }
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  } else {
    await feed.hover();
    await page.mouse.wheel(0, -1500);
  }
  await expect(feed).not.toHaveAttribute('data-pinned', '');
  // Новый хвост не возвращает к низу ленту, которую владелец увёл вверх.
  streamCall(hub, api, s.id, s.run, 'Bash', { command: 'npm test' }, { result: 'ok' });
  expect(await settledGap(page)).toBeGreaterThan(48 * 4);
});
