import { beforeEach, describe, expect, it } from 'vitest';
import { pane } from '#lib/shell/pane.svelte.js';

describe('shell/pane: вкладка панели делегирования', () => {
  beforeEach(() => {
    pane.close();
    pane.forChat('reset');
  });

  it('только openPlan открывает вкладку «План»', () => {
    pane.forChat('a');
    expect(pane.delegationTab).toBe('tree');
    pane.openPlan();
    expect(pane.delegationTab).toBe('plan');
  });

  it('смена чата возвращает вкладку к дереву делегатов', () => {
    pane.forChat('a');
    pane.openPlan();
    pane.forChat('a');
    expect(pane.delegationTab).toBe('plan');
    pane.forChat('b');
    expect(pane.delegationTab).toBe('tree');
  });

  it('кнопка «Делегирование» в шапке открывает дерево, а не прошлую вкладку', () => {
    pane.forChat('a');
    pane.openPlan();
    pane.view = null;
    pane.toggleDelegation();
    expect(pane.view).toBe('delegation');
    expect(pane.delegationTab).toBe('tree');
    pane.toggleDelegation();
    expect(pane.view).toBeNull();
    pane.openPlan();
    pane.showDelegationTree();
    expect(pane.delegationTab).toBe('tree');
  });
});
