import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { trackTypedText } from '#lib/shell/unsaved.js';

function field<T extends 'input' | 'textarea'>(tag: T, parent: HTMLElement = document.body, type?: string): HTMLElementTagNameMap[T] {
  const node = document.createElement(tag);
  if (type) node.setAttribute('type', type);
  parent.append(node);
  return node;
}

function type(node: HTMLInputElement | HTMLTextAreaElement, value: string): void {
  node.value = value;
  node.dispatchEvent(new Event('input', { bubbles: true }));
}

function box(attribute: string, value = ''): HTMLElement {
  const node = document.createElement('div');
  node.setAttribute(attribute, value);
  document.body.append(node);
  return node;
}

describe('shell/unsaved typed text', () => {
  let tracker: ReturnType<typeof trackTypedText>;

  beforeEach(() => {
    document.body.replaceChildren();
    tracker = trackTypedText(document);
  });

  afterEach(() => tracker.destroy());

  it('sees text typed into a form that keeps no draft, until the form is gone', () => {
    const text = field('textarea');
    expect(tracker.pending()).toBe(false);

    type(text, 'длинная инструкция');
    expect(tracker.pending()).toBe(true);

    text.remove();
    expect(tracker.pending()).toBe(false);
  });

  it('counts single-line fields and ignores a field the owner emptied', () => {
    const name = field('input');
    type(name, 'разбор коммитов');
    expect(tracker.pending()).toBe(true);

    type(name, '');
    expect(tracker.pending()).toBe(false);
  });

  it('ignores a value the screen filled in by itself', () => {
    field('input').value = 'имя из хаба';
    expect(tracker.pending()).toBe(false);
  });

  it('leaves the composer to its draft and does not count search fields or switches', () => {
    type(field('textarea', box('data-composer')), 'сообщение');
    type(field('input', box('role', 'search')), 'запрос');
    type(field('input', document.body, 'search'), 'запрос');
    const toggle = field('input', document.body, 'checkbox');
    toggle.checked = true;
    toggle.dispatchEvent(new Event('input', { bubbles: true }));

    expect(tracker.pending()).toBe(false);
  });

  it('does not count a field left inside a closed sheet', () => {
    const dialog = document.createElement('dialog');
    document.body.append(dialog);
    const name = field('input', dialog);
    dialog.setAttribute('open', '');
    type(name, 'Новый телефон');
    expect(tracker.pending()).toBe(true);

    dialog.removeAttribute('open');
    expect(tracker.pending()).toBe(false);
  });

  it('stops listening after destroy', () => {
    tracker.destroy();
    type(field('textarea'), 'текст');
    expect(tracker.pending()).toBe(false);
  });
});
