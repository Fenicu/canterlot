import { modelLabel } from '#lib/format/labels.js';

/** Вендор модели: по нему строка списка выбирает аватар. */
export type Vendor =
  | 'anthropic'
  | 'openai'
  | 'deepseek'
  | 'meta'
  | 'mimo'
  | 'zai'
  | 'moonshot'
  | 'qwen'
  | 'minimax'
  | 'gemini'
  | 'xai'
  | 'mistral'
  | 'opencode'
  | 'antigravity';

export const VENDOR_TITLES: Record<Vendor, string> = {
  anthropic: 'Anthropic',
  openai: 'OpenAI',
  deepseek: 'DeepSeek',
  meta: 'Meta',
  mimo: 'Xiaomi MiMo',
  zai: 'Z.ai',
  moonshot: 'Moonshot',
  qwen: 'Qwen',
  minimax: 'MiniMax',
  gemini: 'Google Gemini',
  xai: 'xAI',
  mistral: 'Mistral',
  opencode: 'OpenCode',
  antigravity: 'Antigravity'
};

// Порядок важен: семейство узнаётся раньше общего `gpt`, который встречается и у чужих моделей.
const BY_MODEL: [RegExp, Vendor][] = [
  [/claude|opus|sonnet|haiku|fable|anthropic/, 'anthropic'],
  [/deepseek/, 'deepseek'],
  [/muse|llama|(^|\/)meta[-/]/, 'meta'],
  [/mimo|xiaomi/, 'mimo'],
  [/glm|zhipu|z-ai|(^|\/)zai\//, 'zai'],
  [/kimi|moonshot/, 'moonshot'],
  [/qwen/, 'qwen'],
  [/minimax/, 'minimax'],
  [/gemini|gemma/, 'gemini'],
  [/grok|(^|\/)x-?ai\//, 'xai'],
  [/mistral|devstral|codestral|magistral|ministral/, 'mistral'],
  [/gpt|openai|codex|(^|\/)o\d/, 'openai']
];

/**
 * Вендор по харнессу и модели: Claude Code — Anthropic, Codex — OpenAI, остальные по
 * имени модели. Незнакомая модель OpenCode и Antigravity получает знак харнесса; `null` —
 * знака нет, строка рисует букву.
 */
export function vendorOf(provider: string, model: string): Vendor | null {
  const family = provider.toLowerCase();
  if (family === 'claude') return 'anthropic';
  if (family === 'codex') return 'openai';
  const id = model.toLowerCase();
  for (const [pattern, vendor] of BY_MODEL) if (pattern.test(id)) return vendor;
  if (family === 'opencode') return 'opencode';
  if (family === 'antigravity') return 'antigravity';
  return null;
}

const BRANDS: Record<string, string> = {
  deepseek: 'DeepSeek',
  mimo: 'MiMo',
  glm: 'GLM',
  kimi: 'Kimi',
  qwen: 'Qwen',
  minimax: 'MiniMax',
  gemini: 'Gemini',
  gemma: 'Gemma',
  grok: 'Grok',
  mistral: 'Mistral',
  devstral: 'Devstral',
  codestral: 'Codestral',
  muse: 'Muse',
  llama: 'Llama',
  gpt: 'GPT',
  oss: 'OSS'
};

// Хвосты, которые в строке ничего не различают.
const NOISE = /^(?:1m|\(1m\)|preview|latest|instruct|chat|exp|experimental|\d{6,8})$/i;
const VERSION = /^(?:v?\d+(?:[.-]\d+)*[a-z]?|[a-z]\d+(?:\.\d+)*)$/i;

function word(token: string): string {
  const brand = BRANDS[token.toLowerCase()];
  if (brand) return brand;
  if (/^v\d/i.test(token)) return token.toUpperCase();
  if (/^[a-z]\d/i.test(token)) return token.toUpperCase();
  return token.charAt(0).toUpperCase() + token.slice(1);
}

/**
 * Короткое имя модели для второй строки: «Opus», «GPT-6.1 Sol», «DeepSeek V4.1», «Muse Spark».
 * Берёт название из каталога машины, если оно есть, без `1M`; версия уходит, когда
 * имя и без неё различимо (у Claude семейство, у прочих — слово после версии).
 */
export function modelShortName(model: string, resolved?: string | null, catalogTitle?: string | null): string {
  const source = resolved || model;
  if (!source && !catalogTitle) return 'Модель';
  const claude = /^(?:claude-)?(opus|sonnet|haiku|fable)\b/i.exec((resolved || model).replace(/^.*\//, ''));
  if (claude?.[1]) return word(claude[1]);
  const title = (catalogTitle?.trim() || modelLabel(source)).replace(/^claude\s+/i, '');
  const family = /^(opus|sonnet|haiku|fable)\b/i.exec(title);
  if (family?.[1]) return word(family[1]);
  // GPT держит версию в первом слове: «GPT-6.1 Sol».
  if (/^gpt-\d/i.test(title)) return title.replace(/\s*\b1M\b/i, '').trim();
  const tokens = title
    .replace(/^.*\//, '')
    .split(/[\s_-]+/)
    .filter((token) => token && !NOISE.test(token));
  if (tokens.length === 0) return title;
  const [head, ...rest] = tokens;
  const words = rest.filter((token) => !VERSION.test(token));
  const named = words.length > 0 ? [head, ...words.slice(0, 1)] : [head, ...rest.slice(0, 1)];
  return named.map((token) => word(token ?? '')).join(' ');
}

/** Буква аватара для модели без знака вендора. */
export function vendorLetter(model: string, fallback: string): string {
  const text = (model.replace(/^.*\//, '') || fallback).trim();
  const letter = /[\p{L}\p{N}]/u.exec(text)?.[0] ?? '?';
  return letter.toLocaleUpperCase('ru-RU');
}
