import { describe, test, expect } from 'vitest';
import { createMarkdown } from '../../../src/lib/render/markdown';
import { StreamingMarkdown } from '../../../src/lib/render/streamingMarkdown';

const ALLOWED_TAGS = new Set([
  'p', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'ul', 'ol', 'li',
  'pre', 'code', 'em', 'strong', 'del', 'blockquote', 'hr',
  'table', 'thead', 'tbody', 'tr', 'th', 'td', 'a', 'br', 'span'
]);

const ALLOWED_ATTRS = new Set(['href', 'title', 'class', 'rel', 'target', 'start']);

function parseAndValidateHtml(html: string) {
  const doc = new DOMParser().parseFromString(html, 'text/html');
  const elements = doc.body.querySelectorAll('*');
  for (const el of elements) {
    const tag = el.tagName.toLowerCase();
    // 1. Tag must be in allow-list
    expect(ALLOWED_TAGS.has(tag), `Forbidden tag <${tag}> in HTML: ${html}`).toBe(true);

    // 2. No img tag
    expect(tag).not.toBe('img');

    // 3. No svg or mathml
    expect(tag).not.toBe('svg');
    expect(tag).not.toBe('math');

    // 4. Attributes check
    for (const attr of el.attributes) {
      const name = attr.name.toLowerCase();
      // No event handler
      expect(name.startsWith('on'), `Event handler ${name} found in HTML: ${html}`).toBe(false);
      // No style
      expect(name).not.toBe('style');
      // No srcset
      expect(name).not.toBe('srcset');
      // Allowed attribute
      expect(ALLOWED_ATTRS.has(name), `Forbidden attribute ${name} in HTML: ${html}`).toBe(true);

      if (name === 'class') {
        const classes = attr.value.split(/\s+/).filter(Boolean);
        for (const cls of classes) {
          expect(
            cls.startsWith('hljs') || cls.startsWith('language-'),
            `Forbidden class "${cls}" in HTML: ${html}`
          ).toBe(true);
        }
      }

      if (name === 'href') {
        const val = attr.value.trim().toLowerCase();
        expect(
          /^(https?:|mailto:|#)/.test(val),
          `Disallowed href "${attr.value}" in HTML: ${html}`
        ).toBe(true);
      }

      if (tag === 'a' && name === 'rel') {
        expect(attr.value).toContain('noopener');
        expect(attr.value).toContain('noreferrer');
        expect(attr.value).toContain('nofollow');
      }

      if (tag === 'a' && name === 'target') {
        expect(attr.value).toBe('_blank');
      }
    }
  }
}

describe('ordered list numbering', () => {
  test('list_keeps_the_number_it_starts_from', () => {
    const md = createMarkdown();
    expect(md.render('3. x')).toContain('<ol start="3">');
    expect(md.render('2. Каталог `/tmp` создан.')).toContain('<ol start="2">');
  });

  test('list_from_one_has_no_start_attribute', () => {
    expect(createMarkdown().render('1. x')).toContain('<ol>');
  });
});

describe('markdown and xss security', () => {
  const xssCorpus = [
    // 1-5: Classic script and image handlers
    '<script>alert(1)</script>',
    '<script src="http://evil.com/x.js"></script>',
    '<img src="x" onerror="alert(1)">',
    '<img src=x onerror=alert(1)>',
    '<img src="http://tracker.com/p.png">',

    // 6-10: Markdown image vectors
    '![t](http://tracker/p.png)',
    '![evil](javascript:alert(1))',
    '![xss](JaVaScRiPt:alert(1))',
    '![data](data:image/svg+xml,<svg onload=alert(1)>)',
    '<svg onload="alert(1)">',

    // 11-15: SVG and javascript: links
    '<svg><script>alert(1)</script></svg>',
    '[x](javascript:alert(1))',
    '[x](JaVaScRiPt:alert(1))',
    '[x](data:text/html,<script>alert(1)</script>)',
    '[x](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)',

    // 16-20: Vbscript, reference links, encoded schemes
    '[x](vbscript:alert(1))',
    '[x][1]\n\n[1]: javascript:alert(1)',
    '[x][ref]\n\n[ref]: vbscript:msgbox(1)',
    '[x](&#106;avascript:alert(1))',
    '[x](jav&#x09;ascript:alert(1))',

    // 21-25: Entity-encoded and control character links
    '[x](&Tab;javascript:alert(1))',
    '[x](&#x6a;&#x61;&#x76;&#x61;&#x73;&#x63;&#x72;&#x69;&#x70;&#x74;:alert(1))',
    '[x](\u0001javascript:alert(1))',
    '[x](\u0000javascript:alert(1))',
    '<iframe><script>alert(1)</script></iframe>',

    // 26-30: Iframes, styles, forms, base, meta
    '<iframe src="javascript:alert(1)"></iframe>',
    '<style>body { background: url("javascript:alert(1)"); }</style>',
    '<form action="http://evil.com"><input type="submit"></form>',
    '<base href="http://evil.com/">',
    '<meta http-equiv="refresh" content="0;url=http://evil.com">',

    // 31-35: Objects, embeds, links, raw a tags
    '<object data="javascript:alert(1)"></object>',
    '<embed src="javascript:alert(1)">',
    '<link rel="stylesheet" href="http://evil.com/style.css">',
    '<a href="javascript:alert(1)" onclick="alert(2)">click me</a>',
    '<button onclick="alert(1)">button</button>',

    // 36-40: Event handlers on diverse tags
    '<div onmouseover="alert(1)">hover me</div>',
    '<body onload="alert(1)">',
    '<input autofocus onfocus="alert(1)">',
    '<math><mtext><svg onload="alert(1)"></svg></mtext></math>',
    '<video><source onerror="alert(1)"></video>',

    // 41-50: Additional vectors beyond the 40 required
    '<audio src="x" onerror="alert(1)"></audio>',
    '[x](feed:javascript:alert(1))',
    '[x](blob:http://evil.com/uuid)',
    '<textarea oninput="alert(1)"></textarea>',
    '<details open ontoggle="alert(1)"><summary>s</summary></details>',
    '<marquee onstart="alert(1)">scroll</marquee>',
    '<select onchange="alert(1)"><option>1</option></select>',
    '[x](javascript%3Aalert(1))',
    '<div style="width: expression(alert(1));">expression</div>',
    '<div style="background-image: url(\'javascript:alert(1)\')">bg</div>',
    '[x]( java script:alert(1))',
    '<svg><animate onbegin=alert(1) attributeName=x dur=1s>'
  ];

  test('xss_corpus_produces_no_script_handlers_or_javascript_urls', () => {
    expect(xssCorpus.length).toBeGreaterThanOrEqual(40);
    const md = createMarkdown();

    for (const vector of xssCorpus) {
      const output = md.render(vector);
      parseAndValidateHtml(output);
    }
  });

  test('links_open_in_a_new_context_with_noopener_and_nofollow', () => {
    const md = createMarkdown();
    const doc = [
      '[standard link](https://example.com)',
      'https://autolink.org',
      '[mailto link](mailto:test@canterlot.local)',
      '[anchor link](#section-1)'
    ].join('\n\n');

    const output = md.render(doc);
    const parsedDoc = new DOMParser().parseFromString(output, 'text/html');

    const links = parsedDoc.body.querySelectorAll('a');
    expect(links.length).toBe(4);

    for (const a of links) {
      expect(a.getAttribute('target')).toBe('_blank');
      const rel = a.getAttribute('rel') || '';
      expect(rel.split(/\s+/)).toEqual(expect.arrayContaining(['noopener', 'noreferrer', 'nofollow']));
      expect(/^(https?:|mailto:|#)/.test(a.getAttribute('href') || '')).toBe(true);
    }
  });

  test('final_renders_the_whole_block_once_and_matches_a_plain_render', () => {
    const md = createMarkdown();
    const sm = new StreamingMarkdown(md);

    const testCorpus = [
      '# Heading 1\n\nParagraph text with **bold** and *italic* and `code`.\n\n## Subheading\n\nAnother paragraph.',
      '1. Item one\n2. Item two\n   continuation line\n3. Item three',
      '- Bullet A\n- Bullet B\n  - Nested B1\n  - Nested B2',
      '```typescript\nfunction add(a: number, b: number): number {\n  return a + b;\n}\n```\n\nPost code paragraph.',
      '> Blockquote level 1\n>\n> Second paragraph in quote',
      '| Col A | Col B |\n| --- | --- |\n| Val 1 | Val 2 |\n| Val 3 | Val 4 |'
    ];

    for (const sample of testCorpus) {
      const plain = md.render(sample);
      const streamingFinal = sm.update(sample, true);

      expect(streamingFinal.chunks).toHaveLength(1);
      const firstChunk = streamingFinal.chunks[0];
      expect(firstChunk?.id).toBe('final');
      expect(firstChunk?.html).toBe(plain);
      expect(streamingFinal.tail.html).toBe('');
      expect(streamingFinal.tail.plain).toBe(false);
    }
  });
});
