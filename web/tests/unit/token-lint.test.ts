import { describe, test, expect } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { lintFile, lintTree } from '../../scripts/token-lint.mjs';
import { icons } from '../../src/lib/ui/icons';
const directory = 'tests/unit/fixtures/token-lint';
describe('tokens', () => {
  test('repo_has_no_raw_styling_values', () => expect(lintTree('src', JSON.parse(readFileSync('token-lint.allow.json', 'utf8')))).toEqual([]));
  test('lint_detects_each_forbidden_form', () => {
    for (const name of readdirSync(directory).filter(n => n.startsWith('bad-'))) {
      const text = readFileSync(`${directory}/${name}`, 'utf8');
      const rule = name.replace(/^bad-/, '').replace(/-\d+$/, '').replace(/\.(css|svelte|ts)$/, '');
      expect(lintFile(name, text).map(v => ({ rule: v.rule, line: v.line })), name).toEqual([{ rule, line: 2 }]);
    }
  });
  test('lint_accepts_token_forms', () => expect(lintFile('good.css', readFileSync(`${directory}/good.css`, 'utf8'))).toEqual([]));
  test('allow_list_entries_need_a_reason', () => expect(() => lintTree('src', [{ path: 'x.css', rule: 'raw-color' }])).toThrow(/reason/));
  test('all_durations_go_through_the_motion_scale', () => {
    expect(lintFile('bad.css', '.a { transition-duration: var(--unscaled); }').map(v => v.rule)).toContain('all_durations_go_through_the_motion_scale');
    for (const code of ['element.animate([], { duration: 123 });', 'element.animate([], 200);', 'element.animate([], {duration});', "element.animate([], {duration: durationMs('--duration-fast-base')});"]) expect(lintFile('bad.ts', code).map(v => v.rule)).toContain('all_durations_go_through_the_motion_scale');
  });
  test('ui_strings_have_no_emoji_or_checkmark_glyphs', () => {
    for (const glyph of ['\u2713', '\u2714', '\u2717', '\u2705', '\u{1f680}', '\u{1f1f7}\u{1f1fa}', '1\ufe0f\u20e3']) expect(lintFile('bad.svelte', `<p>${glyph}</p>`).map(v => v.rule)).toContain('ui_strings_have_no_emoji_or_checkmark_glyphs');
    expect(lintFile('bad.ts', String.raw`export const label = '\u2713';`).map(v => v.rule)).toContain('ui_strings_have_no_emoji_or_checkmark_glyphs');
    expect(lintFile('bad.svelte', '<p>&#10003;</p>').map(v => v.rule)).toContain('ui_strings_have_no_emoji_or_checkmark_glyphs');
    expect(lintFile('ok.svelte', '<p>Доставлено · 0,2 с</p>')).toEqual([]);
    for (const bad of ['check', 'check-circle', 'checkmark', 'circle_check', 'CHECK']) {
      expect(lintFile('src/lib/ui/icons.ts', `export const icons = { ${bad}: 'm0 0' };`).map(v => v.rule)).toContain('ui_strings_have_no_emoji_or_checkmark_glyphs');
    }
    for (const name of Object.keys(icons)) {
      expect(name.toLowerCase(), `icon name ${name} contains check`).not.toContain('check');
    }
  });
});
