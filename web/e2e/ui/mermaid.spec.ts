import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d6';
const AT = '2026-11-02T12:00:00.000Z';
const DIAGRAM = ['flowchart LR', '  A[Begin] --> B[Check] --> C[Finish]'].join('\n');
const BROKEN = 'flowchart LR\n  A[Begin] --> --> C[Finish]';

function block(api: FakeHubApi, content: string, seq: string): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text: content, rev: 1, state: 'final' });
  return {
    kind: 'block', id, block_kind: 'text', content,
    content_bytes: content.length, content_truncated: false,
    created_seq: seq, created_at: AT, rev: 1, state: 'final',
    agent: 'агент', model: 'opus', tool_name: null, summary: null,
    parent_block_id: null, input_command_id: null, line_count: content.split('\n').length
  };
}

async function boot(page: import('@playwright/test').Page, content: string) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  api.histories.set(id, [block(api, content, '1')]);
  api.bindConversation(id);
  await page.reload();
  return page.locator('.mermaid-block').first();
}

for (const project of ['phone', 'desktop']) {
  test(`mermaid_renders_svg_and_toggles_${project}`, async ({ page }, info) => {
    test.skip(info.project.name !== project, 'другой проект');
    // Mermaid размечает свой временный DOM инлайн-стилями; проверяем вставленный SVG отдельно.
    await page.addInitScript(() => { (window as unknown as { __mermaidCspAllowed?: boolean }).__mermaidCspAllowed = true; });
    const wrap = await boot(page, `Вот схема:\n\n\`\`\`mermaid\n${DIAGRAM}\n\`\`\`\n\nГотово.`);
    await expect(wrap.locator('.mermaid-view svg')).toBeVisible({ timeout: 20_000 });
    await expect(wrap.locator('.mermaid-view')).toHaveAttribute('role', 'img');
    const inserted = await wrap.locator('.mermaid-view').evaluate((node) => ({
      styleTags: node.querySelectorAll('style').length,
      styleAttrs: node.querySelectorAll('[style]').length
    }));
    expect(inserted.styleTags).toBe(0);
    expect(inserted.styleAttrs).toBe(0);

    // Схема остаётся естественного размера: подписи не крупнее текста ленты,
    // а на десктопе маленькая схема не растягивается на всю колонку.
    const metrics = await wrap.locator('.mermaid-view svg').evaluate((svg) => {
      const prose = svg.closest('.prose') as HTMLElement;
      const label = svg.querySelector('text');
      const body = prose.querySelector('p');
      return {
        width: svg.getBoundingClientRect().width,
        column: prose.getBoundingClientRect().width,
        label: label ? Number.parseFloat(getComputedStyle(label).fontSize) : 0,
        body: body ? Number.parseFloat(getComputedStyle(body).fontSize) : 0
      };
    });
    if (info.project.name === 'desktop') {
      expect(metrics.width, `SVG ${metrics.width} vs колонка ${metrics.column}`).toBeLessThan(metrics.column);
    }
    expect(metrics.label, 'размер подписи узла').toBeGreaterThan(0);
    expect(metrics.body, 'размер текста ленты').toBeGreaterThan(0);
    expect(metrics.label, `подпись ${metrics.label}px vs текст ${metrics.body}px`).toBeLessThanOrEqual(metrics.body * 1.3);

    const before = await page.locator('article.card').last().evaluate((node) => ({
      card: node.scrollWidth - node.clientWidth,
      prose: (node.querySelector('.prose') as HTMLElement).scrollWidth - (node.querySelector('.prose') as HTMLElement).clientWidth
    }));
    expect(before.card).toBeLessThanOrEqual(0);
    expect(before.prose).toBeLessThanOrEqual(0);

    const codeButton = wrap.getByRole('button', { name: 'Код', exact: true });
    const diagramButton = wrap.getByRole('button', { name: 'Диаграмма', exact: true });
    await codeButton.click();
    await expect(wrap).toHaveAttribute('data-phase', 'code');
    await expect(wrap.locator('.mermaid-view svg')).toBeHidden();
    await expect(page.locator('.prose .code-block').first()).toBeVisible();
    await diagramButton.click();
    await expect(wrap).toHaveAttribute('data-phase', 'diagram');
    await expect(wrap.locator('.mermaid-view svg')).toBeVisible();
  });

  test(`mermaid_syntax_error_shows_source_${project}`, async ({ page }, info) => {
    test.skip(info.project.name !== project, 'другой проект');
    await page.addInitScript(() => { (window as unknown as { __mermaidCspAllowed?: boolean }).__mermaidCspAllowed = true; });
    const wrap = await boot(page, `\`\`\`mermaid\n${BROKEN}\n\`\`\``);
    await expect(wrap.locator('.mermaid-error')).toHaveText('Не удалось построить диаграмму', { timeout: 20_000 });
    await expect(wrap).toHaveAttribute('data-phase', 'error');
  });
}
