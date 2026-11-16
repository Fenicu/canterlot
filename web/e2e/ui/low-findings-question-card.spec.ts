import type { Page } from '@playwright/test';
import type { HistoryBlock, HistoryInput, HistoryItem } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';

async function boot(page: Page, viewport: { width: number; height: number }, seed?: (api: FakeHubApi, id: string) => void) {
  await page.setViewportSize(viewport);
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, {
    id: 'workstation',
    name: 'workstation',
    harnesses: [
      {
        id: 'claude',
        title: 'Claude Code',
        provider: 'claude',
        models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium', 'high'] }],
        permission_modes: ['owner', 'ask', 'bypass', 'read_only']
      }
    ]
  });
  const id = seedConversation(api, {
    title: 'Карточка вопроса',
    model: 'opus-5.5',
    activity: { state: 'idle', evidence: {}, since: new Date().toISOString() }
  });
  const detail = api.details.get(id);
  if (detail) {
    detail.harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
    detail.machine = { id: 'workstation', name: 'workstation' };
    detail.model = 'opus-5.5';
    detail.effort = 'medium';
    detail.permission_mode = 'owner';
  }
  seed?.(api, id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

function historyBlock(api: FakeHubApi, seq: number, text: string): HistoryBlock {
  const blockId = api.id();
  api.blocks.set(blockId, { text, rev: 1, state: 'final' });
  return {
    kind: 'block',
    id: blockId,
    block_kind: 'text',
    content: text,
    content_bytes: text.length,
    content_truncated: false,
    created_seq: String(seq),
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'agent',
    model: 'opus',
    tool_name: null,
    summary: null,
    parent_block_id: null,
    input_command_id: null,
    line_count: text.split('\n').length
  };
}

function historyMessage(api: FakeHubApi, seq: number, text: string): HistoryInput {
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: `message-${seq}-${api.id()}`,
    state: 'accepted',
    content: [{ type: 'text', text }],
    attachments: [],
    created_seq: String(seq),
    feed_seq: String(seq),
    created_at: AT
  };
}

/** Длинная история: лента заведомо выше окна, и у низа есть куда стоять. */
function seedLongHistory(api: FakeHubApi, id: string, count = 60): string {
  const items: HistoryItem[] = [];
  let last = '';
  for (let seq = 1; seq <= count; seq++) {
    const text = seq % 5 === 0 ? `Моё сообщение ${seq}` : `Ответ ${seq}: строка истории разговора`;
    items.push(seq % 5 === 0 ? historyMessage(api, seq, text) : historyBlock(api, seq, text));
    last = text;
  }
  api.histories.set(id, items);
  api.bindConversation(id);
  return last;
}

function openQuestion(hub: FakeHub, api: FakeHubApi, id: string, questions: unknown[]) {
  hub.append(`conversation:${id}`, {
    kind: 'request_opened',
    conversation_id: id,
    payload: { request_id: api.id(), kind: 'question', payload: { questions } }
  });
  hub.flush();
}

const OPTIONS = [
  { label: 'Мягкий ребейз', description: 'Линейная история без лишнего коммита' },
  { label: 'Слияние коммитом', description: 'Сохранить граф ветки' },
  { label: 'Отложить до утра' },
  { label: 'Спросить владельца' }
];

// Варианты в одну строку: на коротком экране карточка обязана показать два целых варианта.
const PLAIN_OPTIONS = ['Мягкий ребейз', 'Слияние коммитом', 'Отложить до утра', 'Спросить владельца'];

async function settled(page: Page) {
  await expect(page.locator('.question-card')).toBeVisible();
  await expect
    .poll(
      () => page.locator('.request-cards-slot > .card-root').evaluateAll((elements) => elements.every((element) => element.getAnimations().length === 0)),
      { timeout: 5_000 }
    )
    .toBe(true);
}

test('the sticky answer bar keeps the tint of the arriving question card', async ({ page }) => {
  const { hub, api, id } = await boot(page, { width: 412, height: 892 });

  // Проба снимает цвета каждые 100 мс с момента взвода, ещё до прихода запроса: так в окно
  // попадает вся секунда, пока карточка теплится тоном акцента.
  await page.evaluate(() => {
    const state = { samples: [] as { at: number; card: string | null; actions: string | null; animating: boolean }[], done: false };
    (window as unknown as { __arrival?: typeof state }).__arrival = state;
    const start = performance.now();
    const tick = () => {
      const card = document.querySelector<HTMLElement>('.request-cards-slot > .card-root');
      const actions = card?.querySelector<HTMLElement>('.actions') ?? null;
      state.samples.push({
        at: Math.round(performance.now() - start),
        card: card ? getComputedStyle(card).backgroundColor : null,
        actions: actions ? getComputedStyle(actions).backgroundColor : null,
        // Svelte приписывает к имени кадра область компонента: сверяем по окончанию.
        animating: card ? card.getAnimations().some((animation) => (animation as CSSAnimation).animationName.endsWith('request-accent')) : false
      });
      if (performance.now() - start < 1_400) setTimeout(tick, 100);
      else state.done = true;
    };
    tick();
  });

  openQuestion(hub, api, id, [{ id: 'q0', header: 'ВЕТКА', text: 'Как слить ветку в основную?', options: OPTIONS }]);
  await page.waitForFunction(() => (window as unknown as { __arrival?: { done: boolean } }).__arrival?.done === true, undefined, { timeout: 10_000 });
  const samples = await page.evaluate(() => (window as unknown as { __arrival: { samples: { at: number; card: string | null; actions: string | null; animating: boolean }[] } }).__arrival.samples);

  const painted = samples.filter((sample) => sample.card !== null);
  expect(painted.length, 'проба увидела карточку').toBeGreaterThanOrEqual(6);
  // Проба действительно попала в движение: карточка красилась не только своим спокойным фоном.
  const settledColor = painted.at(-1)!.card;
  expect(painted.some((sample) => sample.card !== settledColor), 'вход карточки попал в окно замера').toBe(true);
  expect(painted.some((sample) => sample.animating), 'вход карточки ещё шёл').toBe(true);
  // Полоса «Ответить» красится тем же цветом, что карточка под ней, в любой момент входа.
  for (const sample of painted) expect(sample.actions, `на ${sample.at} мс`).toBe(sample.card);
});

test('two questions without headers keep the counter on the title line', async ({ page }) => {
  const { hub, api, id } = await boot(page, { width: 412, height: 892 });
  openQuestion(hub, api, id, [
    { id: 'q0', text: 'Какой вариант собрать?', options: ['Первый', 'Второй'] },
    { id: 'q1', text: 'Куда положить отчёт?', options: ['В папку', 'В чат'] }
  ]);
  await settled(page);
  await expect(page.locator('.counter')).toHaveCount(2);

  const counters = await page.evaluate(() =>
    [...document.querySelectorAll<HTMLElement>('.counter')].map((counter) => {
      const row = counter.parentElement!;
      const box = counter.getBoundingClientRect();
      const title = row.querySelector('.q-text')?.getBoundingClientRect();
      return {
        text: (counter.textContent ?? '').trim(),
        rowHasOtherChildren: [...row.children].some((child) => child !== counter),
        rowHasOwnText: [...row.childNodes].some((node) => node.nodeType === Node.TEXT_NODE && (node.textContent ?? '').trim() !== ''),
        visible: box.width > 0 && box.height > 0,
        fontSize: Number.parseFloat(getComputedStyle(counter).fontSize),
        sameRowAsTitle: title ? box.top < title.bottom && box.bottom > title.top : false
      };
    })
  );

  for (const [index, counter] of counters.entries()) {
    expect(counter.text).toBe(`${index + 1} из 2`);
    // Строка с одним счётчиком — тот самый лишний ряд в 22 px, которого быть не должно.
    expect(counter.rowHasOtherChildren || counter.rowHasOwnText, `счётчик ${counter.text} стоит в своей строке`).toBe(true);
    expect(counter.sameRowAsTitle, `счётчик ${counter.text} стоит на строке вопроса`).toBe(true);
    expect(counter.visible).toBe(true);
    expect(counter.fontSize).toBeGreaterThanOrEqual(11);
  }
});

type Landscape = {
  viewport: { width: number; height: number };
  header: Box | null;
  feed: Box | null;
  slot: Box | null;
  card: Box | null;
  actions: Box | null;
  composer: Box | null;
  scrollTop: number;
  options: { label: string; box: Box; hit: string | null; behindActions: boolean }[];
};

type Box = { top: number; bottom: number; left: number; right: number; height: number };

async function measure(page: Page): Promise<Landscape> {
  return page.evaluate(() => {
    const rect = (element: Element | null): Box | null => {
      if (!element) return null;
      const box = element.getBoundingClientRect();
      return { top: box.top, bottom: box.bottom, left: box.left, right: box.right, height: box.height };
    };
    const slot = document.querySelector<HTMLElement>('.request-cards-slot')!;
    const card = slot.querySelector<HTMLElement>(':scope > .card-root')!;
    const actions = card.querySelector<HTMLElement>('.actions')!;
    const options = [...card.querySelectorAll<HTMLElement>('[data-option]')].map((option) => {
      const box = option.getBoundingClientRect();
      const hit = document.elementFromPoint((box.left + box.right) / 2, (box.top + box.bottom) / 2);
      return {
        label: option.dataset.option ?? '',
        box: { top: box.top, bottom: box.bottom, left: box.left, right: box.right, height: box.height },
        hit: hit?.closest('[data-option]')?.getAttribute('data-option') ?? null,
        behindActions: hit !== null && hit.closest('.actions') !== null
      };
    });
    return {
      viewport: { width: innerWidth, height: innerHeight },
      header: rect(document.querySelector('.chat-header')),
      feed: rect(document.querySelector('.feed-wrap')),
      slot: rect(slot),
      card: rect(card),
      actions: rect(actions),
      composer: rect(document.querySelector('section.composer')),
      scrollTop: card.scrollTop,
      options
    };
  });
}

const SLOTS = [
  { width: 892, height: 412 },
  { width: 412, height: 892 }
] as const;

for (const viewport of SLOTS) {
  test(`a single question shows two options above the answer bar at ${viewport.width}x${viewport.height}`, async ({ page }) => {
    const { hub, api, id } = await boot(page, viewport);
    openQuestion(hub, api, id, [{ id: 'q0', header: 'ВЕТКА', text: 'Как слить ветку в основную?', options: PLAIN_OPTIONS }]);
    await settled(page);
    const before = await measure(page);

    expect(before.card).not.toBeNull();
    expect(before.actions).not.toBeNull();
    // Первые два варианта целиком видны над закреплённой «Ответить» и принимают касание.
    for (const option of before.options.slice(0, 2)) {
      expect(option.box.top, `${option.label} не срезан верхом карточки`).toBeGreaterThanOrEqual(before.card!.top - 1);
      expect(option.box.bottom, `${option.label} выше «Ответить»`).toBeLessThanOrEqual(before.actions!.top + 1);
      expect(option.hit, `${option.label} принимает касание`).toBe(option.label);
      expect(option.behindActions, `${option.label} не под полосой «Ответить»`).toBe(false);
    }
    // Кнопка ответа и поле ввода остаются в окне.
    expect(before.actions!.bottom).toBeLessThanOrEqual(before.viewport.height + 1);
    expect(before.composer!.bottom).toBeLessThanOrEqual(before.viewport.height + 1);

    if (viewport.height <= 480) {
      // Короткий экран: карточка запроса занимает место ленты целиком, от шапки до поля ввода.
      expect(before.feed!.height, 'лента уступила место карточке').toBe(0);
      expect(before.slot!.top, 'слот начинается под шапкой').toBeLessThanOrEqual(before.header!.bottom + 1);
      // Заголовок вопроса уезжает вверх прокруткой, а не отнимает высоту у вариантов.
      expect(before.scrollTop).toBeGreaterThan(0);
    } else {
      // Портрет не изменился: карточка стоит на месте, заголовок вопроса виден.
      expect(before.scrollTop).toBe(0);
      await expect(page.locator('.q-text')).toBeInViewport();
    }
  });
}

/** Запас ленты до последней строки: 0 — лента стоит у хвоста. */
async function feedGap(page: Page): Promise<number> {
  return page.locator('.feed').evaluate((element) => Math.round(element.scrollHeight - element.scrollTop - element.clientHeight));
}

const JUMP_SLOTS = [
  { width: 892, height: 412 },
  { width: 412, height: 450 }
] as const;

for (const viewport of JUMP_SLOTS) {
  test(`the feed keeps the newest message while a question opens and closes at ${viewport.width}x${viewport.height}`, async ({ page }) => {
    let lastText = '';
    const { hub, api, id } = await boot(page, viewport, (fakeApi, conversationId) => {
      lastText = seedLongHistory(fakeApi, conversationId);
    });

    // Открытие уже прижало ленту к хвосту: от этого места и считаем.
    await expect.poll(() => feedGap(page), { timeout: 10_000 }).toBeLessThanOrEqual(2);
    const before = await page.locator('.feed').evaluate((element) => element.scrollTop);
    expect(before, 'история длиннее окна').toBeGreaterThan(100);

    openQuestion(hub, api, id, [{ id: 'q0', text: 'Как слить ветку?', options: ['Мягкий ребейз', 'Слияние коммитом'] }]);
    await expect(page.locator('.question-card')).toBeVisible();

    // Проба кадров: положение ленты на каждом кадре, пока карточка занимает и отдаёт место.
    await page.evaluate(() => {
      const state: { tops: { at: number; top: number }[]; stop: boolean } = { tops: [], stop: false };
      (window as unknown as { __feedSpot?: typeof state }).__feedSpot = state;
      const start = performance.now();
      const tick = () => {
        const feed = document.querySelector<HTMLElement>('.feed');
        state.tops.push({ at: Math.round(performance.now() - start), top: feed ? Math.round(feed.scrollTop) : -1 });
        if (!state.stop) requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    });

    await page.locator('[data-option="Мягкий ребейз"]').click();
    await page.getByRole('button', { name: 'Ответить' }).click();
    await expect(page.locator('.request-cards-slot')).toHaveCount(0, { timeout: 10_000 });
    await page.waitForTimeout(600);
    await page.evaluate(() => {
      (window as unknown as { __feedSpot: { stop: boolean } }).__feedSpot.stop = true;
    });
    const samples = await page.evaluate(
      () => (window as unknown as { __feedSpot: { tops: { at: number; top: number }[] } }).__feedSpot.tops
    );

    expect(samples.length, 'проба сняла кадры').toBeGreaterThan(10);
    const jumped = samples.filter((sample) => sample.top <= 0);
    expect(
      jumped,
      `лента стояла в начале истории на ${jumped.map((sample) => `${sample.at} мс`).join(', ')}`
    ).toEqual([]);
    await expect.poll(() => feedGap(page), { timeout: 10_000 }).toBeLessThanOrEqual(2);
    await expect(page.getByText(lastText, { exact: true })).toBeInViewport();
    expect(await page.locator('.feed').evaluate((element) => element.scrollTop)).toBeGreaterThan(100);
  });
}

for (const viewport of JUMP_SLOTS) {
  test(`the feed keeps a mid-history position while a question opens and closes at ${viewport.width}x${viewport.height}`, async ({ page }) => {
    const { hub, api, id } = await boot(page, viewport, (fakeApi, conversationId) => {
      seedLongHistory(fakeApi, conversationId);
    });
    await expect.poll(() => feedGap(page), { timeout: 10_000 }).toBeLessThanOrEqual(2);

    // Владелец читает историю выше хвоста: прижатие к низу снято его же прокруткой.
    await page.locator('.feed').evaluate((element) => element.scrollTo({ top: 1200 }));
    await page.waitForTimeout(300);
    const reading = await page.locator('.feed').evaluate((element) => element.scrollTop);
    expect(reading, 'лента стоит выше хвоста').toBeGreaterThan(100);
    expect(await feedGap(page)).toBeGreaterThan(100);

    openQuestion(hub, api, id, [{ id: 'q0', text: 'Как слить ветку?', options: ['Мягкий ребейз', 'Слияние коммитом'] }]);
    await expect(page.locator('.question-card')).toBeVisible();
    await page.locator('[data-option="Мягкий ребейз"]').click();
    await page.getByRole('button', { name: 'Ответить' }).click();
    await expect(page.locator('.request-cards-slot')).toHaveCount(0, { timeout: 10_000 });
    await page.waitForTimeout(600);

    // Карточка отдала место, а читаемая строка осталась на месте: ни начала истории, ни хвоста.
    const after = await page.locator('.feed').evaluate((element) => element.scrollTop);
    expect(Math.abs(after - reading), 'лента не сдвинулась с читаемой строки').toBeLessThanOrEqual(2);
  });
}
