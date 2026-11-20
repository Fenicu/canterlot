// Лента оркестратора с задачами на другой машине: удалённый запуск делегата и вызовы `job_*`.
import type { FakeHubApi } from '../fake-hub/api';
import { seedJob, type FakeJob } from '../fake-hub/jobs';
import { seedConversation, seedMachine } from '../fake-hub/seed';
import { Feed, hubText } from './feed-cards';

const MODEL = 'claude-opus-5-5';
const CLAUDE = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };
const MINUTE = 60_000;

export const LONG_TITLE = 'Полная пересборка и проверка всех пакетов рабочего пространства вместе с интеграционными тестами второй машины';
export const LONG_SHELL = `cd /h/u/proj/packages/very-long-package-name-that-keeps-going/and-going && ./scripts/build-everything-with-a-very-long-script-name.sh --target=release --features=${'feature-flag-'.repeat(12)}end`;

export interface JobCardsSeed {
  id: string;
  /** Делегат на второй машине. */
  child: string;
  machine: { id: string; name: string };
  /** Законченная задача: хвост вывода есть в ответе. */
  done: FakeJob;
  /** Идёт: ответ вызова назвал её `running`, дальше её ведут события хаба. */
  running: FakeJob;
  /** Длинные название и команда: карточка не вылезает за экран. */
  long: FakeJob;
  blocks: Record<string, string>;
}

export const DONE_LOG = 'compiling 12 crates\nfinished release in 4.2s\nbuilt 12 files\n';

export function seedJobCards(api: FakeHubApi, opts: { now?: number } = {}): JobCardsSeed {
  const now = opts.now ?? Date.now();
  const machine = seedMachine(api, { name: 'machine-b', accepts_remote_work: true });
  const id = seedConversation(api, { title: 'Кантерлот', model: MODEL, harness: CLAUDE });
  const child = seedConversation(api, { title: 'Сервис на второй машине', parent_id: id, spawn_depth: 1, model: 'opus[1m]', harness: CLAUDE, machine: { id: machine.id, name: machine.name }, activity: { state: 'working', evidence: { phase: 'running_tool' }, since: new Date(now).toISOString() } as never });
  const at = (ms: number) => new Date(now + ms).toISOString();
  const conversation = { conversation_id: id, conversation_title: 'Кантерлот' };
  const done = seedJob(api, machine, { ...conversation, state: 'succeeded', title: 'Сборка веба', exit_code: 0, duration_ms: 4200, created_at: at(-9 * MINUTE), started_at: at(-9 * MINUTE + 1000), finished_at: at(-9 * MINUTE + 5200) }, DONE_LOG);
  const running = seedJob(api, machine, { ...conversation, state: 'running', title: 'Прогон тестов', shell: 'npm test', created_at: at(-2 * MINUTE), started_at: at(-2 * MINUTE + 1000) }, 'running 40 tests\nok 1 - parser\n');
  const long = seedJob(api, machine, { ...conversation, state: 'succeeded', title: LONG_TITLE, shell: LONG_SHELL, exit_code: 0, duration_ms: 61_000, created_at: at(-20 * MINUTE), started_at: at(-20 * MINUTE + 1000), finished_at: at(-19 * MINUTE) }, 'ok\n');

  const feed = new Feed(api, now - 30 * MINUTE);
  feed.input('Собери веб на второй машине и прогони тесты там');
  const run = api.id();
  feed.text(run, 'Запускаю делегата на второй машине и команды сборки.');
  feed.call(run, 'spawn', 'mcp__canterlot__session_spawn', { title: 'Сервис на второй машине', brief: 'Подними сервис и проверь его.', harness: 'claude_code', model: 'opus[1m]', effort: 'high', machine: machine.name, workdir: '/h/u/proj' }, hubText({ machine_name: machine.name, permission_mode: 'bypass', session_id: child, status: 'started', subscribed: true }, 'Session created.'));
  const runCommand = { machine: machine.name, workdir: '/h/u/proj', wait_seconds: 20 };
  feed.call(run, 'jobDone', 'mcp__canterlot__job_run', { ...runCommand, shell: 'npm run build', title: 'Сборка веба' }, hubText({ job_id: done.id, state: 'succeeded', machine: machine.name, exit_code: 0, duration_ms: 4200, output_tail: DONE_LOG, output_bytes: DONE_LOG.length, truncated: false }));
  feed.call(run, 'jobRunning', 'mcp__canterlot__job_run', { ...runCommand, shell: 'npm test', title: 'Прогон тестов' }, hubText({ job_id: running.id, state: 'running', machine: machine.name }, 'The result arrives as a [canterlot] message.'));
  feed.call(run, 'jobLong', 'mcp__canterlot__job_run', { ...runCommand, shell: LONG_SHELL, title: LONG_TITLE }, hubText({ job_id: long.id, state: 'succeeded', machine: machine.name, exit_code: 0, duration_ms: 61_000, output_tail: 'ok\n', output_bytes: 3, truncated: false }));
  feed.call(run, 'jobRefused', 'mcp__canterlot__job_run', { ...runCommand, shell: 'make deploy' }, 'remote_work_disabled: The machine does not accept work from other machines', {}, { summary: 'error' });
  feed.text(run, 'Сборка готова, тесты идут.');

  api.histories.set(id, feed.items);
  api.bindConversation(id);
  api.hub.reach(`conversation:${id}`, String(feed.seq));
  return { id, child, machine: { id: machine.id, name: machine.name }, done, running, long, blocks: feed.blocks };
}
