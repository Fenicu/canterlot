import type { Page } from '@playwright/test';
import type { HistoryBlock, HistoryInput, HistoryItem } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedFeedCards, setActivity, streamCall, type FeedCardsSeed } from '../../testing/fixtures/feed-cards';
import { expect, test } from './fixtures';

// Повторное открытие живого оркестратора вставало посреди старого длинного ответа с первого кадра.
// Тёплая лента монтирует окно разом, и пока строки домериваются, окно перестраивается: высота
// ленты на миг проседает, браузер прижимает scrollTop, а к событию прокрутки высота уже снова
// выросла. Контроллер принимал такое прижатие за прокрутку владельца вверх и отпускал низ.
const OWNER = '00000000-0000-4000-8000-0000000000c1';
const AT = '2026-11-07T18:00:00.000Z';

function longAnswer(n: number): string {
  const parts = [`**На чём остановилась ${n}.** ` + 'Проверка, что сервер отдаёт файлы заранее сжатыми. '.repeat(8)];
  for (let i = 0; i < 8; i++) parts.push(`- **Пункт ${i}.** ` + 'Длинное пояснение с подробностями выкатки. '.repeat(6));
  parts.push('```ts', ...Array.from({ length: 12 }, (_, i) => `const v${i} = f(${i});`), '```');
  parts.push('Вопрос. ' + 'Скрипт после остановки оставил воркер выключенным. '.repeat(10));
  return parts.join('\n\n');
}

/** Долгая сессия оркестратора: сотни записей с длинными ответами, под ними карточки инструментов и идущий ход. */
function seed(api: FakeHubApi): FeedCardsSeed {
  const s = seedFeedCards(api);
  const old: HistoryItem[] = [];
  for (let turn = 0; turn < 120; turn++) {
    old.push({ kind: 'input', id: api.id(), idempotency_key: `old-${turn}`, state: 'accepted', content: [{ type: 'text', text: `Вопрос ${turn} ` + 'текст '.repeat((turn % 7) * 5) }], attachments: [], created_seq: '0', feed_seq: '0', created_at: AT } as HistoryInput);
    const id = api.id();
    const text = turn % 3 === 0 ? longAnswer(turn) : 'Короткий ответ. '.repeat(10 + (turn % 5) * 6);
    api.blocks.set(id, { text, rev: 1, state: 'final' });
    old.push({ kind: 'block', id, block_kind: 'text', content: text, content_bytes: text.length, content_truncated: false, created_seq: '0', created_at: AT, rev: 1, state: 'final', agent: 'orchestrator', model: 'opus', tool_name: null, summary: null, parent_block_id: null, input_command_id: null, line_count: 1 } as HistoryBlock);
  }
  const all = [...old, ...api.histories.get(s.id)!];
  // Номера засеянной истории заняты: живые события хаба встают после неё.
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

/** Каждый кадр после открытия, пока строки домериваются: прижата ли лента и сколько до низа. */
async function framesAfterOpen(page: Page, ms: number): Promise<{ pinned: boolean; gap: number }[]> {
  return page.evaluate(async (ms) => {
    const out: { pinned: boolean; gap: number }[] = [];
    const start = performance.now();
    while (performance.now() - start < ms) {
      await new Promise((done) => requestAnimationFrame(done));
      const el = document.querySelector<HTMLElement>('.feed');
      if (!el || el.querySelector('[data-row]') === null) continue;
      out.push({ pinned: el.hasAttribute('data-pinned'), gap: Math.round(el.scrollHeight - el.clientHeight - el.scrollTop) });
    }
    return out;
  }, ms);
}

/** Зазор до низа после того, как строки домерились: несколько кадров подряд без изменений. */
async function settledGap(page: Page): Promise<number> {
  return page.locator('.feed').evaluate(async (el) => {
    const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));
    let last = -1;
    let still = 0;
    for (let i = 0; i < 600 && still < 20; i++) {
      await frame();
      const gap = Math.round(el.scrollHeight - el.scrollTop - el.clientHeight);
      still = gap === last ? still + 1 : 0;
      last = gap;
    }
    return last;
  });
}

test('reopening_a_live_orchestrator_stays_at_the_tail_from_the_first_frame', async ({ page }, info) => {
  test.setTimeout(120_000);
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const s = seed(api);
  await installChatRoutes(page, hub, api, OWNER);
  if (info.project.name === 'phone') {
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });
  }
  let n = 0;
  const bash = () => {
    n += 1;
    streamCall(hub, api, s.id, s.run, 'Bash', { command: `npm test -- part ${n}`, description: 'тесты' }, { result: 'ok\n'.repeat((n % 5) + 1) });
  };
  await page.goto('/');
  const link = page.getByRole('link', { name: /Кантерлот/ }).first();
  for (let visit = 0; visit < 3; visit++) {
    if (visit > 0) {
      // Пока владелец в списке, ход идёт дальше.
      bash();
      bash();
      setActivity(hub, api, s.id, 'working', { phase: 'generating' });
    }
    await link.click();
    const frames = await framesAfterOpen(page, 2500);
    expect(frames.length, `visit ${visit}`).toBeGreaterThan(0);
    expect(frames.filter((frame) => !frame.pinned), `visit ${visit}: лента отпустила низ`).toEqual([]);
    expect(await settledGap(page), `visit ${visit}`).toBeLessThanOrEqual(48);
    await expect(page.locator('.feed')).toHaveAttribute('data-pinned', '');
    await page.goBack();
    await link.waitFor();
  }
});

test('scrolling_up_while_the_reopened_feed_settles_releases_the_bottom', async ({ page }, info) => {
  test.setTimeout(120_000);
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const s = seed(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/');
  const link = page.getByRole('link', { name: /Кантерлот/ }).first();
  await link.click();
  await expect(page.locator('.feed [data-row]').first()).toBeVisible({ timeout: 15_000 });
  await page.goBack();
  await link.waitFor();
  streamCall(hub, api, s.id, s.run, 'Bash', { command: 'npm test', description: 'тесты' }, { result: 'ok' });
  await link.click();
  const feed = page.locator('.feed');
  await expect(feed.locator('[data-row]').first()).toBeVisible();
  // Владелец тянет ленту вверх сразу, пока строки ещё домериваются.
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
  const gap = await feed.evaluate(async (el) => {
    for (let i = 0; i < 30; i++) await new Promise((done) => requestAnimationFrame(done));
    return el.scrollHeight - el.clientHeight - el.scrollTop;
  });
  // Окно стоит там, куда его увёл владелец, и не возвращается к низу само.
  expect(gap).toBeGreaterThan(48 * 4);
});
