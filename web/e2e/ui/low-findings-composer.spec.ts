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

async function boot(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { harness: CLAUDE });
  seedHistory(api, id, 1);
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
