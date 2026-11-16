import type { Locator, Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { FakeHub, FakeHubApi, seedConversation, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryBlock, HistoryInput, ScheduledMessage } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const SHOTS = process.env.CHAT_VISUAL_SHOTS;

const HARNESS = { id: 'opencode', title: 'OpenCode', provider: 'opencode' };
const MACHINE = { id: '00000000-0000-4000-8000-000000000101', name: 'workstation' };

const CODE = "const palette = 'aquamarine' as const;\nsetPalette(palette);";
const LONG_CODE = `${CODE}\nconst identifier = '${'very-long-token-'.repeat(24)}end';`;

const NOW_MS = Math.floor(Date.now() / 1000) * 1000;
const stamp = (minutes: number) => new Date(NOW_MS + minutes * 60_000).toISOString();

function seedLimits(api: FakeHubApi): void {
  api.limits = [
    {
      provider: 'claude',
      weekly_reserve_active: false,
      five_hour: { used_permille: 610, resets_at: stamp(110), observed_at: stamp(-1) },
      weekly: { used_permille: 840, resets_at: stamp(2 * 24 * 60 + 5), observed_at: stamp(-1) }
    }
  ];
}

function block(api: FakeHubApi, content: string, kind: 'text' | 'tool_call' | 'tool_result', patch: Partial<HistoryBlock> = {}): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text: content, rev: 1, state: 'final' });
  return {
    kind: 'block',
    id,
    block_kind: kind,
    content,
    content_bytes: new TextEncoder().encode(content).length,
    content_truncated: false,
    created_seq: '2',
    created_at: stamp(-6),
    rev: 1,
    state: 'final',
    agent: 'OpenCode',
    model: 'deepseek',
    tool_name: kind === 'tool_call' ? 'Bash' : null,
    summary: kind === 'tool_call' ? 'cargo test -p canterlot-hub --test stream' : null,
    parent_block_id: null,
    input_command_id: null,
    line_count: content.split('\n').length,
    ...patch
  };
}

function input(api: FakeHubApi, text: string, state: 'accepted' | 'queued', seq: string): HistoryInput {
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: `visual-${seq}`,
    state,
    content: [{ type: 'text', text }],
    attachments: [],
    created_seq: seq,
    feed_seq: seq,
    created_at: stamp(-8)
  };
}

interface Boot {
  hub: FakeHub;
  api: FakeHubApi;
  id: string;
  user: HistoryInput;
}

async function boot(page: Page, opts: { longCode?: boolean; palette?: string; theme?: string; start?: 'waiting' | 'working' } = {}): Promise<Boot> {
  await page.addInitScript(({ palette, theme }) => {
    localStorage.setItem('canterlot.palette', palette);
    localStorage.setItem('canterlot.theme', theme);
    localStorage.setItem('canterlot.motion', 'off');
  }, { palette: opts.palette ?? 'aquamarine', theme: opts.theme ?? 'dark' });
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, {
    id: MACHINE.id,
    name: MACHINE.name,
    harnesses: [
      {
        id: 'opencode',
        title: 'OpenCode',
        provider: 'opencode',
        models: [{ id: 'deepseek', title: 'deepseek', efforts: ['low', 'medium', 'high'] }],
        permission_modes: ['owner', 'ask', 'bypass', 'read_only']
      }
    ]
  });
  seedLimits(api);
  const id = seedConversation(api, {
    title: 'Темы оформления',
    color: 'blue',
    model: 'deepseek',
    harness: HARNESS,
    machine: MACHINE,
    activity: opts.start === 'waiting'
      ? { state: 'waiting_for_user', evidence: { question: 'Какую палитру взять за основу?' }, since: stamp(-1) }
      : { state: 'working', evidence: { phase: 'generating' }, since: stamp(-1) } as never,
    last_activity_at: stamp(-1)
  });
  const user = input(api, 'Обнови палитру и проверь контраст в светлой теме.', 'accepted', '1');
  const answer = block(api, `Проверил три палитры: контраст проходит.\n\n\`\`\`ts\n${opts.longCode ? LONG_CODE : CODE}\n\`\`\`\n\nПрогоняю тесты.`, 'text');
  const call = block(api, '', 'tool_call', { created_seq: '3' });
  const result = block(api, 'running 3 tests\ncursor_survives_snapshot ... ok\nno_duplicates_after_reconnect ... ok\nreplay_keeps_order ... ok\ntest result: ok. 3 passed', 'tool_result', { created_seq: '4', parent_block_id: call.id });
  const queued = input(api, 'Добавь вариант «Норд» и проверь контраст в светлой теме.', 'queued', '5');
  api.histories.set(id, [user, answer, call, result, queued]);
  api.list.get(id)!.queue = { queued: 1, pause_reasons: [], hold_reasons: [] };
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await settleLiveTurn(page, hub, api, id, user, opts.start ?? 'working');
  return { hub, api, id, user };
}

/** Ход идёт: у шапки кольцо генерации, у очереди есть «Влить», копирование доступно и на телефоне. */
async function settleLiveTurn(page: Page, hub: FakeHub, api: FakeHubApi, id: string, user: HistoryInput, start: 'waiting' | 'working'): Promise<void> {
  if (start === 'working') appendActivity(hub, api, id, { state: 'working', evidence: { phase: 'generating' }, since: stamp(-1) });
  hub.append(`conversation:${id}`, {
    kind: 'queue_state_changed',
    conversation_id: id,
    payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: user.id } } }
  });
  hub.flush();
  await expect(page.locator('.title-row .mark')).toHaveAttribute('data-state', start === 'working' ? 'running' : 'waiting');
}

function appendActivity(hub: FakeHub, api: FakeHubApi, id: string, activity: Record<string, unknown>): void {
  hub.append(`conversation:${id}`, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity,
      unread_count: 0,
      queue: { queued: 1, pause_reasons: [], hold_reasons: [] },
      last_activity_at: stamp(-1),
      settings: { model: 'deepseek', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.flush();
}

async function shot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return;
  mkdirSync(SHOTS, { recursive: true });
  await page.emulateMedia({ media: 'screen' });
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({ path: `${SHOTS}/${name}.png`, animations: 'disabled' });
}

/**
 * Кольцо состояния рисуется контуром вокруг точки. Контур выходит за рамку раскладки,
 * поэтому меряется не только рамка, но и запас контура: иначе зазор с названием врёт.
 */
function ringGap(page: Page): Promise<{ gap: number; state: string; outline: number }> {
  return page.evaluate(() => {
    const mark = document.querySelector('.title-row .mark') as HTMLElement | null;
    const title = document.querySelector('.header-title') as HTMLElement | null;
    if (!mark || !title) throw new Error('нет знака или названия в шапке');
    const style = getComputedStyle(mark);
    // Положительный outline-offset выносит контур наружу рамки, отрицательный прячет его внутрь.
    const outline = style.outlineStyle === 'none'
      ? 0
      : Math.max(0, Number.parseFloat(style.outlineWidth) + (Number.parseFloat(style.outlineOffset) || 0));
    const markBox = mark.getBoundingClientRect();
    return { gap: title.getBoundingClientRect().left - (markBox.right + outline), state: mark.dataset.state ?? '', outline };
  });
}

test('header_status_ring_keeps_the_same_gap_to_the_title_in_every_state', async ({ page }) => {
  const { hub, api, id } = await boot(page, { start: 'waiting' });
  // Ждёт владельца: янтарное кольцо.
  await expect(page.locator('.title-row .mark')).toHaveAttribute('data-state', 'waiting');
  const waiting = await ringGap(page);
  // Идёт ход: бирюзовое кольцо.
  appendActivity(hub, api, id, { state: 'working', evidence: { phase: 'generating' }, since: stamp(-1) });
  await expect(page.locator('.title-row .mark')).toHaveAttribute('data-state', 'running');
  const running = await ringGap(page);
  // Спокойная сессия: маленькая точка, зазор которой считается образцом.
  appendActivity(hub, api, id, { state: 'idle', evidence: { kind: 'idle', last_turn_at: stamp(-1) }, since: stamp(-1) });
  await expect(page.locator('.title-row .mark')).toHaveAttribute('data-state', 'live');
  const idle = await ringGap(page);

  expect(idle.outline, 'у точки нет контура за рамкой').toBe(0);
  expect(idle.gap, 'зазор точки до названия').toBeGreaterThanOrEqual(5.5);
  for (const [name, state] of [['ожидание', waiting], ['ход', running]] as const) {
    expect(state.gap, `зазор кольца (${name}) равен зазору точки`).toBeCloseTo(idle.gap, 0);
  }
});

test('limits_strip_spaces_both_windows_the_same_way', async ({ page }) => {
  await boot(page);
  const segments = await page.evaluate(() => {
    const read = (window: string) => {
      const meter = document.querySelector(`.limits .meter[data-window="${window}"]`);
      if (!meter) throw new Error(`нет окна ${window}`);
      const rect = (selector: string) => (meter.querySelector(selector) as HTMLElement).getBoundingClientRect();
      // Метка растянута по своей колонке: глаз меряет зазор от букв, а не от края колонки.
      const ink = (selector: string) => {
        const range = document.createRange();
        range.selectNodeContents(meter.querySelector(selector)!);
        return range.getBoundingClientRect();
      };
      const tag = ink('.tag');
      const bar = rect('.bar');
      const pct = ink('.pct');
      return {
        tag: (meter.querySelector('.tag') as HTMLElement).textContent?.trim() ?? '',
        pct: (meter.querySelector('.pct') as HTMLElement).textContent?.trim() ?? '',
        tagToBar: bar.left - tag.right,
        barToPct: pct.left - bar.right,
        barWidth: bar.width
      };
    };
    return { five: read('five'), week: read('week') };
  });

  expect(segments.five.tag, 'первое окно — пятичасовое').toBe('5ч');
  expect(segments.week.tag, 'второе окно — недельное').toBe('нед');
  expect(segments.five.tagToBar, 'зазор после метки недели как после метки 5ч').toBeCloseTo(segments.week.tagToBar, 0);
  expect(segments.week.tagToBar, 'зазор после метки недели').toBeGreaterThanOrEqual(1.5);
  expect(segments.five.barToPct, 'зазор после полоски недели как после полоски 5ч').toBeCloseTo(segments.week.barToPct, 0);
  expect(segments.week.barToPct, 'зазор после полоски недели').toBeGreaterThanOrEqual(1.5);
  expect(segments.week.barWidth, 'полоски одной ширины').toBeCloseTo(segments.five.barWidth, 0);
});

test('small_gauge_fill_is_clipped_by_its_track', async ({ page }) => {
  await boot(page);
  const fill = await page.evaluate(() => {
    // Недельное окно заполнено на шестнадцать процентов: заливка уже, чем её скругление.
    const track = document.querySelector('.limits .meter[data-window="week"] .bar [role="meter"]') as HTMLElement | null;
    if (!track) throw new Error('нет полоски недели');
    const span = track.querySelector('span') as HTMLElement;
    const trackStyle = getComputedStyle(track);
    const fillStyle = getComputedStyle(span);
    return {
      trackOverflow: trackStyle.overflow,
      trackRadius: Number.parseFloat(trackStyle.borderTopLeftRadius),
      trackHeight: track.getBoundingClientRect().height,
      fillRadius: Number.parseFloat(fillStyle.borderTopLeftRadius),
      now: track.getAttribute('aria-valuenow')
    };
  });
  expect(fill.now, 'неделя заполнена на 16%').toBe('16');
  expect(fill.trackOverflow, 'дорожка обрезает заливку').toBe('hidden');
  expect(fill.trackRadius, 'дорожка круглая').toBeGreaterThanOrEqual(fill.trackHeight / 2 - 0.5);
  expect(fill.fillRadius, 'заливка не скругляет сама себя').toBe(0);
});

test('copy_button_centres_its_icon_and_the_code_reserves_room_for_it', async ({ page }) => {
  await boot(page);
  await expect(page.locator('.prose .copy-button')).toBeVisible();
  const measured = await page.evaluate(() => {
    const button = document.querySelector('.prose .copy-button') as HTMLElement;
    const icon = button.querySelector('svg') as SVGElement;
    const pre = document.querySelector('.prose pre') as HTMLElement;
    const buttonBox = button.getBoundingClientRect();
    const iconBox = icon.getBoundingClientRect();
    const preBox = pre.getBoundingClientRect();
    const paddingRight = Number.parseFloat(getComputedStyle(pre).paddingRight);
    return {
      iconCentre: iconBox.left + iconBox.width / 2,
      buttonCentre: buttonBox.left + buttonBox.width / 2,
      codeRight: preBox.right - paddingRight,
      buttonLeft: buttonBox.left
    };
  });
  expect(Math.abs(measured.iconCentre - measured.buttonCentre), 'значок по центру кнопки').toBeLessThanOrEqual(1);
  expect(measured.codeRight, 'строка кода кончается до кнопки').toBeLessThanOrEqual(measured.buttonLeft + 0.5);
});

test('long_code_line_scrolled_to_the_end_stops_before_the_copy_button', async ({ page }) => {
  await boot(page, { longCode: true });
  const pre = page.locator('.prose pre');
  await expect(pre.locator('code')).toContainText('end');
  const measured = await pre.evaluate((node) => {
    node.scrollLeft = node.scrollWidth;
    const code = node.querySelector('code') as HTMLElement;
    const button = document.querySelector('.prose .copy-button') as HTMLElement;
    return {
      scrollLeft: node.scrollLeft,
      codeRight: code.getBoundingClientRect().right,
      buttonLeft: button.getBoundingClientRect().left
    };
  });
  expect(measured.scrollLeft, 'длинная строка листается вбок').toBeGreaterThan(0);
  expect(measured.codeRight, 'конец длинной строки не уходит под кнопку').toBeLessThanOrEqual(measured.buttonLeft + 0.5);
});
test('expanded_tool_output_is_inset_symmetrically_like_the_card_head', async ({ page }) => {
  await boot(page);
  await page.locator('.tool .head').last().click();
  await expect(page.locator('.tool .out pre')).toContainText('test result: ok. 3 passed');
  const measured = await page.evaluate(() => {
    // Раскрытый вывод живёт в карточке инструмента, а не в прозе ответа.
    const tool = [...document.querySelectorAll<HTMLElement>('.tool')].find((node) => node.querySelector('.out'));
    if (!tool) throw new Error('нет раскрытого вывода инструмента');
    const head = tool.querySelector('.head') as HTMLElement;
    const out = tool.querySelector('.out') as HTMLElement;
    const toolBox = tool.getBoundingClientRect();
    const outBox = out.getBoundingClientRect();
    return {
      left: outBox.left - toolBox.left,
      right: toolBox.right - outBox.right,
      headInset: Number.parseFloat(getComputedStyle(head).paddingLeft)
    };
  });
  expect(measured.left, 'вывод вложен как шапка карточки').toBeCloseTo(measured.headInset, 0);
  expect(measured.right, 'отступ вывода справа такой же, как слева').toBeCloseTo(measured.left, 0);
});
test('queue_card_aligns_its_text_and_actions_and_spaces_them_evenly', async ({ page }, info) => {
  test.skip(info.project.name !== 'phone', 'раскладка очереди в две строки живёт на телефоне');
  await boot(page);
  const pill = page.locator('.strip .pill').first();
  await expect(pill).toBeVisible();
  await expect(page.getByRole('button', { name: /^Влить/ }).first()).toBeVisible();
  const measured = await page.evaluate(() => {
    const pill = document.querySelector('.strip .pill') as HTMLElement;
    const body = pill.querySelector('.body') as HTMLElement;
    const steer = pill.querySelector('.steer') as HTMLElement;
    const pillBox = pill.getBoundingClientRect();
    const range = (el: Element) => {
      const node = document.createRange();
      node.selectNodeContents(el);
      return [...node.getClientRects()];
    };
    // Отступы меряются по чернилам строк, а рамки блока текста и действия — для проверки скругления:
    // верх блока текста и есть рамка первой строки, а «Влить» — его чернила, не цель касания.
    const whole = document.createRange();
    whole.selectNodeContents(body);
    const bodyText = whole.getBoundingClientRect();
    const steerRects = range(steer);
    // Чернила действия могут лечь несколькими прямоугольниками: берём общий охват.
    const steerText = {
      left: Math.min(...steerRects.map((rect) => rect.left)),
      right: Math.max(...steerRects.map((rect) => rect.right)),
      top: Math.min(...steerRects.map((rect) => rect.top)),
      bottom: Math.max(...steerRects.map((rect) => rect.bottom))
    };
    const bodyBox = body.getBoundingClientRect();
    const overflowBox = (pill.querySelector('.row-actions .overflow') as HTMLElement).getBoundingClientRect();
    const pad = getComputedStyle(pill);
    const paddingTop = Number.parseFloat(pad.paddingTop);
    const paddingLeft = Number.parseFloat(pad.paddingLeft);
    const root = getComputedStyle(document.documentElement);
    const radiusCard = Number.parseFloat(root.getPropertyValue('--radius-card'));
    const radius = Number.parseFloat(getComputedStyle(pill).borderTopLeftRadius);
    // Браузер не даёт радиусу быть больше половины меньшей стороны.
    const used = Math.min(radius, pillBox.width / 2, pillBox.height / 2);
    // Точка внутри скруглённого прямоугольника: расстояние до ближайшего центра угла не больше радиуса.
    const outside = (x: number, y: number) => {
      const dx = Math.max(pillBox.left + used - x, 0, x - (pillBox.right - used));
      const dy = Math.max(pillBox.top + used - y, 0, y - (pillBox.bottom - used));
      return dx * dx + dy * dy > used * used + 0.5;
    };
    // Углы текста не должны заходить в скругление карточки: там фон уже не нарисован.
    const escaping = [
      { corner: 'первая строка слева', x: bodyBox.left, y: bodyBox.top },
      { corner: 'первая строка справа', x: bodyBox.right, y: bodyBox.top },
      { corner: 'действия слева', x: steerText.left, y: steerText.bottom },
      { corner: 'действия справа', x: steerText.right, y: steerText.bottom }
    ].filter((point) => outside(point.x, point.y)).map((point) => point.corner);
    return {
      bodyLeft: bodyText.left,
      steerLeft: steerText.left,
      top: bodyText.top - pillBox.top,
      middle: steerText.top - bodyText.bottom,
      bottom: pillBox.bottom - steerText.bottom,
      // Чернила текста упираются в поля карточки: слева — началом строки, справа — самым длинным её концом.
      leftInset: bodyText.left - pillBox.left,
      rightInset: pillBox.right - bodyText.right,
      // Кнопка «⋯» держит цель касания, поэтому её рамка, а не чернила значка, мерится до края.
      overflowInset: pillBox.right - overflowBox.right,
      steerHeight: steer.getBoundingClientRect().height,
      target: Number.parseFloat(root.getPropertyValue('--target')),
      radius,
      radiusCard,
      escaping,
      // Рамка первой строки целиком лежит в поле карточки, а не в её скруглении.
      firstLineInPadding: bodyBox.top >= pillBox.top + paddingTop - 0.5 && bodyBox.left >= pillBox.left + paddingLeft - 0.5 && bodyBox.right <= pillBox.right + 0.5
    };
  });
  expect(Math.abs(measured.bodyLeft - measured.steerLeft), 'текст и «Влить» на одной линии').toBeLessThanOrEqual(1);
  expect(measured.top, 'отступ над текстом как под действиями').toBeCloseTo(measured.bottom, 0);
  expect(measured.middle, 'зазор текста до действий как сверху').toBeCloseTo(measured.top, 0);
  // Справа текст не упирается в край: его правый зазор не меньше левого (допуск на перенос строки).
  expect(measured.rightInset, 'правый зазор текста не меньше левого').toBeGreaterThanOrEqual(measured.leftInset - 1);
  expect(measured.overflowInset, '«⋯» стоит от правого края как подпись от левого').toBeCloseTo(measured.leftInset, 0);
  expect(measured.steerHeight, 'цель нажатия не меньше --target').toBeGreaterThanOrEqual(measured.target - 0.5);
  expect(measured.firstLineInPadding, 'первая строка внутри поля карточки').toBe(true);
  expect(measured.escaping, 'текст не заходит в скругление карточки').toEqual([]);
  expect(measured.radius, 'скругление многострочной карточки как у карточки').toBe(measured.radiusCard);
});

/** Отложенные живут в снимке беседы, а не в истории: их кладут в снимок до перезагрузки. */
function seedScheduled(api: FakeHubApi, id: string, rows: { text: string; state?: ScheduledMessage['state'] }[]): void {
  const snap = api.conversationData(id);
  snap.scheduled = rows.map((row, index) => ({
    id: api.id(),
    conversation_id: id,
    run_at: stamp(60 * (index + 1)),
    content: [{ type: 'text', text: row.text }],
    attachment_ids: [],
    state: row.state ?? 'pending',
    input_id: null,
    attempts: row.state === 'failed' ? 1 : 0
  }));
  api.hub.setSnapshot(`conversation:${id}`, () => snap);
}

interface CardShape {
  height: number;
  textHeight: number;
  clipped: boolean;
  topInset: number;
  bottomInset: number;
  leftInset: number;
  textRightInset: number;
  actionInset: number | null;
  textToIcon: number | null;
  firstLineTop: number;
  firstLineBottom: number;
  iconTop: number | null;
  targetHeight: number | null;
  line: number;
  target: number;
  radius: number;
  radiusCard: number;
  escaping: string[];
}

/**
 * Карточка меряется по строкам блока текста, а не по чернилам отдельных букв: зазоры владелец
 * видит между строкой и краем карточки, а запас под выносные буквы — частью строки.
 * «⋯» меряется по значку: цель касания выше него и в строку не помещается.
 */
function shapeOf(pill: Locator): Promise<CardShape> {
  return pill.evaluate((node) => {
    const pillBox = node.getBoundingClientRect();
    const body = node.querySelector('.body') as HTMLElement;
    const bodyBox = body.getBoundingClientRect();
    const bodyStyle = getComputedStyle(body);
    const line = Number.parseFloat(bodyStyle.lineHeight) || Number.parseFloat(bodyStyle.fontSize) * 1.4;
    const overflow = node.querySelector('.row-actions .overflow') as HTMLElement | null;
    const overflowBox = overflow?.getBoundingClientRect() ?? null;
    const iconBox = (overflow?.querySelector('svg') as SVGSVGElement | null)?.getBoundingClientRect() ?? null;
    const range = document.createRange();
    range.selectNodeContents(body);
    const rects = [...range.getClientRects()];
    // Строки за пределом обрезки остаются в прямоугольниках диапазона, но владелец их не видит.
    const text = {
      left: Math.max(Math.min(...rects.map((rect) => rect.left)), bodyBox.left),
      right: Math.min(Math.max(...rects.map((rect) => rect.right)), bodyBox.right),
      bottom: Math.min(Math.max(...rects.map((rect) => rect.bottom)), bodyBox.bottom)
    };
    const root = getComputedStyle(document.documentElement);
    const radius = Number.parseFloat(getComputedStyle(node).borderTopLeftRadius);
    const used = Math.min(radius, pillBox.width / 2, pillBox.height / 2);
    // Точка внутри скруглённого прямоугольника: расстояние до ближайшего центра угла не больше радиуса.
    const outside = (x: number, y: number) => {
      const dx = Math.max(pillBox.left + used - x, 0, x - (pillBox.right - used));
      const dy = Math.max(pillBox.top + used - y, 0, y - (pillBox.bottom - used));
      return dx * dx + dy * dy > used * used + 0.5;
    };
    return {
      height: pillBox.height,
      textHeight: bodyBox.height,
      clipped: Math.max(...rects.map((rect) => rect.bottom)) > bodyBox.bottom + 0.5,
      topInset: bodyBox.top - pillBox.top,
      bottomInset: pillBox.bottom - bodyBox.bottom,
      leftInset: text.left - pillBox.left,
      textRightInset: pillBox.right - text.right,
      // Кнопка «⋯» держит цель касания, поэтому её рамка, а не чернила значка, мерится до края.
      actionInset: overflowBox ? pillBox.right - overflowBox.right : null,
      textToIcon: iconBox ? iconBox.left - text.right : null,
      // Полоса первой строки — это её место в блоке текста, а не чернила подписи другим кеглем.
      firstLineTop: bodyBox.top,
      firstLineBottom: bodyBox.top + line,
      iconTop: iconBox ? iconBox.top : null,
      targetHeight: overflowBox ? overflowBox.height : null,
      line,
      target: Number.parseFloat(root.getPropertyValue('--target')),
      radius,
      radiusCard: Number.parseFloat(root.getPropertyValue('--radius-card')),
      // Углы текста не должны заходить в скругление карточки: там фон уже не нарисован.
      escaping: [
        { corner: 'первая строка слева', x: text.left, y: bodyBox.top },
        { corner: 'первая строка справа', x: text.right, y: bodyBox.top },
        { corner: 'последняя строка слева', x: text.left, y: text.bottom },
        { corner: 'последняя строка справа', x: text.right, y: text.bottom }
      ].filter((point) => outside(point.x, point.y)).map((point) => point.corner)
    };
  });
}

/** Общие требования к карточке без «Влить»: «⋯» в первой строке, равные зазоры, 12 px по бокам. */
function expectCompactCard(shape: CardShape): void {
  // Пустой строки под текстом нет: высота карточки — это строки текста и по зазору сверху и снизу.
  expect(shape.height, 'под текстом нет отдельной строки действий').toBeLessThanOrEqual(shape.textHeight + 2 * shape.topInset + 1);
  expect(Math.abs(shape.topInset - shape.bottomInset), 'верхний и нижний зазоры равны').toBeLessThanOrEqual(1);
  expect(shape.leftInset, 'текст от левого края на 12 px').toBeCloseTo(12, 0);
  expect(shape.actionInset, '«⋯» от правого края на 12 px').not.toBeNull();
  expect(shape.actionInset ?? Number.NaN, '«⋯» от правого края на 12 px').toBeCloseTo(12, 0);
  expect(shape.textRightInset, 'текст не упирается в правый край').toBeGreaterThanOrEqual(shape.leftInset - 1);
  expect(shape.targetHeight ?? 0, 'цель нажатия не меньше --target').toBeGreaterThanOrEqual(shape.target - 0.5);
  expect(shape.escaping, 'текст не заходит в скругление карточки').toEqual([]);
  expect(shape.radius, 'скругление карточки как у очереди').toBe(shape.radiusCard);
}

test('scheduled_card_puts_its_overflow_on_the_first_text_line', async ({ page }, info) => {
  test.skip(info.project.name !== 'phone', 'раскладка очереди в две строки живёт на телефоне');
  const { api, id } = await boot(page);
  seedScheduled(api, id, [{ text: 'можно тестировать, это отложенное сообщение, у тебя весь день' }]);
  await page.reload();
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const card = page.locator('.strip .item').filter({ hasText: 'Отправится в' });
  await expect(card).toHaveCount(1);
  const shape = await shapeOf(card.locator('.pill'));
  expect(shape.textHeight, 'отложенное заняло две строки').toBeGreaterThan(20);
  expect(shape.clipped, 'текст занимает обе строки целиком').toBe(false);
  expectCompactCard(shape);
  // Две строки и по половине цели касания сверху и снизу: выше — воздух сверх цели, а не зазоры.
  expect(shape.height, 'карточка в две строки не выше строк и цели касания').toBeLessThanOrEqual(2 * shape.line + (shape.target - shape.line) + 1);
  // Значок 20 px выше строки текста: по центру строки он выступает над ней на 1 px.
  expect(shape.iconTop ?? Number.NEGATIVE_INFINITY, '«⋯» стоит на первой строке, а не отдельной').toBeGreaterThanOrEqual(shape.firstLineTop - 1.5);
  expect(shape.iconTop ?? Number.POSITIVE_INFINITY, '«⋯» стоит на первой строке, а не отдельной').toBeLessThanOrEqual(shape.firstLineBottom + 0.5);
  expect(shape.textToIcon ?? Number.NEGATIVE_INFINITY, 'текст переносится до «⋯», а не уходит под него').toBeGreaterThanOrEqual(0);
});

test('one_line_scheduled_card_keeps_even_gaps', async ({ page }, info) => {
  test.skip(info.project.name !== 'phone', 'раскладка очереди в две строки живёт на телефоне');
  const { api, id } = await boot(page);
  seedScheduled(api, id, [{ text: 'позже' }]);
  await page.reload();
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const card = page.locator('.strip .item').filter({ hasText: 'Отправится в' });
  await expect(card).toHaveCount(1);
  const shape = await shapeOf(card.locator('.pill'));
  expect(shape.textHeight, 'отложенное заняло одну строку').toBeLessThan(30);
  expectCompactCard(shape);
  // Одна строка и по половине цели касания сверху и снизу: карточка не выше самой цели.
  expect(shape.height, 'карточка в одну строку не выше цели касания').toBeLessThanOrEqual(shape.target + 1);
  // Значок 20 px выше строки текста: по центру строки он выступает над ней на 1 px.
  expect(shape.iconTop ?? Number.NEGATIVE_INFINITY, '«⋯» стоит на первой строке').toBeGreaterThanOrEqual(shape.firstLineTop - 1.5);
  expect(shape.iconTop ?? Number.POSITIVE_INFINITY, '«⋯» стоит на первой строке').toBeLessThanOrEqual(shape.firstLineBottom + 0.5);
});

test('failed_card_keeps_even_gaps_without_an_action_row', async ({ page }, info) => {
  test.skip(info.project.name !== 'phone', 'раскладка очереди в две строки живёт на телефоне');
  const { api, id } = await boot(page);
  seedScheduled(api, id, [{ text: 'не ушло', state: 'failed' }]);
  await page.reload();
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const card = page.locator('.strip .item').filter({ hasText: 'Не отправлено' });
  await expect(card).toHaveCount(1);
  const shape = await shapeOf(card.locator('.pill'));
  expect(shape.textHeight, 'неотправленное заняло одну строку').toBeLessThan(30);
  expect(shape.height, 'высота — строки текста и равные зазоры').toBeLessThanOrEqual(shape.textHeight + 2 * shape.topInset + 1);
  expect(Math.abs(shape.topInset - shape.bottomInset), 'верхний и нижний зазоры равны').toBeLessThanOrEqual(1);
  expect(shape.leftInset, 'текст от левого края на 12 px').toBeCloseTo(12, 0);
  expect(shape.escaping, 'текст не заходит в скругление карточки').toEqual([]);
  expect(shape.radius, 'скругление карточки как у очереди').toBe(shape.radiusCard);
});

test('scheduled_card_shots', async ({ page }, info) => {
  test.skip(!SHOTS, 'снимки для отчёта снимаются отдельным прогоном');
  test.skip(info.project.name !== 'phone', 'карточки снимаются на телефоне');
  const dir = SHOTS;
  if (!dir) return;
  mkdirSync(dir, { recursive: true });
  await page.setViewportSize({ width: 390, height: 844 });
  for (const [palette, theme] of [['aquamarine', 'dark'], ['standard', 'light']] as const) {
    const { api, id } = await boot(page, { palette, theme });
    seedScheduled(api, id, [
      { text: 'можно тестировать, это отложенное сообщение, у тебя весь день' },
      { text: 'позже' }
    ]);
    await page.reload();
    await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('.strip .item').filter({ hasText: 'Отправится в' })).toHaveCount(2);
    await settle(page);
    await page.locator('.strip').screenshot({ path: `${dir}/scheduled-cards-${palette}-${theme}.png`, animations: 'disabled' });
  }
});

test('chat_screen_for_before_and_after_shots', async ({ page }, info) => {
  test.skip(!SHOTS, 'снимки для отчёта снимаются отдельным прогоном');
  if (info.project.name === 'desktop') {
    await page.setViewportSize({ width: 1440, height: 900 });
    await boot(page, { palette: 'aquamarine', theme: 'dark' });
    await expect(page.locator('.prose pre code')).toBeVisible();
    await settle(page);
    await shot(page, 'chat-desktop-aquamarine-dark');
    return;
  }
  await page.setViewportSize({ width: 390, height: 844 });
  await boot(page, { palette: 'aquamarine', theme: 'dark' });
  await expect(page.locator('.prose pre code')).toBeVisible();
  await page.locator('.tool .head').last().click();
  await expect(page.locator('.tool .out pre')).toContainText('test result: ok. 3 passed');
  await settle(page);
  await shot(page, 'chat-phone-aquamarine-dark');

  await boot(page, { palette: 'standard', theme: 'light' });
  await expect(page.locator('.prose pre code')).toBeVisible();
  await page.locator('.tool .head').last().click();
  await expect(page.locator('.tool .out pre')).toContainText('test result: ok. 3 passed');
  await settle(page);
  await shot(page, 'chat-phone-standard-light');
});

async function settle(page: Page): Promise<void> {
  await page.evaluate(() => document.fonts.ready);
  await page.evaluate(() => {
    delete document.documentElement.dataset.themeSwitching;
    document.documentElement.dataset.motion = 'off';
  });
  await page.waitForTimeout(150);
}
