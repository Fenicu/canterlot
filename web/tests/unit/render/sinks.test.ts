import { describe, test, expect } from 'vitest';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { resolve, relative } from 'node:path';

function findFiles(dir: string, extRegex: RegExp): string[] {
  const result: string[] = [];
  function walk(current: string) {
    for (const entry of readdirSync(current)) {
      const full = resolve(current, entry);
      const s = statSync(full);
      if (s.isDirectory()) {
        walk(full);
      } else if (extRegex.test(entry)) {
        result.push(full);
      }
    }
  }
  walk(dir);
  return result;
}

export function scanAtHtml(filePath: string, content: string): string[] {
  const violations: string[] = [];
  const normalized = filePath.replaceAll('\\', '/');
  if (normalized.endsWith('src/lib/render/Html.svelte')) {
    return violations;
  }
  if (content.includes('{@html')) {
    violations.push(`Forbidden {@html} in ${filePath}`);
  }
  return violations;
}

export function scanHtmlSinks(filePath: string, content: string): string[] {
  const violations: string[] = [];
  const sinksRegex = /\b(?:innerHTML|outerHTML|insertAdjacentHTML|document\.write)\b/;
  const lines = content.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (line !== undefined && sinksRegex.test(line)) {
      violations.push(`${filePath}:${i + 1}: ${line.trim()}`);
    }
  }
  return violations;
}

describe('html sinks lint', () => {
  test('at_html_is_used_only_in_the_allow_listed_renderer', () => {
    // Negative controls
    expect(scanAtHtml('src/lib/ui/Card.svelte', '<p>{@html raw}</p>')).toHaveLength(1);
    expect(scanAtHtml('src/routes/+page.svelte', '<div>{@html text}</div>')).toHaveLength(1);
    expect(scanAtHtml('src/lib/render/Html.svelte', '<div>{@html html}</div>')).toEqual([]);

    // Scan actual repo src/ directory
    const srcDir = resolve(__dirname, '../../../src');
    const svelteFiles = findFiles(srcDir, /\.svelte$/);
    const violations: string[] = [];
    for (const file of svelteFiles) {
      const content = readFileSync(file, 'utf8');
      violations.push(...scanAtHtml(file, content));
    }
    expect(violations).toEqual([]);

    // Verify that Html.svelte does contain {@html}
    const htmlSveltePath = resolve(srcDir, 'lib/render/Html.svelte');
    const htmlSvelteContent = readFileSync(htmlSveltePath, 'utf8');
    expect(htmlSvelteContent).toContain('{@html');
  });

  test('inner_html_sinks_are_used_only_in_the_renderer', () => {
    // Negative controls for each forbidden sink
    expect(scanHtmlSinks('test.ts', "element.innerHTML = '<b>hi</b>';")).toHaveLength(1);
    expect(scanHtmlSinks('test.ts', "element.outerHTML = '<div>hi</div>';")).toHaveLength(1);
    expect(scanHtmlSinks('test.ts', "element.insertAdjacentHTML('beforeend', '<p>hi</p>');")).toHaveLength(1);
    expect(scanHtmlSinks('test.ts', "document.write('alert(1)');")).toHaveLength(1);

    // Scan actual repo src/ directory (ts, js, svelte)
    const srcDir = resolve(__dirname, '../../../src');
    const codeFiles = findFiles(srcDir, /\.(svelte|ts|js)$/);
    const violations: string[] = [];
    for (const file of codeFiles) {
      const content = readFileSync(file, 'utf8');
      violations.push(...scanHtmlSinks(relative(srcDir, file), content));
    }
    expect(violations).toEqual([]);
  });
});
