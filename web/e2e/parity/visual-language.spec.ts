import AxeBuilder from '@axe-core/playwright';
import type { Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { PALETTES } from '../../src/lib/palettes';
import { expect, test } from '../ui/fixtures';
import { bootScreen, SCREENS, type Theme } from './states';
import { geometryAudit, type Finding } from './geometry';

const THEMES: Theme[] = ['dark', 'light'];
const COLOR_TOKENS = [...readFileSync(resolve(process.cwd(), 'src/lib/styles/tokens.css'), 'utf8').matchAll(/(--[a-z0-9-]+):\s*(?:#|rgb|hsl|oklch|color-mix)/g)].map((match) => match[1]!);
// 03-chat-light, docx-viewer-light и xlsx-viewer-light отличаются от тёмных экранов только
// темой по умолчанию, а аудит прогоняет обе темы сам: в проверках это точные дубли.
// В SCREENS они остаются ради контактного листа и снимков макетов.
const ALIASES = new Set(['03-chat-light', 'docx-viewer-light', 'xlsx-viewer-light']);
const AUDITED = SCREENS.filter((screen) => !ALIASES.has(screen.name));
// Минимум узлов color-contrast, которые axe обязан проверить на каждом экране. Порог — 60 % от
// сегодняшнего покрытия (но не меньше трёх узлов): правка фикстуры на пару слов его не сломает,
// а ставшее неприменимым правило (0–2 узла) — сломает.
const CONTRAST_FLOOR: Record<string, number> = {
  '01-sessions': 36,
  '02-chat': 9,
  '04-info-sheet': 18,
  '05-subagent-lane': 11,
  '06-question': 7,
  '07-new-session': 11,
  '07-new-session-collapsed': 7,
  '08-quota-paused': 7,
  '09-machine': 19,
  '09-machine-lease-queue': 20,
  '10-limits-history': 36,
  '11-desktop': 31,
  attachments: 12,
  bookmarks: 24,
  'bookmarks-desktop': 37,
  'chat-trashed': 3,
  'delegation-queued': 9,
  diagnostics: 16,
  'docx-viewer': 19,
  'docx-viewer-desktop': 18,
  'feed-cards': 40,
  'feed-cards-desktop': 39,
  'feed-cards-top': 12,
  'group-manager': 7,
  'image-viewer': 3,
  'image-viewer-desktop': 3,
  'limits-desktop': 117,
  'machine-watch': 21,
  'machine-watch-desktop': 58,
  'markdown-viewer': 13,
  'permission-request': 12,
  'plan-cards': 57,
  'plan-cards-desktop': 38,
  'plan-cards-top': 32,
  'plan-screen-desktop': 36,
  'plan-tab': 22,
  'plan-tab-desktop': 47,
  'plans-list': 3,
  'question-request': 16,
  queue: 12,
  'route-sheet': 10,
  'routing-editor': 40,
  'routing-preview': 80,
  search: 28,
  'search-desktop': 58,
  'search-session': 14,
  'send-later': 6,
  'session-menu': 13,
  'session-popover': 4,
  'session-sheet': 10,
  settings: 8,
  'settings-pane': 34,
  'settings-sheet': 15,
  share: 3,
  'spawn-queue': 16,
  'system-bubbles': 51,
  'taiga-cards': 50,
  'taiga-cards-comment': 40,
  'taiga-cards-desktop': 33,
  'taiga-cards-list': 87,
  'tool-cards': 38,
  'tool-cards-desktop': 35,
  'tool-cards-top': 13,
  trash: 17,
  'url-request': 13,
  'xlsx-viewer': 14,
  'xlsx-viewer-desktop': 5
};
const PALETTE_KEYS = PALETTES.map((palette) => palette.key);
// Экраны, где видны все пары из F-18: активная вкладка навигации, пилюля действующей политики
// и состояние задачи плана. Остальные экраны идут в палитре по умолчанию.
const PALETTE_SCREENS = ['01-sessions', 'routing-editor', 'plan-cards'];

type AxeResults = Awaited<ReturnType<AxeBuilder['analyze']>>;

interface Audit { rule: string; where: string; count: number; example: string }

function contrastNodes(results: AxeResults): number {
  return [...results.passes, ...results.violations, ...results.incomplete]
    .filter((result) => result.id === 'color-contrast')
    .reduce((total, result) => total + result.nodes.length, 0);
}

function contrastProblems(results: AxeResults, label: string): string[] {
  return results.violations
    .filter((violation) => violation.id === 'color-contrast')
    .flatMap((violation) => violation.nodes.slice(0, 3).map((node) => `${label}: ${node.target.join(' ')} — ${node.any[0]?.message ?? violation.help}`));
}

/**
 * Известные случаи мягких проверок. Это не разрешение на дефект, а снимок того, что осталось:
 * новый случай или лишнее повторение того же узла валит тест, а исчезнувший повод править эталон
 * не требует — правка ради уменьшения счётчика ломала бы прогон из-за разницы браузеров.
 */
interface SoftBaselineEntry {
  screen: string;
  rule: string;
  where: string;
  count: number;
  reason: string;
}

const SOFT_BASELINE: SoftBaselineEntry[] = JSON.parse(readFileSync(resolve(process.cwd(), 'e2e/parity/geometry-baseline.json'), 'utf8'));

/** Мягкие находки, которых нет в эталоне или которых стало больше, чем записано. */
function newSoftCases(screen: string, found: Finding[]): Finding[] {
  const known = new Map(
    SOFT_BASELINE.filter((entry) => entry.screen === screen).map((entry) => [`${entry.rule}|${entry.where}`, entry.count])
  );
  return found.filter((item) => item.count > (known.get(`${item.rule}|${item.where}`) ?? 0));
}

/**
 * Продукт держит html нулевой высоты: оболочка `position: fixed`, а
 * `html:not(:has([data-harness]))` прячет переполнение. axe считает потомков нулевого клипующего
 * предка скрытыми, поэтому на время аудита поднимаем корень. Меняем только страницу теста:
 * base.css остаётся продуктовым поведением.
 */
async function liftRootClipping(page: Page): Promise<void> {
  await page.evaluate(() => {
    document.documentElement.style.setProperty('overflow', 'visible', 'important');
    document.documentElement.style.setProperty('min-height', '100dvh');
    document.body.style.setProperty('min-height', '100dvh');
    // Лист делает оболочку inert, и axe выбрасывает весь лист вместе с фоном. Снимаем только на время аудита.
    document.getElementById('app-shell')?.removeAttribute('inert');
  });
}

/** Ставит палитру и тему атрибутами корня, как theme.svelte.ts, и ждёт устоявшихся цветов. */
async function setVariant(page: Page, palette: string, theme: Theme): Promise<void> {
  await page.evaluate(({ palette, theme }) => {
    const root = document.documentElement;
    root.dataset.themeSwitching = '';
    root.dataset.palette = palette;
    root.dataset.theme = theme;
  }, { palette, theme });
  await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
  await page.evaluate(() => { delete document.documentElement.dataset.themeSwitching; });
  await page.waitForFunction(() => document.getAnimations().every((animation) => {
    const timing = animation.effect?.getComputedTiming();
    const finite = typeof timing?.duration === 'number' && Number.isFinite(timing.duration) && timing.iterations !== Infinity;
    return animation.playState !== 'running' || !finite;
  }));
}

/** Язык оформления страницы: формы, цвета, цели нажатия, шрифт. Возвращает расхождения с подписью экрана. */
async function visualLanguageProblems(page: Page, label: string): Promise<string[]> {
  // Срез не должен попасть в середину движения: переход цвета или размера даёт промежуточное
  // значение, которого нет среди токенов. Бесконечные пульсации не ждём — у них нет конца.
  await page.waitForFunction(() => document.getAnimations().every((animation) => {
    const timing = animation.effect?.getComputedTiming();
    const finite = typeof timing?.duration === 'number' && Number.isFinite(timing.duration) && timing.iterations !== Infinity;
    return animation.playState !== 'running' || !finite;
  }));
  const found: Audit[] = await page.evaluate((colorTokens) => {
    const rules = new Map<string, Audit>();
    const note = (rule: string, el: Element, detail: string) => {
      const where = `${el.tagName.toLowerCase()}${el.className && typeof el.className === 'string' ? '.' + el.className.trim().split(/\s+/).filter((name) => !name.startsWith('svelte-')).join('.') : ''}`;
      const key = `${rule}|${where}`;
      const known = rules.get(key);
      if (known) known.count += 1;
      else rules.set(key, { rule, where, count: 1, example: detail });
    };
    const root = document.documentElement;
    const probe = document.createElement('div');
    probe.style.cssText = 'position:fixed;left:-9999px;top:0;width:100px;height:100px';
    document.body.append(probe);
    const radius = (token: string) => { probe.style.borderRadius = `var(${token})`; return getComputedStyle(probe).borderTopLeftRadius; };
    const px = Number.parseFloat(getComputedStyle(root).getPropertyValue('--target'));
    const target = (() => { probe.style.width = 'var(--target)'; return Number.parseFloat(getComputedStyle(probe).width) || px; })();
    probe.style.width = '100px';
    const allowedRadii = new Set(['--radius-sm', '--radius', '--radius-card', '--radius-sheet', '--radius-composer', '--radius-pill', '--radius-round'].map(radius));
    allowedRadii.add('0px');
    // Цвет текста сверяется с токенами своей палитры: предпросмотр темы задаёт data-palette на своём узле.
    const colorSets = new Map<Element, Set<string>>();
    const colorSet = (el: Element) => {
      const scope = el.closest('[data-palette]') ?? root;
      let found = colorSets.get(scope);
      if (!found) {
        const swatch = document.createElement('span');
        swatch.hidden = true;
        scope.append(swatch);
        found = new Set(colorTokens.map((token) => { swatch.style.color = `var(${token})`; return getComputedStyle(swatch).color; }));
        swatch.remove();
        colorSets.set(scope, found);
      }
      return found;
    };
    const visible = (el: Element) => {
      const rect = el.getBoundingClientRect();
      if (rect.width === 0 || rect.height === 0) return false;
      const style = getComputedStyle(el);
      return style.visibility !== 'hidden' && style.display !== 'none' && style.opacity !== '0';
    };
    const expectRadius = (selector: string, token: string, corners?: 'top') => {
      for (const el of document.querySelectorAll(selector)) {
        if (!visible(el)) continue;
        const style = getComputedStyle(el);
        const want = radius(token);
        const got = corners === 'top' ? [style.borderTopLeftRadius, style.borderTopRightRadius] : [style.borderTopLeftRadius, style.borderTopRightRadius, style.borderBottomLeftRadius, style.borderBottomRightRadius];
        if (got.some((value) => value !== want)) note(`shape ${selector} must use ${token}`, el, got.join(' '));
      }
    };
    for (const bar of document.querySelectorAll('.topbar')) {
      const style = getComputedStyle(bar);
      if (style.backgroundColor !== 'rgba(0, 0, 0, 0)' || style.borderTopLeftRadius !== '0px') {
        note('topbar-has-backing', bar, `${style.backgroundColor} ${style.borderTopLeftRadius}`);
      }
    }
    const dock = document.querySelector('.dock nav');
    if (dock && getComputedStyle(dock).backgroundColor !== 'rgba(0, 0, 0, 0)') {
      note('dock-has-backing', dock, getComputedStyle(dock).backgroundColor);
    }
    expectRadius('article.card', '--radius-card');
    expectRadius('.capsule', '--radius-composer');
    expectRadius('.send', '--radius-round');
    expectRadius('.fab', '--radius-round');
    expectRadius('dialog[open]:has(.handle)', '--radius-sheet', 'top');
    expectRadius('pre:not(.out pre)', '--radius-sm');
    // Вывод инструмента без своего фона: скругление с overflow срезало бы углы текста.
    for (const pre of document.querySelectorAll('.out pre')) {
      if (visible(pre) && getComputedStyle(pre).borderTopLeftRadius !== '0px') {
        note('tool-output-rounded', pre, getComputedStyle(pre).borderTopLeftRadius);
      }
    }

    for (const el of document.querySelectorAll('body *')) {
      if (!visible(el) || el === probe) continue;
      const style = getComputedStyle(el);
      const inSvg = el instanceof SVGElement;
      if (style.boxShadow !== 'none' && !el.matches('.st-dot')) note('box-shadow must be none', el, style.boxShadow);
      if (style.backgroundImage !== 'none') note('background-image must be none', el, style.backgroundImage);
      const borders = [style.borderTopWidth, style.borderRightWidth, style.borderBottomWidth, style.borderLeftWidth];
      const chart = el.matches('[data-series], [role="img"] *, svg *') || el.closest('[data-series]') !== null;
      const focused = el === document.activeElement;
      if (!chart && !focused && borders.some((width) => width !== '0px') && !(el instanceof HTMLInputElement && ['checkbox', 'radio'].includes(el.type))) note('border only for focus and chart outlines', el, `${borders.join(' ')} «${(el.textContent ?? '').trim().slice(0, 30)}»`);
      if (!inSvg && !el.matches('[data-series], .bar')) {
        for (const corner of [style.borderTopLeftRadius, style.borderTopRightRadius, style.borderBottomLeftRadius, style.borderBottomRightRadius]) {
          if (!allowedRadii.has(corner)) note('radius must be a token', el, corner);
        }
      }
      if (el instanceof HTMLInputElement && el.type === 'file') continue;
      const interactive = el.matches('a[href], button, select, textarea, input:not([type="hidden"]), [role="button"], [role="menuitem"], [role="tab"], [role="radio"], [role="switch"], summary');
      if (interactive && !el.closest('.sr-only, [hidden], [inert]') && !(el instanceof HTMLAnchorElement && el.closest('p, li, span, label, td') && el.parentElement?.tagName !== 'NAV')) {
        const holder = el instanceof HTMLInputElement && ['checkbox', 'radio'].includes(el.type) ? (el.closest('label') ?? el) : el;
        const box = holder.getBoundingClientRect();
        // Цель касания, вынесенная из раскладки в абсолютный ::after, добавляет к рамке свои выносы.
        const hit = getComputedStyle(holder, '::after');
        const out = (side: string) => (hit.content !== 'none' && hit.position === 'absolute' ? Math.max(0, -(parseFloat(side) || 0)) : 0);
        const rect = { width: box.width + out(hit.left) + out(hit.right), height: box.height + out(hit.top) + out(hit.bottom) };
        // Под точным указателем помеченным плотным спискам хватает минимума WCAG 2.5.8 (24px).
        const fine = el.closest('[data-fine-targets]') && matchMedia('(pointer: fine)').matches;
        const need = fine ? 24 : target;
        if (rect.width < need - 0.5 || rect.height < need - 0.5) note(`interactive target >= ${fine ? '24px (pointer: fine)' : `--target (${target}px)`}`, el, `${Math.round(rect.width)}x${Math.round(rect.height)} «${(el.textContent ?? '').trim().slice(0, 30)}»`);
      }
      if ([...el.childNodes].some((node) => node.nodeType === Node.TEXT_NODE && (node.textContent ?? '').trim() !== '')) {
        if (!colorSet(el).has(style.color)) note('text colour must be a token', el, style.color);
      }
    }
    probe.remove();
    return [...rules.values()];
  }, COLOR_TOKENS);
  return found.map((item) => `${item.rule} | ${item.where}: ${label}: x${item.count}, ${item.example}`);
}

// Один заход на экран отдаёт обе проверки: axe и инварианты языка смотрят одну и ту же страницу,
// а заходов у маршрута с двумя темами было вдвое больше, чем нужно. Каждая пара «экран, тема»
// остаётся отдельным тестом: проверки идут параллельно, а падение называет экран.
for (const screen of AUDITED) {
  for (const theme of THEMES) {
    test(`visual_language_and_axe_hold_on_every_route_in_both_themes: ${screen.name} ${theme}`, async ({ page }) => {
      test.setTimeout(120_000);
      const label = `${screen.name} (${theme})`;
      await bootScreen(page, screen, theme);
      // Последний щелчок оставляет указатель над строкой: аудит смотрит покой, а не случайный :hover.
      await page.mouse.move(0, 0);
      const results = await new AxeBuilder({ page }).analyze();
      const serious = results.violations
        .filter((violation) => violation.impact === 'serious' || violation.impact === 'critical')
        .map((violation) => `${violation.id} (${violation.impact}): ${violation.nodes.slice(0, 3).map((node) => node.target.join(' ')).join(' | ')}`);
      expect(serious, label).toEqual([]);
      const problems = await visualLanguageProblems(page, label);
      expect(problems, problems.join('\n')).toEqual([]);
      // Геометрический обход идёт по этой же странице: второй заход на экран стоил бы столько же,
      // сколько весь остальной аудит, а инварианты раскладки от темы не зависят.
      const geometry = await geometryAudit(page);
      const geoLabel = (item: Finding) => `${label}: ${item.rule} x${item.count} — ${item.where}: ${item.detail}`;
      expect(geometry.hard.map(geoLabel), 'геометрия раскладки').toEqual([]);
      const fresh = newSoftCases(screen.name, geometry.soft);
      expect(fresh.map(geoLabel), 'новые случаи мягких проверок: допишите их в e2e/parity/geometry-baseline.json с причиной').toEqual([]);
      // color-contrast разбирается отдельно: продукт держит корень обрезанным, и axe молча пропускает текст.
      await liftRootClipping(page);
      const contrastResults = await new AxeBuilder({ page }).withRules(['color-contrast']).analyze();
      const evaluated = contrastNodes(contrastResults);
      const floor = CONTRAST_FLOOR[screen.name];
      expect(floor, `${label}: no contrast floor`).toBeDefined();
      expect(evaluated, `${label}: color-contrast evaluated ${evaluated} nodes, floor ${floor}`).toBeGreaterThanOrEqual(floor!);
      const contrast = contrastProblems(contrastResults, label);
      expect(contrast, contrast.join('\n')).toEqual([]);
    });
  }
}

// Полная матрица «экран × палитра × тема» на всех маршрутах слишком дорога для обычного прогона:
// каждый экран в палитре по умолчанию уже прошёл выше. Здесь каждая палитра и обе темы проверяются
// на нескольких экранах, где видны пары из F-18. Токены не зависят от движка, поэтому матрица
// идёт один раз, в проекте phone.
for (const name of PALETTE_SCREENS) {
  const screen = SCREENS.find((item) => item.name === name)!;
  test(`color_contrast_holds_in_every_palette_and_both_themes: ${name}`, async ({ page }, info) => {
    test.skip(info.project.name !== 'phone', 'Палитры проверяются в одном браузере: токены не зависят от движка.');
    test.setTimeout(240_000);
    await bootScreen(page, screen, screen.theme);
    await page.mouse.move(0, 0);
    await liftRootClipping(page);
    const floor = CONTRAST_FLOOR[name]!;
    for (const palette of PALETTE_KEYS) {
      for (const theme of THEMES) {
        await setVariant(page, palette, theme);
        const results = await new AxeBuilder({ page }).withRules(['color-contrast']).analyze();
        const label = `${name} ${palette} (${theme})`;
        const evaluated = contrastNodes(results);
        expect(evaluated, `${label}: color-contrast evaluated ${evaluated} nodes, floor ${floor}`).toBeGreaterThanOrEqual(floor);
        const problems = contrastProblems(results, label);
        expect(problems, problems.join('\n')).toEqual([]);
      }
    }
  });
}

for (const screen of AUDITED.filter((item) => item.viewport.width < 1000)) {
  for (const width of [360, 320]) {
    test(`no_horizontal_scroll_at_360_320_and_text_200_percent_on_every_route: ${screen.name} ${width}`, async ({ page }) => {
      test.setTimeout(120_000);
      await bootScreen(page, screen, 'dark', { width, height: 740 });
      // Текст 200 %: растёт только размер шрифта корня, вьюпорт не меняется.
      await page.evaluate(() => document.documentElement.style.setProperty('font-size', '200%', 'important'));
      await page.waitForTimeout(150);
      const overflow = await page.evaluate(() => {
        const doc = document.documentElement;
        const wide = [...document.body.querySelectorAll<HTMLElement>('*')]
          .filter((el) => el.getBoundingClientRect().right > doc.clientWidth + 1 && el.getBoundingClientRect().width > 0)
          .filter((el) => { let node: HTMLElement | null = el.parentElement; while (node) { if (getComputedStyle(node).overflowX !== 'visible' && node !== document.body && node !== doc) return false; node = node.parentElement; } return true; })
          .slice(0, 3)
          .map((el) => `${el.tagName.toLowerCase()}.${el.className}`);
        return { scrollWidth: doc.scrollWidth, clientWidth: doc.clientWidth, wide };
      });
      expect(overflow.scrollWidth, `${screen.name} at ${width}px with 200% text: ${overflow.wide.join(', ')}`).toBeLessThanOrEqual(overflow.clientWidth);
    });
  }
}

// Эталон мягких проверок — часть гейта: запись без причины или с опечаткой в имени экрана молча
// ничего не разрешает, а через месяц выглядит как разрешение. Проверяем сам файл, без захода на страницу.
test('soft_geometry_baseline_entries_name_a_known_screen_and_a_reason', () => {
  const screens = new Set(SCREENS.map((screen) => screen.name));
  const problems = SOFT_BASELINE.flatMap((entry) => {
    const where = `${entry.screen}: ${entry.rule} — ${entry.where}`;
    if (!screens.has(entry.screen)) return [`${where}: нет такого экрана`];
    if (!entry.reason?.trim()) return [`${where}: нет причины`];
    if (!Number.isInteger(entry.count) || entry.count < 1) return [`${where}: count должен быть целым и не меньше 1`];
    return [];
  });
  expect(problems, problems.join('\n')).toEqual([]);
});

test('keyboard_only_flow_reaches_send_sheets_and_back', async ({ page }) => {
  test.setTimeout(120_000);
  const chat = SCREENS.find((screen) => screen.name === '02-chat')!;
  await bootScreen(page, chat);
  const focused = () => page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    return el ? `${el.getAttribute('aria-label') ?? el.textContent?.trim().slice(0, 40) ?? ''}|${el.tagName}` : '';
  });
  const visibleFocus = () => page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el || el === document.body) return false;
    const style = getComputedStyle(el);
    if (style.outlineStyle !== 'none' && parseFloat(style.outlineWidth) > 0 || style.boxShadow !== 'none') return true;
    // Рамка фокуса поля ввода живёт на капсуле целиком.
    const capsule = el.closest('.capsule');
    return el instanceof HTMLTextAreaElement && capsule !== null && getComputedStyle(capsule).outlineStyle !== 'none';
  });

  // Табуляция доходит до поля ввода и кнопки «Отправить», фокус везде виден.
  const seen: string[] = [];
  for (let step = 0; step < 60; step++) {
    await page.keyboard.press('Tab');
    seen.push(await focused());
    expect(await visibleFocus(), `фокус виден на «${seen.at(-1)}»`).toBe(true);
    if (seen.at(-1)!.startsWith('Отправить|')) break;
  }
  expect(seen.some((name) => name.startsWith('Сообщение|TEXTAREA'))).toBe(true);
  expect(seen.at(-1)).toMatch(/^Отправить\|BUTTON/);

  // Меню сессии открывается с клавиатуры, Escape закрывает его и возвращает фокус на кнопку.
  await page.getByRole('button', { name: 'Меню сессии' }).focus();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('menu')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('menu')).toHaveCount(0);
  await expect.poll(focused).toMatch(/^Меню сессии\|BUTTON/);

  // «Отправить позже» открывается сочетанием, фокус уходит в окно и возвращается.
  const composer = page.getByRole('textbox', { name: 'Сообщение' });
  await composer.focus();
  await composer.fill('черновик');
  await page.keyboard.press('Control+Shift+Enter');
  const dialog = page.getByRole('dialog').last();
  await expect(dialog).toBeVisible();
  // Лист на телефоне забирает фокус сам; всплывающее окно на рабочем столе достаётся клавишей Tab.
  let inside = false;
  for (let step = 0; step < 14 && !inside; step++) {
    inside = await dialog.evaluate((el) => el.contains(document.activeElement));
    if (!inside) await page.keyboard.press('Tab');
  }
  expect(inside, 'фокус добирается до окна «Отправить позже»').toBe(true);
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(composer).toBeFocused();

  // Окно «Сведения о ходе»: открыть Enter, Tab держит фокус внутри, Escape возвращает на кнопку.
  const info = SCREENS.find((screen) => screen.name === '04-info-sheet')!;
  await bootScreen(page, info);
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  const open = page.getByRole('button', { name: 'Сведения о ходе' }).first();
  await open.focus();
  await page.keyboard.press('Enter');
  const sheet = page.getByRole('dialog', { name: 'Об ответе' });
  await expect(sheet).toBeVisible();
  for (let step = 0; step < 12; step++) {
    await page.keyboard.press('Tab');
    expect(await sheet.evaluate((el) => el.contains(document.activeElement)), 'фокус не покидает окно').toBe(true);
  }
  await page.keyboard.press('Escape');
  await expect(sheet).toHaveCount(0);
  await expect(open).toBeFocused();
});
