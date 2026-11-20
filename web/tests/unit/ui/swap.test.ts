import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const motion = vi.hoisted(() => ({ out: 100, inn: 200 }));
vi.mock('../../../src/lib/motion', () => ({
  durationMs: (token: string) => (token === '--duration-label-out' ? motion.out : token === '--duration-label-in' ? motion.inn : 0)
}));

import { swapOn } from '../../../src/lib/ui/swap';

interface FakeAnimation {
  keyframes: Keyframe[];
  onfinish: (() => void) | null;
  cancelled: boolean;
  cancel(): void;
  finish(): void;
}

let animations: FakeAnimation[];
let host: HTMLElement;
let node: HTMLElement;

function install(el: HTMLElement) {
  (el as unknown as { animate: unknown }).animate = vi.fn((keyframes: Keyframe[]) => {
    const animation: FakeAnimation = {
      keyframes,
      onfinish: null,
      cancelled: false,
      cancel() {
        this.cancelled = true;
      },
      finish() {
        this.onfinish?.();
      }
    };
    animations.push(animation);
    return animation;
  });
}

beforeEach(() => {
  motion.out = 100;
  motion.inn = 200;
  animations = [];
  host = document.createElement('div');
  node = document.createElement('span');
  host.append(node);
  document.body.append(host);
  install(node);
});

afterEach(() => {
  document.body.textContent = '';
});

describe('swapOn', () => {
  it('writes_the_first_word_without_motion', () => {
    swapOn(node, { text: 'идёт', key: 'running' });
    expect(node.textContent).toBe('идёт');
    expect(animations).toHaveLength(0);
  });

  it('a_changed_key_leaves_up_then_writes_and_arrives_from_below', () => {
    const swap = swapOn(node, { text: 'идёт', key: 'running' });
    swap.update({ text: '2 записи', key: 'done' });
    expect(animations).toHaveLength(1);
    expect(node.textContent).toBe('идёт');
    animations[0]!.finish();
    expect(node.textContent).toBe('2 записи');
    expect(animations).toHaveLength(2);
    expect(animations[1]!.keyframes[0]).toMatchObject({ opacity: 0 });
  });

  it('the_same_key_rewrites_in_place_without_animating', () => {
    const swap = swapOn(node, { text: '12 мин', key: 'waiting' });
    swap.update({ text: '13 мин', key: 'waiting' });
    expect(node.textContent).toBe('13 мин');
    expect(animations).toHaveLength(0);
  });

  it('the_text_is_written_into_the_one_existing_text_node', () => {
    const swap = swapOn(node, { text: '12 мин', key: 'waiting' });
    const text = node.firstChild;
    swap.update({ text: '13 мин', key: 'waiting' });
    expect(node.firstChild).toBe(text);
    expect(node.childNodes).toHaveLength(1);
    swap.update({ text: 'готово', key: 'done' });
    animations[0]!.finish();
    expect(node.firstChild).toBe(text);
    expect(node.textContent).toBe('готово');
  });

  it('a_change_during_the_leaving_animation_is_picked_up_by_it', () => {
    const swap = swapOn(node, { text: 'идёт', key: 'running' });
    swap.update({ text: 'ошибка', key: 'error' });
    swap.update({ text: 'прервано', key: 'interrupted' });
    expect(animations).toHaveLength(1);
    animations[0]!.finish();
    expect(node.textContent).toBe('прервано');
  });

  it('an_emptied_word_removes_the_element_after_it_has_left', () => {
    const swap = swapOn(node, { text: 'идёт', key: 'running' });
    swap.update({ text: '', key: 'done' });
    expect(node.isConnected).toBe(true);
    animations[0]!.finish();
    expect(node.isConnected).toBe(false);
    expect(host.textContent).toBe('');
    expect(animations).toHaveLength(1);
  });

  it('a_word_that_returns_puts_the_element_back_in_its_place_and_arrives', () => {
    host.prepend(document.createElement('i'));
    host.append(document.createElement('b'));
    const swap = swapOn(node, { text: 'идёт', key: 'running' });
    swap.update({ text: '', key: 'done' });
    animations[0]!.finish();
    expect(node.isConnected).toBe(false);
    swap.update({ text: 'ошибка', key: 'error' });
    expect(node.isConnected).toBe(true);
    expect([...host.children].map((c) => c.tagName)).toEqual(['I', 'SPAN', 'B']);
    expect(node.textContent).toBe('ошибка');
    expect(animations).toHaveLength(2);
    expect(animations[1]!.keyframes[0]).toMatchObject({ opacity: 0 });
  });

  it('a_word_that_starts_empty_is_not_in_the_dom_at_all', () => {
    const swap = swapOn(node, { text: '', key: 'done' });
    expect(node.isConnected).toBe(false);
    swap.update({ text: '', key: 'done' });
    expect(node.isConnected).toBe(false);
    swap.destroy();
    expect(host.childNodes).toHaveLength(0);
  });

  it('destroy_leaves_no_placeholder_and_cancels_what_runs', () => {
    const swap = swapOn(node, { text: 'идёт', key: 'running' });
    swap.update({ text: '', key: 'done' });
    animations[0]!.finish();
    swap.destroy();
    expect(host.childNodes).toHaveLength(0);
    const other = document.createElement('span');
    host.append(other);
    install(other);
    const second = swapOn(other, { text: 'a', key: 'a' });
    second.update({ text: 'b', key: 'b' });
    second.destroy();
    expect(animations.at(-1)!.cancelled).toBe(true);
  });

  describe('under_reduced_motion', () => {
    beforeEach(() => {
      motion.out = 0;
      motion.inn = 0;
    });

    it('the_word_changes_at_once_with_no_animation', () => {
      const swap = swapOn(node, { text: 'идёт', key: 'running' });
      swap.update({ text: '2 записи', key: 'done' });
      expect(node.textContent).toBe('2 записи');
      expect(animations).toHaveLength(0);
    });

    it('an_emptied_word_leaves_the_dom_at_once', () => {
      const swap = swapOn(node, { text: 'идёт', key: 'running' });
      swap.update({ text: '', key: 'done' });
      expect(node.isConnected).toBe(false);
      expect(animations).toHaveLength(0);
    });
  });
});
