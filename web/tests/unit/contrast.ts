import postcss from 'postcss';
export function themes(text: string): { dark: Record<string, string>; light: Record<string, string> } {
  const dark: Record<string, string> = {}; const light: Record<string, string> = {};
  postcss.parse(text).walkRules(rule => {
    if (rule.parent?.type !== 'root') return;
    if (rule.selector !== ':root' && rule.selector !== '[data-theme=light]') return;
    rule.walkDecls(d => { if (!d.prop.startsWith('--')) return; (rule.selector === ':root' ? dark : light)[d.prop] = d.value; });
  });
  return { dark, light };
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
