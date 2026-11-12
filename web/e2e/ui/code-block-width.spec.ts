import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';
const SHOTS = process.env.CODEBLOCK_SHOTS;

const budget = 'OpenCode API budget: five-hour window 0% used, resets in 1h 36m; weekly window 77% used, resets in 8h 53m; at the average pace (0.5%/h) lasts until the reset; monthly window 48% used, resets in 20d 4h; at the average pace (2.1%/h) runs out in 6d 2h before the reset.';
const url = `https://github.com/Fenicu/canterlot/releases/download/v1.22.2/${'canterlot-web-bundle-'.repeat(8)}linux-amd64.tar.gz`;
const inline = `\`${'web/src/lib/timeline/AnswerCard.svelte::render::'.repeat(5)}\``;

const answer = [
  'Ответ инструмента сейчас:',
  '',
  '```',
  'Claude API budget: weekly window 66% used, resets in 1d 9h; at the average pace (0.5%/h) lasts until the reset.',
  'Codex API budget: five-hour window 0% used, resets in 4h 58m; weekly window 0% used, resets in 6d 23h.',
  budget,
  'Observed: claude 2 min ago, codex 1 min ago, opencode 1 min ago.',
  '```',
  '',
  `Ссылка на сборку: ${url}`,
  '',
  `Путь: ${inline}`,
  '',
  '| Провайдер | Окно | Строка |',
  '| --- | --- | --- |',
  `| OpenCode | мес | ${budget.replace(/\|/g, '/')} |`,
  '',
  '1. В пункте списка:',
  '   ```',
  `   ${budget}`,
  '   ```',
  '',
  '> В цитате:',
  '>',
  '> ```',
  `> ${budget}`,
  '> ```'
].join('\n');

function block(api: FakeHubApi, content: string, kind: 'text' | 'tool_call', seq: string): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text: content, rev: 1, state: 'final' });
  return {
    kind: 'block', id, block_kind: kind, content, content_bytes: content.length, content_truncated: false,
    created_seq: seq, created_at: AT, rev: 1, state: 'final', agent: 'агент', model: 'opus',
    tool_name: kind === 'tool_call' ? 'Bash' : null, summary: kind === 'tool_call' ? 'scripts/limits.sh' : null,
    parent_block_id: null, input_command_id: null, line_count: content.split('\n').length
  };
}

async function boot(page: import('@playwright/test').Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  api.histories.set(id, [block(api, `${budget}\n${budget}`, 'tool_call', '1'), block(api, answer, 'text', '2')]);
  api.bindConversation(id);
  await page.reload();
  const card = page.locator('article.card').filter({ has: page.locator('.prose') }).last();
  await expect(card.locator('pre').first()).toBeVisible({ timeout: 15_000 });
  return card;
}

type Box = { left: number; right: number; width: number };

const sizes = [
  { name: 'desktop-1280', project: 'desktop', width: 1280, height: 800 },
  { name: 'desktop-2100', project: 'desktop', width: 2100, height: 1300 },
  { name: 'phone-390', project: 'phone', width: 390, height: 844 }
];

for (const size of sizes) {
  test(`wide_markdown_stays_inside_the_answer_card_${size.name}`, async ({ page }, info) => {
    test.skip(info.project.name !== size.project, 'размер для другого проекта');
    await page.setViewportSize({ width: size.width, height: size.height });
    const card = await boot(page);
    if ((await page.locator('html').getAttribute('data-theme')) === 'light') await page.locator('.chat-header [data-theme-toggle]').click();
    await expect(page.locator('html')).not.toHaveAttribute('data-theme', 'light');
    const prose = card.locator('.prose').first();
    const cardWidth = await card.evaluate((node) => node.getBoundingClientRect().width);

    if (SHOTS) {
      await card.locator('pre').first().scrollIntoViewIfNeeded();
      await page.screenshot({ path: `${SHOTS}-${size.name}.png` });
    }

    const measured = await card.evaluate((node) => {
      const box = (el: Element): Box => { const r = el.getBoundingClientRect(); return { left: r.left, right: r.right, width: r.width }; };
      const text = node.querySelector('.prose')!;
      const wide = [...text.querySelectorAll('pre, table, .code-block, p, :not(pre) > code, a')].map((el) => ({ tag: el.tagName, cls: el.className, ...box(el) }));
      const pres = [...text.querySelectorAll('pre')].map((el) => ({ scroll: el.scrollWidth, client: el.clientWidth, ...box(el) }));
      return { card: box(node), prose: box(text), wide, pres, overflow: node.scrollWidth - node.clientWidth };
    });

    info.attach('measured', { body: JSON.stringify(measured, null, 1), contentType: 'application/json' });
    // Карточка не шире ленты и не раздута содержимым.
    expect(measured.overflow).toBeLessThanOrEqual(0);
    expect(measured.card.right).toBeLessThanOrEqual(size.width);
    for (const el of measured.wide) {
      expect(el.right, `${el.tag}.${el.cls} справа`).toBeLessThanOrEqual(measured.prose.right + 1);
      expect(el.left, `${el.tag}.${el.cls} слева`).toBeGreaterThanOrEqual(measured.prose.left - 1);
    }
    expect(measured.pres).toHaveLength(3);
    for (const pre of measured.pres) {
      expect(pre.right).toBeLessThanOrEqual(measured.card.right);
      expect(pre.scroll).toBeGreaterThan(pre.client);
    }

    // Код листается вбок внутри себя, карточка при этом не меняется.
    const pre = card.locator('pre').first();
    await pre.evaluate((node) => { node.scrollLeft = node.scrollWidth; });
    expect(await pre.evaluate((node) => node.scrollLeft)).toBeGreaterThan(0);
    expect(await card.evaluate((node) => node.getBoundingClientRect().width)).toBe(cardWidth);
    expect(await prose.evaluate((node) => node.scrollWidth - node.clientWidth)).toBeLessThanOrEqual(0);

    // Вывод инструмента тоже не выходит за свою карточку.
    const tool = page.locator('pre').filter({ hasText: 'OpenCode API budget' }).first();
    if (await tool.count()) {
      const toolBox = await tool.boundingBox();
      expect(toolBox!.x + toolBox!.width).toBeLessThanOrEqual(size.width);
    }
  });
}
