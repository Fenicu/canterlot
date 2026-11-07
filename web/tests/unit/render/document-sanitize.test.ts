import { describe, expect, it } from 'vitest';
import { sanitizeDocument } from '#lib/render/documentSanitize.js';
import { sanitizeRendered } from '#lib/render/sanitize.js';

const host = (html: string) => new DOMParser().parseFromString(`<body>${html}</body>`, 'text/html').body;

describe('render/sanitizeDocument', () => {
  it('keeps_the_semantic_markup_mammoth_produces', () => {
    const html = '<h1>Отчёт</h1><p><strong>Жирный</strong> и <em>курсив</em><sup>1</sup></p><ul><li>пункт</li></ul><ol><li>шаг</li></ol><table><tr><td colspan="2"><p>ячейка</p></td></tr></table>';
    const out = host(sanitizeDocument(html));
    expect(out.querySelector('h1')?.textContent).toBe('Отчёт');
    expect(out.querySelector('strong')?.textContent).toBe('Жирный');
    expect(out.querySelector('em')?.textContent).toBe('курсив');
    expect(out.querySelector('sup')?.textContent).toBe('1');
    expect(out.querySelectorAll('li')).toHaveLength(2);
    expect(out.querySelector('td')?.getAttribute('colspan')).toBe('2');
  });

  it('strips_scripts_styles_and_event_handlers', () => {
    const html = '<p onclick="alert(1)" style="color:red" class="x" id="top">текст</p><script>window.__ran = true</script><style>p{}</style><img src="docx-image:0" onerror="alert(2)"><svg onload="alert(3)"></svg><iframe src="https://example.com"></iframe>';
    const out = host(sanitizeDocument(html, ['blob:http://localhost/1']));
    const p = out.querySelector('p')!;
    expect(p.textContent).toBe('текст');
    for (const name of ['onclick', 'style', 'class', 'id']) expect(p.hasAttribute(name)).toBe(false);
    expect(out.querySelector('script, style, svg, iframe')).toBeNull();
    expect(out.querySelector('img')?.hasAttribute('onerror')).toBe(false);
    expect((window as unknown as { __ran?: boolean }).__ran).toBeUndefined();
  });

  it('drops_javascript_and_data_links_and_opens_safe_ones_in_a_new_tab_without_referrer', () => {
    const html = [
      '<a href="javascript:alert(1)">js</a>',
      '<a href=" JaVaScRiPt:alert(1)">js2</a>',
      '<a href="java&#x09;script:alert(1)">js3</a>',
      '<a href="data:text/html,<b>x</b>">data</a>',
      '<a href="#footnote-1">сноска</a>',
      '<a href="https://example.com/docs">docs</a>',
      '<a href="mailto:owner@example.com">mail</a>'
    ].join('');
    const out = host(sanitizeDocument(html));
    const links = [...out.querySelectorAll('a')];
    expect(links.map((link) => link.textContent)).toEqual(['js', 'js2', 'js3', 'data', 'сноска', 'docs', 'mail']);
    expect(links.slice(0, 5).every((link) => !link.hasAttribute('href'))).toBe(true);
    const docs = links[5]!;
    expect(docs.getAttribute('href')).toBe('https://example.com/docs');
    expect(docs.getAttribute('target')).toBe('_blank');
    expect(docs.getAttribute('rel')).toContain('noopener');
    expect(docs.getAttribute('rel')).toContain('noreferrer');
    expect(links[6]!.getAttribute('href')).toBe('mailto:owner@example.com');
  });

  it('shows_only_images_from_the_file_through_the_given_blob_urls', () => {
    const html = '<p><img src="docx-image:0" alt="Схема"><img src="docx-image:5"><img src="https://tracker.example/pixel.png"><img src="data:image/png;base64,AAAA"><img src="file:///etc/passwd"></p>';
    const out = host(sanitizeDocument(html, ['blob:http://localhost/abc']));
    const images = [...out.querySelectorAll('img')];
    expect(images).toHaveLength(1);
    expect(images[0]!.getAttribute('src')).toBe('blob:http://localhost/abc');
    expect(images[0]!.getAttribute('alt')).toBe('Схема');
  });

  it('a_blob_url_list_from_one_call_does_not_leak_into_the_next', () => {
    sanitizeDocument('<img src="docx-image:0">', ['blob:http://localhost/one']);
    expect(host(sanitizeDocument('<img src="docx-image:0">')).querySelector('img')).toBeNull();
  });

  it('does_not_change_how_markdown_is_sanitised', () => {
    const out = host(sanitizeRendered('<a href="#x">якорь</a><img src="docx-image:0">', 'markdown'));
    expect(out.querySelector('a')?.getAttribute('href')).toBe('#x');
    expect(out.querySelector('img')).toBeNull();
  });
});
