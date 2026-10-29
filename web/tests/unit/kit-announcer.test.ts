import { test, expect } from 'vitest';
import { readFileSync, existsSync } from 'node:fs';
import { removeKitAnnouncer } from '../../scripts/kit-announcer.mjs';
test('kit_announcer_is_replaced_by_the_single_token_styled_host', () => {
  const rootPath = existsSync('node_modules/@sveltejs/kit/src/runtime/components/root.svelte')
    ? 'node_modules/@sveltejs/kit/src/runtime/components/root.svelte'
    : '.svelte-kit/generated/root.svelte';
  const content = readFileSync(rootPath, 'utf8');
  const code = removeKitAnnouncer({ content, filename: `${process.cwd()}/${rootPath}` })?.code;
  expect(code).not.toContain('svelte-announcer'); expect(code).not.toContain('style='); expect(code).toContain('node(tree, 0)');
  expect(() => removeKitAnnouncer({ content: 'changed', filename: `${process.cwd()}/${rootPath}` })).toThrow(/changed/);
  expect(removeKitAnnouncer({ content: 'unchanged', filename: '/src/lib/Any.svelte' })).toBeUndefined();
});
