import { mkdirSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000e4';
const AT = '2026-11-16T09:00:00.000Z';
const SHOTS = process.env.PROSE_SHOTS;

/** Цитата в две строки на всех трёх ширинах после запаса под кнопку: около 60 знаков. */
const TWO_LINE = 'Строка цитаты, которая переносится на телефоне под кнопкой.';
/** Цитата в три строки: около 95 знаков. */
const THREE_LINE = 'Первая строка цитаты с длинным текстом, вторая продолжает ту же мысль, а третья её заканчивает.';

const ONE_LINE_CODE = "const palette = 'aquamarine';";
const TWO_LINE_CODE = "const palette = 'aquamarine';\nsetPalette(palette);";

const ANSWER = [
  'Короткая цитата:',
  '',
  '> Короткая цитата в одну строку.',
  '',
  'Цитата в две строки:',
  '',
  `> ${TWO_LINE}`,
  '',
  'Цитата в три строки:',
  '',
  `> ${THREE_LINE}`,
  '',
  'Одна строка кода:',
  '',
  '```ts',
  ONE_LINE_CODE,
  '```',
  '',
  'Абзац после однострочного блока.',
  '',
  'Блок в две строки:',
  '',
  '```ts',
  TWO_LINE_CODE,
  '```',
  '',
  'Абзац после блока в две строки.'
].join('\n');

function block(api: FakeHubApi, content: string): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text: content, rev: 1, state: 'final' });
  return {
    kind: 'block',
    id,
    block_kind: 'text',
    content,
    content_bytes: content.length,
    content_truncated: false,
    created_seq: '2',
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'агент',
    model: 'opus',
    tool_name: null,
    summary: null,
    parent_block_id: null,
    input_command_id: null,
    line_count: content.split('\n').length
  };
}

function input(api: FakeHubApi): HistoryInput {
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: 'prose-copy',
    state: 'accepted',
    content: [{ type: 'text', text: 'Покажи цитаты и блоки кода.' }],
    attachments: [],
    created_seq: '1',
    feed_seq: '1',
    created_at: AT
  };
}

async function boot(page: Page, theme: 'dark' | 'light' = 'dark'): Promise<void> {
  await page.addInitScript((value) => {
    localStorage.setItem('canterlot.theme', value);
    localStorage.setItem('canterlot.motion', 'off');
  }, theme);
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Цитаты и копирование' });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  api.histories.set(id, [input(api), block(api, ANSWER)]);
  api.bindConversation(id);
  await page.reload();
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.prose blockquote > .copy-button')).toHaveCount(3);
  await expect(page.locator('.prose .code-block > .copy-button')).toHaveCount(2);
  await page.evaluate(() => document.fonts.ready);
  await page.waitForTimeout(200);
}

interface Box {
  left: number;
  top: number;
  right: number;
  bottom: number;
  width: number;
  height: number;
}

interface QuoteShape {
  card: Box;
  quote: Box;
  button: Box;
  icon: Box;
  /** Рамка текста цитаты: по ней меряется верхнее поле. */
  text: Box;
  /** Прямоугольники строк текста, без кнопки и её подписи. */
  lines: Box[];
}

interface CodeShape {
  card: Box;
  block: Box;
  pre: Box;
  button: Box;
  icon: Box;
  code: Box;
  /** Верх следующего абзаца: им меряется зазор под блоком. */
  nextTop: number | null;
}

function overlaps(a: Box, b: Box): boolean {
  // Допуск гасит субпиксельную границу: касание краями не считается наложением.
  return a.left < b.right - 0.5 && b.left < a.right - 0.5 && a.top < b.bottom - 0.5 && b.top < a.bottom - 0.5;
}

/**
 * Кнопка против строк текста: прямоугольник диапазона строки — это то, что владелец видит,
 * а не рамка абзаца с полями. Кнопка и её подпись из строк исключены.
 */
async function quoteShapes(page: Page): Promise<QuoteShape[]> {
  return page.locator('.prose blockquote').evaluateAll((quotes) => {
    const box = (rect: DOMRect) => ({
      left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom, width: rect.width, height: rect.height
    });
    return quotes.map((quote) => {
      const button = quote.querySelector(':scope > .copy-button') as HTMLElement;
      const icon = button.querySelector('svg') as SVGElement;
      const lines: ReturnType<typeof box>[] = [];
      const walker = document.createTreeWalker(quote, NodeFilter.SHOW_TEXT);
      for (let node = walker.nextNode(); node; node = walker.nextNode()) {
        if (button.contains(node) || !node.textContent?.trim()) continue;
        const range = document.createRange();
        range.selectNodeContents(node);
        for (const rect of range.getClientRects()) {
          if (rect.width < 0.5 || rect.height < 0.5) continue;
          lines.push(box(rect));
        }
      }
      const card = quote.closest('article.card') as HTMLElement;
      const text = [...quote.children].find((child) => !child.classList.contains('copy-button')) as HTMLElement;
      return {
        card: box(card.getBoundingClientRect()),
        quote: box(quote.getBoundingClientRect()),
        button: box(button.getBoundingClientRect()),
        icon: box(icon.getBoundingClientRect()),
        text: box(text.getBoundingClientRect()),
        lines
      };
    });
  });
}

async function codeShapes(page: Page): Promise<CodeShape[]> {
  return page.locator('.prose .code-block').evaluateAll((blocks) => {
    const box = (rect: DOMRect) => ({
      left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom, width: rect.width, height: rect.height
    });
    return blocks.map((frame) => {
      const pre = frame.querySelector('pre') as HTMLElement;
      const button = frame.querySelector(':scope > .copy-button') as HTMLElement;
      const icon = button.querySelector('svg') as SVGElement;
      const code = pre.querySelector('code') as HTMLElement;
      const next = frame.nextElementSibling as HTMLElement | null;
      const card = frame.closest('article.card') as HTMLElement;
      return {
        card: box(card.getBoundingClientRect()),
        block: box(frame.getBoundingClientRect()),
        pre: box(pre.getBoundingClientRect()),
        button: box(button.getBoundingClientRect()),
        icon: box(icon.getBoundingClientRect()),
        code: box(code.getBoundingClientRect()),
        nextTop: next ? next.getBoundingClientRect().top : null
      };
    });
  });
}

function iconCentreGap(shape: { button: Box; icon: Box }): number {
  return Math.abs((shape.icon.left + shape.icon.right) / 2 - (shape.button.left + shape.button.right) / 2);
}

const WIDTHS = [360, 390, 412];

for (const width of WIDTHS) {
  test(`blockquote_copy_button_does_not_cover_the_quote_text_${width}`, async ({ page }, info) => {
    test.skip(info.project.name !== 'phone', 'кнопка видна всегда только на сенсорном экране');
    await page.setViewportSize({ width, height: 892 });
    await boot(page);
    const quotes = await quoteShapes(page);

    expect(quotes, 'три цитаты с кнопками').toHaveLength(3);
    const [single, two, three] = quotes as [QuoteShape, QuoteShape, QuoteShape];
    expect(single.lines.length, 'первая цитата занимает одну строку').toBe(1);
    expect(two.lines.length, 'вторая цитата занимает не меньше двух строк').toBeGreaterThanOrEqual(2);
    expect(three.lines.length, 'третья цитата занимает не меньше трёх строк').toBeGreaterThanOrEqual(3);

    for (const [name, shape] of [['в одну строку', single], ['в две строки', two], ['в три строки', three]] as const) {
      expect(shape.button.width, `ширина кнопки (${name})`).toBeGreaterThanOrEqual(44 - 0.5);
      expect(shape.button.height, `высота кнопки (${name})`).toBeGreaterThanOrEqual(44 - 0.5);
      expect(iconCentreGap(shape), `значок по центру кнопки (${name})`).toBeLessThanOrEqual(1);
      // Кнопка не выходит за нижний край цитаты: цель касания целиком внутри блока.
      expect(shape.button.bottom, `низ кнопки внутри цитаты (${name})`).toBeLessThanOrEqual(shape.quote.bottom + 0.5);
      expect(shape.quote.right, `цитата внутри карточки (${name})`).toBeLessThanOrEqual(shape.card.right + 0.5);
      // Текст остаётся на своём месте: запас под кнопку не добавляет цитате вертикальных полей.
      expect(shape.text.top - shape.quote.top, `верхнее поле цитаты (${name})`).toBeCloseTo(8, 0);
      const hit = shape.lines.filter((line) => overlaps(shape.button, line));
      expect(hit, `кнопка накрывает строки цитаты (${name})`).toEqual([]);
      // Строка целиком слева от кнопки: она не подходит под неё и не выглядывает справа.
      for (const line of shape.lines) {
        expect(line.right, `строка не заходит под кнопку (${name})`).toBeLessThanOrEqual(shape.button.left + 0.5);
      }
    }
  });

  test(`one_line_code_block_contains_the_copy_button_${width}`, async ({ page }, info) => {
    test.skip(info.project.name !== 'phone', 'кнопка видна всегда только на сенсорном экране');
    await page.setViewportSize({ width, height: 892 });
    await boot(page);
    const blocks = await codeShapes(page);

    expect(blocks, 'два блока кода с кнопками').toHaveLength(2);
    const [single, multi] = blocks as [CodeShape, CodeShape];
    expect(single.code.height, 'первый блок в одну строку').toBeLessThan(30);
    expect(multi.code.height, 'второй блок в две строки').toBeGreaterThan(30);

    for (const [name, shape] of [['однострочный', single], ['двухстрочный', multi]] as const) {
      expect(shape.button.width, `ширина кнопки (${name})`).toBeGreaterThanOrEqual(44 - 0.5);
      expect(shape.button.height, `высота кнопки (${name})`).toBeGreaterThanOrEqual(44 - 0.5);
      expect(iconCentreGap(shape), `значок по центру кнопки (${name})`).toBeLessThanOrEqual(1);
      // Кнопка не свисает ниже блока и не отнимает зазор у следующего абзаца.
      expect(shape.button.bottom, `низ кнопки внутри блока (${name})`).toBeLessThanOrEqual(shape.block.bottom + 0.5);
      expect(shape.button.bottom, `низ кнопки внутри поля блока (${name})`).toBeLessThanOrEqual(shape.pre.bottom + 0.5);
      // Строка кода не уезжает под кнопку.
      expect(shape.code.right, `строка кода кончается до кнопки (${name})`).toBeLessThanOrEqual(shape.button.left + 0.5);
      expect(shape.block.right, `блок внутри карточки (${name})`).toBeLessThanOrEqual(shape.card.right + 0.5);
    }

    // Зазор до следующего абзаца одинаков у однострочного и у двухстрочного блока.
    const singleGap = single.nextTop! - single.block.bottom;
    const multiGap = multi.nextTop! - multi.block.bottom;
    expect(singleGap, 'зазор под однострочным блоком как под двухстрочным').toBeCloseTo(multiGap, 0);
    expect(singleGap, 'зазор под блоком по вёрстке').toBeGreaterThan(6);
  });
}

test('touch_reservations_leave_the_hover_layout_alone', async ({ page }, info) => {
  test.skip(info.project.name !== 'desktop', 'проверка наведения живёт на рабочем столе');
  await page.setViewportSize({ width: 1440, height: 900 });
  await boot(page);
  const measured = await page.evaluate(() => {
    const quote = document.querySelector('.prose blockquote') as HTMLElement;
    const pre = document.querySelector('.prose .code-block > pre') as HTMLElement;
    const style = getComputedStyle(quote);
    return {
      hoverNone: matchMedia('(hover: none)').matches,
      paddingRight: Number.parseFloat(style.paddingRight),
      paddingTop: Number.parseFloat(style.paddingTop),
      minHeight: Number.parseFloat(style.minHeight) || 0,
      preHeight: pre.getBoundingClientRect().height,
      prePaddingTop: Number.parseFloat(getComputedStyle(pre).paddingTop)
    };
  });
  expect(measured.hoverNone, 'на рабочем столе есть наведение').toBe(false);
  // Поля цитаты и высота блока кода остаются прежними: запас под кнопку только на касании.
  expect(measured.paddingRight, 'правое поле цитаты').toBe(12);
  expect(measured.paddingTop, 'верхнее поле цитаты').toBe(8);
  expect(measured.minHeight, 'цитата не держит высоту кнопки').toBe(0);
  expect(measured.preHeight, 'однострочный блок не вырос').toBeCloseTo(39.2, 0);
  expect(measured.prePaddingTop, 'верхнее поле блока кода').toBe(10);
});

/** Снимки «до» и «после» для отчёта: снимаются отдельным прогоном с PROSE_SHOTS. */
test('prose_copy_button_shots', async ({ page }, info) => {
  test.skip(!SHOTS, 'снимки для отчёта снимаются отдельным прогоном');
  test.skip(info.project.name !== 'phone', 'снимки для телефона');
  const dir = SHOTS as string;
  mkdirSync(dir, { recursive: true });
  const clipAround = async (selector: string, name: string, margin = 16) => {
    const target = page.locator(selector).first();
    await target.scrollIntoViewIfNeeded();
    await page.waitForTimeout(150);
    const box = await target.boundingBox();
    if (!box) throw new Error(`нет элемента ${selector}`);
    const size = page.viewportSize()!;
    await page.screenshot({
      path: `${dir}/${name}.png`,
      animations: 'disabled',
      clip: {
        x: Math.max(0, box.x - margin),
        y: Math.max(0, box.y - margin),
        width: Math.min(size.width, box.width + margin * 2),
        height: Math.min(size.height - Math.max(0, box.y - margin), box.height + margin * 2)
      }
    });
  };

  await page.setViewportSize({ width: 412, height: 892 });
  await boot(page, 'dark');
  await clipAround('.prose blockquote >> nth=1', 'two-line-blockquote-412');
  await clipAround('.prose blockquote >> nth=2', 'three-line-blockquote-412');
  await clipAround('.prose .code-block >> nth=0', 'one-line-code-block-412');
  await clipAround('.prose .code-block >> nth=1', 'multi-line-code-block-412-dark');

  await page.setViewportSize({ width: 360, height: 740 });
  await page.reload();
  await expect(page.locator('.prose blockquote > .copy-button')).toHaveCount(3);
  await page.evaluate(() => document.fonts.ready);
  await page.waitForTimeout(200);
  await clipAround('.prose blockquote >> nth=1', 'two-line-blockquote-360');

  await page.setViewportSize({ width: 412, height: 892 });
  await boot(page, 'light');
  await clipAround('.prose .code-block >> nth=1', 'multi-line-code-block-412-light');
});
