import { describe, expect, it } from 'vitest';
import { cardDef, cardIds, resolveCard } from '../../../../src/lib/timeline/cards/registry';
import { icons } from '../../../../src/lib/ui/icons';
import { hub, made } from './helpers';

const NOW = Date.parse('2026-11-07T10:05:00Z');

/** Семейства собственных инструментов canterlot: один значок на семейство, а не на глагол. */
const FAMILIES: Record<string, string[]> = {
  brain: ['memory_search', 'memory_get', 'memory_write', 'memory_update', 'memory_delete'],
  branch: ['session_spawn', 'session_accept', 'session_send', 'session_inspect', 'session_wait', 'session_stop', 'session_interrupt', 'session_subscribe', 'session_unsubscribe', 'spawn_cancel'],
  chats: ['sessions_list'],
  alarm: ['alarm_set', 'alarm_cancel', 'alarm_list', 'nudge_set', 'nudge_clear'],
  list: ['plan_add', 'plan_update', 'plan_list', 'plan_adopt'],
  gauge: ['limits'],
  share: ['share_file']
};

describe('card definition icon', () => {
  for (const [icon, tools] of Object.entries(FAMILIES)) {
    for (const tool of tools) {
      it(`${tool}_carries_${icon}`, () => {
        expect(cardDef(`canterlot.${tool}`)?.icon).toBe(icon);
      });
    }
  }

  it('every_icon_name_exists_in_the_registry', () => {
    for (const id of cardIds()) {
      const icon = cardDef(id)?.icon;
      if (icon) expect(Object.keys(icons), id).toContain(icon);
    }
  });

  it('harness_tools_have_no_icon', () => {
    const harness = [
      'shell.exec', 'file.read', 'file.search', 'file.edit', 'file.write', 'file.patch',
      'web.search', 'web.fetch', 'skill', 'todo.write', 'todo.read', 'agent.task', 'question.ask',
      'meta.tool_search', 'meta.monitor', 'taiga.whoami'
    ];
    for (const id of harness) {
      const def = cardDef(id);
      expect(def, id).not.toBeNull();
      expect(def?.icon, id).toBeNull();
    }
  });

  it('server_families_have_no_icon', () => {
    for (const id of ['taiga.stats', 'context7.query-docs', 'forgejo.pr_list', 'meeting.list']) {
      expect(cardDef(id)?.icon ?? null, id).toBeNull();
    }
  });

  it('settings_tools_that_are_not_in_the_icon_table_stay_without_one', () => {
    for (const id of ['canterlot.route_preview', 'canterlot.presets_list', 'canterlot.preset_save', 'canterlot.preset_apply']) {
      expect(cardDef(id)?.icon, id).toBeNull();
    }
  });

  it('the_resolved_card_carries_the_icon_of_its_definition', () => {
    const memory = resolveCard(made('mcp__canterlot__memory_search', { query: 'вкус чая' }, hub({ count: 0, results: [] })), NOW);
    expect(memory?.icon).toBe('brain');
    const shell = resolveCard(made('Bash', { command: 'ls' }, 'a.txt'), NOW);
    expect(shell).not.toBeNull();
    expect(shell?.icon).toBeNull();
  });
});
