// Лента с карточками команд, навыков и правок файлов во всех формах харнессов: e2e, visual-language и снимки.
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import type { FakeHubApi } from '../fake-hub/api';
import { seedConversation } from '../fake-hub/seed';
import { Feed } from './feed-cards';

const MINUTE = 60_000;

export interface ToolCardsSeed {
  id: string;
  run: string;
  /** id блоков по имени сцены. */
  blocks: Record<string, string>;
}

export const LONG_COMMAND = 'cd web && npm ci --no-audit --no-fund && npm run lint:tokens && npm run test:unit -- --reporter=dot 2>&1 | tail -20';
export const PIPELINE = `rg -n "cardDef\\(" src/lib/timeline | awk -F: '{print $1}' | sort | uniq -c | sort -rn | head -10`;

const TEST_OUTPUT = Array.from({ length: 34 }, (_, index) => (index < 30 ? ` PASS tests/unit/timeline/cards/case-${index + 1}.test.ts (${index + 3} tests) ${12 + index}ms` : ['', ' Test Files  30 passed (30)', '      Tests  465 passed (465)', '   Duration  4.21s'][index - 30]!)).join('\n');

export const REGISTRY_PATH = '/home/owner/canterlot/web/src/lib/timeline/cards/registry.ts';
const EDIT_OLD = [
  "  'web.fetch': card({ ...fetchPage, link: (_call, data) => ({ text: data.domain, href: data.url, external: true }) }),",
  '  skill: card(skill),',
  "  'meta.tool_search': card(quiet),"
].join('\n');
const EDIT_NEW = [
  "  'web.fetch': card({ ...fetchPage, link: (_call, data) => ({ text: data.domain, href: data.url, external: true }) }),",
  '  skill: card(skill, SkillCard),',
  "  'file.edit': card(outcomed(edit), EditCard),",
  "  'file.write': card(outcomed(write), EditCard),",
  "  'meta.tool_search': card(quiet),"
].join('\n');
const LIMIT_NAMES = ['контекста', 'кусков', 'файлов', 'строк'];
/** Новый модуль на 72 строки: раскрытая карточка режет его на 60 и предлагает «Показать всё». */
export const NEW_MODULE = [
  '// Разница файла для карточки правки.',
  "import { fmt } from '#lib/format/ru.js';",
  '',
  "export type DiffKind = 'add' | 'del' | 'ctx' | 'hunk';",
  '',
  ...Array.from({ length: 66 }, (_, index) => (index % 3 === 0 ? `export const LIMIT_${index} = ${index * 8};` : index % 3 === 1 ? `/** Предел ${index}: ${LIMIT_NAMES[index % 4]}. */` : '')),
  "export const DONE = 'diff';"
].join('\n');
const PY_OLD = 'def limit(rows):\n    return rows[:20]\n';
const PY_NEW = 'def limit(rows, size=40):\n    """Первые строки отчёта."""\n    return rows[:size]\n';
const CODEX_DIFF = ['@@ -12,6 +12,7 @@ pub fn command_summary(item: &Value) -> String {', '     let actions = item', '         .get("commandActions")', '-        .and_then(Value::as_array)', '+        .and_then(Value::as_array)', '+        .filter(|actions| !actions.is_empty())', '         .map(|actions| {', ''].join('\n');
const CODEX_ADD = '# Пузыри инструментов\n\nКарточки команды, навыка и правки файла.\n';
const ENVELOPE = [
  '*** Begin Patch',
  '*** Update File: web/src/lib/timeline/cards/copy.ts',
  '@@ export function clip(text: string, max = 80): string {',
  "-  const clean = text.replace(/\\s+/g, ' ');",
  "+  const clean = text.replace(/\\s+/g, ' ').trim();",
  '+  if (!clean) return clean;',
  '*** Add File: web/src/lib/timeline/cards/outcome.ts',
  '+// Итог вызова команды или правки файла.',
  '+export interface Outcome { exit: number | null }',
  '*** End Patch'
].join('\n');

/** Лента одной сессии Claude Code; вызовы OpenCode и Codex несут свои формы входа и сводки. */
export function seedToolCards(api: FakeHubApi, opts: { now?: number; working?: boolean } = {}): ToolCardsSeed {
  const now = opts.now ?? Date.now();
  const id = seedConversation(api, {
    title: 'Пузыри инструментов',
    model: 'claude-opus-5-5',
    harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' },
    ...(opts.working ? { activity: { state: 'working', evidence: { phase: 'running_tool' }, since: new Date(now).toISOString() } } : {})
  });
  const feed = new Feed(api, now - 40 * MINUTE);
  const run = api.id();
  const items = feed.items as (HistoryBlock | HistoryInput)[];

  feed.input('Прогони проверки веба и поправь карточки');
  feed.text(run, 'Запускаю проверки.');
  feed.call(run, 'shellLong', 'Bash', { command: LONG_COMMAND, description: 'Проверки веба: токены и юнит-тесты' }, TEST_OUTPUT);
  feed.text(run, 'Один тест упал, смотрю, где и кто вызывает реестр.');
  feed.call(run, 'shellFailed', 'Bash', { command: 'npm run test:unit -- tests/unit/timeline/cards/shell.test.ts' }, 'Exit code 1\n FAIL  tests/unit/timeline/cards/shell.test.ts > formatCommand\nAssertionError: expected "a" to be "b"', {}, { summary: 'error' });
  feed.call(run, 'shellPipeline', 'bash', PIPELINE, '   7 src/lib/timeline/cards/registry.ts\n   2 src/lib/timeline/cards/ToolCard.svelte', { summary: 'bash: rg -n "cardDef\\(" src/lib/timeline, exit 0' });
  feed.text(run, 'Чищу сборку и прогоняю тесты воркера.');
  feed.call(run, 'shellDenied', 'shell', 'rm -rf web/build web/.svelte-kit', '', { summary: 'rm -rf web/build web/.svelte-kit, declined' });
  feed.text(run, 'Без чистки: тесты воркера.');
  feed.call(run, 'shellCwd', 'Bash', { command: 'cargo test -p canterlot-worker --lib adapter::codex', cwd: '/home/owner/canterlot' }, 'test result: ok. 214 passed; 0 failed');

  feed.text(run, 'Команды готовы.');

  const skills = api.id();
  feed.input('Сначала план, потом код');
  feed.call(skills, 'skillDone', 'Skill', { skill: 'superpowers:test-driven-development', args: 'карточки ленты: формат команды и счёт строк' }, 'Launching skill: superpowers:test-driven-development');
  feed.text(skills, 'Навык загружен. Беру следующий.');
  feed.call(skills, 'skillOpencode', 'skill', { name: 'frontend-design' }, '<skill_content name="frontend-design">…</skill_content>');
  feed.text(skills, 'Навыки на месте.');

  const files = api.id();
  feed.input('Поправь реестр и добавь модуль разницы');
  feed.call(files, 'editClaude', 'Edit', { file_path: REGISTRY_PATH, old_string: EDIT_OLD, new_string: EDIT_NEW }, `The file ${REGISTRY_PATH} has been updated. Here's the result of running \`cat -n\` on a snippet of the edited file.`);
  feed.text(files, 'Новый модуль разницы.');
  feed.call(files, 'writeClaude', 'Write', { file_path: '/home/owner/canterlot/web/src/lib/timeline/cards/diff.ts', content: NEW_MODULE }, 'File created successfully at: /home/owner/canterlot/web/src/lib/timeline/cards/diff.ts');
  feed.text(files, 'OpenCode правит отчёт.');
  feed.call(files, 'editOpencode', 'edit', { filePath: '/srv/reports/limit.py', oldString: PY_OLD, newString: PY_NEW }, 'Edit applied successfully.');
  feed.text(files, 'Codex правит воркер и пишет заметку.');
  feed.call(files, 'patchCodex', 'apply_patch', CODEX_DIFF, null, { summary: 'crates/canterlot-worker/src/adapter/codex/events.rs, completed' });
  feed.call(files, 'patchCodexAdd', 'apply_patch', CODEX_ADD, null, { summary: 'docs/bubbles.md, completed' });
  feed.text(files, 'И конвертом на два файла.');
  feed.call(files, 'patchEnvelope', 'apply_patch', { input: ENVELOPE }, 'Success. Updated the following files:\nM web/src/lib/timeline/cards/copy.ts\nA web/src/lib/timeline/cards/outcome.ts');
  feed.text(files, 'Правки готовы.');

  const explore = api.id();
  feed.input('Разберись, как лента сворачивает шаги');
  feed.text(explore, 'Смотрю код ленты.');
  feed.call(explore, 'readSteps', 'Read', { file_path: '/home/owner/canterlot/web/src/lib/timeline/steps.ts', offset: 120, limit: 61 }, '120\texport function foldSteps(');
  feed.call(explore, 'readOpencode', 'read', { filePath: '/home/owner/canterlot/web/src/lib/timeline/folds.ts' }, '<file>\n00001| export class FeedFolds {\n</file>');
  feed.call(explore, 'grepClaude', 'Grep', { pattern: 'foldSteps\\(', path: 'web/src', glob: '*.svelte' }, 'Found 2 files\nweb/src/lib/timeline/AnswerCard.svelte\nweb/src/lib/timeline/Timeline.svelte');
  feed.call(explore, 'globOpencode', 'glob', { pattern: 'web/src/lib/timeline/cards/*.svelte' }, Array.from({ length: 24 }, (_, index) => `/home/owner/canterlot/web/src/lib/timeline/cards/Card${index}.svelte`).join('\n'));
  feed.call(explore, 'readAgain', 'Read', { file_path: '/home/owner/canterlot/web/src/lib/timeline/steps.ts', offset: 1, limit: 60 }, '1\timport { fmt }');
  feed.call(explore, 'readRows', 'Read', { file_path: '/home/owner/canterlot/web/src/lib/timeline/rows.ts' }, '1\texport interface BlockLinks');
  feed.text(explore, 'Шаги сворачивает foldSteps. Проверю ещё один файл.');
  feed.call(explore, 'readSingle', 'Read', { file_path: '/home/owner/canterlot/web/src/lib/timeline/AnswerCard.svelte', offset: 80, limit: 20 }, '80\t  const items = $derived(foldSteps(');
  feed.text(explore, 'И два файла, которых уже нет.');
  feed.call(explore, 'readOk', 'Read', { file_path: '/home/owner/canterlot/web/src/lib/timeline/QuietRow.svelte' }, '1\t<script lang="ts">');
  feed.call(explore, 'readGone', 'Read', { file_path: '/home/owner/canterlot/web/src/lib/timeline/Gone.svelte' }, '<tool_use_error>File does not exist.</tool_use_error>', {}, { summary: 'error' });
  feed.text(explore, 'Разобралась.');

  api.histories.set(id, items);
  api.bindConversation(id);
  api.hub.reach(`conversation:${id}`, String(feed.seq));
  return { id, run, blocks: feed.blocks };
}
