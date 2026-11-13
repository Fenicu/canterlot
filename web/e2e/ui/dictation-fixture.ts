import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import type { FakeDictation } from '../../testing/fake-hub/dictation';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect } from './fixtures';

export const OWNER = '00000000-0000-4000-8000-0000000000d1';

export type MicrophoneStub = 'granted' | 'device' | 'NotAllowedError' | 'NotFoundError';

/**
 * Микрофон без разрешений и устройств: синтетическая дорожка из осциллятора.
 * Отказ и отсутствие устройства повторяют ошибки getUserMedia, на которые реагирует поле ввода.
 */
export async function stubMicrophone(page: Page, stub: MicrophoneStub = 'granted'): Promise<void> {
  // Настоящий поддельный микрофон Chromium: заглушка не нужна.
  if (stub === 'device') return;
  await page.addInitScript((mode) => {
    const media = navigator.mediaDevices;
    const override = (fn: unknown) =>
      Object.defineProperty(media, 'getUserMedia', { configurable: true, writable: true, value: fn });
    if (mode !== 'granted') {
      override(async () => {
        throw Object.assign(new Error(mode), { name: mode });
      });
      return;
    }
    override(async () => {
      const context = new AudioContext();
      const destination = context.createMediaStreamDestination();
      const oscillator = context.createOscillator();
      oscillator.frequency.value = 220;
      const gain = context.createGain();
      gain.gain.value = 0.2;
      oscillator.connect(gain);
      gain.connect(destination);
      oscillator.start();
      void context.resume().catch(() => undefined);
      return destination.stream;
    });
  }, stub);
}

export interface BootOptions {
  draft?: string;
  pref?: 'on' | 'off';
  mic?: MicrophoneStub;
  configure?(dictation: FakeDictation): void;
}

export async function bootDictation(page: Page, options: BootOptions = {}) {
  if (options.pref) await page.addInitScript((value) => localStorage.setItem('canterlot.dictation', value), options.pref);
  await stubMicrophone(page, options.mic ?? 'granted');
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  if (options.draft !== undefined) api.putDraft(id, { text: options.draft });
  options.configure?.(hub.dictation);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toBeVisible();
  return { hub, api, id };
}

export const micButton = (page: Page) => page.getByRole('button', { name: 'Диктовка' });
export const panel = (page: Page) => page.locator('[data-dictation]');
export const panelState = (page: Page) => page.locator('[data-dictation-state]');
export const area = (page: Page) => page.getByRole('textbox', { name: 'Сообщение' });

export async function startDictation(page: Page): Promise<void> {
  await micButton(page).click();
  await expect(panel(page)).toBeVisible();
}
