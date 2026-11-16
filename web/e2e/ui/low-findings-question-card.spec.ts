import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function boot(page: Page, viewport: { width: number; height: number }) {
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
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
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
