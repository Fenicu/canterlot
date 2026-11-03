import { describe, expect, it } from 'vitest';
import { spawnApprovalView } from '#lib/requests/spawnApprovalView.js';

describe('spawnApprovalView', () => {
  it('предупреждает, когда режим у CLI потомка сильнее, чем у родителя', () => {
    const view = spawnApprovalView({ harness: 'codex', permission_mode: 'ask', wider_than_parent: true, brief: 'b' });
    expect(view.facts.find((fact) => fact.label === 'Права')?.value).toContain('шире, чем у родителя');
  });

  it('без пометки строки о правах нет', () => {
    const view = spawnApprovalView({ harness: 'codex', permission_mode: 'ask', brief: 'b' });
    expect(view.facts.some((fact) => fact.label === 'Права')).toBe(false);
  });
});
