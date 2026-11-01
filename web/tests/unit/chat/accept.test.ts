import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount } from 'svelte';
import WorktreeNotice from '#lib/screens/chat/WorktreeNotice.svelte';
import { acceptView } from '#lib/screens/chat/accept.js';

afterEach(() => {
  document.body.textContent = '';
});

describe('chat/accept', () => {
  it('accept_shows_dirty_state', () => {
    const view = acceptView({ spawn_depth: 1, worktree: { branch: 'canterlot/ab', state: 'dirty', path: '/w/x' } });
    expect(view.dirty).toBe(true);
    expect(view.force).toBe(true);
    expect(view.confirmLabel).toBe('Убрать всё равно');
    expect(view.body).toContain('В рабочей копии есть незакоммиченные правки или коммиты вне веток');

    const target = document.createElement('div');
    document.body.appendChild(target);
    const onforce = vi.fn();
    mount(WorktreeNotice, { target, props: { onforce } });
    flushSync();
    expect(target.textContent).toContain('В рабочей копии есть незакоммиченные правки или коммиты вне веток');
    target.querySelector('button')!.click();
    expect(onforce).toHaveBeenCalled();
  });

  it('a_clean_copy_asks_before_removing_and_does_not_force', () => {
    const view = acceptView({ spawn_depth: 1, worktree: { branch: 'b', state: 'active', path: '/w' } });
    expect(view.available).toBe(true);
    expect(view.dirty).toBe(false);
    expect(view.force).toBe(false);
    expect(view.confirmLabel).toBe('Принять результат и убрать');
    expect(view.body).toBe('Сессия остановится, рабочая копия будет удалена. Ветка с коммитами останется.');
  });

  it('only_a_delegated_unaccepted_session_offers_the_action', () => {
    expect(acceptView({ worktree: null }).available).toBe(false);
    expect(acceptView(null).available).toBe(false);
    expect(acceptView({ spawn_depth: 0 }).available).toBe(false);
    expect(acceptView({ spawn_depth: 1, archived: true }).available).toBe(false);
    expect(acceptView({ spawn_depth: 1, worktree: { state: 'removed' } }).available).toBe(false);
    expect(acceptView({ spawn_depth: 1, worktree: { state: 'removing' } }).available).toBe(false);
    expect(acceptView({ spawn_depth: 1, worktree: null }).available).toBe(true);
  });

  it('a_child_outlives_its_parent_and_can_still_be_accepted', () => {
    const view = acceptView({ parent_id: null, spawn_depth: 2, worktree: { state: 'active' } });
    expect(view.available).toBe(true);
  });
});
