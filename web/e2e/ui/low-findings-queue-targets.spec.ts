import type { Locator, Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryBlock, HistoryInput, ScheduledMessage } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

/**
 * Мелкие находки ревью от 2026-11-16: F-20, F-21 и F-40.
 * Очередь меряется только там, где включается её телефонная раскладка (`pointer: coarse`).
 */
const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';
const WIDTHS = [360, 412];
const SHOTS = process.env.W3_SHOTS ? '/tmp/w3-queue' : null;

// Высоты до правки F-40, снятые на 3cbf923f при 360 и 412: пузырь без вложений, пузырь
// с файлом и карточка с живой строкой. Правка не должна растить их сверх нужного.
const PRE_F40_BUBBLE = 66.5;
const PRE_F40_BUBBLE_FILES = 128.5;
const PRE_F40_CARD = 172.5;
const F40_BUBBLE_GROWTH = 8;
const F40_CARD_GROWTH = 12;
const F40_BOTTOM_OVERHANG = 3.5;

const phoneOnly = (info: { project: { name: string } }) => test.skip(info.project.name !== 'phone', 'раскладка очереди с «⋯» живёт под pointer: coarse');

interface Box {
  left: number;
  top: number;
  right: number;
  bottom: number;
  width: number;
  height: number;
}

async function boot(page: Page, width: number, seed?: (api: FakeHubApi, id: string) => void): Promise<{ hub: FakeHub; api: FakeHubApi; id: string }> {
  await page.setViewportSize({ width, height: 892 });
  await page.addInitScript(() => localStorage.setItem('canterlot.theme', 'dark'));
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  api.bindConversation(id);
  seed?.(api, id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

function seedScheduled(api: FakeHubApi, id: string, rows: { text: string }[]): void {
  const snap = api.conversationData(id);
  snap.scheduled = rows.map((row, index) => ({
    id: api.id(),
    conversation_id: id,
    run_at: new Date(Date.now() + 60 * 60_000 * (index + 1)).toISOString(),
    content: [{ type: 'text', text: row.text }],
    attachment_ids: [],
    state: 'pending',
    input_id: null,
    attempts: 0
  })) as ScheduledMessage[];
  api.hub.setSnapshot(`conversation:${id}`, () => snap);
}

async function queue(hub: FakeHub, api: FakeHubApi, id: string, texts: string[]): Promise<void> {
  texts.forEach((text, index) =>
    hub.append(`conversation:${id}`, {
      kind: 'input_queued',
      conversation_id: id,
      payload: { input_id: api.id(), input_seq: String(index + 20), idempotency_key: api.id(), state: 'queued', content: [{ type: 'text', text }] }
    })
  );
  hub.flush();
}

async function shot(target: Locator, name: string): Promise<void> {
  if (!SHOTS) return;
  mkdirSync(SHOTS, { recursive: true });
  await target.screenshot({ path: join(SHOTS, `${name}.png`) });
}

function block(api: FakeHubApi, seq: number, patch: Partial<HistoryBlock>): HistoryBlock {
  const id = api.id();
  const content = patch.content ?? '';
  api.blocks.set(id, { text: content, rev: 1, state: 'final' });
  return {
    kind: 'block',
    id,
    block_kind: 'text',
    content,
    content_bytes: content.length,
    content_truncated: false,
    created_seq: String(seq),
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'агент',
    model: 'opus',
    tool_name: null,
    summary: null,
    parent_block_id: null,
    input_command_id: null,
    line_count: 1,
    ...patch
  };
}

function ownerInput(api: FakeHubApi, seq: number, text: string, attachments: HistoryInput['attachments'] = []): HistoryInput {
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: `w3-${seq}`,
    state: 'accepted',
    content: [{ type: 'text', text }],
    attachments,
    created_seq: String(seq),
    feed_seq: String(seq),
    created_at: AT
  } as HistoryInput;
}

async function boxes(pill: Locator): Promise<{
  pill: Box;
  body: Box;
  line: number;
  target: number;
  button: Box;
  icon: Box;
}> {
  return pill.evaluate((node) => {
    const box = (el: Element): Box => {
      const rect = el.getBoundingClientRect();
      return { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom, width: rect.width, height: rect.height };
    };
    const body = node.querySelector('.body') as HTMLElement;
    const bodyStyle = getComputedStyle(body);
    const line = Number.parseFloat(bodyStyle.lineHeight) || Number.parseFloat(bodyStyle.fontSize) * 1.4;
    const button = node.querySelector('.row-actions .overflow') as HTMLElement;
    const icon = button.querySelector('svg') as SVGSVGElement;
    return {
      pill: box(node),
      body: box(body),
      line,
      target: Number.parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--target')),
      button: box(button),
      icon: box(icon)
    };
  });
}

/** «⋯» стоит по центру первой строки, а цель касания не выходит за карточку. */
async function expectOverflowOnFirstLine(pill: Locator, label: string): Promise<void> {
  const found = await boxes(pill);
  const firstLineCentre = found.body.top + found.line / 2;
  const iconCentre = found.icon.top + found.icon.height / 2;
  expect(Math.abs(iconCentre - firstLineCentre), `${label}: «⋯» по центру первой строки`).toBeLessThanOrEqual(1);
  expect(found.button.height, `${label}: цель «⋯» не ниже --target`).toBeGreaterThanOrEqual(found.target - 0.5);
  expect(found.button.width, `${label}: цель «⋯» не уже --target`).toBeGreaterThanOrEqual(found.target - 0.5);
  expect(found.button.top, `${label}: цель не выходит над карточкой`).toBeGreaterThanOrEqual(found.pill.top - 0.5);
  expect(found.button.bottom, `${label}: цель не выходит под карточкой`).toBeLessThanOrEqual(found.pill.bottom + 0.5);
  expect(found.button.left, `${label}: цель не выходит слева`).toBeGreaterThanOrEqual(found.pill.left - 0.5);
  expect(found.button.right, `${label}: цель не выходит справа`).toBeLessThanOrEqual(found.pill.right + 0.5);
  // Точка внутри цели принадлежит кнопке, а не соседнему элементу.
  const hit = await pill.evaluate((node) => {
    const button = node.querySelector('.row-actions .overflow') as HTMLElement;
    const rect = button.getBoundingClientRect();
    const points = [
      [rect.left + rect.width / 2, rect.top + rect.height / 2],
      [rect.left + rect.width / 2, rect.top + 2],
      [rect.left + rect.width / 2, rect.bottom - 2]
    ];
    return points.map(([x, y]) => {
      const found = document.elementFromPoint(x!, y!);
      return { x, y, own: found !== null && (found === button || button.contains(found)) };
    });
  });
  expect(hit.filter((point) => !point.own), `${label}: точки цели принадлежат «⋯»`).toEqual([]);
}

const LONG_TOKEN = `sha256:${'9f8e'.repeat(48)}`;
const TWO_LINES = 'После тестов обнови README и добавь строку про порядок подписки и снимка журнала';

for (const width of WIDTHS) {
  test(`queue_overflow_sits_on_the_first_text_line ${width}`, async ({ page }, info) => {
    phoneOnly(info);
    await boot(page, width, (a, conversation) => {
      seedScheduled(a, conversation, [{ text: 'позже' }, { text: TWO_LINES }]);
    });
    await expect(page.locator('.strip .pill')).toHaveCount(2);
    await expect(page.locator('.strip .pill').nth(0)).toBeVisible();
    await expectOverflowOnFirstLine(page.locator('.strip .pill').nth(0), `отложенное в одну строку @${width}`);
    await expectOverflowOnFirstLine(page.locator('.strip .pill').nth(1), `отложенное в две строки @${width}`);
    await shot(page.locator('.strip').first(), `f20-scheduled-${width}`);
  });

  test(`queued_card_overflow_sits_on_the_first_text_line ${width}`, async ({ page }, info) => {
    phoneOnly(info);
    const { hub, api, id } = await boot(page, width);
    await queue(hub, api, id, ['Третье', TWO_LINES]);
    const queued = page.locator('.strip [data-state] .pill');
    await expect(queued).toHaveCount(2);
    await expectOverflowOnFirstLine(queued.nth(0), `очередь в одну строку @${width}`);
    await expectOverflowOnFirstLine(queued.nth(1), `очередь в две строки @${width}`);
    await shot(page.locator('.strip').first(), `f20-queue-${width}`);
  });

  test(`queue_wraps_an_unbroken_token_under_the_clamp ${width}`, async ({ page }) => {
    const { hub, api, id } = await boot(page, width);
    await queue(hub, api, id, [LONG_TOKEN]);
    const pill = page.locator('.strip .pill').first();
    await expect(pill).toBeVisible();
    const found = await pill.evaluate((node) => {
      const body = node.querySelector('.body') as HTMLElement;
      const text = body.querySelector('.text') as HTMLElement;
      // Обрезка стоит на теле карточки, а на столе — на самом тексте.
      const clamp = [body, text].find((el) => getComputedStyle(el).webkitLineClamp === '2') ?? body;
      const style = getComputedStyle(clamp);
      const range = document.createRange();
      range.selectNodeContents(clamp);
      const rect = clamp.getBoundingClientRect();
      const rects = [...range.getClientRects()].filter((item) => item.width > 0.5);
      return {
        clampHeight: rect.height,
        clampTop: rect.top,
        clampRight: rect.right,
        clampBottom: rect.bottom,
        overflowWrap: style.overflowWrap,
        lineClamp: style.webkitLineClamp,
        overflow: style.overflow,
        lines: rects.length,
        right: Math.max(...rects.map((item) => item.right)),
        bottom: Math.max(...rects.map((item) => item.bottom))
      };
    });
    // Токен ломается по буквам и остаётся внутри своей колонки: половины глифа у правого края нет.
    expect(found.overflowWrap, 'перенос длинного токена разрешён').toBe('anywhere');
    expect(found.lines, 'токен занял несколько строк').toBeGreaterThanOrEqual(2);
    expect(found.right, 'строки не вылезают за правый край').toBeLessThanOrEqual(found.clampRight + 0.5);
    // Строк больше, чем помещается в предел: видимую часть заканчивает многоточие обрезки.
    expect(found.lineClamp, 'очередь держит предел в две строки').toBe('2');
    expect(found.overflow, 'лишние строки скрыты обрезкой').toBe('hidden');
    expect(found.bottom, 'видимых строк больше, чем влезает в предел').toBeGreaterThan(found.clampBottom + 0.5);
    await shot(page.locator('.strip').first(), `f21-token-${width}`);
  });

  test(`owner_bubble_without_attachments_keeps_its_height ${width}`, async ({ page }) => {
    await boot(page, width, (api, id) => {
      api.histories.set(id, [ownerInput(api, 1, 'Лог приложен')]);
    });
    const bubble = page.locator('article.bubble').first();
    await expect(bubble).toBeVisible();
    const found = await measureHitArea(bubble.locator('[data-role="bubble-actions"] button'), 'article.bubble');
    // Пузырь без вложений не растёт: строка статуса с закладкой держит прежнюю высоту.
    expect(Math.abs(found.card.height - PRE_F40_BUBBLE), 'высота пузыря без вложений как до F-40').toBeLessThanOrEqual(0.5);
    expect(found.button.height, 'закладка держит --target').toBeGreaterThanOrEqual(found.target - 0.5);
    expect(found.button.width, 'закладка держит --target по ширине').toBeGreaterThanOrEqual(found.target - 0.5);
    expect(found.button.top, 'закладка не выходит над пузырём').toBeGreaterThanOrEqual(found.card.top - 0.5);
    expect(found.button.left, 'закладка не выходит слева').toBeGreaterThanOrEqual(found.card.left - 0.5);
    expect(found.button.right, 'закладка не выходит справа').toBeLessThanOrEqual(found.card.right + 0.5);
    expect(found.button.bottom - found.card.bottom, 'низ цели заходит в зазор между пузырями не больше 3 px').toBeLessThanOrEqual(F40_BOTTOM_OVERHANG);
    expect(found.points.filter((point) => !point.own), 'точки цели принадлежат закладке').toEqual([]);
    await shot(bubble, `f40-bubble-plain-${width}`);
  });

  test(`owner_bubble_with_attachments_keeps_its_height_and_clears_the_tile ${width}`, async ({ page }) => {
    await boot(page, width, (api, id) => {
      api.histories.set(id, [
        ownerInput(api, 1, 'Лог приложен', [{ attachment_id: api.id(), name: 'hub.log', size: 2048, mime: 'text/plain', sha256: 'x', image: false }])
      ]);
    });
    const bubble = page.locator('article.bubble').first();
    await expect(bubble).toBeVisible();
    const found = await measureHitArea(bubble.locator('[data-role="bubble-actions"] button'), 'article.bubble');
    // Прибавка равна пропущенному зазору до плитки вложения, а не высоте цели.
    expect(found.card.height, 'пузырь вырос не больше чем на пропущенный зазор').toBeLessThanOrEqual(PRE_F40_BUBBLE_FILES + F40_BUBBLE_GROWTH + 0.5);
    expect(found.card.height, 'пузырь не стал ниже').toBeGreaterThanOrEqual(PRE_F40_BUBBLE_FILES - 0.5);
    expect(found.button.height, 'закладка держит --target').toBeGreaterThanOrEqual(found.target - 0.5);
    expect(found.button.width, 'закладка держит --target по ширине').toBeGreaterThanOrEqual(found.target - 0.5);
    expect(found.button.top, 'закладка не выходит над пузырём').toBeGreaterThanOrEqual(found.card.top - 0.5);
    expect(found.button.left, 'закладка не выходит слева').toBeGreaterThanOrEqual(found.card.left - 0.5);
    expect(found.button.right, 'закладка не выходит справа').toBeLessThanOrEqual(found.card.right + 0.5);
    expect(found.button.bottom - found.card.bottom, 'низ цели заходит в зазор между пузырями не больше 3 px').toBeLessThanOrEqual(F40_BOTTOM_OVERHANG);
    expect(found.overlaps, 'закладка не пересекает вложения').toEqual([]);
    expect(found.points.filter((point) => !point.own), 'точки цели принадлежат закладке').toEqual([]);
    await shot(bubble, `f40-bubble-files-${width}`);
  });

  test(`live_line_interrupt_keeps_its_target_clear ${width}`, async ({ page }) => {
    await boot(page, width, (api, id) => {
      const run = api.id();
      const conversation = api.details.get(id) as Record<string, unknown>;
      conversation.activity = {
        state: 'working',
        evidence: { phase: 'running_tool', tool_name: 'Bash', phase_since: new Date(Date.now() - 754_000).toISOString() },
        since: new Date(Date.now() - 900_000).toISOString()
      };
      api.histories.set(id, [
        ownerInput(api, 1, 'Прогони тесты'),
        block(api, 2, { content: 'Запускаю.', input_command_id: run }),
        block(api, 3, { block_kind: 'tool_call', tool_name: 'Bash', state: 'streaming', summary: 'cargo test --workspace', content: 'running…', input_command_id: run })
      ]);
    });
    const stop = page.locator('[data-working] [data-interrupt]');
    await expect(stop).toBeVisible();
    const found = await measureHitArea(stop, 'article.card, [data-row]');
    // Прибавка карточки равна недостающим зазорам сверху и снизу, а не высоте цели.
    expect(found.card.height, 'карточка выросла не больше чем на снятые пересечения').toBeLessThanOrEqual(PRE_F40_CARD + F40_CARD_GROWTH + 0.5);
    expect(found.card.height, 'карточка не стала ниже').toBeGreaterThanOrEqual(PRE_F40_CARD - 0.5);
    expect(found.button.height, '«Прервать» держит --target').toBeGreaterThanOrEqual(found.target - 0.5);
    expect(found.button.width, '«Прервать» держит --target по ширине').toBeGreaterThanOrEqual(found.target - 0.5);
    expect(found.button.top, '«Прервать» не выходит над карточкой').toBeGreaterThanOrEqual(found.card.top - 0.5);
    expect(found.button.bottom, '«Прервать» не выходит под карточкой').toBeLessThanOrEqual(found.card.bottom + 0.5);
    expect(found.overlaps, '«Прервать» не пересекает строку инструмента и подвал').toEqual([]);
    expect(found.points.filter((point) => !point.own), 'точки цели принадлежат «Прервать»').toEqual([]);
    await shot(page.locator('article.card').filter({ has: stop }), `f40-live-${width}`);
  });
}

/** Габариты цели, её карточки и пересечения с другими контролами внутри карточки. */
async function measureHitArea(control: Locator, cardSelector: string): Promise<{
  card: Box;
  button: Box;
  target: number;
  overlaps: { label: string; w: number; h: number }[];
  points: { x: number; y: number; own: boolean }[];
}> {
  return control.evaluate((button, selector) => {
    const box = (el: Element): Box => {
      const rect = el.getBoundingClientRect();
      return { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom, width: rect.width, height: rect.height };
    };
    const card = button.closest(selector) as HTMLElement;
    const buttonBox = box(button);
    const others = [...card.querySelectorAll<HTMLElement>('a[href], button, input, textarea, select, [role="button"], [role="menuitem"]')]
      .filter((el) => el !== button && !button.contains(el) && !el.contains(button) && el.getClientRects().length > 0)
      .map((el) => ({ label: el.getAttribute('aria-label') ?? (el.textContent ?? '').trim().slice(0, 30), box: box(el) }));
    const overlaps = others
      .map((other) => ({
        label: other.label,
        w: Math.min(buttonBox.right, other.box.right) - Math.max(buttonBox.left, other.box.left),
        h: Math.min(buttonBox.bottom, other.box.bottom) - Math.max(buttonBox.top, other.box.top)
      }))
      .filter((item) => item.w > 0.5 && item.h > 0.5);
    const points = [
      [buttonBox.left + buttonBox.width / 2, buttonBox.top + buttonBox.height / 2],
      [buttonBox.left + buttonBox.width / 2, buttonBox.top + 2],
      [buttonBox.left + buttonBox.width / 2, buttonBox.bottom - 2]
    ];
    return {
      card: box(card),
      button: buttonBox,
      target: Number.parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--target')),
      overlaps,
      points: points.map(([x, y]) => {
        const hit = document.elementFromPoint(x!, y!);
        return { x: x!, y: y!, own: hit !== null && (hit === button || button.contains(hit)) };
      })
    };
  }, cardSelector);
}
