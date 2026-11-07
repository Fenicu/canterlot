import type { CDPSession, Page } from '@playwright/test';
import { PNG } from 'pngjs';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import { expect } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000e1';
const AT = '2026-11-02T12:00:00.000Z';

const cache = new Map<string, Buffer>();

/**
 * Снимок «рабочего стола»: шапка, боковая панель, карточки и мелкая сетка, чтобы приближение было видно
 * и на записи, и на снимке. Цвета задаёт оттенок, чтобы соседние картинки различались.
 */
export function screenshotPng(width: number, height: number, hue: number): Buffer {
  const key = `${width}x${height}:${hue}`;
  const known = cache.get(key);
  if (known) return known;
  const png = new PNG({ width, height });
  const tint = (shade: number): [number, number, number] => {
    const angle = (hue / 360) * Math.PI * 2;
    return [shade + 40 * Math.cos(angle), shade + 40 * Math.cos(angle - 2.1), shade + 40 * Math.cos(angle + 2.1)].map((v) => Math.max(0, Math.min(255, Math.round(v)))) as [number, number, number];
  };
  const bar = Math.round(height * 0.07);
  const side = Math.round(width * 0.18);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      let colour: [number, number, number];
      if (y < bar) colour = tint(60);
      else if (x < side) colour = tint(90);
      else {
        const cardX = Math.floor((x - side) / ((width - side) / 3));
        const cardY = Math.floor((y - bar) / ((height - bar) / 3));
        const insideX = (x - side) % ((width - side) / 3);
        const insideY = (y - bar) % ((height - bar) / 3);
        const card = insideX > 16 && insideY > 16;
        colour = card ? tint(170 + ((cardX + cardY) % 3) * 25) : tint(225);
        // Строки «текста» в карточках: мелкие детали, видные только в приближении.
        if (card && insideY % 22 < 4 && insideX > 30 && insideX < ((width - side) / 3) * 0.8) colour = tint(80);
      }
      if (x % 120 === 0 || y % 120 === 0) colour = [colour[0] * 0.85, colour[1] * 0.85, colour[2] * 0.85].map(Math.round) as [number, number, number];
      const at = (y * width + x) * 4;
      png.data[at] = colour[0];
      png.data[at + 1] = colour[1];
      png.data[at + 2] = colour[2];
      png.data[at + 3] = 255;
    }
  }
  const buffer = PNG.sync.write(png);
  cache.set(key, buffer);
  return buffer;
}

export interface Picture { id: string; width: number; height: number; hue: number }

export const PICTURES: Picture[] = [
  { id: 'shot-a', width: 1440, height: 900, hue: 210 },
  { id: 'shot-b', width: 1600, height: 1000, hue: 20 },
  { id: 'shot-c', width: 900, height: 1400, hue: 130 }
];

function block(api: FakeHubApi, patch: Partial<HistoryBlock>): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text: '', rev: 1, state: 'final' });
  return {
    kind: 'block',
    id,
    block_kind: 'tool_call',
    content: '',
    content_bytes: 0,
    content_truncated: false,
    created_seq: '1',
    created_at: AT,
    rev: 1,
    state: 'final',
    agent: 'агент',
    model: 'opus',
    tool_name: 'Screenshot',
    summary: 'экран',
    parent_block_id: null,
    input_command_id: null,
    line_count: 0,
    ...patch
  };
}

export type Source = 'owner' | 'answer' | 'shared';

/** Чат с тремя картинками из выбранного источника. Картинки отдаются настоящими PNG своего размера. */
export async function openChat(page: Page, source: Source, pictures: Picture[] = PICTURES) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  if (source === 'owner') {
    const input: HistoryInput = {
      kind: 'input', id: api.id(), idempotency_key: 'shots', state: 'accepted',
      content: [{ type: 'text', text: 'Посмотри экраны' }],
      attachments: pictures.map((picture, i) => ({ attachment_id: picture.id, name: `screen-${i + 1}.png`, mime: 'image/png', size: 200_000, sha256: '', image: true })),
      created_seq: '1', feed_seq: '1', created_at: AT
    };
    api.histories.set(id, [input]);
  } else if (source === 'answer') {
    const item = block(api, {}) as HistoryBlock & { media: unknown[] };
    item.media = pictures.map((picture) => ({ media_id: picture.id, mime: 'image/png', width: picture.width, height: picture.height, ready: true }));
    api.histories.set(id, [item]);
  } else {
    const item = block(api, { tool_name: 'share_file', summary: 'Снимки' });
    item.files = pictures.map((picture, i) => ({ attachment_id: picture.id, name: `desktop-${i + 1}.png`, mime: 'image/png', size: 300_000, title: `Рабочий стол ${i + 1}`, note: '', width: picture.width, height: picture.height }));
    api.histories.set(id, [item]);
  }
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  const serve = async (route: import('@playwright/test').Route, picture: Picture | undefined) => {
    if (!picture) return route.fallback();
    await route.fulfill({ contentType: 'image/png', body: screenshotPng(picture.width, picture.height, picture.hue) });
  };
  await page.route('**/api/v1/media/*', (route) => serve(route, pictures.find((picture) => route.request().url().endsWith(`/media/${picture.id}`))));
  await page.route('**/api/v1/attachments/*/content', (route) => serve(route, pictures.find((picture) => route.request().url().includes(`/attachments/${picture.id}/`))));
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

/** Кнопка миниатюры картинки `n` (с нуля) в выбранном источнике. */
export function thumb(page: Page, source: Source, n: number) {
  if (source === 'owner') return page.locator('.bubble button.thumb').nth(n);
  if (source === 'shared') return page.locator('.files button.thumb').nth(n);
  return page.locator('.shot button').nth(n);
}

export function viewer(page: Page) {
  return page.locator('dialog.viewer');
}

/** Ждёт, пока в просмотрщике не останется идущих переходов и анимаций. */
export async function settled(page: Page): Promise<void> {
  await page.waitForFunction(() => {
    const dialog = document.querySelector('dialog.viewer');
    if (!dialog) return true;
    return document.getAnimations().every((animation) => {
      const target = (animation.effect as KeyframeEffect | null)?.target;
      return !target || !dialog.contains(target) || animation.playState !== 'running';
    });
  });
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
}

export interface FrameState { left: number; top: number; width: number; height: number; scale: number; natural: number; stage: { left: number; top: number; width: number; height: number } }

/** Где сейчас кадр текущей картинки и во сколько раз он увеличен от натуральных пикселей. */
export async function frameState(page: Page): Promise<FrameState> {
  return page.locator('dialog.viewer .slide[data-current] .frame').evaluate((frame) => {
    const box = frame.getBoundingClientRect();
    const natural = (frame.querySelector('img.full') as HTMLImageElement | null)?.naturalWidth || parseFloat((frame as HTMLElement).style.width);
    const stage = frame.closest('.stage')!.getBoundingClientRect();
    return { left: box.left, top: box.top, width: box.width, height: box.height, scale: box.width / natural, natural, stage: { left: stage.left, top: stage.top, width: stage.width, height: stage.height } };
  });
}

/** Какая точка картинки (в долях) лежит под точкой экрана. */
export function under(frame: FrameState, x: number, y: number): { u: number; v: number } {
  return { u: (x - frame.left) / frame.width, v: (y - frame.top) / frame.height };
}

export function project(frame: FrameState, point: { u: number; v: number }): { x: number; y: number } {
  return { x: frame.left + point.u * frame.width, y: frame.top + point.v * frame.height };
}

/** Касания через CDP: Playwright умеет только одиночный тап, а щипок и свайп нужны пальцами. */
export class Fingers {
  private constructor(private readonly cdp: CDPSession) {}

  static async on(page: Page): Promise<Fingers> {
    return new Fingers(await page.context().newCDPSession(page));
  }

  async down(points: { x: number; y: number }[]): Promise<void> {
    await this.cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: points.map((point, id) => ({ ...point, id })) });
  }

  async move(points: { x: number; y: number }[]): Promise<void> {
    await this.cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: points.map((point, id) => ({ ...point, id })) });
  }

  async up(): Promise<void> {
    await this.cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  }

  /** Один палец ведёт от `from` к `to` за `steps` шагов по `stepMs`. */
  async drag(from: { x: number; y: number }, to: { x: number; y: number }, steps = 12, stepMs = 16): Promise<void> {
    await this.down([from]);
    for (let step = 1; step <= steps; step++) {
      const t = step / steps;
      await this.move([{ x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t }]);
      await new Promise((resolve) => setTimeout(resolve, stepMs));
    }
    await this.up();
  }

  /** Щипок двумя пальцами вокруг `centre`: расстояние между ними меняется с `from` до `to`. */
  async pinch(centre: { x: number; y: number }, from: number, to: number, steps = 14): Promise<void> {
    const pair = (gap: number) => [{ x: centre.x - gap / 2, y: centre.y }, { x: centre.x + gap / 2, y: centre.y }];
    await this.down(pair(from));
    for (let step = 1; step <= steps; step++) {
      await this.move(pair(from + ((to - from) * step) / steps));
      await new Promise((resolve) => setTimeout(resolve, 16));
    }
    await this.up();
  }

  async tap(point: { x: number; y: number }): Promise<void> {
    await this.down([point]);
    await this.up();
  }
}
