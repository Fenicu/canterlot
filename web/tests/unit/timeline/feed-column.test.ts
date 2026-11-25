import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import postcss, { type Rule } from 'postcss';

const tokens = readFileSync('src/lib/styles/tokens.css', 'utf8');
const scaled = tokens.match(/@media \(min-width: 118rem\) and \(min-height: 56rem\), \(min-width: 62rem\) and \(min-height: 112rem\) \{\s*:root \{([^{}]*)\}/g)!.at(-1)!;

describe('desktop feed column', () => {
  it('is_wider_than_the_reading_column_and_scales_with_the_ui', () => {
    expect(tokens).toMatch(/--reading-max: 720px;/);
    expect(tokens).toMatch(/--feed-max: 960px;/);
    expect(scaled).toContain('--reading-max: calc(720px * var(--ui-scale))');
    expect(scaled).toContain('--feed-max: calc(960px * var(--ui-scale))');
    // Мера прозы в rem растёт с корневым шрифтом и в блоке масштаба не повторяется.
    expect(tokens).toMatch(/--prose-max: 41\.5rem;/);
    expect(scaled).not.toContain('--prose-max');
  });

  it('header_feed_strips_and_composer_follow_the_feed_column', () => {
    const screen = readFileSync('src/lib/screens/chat/ChatScreen.svelte', 'utf8');
    expect(screen).toMatch(/\.screen > :global\(:not\(dialog\)\) \{[^}]*max-width: var\(--feed-max\)/);
    expect(readFileSync('src/lib/timeline/Timeline.svelte', 'utf8')).toContain('calc((100% - var(--feed-max)) / 2 + var(--gutter))');
    expect(readFileSync('src/lib/screens/chat/Lane.svelte', 'utf8')).toContain('max-width: calc(var(--feed-max) - 2 * var(--gutter))');
  });

  it('prose_keeps_its_measure_while_tables_and_code_take_the_full_width', () => {
    const capped = new Set<string>();
    postcss.parse(readFileSync('src/lib/styles/prose.css', 'utf8')).walkDecls('max-width', (decl) => {
      if (decl.value === 'var(--prose-max)') capped.add((decl.parent as Rule).selector);
    });
    expect([...capped]).toEqual(['.prose :is(p, ul, ol, blockquote, h1, h2, h3, h4, h5, h6)']);
    expect(tokens).toMatch(/--bubble-max: min\(86%, var\(--prose-max\)\);/);
  });
});
