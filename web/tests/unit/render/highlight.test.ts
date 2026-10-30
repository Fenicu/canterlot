import { describe, test, expect, vi } from 'vitest';
import { highlightLazy, highlightWithin, HIGHLIGHT_LIMITS } from '../../../src/lib/render/highlight';

describe('highlighting', () => {
  test('highlight_before_markdown_initialisation_filters_every_class_token', async () => {
    vi.resetModules();
    const shared = (await import('dompurify')).default;
    shared.removeAllHooks();
    const { highlightLazy: highlightFirst } = await import('../../../src/lib/render/highlight');
    const html = await highlightFirst('class Foo {} function bar() {}', 'javascript');
    expect(html).toContain('hljs-title');
    const doc = new DOMParser().parseFromString(html!, 'text/html');
    for (const element of doc.querySelectorAll('[class]')) {
      for (const token of element.classList) {
        expect(token.startsWith('hljs') || token.startsWith('language-'), token).toBe(true);
      }
    }
    const { createMarkdown } = await import('../../../src/lib/render/markdown');
    createMarkdown();
    expect(await highlightFirst('class Foo {} function bar() {}', 'javascript')).toBe(html);
    // Renderer configuration must not install hooks on the imported singleton.
    expect(shared.sanitize('<span class="outside">text</span>')).toContain('class="outside"');
  });

  test('renderers_use_private_sanitiser_without_mutating_shared_hooks', async () => {
    const shared = (await import('dompurify')).default;
    shared.addHook('afterSanitizeAttributes', node => {
      node.setAttribute('data-shared-hook', 'present');
    });
    try {
      const { createMarkdown } = await import('../../../src/lib/render/markdown');
      const { highlightLazy: highlight } = await import('../../../src/lib/render/highlight');
      const markdown = createMarkdown().render('[link](https://example.com)');
      const highlighted = await highlight('class Foo {}', 'javascript');
      expect(markdown).not.toContain('data-shared-hook');
      expect(highlighted).not.toContain('data-shared-hook');
      expect(shared.sanitize('<span class="outside">text</span>')).toContain('data-shared-hook="present"');
    } finally {
      shared.removeAllHooks();
    }
  });

  test('highlight_skips_large_blocks_and_unknown_languages_and_loads_the_language_lazily', async () => {
    // 1. Unknown language -> null
    const unknownRes = await highlightLazy('const x = 1;', 'unknownlang123');
    expect(unknownRes).toBeNull();

    // 2. Null or empty language -> null
    expect(await highlightLazy('const x = 1;', null)).toBeNull();
    expect(await highlightLazy('const x = 1;', '')).toBeNull();
    expect(await highlightLazy('const x = 1;', '   ')).toBeNull();

    // 3. Exceeding maxBytes (20480) -> null
    const hugeBytes = 'let a = 1;\n'.repeat(2500); // > 25000 bytes
    expect(new TextEncoder().encode(hugeBytes).length).toBeGreaterThan(HIGHLIGHT_LIMITS.maxBytes);
    const byteLimitRes = await highlightLazy(hugeBytes, 'typescript');
    expect(byteLimitRes).toBeNull();

    // 4. Exceeding maxLines (400) -> null
    const hugeLines = Array.from({ length: 450 }, (_, i) => `x = ${i}`).join('\n');
    expect(hugeLines.split('\n').length).toBeGreaterThan(HIGHLIGHT_LIMITS.maxLines);
    const lineLimitRes = await highlightLazy(hugeLines, 'python');
    expect(lineLimitRes).toBeNull();

    // 5. Valid supported languages load lazily and return highlighted HTML
    const rustRes = await highlightLazy('fn main() { println!("hi"); }', 'rust');
    expect(rustRes).not.toBeNull();
    expect(rustRes).toContain('hljs-keyword');

    const pyRes = await highlightLazy('def hello():\n    return 42', 'python');
    expect(pyRes).not.toBeNull();
    expect(pyRes).toContain('hljs-keyword');

    const bashRes = await highlightLazy('echo "test" && ls -la', 'bash');
    expect(bashRes).not.toBeNull();

    const jsonRes = await highlightLazy('{"key": "value", "num": 123}', 'json');
    expect(jsonRes).not.toBeNull();

    // highlight.js разбирает TOML языком ini.
    const tomlRes = await highlightLazy('[package]\nname = "canterlot"', 'toml');
    expect(tomlRes).toContain('hljs-');

    // Svelte maps to xml
    const svelteRes = await highlightLazy('<script>let x = 1;</script><div>{x}</div>', 'svelte');
    expect(svelteRes).not.toBeNull();
    expect(svelteRes).toContain('hljs-tag');
  });

  test('final_code_blocks_are_painted_in_place_and_repeat_mounts_paint_at_once', async () => {
    const root = document.createElement('div');
    const fill = () => {
      const pre = document.createElement('pre');
      const code = document.createElement('code');
      code.className = 'language-rust';
      code.textContent = 'fn main() {}';
      pre.append(code);
      root.replaceChildren(pre);
      return code;
    };
    const first = fill();
    highlightWithin(root);
    await vi.waitFor(() => expect(first.querySelector('.hljs-keyword')).not.toBeNull());
    expect(first.textContent).toBe('fn main() {}');
    const again = fill();
    highlightWithin(root);
    expect(again.classList.contains('hljs')).toBe(true);
  });

  test('highlighted_output_passes_through_the_dompurify_allow_list', async () => {
    // A snippet that contains HTML tags inside code
    const snippetWithHtml = 'const html = "<script>alert(1)</script><img src=x onerror=alert(2)>";';
    const highlighted = await highlightLazy(snippetWithHtml, 'javascript');
    expect(highlighted).not.toBeNull();

    // Must not contain any unescaped scripts or img tags
    expect(highlighted).not.toContain('<script>');
    expect(highlighted).not.toContain('<img');

    // Only allowlisted elements (span) and classes (hljs-*)
    const doc = new DOMParser().parseFromString(highlighted!, 'text/html');
    expect(doc.body.querySelector('img')).toBeNull();
    expect(doc.body.querySelector('script')).toBeNull();

    for (const el of doc.body.querySelectorAll('*')) {
      expect(el.tagName.toLowerCase()).toBe('span');
      for (const attr of el.attributes) {
        expect(attr.name).toBe('class');
        for (const token of attr.value.split(/\s+/)) {
          expect(token.startsWith('hljs') || token.startsWith('language-')).toBe(true);
        }
      }
    }
  });
});
