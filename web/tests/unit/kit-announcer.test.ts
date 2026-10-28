import { test, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { removeKitAnnouncer } from '../../scripts/kit-announcer.mjs';
test('kit_announcer_is_replaced_by_the_single_token_styled_host', () => {
  const content = readFileSync('.svelte-kit/generated/root.svelte', 'utf8');
  const code = removeKitAnnouncer({ content, filename: `${process.cwd()}/.svelte-kit/generated/root.svelte` })?.code;
  expect(code).not.toContain('svelte-announcer'); expect(code).not.toContain('style='); expect(code).toContain('Pyramid_');
  expect(() => removeKitAnnouncer({ content: 'changed', filename: `${process.cwd()}/.svelte-kit/generated/root.svelte` })).toThrow(/changed/);
  expect(removeKitAnnouncer({ content: 'unchanged', filename: '/src/lib/Any.svelte' })).toBeUndefined();
});
