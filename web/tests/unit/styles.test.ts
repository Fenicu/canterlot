import { test, expect } from 'vitest';
import { readFileSync, existsSync, readdirSync } from 'node:fs';
import { themes, contrast } from './contrast';
const text = readFileSync('src/lib/styles/tokens.css', 'utf8');
const { dark, light } = themes(text);
test('tokens_match_the_approved_mockup_when_available', () => {
  const path = '../.superpowers/design/mockups/tokens.css';
  if (!existsSync(path)) return;
  expect(text.split('\n/* pwa additions */')[0]).toBe(readFileSync(path, 'utf8'));
});
test('light_theme_overrides_every_color_token', () => {
  for (const [name, value] of Object.entries(dark)) {
    if (/^#|^transparent$/.test(value)) expect(light[name], name).toBeDefined();
    else expect(light[name], name).toBeUndefined();
  }
});
test('text_pairs_meet_wcag_aa_in_both_themes', () => {
  const pairs: [string, string[]][] = [
    ['text', ['bg', 'chat-bg', 'surface', 'raised', 'selected', 'accent-soft', 'code']],
    ...['secondary', 'muted', 'time'].map(n => [n, ['bg', 'surface', 'raised', 'selected']] as [string, string[]]),
    ['chip-text', ['chip']], ['accent-ink', ['accent']], ['accent-link', ['bg', 'surface', 'raised']],
    ['warning', ['surface', 'warning-soft']], ['danger', ['surface', 'danger-soft']], ['success', ['surface']]
  ];
  for (const palette of [dark, { ...dark, ...light }]) for (const [fg, backgrounds] of pairs) for (const bg of backgrounds) expect(contrast(palette[`--${fg}`]!, palette[`--${bg}`]!), `${fg}/${bg}`).toBeGreaterThanOrEqual(4.5);
});
test('reduced_motion_zeroes_all_duration_tokens', () => {
  const reduced = text.slice(text.lastIndexOf('@media (prefers-reduced-motion: reduce)'));
  for (const name of Object.keys(dark).filter(n => n.startsWith('--duration-'))) {
    expect(dark[name]).toContain('var(--motion-scale)');
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
  const opening = /\{[#:](?:if|else|each|key|await|then|catch)\b[^}]*\}(?:[ \t]|&#32;)+[^\s<{]/;
  const closing = /[^\s>}](?:[ \t]|&#32;)+\{[/:](?:if|else|each|key|await|then|catch)\b/;
  for (const bad of ['<b>1</b>{#if x} · хаб{/if}', '{#if x}&#32;· хаб{/if}', '{:else} · b', 'a · {/if}']) expect(opening.test(bad) || closing.test(bad), bad).toBe(true);
  expect(opening.test("{#if x}{' · хаб '}<b>2</b>{/if}")).toBe(false);
  const walk = (path: string): string[] => readdirSync(path, { withFileTypes: true }).flatMap(e => e.isDirectory() ? walk(`${path}/${e.name}`) : [`${path}/${e.name}`]);
  for (const path of walk('src').filter(p => p.endsWith('.svelte'))) {
    const markup = readFileSync(path, 'utf8').replace(/<(script|style)[^>]*>[\s\S]*?<\/\1>/g, '');
    expect(markup.match(opening)?.[0], path).toBeUndefined();
    expect(markup.match(closing)?.[0], path).toBeUndefined();
  }
});
