import { describe, expect, it } from 'vitest';
import { blockquoteSources, codeCopyText } from '#lib/render/copy.js';
import { createMarkdown } from '#lib/render/markdown.js';

const QUOTE = [
  '> ## Обновление сборки mc',
  '>',
  '> Друзья, **обновляем** сборку `mc`:',
  '>',
  '> 1. Скачайте архив',
  '> 2. Выполните в PowerShell:',
  '>    ```powershell',
  '>    Write-Host "готово"',
  '>    ```',
  '> 3. Готово',
  '>',
  '> > Вложенная цитата'
].join('\n');

const INNER = [
  '## Обновление сборки mc',
  '',
  'Друзья, **обновляем** сборку `mc`:',
  '',
  '1. Скачайте архив',
  '2. Выполните в PowerShell:',
  '   ```powershell',
  '   Write-Host "готово"',
  '   ```',
  '3. Готово',
  '',
  '> Вложенная цитата'
].join('\n');

describe('blockquoteSources', () => {
  it('returns_inner_markdown_without_the_quote_markers', () => {
    expect(blockquoteSources(QUOTE)).toEqual([INNER]);
  });

  it('keeps_the_nested_quote_marker_and_skips_quotes_inside_lists', () => {
    const source = ['- пункт', '', '  > внутри списка', '', '> верхняя'].join('\n');
    expect(blockquoteSources(source)).toEqual(['верхняя']);
  });

  it('separates_two_top_level_quotes', () => {
    const source = ['> первый', '', 'абзац', '', '> второй', '> продолжение'].join('\n');
    expect(blockquoteSources(source)).toEqual(['первый', 'второй\nпродолжение']);
  });

  it('does_not_add_a_trailing_newline', () => {
    expect(blockquoteSources('> один\n\n> два')).toEqual(['один', 'два']);
  });
});

describe('codeCopyText', () => {
  it('copies_the_code_without_line_numbers_or_an_added_newline', () => {
    const command = `Write-Host "${'x'.repeat(400)}"`;
    const html = createMarkdown().render(`\`\`\`powershell\n${command}\n\`\`\``);
    const host = new DOMParser().parseFromString(html, 'text/html').body;
    const pre = host.querySelector('pre')!;
    expect(codeCopyText(pre)).toBe(`${command}\n`);
  });

  it('ignores_extra_elements_inside_the_block', () => {
    const pre = document.createElement('pre');
    const code = document.createElement('code');
    code.textContent = 'echo привет';
    const button = document.createElement('button');
    button.textContent = 'Скопировать код';
    pre.append(code, button);
    expect(codeCopyText(pre)).toBe('echo привет');
  });
});
