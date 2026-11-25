import { describe, test, expect } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { definedTokens, lintFile, lintTree, undefinedTokens } from '../../scripts/token-lint.mjs';
import { icons } from '../../src/lib/ui/icons';
const directory = 'tests/unit/fixtures/token-lint';
describe('tokens', () => {
  test('lint_detects_each_forbidden_form', () => {
    for (const name of readdirSync(directory).filter(n => n.startsWith('bad-'))) {
      const text = readFileSync(`${directory}/${name}`, 'utf8');
      const rule = name.replace(/^bad-/, '').replace(/\.(css|svelte|ts)$/, '').replace(/-\d+$/, '');
      expect(lintFile(name, text).map(v => ({ rule: v.rule, line: v.line })), name).toEqual([{ rule, line: 2 }]);
    }
  });
  test('lint_accepts_token_forms', () => expect(lintFile('good.css', readFileSync(`${directory}/good.css`, 'utf8'))).toEqual([]));
  test('allow_list_entries_need_a_reason', () => {
    expect(() => lintTree('src', [{ path: 'x.css', rule: 'raw-color', value: 'red' }])).toThrow(/reason/);
    expect(() => lintTree('src', [{ path: 'x.css', rule: 'raw-color', reason: 'valid reason without value' }])).toThrow(/value/);
  });
  test('all_durations_go_through_the_motion_scale', () => {
    expect(lintFile('bad.css', '.a { transition-duration: var(--unscaled); }').map(v => v.rule)).toContain('all_durations_go_through_the_motion_scale');
    for (const code of [
      'element.animate([], { duration: 123 });',
      'element.animate([], 200);',
      'element.animate([], {duration});',
      "element.animate([], {duration: durationMs('--duration-fast-base')});",
      'element.style.animationDuration = "200ms";',
      'element.style.transitionDuration = "200ms";',
      'element.animate([], { duration: scaledDurationMs(200) });',
      'scaledDurationMs(200);',
      "element.animate([], { duration: fadeMs('--unscaled') });"
    ]) expect(lintFile('bad.ts', code).map(v => v.rule)).toContain('all_durations_go_through_the_motion_scale');
    expect(lintFile('good.ts', "element.animate([], { duration: fadeMs('--duration-viewer-fade') });")).toEqual([]);
  });
  test('raw_length_detects_transforms_flex_and_background_positions', () => {    expect(lintFile('bad.css', '.a { transform: translateY(12px); }').map(v => v.rule)).toContain('raw-length');
    expect(lintFile('bad.css', '.a { flex-basis: 14px; }').map(v => v.rule)).toContain('raw-length');
    expect(lintFile('bad.css', '.a { flex: 0 0 20px; }').map(v => v.rule)).toContain('raw-length');
    expect(lintFile('bad.css', '.a { background-position: 10px 20px; }').map(v => v.rule)).toContain('raw-length');
    expect(lintFile('good.css', '.a { transform: translateY(var(--s4)); flex: 1; background-position: 0 0; }')).toEqual([]);
  });
  test('layout_rules_catch_their_forms_and_accept_the_repairs', () => {
    const rules = (css: string) => lintFile('x.css', css).map(v => v.rule);
    // Вертикальный скроллер объявляет вторую ось: иначе широкий ребёнок делает его скроллером вбок.
    expect(rules('.a { overflow-y: auto; }')).toEqual(['both_axes_of_a_scroller_are_declared']);
    expect(rules('.a { overflow-y: scroll; }')).toEqual(['both_axes_of_a_scroller_are_declared']);
    expect(rules('.a { overflow-x: hidden; overflow-y: auto; }')).toEqual([]);
    expect(rules('.a { overflow: hidden auto; }')).toEqual([]);
    // Обрезка по строкам и отступ блока: следующая строка видна в отступе.
    expect(rules('.a { -webkit-line-clamp: 3; padding: var(--s2) var(--s4); }')).toEqual(['line_clamp_has_no_block_padding']);
    expect(rules('.a { -webkit-line-clamp: 3; padding-block: var(--s2); }')).toEqual(['line_clamp_has_no_block_padding']);
    expect(rules('.a { -webkit-line-clamp: 3; padding: var(--s0) var(--s4); }')).toEqual([]);
    expect(rules('.a { -webkit-line-clamp: 3; padding-inline: var(--s4); }')).toEqual([]);
    // Пилюля обещает одну строку.
    expect(rules('.a { border-radius: var(--radius-pill); }')).toEqual(['pill_radius_needs_one_line']);
    expect(rules('.a { border-radius: var(--radius-pill); white-space: nowrap; }')).toEqual([]);
    // Отрицательное поле прячет цель касания в текстовую строку.
    expect(rules('.a { margin-block: calc((var(--target) - var(--fs-sm)) / -2); }')).toEqual(['tap_target_is_not_a_negative_margin']);
    expect(rules('.a { --pad: calc((var(--target) - var(--fs-sm)) / 2); margin-block: calc(-1 * var(--pad)); }')).toEqual(['tap_target_is_not_a_negative_margin']);
    expect(rules('.a { margin-block: calc(var(--s2) * -1); }')).toEqual([]);
    expect(rules('.a { margin-inline: calc(var(--s2) * -1); }')).toEqual([]);
    // Перенос в шапке — предупреждение, а не ошибка: гейт он не валит.
    const header = lintFile('x.css', 'header.topbar { flex-wrap: wrap; }');
    expect(header.map(v => v.rule)).toEqual(['header_does_not_wrap']);
    expect(header[0]!.severity).toBe('warning');
    expect(lintFile('x.css', '.topbar { flex-wrap: wrap; }')).toEqual([]);
  });
  test('ui_strings_have_no_emoji_or_checkmark_glyphs', () => {
    for (const glyph of ['\u2713', '\u2714', '\u2717', '\u2705', '\u{1f680}', '\u{1f1f7}\u{1f1fa}', '1\ufe0f\u20e3']) expect(lintFile('bad.svelte', `<p>${glyph}</p>`).map(v => v.rule)).toContain('ui_strings_have_no_emoji_or_checkmark_glyphs');
    expect(lintFile('bad.ts', String.raw`export const label = '\u2713';`).map(v => v.rule)).toContain('ui_strings_have_no_emoji_or_checkmark_glyphs');
    expect(lintFile('bad.svelte', '<p>&#10003;</p>').map(v => v.rule)).toContain('ui_strings_have_no_emoji_or_checkmark_glyphs');
    expect(lintFile('bad.css', '.a { content: "\\2713"; }').map(v => v.rule)).toContain('ui_strings_have_no_emoji_or_checkmark_glyphs');
    expect(lintFile('bad.svelte', '<style>.a { content: "\\2713"; }</style>').map(v => v.rule)).toContain('ui_strings_have_no_emoji_or_checkmark_glyphs');
    expect(lintFile('ok.svelte', '<p>Доставлено · 0,2 с</p>')).toEqual([]);
    for (const bad of ['check', 'check-circle', 'checkmark', 'circle_check', 'CHECK']) {
      expect(lintFile('src/lib/ui/icons.ts', `export const icons = { ${bad}: 'm0 0' };`).map(v => v.rule)).toContain('ui_strings_have_no_emoji_or_checkmark_glyphs');
    }
    for (const name of Object.keys(icons)) {
      expect(name.toLowerCase(), `icon name ${name} contains check`).not.toContain('check');
    }
  });
  test('lint_rejects_undefined_tokens', () => {
    const defined = definedTokens([':root { --raised: #000; }', '<div style:--gap={x}></div>', "use:styleProperty={{ name: '--fill', value: '1%' }}"]);
    expect(undefinedTokens('.a {\n  background: var(--surface-raised);\n}', defined)).toEqual([{ rule: 'undefined-token', line: 2, value: '--surface-raised' }]);
    expect(undefinedTokens('.a { background: var(--raised); gap: var(--gap); width: var(--fill, 0); }', defined)).toEqual([]);
  });
});
