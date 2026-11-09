import type { Locator, Page } from '@playwright/test';
import { expect, test } from './fixtures';
import { Fingers, PICTURES, openChat, settled, thumb, viewer, type Source } from './image-viewer-fixture';

// Меню браузера на картинке (копировать, скачать, открыть в новой вкладке) строится по элементу под пальцем
// или курсором. Оно появляется, только если там настоящая загруженная <img> с адресом этого же сайта,
// а нажатие не отменено обработчиками страницы.

const phone = (name: string) => name === 'phone';

/** Состояние элемента в точке: что под ней, откуда картинка, не запрещено ли меню стилями. */
async function hit(page: Page, point: { x: number; y: number }) {
  return page.evaluate(({ x, y }) => {
    const node = document.elementFromPoint(x, y);
    const image = node instanceof HTMLImageElement ? node : null;
    const chain: string[] = [];
    for (let el: Element | null = node; el; el = el.parentElement) {
      const style = getComputedStyle(el) as CSSStyleDeclaration & { webkitTouchCallout?: string };
      if (style.webkitTouchCallout === 'none' || style.getPropertyValue('-webkit-touch-callout') === 'none') chain.push(el.tagName);
    }
    return {
      tag: node?.tagName ?? null,
      src: image?.src ?? null,
      loaded: Boolean(image?.complete && image.naturalWidth > 0),
      sameOrigin: image ? new URL(image.src).origin === location.origin : false,
      callout: chain
    };
  }, point);
}

/** Шлёт contextmenu на элемент в точке и возвращает, отменила ли его страница. */
async function contextMenuPrevented(page: Page, point: { x: number; y: number }): Promise<boolean> {
  return page.evaluate(({ x, y }) => {
    const node = document.elementFromPoint(x, y)!;
    const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, composed: true, clientX: x, clientY: y, button: 2 });
    node.dispatchEvent(event);
    return event.defaultPrevented;
  }, point);
}

/** Картинку можно нарисовать на холсте и прочитать: значит она не «чужая» и браузер может её скопировать. */
async function readable(image: Locator): Promise<boolean> {
  return image.evaluate((node: HTMLImageElement) => {
    const canvas = document.createElement('canvas');
    canvas.width = Math.min(16, node.naturalWidth);
    canvas.height = Math.min(16, node.naturalHeight);
    canvas.getContext('2d')!.drawImage(node, 0, 0, canvas.width, canvas.height);
    try {
      return canvas.toDataURL('image/png').length > 0;
    } catch {
      return false;
    }
  });
}

async function centre(locator: Locator): Promise<{ x: number; y: number }> {
  const box = (await locator.boundingBox())!;
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/** Следит за событиями нажатия на окне: отменил ли их кто-то и куда пришло contextmenu. */
async function watchPresses(page: Page): Promise<void> {
  await page.evaluate(() => {
    const log: { type: string; prevented: boolean; target: string }[] = [];
    (window as unknown as { __presses: typeof log }).__presses = log;
    for (const type of ['touchstart', 'touchend', 'pointerdown', 'pointerup', 'contextmenu']) {
      window.addEventListener(type, (event) => log.push({ type, prevented: event.defaultPrevented, target: (event.target as Element | null)?.tagName ?? '' }), { passive: true });
    }
  });
}

async function presses(page: Page): Promise<{ type: string; prevented: boolean; target: string }[]> {
  return page.evaluate(() => (window as unknown as { __presses: { type: string; prevented: boolean; target: string }[] }).__presses);
}

const ADDRESS: Record<Source, (id: string) => string> = {
  owner: (id) => `/api/v1/attachments/${id}/content`,
  shared: (id) => `/api/v1/attachments/${id}/content`,
  answer: (id) => `/api/v1/media/${id}`
};

for (const source of ['owner', 'answer', 'shared'] as const) {
  test(`${source}_feed_image_is_a_plain_same_origin_img_the_browser_menu_can_use`, async ({ page }) => {
    await openChat(page, source);
    const link = thumb(page, source, 0);
    const image = link.locator('img');
    await expect(image).toHaveJSProperty('complete', true);
    // Миниатюра — ссылка на сам файл: меню браузера предлагает и картинку, и «Открыть в новой вкладке».
    await expect(link).toHaveAttribute('href', ADDRESS[source](PICTURES[0]!.id));
    await expect(image).toHaveAttribute('src', ADDRESS[source](PICTURES[0]!.id));
    const point = await centre(image);
    const state = await hit(page, point);
    expect(state).toMatchObject({ tag: 'IMG', loaded: true, sameOrigin: true, callout: [] });
    expect(new URL(state.src!).pathname).toBe(ADDRESS[source](PICTURES[0]!.id));
    expect(await contextMenuPrevented(page, point)).toBe(false);
    expect(await readable(image)).toBe(true);
    // Обычное нажатие по-прежнему открывает просмотрщик, а не переходит по ссылке.
    await link.click();
    await expect(viewer(page)).toBeVisible();
    await expect(page).toHaveURL(/\/c\/[^/]+$/);
  });
}

test('viewer_image_receives_long_press_and_right_click_without_page_blocking_them', async ({ page }, info) => {
  await openChat(page, 'owner');
  await thumb(page, 'owner', 0).click();
  const dialog = viewer(page);
  await expect(dialog.locator('.slide[data-current] .frame[data-ready]')).toBeVisible();
  await settled(page);
  const image = dialog.locator('.slide[data-current] img.full');
  const point = await centre(image);
  // Под пальцем сам оригинал по адресу вложения, а не подложка сцены.
  const state = await hit(page, point);
  expect(state).toMatchObject({ tag: 'IMG', loaded: true, sameOrigin: true, callout: [] });
  expect(new URL(state.src!).pathname).toBe(`/api/v1/attachments/${PICTURES[0]!.id}/content`);
  expect(await contextMenuPrevented(page, point)).toBe(false);
  expect(await readable(image)).toBe(true);

  await watchPresses(page);
  if (phone(info.project.name)) {
    // Долгое нажатие пальцем. Меню телефона безголовый Chromium не строит, поэтому проверяется то, от чего оно
    // зависит: касание приходит на картинку и страница его не отменяет.
    const fingers = await Fingers.on(page);
    await fingers.down([point]);
    await page.waitForTimeout(800);
    await fingers.up();
    const log = await presses(page);
    expect(log.filter((entry) => entry.prevented)).toEqual([]);
    expect(log.filter((entry) => entry.type === 'touchstart' || entry.type === 'pointerdown').map((entry) => entry.target)).toEqual(['IMG', 'IMG']);
  } else {
    await page.mouse.click(point.x, point.y, { button: 'right' });
    const log = await presses(page);
    expect(log.filter((entry) => entry.prevented)).toEqual([]);
    expect(log.filter((entry) => entry.type === 'contextmenu').map((entry) => entry.target)).toEqual(['IMG']);
  }
  // Меню не закрыло просмотрщик и не листнуло картинку.
  await page.waitForTimeout(400);
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.slide[data-current]')).toHaveAttribute('data-index', '0');
});

test('viewer_image_copies_to_the_clipboard_through_the_async_clipboard_api', async ({ page, context }, info) => {
  // Буфер с картинками в Playwright есть только у Chromium.
  test.skip(!phone(info.project.name), 'Clipboard API с картинками проверяется в Chromium');
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await openChat(page, 'owner');
  await thumb(page, 'owner', 0).click();
  const image = viewer(page).locator('.slide[data-current] img.full');
  await expect(viewer(page).locator('.slide[data-current] .frame[data-ready]')).toBeVisible();
  const copied = await image.evaluate(async (node: HTMLImageElement) => {
    const blob = await (await fetch(node.src, { credentials: 'same-origin' })).blob();
    await navigator.clipboard.write([new ClipboardItem({ 'image/png': blob })]);
    const [item] = await navigator.clipboard.read();
    const back = await item!.getType('image/png');
    const bitmap = await createImageBitmap(back);
    return { type: blob.type, width: bitmap.width, height: bitmap.height, natural: [node.naturalWidth, node.naturalHeight] };
  });
  expect(copied.type).toBe('image/png');
  expect([copied.width, copied.height]).toEqual(copied.natural);
});

test('save_button_still_downloads_the_file_under_its_name', async ({ page }) => {
  await openChat(page, 'owner');
  await thumb(page, 'owner', 1).click();
  const dialog = viewer(page);
  await expect(dialog.locator('.slide[data-current] .frame[data-ready]')).toBeVisible();
  const [download] = await Promise.all([page.waitForEvent('download'), dialog.getByRole('link', { name: 'Сохранить' }).click()]);
  expect(download.suggestedFilename()).toBe('screen-2.png');
  expect(new URL(download.url()).pathname).toBe(`/api/v1/attachments/${PICTURES[1]!.id}/content`);
});
