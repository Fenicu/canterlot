import type { Page } from '@playwright/test';
import { expect, test } from './fixtures';
import { Fingers, PICTURES, frameState, openChat, project, settled, thumb, under, viewer, type Source } from './image-viewer-fixture';

const phone = (name: string) => name === 'phone';

async function open(page: Page, source: Source, n = 0) {
  await thumb(page, source, n).click();
  const dialog = viewer(page);
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.slide[data-current] .frame[data-ready]')).toBeVisible();
  await settled(page);
  return dialog;
}

/** Двойное касание на телефоне пальцем, на рабочем столе двойной щелчок. */
async function doubleTap(page: Page, isPhone: boolean, point: { x: number; y: number }) {
  if (isPhone) {
    const fingers = await Fingers.on(page);
    await fingers.tap(point);
    await page.waitForTimeout(60);
    await fingers.tap(point);
  } else {
    await page.mouse.dblclick(point.x, point.y);
  }
  await settled(page);
}

async function drag(page: Page, isPhone: boolean, from: { x: number; y: number }, to: { x: number; y: number }, steps = 12) {
  if (isPhone) {
    await (await Fingers.on(page)).drag(from, to, steps);
  } else {
    await page.mouse.move(from.x, from.y);
    await page.mouse.down();
    for (let step = 1; step <= steps; step++) {
      await page.mouse.move(from.x + ((to.x - from.x) * step) / steps, from.y + ((to.y - from.y) * step) / steps);
    }
    await page.mouse.up();
  }
  await settled(page);
}

test('owner_attachment_opens_full_screen_on_a_dark_stage_and_escape_returns_focus', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('canterlot.theme', 'light'));
  await openChat(page, 'owner');
  const dialog = await open(page, 'owner', 1);
  await expect(dialog).toHaveAttribute('aria-modal', 'true');
  await expect(page.getByRole('dialog', { name: 'Вложения' })).toBeVisible();
  // Во весь экран и чёрный фон даже в светлой теме.
  const box = (await dialog.boundingBox())!;
  const viewport = page.viewportSize()!;
  expect(box.width).toBe(viewport.width);
  expect(box.height).toBe(viewport.height);
  expect(await dialog.locator('.backdrop').evaluate((node) => getComputedStyle(node).backgroundColor)).toBe('rgb(0, 0, 0)');
  await expect(dialog.locator('.counter')).toHaveText('2 из 3');
  const counter = (await dialog.locator('.counter').boundingBox())!;
  expect(Math.abs(counter.x + counter.width / 2 - viewport.width / 2)).toBeLessThan(2);
  // Картинка вписана: целиком на сцене и по центру.
  const frame = await frameState(page);
  expect(frame.width).toBeLessThanOrEqual(frame.stage.width + 1);
  expect(frame.height).toBeLessThanOrEqual(frame.stage.height + 1);
  expect(Math.abs(frame.left + frame.width / 2 - (frame.stage.left + frame.stage.width / 2))).toBeLessThan(2);
  const save = dialog.getByRole('link', { name: 'Сохранить' });
  await expect(save).toHaveAttribute('href', `/api/v1/attachments/${PICTURES[1]!.id}/content`);
  await expect(save).toHaveAttribute('download', 'screen-2.png');
  await expect(dialog.locator('.caption')).toContainText('screen-2.png');
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await expect(thumb(page, 'owner', 1)).toBeFocused();
  await expect(page).toHaveURL(/\/c\/[^/]+$/);
});

test('answer_image_opens_with_its_size_line_and_saves_the_media_url', async ({ page }) => {
  await openChat(page, 'answer');
  const dialog = await open(page, 'answer', 0);
  await expect(page.getByRole('dialog', { name: 'Картинки ответа' })).toBeVisible();
  await expect(dialog.locator('.caption')).toContainText('1440 × 900 · PNG');
  const save = dialog.getByRole('link', { name: 'Сохранить' });
  await expect(save).toHaveAttribute('href', `/api/v1/media/${PICTURES[0]!.id}`);
  await expect(save).toHaveAttribute('download', `картинка-${PICTURES[0]!.id.slice(0, 8)}.png`);
});

test('shared_file_image_opens_with_its_title_and_saves_under_its_name', async ({ page }) => {
  await openChat(page, 'shared');
  const dialog = await open(page, 'shared', 2);
  await expect(page.getByRole('dialog', { name: 'Файл' })).toBeVisible();
  await expect(dialog.locator('.counter')).toHaveText('3 из 3');
  await expect(dialog.locator('.caption')).toContainText('Рабочий стол 3');
  const save = dialog.getByRole('link', { name: 'Сохранить' });
  await expect(save).toHaveAttribute('href', `/api/v1/attachments/${PICTURES[2]!.id}/content`);
  await expect(save).toHaveAttribute('download', 'desktop-3.png');
});

test('double_tap_zooms_at_the_tapped_point_and_toggles_back_to_fit', async ({ page }, info) => {
  await openChat(page, 'shared');
  await open(page, 'shared', 0);
  const fit = await frameState(page);
  const point = { x: fit.left + fit.width * 0.3, y: fit.top + fit.height * 0.45 };
  const spot = under(fit, point.x, point.y);
  await doubleTap(page, phone(info.project.name), point);
  const zoomed = await frameState(page);
  expect(zoomed.scale).toBeGreaterThan(fit.scale * 2);
  // Двойное касание даёт 2,5 от вписанного или натуральный размер, если он крупнее.
  expect(zoomed.scale).toBeCloseTo(Math.max(fit.scale * 2.5, 1), 2);
  const back = project(zoomed, spot);
  expect(Math.abs(back.x - point.x)).toBeLessThan(4);
  expect(Math.abs(back.y - point.y)).toBeLessThan(4);
  await expect(viewer(page)).toHaveAttribute('data-zoomed', '');
  await doubleTap(page, phone(info.project.name), point);
  const again = await frameState(page);
  expect(again.scale).toBeCloseTo(fit.scale, 3);
  expect(Math.abs(again.left - fit.left)).toBeLessThan(1);
});

test('pan_is_bounded_with_a_rubber_band_and_the_image_cannot_be_lost', async ({ page }, info) => {
  await openChat(page, 'shared');
  await open(page, 'shared', 0);
  const fit = await frameState(page);
  await doubleTap(page, phone(info.project.name), { x: fit.left + fit.width / 2, y: fit.top + fit.height / 2 });
  const zoomed = await frameState(page);
  const { stage } = zoomed;
  const y = stage.top + stage.height / 2;
  // Тянем вправо и вниз несколько раз: картинка доходит до края сцены и дальше не уходит.
  for (let pass = 0; pass < 4; pass++) {
    await drag(page, phone(info.project.name), { x: stage.left + stage.width * 0.1, y: stage.top + stage.height * 0.1 }, { x: stage.left + stage.width * 0.9, y: stage.top + stage.height * 0.9 }, 16);
  }
  const pulled = await frameState(page);
  expect(pulled.scale).toBeCloseTo(zoomed.scale, 3);
  expect(Math.abs(pulled.left - stage.left)).toBeLessThan(1);
  if (pulled.height > stage.height) expect(Math.abs(pulled.top - stage.top)).toBeLessThan(1);
  else expect(Math.abs(pulled.top + pulled.height / 2 - (stage.top + stage.height / 2))).toBeLessThan(1);
  // И в обратную сторону: правый край не отходит от правого края сцены.
  for (let pass = 0; pass < 6; pass++) {
    await drag(page, phone(info.project.name), { x: stage.left + stage.width * 0.9, y }, { x: stage.left + stage.width * 0.1, y }, 16);
  }
  const other = await frameState(page);
  expect(Math.abs(other.left + other.width - (stage.left + stage.width))).toBeLessThan(1);
});

test('rubber_band_lets_the_image_past_the_edge_while_the_finger_holds_it', async ({ page }, info) => {
  test.skip(!phone(info.project.name), 'удержание пальцем проверяется на телефоне');
  await openChat(page, 'shared');
  await open(page, 'shared', 0);
  const fit = await frameState(page);
  await doubleTap(page, true, { x: fit.left + fit.width / 2, y: fit.top + fit.height / 2 });
  const zoomed = await frameState(page);
  const fingers = await Fingers.on(page);
  const start = { x: zoomed.stage.width / 2, y: zoomed.stage.height / 2 };
  await fingers.down([start]);
  for (let step = 1; step <= 10; step++) await fingers.move([{ x: start.x + step * 200, y: start.y }]);
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  const held = await frameState(page);
  // За краем картинка идёт, но медленнее пальца: 2000 px пальца дают меньше ширины сцены.
  expect(held.left).toBeGreaterThan(held.stage.left + 5);
  expect(held.left).toBeLessThan(held.stage.left + held.stage.width);
  await fingers.up();
  await settled(page);
  expect(Math.abs((await frameState(page)).left - held.stage.left)).toBeLessThan(1);
});

test('pinch_zooms_around_the_fingers', async ({ page }, info) => {
  test.skip(!phone(info.project.name), 'щипок двумя пальцами есть только на сенсорном экране');
  await openChat(page, 'shared');
  await open(page, 'shared', 0);
  const fit = await frameState(page);
  const centre = { x: fit.left + fit.width * 0.6, y: fit.top + fit.height * 0.5 };
  const spot = under(fit, centre.x, centre.y);
  const fingers = await Fingers.on(page);
  await fingers.pinch(centre, 60, 240);
  await settled(page);
  const zoomed = await frameState(page);
  expect(zoomed.scale / fit.scale).toBeGreaterThan(3.2);
  const back = project(zoomed, spot);
  expect(Math.abs(back.x - centre.x)).toBeLessThan(12);
  expect(Math.abs(back.y - centre.y)).toBeLessThan(12);
  // Щипок назад сводит к вписанному размеру и не меньше.
  await fingers.pinch(centre, 300, 20);
  await settled(page);
  expect((await frameState(page)).scale).toBeCloseTo(fit.scale, 3);
});

test('wheel_and_trackpad_pinch_zoom_at_the_cursor_on_desktop', async ({ page }, info) => {
  test.skip(phone(info.project.name), 'колесо есть на рабочем столе');
  await openChat(page, 'answer');
  await open(page, 'answer', 1);
  const fit = await frameState(page);
  const cursor = { x: fit.left + fit.width * 0.7, y: fit.top + fit.height * 0.35 };
  const spot = under(fit, cursor.x, cursor.y);
  await page.mouse.move(cursor.x, cursor.y);
  for (let notch = 0; notch < 4; notch++) await page.mouse.wheel(0, -120);
  await settled(page);
  const zoomed = await frameState(page);
  expect(zoomed.scale).toBeGreaterThan(fit.scale * 1.5);
  const back = project(zoomed, spot);
  expect(Math.abs(back.x - cursor.x)).toBeLessThan(3);
  expect(Math.abs(back.y - cursor.y)).toBeLessThan(3);
  // Прокрутка вниз возвращает к вписанному виду и не уводит меньше.
  for (let notch = 0; notch < 20; notch++) await page.mouse.wheel(0, 240);
  await settled(page);
  expect((await frameState(page)).scale).toBeCloseTo(fit.scale, 3);
});

test('keyboard_zooms_with_plus_minus_and_resets_with_zero', async ({ page }) => {
  await openChat(page, 'answer');
  await open(page, 'answer', 0);
  const fit = await frameState(page);
  await page.keyboard.press('+');
  await settled(page);
  expect((await frameState(page)).scale).toBeCloseTo(fit.scale * 1.5, 3);
  await page.keyboard.press('+');
  await page.keyboard.press('-');
  await settled(page);
  expect((await frameState(page)).scale).toBeCloseTo(fit.scale * 1.5, 3);
  await page.keyboard.press('0');
  await settled(page);
  expect((await frameState(page)).scale).toBeCloseTo(fit.scale, 3);
});

test('swipe_and_arrow_keys_move_between_images_with_the_counter', async ({ page }, info) => {
  await openChat(page, 'owner');
  const dialog = await open(page, 'owner', 0);
  const counter = dialog.locator('.counter');
  await expect(counter).toHaveText('1 из 3');
  if (phone(info.project.name)) {
    const { stage } = await frameState(page);
    const y = stage.top + stage.height / 2;
    await drag(page, true, { x: stage.width * 0.8, y }, { x: stage.width * 0.2, y }, 10);
    await expect(counter).toHaveText('2 из 3');
    await drag(page, true, { x: stage.width * 0.8, y }, { x: stage.width * 0.2, y }, 10);
    await expect(counter).toHaveText('3 из 3');
    // На последней картинке лента только пружинит.
    await drag(page, true, { x: stage.width * 0.8, y }, { x: stage.width * 0.2, y }, 10);
    await expect(counter).toHaveText('3 из 3');
    await drag(page, true, { x: stage.width * 0.2, y }, { x: stage.width * 0.8, y }, 10);
    await expect(counter).toHaveText('2 из 3');
  } else {
    await page.keyboard.press('ArrowRight');
    await expect(counter).toHaveText('2 из 3');
    await page.keyboard.press('ArrowRight');
    await page.keyboard.press('ArrowRight');
    await expect(counter).toHaveText('3 из 3');
    await page.keyboard.press('ArrowLeft');
    await expect(counter).toHaveText('2 из 3');
    await dialog.getByRole('button', { name: 'Предыдущая картинка' }).click();
    await expect(counter).toHaveText('1 из 3');
    await expect(dialog.getByRole('button', { name: 'Предыдущая картинка' })).toHaveAttribute('aria-disabled', 'true');
  }
  await settled(page);
  // Текущая картинка стоит ровно в кадре после перехода, соседи уже загружены.
  const frame = await frameState(page);
  expect(Math.abs(frame.left + frame.width / 2 - (frame.stage.left + frame.stage.width / 2))).toBeLessThan(2);
  // Соседи догружаются после перехода: ждём, а не ловим момент.
  await expect(async () => {
    const loaded = await dialog.locator('.slide img.full').evaluateAll((nodes) => nodes.map((node) => (node as HTMLImageElement).complete && (node as HTMLImageElement).naturalWidth > 0));
    expect(loaded.length).toBeGreaterThanOrEqual(2);
    expect(loaded.every(Boolean)).toBe(true);
  }).toPass();
});

test('swipe_down_closes_and_the_backdrop_fades_with_the_drag', async ({ page }, info) => {
  await openChat(page, 'shared');
  const dialog = await open(page, 'shared', 1);
  const { stage } = await frameState(page);
  const start = { x: stage.left + stage.width / 2, y: stage.top + stage.height / 2 };
  if (phone(info.project.name)) {
    const fingers = await Fingers.on(page);
    await fingers.down([start]);
    for (let step = 1; step <= 8; step++) await fingers.move([{ x: start.x, y: start.y + step * 20 }]);
    await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    const opacity = Number(await dialog.locator('.backdrop').evaluate((node) => getComputedStyle(node).opacity));
    expect(opacity).toBeLessThan(0.9);
    expect(opacity).toBeGreaterThan(0.2);
    // Картинка идёт за пальцем.
    const moved = await page.locator('dialog.viewer .slide[data-current]').evaluate((node) => new DOMMatrix(getComputedStyle(node).transform).m42);
    expect(moved).toBeGreaterThan(120);
    for (let step = 9; step <= 14; step++) await fingers.move([{ x: start.x, y: start.y + step * 20 }]);
    await fingers.up();
  } else {
    await drag(page, false, start, { x: start.x, y: start.y + 300 }, 10);
  }
  await expect(dialog).toHaveCount(0);
  await expect(thumb(page, 'shared', 1)).toBeFocused();
});

test('a_short_vertical_drag_springs_back_without_closing', async ({ page }, info) => {
  await openChat(page, 'shared');
  const dialog = await open(page, 'shared', 0);
  const { stage } = await frameState(page);
  const start = { x: stage.left + stage.width / 2, y: stage.top + stage.height / 2 };
  await drag(page, phone(info.project.name), start, { x: start.x, y: start.y + 40 }, 16);
  await expect(dialog).toBeVisible();
  await expect.poll(async () => Number(await dialog.locator('.backdrop').evaluate((node) => getComputedStyle(node).opacity))).toBe(1);
});

test('browser_back_closes_the_viewer_and_stays_in_the_chat', async ({ page }) => {
  await openChat(page, 'owner');
  const url = page.url();
  const dialog = await open(page, 'owner', 2);
  await page.goBack();
  await expect(dialog).toHaveCount(0);
  expect(page.url()).toBe(url);
  await expect(page.locator('.bubble')).toBeVisible();
  await expect(thumb(page, 'owner', 2)).toBeFocused();
  // Повторное открытие снова кладёт запись в историю и закрывается кнопкой без лишнего шага назад.
  await open(page, 'owner', 0);
  await viewer(page).getByRole('button', { name: 'Закрыть' }).click();
  await expect(viewer(page)).toHaveCount(0);
  await expect(thumb(page, 'owner', 0)).toBeFocused();
  await expect(page).toHaveURL(url);
});

// QUARANTINE: в полном гейте 1.22.3 упал на телефоне под нагрузкой; отдельно проходит.
test.describe('quarantine controls_hide_after_a_tap_and_the_next_tap_brings_them_back', () => {
  test.describe.configure({ retries: 2 });
  test('controls_hide_after_a_tap_and_the_next_tap_brings_them_back', async ({ page }, info) => {
    await openChat(page, 'owner');
    const dialog = await open(page, 'owner', 0);
    await expect(dialog).toHaveAttribute('data-chrome', 'shown');
    const { stage } = await frameState(page);
    const point = { x: stage.left + stage.width / 2, y: stage.top + stage.height * 0.8 };
    const tap = async () => (phone(info.project.name) ? (await Fingers.on(page)).tap(point) : page.mouse.click(point.x, point.y));
    await tap();
    await expect(dialog).toHaveAttribute('data-chrome', 'hidden');
    // Кнопки гаснут CSS-переходом: атрибут меняется раньше, чем прозрачность.
    await expect.poll(async () => Number(await dialog.locator('.chrome').evaluate((node) => getComputedStyle(node).opacity))).toBeLessThan(0.5);
    await tap();
    await expect(dialog).toHaveAttribute('data-chrome', 'shown');
  });
});

test('focus_stays_inside_the_viewer_and_controls_are_labelled', async ({ page }) => {
  await openChat(page, 'answer');
  const dialog = await open(page, 'answer', 0);
  for (let step = 0; step < 8; step++) {
    await page.keyboard.press('Tab');
    expect(await dialog.evaluate((node) => node.contains(document.activeElement) && document.activeElement !== node)).toBe(true);
  }
  // Tab показывает спрятанные кнопки: клавиатуре они нужны всегда.
  await expect(dialog).toHaveAttribute('data-chrome', 'shown');
  await expect(dialog.getByRole('button', { name: 'Закрыть' })).toBeVisible();
  await expect(dialog.getByRole('link', { name: 'Сохранить' })).toBeVisible();
});

test('reduced_motion_fades_instead_of_flying_from_the_thumbnail', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  // Каждая анимация записывается в момент запуска: короткое затухание успело бы кончиться до проверки.
  await page.addInitScript(() => {
    const log: { target: string; props: string[]; duration: number }[] = [];
    (window as unknown as { __animations: typeof log }).__animations = log;
    const original = Element.prototype.animate;
    Element.prototype.animate = function (this: Element, keyframes: Keyframe[] | PropertyIndexedKeyframes | null, options?: number | KeyframeAnimationOptions) {
      const frames = Array.isArray(keyframes) ? keyframes : [];
      const props = [...new Set(frames.flatMap((frame) => Object.keys(frame)).filter((key) => !['offset', 'easing', 'composite'].includes(key)))];
      log.push({ target: typeof this.className === 'string' ? this.className : '', props, duration: typeof options === 'number' ? options : Number(options?.duration ?? 0) });
      return original.call(this, keyframes, options);
    };
  });
  await openChat(page, 'answer');
  await thumb(page, 'answer', 0).click();
  const dialog = viewer(page);
  await expect(dialog).toBeVisible();
  await settled(page);
  const opened = await page.evaluate(() => (window as unknown as { __animations: { target: string; props: string[]; duration: number }[] }).__animations.filter((item) => item.target.includes('viewer') || item.target.includes('frame') || item.target.includes('backdrop')));
  // Только прозрачность окна, и она не нулевая; кадр не летит из миниатюры.
  expect(opened.filter((item) => item.props.includes('opacity') && item.duration > 0).map((item) => item.target.split(' ')[0])).toEqual(['viewer']);
  expect(opened.some((item) => item.props.includes('transform'))).toBe(false);
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  const closed = await page.evaluate(() => (window as unknown as { __animations: { props: string[] }[] }).__animations);
  expect(closed.some((item) => item.props.includes('transform'))).toBe(false);
  await expect(thumb(page, 'answer', 0)).toBeFocused();
});

test('opening_flies_the_frame_out_of_its_thumbnail', async ({ page }) => {
  await openChat(page, 'answer');
  const origin = (await thumb(page, 'answer', 0).boundingBox())!;
  await page.evaluate(() => {
    // Первый кадр анимации: где кадр начинает свой путь.
    const seen: { x: number; y: number; w: number }[] = [];
    (window as unknown as { __viewerStart: typeof seen }).__viewerStart = seen;
    const observer = new MutationObserver(() => {
      const frame = document.querySelector('dialog.viewer .slide[data-current] .frame');
      if (!frame || seen.length) return;
      const animation = frame.getAnimations()[0];
      if (!animation) return;
      animation.pause();
      const box = frame.getBoundingClientRect();
      seen.push({ x: box.left, y: box.top, w: box.width });
      animation.play();
      observer.disconnect();
    });
    observer.observe(document.body, { subtree: true, childList: true, attributes: true });
  });
  await thumb(page, 'answer', 0).click();
  await expect(viewer(page)).toBeVisible();
  await settled(page);
  const start = await page.evaluate(() => (window as unknown as { __viewerStart: { x: number; y: number; w: number }[] }).__viewerStart[0]);
  expect(start).toBeTruthy();
  // Кадр в начале покрывает миниатюру (обрезка `cover`), а не стоит сразу на месте.
  const fit = await frameState(page);
  expect(start!.w).toBeLessThan(fit.width);
  expect(start!.y).toBeGreaterThan(origin.y - origin.height);
  expect(start!.y).toBeLessThan(origin.y + origin.height);
});
