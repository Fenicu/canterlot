import type { Locator, Page } from '@playwright/test';
import type { HistoryInput } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { expect, resizeViewport, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-11-10T12:00:00.000Z';
// Сорок с лишним символов хватает, чтобы заголовок переносился на всех телефонных ширинах.
const LONG = 'Ревью фронтенда: вёрстка карточек ленты и очереди на телефоне 412 px';
const SHORT = 'Сборка';
const BODY = 'Проверь сборку и напиши отчёт: что упало и почему.';

type Box = { x: number; y: number; width: number; height: number };
type Ink = { top: number; bottom: number };

let seq = 0;

function serviceInput(
  api: FakeHubApi,
  text: string,
  origin: Record<string, unknown>,
  createdAt: string | null = AT
): HistoryInput {
  const n = ++seq;
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: `linked-title-${n}`,
    state: 'accepted',
    content: [{ type: 'text', text }],
    attachments: [],
    created_seq: String(n),
    feed_seq: String(n),
    created_at: createdAt,
    origin
  } as HistoryInput;
}

function newHub(): { hub: FakeHub; api: FakeHubApi } {
  const fake = new FakeHub();
  return { hub: fake, api: new FakeHubApi(fake) };
}

async function openChat(page: Page, hub: FakeHub, api: FakeHubApi, id: string): Promise<void> {
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
}

const box = (locator: Locator) => locator.boundingBox() as Promise<Box>;

/** Строки текста по Range: меряется краска текста, а не коробка элемента. */
function inkLines(locator: Locator): Promise<Ink[]> {
  return locator.evaluate((node) => {
    const range = document.createRange();
    range.selectNodeContents(node);
    return [...range.getClientRects()]
      .filter((rect) => rect.width > 1 && rect.height > 1)
      .map((rect) => ({ top: rect.top, bottom: rect.bottom }))
      .sort((a, b) => a.top - b.top);
  });
}

/** Высота цели касания: коробка ссылки вместе с псевдоэлементом, вынесенным из раскладки. */
function hitHeight(link: Locator): Promise<number> {
  return link.evaluate((node) => {
    const after = getComputedStyle(node, '::after');
    const px = (value: string) => (Number.isFinite(parseFloat(value)) ? parseFloat(value) : 0);
    const top = Math.min(0, px(after.top));
    const bottom = Math.min(0, px(after.bottom));
    return node.getBoundingClientRect().height - top - bottom;
  });
}

/** Зазор от нижней краски верхней строки до верхней краски следующей. */
function gapAbove(upper: Ink[], lower: Ink[]): number {
  return lower[0]!.top - upper.at(-1)!.bottom;
}

async function targetSize(page: Page): Promise<number> {
  return page.evaluate(() => parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--target')));
}

/** Телефонные ширины из задачи; на рабочем столе проверяется текущая. */
async function eachWidth(page: Page, run: (label: string, width: number | null) => Promise<void>) {
  if (test.info().project.name !== 'phone') {
    await run('desktop', null);
    return;
  }
  for (const width of [360, 390, 412]) {
    await resizeViewport(page, { width, height: 892 });
    await run(`${width}`, width);
  }
}

function firstLineBand(title: Box, lineHeight: number): [number, number] {
  return [title.y - 1, title.y + lineHeight + 1];
}

test('a_wrapped_sender_title_keeps_its_row_gaps_and_the_header_on_one_row', async ({ page }) => {
  const { hub, api } = newHub();
  const id = seedConversation(api, { title: 'Оркестратор' });
  const child = seedConversation(api, { title: 'Сборка' });
  const message = (title: string, linked: boolean) =>
    serviceInput(api, BODY, {
      kind: 'message',
      title,
      model: 'claude-sonnet-5',
      chain_depth: 1,
      ...(linked ? { from: child } : {})
    });
  api.histories.set(id, [message(LONG, true), message(LONG, false), message(SHORT, true)]);
  api.bindConversation(id);
  await openChat(page, hub, api, id);

  const linked = page
    .locator('article.message[data-role="session-message"]')
    .filter({ hasText: LONG })
    .filter({ has: page.locator('header a.title') })
    .first();
  const plain = page
    .locator('article.message[data-role="session-message"]')
    .filter({ hasText: LONG })
    .filter({ has: page.locator('header span.title') })
    .first();
  const short = page
    .locator('article.message[data-role="session-message"]')
    .filter({ hasText: `Сессия «${SHORT}»` })
    .filter({ has: page.locator('header a.title') })
    .first();
  await linked.scrollIntoViewIfNeeded();
  const target = await targetSize(page);

  await eachWidth(page, async (label) => {
    const card = await box(linked);
    const padding = await linked.evaluate((node) => parseFloat(getComputedStyle(node).paddingTop));
    const titleLines = await inkLines(linked.locator('header .title'));
    const plainLines = await inkLines(plain.locator('header .title'));
    const bodyLines = await inkLines(linked.locator('.body'));
    const plainBody = await inkLines(plain.locator('.body'));
    const title = await box(linked.locator('header .title'));
    const header = await box(linked.locator('header'));
    const time = await box(linked.locator('header time'));
    const bookmark = await box(linked.getByRole('button', { name: 'В избранное' }));
    const lineHeight = await linked
      .locator('header .title')
      .evaluate((node) => parseFloat(getComputedStyle(node).lineHeight));

    // Первая строка стоит на отступе карточки, а не в её скруглении.
    expect(titleLines[0]!.top - card.y, `${label}: первая строка от отступа`).toBeGreaterThanOrEqual(
      padding - 1
    );
    // Зазор до тела сообщения тот же, что у незалинкованного заголовка.
    const linkedGap = gapAbove(titleLines, bodyLines);
    const plainGap = gapAbove(plainLines, plainBody);
    expect(Math.abs(linkedGap - plainGap), `${label}: зазор под заголовком`).toBeLessThanOrEqual(1);

    // Время и закладка стоят в полосе первой строки заголовка, а не отдельной строкой.
    const [top, bottom] = firstLineBand(title, lineHeight);
    for (const [name, item] of [
      ['время', time],
      ['закладка', bookmark]
    ] as const) {
      const center = item.y + item.height / 2;
      expect(center, `${label}: ${name} в первой строке`).toBeGreaterThanOrEqual(top);
      expect(center, `${label}: ${name} в первой строке`).toBeLessThanOrEqual(bottom);
    }
    // Заголовок переносится в своей колонке: пара справа остаётся правее него.
    expect(time.x, `${label}: время правее заголовка`).toBeGreaterThan(title.x + 1);
    expect(bookmark.x, `${label}: закладка правее времени`).toBeGreaterThan(time.x);
    // Лишней строки ради времени и закладки нет: высота шапки — высота заголовка.
    expect(header.height, `${label}: шапка без отдельной строки`).toBeLessThanOrEqual(title.height + 1);

    // Цель касания ссылки не уменьшилась: у одной строки она по-прежнему не меньше --target.
    expect(await hitHeight(short.locator('header a.title')), `${label}: цель касания заголовка`).toBeGreaterThanOrEqual(
      target - 0.5
    );
  });
});

test('a_wrapped_linked_system_title_keeps_the_card_padding_and_the_gap_to_the_controls', async ({ page }) => {
  const { hub, api } = newHub();
  const id = seedConversation(api, { title: 'Оркестратор' });
  const child = seedConversation(api, { title: 'Сборка' });
  const quota = (title: string, linked: boolean, createdAt: string | null = AT) =>
    serviceInput(
      api,
      `[canterlot] Session "${title}" is quota_paused.`,
      {
        kind: 'event',
        title,
        chain_depth: 0,
        event: {
          kind: 'quota_paused',
          state: 'quota_paused',
          ...(linked ? { session_id: child } : {})
        }
      },
      createdAt
    );
  api.histories.set(id, [quota(LONG, true), quota(LONG, false), quota('Я', true)]);
  api.bindConversation(id);
  await openChat(page, hub, api, id);

  const linked = page
    .locator('article.system')
    .filter({ hasText: LONG })
    .filter({ has: page.locator('header a.title') })
    .first();
  const plain = page
    .locator('article.system')
    .filter({ hasText: LONG })
    .filter({ has: page.locator('header span.title') })
    .first();
  const short = page
    .locator('article.system')
    .filter({ hasText: 'Помощник «Я»' })
    .filter({ has: page.locator('header a.title') })
    .first();
  await linked.scrollIntoViewIfNeeded();
  const target = await targetSize(page);

  await eachWidth(page, async (label) => {
    const card = await box(linked);
    const padding = await linked.evaluate((node) => parseFloat(getComputedStyle(node).paddingTop));
    const titleLines = await inkLines(linked.locator('header .title'));
    const plainLines = await inkLines(plain.locator('header .title'));
    const controls = await inkLines(linked.locator('[data-role="system-controls"]'));
    const plainControls = await inkLines(plain.locator('[data-role="system-controls"]'));

    expect(titleLines[0]!.top - card.y, `${label}: первая строка от отступа карточки`).toBeGreaterThanOrEqual(
      padding - 1
    );
    // От заголовка до строки действий тот же зазор, что у незалинкованного заголовка.
    expect(
      Math.abs(gapAbove(titleLines, controls) - gapAbove(plainLines, plainControls)),
      `${label}: зазор под заголовком`
    ).toBeLessThanOrEqual(1);
    expect(await hitHeight(short.locator('header a.title')), `${label}: цель касания заголовка`).toBeGreaterThanOrEqual(
      target - 0.5
    );
  });
});

test('a_wrapped_alarm_session_row_keeps_the_gaps_above_and_below', async ({ page }) => {
  const { hub, api } = newHub();
  const id = seedConversation(api, { title: 'Оркестратор' });
  const child = seedConversation(api, { title: 'Сборка' });
  // Две одинаковые карточки отличаются только порядком строк: в первой первая строка
  // в одну строку, во второй — переносится. Нижняя строка та же, поэтому зазоры сравнимы.
  const alarm = (key: string, rows: string[]) =>
    serviceInput(api, `[canterlot] Alarm "${key}" timed out.`, {
      kind: 'alarm',
      alarm_id: key,
      chain_depth: 0,
      event: {
        kind: 'alarm',
        alarm_id: key,
        alarm_key: key,
        fired_reason: 'timed_out',
        armed_at: AT,
        condition: { kind: 'session_state', session_ids: [child], states: ['stalled'] },
        sessions: rows.map((title) => ({ id: child, title }))
      }
    });
  api.histories.set(id, [alarm('a', [SHORT, LONG]), alarm('b', [LONG, SHORT])]);
  api.bindConversation(id);
  await openChat(page, hub, api, id);

  const oneLine = page.locator('article.system').filter({ hasText: 'Будильник «a»' }).first();
  const wrapped = page.locator('article.system').filter({ hasText: 'Будильник «b»' }).first();
  await oneLine.scrollIntoViewIfNeeded();
  const target = await targetSize(page);

  await eachWidth(page, async (label, width) => {
    const reason = oneLine.locator('.reason');
    const oneRows = oneLine.locator('.rows li');
    const wrappedRows = wrapped.locator('.rows li');
    const [oneReason, oneFirst, oneSecond, wrappedReason, wrappedFirst, wrappedSecond] = await Promise.all([
      inkLines(reason),
      inkLines(oneRows.nth(0).locator('a')),
      inkLines(oneRows.nth(1).locator('a')),
      inkLines(wrapped.locator('.reason')),
      inkLines(wrappedRows.nth(0).locator('a')),
      inkLines(wrappedRows.nth(1).locator('a'))
    ]);
    if (width !== null) expect(wrappedFirst.length, `${label}: длинный заголовок переносится`).toBeGreaterThan(1);

    // Зазор от причины до первой строки и от строки к строке не зависит от переноса.
    expect(
      Math.abs(gapAbove(wrappedReason, wrappedFirst) - gapAbove(oneReason, oneFirst)),
      `${label}: зазор над строкой`
    ).toBeLessThanOrEqual(1);
    expect(
      Math.abs(gapAbove(wrappedFirst, wrappedSecond) - gapAbove(oneFirst, oneSecond)),
      `${label}: зазор под строкой`
    ).toBeLessThanOrEqual(1);

    expect(await hitHeight(oneRows.nth(0).locator('a')), `${label}: цель касания строки`).toBeGreaterThanOrEqual(
      target - 0.5
    );
  });
});

test('a_wrapped_result_card_title_keeps_the_status_first_line_and_the_gap_to_the_facts', async ({ page }) => {
  const { hub, api } = newHub();
  const id = seedConversation(api, { title: 'Оркестратор' });
  const child = seedConversation(api, { title: 'Сборка' });
  const result = (title: string, linked: boolean) =>
    serviceInput(api, `[canterlot] Session "${title}" is now idle.`, {
      kind: 'event',
      title,
      chain_depth: 0,
      event: {
        kind: 'session_result',
        state: 'idle',
        turns: 3,
        commits: [{ sha: '1a2b3c4', title: 'Перенести лимиты' }],
        ...(linked ? { session_id: child } : {})
      }
    });
  api.histories.set(id, [result(LONG, true), result(LONG, false), result(SHORT, true)]);
  api.bindConversation(id);
  await openChat(page, hub, api, id);

  const cards = page.locator('article.card[data-event-kind="session_result"]');
  const linked = cards.filter({ hasText: LONG }).filter({ has: page.locator('header a.title') }).first();
  const plain = cards.filter({ hasText: LONG }).filter({ has: page.locator('header span.title') }).first();
  const short = cards.filter({ hasText: SHORT }).filter({ has: page.locator('header a.title') }).first();
  await linked.scrollIntoViewIfNeeded();
  const target = await targetSize(page);

  await eachWidth(page, async (label) => {
    const card = await box(linked);
    const padding = await linked.evaluate((node) => parseFloat(getComputedStyle(node).paddingTop));
    const titleLines = await inkLines(linked.locator('header .title'));
    const plainLines = await inkLines(plain.locator('header .title'));
    const facts = await inkLines(linked.locator('.facts dt').first());
    const plainFacts = await inkLines(plain.locator('.facts dt').first());
    const title = await box(linked.locator('header .title'));
    const header = await box(linked.locator('header'));
    const status = await box(linked.locator('header .status'));
    const lineHeight = await linked
      .locator('header .title')
      .evaluate((node) => parseFloat(getComputedStyle(node).lineHeight));

    expect(titleLines[0]!.top - card.y, `${label}: первая строка от отступа карточки`).toBeGreaterThanOrEqual(
      padding - 1
    );
    const linkedGap = gapAbove(titleLines, facts);
    const plainGap = gapAbove(plainLines, plainFacts);
    expect(Math.abs(linkedGap - plainGap), `${label}: зазор до фактов`).toBeLessThanOrEqual(1);

    // Состояние стоит в полосе первой строки заголовка, а не отдельной строкой.
    const [top, bottom] = firstLineBand(title, lineHeight);
    const center = status.y + status.height / 2;
    expect(center, `${label}: состояние в первой строке`).toBeGreaterThanOrEqual(top);
    expect(center, `${label}: состояние в первой строке`).toBeLessThanOrEqual(bottom);
    expect(status.x, `${label}: состояние правее заголовка`).toBeGreaterThan(title.x + 1);
    expect(header.height, `${label}: шапка без отдельной строки`).toBeLessThanOrEqual(title.height + 1);

    expect(await hitHeight(short.locator('header a.title')), `${label}: цель касания заголовка`).toBeGreaterThanOrEqual(
      target - 0.5
    );
  });
});
