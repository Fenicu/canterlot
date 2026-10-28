import { test, expect } from 'vitest';
import { ESLint } from 'eslint';
const eslint = new ESLint({ cwd: process.cwd() });
test('eslint_forbids_inner_html_sinks_outside_the_renderer', async () => {
  for (const code of ['element.innerHTML = value;', 'element.outerHTML = value;', 'element.insertAdjacentHTML("beforeend", value);', 'document.write(value);']) {
    const [result] = await eslint.lintText(code, { filePath: 'src/lib/negative.ts' });
    expect(result?.messages.some(m => m.ruleId === 'no-restricted-properties'), code).toBe(true);
  }
  const [html] = await eslint.lintText('<script>let value = "";</script>{@html value}', { filePath: 'src/lib/Negative.svelte' });
  expect(html?.messages.some(m => m.ruleId === 'svelte/no-at-html-tags')).toBe(true);
});
test('eslint_requires_icon_button_labels_and_forbids_eval_and_console_log', async () => {
  const [icon] = await eslint.lintText('<script>import IconButton from "./ui/IconButton.svelte";</script><IconButton icon="moon" />', { filePath: 'src/lib/Negative.svelte' });
  expect(icon?.messages.some(m => m.ruleId === 'canterlot/icon-label')).toBe(true);
  const [script] = await eslint.lintText('console.log("text"); eval("text");', { filePath: 'src/lib/negative.ts' });
  expect(script?.messages.map(m => m.ruleId)).toEqual(expect.arrayContaining(['no-restricted-properties', 'no-eval']));
});
