import { afterEach, describe, expect, it } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import SessionMessageBubble from '#lib/timeline/SessionMessageBubble.svelte';
import { freshInput, type InputOrigin, type InputView } from '#lib/stores/inputs.js';

const SPAWN: InputOrigin = { kind: 'spawn', from: 'p1', title: 'Оркестратор', model: 'opus', chain_depth: 0 };
const MESSAGE: InputOrigin = { kind: 'message', from: 'p1', title: 'Сборка', model: 'sonnet-5', chain_depth: 1 };
const HEADER =
  '[canterlot] You were started by session "Оркестратор" (id p1, claude_code, model opus). It is an agent, not the owner.';

let instance: ReturnType<typeof mount> | undefined;

function render(origin: InputOrigin, text: string): HTMLElement {
  const input: InputView = freshInput('k1', text, Date.parse('2026-11-10T12:00:00.000Z'));
  input.state = 'accepted';
  input.placement = 'feed';
  input.origin = origin;
  const target = document.createElement('div');
  document.body.appendChild(target);
  instance = mount(SessionMessageBubble, { target, props: { input } });
  flushSync();
  return target;
}

function card(target: HTMLElement): HTMLElement {
  const node = target.querySelector<HTMLElement>('[data-role="session-message"]');
  if (!node) throw new Error('карточки нет');
  return node;
}

/** jsdom не считает раскладку: тело карточки получает высоты вручную. */
function stubFoldHeights(): void {
  Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
    configurable: true,
    get(this: HTMLElement) {
      return this.classList.contains('body') ? 270 : 0;
    }
  });
  Object.defineProperty(HTMLElement.prototype, 'scrollHeight', {
    configurable: true,
    get(this: HTMLElement) {
      return this.classList.contains('body') && this.querySelector('pre') ? 900 : 50;
    }
  });
}

afterEach(() => {
  if (instance) void unmount(instance);
  instance = undefined;
  delete (HTMLElement.prototype as { clientHeight?: unknown }).clientHeight;
  delete (HTMLElement.prototype as { scrollHeight?: unknown }).scrollHeight;
  document.body.textContent = '';
});

describe('timeline/SessionMessageBubble markdown', () => {
  it('a_brief_body_renders_with_the_agent_markdown_pipeline', () => {
    const text = `${HEADER}\n\n## Goal\n\n**Your area:** the \`timeline\` cards.\n\n- первый\n- второй\n\n\`\`\`ts\nconst step = 1;\n\`\`\``;
    const target = render(SPAWN, text);
    const body = card(target);

    expect(body.querySelector('h2')?.textContent).toBe('Goal');
    expect(body.querySelector('strong')?.textContent).toBe('Your area:');
    expect(body.querySelector('p code')?.textContent).toBe('timeline');
    expect(body.querySelector('pre code')?.textContent).toContain('const step = 1;');
    expect(body.querySelectorAll('li')).toHaveLength(2);
    expect(body.textContent).not.toContain('## Goal');
    expect(body.textContent).not.toContain('**Your area:**');
    expect(body.textContent).not.toContain('[canterlot]');
  });

  it('a_message_body_renders_as_markdown_too', () => {
    const text = '[canterlot] Message from session "Сборка".\n\nПроверь **индексы** и `миграцию`.';
    const body = card(render(MESSAGE, text));
    expect(body.querySelector('strong')?.textContent).toBe('индексы');
    expect(body.querySelector('p code')?.textContent).toBe('миграцию');
    expect(body.textContent).not.toContain('**индексы**');
  });

  it('a_long_brief_folds_by_height_and_the_toggle_reveals_the_whole_fence', () => {
    stubFoldHeights();
    const steps = Array.from({ length: 18 }, (_, index) => `Шаг ${index + 1}: проверь и закоммить.`).join('\n');
    const text = `${HEADER}\n\n## Goal\n\n\`\`\`ts\n${steps}\n\`\`\``;
    const body = card(render(SPAWN, text)).querySelector<HTMLElement>('.body')!;
    const toggle = body.parentElement!.querySelector<HTMLButtonElement>('button.toggle');
    expect(body.classList.contains('folded')).toBe(true);
    expect(toggle?.getAttribute('aria-expanded')).toBe('false');
    expect(toggle?.textContent?.trim()).toBe('Показать полностью');
    // Забор не разрезан: весь код лежит в одном блоке, прячет его только высота.
    expect(body.querySelectorAll('pre')).toHaveLength(1);
    expect(body.querySelector('pre')?.textContent).toContain('Шаг 18');

    toggle!.click();
    flushSync();
    expect(toggle?.getAttribute('aria-expanded')).toBe('true');
    expect(toggle?.textContent?.trim()).toBe('Свернуть');
    expect(body.classList.contains('folded')).toBe(false);

    toggle!.click();
    flushSync();
    expect(toggle?.getAttribute('aria-expanded')).toBe('false');
    expect(body.classList.contains('folded')).toBe(true);
  });

  it('a_short_brief_never_shows_a_toggle', () => {
    stubFoldHeights();
    const body = card(render(SPAWN, `${HEADER}\n\nКороткое задание.`));
    expect(body.querySelector('button.toggle')).toBeNull();
  });

  it('a_short_message_never_shows_a_toggle', () => {
    const body = card(render(MESSAGE, '[canterlot] Message from session "Сборка".\n\nПроверь миграцию.'));
    expect(body.querySelector('button.toggle')).toBeNull();
  });

  it('script_and_onerror_payloads_are_sanitised_like_agent_text', () => {
    const payload = '<script>alert(1)</script>\n\n<img src=x onerror="alert(2)">\n\n[клик](javascript:alert(3))';
    const body = card(render(SPAWN, `${HEADER}\n\n${payload}`));

    expect(body.querySelector('script')).toBeNull();
    expect(body.querySelector('img')).toBeNull();
    expect(body.querySelector('[onerror]')).toBeNull();
    expect(body.querySelector('a[href^="javascript"]')).toBeNull();
    // Разметка из брифа остаётся текстом, а не узлами.
    expect(body.textContent).toContain('<script>alert(1)</script>');
  });
});
