import type { Page } from '@playwright/test';
import type { HistoryBlock, HistoryInput, HistoryItem } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

// Повторное открытие тёплого разговора из списка должно стоять у низа, когда все строки домерились.
// Ломалось на третьем открытии: пока лента прижата, окно строк считалось от поджатого браузером
// scrollTop и качалось между двумя наборами строк, а смена распорок после прокрутки к низу
// ничем не наблюдалась. Подгонка scrollTop браузером читалась как прокрутка владельца вверх.
const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';
const TAIL = 'Конец последнего ответа';

function block(api: FakeHubApi, seq: number, text: string, kind: 'text' | 'tool_call' = 'text'): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text, rev: 1, state: 'final' });
  return {
    kind: 'block', id, block_kind: kind, content: text, content_bytes: text.length, content_truncated: false,
    created_seq: String(seq), created_at: AT, rev: 1, state: 'final', agent: 'agent', model: 'opus',
    tool_name: kind === 'tool_call' ? 'Bash' : null, summary: kind === 'tool_call' ? 'npm test' : null,
    parent_block_id: null, input_command_id: null, line_count: text.split('\n').length
  };
}

function message(api: FakeHubApi, seq: number, text: string, image = false): HistoryInput {
  return {
    kind: 'input', id: api.id(), idempotency_key: `m-${seq}-${api.id()}`, state: 'accepted',
    content: [{ type: 'text', text }],
    attachments: image ? [{ attachment_id: api.id(), name: 'Screenshot.png', mime: 'image/png', size: 474_594, sha256: '', image: true }] : [],
    created_seq: String(seq), feed_seq: String(seq), created_at: AT
  } as HistoryInput;
}

function markdown(n: number): string {
  const parts = [`## Итог шага ${n}`, ''];
  for (let i = 0; i < 6; i++) parts.push(`- пункт ${i}: ${'длинное пояснение с подробностями '.repeat(3)}`);
  parts.push('', '| файл | строки |', '| --- | --- |', '| a.ts | 12 |', '| b.ts | 40 |', '');
  parts.push('```ts', ...Array.from({ length: 18 }, (_, i) => `export const value${i} = compute(${i}, "${'x'.repeat(i * 3)}");`), '```', '');
  parts.push('Абзац текста '.repeat(30));
  return parts.join('\n');
}

type Mix = 'image-long-markdown' | 'code-only' | 'text-only' | 'tools-and-markdown';

function seed(api: FakeHubApi, id: string, mix: Mix): void {
  const items: HistoryItem[] = [];
  let seq = 1;
  for (let turn = 0; turn < 12; turn++) {
    items.push(message(api, seq++, `Вопрос ${turn}`));
    items.push(block(api, seq++, mix === 'text-only' ? 'Короткий ответ. '.repeat(20) : markdown(turn)));
  }
  items.push(message(api, seq++, 'Посмотри на скриншот', mix === 'image-long-markdown' || mix === 'tools-and-markdown'));
  for (let i = 0; i < 3; i++) {
    if (mix === 'tools-and-markdown') items.push(block(api, seq++, '$ npm test\n' + 'ok\n'.repeat(30), 'tool_call'));
    const body = mix === 'code-only' ? '```\n' + Array.from({ length: 60 }, (_, l) => `line ${l}`).join('\n') + '\n```' : mix === 'text-only' ? 'Ответ. '.repeat(120) : markdown(100 + i);
    items.push(block(api, seq++, i === 2 ? `${body}\n\n${TAIL}` : body));
  }
  api.histories.set(id, items);
  api.bindConversation(id);
}

/** Зазор до низа после того, как строки домерились: несколько кадров подряд без изменений. */
async function settledGap(page: Page): Promise<number> {
  return page.locator('.feed').evaluate(async (el) => {
    const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));
    let last = -1;
    let still = 0;
    for (let i = 0; i < 240 && still < 20; i++) {
      await frame();
      const gap = Math.round(el.scrollHeight - el.scrollTop - el.clientHeight);
      still = gap === last ? still + 1 : 0;
      last = gap;
    }
    return last;
  });
}

// QUARANTINE: в полном гейте 1.30.1 (нагрузка от гейта и двух исполнителей) чат на десктопе не стал
// готов за 15 с при повторном открытии; отдельно 9 из 9 на телефоне и десктопе, три прогона подряд.
test.describe('quarantine reopening_from_the_list_lands_at_the_bottom', () => {
  test.describe.configure({ retries: 2 });
  for (const mix of ['image-long-markdown', 'code-only', 'text-only', 'tools-and-markdown'] as const) {
    test(`reopening_from_the_list_lands_at_the_bottom_${mix}`, async ({ page }) => {
      const hub = new FakeHub();
      const api = new FakeHubApi(hub);
      const id = seedConversation(api, { title: 'Сессия Альфа' });
      seed(api, id, mix);
      (api.details.get(id) as Record<string, unknown>).execution = { state: 'running', generation: 1 };
      api.pushContext(id, 900_000, 1_000_000);
      await installChatRoutes(page, hub, api, OWNER);
      await page.goto('/');
      const link = page.getByRole('link', { name: /Сессия Альфа/ }).first();
      for (let visit = 0; visit < 3; visit++) {
        await link.click();
        await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
        if (visit === 1) {
          // Сжатие идёт, пока владелец в списке и снова в чате.
          api.pushCompaction(id);
          hub.flush();
        }
        const gap = await settledGap(page);
        expect(gap, `visit ${visit}`).toBeLessThanOrEqual(48);
        await expect(page.getByText(TAIL)).toBeInViewport();
        await page.goBack();
        await link.waitFor();
      }
    });
  }
});

test('scrolling_up_after_reopening_still_releases_the_bottom', async ({ page }, info) => {
  test.skip(info.project.name === 'phone', 'колесо мыши');
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Сессия Альфа' });
  seed(api, id, 'image-long-markdown');
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/');
  const link = page.getByRole('link', { name: /Сессия Альфа/ }).first();
  await link.click();
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await page.goBack();
  await link.waitFor();
  await link.click();
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  expect(await settledGap(page)).toBeLessThanOrEqual(48);
  const feed = page.locator('.feed');
  await expect(feed).toHaveAttribute('data-pinned', '');
  await feed.hover();
  await page.mouse.wheel(0, -1500);
  await expect(feed).not.toHaveAttribute('data-pinned', '');
  // Окно стоит там, куда его увёл владелец, и не возвращается к низу само.
  expect(await settledGap(page)).toBeGreaterThan(48 * 4);
});
