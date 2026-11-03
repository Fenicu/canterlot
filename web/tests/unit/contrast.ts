import postcss from 'postcss';

export const COLOR_TOKENS = [
  'bg', 'chat-bg', 'surface', 'raised', 'hover', 'selected', 'text', 'secondary', 'muted', 'time',
  'line', 'control-line', 'chip', 'chip-text', 'badge', 'accent', 'accent-link', 'accent-ink', 'accent-soft',
  'success', 'success-soft', 'warning', 'warning-soft', 'danger', 'danger-soft', 'blue',
  'code', 'scrim', 'chart-secondary', 'transparent', 'popover', 'popover-hover',
  'syn-keyword', 'syn-control', 'syn-string', 'syn-function', 'syn-type', 'syn-comment', 'syn-number',
  'syn-variable', 'syn-symbol'
] as const;

export type ThemeMode = 'dark' | 'light';
export interface Variant { palette: string; theme: ThemeMode; tokens: Record<string, string> }

const variantSelector = /^\[data-palette=([a-z-]+)\]\[data-theme=(dark|light)\]$/;

export function variants(text: string): Variant[] {
  const found = new Map<string, Variant>();
  const add = (palette: string, theme: ThemeMode, tokens: Record<string, string>) => {
    const key = `${palette}/${theme}`;
    const current = found.get(key);
    if (current) Object.assign(current.tokens, tokens);
    else found.set(key, { palette, theme, tokens: { ...tokens } });
  };
  postcss.parse(text).walkRules(rule => {
    if (rule.parent?.type !== 'root') return;
    const selector = rule.selector.trim();
    let palette: string;
    let theme: ThemeMode;
    if (selector === ':root') { palette = 'aquamarine'; theme = 'dark'; }
    else {
      const match = selector.match(variantSelector);
      if (!match) return;
      palette = match[1]!;
      theme = match[2] as ThemeMode;
    }
    const tokens: Record<string, string> = {};
    rule.walkDecls(d => { if (d.prop.startsWith('--')) tokens[d.prop] = d.value; });
    add(palette, theme, tokens);
  });
  return [...found.values()];
}

function luminance(hex: string): number {
  const n = parseInt(hex.slice(1), 16);
  const channels = [n >> 16, n >> 8 & 255, n & 255].map(c => { const v = c / 255; return v <= .04045 ? v / 12.92 : ((v + .055) / 1.055) ** 2.4; });
  return channels[0]! * .2126 + channels[1]! * .7152 + channels[2]! * .0722;
}
export function contrast(a: string, b: string): number {
  const x = luminance(a); const y = luminance(b);
  return (Math.max(x, y) + .05) / (Math.min(x, y) + .05);
}
