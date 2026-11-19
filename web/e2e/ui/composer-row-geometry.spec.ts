import type { Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { FakeHub, FakeHubApi, seedConversation, seedMachine, seedPresets } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { stubMicrophone } from './dictation-fixture';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-11-18T12:00:00.000Z';
const WIDTHS = [360, 390, 412, 430];
const SHOTS = process.env.ROW_SHOTS;

const CATALOG = {
  id: 'workstation',
  name: 'workstation',
  harnesses: [{
    id: 'codex', title: 'Codex', provider: 'codex',
    models: [{ id: 'mini', title: 'mini', efforts: ['low', 'medium', 'high'] }],
    permission_modes: ['owner', 'ask', 'bypass', 'read_only']
  }]
};

async function boot(page: Page, width: number, mic: boolean): Promise<{ hub: FakeHub; api: FakeHubApi; id: string }> {
  await page.setViewportSize({ width, height: 892 });
  if (mic) await stubMicrophone(page, 'granted');
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  if (mic) hub.dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
  seedMachine(api, CATALOG as never);
  seedPresets(api);
  const id = seedConversation(api, { title: 'Сессия' });
  api.pushContext(id, 500_000, 1_000_000);
  const detail = api.details.get(id)! as Record<string, unknown>;
  detail.harness = { id: 'codex', title: 'Codex', provider: 'codex', goals: true };
  detail.machine = { id: 'workstation', name: 'workstation' };
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => hub.subscribed(`conversation:${id}`)).toBe(true);
  return { hub, api, id };
}

function startTurn(hub: FakeHub, api: FakeHubApi, id: string): void {
  const scope = `conversation:${id}`;
  hub.append(scope, {
    kind: 'derived_changed',
    conversation_id: id,
    payload: {
      conversation_id: id,
      activity: { state: 'working', evidence: {}, since: new Date().toISOString() },
      unread_count: 0,
      queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
      last_activity_at: AT,
      settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
      crunch: { enabled: false }
    }
  });
  hub.append(scope, { kind: 'queue_state_changed', conversation_id: id, payload: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: api.id() } } } });
  hub.flush();
}

interface Box { name: string; left: number; right: number; top: number; bottom: number; width: number; height: number }

/** Видимые части нижней строки капсулы: кнопки и сама подпись контекста (её содержимое, а не коробка). */
async function rowBoxes(page: Page): Promise<{ boxes: Box[]; goalBg: string; siblingBg: string; ctxClipped: boolean }> {
  return page.locator('.capsule').evaluate((capsule) => {
    const pick = (name: string, node: Element | null): Box | null => {
      if (!node || node.getClientRects().length === 0) return null;
      const r = node.getBoundingClientRect();
      return { name, left: r.left, right: r.right, top: r.top, bottom: r.bottom, width: r.width, height: r.height };
    };
    const list: (Box | null)[] = [
      pick('attach', capsule.querySelector('.attach button')),
      pick('mic', capsule.querySelector('.mic button')),
      pick('goal', capsule.querySelector('.goal')),
      pick('snippets', capsule.querySelector('.tools button')),
      pick('clock', capsule.querySelector('.clock')),
      pick('steer', capsule.querySelector('.steer')),
      pick('send', capsule.querySelector('.send'))
    ];
    const ctx = capsule.querySelector('.ctx');
    const label = ctx?.querySelector('.label') ?? null;
    const track = ctx?.querySelector('.track') ?? null;
    list.push(pick('label', label), pick('track', track));
    const ctxBox = ctx?.getBoundingClientRect();
    const labelBox = label?.getBoundingClientRect();
    const goal = capsule.querySelector('.goal');
    const sibling = capsule.querySelector('.attach button');
    return {
      boxes: list.filter((b): b is Box => b !== null),
      goalBg: goal ? getComputedStyle(goal).backgroundColor : '',
      siblingBg: sibling ? getComputedStyle(sibling).backgroundColor : '',
      ctxClipped: !!ctxBox && !!labelBox && (labelBox.right > ctxBox.right + 0.5 || (label as HTMLElement).scrollWidth > (label as HTMLElement).clientWidth)
    };
  });
}

function overlap(a: Box, b: Box): boolean {
  return a.left < b.right - 0.5 && b.left < a.right - 0.5 && a.top < b.bottom - 0.5 && b.top < a.bottom - 0.5;
}

for (const width of WIDTHS) {
  for (const running of [false, true]) {
   for (const mic of [false, true]) {
    for (const typed of [false, true]) {
      test(`composer_row_no_overlap_${width}_${running ? 'running' : 'idle'}_${typed ? 'text' : 'empty'}_${mic ? 'mic' : 'nomic'}`, async ({ page }) => {
        const { hub, api, id } = await boot(page, width, mic);
        if (running) startTurn(hub, api, id);
        if (typed) await page.getByRole('textbox', { name: 'Сообщение' }).fill('Подготовить отчёт');
        await expect(page.locator('.capsule .goal')).toBeVisible();
        await expect(page.locator('.capsule .ctx')).toBeVisible();
        if (mic) await expect(page.getByRole('button', { name: 'Диктовка' })).toBeVisible();
        if (running) await expect(page.locator('.capsule .send')).toHaveAttribute('aria-label', /очеред|Поставить/i);
        // Заливка кнопки цели меняется с переходом: ждём, пока он закончится.
        await page.waitForTimeout(400);
        const { boxes, goalBg, siblingBg, ctxClipped } = await rowBoxes(page);
        if (SHOTS) {
          mkdirSync(SHOTS, { recursive: true });
          await page.locator('.capsule').screenshot({ path: `${SHOTS}/${width}-${running ? 'running' : 'idle'}-${typed ? 'text' : 'empty'}-${mic ? 'mic' : 'nomic'}.png` });
        }
        const hits: string[] = [];
        for (let i = 0; i < boxes.length; i++) {
          for (let j = i + 1; j < boxes.length; j++) {
            // Подпись и шкала лежат внутри кнопки контекста: между собой они не пересекаются, а с остальным — не должны.
            if (overlap(boxes[i], boxes[j])) hits.push(`${boxes[i].name} x ${boxes[j].name}`);
          }
        }
        expect(hits, 'нижняя строка: ничего не перекрывается').toEqual([]);
        // Окно настольного браузера уже 412 px с кнопкой «Отправить позже» и микрофоном — не телефон: там остаётся многоточие, но не наложение.
        const narrowDesktop = test.info().project.name === 'desktop' && width < 412;
        if (!narrowDesktop) expect(ctxClipped, 'подпись контекста видна целиком, без обрезки').toBe(false);
        for (const box of boxes.filter((b) => ['attach', 'mic', 'goal', 'send'].includes(b.name))) {
          expect(box.width, `${box.name} шире 44`).toBeGreaterThanOrEqual(43.5);
          expect(box.height, `${box.name} выше 44`).toBeGreaterThanOrEqual(43.5);
        }
        expect(goalBg, 'кнопка цели без заливки, как соседние').toBe(siblingBg);
      });
    }
   }
  }
}
