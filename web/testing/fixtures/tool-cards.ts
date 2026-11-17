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

export type ToolCardsScene = 'shell';

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

  api.histories.set(id, items);
  api.bindConversation(id);
  api.hub.reach(`conversation:${id}`, String(feed.seq));
  return { id, run, blocks: feed.blocks };
}
