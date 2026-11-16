import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedHistory } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { stubMicrophone } from './dictation-fixture';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000e4';
const CLAUDE = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };
/** Исполнитель без «Влить в ход»: так выглядит состояние из отчёта про контекст. */
const CLAUDE_PLAIN = { id: 'claude', title: 'Claude Code', provider: 'claude' };

/** Длинный черновик: поле упирается в свой предел и прокручивается. */
const LONG_DRAFT = Array.from(
  { length: 14 },
  (_, index) => `Строка ${index + 1}: проверь и закоммить, потом прогони тесты.`
).join('\n');

/** Длинная подсказка: в поле она не влезает и без обрезки уходит под кнопки. */
const LONG_SUGGESTION =
  'добавь тесты для пустой строки, для строки из одних пробелов и для пути /home/fenicu/.local/state/canterlot-worker/worktrees/01a1e117-25c6-7237-bed5-d31506012780/web/src/lib/timeline/cards/plan/PlanUpdateCard.svelte, затем прогони cargo test и npm run check, а после этого проверь, что подсказка в поле ввода не режется посреди строки, что кнопка «Подставить» вставляет весь текст целиком и что длинный путь по-прежнему переносится, а не выдавливает поле за край.';

async function boot(
  page: Page,
  opts: {
    suggestion?: string;
    context?: { tokens: number; window: number };
    live?: boolean;
    liveTurn?: boolean;
    harness?: { id: string; title: string; provider: string };
    mic?: boolean;
    queued?: string;
  } = {}
) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { harness: opts.harness ?? CLAUDE });
  seedHistory(api, id, 1);
  if (opts.context) api.pushContext(id, opts.context.tokens, opts.context.window);
  if (opts.live) {
    const detail = api.details.get(id) as Record<string, unknown>;
    detail.execution = { state: 'running', generation: 1 };
  }
  if (opts.liveTurn) {
    const detail = api.details.get(id) as Record<string, unknown>;
    detail.activity = { state: 'working', evidence: { phase: 'generating' }, since: new Date().toISOString() };
    (detail.queue as Record<string, unknown>).run_slot = {
      running: { input_id: '00000000-0000-4000-8000-000000000010' }
    };
  }
  if (opts.suggestion) api.setPromptSuggestion(id, opts.suggestion);
  if (opts.mic) {
    hub.dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
    await stubMicrophone(page, 'granted');
    await page.addInitScript(() => localStorage.setItem('canterlot.dictation', 'on'));
  }
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await expect(area).toBeVisible();
  if (opts.queued) {
    hub.append(`conversation:${id}`, {
      kind: 'input_queued',
      conversation_id: id,
      payload: {
        input_id: api.id(),
        input_seq: '50',
        idempotency_key: api.id(),
        state: 'queued',
        content: [{ type: 'text', text: opts.queued }]
      }
    });
    hub.flush();
    await expect(page.locator('.strip').getByText(opts.queued)).toBeVisible();
  }
  return { hub, api, id, area };
}

interface CutMeasured {
  capsuleTop: number;
  capsuleLeft: number;
  radius: number;
  fieldTop: number;
  fieldLeft: number;
  fieldPadLeft: number;
  trayBottom: number | null;
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

/** Где проходит срез прокрученного черновика: кромка поля относительно капсулы и лотка. */
function cutOf(page: Page): Promise<CutMeasured> {
  return page.evaluate(() => {
    const capsule = document.querySelector('[data-composer] .capsule') as HTMLElement;
    const field = document.querySelector('[data-composer] textarea') as HTMLTextAreaElement;
    const tray = document.querySelector('[data-composer] .tray') as HTMLElement | null;
    const capsuleBox = capsule.getBoundingClientRect();
    const fieldBox = field.getBoundingClientRect();
    const trayBox = tray?.getBoundingClientRect() ?? null;
    return {
      capsuleTop: capsuleBox.top,
      capsuleLeft: capsuleBox.left,
      radius: Number.parseFloat(getComputedStyle(capsule).borderTopLeftRadius) || 0,
      fieldTop: fieldBox.top,
      fieldLeft: fieldBox.left,
      fieldPadLeft: Number.parseFloat(getComputedStyle(field).paddingLeft),
      trayBottom: trayBox ? trayBox.bottom : null,
      scrollTop: field.scrollTop,
      scrollHeight: field.scrollHeight,
      clientHeight: field.clientHeight
    };
  });
}

/** Курсор в конце длинного черновика: поле прокручивается за ним до низа. */
async function scrollDraft(page: Page, area: ReturnType<Page['getByRole']>) {
  await area.fill(LONG_DRAFT);
  await area.focus();
  await area.press('End');
  await expect.poll(() => area.evaluate((node: HTMLTextAreaElement) => node.scrollTop)).toBeGreaterThan(0);
}

/** Точка среза лежит на закрашенной части капсулы, а не в её скруглении. */
function expectSlicedLineOnTheCapsule(cut: CutMeasured): void {
  const x = cut.fieldLeft - cut.capsuleLeft + cut.fieldPadLeft;
  const y = cut.fieldTop - cut.capsuleTop;
  const dx = Math.max(cut.radius - x, 0);
  const dy = Math.max(cut.radius - y, 0);
  expect(dx * dx + dy * dy, 'срез первой строки не в зоне скругления капсулы').toBeLessThanOrEqual(
    cut.radius * cut.radius + 0.5
  );
}

test('scrolled_long_draft_is_cut_below_the_capsule_top_inset', async ({ page }) => {
  const { area } = await boot(page);
  await scrollDraft(page, area);

  const cut = await cutOf(page);
  expect(cut.scrollTop, 'черновик прокручен до конца').toBeGreaterThan(0);
  expect(cut.trayBottom, 'вложений нет').toBeNull();
  // Кромка поля, где режется первая видимая строка, стоит ниже верхней кромки капсулы.
  expect(cut.fieldTop - cut.capsuleTop, 'отступ сверху вне прокрутки').toBeGreaterThanOrEqual(8);
  expectSlicedLineOnTheCapsule(cut);
});

test('scrolled_long_draft_with_attachments_is_cut_below_the_chips', async ({ page }) => {
  const { area } = await boot(page);
  await page.setInputFiles('input[type=file]', {
    name: 'Снимок экрана 2026-11-16 в 00.12.44 — чат с длинным названием.png',
    mimeType: 'image/png',
    buffer: Buffer.from(
      'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEklEQVR4nGPQmNbz//9/BggFADYPCI8NpaO2AAAAAElFTkSuQmCC',
      'base64'
    )
  });
  await expect(page.locator('[data-composer] .tray')).toBeVisible();
  await scrollDraft(page, area);

  const cut = await cutOf(page);
  expect(cut.scrollTop, 'черновик прокручен до конца').toBeGreaterThan(0);
  expect(cut.trayBottom, 'лоток вложений на месте').not.toBeNull();
  // Строка среза не прилипает к чипам: между лотком и кромкой поля есть отступ.
  expect(cut.fieldTop - cut.trayBottom!, 'отступ под чипами вне прокрутки').toBeGreaterThanOrEqual(8);
});

test('long_suggestion_is_shown_at_whole_lines_and_still_inserts_everything', async ({ page }, info) => {
  const { area } = await boot(page, { suggestion: LONG_SUGGESTION });

  const shown = await area.evaluate((node: HTMLTextAreaElement) => ({
    placeholder: node.placeholder,
    scrollHeight: node.scrollHeight,
    clientHeight: node.clientHeight
  }));
  // Ни одной строки под кромкой: placeholder не прокручивается, и седьмая строка не режется.
  expect(shown.scrollHeight, 'подсказка не прячет строк за кромкой').toBeLessThanOrEqual(
    shown.clientHeight + 1
  );
  const truncated = shown.placeholder !== LONG_SUGGESTION;
  // На телефоне подсказка заведомо не влезает: видно многоточие на целой строке, а не срез.
  if (info.project.name === 'phone') expect(truncated, 'длинная подсказка обрезана').toBe(true);
  if (truncated) {
    expect(shown.placeholder.endsWith('…'), 'конец подсказки виден многоточием').toBe(true);
    expect(LONG_SUGGESTION.startsWith(shown.placeholder.replace(/…$/, ''))).toBe(true);
    expect(shown.placeholder.length).toBeLessThan(LONG_SUGGESTION.length);
  }

  // «Подставить» вставляет полный текст, а не обрезанный placeholder.
  if (info.project.name === 'phone') {
    await page.getByRole('button', { name: 'Подставить' }).click();
  } else {
    await area.focus();
    await page.keyboard.press('Tab');
  }
  await expect(area).toHaveValue(LONG_SUGGESTION);
});

for (const width of [360, 412] as const) {
  test(`send_later_note_is_not_glued_to_the_button_${width}`, async ({ page }, info) => {
    test.skip(info.project.name !== 'phone', 'лист «Отправить позже» открывается на телефоне');
    await page.setViewportSize({ width, height: 892 });
    const { area } = await boot(page);
    await area.fill('позже');
    await area.press('Control+Shift+Enter');
    const panel = page.getByRole('dialog', { name: 'Отправить позже' });
    await expect(panel).toBeVisible();

    const measured = await panel.evaluate((node) => {
      const zone = node.querySelector('.zone') as HTMLElement | null;
      if (!zone) throw new Error('нет пояса о часовом поясе');
      const button = [...node.querySelectorAll('button')].find((item) =>
        /Запланировать/.test(item.textContent ?? '')
      );
      if (!button) throw new Error('нет кнопки «Запланировать»');
      const root = getComputedStyle(document.documentElement);
      return {
        gap: button.getBoundingClientRect().top - zone.getBoundingClientRect().bottom,
        step: Number.parseFloat(root.getPropertyValue('--s4'))
      };
    });
    expect(measured.gap, 'зазор между поясом о зоне и кнопкой').toBeGreaterThanOrEqual(
      measured.step - 0.5
    );
  });
}

test('context_details_fit_the_keyboard_sized_viewport', async ({ page }, info) => {
  test.skip(info.project.name !== 'phone', 'экранная клавиатура телефона');
  await page.setViewportSize({ width: 412, height: 450 });
  await boot(page, {
    context: { tokens: 630_000, window: 1_000_000 },
    live: true,
    liveTurn: true,
    harness: CLAUDE_PLAIN
  });
  await expect(page.locator('html')).toHaveAttribute('data-compact', '');

  const ctx = page.getByRole('button', { name: /^Контекст/ });
  await expect(ctx).toBeVisible();
  await ctx.click();
  const details = page.locator('[data-details]');
  await expect(details).toBeVisible();
  const squeeze = details.getByRole('button', { name: 'Сжать контекст' });

  // Подробности не влезают целиком — капсула прокручивается и сама долистывает их до кнопки.
  const viewport = page.viewportSize()!.height;
  await expect
    .poll(async () => {
      const box = await squeeze.boundingBox();
      return box ? box.y + box.height : Number.POSITIVE_INFINITY;
    }, { message: 'низ кнопки «Сжать контекст» в окне' })
    .toBeLessThanOrEqual(viewport + 0.5);

  const measured = await details.evaluate((node) => {
    let host: HTMLElement | null = node as HTMLElement;
    let scrolled = false;
    while (host) {
      const style = getComputedStyle(host);
      if (/(auto|scroll)/.test(style.overflowY) && host.scrollHeight > host.clientHeight + 1) {
        scrolled = host.scrollTop > 0;
        break;
      }
      host = host.parentElement;
    }
    return {
      viewport: window.innerHeight,
      detailsBottom: node.getBoundingClientRect().bottom,
      scrolled,
      composerBottom: document.querySelector('[data-composer]')!.getBoundingClientRect().bottom
    };
  });
  expect(measured.scrolled, 'подробности прокручиваются внутри капсулы до кнопки').toBe(true);
  expect(measured.composerBottom, 'поле ввода не выходит за окно').toBeLessThanOrEqual(
    measured.viewport + 0.5
  );
  expect(measured.detailsBottom, 'подробности целиком в окне').toBeLessThanOrEqual(
    measured.viewport + 0.5
  );

  // «Сжать контекст» видна целиком и правда принимает нажатие.
  const box = await squeeze.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.y, 'верх кнопки в окне').toBeGreaterThanOrEqual(-0.5);
  expect(box!.y + box!.height, 'низ кнопки в окне').toBeLessThanOrEqual(measured.viewport + 0.5);
  const tappable = await squeeze.evaluate((node) => {
    const rect = node.getBoundingClientRect();
    const found = document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2);
    return !!found && (found === node || node.contains(found));
  });
  expect(tappable, 'кнопка не перекрыта и не обрезана').toBe(true);
});

interface Cell {
  top: number;
  bottom: number;
  center: number;
}

interface ComposerCells {
  send: Cell;
  steer: Cell | null;
  mic: Cell | null;
  details: Cell | null;
  squeeze: Cell | null;
  /** Низ самой нижней ячейки сетки: ниже подробностей ничего не должно стоять. */
  lowest: number;
  capsuleBottom: number;
  viewport: number;
}

/** Ряды сетки капсулы: кнопки нижней строки, раскрытые подробности и самая нижняя ячейка. */
function cellsOf(page: Page): Promise<ComposerCells> {
  return page.evaluate(() => {
    const capsule = document.querySelector('[data-composer] .capsule') as HTMLElement;
    const cell = (element: Element | null): { top: number; bottom: number; center: number } | null => {
      if (!element) return null;
      const rect = element.getBoundingClientRect();
      return { top: rect.top, bottom: rect.bottom, center: rect.top + rect.height / 2 };
    };
    const details = capsule.querySelector('[data-details]');
    const squeeze = details
      ? [...details.querySelectorAll('button')].find((item) => /Сжать контекст/.test(item.textContent ?? '')) ?? null
      : null;
    // Абсолютные элементы (поле файла, скрытая подсказка) в раскладке сетки не участвуют.
    const inFlow = [...capsule.children].filter(
      (child) => getComputedStyle(child).position !== 'absolute'
    );
    return {
      send: cell(capsule.querySelector('.send'))!,
      steer: cell(capsule.querySelector('.steer')),
      mic: cell(capsule.querySelector('.mic')),
      details: cell(details),
      squeeze: cell(squeeze),
      lowest: Math.max(...inFlow.map((child) => child.getBoundingClientRect().bottom)),
      capsuleBottom: capsule.getBoundingClientRect().bottom,
      viewport: window.innerHeight
    };
  });
}

/** Состояния поля ввода из отчёта: «Влить в ход» и микрофон по отдельности и вместе. */
const CELL_CASES = [
  { name: 'steer и микрофон', steer: true, mic: true },
  { name: 'только «Влить в ход»', steer: true, mic: false },
  { name: 'только микрофон', steer: false, mic: true }
] as const;

const CELL_VIEWPORTS = [
  { width: 412, height: 892 },
  { width: 412, height: 450 }
] as const;

for (const viewport of CELL_VIEWPORTS) {
  for (const variant of CELL_CASES) {
    test(`${variant.name} стоят в строке отправки и с раскрытыми подробностями ${viewport.width}x${viewport.height}`, async ({ page }, info) => {
      test.skip(info.project.name !== 'phone', 'сенсорная раскладка поля ввода');
      await page.setViewportSize(viewport);
      await boot(page, {
        harness: CLAUDE,
        liveTurn: variant.steer,
        mic: variant.mic,
        queued: variant.steer ? 'Проверь очередь, пока идёт ход' : undefined,
        context: { tokens: 630_000, window: 1_000_000 },
        live: true
      });

      const steer = page.getByRole('button', { name: 'Влить в ход' });
      const mic = page.getByRole('button', { name: 'Диктовка' });
      if (variant.steer) await expect(steer).toBeVisible();
      else await expect(steer).toHaveCount(0);
      if (variant.mic) await expect(mic).toBeVisible();
      else await expect(mic).toHaveCount(0);

      // До раскрытия: кнопки в нижней строке рядом с отправкой.
      const closed = await cellsOf(page);
      expect(closed.details, 'подробности закрыты').toBeNull();
      if (variant.steer) expect(closed.steer!.center, '«Влить в ход» в строке отправки').toBeCloseTo(closed.send.center, 0);
      if (variant.mic) expect(closed.mic!.center, 'микрофон в строке отправки').toBeCloseTo(closed.send.center, 0);

      await page.getByRole('button', { name: /^Контекст/ }).click();
      await expect(page.locator('[data-details]')).toBeVisible();

      const open = await cellsOf(page);
      expect(open.details, 'подробности раскрыты').not.toBeNull();
      // С раскрытыми подробностями строка кнопок та же: ничего не уезжает в неявный ряд.
      if (variant.steer) expect(open.steer!.center, '«Влить в ход» остался в строке отправки').toBeCloseTo(open.send.center, 0);
      if (variant.mic) expect(open.mic!.center, 'микрофон остался в строке отправки').toBeCloseTo(open.send.center, 0);
      // Ниже подробностей нет ни одной ячейки сетки.
      expect(open.lowest, 'подробности — последний ряд капсулы').toBeCloseTo(open.details!.bottom, 0);
      expect(open.details!.bottom, 'подробности внутри капсулы').toBeLessThanOrEqual(open.capsuleBottom + 0.5);
      // «Сжать контекст» видна целиком.
      expect(open.squeeze, 'кнопка сжатия на месте').not.toBeNull();
      expect(open.squeeze!.top, 'верх кнопки сжатия в окне').toBeGreaterThanOrEqual(-0.5);
      expect(open.squeeze!.bottom, 'низ кнопки сжатия в окне').toBeLessThanOrEqual(open.viewport + 0.5);
    });
  }
}
