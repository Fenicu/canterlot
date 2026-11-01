import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import UserBubble from '#lib/timeline/UserBubble.svelte';
import { originLabel } from '#lib/timeline/origin.js';
import { freshInput, type InputOrigin } from '#lib/stores/inputs.js';
import type { ChatStore } from '#lib/stores/chat.svelte.js';

vi.mock('#lib/ui/Sheet.svelte', () => ({ default: () => undefined }));
vi.mock('#lib/app/runtime.js', () => ({
  getRuntime: () => ({ journal: { ops: new Map(), retryNow: vi.fn() }, drafts: {} })
}));

describe('timeline/UserBubble origin', () => {
  let instance: ReturnType<typeof mount> | undefined;
  beforeEach(() => {
    vi.stubGlobal('matchMedia', (query: string) => ({
      matches: query.includes('reduce'),
      media: query,
      addEventListener() {},
      removeEventListener() {}
    }));
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = '';
  });

  function render(origin: InputOrigin | null) {
    const input = freshInput('k1', 'Сделай миграцию', 1);
    input.state = 'accepted';
    input.placement = 'feed';
    input.origin = origin;
    const target = document.createElement('div');
    document.body.appendChild(target);
    instance = mount(UserBubble, { target, props: { input, chat: { id: 'c' } as unknown as ChatStore } });
    flushSync();
    return target;
  }

  it('bubble_shows_origin_label', () => {
    const message = render({ kind: 'message', from: 'p1', title: 'Оркестратор', model: 'sonnet-5', harness: 'claude_code', chain_depth: 1 });
    expect(message.querySelector('.origin')?.textContent).toBe('От сессии Оркестратор · модель sonnet-5');
    expect(message.textContent).toContain('Сделай миграцию');
  });

  it('a_result_event_is_labelled_as_a_session_result', () => {
    const target = render({ kind: 'event', from: 'c1', title: 'Миграция', model: 'haiku', chain_depth: 0 });
    expect(target.querySelector('.origin')?.textContent).toBe('Итог сессии Миграция');
  });

  it('owner_input_has_no_label_and_no_origin_marker', () => {
    const target = render(null);
    expect(target.querySelector('.origin')).toBeNull();
    expect(target.querySelector('[data-origin]')).toBeNull();
  });

  it('a_deleted_sender_is_named_without_a_title', () => {
    expect(originLabel({ kind: 'message', sender_deleted: true, chain_depth: 1 })).toBe('От удалённой сессии');
    expect(originLabel({ kind: 'event', sender_deleted: true, chain_depth: 0 })).toBe('Итог удалённой сессии');
  });
});
