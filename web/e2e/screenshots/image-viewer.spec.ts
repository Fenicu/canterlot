import { copyFileSync, mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { devices, type Page } from '@playwright/test';
import { expect, test } from '../ui/fixtures';
import { Fingers, frameState, openChat, settled, thumb, viewer } from '../ui/image-viewer-fixture';

// Снимки и запись просмотрщика для владельца: PW_SCREENSHOTS=1 npx playwright test e2e/screenshots/image-viewer.spec.ts --project=phone
const OUT = process.env.VIEWER_SHOTS_DIR ?? '/tmp/image-viewer';
const PHONE = { width: 390, height: 844 };

/** Точки касаний на записи: без них на видео не понять, что делает палец. */
async function showTouches(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const dots = new Map<number, HTMLElement>();
    const place = (event: PointerEvent) => {
      if (event.pointerType !== 'touch') return;
      let dot = dots.get(event.pointerId);
      const host = document.querySelector('dialog[open]') ?? document.body;
      if (!dot) {
        dot = document.createElement('div');
        dot.className = 'touch-dot';
        dot.style.cssText = 'position:fixed;width:34px;height:34px;margin:-17px 0 0 -17px;border-radius:50%;background:rgba(255,255,255,.42);outline:2px solid rgba(255,255,255,.8);pointer-events:none;z-index:2147483647';
        dots.set(event.pointerId, dot);
      }
      if (dot.parentElement !== host) host.append(dot);
      dot.style.left = `${event.clientX}px`;
      dot.style.top = `${event.clientY}px`;
    };
    const lift = (event: PointerEvent) => {
      dots.get(event.pointerId)?.remove();
      dots.delete(event.pointerId);
    };
    addEventListener('pointerdown', place, true);
    addEventListener('pointermove', place, true);
    addEventListener('pointerup', lift, true);
    addEventListener('pointercancel', lift, true);
  });
}

test.describe('просмотр картинок', () => {
  test.beforeAll(() => mkdirSync(OUT, { recursive: true }));

  test('phone_dark_zoomed', async ({ page }, info) => {
    test.skip(info.project.name !== 'phone', 'снимок телефона');
    await page.setViewportSize(PHONE);
    await page.addInitScript(() => localStorage.setItem('canterlot.theme', 'dark'));
    await openChat(page, 'shared');
    await thumb(page, 'shared', 0).click();
    await expect(viewer(page).locator('.slide[data-current] .frame[data-ready]')).toBeVisible();
    await settled(page);
    const fit = await frameState(page);
    const fingers = await Fingers.on(page);
    await fingers.tap({ x: fit.left + fit.width * 0.42, y: fit.top + fit.height * 0.4 });
    await page.waitForTimeout(60);
    await fingers.tap({ x: fit.left + fit.width * 0.42, y: fit.top + fit.height * 0.4 });
    await settled(page);
    // Кнопки на снимке видны: после касания они прятались бы.
    await expect(viewer(page)).toHaveAttribute('data-chrome', 'hidden');
    await page.keyboard.press('Tab');
    await expect(viewer(page)).toHaveAttribute('data-chrome', 'shown');
    await viewer(page).locator('.chrome').evaluate((node) => node.querySelector<HTMLElement>(':focus')?.blur());
    await settled(page);
    await page.waitForTimeout(200);
    await page.screenshot({ path: resolve(OUT, 'phone-390-dark-zoomed.png') });
  });

  test('desktop_controls', async ({ page }, info) => {
    test.skip(info.project.name !== 'desktop', 'снимок рабочего стола');
    await page.setViewportSize({ width: 1440, height: 900 });
    await openChat(page, 'shared');
    // Портретный снимок: видно и чёрную сцену вокруг, и кнопки листания по бокам.
    await thumb(page, 'shared', 2).click();
    await expect(viewer(page).locator('.slide[data-current] .frame[data-ready]')).toBeVisible();
    await settled(page);
    await page.mouse.move(720, 450);
    await expect(viewer(page)).toHaveAttribute('data-chrome', 'shown');
    await page.screenshot({ path: resolve(OUT, 'desktop-1440-controls.png') });
  });

  test.describe('запись', () => {
    test('phone_recording', async ({ browser }, info) => {
      test.skip(info.project.name !== 'phone', 'запись телефона');
      test.setTimeout(90_000);
      // Своя вкладка с записью: телефонная эмуляция проекта, но экран 390 и видео того же размера.
      const { defaultBrowserType: _ignored, ...pixel } = devices['Pixel 7'];
      void _ignored;
      const context = await browser.newContext({ ...pixel, viewport: PHONE, recordVideo: { dir: resolve(OUT, 'raw'), size: PHONE } });
      const page = await context.newPage();
      await showTouches(page);
      await page.addInitScript(() => localStorage.setItem('canterlot.theme', 'dark'));
      await openChat(page, 'shared');
      const fingers = await Fingers.on(page);
      await page.waitForTimeout(1200);
      // Открыть с миниатюры.
      const box = (await thumb(page, 'shared', 0).boundingBox())!;
      await fingers.tap({ x: box.x + box.width / 2, y: box.y + box.height / 2 });
      await expect(viewer(page).locator('.slide[data-current] .frame[data-ready]')).toBeVisible();
      await settled(page);
      await page.waitForTimeout(1300);
      // Двойное касание приближает в точке касания.
      const fit = await frameState(page);
      const spot = { x: fit.left + fit.width * 0.3, y: fit.top + fit.height * 0.35 };
      await fingers.tap(spot);
      await page.waitForTimeout(70);
      await fingers.tap(spot);
      await settled(page);
      await page.waitForTimeout(1100);
      // Сдвиг пальцем по приближенной картинке и упор в край.
      await fingers.drag({ x: 300, y: 520 }, { x: 80, y: 380 }, 40, 22);
      await settled(page);
      await page.waitForTimeout(600);
      await fingers.drag({ x: 90, y: 400 }, { x: 330, y: 470 }, 40, 22);
      await settled(page);
      await page.waitForTimeout(800);
      // Двойное касание возвращает вписанный вид.
      await fingers.tap({ x: 200, y: 420 });
      await page.waitForTimeout(70);
      await fingers.tap({ x: 200, y: 420 });
      await settled(page);
      await page.waitForTimeout(900);
      // Щипок двумя пальцами.
      await fingers.pinch({ x: 230, y: 420 }, 70, 260, 36);
      await settled(page);
      await page.waitForTimeout(1100);
      await fingers.pinch({ x: 230, y: 420 }, 260, 60, 30);
      await settled(page);
      await page.waitForTimeout(900);
      // Свайп к следующей картинке.
      await fingers.drag({ x: 330, y: 420 }, { x: 70, y: 430 }, 22, 16);
      await settled(page);
      await expect(viewer(page).locator('.counter')).toHaveText('2 из 3');
      await page.waitForTimeout(1300);
      // Касание прячет и показывает кнопки.
      await fingers.tap({ x: 195, y: 650 });
      await page.waitForTimeout(1100);
      await fingers.tap({ x: 195, y: 650 });
      await page.waitForTimeout(1100);
      // Свайп вниз закрывает просмотр, кадр уходит в свою миниатюру.
      await fingers.drag({ x: 195, y: 360 }, { x: 205, y: 700 }, 30, 20);
      await expect(viewer(page)).toHaveCount(0);
      await page.waitForTimeout(1500);
      const video = page.video();
      await context.close();
      if (video) copyFileSync(await video.path(), resolve(OUT, 'viewer-phone.webm'));
    });
  });
});
