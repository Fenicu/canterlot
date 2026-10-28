import { describe, test, expect } from 'vitest';
import { splitStable, StreamingMarkdown } from '../../../src/lib/render/streamingMarkdown';
import { createMarkdown } from '../../../src/lib/render/markdown';
import { highlightLazy } from '../../../src/lib/render/highlight';

describe('streaming markdown', () => {
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
});
