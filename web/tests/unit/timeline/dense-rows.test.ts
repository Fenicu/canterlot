import { afterEach, describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import postcss, { type AtRule, type Rule } from 'postcss';
import { createRawSnippet, flushSync, mount, unmount } from 'svelte';
import CardShell from '#lib/timeline/cards/CardShell.svelte';
import StepGroupRow from '#lib/timeline/StepGroupRow.svelte';
import { FeedFolds } from '#lib/timeline/folds.js';
import type { StepGroup } from '#lib/timeline/steps.js';
import type { TimelineEntry } from '#lib/stores/timeline.js';
import type { CardEnv } from '#lib/timeline/cards/types.js';
import { made } from './cards/helpers';

const FINE = '(hover: hover) and (pointer: fine)';
const tokens = readFileSync('src/lib/styles/tokens.css', 'utf8');
const style = (path: string) => postcss.parse(readFileSync(path, 'utf8').match(/<style>([\s\S]*)<\/style>/)![1]!);
const fine = (rule: Rule) => rule.parent?.type === 'atrule' && (rule.parent as AtRule).params === FINE;

/** Значения свойства у правила с этим селектором: вне медиазапроса мыши и внутри него. */
function decls(path: string, selector: string, prop: string): { base: string[]; mouse: string[] } {
  const out = { base: [] as string[], mouse: [] as string[] };
  style(path).walkRules((rule) => {
    if (!rule.selectors.includes(selector)) return;
    rule.walkDecls(prop, (decl) => { (fine(rule) ? out.mouse : out.base).push(decl.value); });
  });
  return out;
}

const scaled = tokens.match(/@media \(min-width: 118rem\) and \(min-height: 56rem\), \(min-width: 62rem\) and \(min-height: 112rem\) \{\s*:root \{([^{}]*)\}/g)!.at(-1)!;

describe('dense rows for a mouse', () => {
  it('row_tokens_scale_with_the_ui', () => {
    expect(tokens).toMatch(/--row: 30px;/);
    expect(tokens).toMatch(/--row-footer: 32px;/);
    expect(scaled).toContain('--row: calc(30px * var(--ui-scale))');
    expect(scaled).toContain('--row-footer: calc(32px * var(--ui-scale))');
  });

  // Телефон держит цель касания 44 px: плотная строка живёт только под мышью.
  it.each([
    ['src/lib/timeline/cards/CardShell.svelte', '.head'],
    ['src/lib/timeline/StepHead.svelte', '.head'],
    ['src/lib/timeline/cards/QuietRow.svelte', '.head']
  ])('%s_row_is_a_touch_target_and_only_a_mouse_gets_the_dense_height', (path, selector) => {
    expect(decls(path, selector, 'min-height')).toEqual({ base: ['var(--target)'], mouse: ['var(--row)'] });
    style(path).walkDecls((decl) => {
      if (decl.value.includes('var(--row)')) expect(fine(decl.parent as Rule), `${path} ${decl.toString()}`).toBe(true);
    });
  });

  it('collapsed_rows_drop_their_plate_only_under_a_mouse', () => {
    const plates: [string, string][] = [
      ['src/lib/timeline/cards/CardShell.svelte', '.feed-card:not([data-open])'],
      ['src/lib/timeline/ToolBlock.svelte', '.tool:not([data-open])'],
      ['src/lib/timeline/StepGroupRow.svelte', '.steps:not([data-open]) .surface'],
      ['src/lib/requests/RequestCard.svelte', '.applied[data-collapsed]']
    ];
    for (const [path, selector] of plates) expect(decls(path, selector, 'background'), path).toEqual({ base: [], mouse: ['var(--transparent)'] });
    for (const path of ['src/lib/timeline/cards/CardShell.svelte', 'src/lib/timeline/StepHead.svelte', 'src/lib/timeline/cards/QuietRow.svelte']) {
      expect(decls(path, '.head:hover', 'background'), path).toEqual({ base: [], mouse: ['var(--hover)'] });
    }
  });

  it('answer_card_rows_touch_and_the_footer_shrinks_only_under_a_mouse', () => {
    const path = 'src/lib/timeline/AnswerCard.svelte';
    expect(decls(path, '.card', 'gap')).toEqual({ base: ['var(--s2)'], mouse: ['var(--s0)'] });
    expect(decls(path, '.byline', 'min-height')).toEqual({ base: ['var(--target)'], mouse: ['var(--row-footer)'] });
  });
});

describe('open state on the row', () => {
  let instance: ReturnType<typeof mount> | undefined;
  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = '';
  });

  const env = (open: boolean): CardEnv => ({
    open,
    onToggle: () => undefined,
    load: async () => ({ text: '', offset: 0, total: 0 }) as never,
    now: 0,
    working: false,
    links: new Map() as never,
    background: null,
    conversationId: 'c1',
    folds: new FeedFolds(),
    icon: null
  });

  it('card_shell_marks_an_expanded_card', () => {
    const call = made('mcp__plugin_taiga_taiga__taiga_stats', { project: 'cus' }, '{"total_points": 41}');
    instance = mount(CardShell, { target: document.body, props: { call, env: env(false), line: 'Статистика' } });
    flushSync();
    expect(document.querySelector('.feed-card')!.hasAttribute('data-open')).toBe(false);
    void unmount(instance);
    instance = mount(CardShell, { target: document.body, props: { call, env: env(true), line: 'Статистика' } });
    flushSync();
    expect(document.querySelector('.feed-card')!.hasAttribute('data-open')).toBe(true);
  });

  it('step_group_marks_itself_open_after_a_toggle', () => {
    const group: StepGroup = { kind: 'group', key: 'group:1', entries: [], label: '2 шага', tally: 'Команда 2', failed: 0, elapsed: 0 };
    const row = createRawSnippet<[TimelineEntry]>(() => ({ render: () => '<span></span>' }));
    instance = mount(StepGroupRow, { target: document.body, props: { group, folds: new FeedFolds(), row } });
    flushSync();
    const steps = document.querySelector('.steps')!;
    expect(steps.hasAttribute('data-open')).toBe(false);
    document.querySelector<HTMLButtonElement>('.steps .head')!.click();
    flushSync();
    expect(steps.hasAttribute('data-open')).toBe(true);
  });
});
