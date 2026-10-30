import { describe, test, expect, vi } from 'vitest';
import { splitStable, StreamingMarkdown } from '../../../src/lib/render/streamingMarkdown';
import { createMarkdown } from '../../../src/lib/render/markdown';
import { highlightLazy } from '../../../src/lib/render/highlight';

describe('streaming markdown', () => {
  test('small_appends_scan_linear_work_and_reuse_closed_chunk_objects', () => {
    const work = (count: number, unfinishedLine = false) => {
      let scanned = 0;
      const indexOf = String.prototype.indexOf;
      const charCodeAt = String.prototype.charCodeAt;
      const lines = vi.spyOn(String.prototype, 'indexOf').mockImplementation(function (
        this: string, search: string, start = 0
      ) {
        const end = indexOf.call(this, search, start);
        if (search === '\n') scanned += (end < 0 ? this.length : end + 1) - start;
        return end;
      });
      const characters = vi.spyOn(String.prototype, 'charCodeAt').mockImplementation(function (
        this: string, index: number
      ) {
        scanned++;
        return charCodeAt.call(this, index);
      });
      const sm = new StreamingMarkdown({ render: text => text });
      let text = '';
      try {
        for (let i = 0; i < count; i++) {
          const appended = unfinishedLine ? 'x'.repeat(16) : `Paragraph ${i}\n\n`;
          text += appended;
          sm.update(text, false);
          splitStable(text, text.length - appended.length);
        }
      } finally {
        lines.mockRestore();
        characters.mockRestore();
      }
      return scanned;
    };
    expect(work(1024)).toBeLessThan(work(256) * 6);
    expect(work(1024, true)).toBeLessThan(work(256, true) * 6);

    const sm = new StreamingMarkdown({ render: text => text });
    const first = sm.update('First\n\nSecond', false);
    const closed = first.chunks[0];
    const second = sm.update('First\n\nSecond\n\nThird', false);
    expect(second.chunks[0]).toBe(closed);
    expect(second.chunks).toBe(first.chunks);
  });

  test('streaming_resets_offsets_fences_and_chunks_when_text_is_replaced', () => {
    const md = createMarkdown();
    const sm = new StreamingMarkdown(md);
    sm.update('Old\n\n```text\nopen', false);
    for (const text of ['New\n\nTail', '', 'New\n\n~~~\ncode\n~~~\n\nEnd']) {
      const result = sm.update(text, false);
      expect(result.chunks.map(chunk => chunk.html).join('') + result.tail.html).toBe(md.render(text));
    }
  });

  test.each([
    ['tilde', '~~~text\nsource\n\n[verify](https://evil.example/login)\n~~~'],
    ['three_space_indent', '   ```text\nsource\n\n[verify](https://evil.example/login)\n   ```'],
    ['longer_opening', '````text\n```\n\n[verify](https://evil.example/login)\n````'],
    ['invalid_closing_info', '```text\n```not-a-close\n\n[verify](https://evil.example/login)\n```'],
    ['invalid_closing_indent', '~~~\n    ~~~\n\n[verify](https://evil.example/login)\n~~~'],
    ['different_marker', '~~~\n```\n\n[verify](https://evil.example/login)\n~~~'],
    ['longer_closing_with_tabs', '~~~text\nsource\n\n[verify](https://evil.example/login)\n~~~~ \t'],
    ['crlf', '~~~text\r\nsource\r\n\r\n[verify](https://evil.example/login)\r\n~~~'],
    ['tab_list_continuation', '- item\n\n\t~~~text\n\tsource\n\n\t[verify](https://evil.example/login)\n\t~~~'],
    ['space_tab_list_continuation', '- item\n\n \t~~~text\n \tsource\n\n \t[verify](https://evil.example/login)\n \t~~~'],
    ['outdent_from_list_opens_outer_fence', '- item\n\n  ~~~text\n  source\n~~~\n\n[verify](https://evil.example/login)\n~~~'],
    ['outdent_from_nested_list_opens_outer_fence', '- outer\n  - inner\n\n    ~~~text\n    source\n  ~~~\n\n  [verify](https://evil.example/login)\n  ~~~']
  ])('streamed_fence_matches_full_render_%s', (_name, payload) => {
    const md = createMarkdown();
    const sm = new StreamingMarkdown(md);
    const text = `Intro\n\n${payload}\n\nAfter`;
    for (let i = 1; i <= text.length; i++) {
      const prefix = text.slice(0, i);
      const result = sm.update(prefix, false);
      const streamed = result.chunks.map(chunk => chunk.html).join('') + result.tail.html;
      expect(streamed, `prefix ${i}: ${prefix}`).toBe(md.render(prefix));
      expect(new DOMParser().parseFromString(streamed, 'text/html').querySelector('a')).toBeNull();
    }
  });

  test('splitter_distinguishes_fence_info_strings_and_indented_code', () => {
    expect(splitStable('```bad`info\ntext').openFence).toBe(false);
    expect(splitStable('~~~info`allowed\ntext').openFence).toBe(true);
    expect(splitStable('    ```\ntext').openFence).toBe(false);
    expect(splitStable('\t```\ntext').openFence).toBe(false);
    expect(splitStable('~~~\ncode\n~~~not-close').openFence).toBe(true);
    expect(splitStable('- ~~~\n  code').openFence).toBe(true);
    expect(splitStable('- ~~~\n  code\n  ~~~').openFence).toBe(false);
    const text = 'First\n\nSecond\n\nThird';
    const all = splitStable(text).boundaries;
    expect(splitStable(text, 8).boundaries).toEqual(all.filter(boundary => boundary >= 8));
  });

  test('split_stable_never_splits_inside_a_fence_or_before_a_list_marker_and_is_append_only', () => {
    // 1. Text with fences, lists, indented continuation, paragraphs
    const sample = [
      '# Introduction',
      '',
      'First paragraph of the text.',
      '',
      '```typescript',
      'function test() {',
      '  // blank line inside code fence',
      '  ',
      '  return true;',
      '}',
      '```',
      '',
      'Post-code paragraph.',
      '',
      '1. First ordered item',
      '2. Second ordered item',
      '   indented continuation line',
      '',
      '3. Third ordered item after blank line',
      '',
      '- Unordered bullet',
      '  continuation',
      '',
      'Final concluding paragraph.'
    ].join('\n');

    // Test append-only property: character by character growth
    let previousBoundaries: number[] = [];
    for (let i = 1; i <= sample.length; i++) {
      const prefix = sample.slice(0, i);
      const { boundaries, openFence } = splitStable(prefix);

      // Boundaries must be strictly monotonic
      for (let j = 1; j < boundaries.length; j++) {
        const curr = boundaries[j];
        const prev = boundaries[j - 1];
        if (curr !== undefined && prev !== undefined) {
          expect(curr).toBeGreaterThan(prev);
        }
      }

      // Append-only: all previous boundaries must be preserved at exact same positions
      for (let j = 0; j < previousBoundaries.length; j++) {
        expect(boundaries[j], `Boundary moved at prefix length ${i}`).toBe(previousBoundaries[j]);
      }

      // Check openFence correctness
      const backtickCount = (prefix.match(/^```/gm) || []).length;
      if (backtickCount % 2 === 1) {
        expect(openFence).toBe(true);
      } else {
        expect(openFence).toBe(false);
      }

      previousBoundaries = boundaries;
    }

    // Check final boundaries
    const finalResult = splitStable(sample);
    expect(finalResult.openFence).toBe(false);

    // Verify none of the boundaries split inside the fence or before list markers
    for (const b of finalResult.boundaries) {
      const sliceBefore = sample.slice(0, b);
      const sliceAfter = sample.slice(b);

      // Must not be inside fence
      const fenceCountBefore = (sliceBefore.match(/^```/gm) || []).length;
      expect(fenceCountBefore % 2).toBe(0);

      // Next line must not be a list marker
      const nextLine = sliceAfter.split('\n')[0] || '';
      expect(/^\s*[-*+]\s/.test(nextLine)).toBe(false);
      expect(/^\s*\d+[.)]\s/.test(nextLine)).toBe(false);
      // Next line must not be indented continuation
      expect(/^\s{2,}\S/.test(nextLine)).toBe(false);
    }
  });

  test('streaming_markdown_rerenders_only_the_open_chunk', () => {
    const md = createMarkdown();
    const sm = new StreamingMarkdown(md);

    // 20 paragraphs, simulated 1000 appended tokens
    const paragraphs = Array.from({ length: 20 }, (_, i) =>
      `Paragraph ${i + 1}: This is the content of paragraph number ${i + 1} with various words to simulate real streaming text generation.`
    );

    let currentText = '';
    let totalCacheHits = 0;
    let lastStats = { rendered: 0, cached: 0, tailBytes: 0 };

    // Stream paragraph by paragraph, token by token
    for (let pIdx = 0; pIdx < paragraphs.length; pIdx++) {
      const p = paragraphs[pIdx] + '\n\n';
      const words = p.split(' ');

      for (const word of words) {
        currentText += word + ' ';
        const res = sm.update(currentText, false);
        totalCacheHits += res.stats.cached;
        lastStats = res.stats;
      }
    }

    // Review Focus 3 assertions:
    // 1. Stable closed chunks are rendered once each
    // 2. Cache hits are counted on each step for already closed chunks
    expect(lastStats.rendered).toBeGreaterThanOrEqual(20);
    expect(totalCacheHits).toBeGreaterThan(100);
    expect(lastStats.cached).toBeGreaterThan(0);

    // After all 20 paragraphs are completed, chunks count should match stable closed chunks
    const finalRes = sm.update(currentText, false);
    expect(finalRes.chunks.length).toBeGreaterThanOrEqual(18);
  });

  test('incomplete_fence_renders_as_plain_code_until_closed_then_highlights_once', async () => {
    const md = createMarkdown();
    const sm = new StreamingMarkdown(md);

    // 1. Streaming incomplete fence: ```typescript without closing ```
    const incomplete = 'Introductory text\n\n```typescript\nconst message: string = "hello";\nconsole.log(message);';
    const splitIncomplete = splitStable(incomplete);
    expect(splitIncomplete.openFence).toBe(true);

    const resIncomplete = sm.update(incomplete, false);
    // Incomplete fence must be rendered in open tail as plain code (no hljs spans)
    expect(resIncomplete.tail.html).toContain('<pre>');
    expect(resIncomplete.tail.html).toContain('<code');
    expect(resIncomplete.tail.html).not.toContain('hljs-keyword');

    // 2. Close the fence
    const closed = incomplete + '\n```\n\nNext section paragraph.';
    const splitClosed = splitStable(closed);
    expect(splitClosed.openFence).toBe(false);

    const resClosed = sm.update(closed, false);
    // Now the closed fence is in a closed stable chunk
    expect(resClosed.chunks.length).toBeGreaterThan(0);
    const codeChunk = resClosed.chunks.find(c => c.html.includes('<pre>'));
    expect(codeChunk).toBeDefined();

    // 3. Highlight closed fence once
    const highlighted = await highlightLazy('const message: string = "hello";', 'typescript');
    expect(highlighted).toContain('hljs-keyword');

    // Second highlight call with same code should hit cache / run once
    const highlighted2 = await highlightLazy('const message: string = "hello";', 'typescript');
    expect(highlighted2).toBe(highlighted);
  });

  test('tail_over_8_kib_takes_the_plain_fast_path', () => {
    const md = createMarkdown();
    const sm = new StreamingMarkdown(md);

    // Large tail without blank lines (> 8 KiB = 8192 bytes)
    const largeTail = 'A'.repeat(9000);
    const res = sm.update(largeTail, false);

    expect(res.stats.tailBytes).toBeGreaterThan(8192);
    expect(res.tail.plain).toBe(true);
    expect(res.tail.html).toContain('AAAA');

    // Special characters in large tail must be escaped
    const specialLargeTail = '<b>dangerous</b> ' + 'X'.repeat(8500);
    const resSpecial = sm.update(specialLargeTail, false);
    expect(resSpecial.tail.plain).toBe(true);
    expect(resSpecial.tail.html).toContain('&lt;b&gt;dangerous&lt;/b&gt;');
    expect(resSpecial.tail.html).not.toContain('<b>dangerous</b>');
  });

  test('plain_tail_carries_the_raw_text_for_a_text_node', () => {
    const sm = new StreamingMarkdown(createMarkdown());
    const raw = '<i>сырой</i> ' + 'Я'.repeat(5000);
    const res = sm.update(raw, false);
    expect(res.stats.tailBytes).toBe(new TextEncoder().encode(raw).length);
    expect(res.tail.plain).toBe(true);
    expect(res.tail.text).toBe(raw);
    expect(sm.update('коротко', false).tail.text).toBeUndefined();
  });
});
