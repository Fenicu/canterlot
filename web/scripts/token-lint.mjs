import { readFileSync, readdirSync } from 'node:fs';
import { resolve, relative } from 'node:path';
import postcss from 'postcss';
import { URL as NodeURL } from 'node:url';
import ts from 'typescript';
const colors = /#[\da-f]{3,8}\b|\b(?:rgba?|hsla?|hwb|lab|lch|oklab|oklch|color)\s*\(/i;
const names = /\b(?:aliceblue|antiquewhite|aqua|aquamarine|azure|beige|bisque|black|blue|blueviolet|brown|burlywood|cadetblue|chartreuse|chocolate|coral|cornflowerblue|cornsilk|crimson|cyan|darkblue|darkcyan|darkgoldenrod|darkgray|darkgreen|darkgrey|darkkhaki|darkmagenta|darkolivegreen|darkorange|darkorchid|darkred|darksalmon|darkseagreen|darkslateblue|darkslategray|darkslategrey|darkturquoise|darkviolet|deeppink|deepskyblue|dimgray|dimgrey|dodgerblue|firebrick|floralwhite|forestgreen|fuchsia|gainsboro|ghostwhite|gold|goldenrod|gray|green|greenyellow|grey|honeydew|hotpink|indianred|indigo|ivory|khaki|lavender|lavenderblush|lawngreen|lemonchiffon|lightblue|lightcoral|lightcyan|lightgoldenrodyellow|lightgray|lightgreen|lightgrey|lightpink|lightsalmon|lightseagreen|lightskyblue|lightslategray|lightslategrey|lightsteelblue|lightyellow|lime|limegreen|linen|magenta|maroon|mediumaquamarine|mediumblue|mediumorchid|mediumpurple|mediumseagreen|mediumslateblue|mediumspringgreen|mediumturquoise|mediumvioletred|midnightblue|mintcream|mistyrose|moccasin|navajowhite|navy|oldlace|olive|olivedrab|orange|orangered|orchid|palegoldenrod|palegreen|paleturquoise|palevioletred|papayawhip|peachpuff|peru|pink|plum|powderblue|purple|rebeccapurple|red|rosybrown|royalblue|saddlebrown|salmon|sandybrown|seagreen|seashell|sienna|silver|skyblue|slateblue|slategray|slategrey|snow|springgreen|steelblue|tan|teal|thistle|tomato|turquoise|violet|wheat|white|whitesmoke|yellow|yellowgreen)\b/i;
const length = /(?:\d*\.)?\d+(?:px|rem|em|pt|pc|in|cm|mm|q|ex|cap|lh|rlh)\b/i;
const motion = /(?:\d*\.)?\d+m?s\b|cubic-bezier\s*\(|\b(?:ease(?:-in(?:-out)?|-out)?|linear|step-start|step-end)\b/i;
const emoji = /[\p{Extended_Pictographic}\p{Regional_Indicator}\u2705\u2713\u2714\u2717\u2718\u2611\u20e3\ufe0f]/u;
const tokenSource = () => readFileSync(new NodeURL('../src/lib/styles/tokens.css', import.meta.url), 'utf8');
function publicDurations() {
  try { return new Set([...tokenSource().matchAll(/(--duration-[\w-]+):\s*calc\([^;]*var\(--motion-scale\)/g)].map(m => m[1])); }
  catch { return new Set(); }
}
const stripVars = value => value.replace(/var\(\s*--[\w-]+\s*\)/g, '');
export function lintFile(path, text) {
  if (path.replaceAll('\\', '/').endsWith('/styles/tokens.css')) return [];
  const found = [];
  const add = (rule, offset, value) => {
    const line = text.slice(0, offset).split('\n').length;
    if (!found.some(v => v.rule === rule && v.line === line)) found.push({ rule, line, value });
  };
  const allowed = publicDurations();
  const aliases = new Map();
  const cssBlocks = path.endsWith('.css') ? [{ text, offset: 0 }] : [...text.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/g)].map(m => ({ text: m[1], offset: m.index + m[0].indexOf('>') + 1 }));
  for (const block of cssBlocks) {
    const tree = postcss.parse(block.text);
    tree.walkDecls(d => { if (d.prop.startsWith('--')) aliases.set(d.prop, d.value); });
  }
  const tokenValues = new Map();
  try { postcss.parse(tokenSource()).walkDecls(d => { if (d.prop.startsWith('--') && !tokenValues.has(d.prop)) tokenValues.set(d.prop, d.value); }); } catch { /* Отсутствие токенов выявят проверки дерева. */ }
  const resolveTime = (value, seen = new Set()) => {
    const vars = [...value.matchAll(/var\(\s*(--[\w-]+)/g)].map(m => m[1]);
    if (!vars.length) return { valid: /^[-.\d\s+*/()]+$/.test(value), time: false };
    const results = vars.map(v => {
      if (allowed.has(v)) return { valid: true, time: true };
      if (v.startsWith('--ease')) return { valid: true, time: false };
      if (seen.has(v)) return { valid: false, time: false };
      const next = aliases.get(v) ?? tokenValues.get(v);
      return next === undefined ? { valid: false, time: false } : resolveTime(next, new Set([...seen, v]));
    });
    return { valid: results.every(r => r.valid), time: results.some(r => r.time) };
  };
  const scaled = value => { const result = resolveTime(value); return result.valid && result.time; };
  function declaration(prop, value, offset, inline = false) {
    const clean = stripVars(value);
    const rawColor = colors.test(clean) || ((/color|background|^fill$|^stroke$|^border|^outline/.test(prop) || prop.startsWith('--')) && names.test(clean));
    const rawLength = length.test(clean) && /font|padding|margin|gap|inset|top|right|bottom|left|width|height|radius|outline|border|size|spacing|translate/.test(prop);
    const rawFont = /^(font|font-family|font-weight|line-height)$/.test(prop) && clean.replace(/[\s/]/g, '') !== '' && !/^(0|inherit|initial|unset|normal)$/.test(clean.trim());
    const rawShadow = /shadow/.test(prop) || /drop-shadow\(/.test(value);
    const badShadow = rawShadow && !/^(none|var\(--shadow\))$/.test(value.trim());
    const rawMotion = /^(transition|animation)/.test(prop) && motion.test(clean);
    if (inline) { if (rawColor || rawLength || rawFont || badShadow || rawMotion) add('inline-style', offset, value); return; }
    if (badShadow) add('raw-shadow', offset, value);
    else if (rawMotion) add('raw-motion', offset, value);
    else {
      if (rawColor) add('raw-color', offset, value);
      if (rawLength) add('raw-length', offset, value);
      if (rawFont) add('raw-font', offset, value);
    }
    if (!rawMotion && /^(?:transition|animation)(?:-duration|-delay)?$/.test(prop) && !/^(none|0|initial|inherit|unset)$/.test(value.trim()) && !scaled(value)) add('all_durations_go_through_the_motion_scale', offset, value);
  }
  for (const block of cssBlocks) postcss.parse(block.text).walkDecls(d => declaration(d.prop, d.value, block.offset + d.source.start.offset));
  const markup = text.replace(/<(script|style)[^>]*>[\s\S]*?<\/\1>/g, m => m.replace(/[^\n]/g, ' '));
  if (!path.endsWith('.css')) {
    for (const m of markup.matchAll(/\bstyle=(['"])([\s\S]*?)\1/g)) {
      try { postcss.parse(`a{${m[2]}}`).walkDecls(d => declaration(d.prop, d.value, m.index, true)); }
      catch { add('inline-style', m.index, m[2]); }
    }
    for (const m of markup.matchAll(/\bstyle:([\w-]+)=\{([^}]+)\}/g)) declaration(m[1], m[2].replace(/^['"]|['"]$/g, ''), m.index, true);
    for (const m of markup.matchAll(/\b(fill|stroke|stop-color)=(['"])(.*?)\2/g)) if (!/^(currentColor|none|var\(--[\w-]+\))$/.test(m[3])) add('svg-paint', m.index, m[3]);
    for (const m of markup.matchAll(/\b(fill|stroke|stop-color)=\{(['"])(.*?)\2\}/g)) if (!/^(currentColor|none|var\(--[\w-]+\))$/.test(m[3])) add('svg-paint', m.index, m[3]);
    const scripts = path.endsWith('.ts') || path.endsWith('.js') ? [{ text, offset: 0 }] : [...text.matchAll(/<script[^>]*>([\s\S]*?)<\/script>/g)].map(m => ({ text: m[1], offset: m.index }));
    for (const block of scripts) {
      for (const m of block.text.matchAll(/(['"`])([^'"`\n]*)\1/g)) if (colors.test(m[2])) add('string-color', block.offset + m.index, m[2]);
      const source = ts.createSourceFile(path, block.text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
      const durationCall = expression => ts.isCallExpression(expression) && ts.isIdentifier(expression.expression) && ['durationMs', 'scaledDurationMs'].includes(expression.expression.text);
      function visit(node) {
        if ((ts.isStringLiteralLike(node) || ts.isTemplateHead(node) || ts.isTemplateMiddle(node) || ts.isTemplateTail(node)) && emoji.test(node.text)) add('ui_strings_have_no_emoji_or_checkmark_glyphs', block.offset + node.getStart(source), node.text);
        const inIconRegistry = path.replaceAll('\\', '/').endsWith('icons.ts') || path.replaceAll('\\', '/').endsWith('icons.js') ||
          (node.parent && ts.isObjectLiteralExpression(node.parent) && ts.isVariableDeclaration(node.parent.parent) && node.parent.parent.name?.getText(source) === 'icons');
        if (inIconRegistry && (ts.isPropertyAssignment(node) || ts.isShorthandPropertyAssignment(node) || ts.isPropertySignature(node)) && /check/i.test(node.name.getText(source).replace(/^['"]|['"]$/g, ''))) {
          add('ui_strings_have_no_emoji_or_checkmark_glyphs', block.offset + node.getStart(source), node.name.getText(source));
        }
        if (inIconRegistry && ts.isLiteralTypeNode(node) && ts.isStringLiteral(node.literal) && /check/i.test(node.literal.text)) {
          add('ui_strings_have_no_emoji_or_checkmark_glyphs', block.offset + node.getStart(source), node.literal.text);
        }
        if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === 'durationMs' && (!node.arguments[0] || !ts.isStringLiteral(node.arguments[0]) || !allowed.has(node.arguments[0].text))) add('all_durations_go_through_the_motion_scale', block.offset + node.getStart(source), node.getText(source));
        if (ts.isPropertyAssignment(node) && ['duration', 'delay'].includes(node.name.getText(source)) && !durationCall(node.initializer)) add('all_durations_go_through_the_motion_scale', block.offset + node.getStart(source), node.initializer.getText(source));
        if (ts.isShorthandPropertyAssignment(node) && ['duration', 'delay'].includes(node.name.text)) add('all_durations_go_through_the_motion_scale', block.offset + node.getStart(source), node.name.text);
        if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression) && node.expression.name.text === 'animate' && node.arguments[1] && !ts.isObjectLiteralExpression(node.arguments[1])) add('all_durations_go_through_the_motion_scale', block.offset + node.getStart(source), node.arguments[1].getText(source));
        ts.forEachChild(node, visit);
      }
      visit(source);
    }
        if (path.endsWith('.svelte') && !path.endsWith('/Button.svelte')) for (const m of markup.matchAll(/<button\b[^>]*\bdisabled\b[^>]*>/g)) add('disabled-native-button', m.index, m[0]);
  }
  for (const m of text.matchAll(new RegExp(emoji.source, 'gu'))) add('ui_strings_have_no_emoji_or_checkmark_glyphs', m.index, m[0]);
  for (const m of markup.matchAll(/&#(?:x([0-9a-f]+)|(\d+));|&(check|checkmark|cross);/gi)) { const glyph = m[3] ? '\u2713' : String.fromCodePoint(parseInt(m[1] ?? m[2], m[1] ? 16 : 10)); if (emoji.test(glyph)) add('ui_strings_have_no_emoji_or_checkmark_glyphs', m.index, m[0]); }
  return found.sort((a, b) => a.line - b.line || a.rule.localeCompare(b.rule));
}
export function lintTree(root, allow) {
  for (const entry of allow) if (!entry.reason?.trim()) throw new Error('Allow entry needs a reason');
  const result = [];
  function walk(directory) {
    for (const item of readdirSync(directory, { withFileTypes: true })) {
      const file = resolve(directory, item.name);
      if (item.isDirectory()) walk(file);
      else if (/\.(css|svelte|ts|js|html)$/.test(item.name)) {
        const path = relative(resolve(root), file).replaceAll('\\', '/');
        for (const violation of lintFile(file, readFileSync(file, 'utf8'))) if (!allow.some(entry => entry.path === path && entry.rule === violation.rule && (entry.value === undefined || entry.value === violation.value))) result.push({ path, ...violation });
      }
    }
  }
  walk(root);
  return result;
}
