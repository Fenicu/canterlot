// Карточки инструментов сессий canterlot: запуск, приёмка, сообщение, просмотр и управление.
import { fmt } from '#lib/format/ru.js';
import { effortName, pairName } from '#lib/routing/decision.js';
import { itemReason } from '#lib/routing/queue.js';
import type { HubEvent } from '#lib/timeline/hubEvent.js';
import { bool, isObject, list, num, obj, str, type JsonObject } from '../json';
import { hubError, quoted, say, sessionWord, stateCountWord, type SessionWord, type Tone } from '../copy';
import type { CardCall, CardLogic, SessionFacts } from '../types';

/** Заголовок сессии: из списка владельца, иначе из ответа, иначе начало id. */
export function sessionTitle(call: CardCall, id: string, fallback = ''): string {
  return call.session(id)?.title || fallback || (id ? `сессия ${id.slice(0, 8)}` : '');
}

function at(value: string): number | null {
  const ts = Date.parse(value);
  return Number.isFinite(ts) ? ts : null;
}

/* ------------------------------------------------------------------ запуск */

export type SpawnStatus = 'running' | 'started' | 'pending_approval' | 'queued' | 'error' | 'cancelled';

export interface SpawnData {
  status: SpawnStatus;
  title: string;
  brief: string;
  /** Бриф пришёл не целиком: история отдала только начало вызова. */
  briefCut: boolean;
  sessionId: string;
  /** Исполнитель словами: «DeepSeek · высокое» или «Claude · Opus 1M». */
  executor: string;
  /** Тип задачи маршрута, если запуск шёл по маршруту. */
  route: string;
  routing: JsonObject | null;
  isolated: boolean;
  branch: string;
  queue: { reason: string; detail: string; notBefore: number | null; position: number | null } | null;
  /** Запрос владельцу на запуск вне белого списка. */
  requestId: string;
  /** Решение владельца из события хаба: allowed, denied, failed, lapsed. */
  decision: string;
  error: string;
  metricsStale: boolean;
}

const HARNESS_IDS: Record<string, string> = { claude: 'claude_code', claude_code: 'claude_code', codex: 'codex', opencode: 'opencode', antigravity: 'antigravity' };

export function spawnExecutor(harness: string, model: string, effort: string): string {
  if (!harness && !model) return effort ? effortName(effort) : '';
  const pair = pairName(HARNESS_IDS[harness] ?? harness, model);
  return [pair, effort ? effortName(effort) : ''].filter(Boolean).join(' · ');
}

function approvalOf(call: CardCall, requestId: string): HubEvent | null {
  if (!requestId) return null;
  return call.events('').find((event) => event.kind === 'approval' && event.request_id === requestId) ?? null;
}

export const spawn: CardLogic<SpawnData> = {
  stays: true,
  label: 'Запуск',
  parse(call) {
    const input = call.input;
    const answer = call.answer ?? {};
    const status = str(answer.status);
    const routeInput = obj(input.route);
    const routing = isObject(answer.routing) ? answer.routing : null;
    const brief = str(input.brief);
    if (!call.hasInput && !call.answer && call.state !== 'error' && call.state !== 'running') return null;
    const requestId = str(answer.request_id);
    const approval = approvalOf(call, requestId);
    const sessionId = str(answer.session_id) || (approval?.decision === 'allowed' ? (approval.session_id ?? approval.child_id ?? '') : '');
    const known: SpawnStatus =
      call.state === 'error'
        ? 'error'
        : call.state === 'running' || !call.output
          ? 'running'
          : status === 'started' || status === 'already_started'
            ? 'started'
            : status === 'pending_approval'
              ? approval?.decision === 'allowed' && sessionId
                ? 'started'
                : 'pending_approval'
              : status === 'queued'
                ? 'queued'
                : status === 'cancelled' || status === 'expired' || status === 'failed'
                  ? status === 'cancelled'
                    ? 'cancelled'
                    : 'error'
                  : call.answer
                    ? 'started'
                    : 'error';
    const harness = str(routing?.harness) || str(input.harness);
    const model = str(routing?.model) || str(input.model);
    const effort = str(routing?.effort) || str(input.effort);
    const worktree = obj(answer.worktree);
    const reason = str(answer.reason);
    return {
      status: known,
      title: str(input.title),
      brief,
      briefCut: call.partial && brief.length > 0,
      sessionId,
      executor: spawnExecutor(harness, model, effort),
      route: str(routing?.task_type) || str(routeInput.task_type),
      routing,
      isolated: bool(input.isolated) === true || Object.keys(worktree).length > 0,
      branch: str(worktree.branch),
      queue:
        known === 'queued'
          ? { reason, detail: str(answer.detail), notBefore: at(str(answer.not_before)), position: num(answer.position) }
          : null,
      requestId,
      decision: approval?.decision ?? '',
      error: known === 'error' ? (call.output ? hubError(call.output) : '') || (isObject(answer.reason) ? str(answer.reason.reason) : '') : '',
      metricsStale: str(answer.machine_metrics) === 'stale'
    };
  },
  summary(call, data) {
    const title = data.title || (data.sessionId ? sessionTitle(call, data.sessionId) : '');
    const named = title ? quoted(title) : '';
    switch (data.status) {
      case 'running':
        return say(call.voice, `Запускает ${named || 'делегата'}`, `Запуск делегата${named ? ` ${named}` : ''}`);
      case 'started':
        return say(call.voice, `Запустила ${named || 'делегата'}`, `Запуск делегата${named ? ` ${named}` : ''}`);
      case 'pending_approval':
        return say(call.voice, `Попросила разрешения на запуск${named ? ` ${named}` : ''}`, `Запуск${named ? ` ${named}` : ' делегата'} ждёт разрешения`);
      case 'queued':
        return say(call.voice, `Поставила в очередь ${named || 'запуск делегата'}`, `Запуск${named ? ` ${named}` : ' делегата'} в очереди`);
      case 'cancelled':
        return `Запуск${named ? ` ${named}` : ' делегата'} отменён`;
      case 'error':
        return say(call.voice, `Не запустила ${named || 'делегата'}`, `Запуск${named ? ` ${named}` : ' делегата'} не удался`);
    }
  }
};

/** Слово и тон состояния запуска: у запущенного — живое состояние ребёнка из списка. */
export function spawnWord(call: CardCall, data: SpawnData, now: number): SessionWord {
  switch (data.status) {
    case 'running':
      return { word: 'запускается', tone: 'accent', live: true };
    case 'pending_approval':
      return data.decision === 'denied'
        ? { word: 'отклонён', tone: 'danger', live: false }
        : data.decision === 'lapsed'
          ? { word: 'запрос истёк', tone: 'muted', live: false }
          : { word: 'ждёт владельца', tone: 'warning', live: false };
    case 'queued': {
      const next = data.queue?.notBefore ?? null;
      return { word: next !== null && next > now ? `следующий в ${fmt.clock(next)}` : 'в очереди', tone: 'warning', live: false };
    }
    case 'cancelled':
      return { word: 'отменён', tone: 'muted', live: false };
    case 'error':
      return { word: 'не запущен', tone: 'danger', live: false };
    case 'started': {
      const row = data.sessionId ? call.session(data.sessionId) : null;
      return sessionWord(row?.state, { accepted: row?.accepted });
    }
  }
}

/** Строка очереди: «Запуск в очереди: частота запусков: …». */
export function queueLine(data: SpawnData, now: number): string {
  if (!data.queue) return '';
  return `Запуск в очереди: ${itemReason({ reason: data.queue.reason, detail: data.queue.detail }, now)}`;
}

/** Где работает делегат: отдельная копия и её ветка. */
export function placeLine(data: SpawnData): string {
  if (data.branch) return `отдельная копия, ветка ${data.branch}`;
  return data.isolated ? 'отдельная копия' : '';
}

/* ------------------------------------------------------------------ приёмка */

export interface AcceptData {
  sessionId: string;
  status: string;
  error: string;
  /** Итог приёмки из события хаба: убрана ли копия и что с веткой. */
  worktree: string;
  branch: string;
  branchKept: boolean | null;
  commits: number | null;
}

/** Последнее событие нужного вида о сессии. */
function lastEvent(call: CardCall, sessionId: string, kind: string): HubEvent | null {
  return call.events(sessionId).findLast((event) => event.kind === kind) ?? null;
}

export const accept: CardLogic<AcceptData> = {
  stays: true,
  label: 'Приёмка',
  parse(call) {
    const sessionId = str(call.input.session_id);
    if (!sessionId && !call.answer && call.state !== 'error') return null;
    const accepted = sessionId ? lastEvent(call, sessionId, 'accepted') : null;
    const result = sessionId ? lastEvent(call, sessionId, 'session_result') : null;
    return {
      sessionId,
      status: str(call.answer?.status),
      error: call.state === 'error' ? hubError(call.output) : '',
      worktree: accepted?.worktree ?? '',
      branch: accepted?.branch ?? '',
      branchKept: accepted?.branch_kept ?? null,
      commits: result?.commits ? result.commits.length : null
    };
  },
  summary(call, data) {
    const title = sessionTitle(call, data.sessionId);
    if (data.error) return say(call.voice, `Не приняла ${quoted(title)}`, `Приёмка ${quoted(title)} не удалась`);
    return say(call.voice, `Приняла ${quoted(title)}`, `Приёмка ${quoted(title)}`);
  }
};

/** Что известно об итоге приёмки: коммиты, копия, ветка. */
export function acceptFacts(data: AcceptData): string[] {
  const facts: string[] = [];
  if (data.commits !== null) facts.push(`${data.commits} ${fmt.plural(data.commits, ['коммит', 'коммита', 'коммитов'])}`);
  if (data.worktree === 'removed') facts.push('рабочая копия убрана');
  else if (data.worktree === 'dirty') facts.push('в копии остались правки');
  else if (data.worktree === 'failed') facts.push('копию убрать не удалось');
  if (data.branch && data.branchKept !== null) facts.push(`ветка ${data.branch} ${data.branchKept ? 'оставлена' : 'удалена'}`);
  return facts;
}

export function acceptWord(call: CardCall, data: AcceptData): SessionWord {
  if (data.error) return { word: 'ошибка', tone: 'danger', live: false };
  if (call.state === 'running') return { word: 'принимается', tone: 'accent', live: true };
  const row = call.session(data.sessionId);
  if (data.worktree || row?.accepted || row?.state === 'stopped' || data.status === 'done') return { word: 'остановлена', tone: 'muted', live: false };
  if (data.status === 'removing') return { word: 'убирается копия', tone: 'accent', live: true };
  return { word: 'останавливается', tone: 'accent', live: true };
}

/* ------------------------------------------------------------------ сообщение */

export interface SendData {
  sessionId: string;
  text: string;
  textCut: boolean;
  subscribed: boolean;
  paused: boolean;
  stopped: boolean;
  error: string;
}

export const send: CardLogic<SendData> = {
  stays: true,
  label: 'Сообщение',
  parse(call) {
    const sessionId = str(call.input.session_id);
    const text = str(call.input.text);
    if (!sessionId && !text) return null;
    const answer = call.answer ?? {};
    return {
      sessionId,
      text,
      textCut: call.partial && text.length > 0,
      subscribed: bool(answer.subscribed) ?? bool(call.input.subscribe) ?? true,
      paused: bool(answer.target_paused) === true,
      stopped: bool(answer.target_stopped) === true,
      error: call.state === 'error' ? hubError(call.output) : ''
    };
  },
  summary(call, data) {
    const title = quoted(sessionTitle(call, data.sessionId));
    return say(call.voice, `Написала ${title}`, `Сообщение ${title}`);
  }
};

/* ------------------------------------------------------------------ просмотр */

export interface InspectData {
  sessionId: string;
  title: string;
  state: string;
  /** Читалась страница одного ответа, а не обзор. */
  page: boolean;
}

export const inspect: CardLogic<InspectData> = {
  label: 'Просмотр',
  parse(call) {
    const answer = call.answer ?? {};
    const session = obj(answer.session);
    const sessionId = str(call.input.session_id) || str(answer.session_id) || str(session.id);
    if (!sessionId) return null;
    return {
      sessionId,
      title: str(session.title),
      state: str(obj(answer.activity).state),
      page: Boolean(str(call.input.block_id)) || isObject(answer.block)
    };
  },
  summary(call, data) {
    const title = quoted(sessionTitle(call, data.sessionId, data.title));
    if (data.page) return say(call.voice, `Прочитала ответ ${title}`, `Ответ ${title}`);
    const word = data.state ? ` · ${sessionWord(data.state).word}` : '';
    return say(call.voice, `Посмотрела ${title}${word}`, `Просмотр ${title}${word}`);
  }
};

export interface ListedSession {
  id: string;
  title: string;
  state: string;
}

export interface ListData {
  sessions: ListedSession[];
  counts: { state: string; count: number }[];
  queued: number;
}

const COUNT_ORDER = ['working', 'waiting_for_user', 'stalled', 'quota_paused', 'failed', 'idle', 'stopped'];

export const sessionsList: CardLogic<ListData> = {
  label: 'Сессии',
  parse(call) {
    if (!call.answer || !Array.isArray(call.answer.sessions)) return call.state === 'running' ? { sessions: [], counts: [], queued: 0 } : null;
    const sessions = list(call.answer.sessions)
      .filter(isObject)
      .filter((row) => bool(row.is_self) !== true)
      .map((row) => ({ id: str(row.id), title: str(row.title), state: str(obj(row.activity).state) }));
    const tally = new Map<string, number>();
    for (const row of sessions) tally.set(row.state, (tally.get(row.state) ?? 0) + 1);
    const counts = [...tally]
      .map(([state, count]) => ({ state, count }))
      .sort((a, b) => rank(a.state) - rank(b.state));
    return { sessions, counts, queued: num(obj(call.answer.queued_spawns).count) ?? 0 };
  },
  summary(_call, data) {
    if (data.sessions.length === 0) return 'Сессии: других нет';
    const parts = data.counts.map((item) => stateCountWord(item.state, item.count));
    if (data.queued > 0) parts.push(`${data.queued} в очереди запуска`);
    return `Сессии: ${parts.join(', ')}`;
  }
};

function rank(state: string): number {
  const index = COUNT_ORDER.indexOf(state);
  return index < 0 ? COUNT_ORDER.length : index;
}

export interface WaitData {
  sessionId: string;
  state: string;
  timedOut: boolean;
}

export const wait: CardLogic<WaitData> = {
  label: 'Ожидание',
  parse(call) {
    const sessionId = str(call.input.session_id);
    if (!sessionId) return null;
    const answer = call.answer ?? {};
    return { sessionId, state: str(answer.state), timedOut: bool(answer.timed_out) === true };
  },
  summary(call, data) {
    const title = quoted(sessionTitle(call, data.sessionId));
    if (call.state === 'running') return say(call.voice, `Ждёт ${title}`, `Ожидание ${title}`);
    if (data.timedOut) return say(call.voice, `Не дождалась ${title}`, `Ожидание ${title}: срок вышел`);
    const word = data.state ? ` · ${sessionWord(data.state).word}` : '';
    return say(call.voice, `Дождалась ${title}${word}`, `Ожидание ${title}${word}`);
  }
};

/* ------------------------------------------------------------------ управление */

/** Однострочные карточки: факт из входа и ответа хаба. */
export interface LineData {
  text: string;
  sessionId: string;
  tone: Tone;
  word: string;
}

function line(text: string, sessionId = '', word = '', tone: Tone = 'muted'): LineData {
  return { text, sessionId, word, tone };
}

function control(build: (call: CardCall, sessionId: string, title: string) => LineData | null, label: string): CardLogic<LineData> {
  return {
    label,
    parse(call) {
      const sessionId = str(call.input.session_id);
      const title = sessionId ? quoted(sessionTitle(call, sessionId)) : '';
      if (call.state === 'error') return line(`${label}: ${hubError(call.output) || 'ошибка'}`, sessionId, 'ошибка', 'danger');
      return build(call, sessionId, title);
    },
    summary: (_call, data) => data.text
  };
}

export const stop = control((call, id, title) => {
  if (!id) return null;
  const status = str(call.answer?.status);
  const word = status === 'nothing_running' ? 'не работала' : status === 'already_stopping' ? 'уже останавливается' : status ? 'копия сохранена' : '';
  return line(say(call.voice, `Остановила ${title}`, `Остановка ${title}`), id, word);
}, 'Остановка');

export const interrupt = control((call, id, title) => {
  if (!id) return null;
  const status = str(call.answer?.status);
  return line(say(call.voice, `Прервала ход ${title}`, `Прерывание хода ${title}`), id, status === 'nothing_running' ? 'ход не шёл' : '');
}, 'Прерывание');

export const subscribe = control((call, id, title) => (id ? line(say(call.voice, `Подписалась на ${title}`, `Подписка на ${title}`), id) : null), 'Подписка');
export const unsubscribe = control((call, id, title) => (id ? line(say(call.voice, `Отписалась от ${title}`, `Отписка от ${title}`), id) : null), 'Отписка');

export const nudgeSet = control((call, id, title) => {
  if (!id) return null;
  if (bool(call.input.enabled) === false) return line(`Подталкивание ${title} выключено`, id);
  const rule = obj(call.answer?.rule);
  const times = num(call.input.max_nudges) ?? num(rule.max_consecutive);
  const idle = num(call.input.idle_minutes) ?? num(rule.idle_minutes);
  const facts = [times !== null ? `до ${times} ${fmt.plural(times, ['раза', 'раз', 'раз'])} подряд` : '', idle !== null ? `после ${idle} мин тишины` : ''].filter(Boolean);
  return line(`Подталкивание ${title}${facts.length ? ` · ${facts.join(', ')}` : ''}`, id);
}, 'Подталкивание');

export const nudgeClear = control((call, id, title) => {
  if (!id) return null;
  const none = str(call.answer?.state) === 'none';
  return line(none ? `Подталкивания у ${title} не было` : `Подталкивание ${title} снято`, id);
}, 'Подталкивание');

export const spawnCancel = control((call) => {
  const status = str(call.answer?.status);
  const word = status === 'already_started' ? 'уже запущен' : status === 'already_cancelled' ? 'уже отменён' : status === 'expired' ? 'истёк' : '';
  return line(say(call.voice, 'Отменила запуск из очереди', 'Отмена запуска из очереди'), str(call.answer?.session_id), word);
}, 'Отмена запуска');

export const routePreview = control((call) => {
  const task = str(call.input.task_type);
  const decision = obj(call.answer?.decision);
  if (!task) {
    const types = list(call.answer?.task_types).length;
    return line(types ? `Маршруты: ${types} ${fmt.plural(types, ['тип задачи', 'типа задач', 'типов задач'])}` : 'Маршруты');
  }
  const executor = spawnExecutor(str(decision.harness), str(decision.model), str(decision.effort));
  return line(`Маршрут «${task}»${executor ? `: ${executor}` : ''}`);
}, 'Маршрут');

/* ------------------------------------------------------------------ пресеты */

export const presetsList = control((call) => {
  const presets = list(call.answer?.presets).length;
  const applied = str(obj(call.answer?.applied).title);
  const id = str(call.input.id);
  if (id) return line(`Пресет «${str(obj(call.answer?.preset).title) || id}»`);
  return line(`Пресеты инструкций${presets ? `: ${presets}` : ''}${applied ? ` · сейчас «${applied}»` : ''}`);
}, 'Пресеты');

export const presetSave = control((call) => {
  const title = str(call.input.title);
  const revision = num(call.answer?.revision);
  return line(say(call.voice, `Сохранила пресет «${title}»`, `Пресет «${title}» сохранён`), '', revision !== null ? `версия ${revision}` : '');
}, 'Пресет');

export const presetApply = control((call) => {
  const id = str(call.input.id);
  const name = PRESET_NAMES[id] ?? id;
  return line(say(call.voice, `Применила пресет «${name}»`, `Пресет «${name}»`), '', 'со следующего хода');
}, 'Пресет');

const PRESET_NAMES: Record<string, string> = { clean: 'Без инструкций', owner: 'Как у владельца' };

/** Ссылка на сессию, о которой строка; без заголовка — пусто. */
export function knownSession(call: CardCall, id: string): SessionFacts | null {
  return id ? call.session(id) : null;
}

