import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedHistory } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000e4';
const CLAUDE = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };

/** Длинный черновик: поле упирается в свой предел и прокручивается. */
const LONG_DRAFT = Array.from(
  { length: 14 },
  (_, index) => `Строка ${index + 1}: проверь и закоммить, потом прогони тесты.`
).join('\n');

/** Длинная подсказка: в поле она не влезает и без обрезки уходит под кнопки. */
const LONG_SUGGESTION =
  'добавь тесты для пустой строки, для строки из одних пробелов и для пути /home/fenicu/.local/state/canterlot-worker/worktrees/01a1e117-25c6-7237-bed5-d31506012780/web/src/lib/timeline/cards/plan/PlanUpdateCard.svelte, затем прогони cargo test и npm run check, а после этого проверь, что подсказка в поле ввода не режется посреди строки, что кнопка «Подставить» вставляет весь текст целиком и что длинный путь по-прежнему переносится, а не выдавливает поле за край.';

async function boot(page: Page, opts: { suggestion?: string } = {}) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { harness: CLAUDE });
  seedHistory(api, id, 1);
  if (opts.suggestion) api.setPromptSuggestion(id, opts.suggestion);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await expect(area).toBeVisible();
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
