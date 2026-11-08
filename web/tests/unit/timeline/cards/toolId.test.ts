import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { CANTERLOT_MCP_NAMES, toolFamily, toolId } from '../../../../src/lib/timeline/cards/toolId';

/** Имена инструментов canterlot из воркера: реестр не должен отстать от схемы. */
function workerTools(): string[] {
  const source = readFileSync(resolve(process.cwd(), '../crates/canterlot-worker/src/adapter/canterlot_mcp/mcp.rs'), 'utf8');
  const agent = [...source.matchAll(/^\s*\("([a-z_]+)", "[a-z_.]+"\),$/gm)].map((match) => match[1]!);
  const own = [...source.matchAll(/"name": "([a-z_]+)"/g)].map((match) => match[1]!);
  return [...new Set([...agent, ...own])];
}

describe('toolId', () => {
  it('every_canterlot_tool_of_the_worker_has_a_canonical_id_in_all_three_spellings', () => {
    const tools = workerTools();
    expect(tools).toEqual(expect.arrayContaining(['session_spawn', 'share_file', 'canterlot_limits', 'approve', 'alarm_set', 'spawn_cancel', 'canterlot_preset_apply', 'plan_list', 'plan_add', 'plan_update', 'plan_adopt']));
    expect(tools.length).toBeGreaterThanOrEqual(27);
    for (const tool of tools) {
      expect(CANTERLOT_MCP_NAMES).toContain(tool);
      const id = toolId(`mcp__canterlot__${tool}`);
      expect(id.startsWith('canterlot.')).toBe(true);
      // OpenCode: `сервер_инструмент`; Codex: `сервер.инструмент`; без сервера — так зовёт свои инструменты Antigravity.
      expect(toolId(`canterlot_${tool}`)).toBe(id);
      expect(toolId(`canterlot.${tool}`)).toBe(id);
      expect(toolId(tool)).toBe(id);
    }
  });

  it.each([
    ['mcp__canterlot__session_spawn', 'canterlot.session_spawn'],
    ['canterlot_session_spawn', 'canterlot.session_spawn'],
    ['canterlot.session_spawn', 'canterlot.session_spawn'],
    ['share_file', 'canterlot.share_file'],
    ['mcp__canterlot__share_file', 'canterlot.share_file'],
    ['canterlot_share_file', 'canterlot.share_file'],
    ['mcp__canterlot__canterlot_limits', 'canterlot.limits'],
    ['canterlot_canterlot_limits', 'canterlot.limits'],
    ['canterlot.canterlot_limits', 'canterlot.limits'],
    ['canterlot_canterlot_presets_list', 'canterlot.presets_list'],
    ['mcp__canterlot__session_accept', 'canterlot.session_accept'],
    ['TodoWrite', 'todo.write'],
    ['todowrite', 'todo.write'],
    ['update_plan', 'todo.write'],
    ['todoread', 'todo.read'],
    ['Agent', 'agent.task'],
    ['Task', 'agent.task'],
    ['task', 'agent.task'],
    ['spawn_agent', 'agent.task'],
    ['AskUserQuestion', 'question.ask'],
    ['question', 'question.ask'],
    ['WebSearch', 'web.search'],
    ['websearch', 'web.search'],
    ['web_search', 'web.search'],
    ['WebFetch', 'web.fetch'],
    ['webfetch', 'web.fetch'],
    ['Skill', 'skill'],
    ['skill', 'skill'],
    ['ToolSearch', 'meta.tool_search'],
    ['Monitor', 'meta.monitor'],
    ['TaskStop', 'meta.task_stop'],
    ['ListAgents', 'meta.list_agents'],
    ['SendMessage', 'meta.send_message'],
    ['PushNotification', 'meta.push_notification'],
    ['mcp__plugin_taiga_taiga__taiga_userstory_update', 'taiga.userstory_update'],
    ['mcp__plugin_taiga_taiga__taiga_comment_list', 'taiga.comment_list'],
    ['mcp__taiga__taiga_task_get', 'taiga.task_get'],
    ['taiga_taiga_issue_get', 'taiga.issue_get'],
    ['taiga.taiga_search', 'taiga.search'],
    ['mcp__plugin_context7_context7__resolve-library-id', 'context7.resolve-library-id'],
    ['mcp__plugin_context7_context7__query-docs', 'context7.query-docs'],
    ['context7_get-library-docs', 'context7.get-library-docs'],
    ['context7.resolve-library-id', 'context7.resolve-library-id'],
    ['Bash', 'shell.exec'],
    ['bash', 'shell.exec'],
    ['Read', 'file.read'],
    ['Edit', 'file.edit'],
    ['Write', 'file.write'],
    ['apply_patch', 'file.patch'],
    ['mcp__github__create_issue', 'mcp.github.create_issue'],
    ['SomethingNew', 'other.SomethingNew'],
    ['', 'other.unknown']
  ])('%s → %s', (raw, id) => {
    expect(toolId(raw)).toBe(id);
  });

  it('family_is_the_part_before_the_first_dot', () => {
    expect(toolFamily('taiga.userstory_get')).toBe('taiga');
    expect(toolFamily('skill')).toBe('skill');
  });
});
