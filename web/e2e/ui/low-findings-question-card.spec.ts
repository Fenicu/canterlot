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
