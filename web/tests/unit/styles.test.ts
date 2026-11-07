import { test, expect } from 'vitest';
import { readFileSync, existsSync, readdirSync } from 'node:fs';
import postcss from 'postcss';
import { variants, contrast, rootPalette, COLOR_TOKENS } from './contrast';
import { PALETTES, DEFAULT_PALETTE } from '../../src/lib/palettes';
const text = readFileSync('src/lib/styles/tokens.css', 'utf8');
const palettes = variants(text);
const byName = (palette: string, theme: 'dark' | 'light') => palettes.find(v => v.palette === palette && v.theme === theme)!;

const AQUAMARINE = {
  dark: {
    'bg': '#183135', 'chat-bg': '#152d31', 'surface': '#1d383b', 'raised': '#1d4044',
    'hover': '#235451', 'selected': '#1f5457', 'text': '#e5f3f4', 'secondary': '#bed2d3',
    'muted': '#adc7c8', 'time': '#adc7c8', 'line': '#2e5053', 'control-line': '#415d5f',
    'chip': '#415d5f', 'chip-text': '#e5f3f4', 'badge': '#5c8282', 'accent': '#00f4ee',
    'accent-link': '#56fffc', 'accent-ink': '#183135', 'accent-soft': '#235451',
    'success': '#91d8bf', 'warning': '#e7bf84', 'warning-soft': '#3c3c31',
    'danger': '#eda99d', 'danger-soft': '#483b39', 'blue': '#a8cbd8', 'code': '#142d31',
    'scrim': '#132b2e', 'chart-secondary': '#9bbac8', 'transparent': 'transparent',
    'popover': '#235451', 'popover-hover': '#1d4044'
  },
  light: {
    'bg': '#e7f2ee', 'chat-bg': '#e1eeea', 'surface': '#f4faf7', 'raised': '#d3e8e2',
    'hover': '#bdddd5', 'selected': '#c2e1d9', 'text': '#183a3d', 'secondary': '#365d5f',
    'muted': '#3c6261', 'time': '#3c6261', 'line': '#c2d9d2', 'control-line': '#adcac2',
    'chip': '#c6dfd7', 'chip-text': '#254f50', 'badge': '#729e96', 'accent': '#006966',
    'accent-link': '#006360', 'accent-ink': '#f2fffc', 'accent-soft': '#c3e6de',
    'success': '#256a53', 'warning': '#7d531d', 'warning-soft': '#f0e7d4',
    'danger': '#9d493e', 'danger-soft': '#f4e1db', 'blue': '#3d6579', 'code': '#dfede7',
    'scrim': '#ccdfd7', 'chart-secondary': '#50778a', 'transparent': 'transparent',
    'popover': '#cce6de', 'popover-hover': '#bdddd5'
  }
} as const;

test('tokens_match_the_approved_mockup_when_available', () => {
  const path = '../.superpowers/design/mockups/tokens.css';
  if (!existsSync(path)) return;
  expect(text.split('\n/* pwa additions */')[0]).toBe(readFileSync(path, 'utf8'));
});

test('aquamarine_resolves_to_the_unchanged_main_palette', () => {
  for (const theme of ['dark', 'light'] as const) {
    const actual = byName('aquamarine', theme).tokens;
    for (const [name, value] of Object.entries(AQUAMARINE[theme])) {
      expect(actual[`--${name}`], `${theme} --${name}`).toBe(value);
    }
  }
});

test('aquamarine_syntax_tokens_keep_the_old_highlight_colors', () => {
  const dark = byName('aquamarine', 'dark').tokens;
  expect(dark['--syn-keyword']).toBe(dark['--accent']);
  expect(dark['--syn-control']).toBe(dark['--accent']);
  expect(dark['--syn-string']).toBe(dark['--success']);
  expect(dark['--syn-function']).toBe(dark['--blue']);
  expect(dark['--syn-type']).toBe(dark['--success']);
  expect(dark['--syn-comment']).toBe(dark['--muted']);
  expect(dark['--syn-number']).toBe(dark['--warning']);
  expect(dark['--syn-variable']).toBe(dark['--text']);
  expect(dark['--syn-symbol']).toBe(dark['--accent-link']);
  const light = byName('aquamarine', 'light').tokens;
  expect(light['--syn-keyword']).toBe(light['--accent']);
  expect(light['--syn-control']).toBe(light['--accent']);
  expect(light['--syn-string']).toBe(light['--success']);
  expect(light['--syn-function']).toBe(light['--blue']);
  expect(light['--syn-type']).toBe(light['--success']);
  expect(light['--syn-comment']).toBe(light['--muted']);
  expect(light['--syn-number']).toBe(light['--warning']);
  expect(light['--syn-variable']).toBe(light['--text']);
  expect(light['--syn-symbol']).toBe(light['--accent-link']);
});

test('every_palette_defines_the_full_color_token_set_in_both_modes', () => {
  // Набор задаёт аквамарин, исходная палитра: у каждой новой ровно те же цветовые токены и ничего лишнего.
  const reference = Object.keys(byName('aquamarine', 'dark').tokens).sort();
  expect(new Set(reference)).toEqual(new Set(COLOR_TOKENS.map(name => `--${name}`)));
  expect(palettes.map(v => `${v.palette}/${v.theme}`).sort()).toEqual(PALETTES.flatMap(p => [`${p.key}/dark`, `${p.key}/light`]).sort());
  for (const variant of palettes) {
    const label = `${variant.palette}/${variant.theme}`;
    expect(Object.keys(variant.tokens).sort(), label).toEqual(reference);
    for (const [name, value] of Object.entries(variant.tokens)) expect(/^#[\da-f]{6}$/i.test(value) || value === 'transparent', `${label} ${name}`).toBe(true);
  }
});

test('root_fallback_is_the_default_palette', () => {
  expect(rootPalette(text)).toBe(DEFAULT_PALETTE);
  // Голый `:root` держит только нецветовые токены: иначе он перебил бы палитру без атрибутов.
  postcss.parse(text).walkRules(rule => {
    if (rule.parent?.type !== 'root' || rule.selector.trim() !== ':root') return;
    rule.walkDecls(d => expect(COLOR_TOKENS.map(name => `--${name}`), d.prop).not.toContain(d.prop));
  });
});

const textPairs: [string, string[]][] = [
  ['text', ['bg', 'chat-bg', 'surface', 'raised', 'selected', 'accent-soft', 'code', 'popover']],
  ...['secondary', 'muted', 'time'].flatMap(fg => [
    [fg, ['bg', 'chat-bg', 'surface', 'raised', 'selected']] as [string, string[]]
  ]),
  ['chip-text', ['chip']],
  ['accent-ink', ['accent']],
  ['accent-link', ['bg', 'surface', 'raised']],
  ...['success', 'warning', 'danger'].flatMap(fg => [
    [fg, ['bg', 'surface', `${fg}-soft`]] as [string, string[]]
  ])
];

test('popover_surface_is_an_opaque_colour_in_every_palette_and_mode', () => {
  for (const variant of palettes) {
    expect(variant.tokens['--popover']!, `${variant.palette}/${variant.theme}`).toMatch(/^#[\da-f]{6}$/i);
  }
});

test('text_pairs_meet_wcag_aa_in_every_palette_and_mode', () => {
  for (const variant of palettes) {
    const label = `${variant.palette}/${variant.theme}`;
    for (const [fg, backgrounds] of textPairs) for (const bg of backgrounds) {
      expect(contrast(variant.tokens[`--${fg}`]!, variant.tokens[`--${bg}`]!), `${label} ${fg}/${bg}`).toBeGreaterThanOrEqual(4.5);
    }
    for (const name of COLOR_TOKENS.filter(name => name.startsWith('syn-'))) {
      expect(contrast(variant.tokens[`--${name}`]!, variant.tokens['--code']!), `${label} ${name}/code`).toBeGreaterThanOrEqual(4.5);
    }
    for (const line of ['line', 'control-line']) {
      expect(contrast(variant.tokens[`--${line}`]!, variant.tokens['--surface']!), `${label} ${line}/surface`).toBeGreaterThanOrEqual(1.3);
    }
  }
});

test('office_viewers_meet_contrast_in_every_palette_and_mode', () => {
  // Просмотрщики Word и Excel: шапка и полосы сетки, тело и липкий столбец, ссылки и линии-разделители.
  // Пары те же, что видит пользователь на экранах docx-viewer и xlsx-viewer.
  const pairs: [string, string, string, number][] = [
    ['text', 'raised', 'sheet header and zebra rows', 4.5],
    ['text', 'surface', 'sheet body and sticky column', 4.5],
    ['text', 'surface', 'docx body text', 4.5],
    ['accent-link', 'surface', 'docx link on body', 4.5],
    ['accent-link', 'raised', 'docx link on table stripe', 4.5],
    ['line', 'surface', 'table and grid separators', 1.3]
  ];
  for (const variant of palettes) {
    const label = `${variant.palette}/${variant.theme}`;
    for (const [fg, bg, what, minimum] of pairs) {
      expect(contrast(variant.tokens[`--${fg}`]!, variant.tokens[`--${bg}`]!), `${label} ${what}: ${fg}/${bg}`).toBeGreaterThanOrEqual(minimum);
    }
  }
});

test('high_contrast_palette_keeps_text_at_seven_to_one', () => {
  for (const theme of ['dark', 'light'] as const) {
    const tokens = byName('high-contrast', theme).tokens;
    for (const [fg, backgrounds] of textPairs) for (const bg of backgrounds) {
      expect(contrast(tokens[`--${fg}`]!, tokens[`--${bg}`]!), `${theme} ${fg}/${bg}`).toBeGreaterThanOrEqual(7);
    }
  }
});

test('reduced_motion_zeroes_all_duration_tokens', () => {
  const reduced = text.slice(text.lastIndexOf('@media (prefers-reduced-motion: reduce)'));
  const root: Record<string, string> = {};
  postcss.parse(text).walkRules(rule => { if (rule.parent?.type === 'root' && rule.selector.trim() === ':root') rule.walkDecls(d => { root[d.prop] = d.value; }); });
  const durations = Object.keys(root).filter(n => n.startsWith('--duration-'));
  expect(durations).toContain('--duration-theme-reveal');
  for (const name of durations) {
    expect(root[name]).toContain('var(--motion-scale)');
    expect(reduced).toContain(`${name}: 0ms`);
  }
});

test('disabled_button_requires_a_reason', () => {
  const walk = (path: string): string[] => readdirSync(path, { withFileTypes: true }).flatMap(e => e.isDirectory() ? walk(`${path}/${e.name}`) : [`${path}/${e.name}`]);
  for (const path of walk('src').filter(p => p.endsWith('.svelte') && !p.endsWith('/Button.svelte'))) expect(readFileSync(path, 'utf8'), path).not.toMatch(/<button\b[^>]*\bdisabled\b/);
});

test('pwa_design_doc_records_removal_of_check_icon_and_unscaled_countdown', () => {
  const doc = readFileSync('../docs/design/pwa.md', 'utf8');
  expect(doc).not.toMatch(/Регистрация `check` остаётся/);
  expect(doc).toMatch(/нет иконки `check`/i);
});

test('separator_spacing_survives_svelte_block_trimming', () => {
  // Svelte срезает пробелы, в том числе &#32;, в начале и в конце блока: «1.0.0{#if x} · хаб» выводится как «1.0.0· хаб».
  // Пробел ставится перед блоком, снаружи, или разделитель собирается в скрипте.
  const opening = /\{[#:](?:if|else|each|key|await|then|catch)\b[^}]*\}(?:[ \t]|&#32;)+[^\s<{]/;
  const closing = /[^\s>}](?:[ \t]|&#32;)+\{[/:](?:if|else|each|key|await|then|catch)\b/;
  for (const bad of ['<b>1</b>{#if x} · хаб{/if}', '{#if x}&#32;· хаб{/if}', '{:else} · b', 'a · {/if}']) expect(opening.test(bad) || closing.test(bad), bad).toBe(true);
  expect(opening.test("<b>1</b> {#if x}· хаб <b>2</b>{/if}")).toBe(false);
  const walk = (path: string): string[] => readdirSync(path, { withFileTypes: true }).flatMap(e => e.isDirectory() ? walk(`${path}/${e.name}`) : [`${path}/${e.name}`]);
  for (const path of walk('src').filter(p => p.endsWith('.svelte'))) {
    const markup = readFileSync(path, 'utf8').replace(/<(script|style)[^>]*>[\s\S]*?<\/\1>/g, '');
    expect(markup.match(opening)?.[0], path).toBeUndefined();
    expect(markup.match(closing)?.[0], path).toBeUndefined();
  }
});

test('selected_fill_meets_non_text_contrast_in_all_variants', () => {
  // Вкладки групп лежат на --bg, сегменты на --raised: заливка выбора отличима от них на 3:1.
  for (const variant of palettes) {
    const label = `${variant.palette}/${variant.theme}`;
    for (const bg of ['bg', 'surface', 'raised']) {
      expect(contrast(variant.tokens['--accent']!, variant.tokens[`--${bg}`]!), `${label} accent/${bg}`).toBeGreaterThanOrEqual(3);
    }
  }
  for (const path of ['src/lib/screens/sessions/GroupTabs.svelte', 'src/lib/ui/Segmented.svelte']) expect(readFileSync(path, 'utf8'), path).toMatch(/(?:aria-selected='true'\]|:checked \+ span) \{ background: var\(--accent\);/);
});
