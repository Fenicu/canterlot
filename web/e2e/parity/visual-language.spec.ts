import AxeBuilder from '@axe-core/playwright';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { expect, test } from '../ui/fixtures';
import { bootScreen, SCREENS, type Theme } from './states';

const THEMES: Theme[] = ['dark', 'light'];
const COLOR_TOKENS = [...readFileSync(resolve(process.cwd(), 'src/lib/styles/tokens.css'), 'utf8').matchAll(/(--[a-z0-9-]+):\s*(?:#|rgb|hsl|oklch|color-mix)/g)].map((match) => match[1]!);

// Каждая пара «экран, тема» отдельным тестом: проверки идут параллельно, а падение называет экран.
for (const screen of SCREENS) {
  for (const theme of THEMES) {
    test(`axe_has_no_serious_violations_on_every_route_in_both_themes: ${screen.name} ${theme}`, async ({ page }) => {
      test.setTimeout(120_000);
      await bootScreen(page, screen, theme);
      const results = await new AxeBuilder({ page }).analyze();
      const serious = results.violations
        .filter((violation) => violation.impact === 'serious' || violation.impact === 'critical')
        .map((violation) => `${violation.id} (${violation.impact}): ${violation.nodes.slice(0, 3).map((node) => node.target.join(' ')).join(' | ')}`);
      expect(serious, `${screen.name} (${theme})`).toEqual([]);
    });
  }
}

for (const screen of SCREENS.filter((item) => item.viewport.width < 1000)) {
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
  expect(await focused()).toMatch(/^Меню сессии\|BUTTON/);

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

interface Audit { rule: string; where: string; count: number; example: string }

// Каждая пара «экран, тема» отдельным тестом: проверки идут параллельно, а падение называет экран.
for (const screen of SCREENS) {
  for (const theme of THEMES) {
    test(`visual_language_invariants_hold_on_every_screen: ${screen.name} ${theme}`, async ({ page }) => {
      test.setTimeout(120_000);
      const problems: string[] = [];
      const observed = new Map<string, string>();
      await bootScreen(page, screen, theme);
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
        const colorSet = new Set(colorTokens.map((token) => { probe.style.color = `var(${token})`; return getComputedStyle(probe).color; }));
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
            const rect = holder.getBoundingClientRect();
            if (rect.width < target - 0.5 || rect.height < target - 0.5) note(`interactive target >= --target (${target}px)`, el, `${Math.round(rect.width)}x${Math.round(rect.height)} «${(el.textContent ?? '').trim().slice(0, 30)}»`);
          }
          if ([...el.childNodes].some((node) => node.nodeType === Node.TEXT_NODE && (node.textContent ?? '').trim() !== '')) {
            if (!colorSet.has(style.color)) note('text colour must be a token', el, style.color);
          }
        }
        probe.remove();
        return [...rules.values()];
      }, COLOR_TOKENS);
      for (const item of found) observed.set(`${item.rule} | ${item.where}`, `${screen.name} (${theme}): x${item.count}, ${item.example}`);
      for (const [key, example] of observed) problems.push(`${key}: ${example}`);
      expect(problems, problems.join('\n')).toEqual([]);
    });
  }
}
