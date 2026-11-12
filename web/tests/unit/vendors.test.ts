import { describe, expect, it } from 'vitest';
import { LOGOS, parseLogo } from '#lib/vendors/logos.js';
import { VENDOR_TITLES, modelShortName, vendorLetter, vendorOf, type Vendor } from '#lib/vendors/vendor.js';

describe('vendorOf', () => {
  it.each<[string, string, Vendor | null]>([
    ['claude', 'opus[1m]', 'anthropic'],
    ['claude', 'claude-fable-5-2', 'anthropic'],
    ['claude', 'some-future-model', 'anthropic'],
    ['codex', 'gpt-6.1-sol', 'openai'],
    ['codex', 'o5-mini', 'openai'],
    ['opencode', 'deepseek/deepseek-v4.1', 'deepseek'],
    ['opencode', 'meta/muse-spark', 'meta'],
    ['opencode', 'xiaomi/mimo-v2-flash', 'mimo'],
    ['opencode', 'zai/glm-4.6', 'zai'],
    ['opencode', 'moonshotai/kimi-k2', 'moonshot'],
    ['opencode', 'qwen/qwen3-coder', 'qwen'],
    ['opencode', 'minimax/minimax-m2', 'minimax'],
    ['opencode', 'google/gemini-3-pro', 'gemini'],
    ['opencode', 'xai/grok-4-fast', 'xai'],
    ['opencode', 'mistral/devstral-medium', 'mistral'],
    ['opencode', 'openai/gpt-oss-120b', 'openai'],
    ['opencode', 'anthropic/claude-sonnet-5', 'anthropic'],
    ['opencode', 'local/unknown-model', 'opencode'],
    ['antigravity', 'gemini-3-pro', 'gemini'],
    ['antigravity', 'mystery', 'antigravity'],
    ['synthetic', 'synthetic-small', null]
  ])('%s %s → %s', (provider, model, vendor) => {
    expect(vendorOf(provider, model)).toBe(vendor);
  });
});

describe('modelShortName', () => {
  it.each<[string, string | null, string | null, string]>([
    ['opus[1m]', 'claude-opus-5-5', 'Opus 5.5 1M', 'Opus'],
    ['sonnet', null, null, 'Sonnet'],
    ['fable[1m]', 'claude-fable-5-2', null, 'Fable'],
    ['haiku', null, 'Haiku 5.5', 'Haiku'],
    ['gpt-6.1-sol', null, null, 'GPT-6.1 Sol'],
    ['gpt-6.1-sol', null, 'GPT-6.1 Sol', 'GPT-6.1 Sol'],
    ['deepseek/deepseek-v4.1', null, null, 'DeepSeek V4.1'],
    ['deepseek/deepseek-v4.1', null, 'DeepSeek V4.1', 'DeepSeek V4.1'],
    ['meta/muse-spark', null, null, 'Muse Spark'],
    ['xiaomi/mimo-v2-flash', null, null, 'MiMo Flash'],
    ['xiaomi/mimo', null, null, 'MiMo'],
    ['google/gemini-2.5-pro-preview', null, null, 'Gemini Pro'],
    ['zai/glm-4.6', null, null, 'GLM 4.6']
  ])('%s → %s', (model, resolved, title, short) => {
    expect(modelShortName(model, resolved, title)).toBe(short);
  });

  it('keeps names short', () => {
    expect(modelShortName('vendor/very-long-model-name-with-many-words').split(' ').length).toBeLessThanOrEqual(2);
  });
});

describe('logos', () => {
  it('bundles a parsed mark for every vendor', () => {
    for (const vendor of Object.keys(VENDOR_TITLES) as Vendor[]) {
      const logo = LOGOS[vendor];
      expect(logo, vendor).toBeDefined();
      expect(logo?.paths.length, vendor).toBeGreaterThan(0);
      expect(logo?.viewBox).toBe('0 0 24 24');
    }
  });

  it('reads only path geometry', () => {
    expect(parseLogo('<svg fill-rule="evenodd" viewBox="0 0 24 24"><path d="M1 1h2"/><path d="M3 3h1"/></svg>')).toEqual({
      viewBox: '0 0 24 24',
      evenodd: true,
      paths: ['M1 1h2', 'M3 3h1']
    });
  });

  it('falls back to a letter', () => {
    expect(vendorLetter('local/synthetic-small', 'Сессия')).toBe('S');
    expect(vendorLetter('', 'сессия')).toBe('С');
  });
});
