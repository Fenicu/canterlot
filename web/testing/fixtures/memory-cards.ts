// Лента оркестратора с вызовами памяти: поиск, чтение, запись, правка и архив; удачи и отказы.
import type { FakeHubApi } from '../fake-hub/api';
import { seedConversation } from '../fake-hub/seed';
import { Feed, hubText } from './feed-cards';

const MINUTE = 60_000;
const MODEL = 'claude-opus-5-5';
const CLAUDE = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };

export const LONG_SUBJECT = 'Очень длинная тема записи про миграции тестовой базы данных в изолированной рабочей копии без пробелов-разрывов-и-переносов-подряд-подряд-подряд-подряд-подряд';

export interface MemoryCardsSeed {
  id: string;
  /** Ход, в котором стоят вызовы: живые вызовы того же хода пишутся с ним. */
  run: string;
  /** Записи памяти, заведённые в фиктивном хабе: по теме. */
  entries: Record<'port' | 'build' | 'recipe', { id: string; version: number }>;
  blocks: Record<string, string>;
}

export function seedMemoryCards(api: FakeHubApi, opts: { now?: number } = {}): MemoryCardsSeed {
  const now = opts.now ?? Date.now();
  const id = seedConversation(api, { title: 'Кантерлот', model: MODEL, harness: CLAUDE });
  const port = api.seedMemory({ domain: 'work', subject: 'Порт тестовой базы', kind: 'gotcha', level: 'project', project: '/h/work/canterlot', project_label: 'canterlot', body: '**Порт** тестовой базы: `55883`.\n\nНе занимать порты чужих стендов.', version: 2 });
  const build = api.seedMemory({ domain: 'work', subject: 'Сборка воркера', kind: 'reference', body: 'cargo build -p canterlot-worker' });
  const recipe = api.seedMemory({ domain: 'personal', subject: 'Рецепт борща', kind: 'user', body: 'Свёкла, капуста, говядина' });
  const entries = { port: { id: port.id, version: port.version }, build: { id: build.id, version: build.version }, recipe: { id: recipe.id, version: recipe.version } };

  const feed = new Feed(api, now - 40 * MINUTE);
  const run = api.id();
  const fail = { summary: 'error' } as const;
  const search = (name: string, input: unknown, results: unknown[]) => feed.call(run, name, 'mcp__canterlot__memory_search', input, hubText({ count: results.length, results }));
  const row = (entry: { id: string }, subject: string, patch: Record<string, unknown> = {}) => ({ id: entry.id, kind: 'gotcha', level: 'project', project_label: 'canterlot', subject, excerpt: 'Порт тестовой базы', updated_at: '2026-11-18T09:00:00Z', expired: false, ...patch });

  feed.input('Проверь, что мы знаем про тестовую базу, и запомни выводы');
  feed.text(run, 'Смотрю память.');
  search('search', { query: 'тестовая база' }, [row(port, 'Порт тестовой базы'), row(build, 'Сборка воркера', { kind: 'reference', level: 'domain', project_label: null, expired: true }), row(recipe, LONG_SUBJECT, { kind: 'user', level: 'domain', project_label: null })]);
  search('searchEmpty', { query: 'квантовая гравитация' }, []);
  search('searchNew', {}, [row(port, 'Порт тестовой базы')]);
  feed.call(run, 'get', 'mcp__canterlot__memory_get', { id: port.id }, hubText({ id: port.id, domain: 'work', level: 'project', project_label: 'canterlot', kind: 'gotcha', subject: 'Порт тестовой базы', body: port.body, status: 'active', valid_until: '2026-12-01T00:00:00Z', expired: false, version: 2, created_by: { kind: 'agent' }, created_at: '2026-11-01T09:00:00Z', updated_by: { kind: 'owner' }, updated_at: '2026-11-18T09:00:00Z', import_source: null }));
  feed.call(run, 'write', 'mcp__canterlot__memory_write', { kind: 'feedback', subject: 'Не писать эмодзи в карточках', body: 'Владелец не любит эмодзи.\n\n- состояния словами\n- без галочек', level: 'domain' }, hubText({ id: build.id, domain: 'work', level: 'domain', project_label: null, version: 1 }));
  feed.call(run, 'writeLong', 'mcp__canterlot__memory_write', { kind: 'gotcha', subject: LONG_SUBJECT, body: 'Короткий текст.', level: 'project' }, hubText({ id: port.id, domain: 'work', level: 'project', project_label: 'canterlot', version: 1 }));
  feed.call(run, 'writeSecret', 'mcp__canterlot__memory_write', { kind: 'reference', subject: 'Доступ к API', body: 'token: abcdef', level: 'domain' }, 'looks_like_secret: The text looks like a secret {"class":"assignment"}', {}, fail);
  feed.call(run, 'writeExists', 'mcp__canterlot__memory_write', { kind: 'gotcha', subject: 'Порт тестовой базы', body: 'ещё раз', level: 'project' }, `subject_exists: An active entry with this subject already exists {"id":"${port.id}","version":2}`, {}, fail);
  feed.call(run, 'writeRate', 'mcp__canterlot__memory_write', { kind: 'gotcha', subject: 'Лишнее', body: 'ещё', level: 'domain' }, 'rate_limited: Too many requests, try later {"retry_after_s":900}', {}, fail);
  feed.call(run, 'update', 'mcp__canterlot__memory_update', { id: port.id, expected_version: 2, body: 'Порт тестовой базы теперь `55884`.', valid_until: null }, hubText({ id: port.id, domain: 'work', level: 'project', project_label: 'canterlot', version: 3 }));
  feed.call(run, 'updateConflict', 'mcp__canterlot__memory_update', { id: port.id, expected_version: 2, subject: 'Порт' }, 'version_conflict: The entry changed since you read it {"current_version":3}', {}, fail);
  feed.call(run, 'delete', 'mcp__canterlot__memory_delete', { id: port.id, expected_version: 3 }, hubText({ id: port.id, status: 'archived', version: 4 }));
  feed.call(run, 'deleteUnknown', 'mcp__canterlot__memory_delete', { id: '00000000-0000-4000-8000-0000000000ff', expected_version: 1 }, 'not_found: No such memory entry', {}, fail);
  feed.text(run, 'Готово.');

  api.histories.set(id, feed.items);
  api.bindConversation(id);
  api.hub.reach(`conversation:${id}`, String(feed.seq));
  return { id, run, entries, blocks: feed.blocks };
}
