/**
 * Одно имя инструмента на все харнессы. Claude Code зовёт MCP `mcp__сервер__инструмент`,
 * OpenCode — `сервер_инструмент`, Codex — `сервер.инструмент`; встроенные инструменты
 * у каждого свои. Карточки ленты знают только канонический id.
 */

/** Инструменты MCP-сервера canterlot: имя в схеме воркера → каноническое имя. */
const CANTERLOT_TOOLS: Record<string, string> = {
  canterlot_limits: 'limits',
  share_file: 'share_file',
  approve: 'approve',
  canterlot_presets_list: 'presets_list',
  canterlot_preset_save: 'preset_save',
  canterlot_preset_apply: 'preset_apply',
  sessions_list: 'sessions_list',
  session_inspect: 'session_inspect',
  session_send: 'session_send',
  session_spawn: 'session_spawn',
  session_subscribe: 'session_subscribe',
  session_unsubscribe: 'session_unsubscribe',
  session_wait: 'session_wait',
  session_accept: 'session_accept',
  session_interrupt: 'session_interrupt',
  session_stop: 'session_stop',
  alarm_set: 'alarm_set',
  alarm_cancel: 'alarm_cancel',
  alarm_list: 'alarm_list',
  nudge_set: 'nudge_set',
  nudge_clear: 'nudge_clear',
  route_preview: 'route_preview',
  spawn_cancel: 'spawn_cancel',
  plan_list: 'plan_list',
  plan_add: 'plan_add',
  plan_update: 'plan_update',
  plan_adopt: 'plan_adopt',
  history_search: 'history_search'
};

/** Все имена инструментов canterlot, как их объявляет воркер. */
export const CANTERLOT_MCP_NAMES = Object.keys(CANTERLOT_TOOLS);

/** Встроенные инструменты харнессов. Ключ — имя в нижнем регистре. */
const BUILTIN: Record<string, string> = {
  todowrite: 'todo.write',
  update_plan: 'todo.write',
  todoread: 'todo.read',
  agent: 'agent.task',
  task: 'agent.task',
  spawn_agent: 'agent.task',
  askuserquestion: 'question.ask',
  question: 'question.ask',
  websearch: 'web.search',
  web_search: 'web.search',
  webfetch: 'web.fetch',
  web_fetch: 'web.fetch',
  skill: 'skill',
  toolsearch: 'meta.tool_search',
  monitor: 'meta.monitor',
  taskstop: 'meta.task_stop',
  killshell: 'meta.task_stop',
  taskoutput: 'meta.task_output',
  bashoutput: 'meta.task_output',
  listagents: 'meta.list_agents',
  sendmessage: 'meta.send_message',
  pushnotification: 'meta.push_notification',
  bash: 'shell.exec',
  shell: 'shell.exec',
  exec_command: 'shell.exec',
  read: 'file.read',
  view_image: 'file.read',
  edit: 'file.edit',
  multiedit: 'file.edit',
  write: 'file.write',
  apply_patch: 'file.patch',
  notebookedit: 'file.edit',
  grep: 'file.search',
  glob: 'file.search',
  list: 'file.search'
};

function canterlot(tool: string): string | null {
  const name = CANTERLOT_TOOLS[tool];
  return name ? `canterlot.${name}` : null;
}

/** Сервер и инструмент из имени MCP любого харнесса; `null`, если это не MCP. */
function mcpParts(raw: string): { server: string; tool: string } | null {
  const claude = /^mcp__(.+?)__(.+)$/.exec(raw);
  if (claude) {
    const server = claude[1] ?? '';
    // Плагин Claude Code называет сервер `plugin_<плагин>_<сервер>`.
    const plugin = /^plugin_(.+)_([^_]+)$/.exec(server);
    return { server: plugin?.[2] ?? server, tool: claude[2] ?? '' };
  }
  const codex = /^([a-z0-9-]+)\.(.+)$/i.exec(raw);
  if (codex) return { server: codex[1] ?? '', tool: codex[2] ?? '' };
  return null;
}

/** Известные серверы, чьё имя OpenCode склеивает с инструментом через `_`. */
const OPENCODE_SERVERS = ['canterlot', 'taiga', 'context7'];

function fromServer(server: string, tool: string): string | null {
  const name = server.toLowerCase();
  if (name === 'canterlot') return canterlot(tool) ?? `canterlot.${tool}`;
  if (name === 'taiga') return `taiga.${tool.replace(/^taiga_/, '')}`;
  if (name === 'context7') return `context7.${tool}`;
  return null;
}

/** Канонический id инструмента: `canterlot.session_spawn`, `todo.write`, `taiga.userstory_get` … */
export function toolId(toolName: string | null | undefined): string {
  const raw = (toolName ?? '').trim();
  if (!raw) return 'other.unknown';
  const bare = canterlot(raw);
  if (bare) return bare;
  const mcp = mcpParts(raw);
  if (mcp) {
    const known = fromServer(mcp.server, mcp.tool);
    if (known) return known;
    if (raw.startsWith('mcp__')) return `mcp.${mcp.server}.${mcp.tool}`;
  }
  for (const server of OPENCODE_SERVERS) {
    if (!raw.startsWith(`${server}_`)) continue;
    const known = fromServer(server, raw.slice(server.length + 1));
    if (known) return known;
  }
  const builtin = BUILTIN[raw.toLowerCase()];
  if (builtin) return builtin;
  return `other.${raw}`;
}

/** Семейство id: `taiga.userstory_get` → `taiga`. */
export function toolFamily(id: string): string {
  const dot = id.indexOf('.');
  return dot < 0 ? id : id.slice(0, dot);
}
