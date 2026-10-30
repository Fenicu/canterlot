import type { Page } from '@playwright/test';
import { expect } from '@playwright/test';
import { randomUUID } from 'node:crypto';
import { getStack, type Stack } from './global-setup';

export interface InputItem {
  kind: 'input';
  id: string;
  state: string;
  content: { type: string; text?: string; name?: string; sha256?: string }[];
  created_at: string;
  created_seq: string;
}
export interface BlockItem {
  kind: 'block';
  id: string;
  block_kind: string;
  agent: string;
  model: string;
  state: string;
  content: string;
  summary?: string | null;
  tool_name?: string | null;
  parent_block_id?: string | null;
  input_command_id?: string | null;
  created_at: string;
  started_at: string;
  finished_at?: string | null;
  rev: number;
}
export type HistoryItem = InputItem | BlockItem;

export interface HubClient {
  hubUrl: string;
  fetch(path: string, init?: RequestInit): Promise<Response>;
  json<T = Record<string, unknown>>(path: string, init?: RequestInit): Promise<T>;
  history(conversationId: string, limit?: number): Promise<HistoryItem[]>;
  inputs(conversationId: string): Promise<InputItem[]>;
  textOf(conversationId: string): Promise<string>;
  /** Все вводы по журналу событий: текст и последнее состояние, включая очередь и отменённые. */
  inputLog(conversationId: string): Promise<{ id: string; seq: number; text: string; state: string }[]>;
  eventsHead(scope: string): Promise<string>;
  events(scope: string, after: string, limit?: number): Promise<WireEvent[]>;
  machineId(name?: string): Promise<string>;
  createConversation(opts?: { harness?: string; model?: string; effort?: string; title?: string; start?: boolean }): Promise<string>;
  postInput(conversationId: string, text: string, key?: string): Promise<{ input_id: string; state: string }>;
  /** Отправляет ввод и ждёт `run_finished` именно для него: простой по `waitIdle` сразу после отправки ненадёжен. */
  postAndFinish(conversationId: string, text: string, timeoutMs?: number): Promise<string>;
  detail(conversationId: string): Promise<{ activity: { state: string }; queue: { queued: number } }>;
  waitIdle(conversationId: string, timeoutMs?: number): Promise<void>;
}

export interface WireEvent {
  id: string;
  scope: string;
  scope_seq: string;
  scope_seq_from: string;
  committed_at: string;
  committed_at_from: string;
  kind: string;
  payload: Record<string, unknown>;
}

/** Клиент идёт на хаб напрямую: те же заголовки, что и у кромки, но без её цепочки и без хаоса. */
export function createHubClient(stack: Stack = getStack(), owner = 'owner'): HubClient {
  const hubUrl = stack.hubUrl;
  const call = (path: string, init: RequestInit = {}) => {
    const headers = new Headers(init.headers ?? {});
    headers.set('Remote-User', owner);
    headers.set('X-Canterlot-Proxy', stack.proxySecret);
    headers.set('Origin', stack.baseUrl);
    if (init.method && init.method !== 'GET') {
      headers.set('X-Canterlot-Client', '1');
      if (!headers.has('Idempotency-Key')) headers.set('Idempotency-Key', randomUUID());
      if (init.body && !headers.has('Content-Type')) headers.set('Content-Type', 'application/json');
    }
    return fetch(new URL(path, hubUrl), { ...init, headers });
  };
  const json = async <T>(path: string, init?: RequestInit): Promise<T> => {
    const res = await call(path, init);
    if (!res.ok) throw new Error(`${init?.method ?? 'GET'} ${path} -> ${res.status} ${await res.text()}`);
    return (await res.json()) as T;
  };
  const client: HubClient = {
    hubUrl,
    fetch: call,
    json,
    async history(conversationId, limit = 1000) {
      // Хаб отдаёт окно до 200 элементов; старые страницы собираем по next_before.
      let items: HistoryItem[] = [];
      let before: string | null = null;
      while (items.length < limit) {
        const query = new URLSearchParams({ limit: String(Math.min(200, limit - items.length)) });
        if (before) query.set('before', before);
        const page = await json<{ items: HistoryItem[]; next_before?: string | null }>(`/api/v1/conversations/${conversationId}/history?${query}`);
        items = [...page.items, ...items];
        before = page.next_before ?? null;
        if (!before || page.items.length === 0) break;
      }
      return items;
    },
    async inputs(conversationId) {
      return (await client.history(conversationId)).filter((item): item is InputItem => item.kind === 'input');
    },
    async textOf(conversationId) {
      const items = await client.history(conversationId);
      return items.filter((item): item is BlockItem => item.kind === 'block' && item.block_kind === 'text').map((block) => block.content).join('');
    },
    async inputLog(conversationId) {
      const log = new Map<string, { id: string; seq: number; text: string; state: string }>();
      for (const event of await client.events(`conversation:${conversationId}`, '0')) {
        const payload = event.payload as { input_id?: string; input_seq?: string; state?: string; content?: { type: string; text?: string }[] };
        if (!payload.input_id) continue;
        if (event.kind === 'input_queued') {
          log.set(payload.input_id, { id: payload.input_id, seq: Number(payload.input_seq), text: (payload.content ?? []).map((b) => b.text ?? '').join(''), state: payload.state ?? 'queued' });
        } else if (event.kind === 'input_state_changed' && log.has(payload.input_id)) {
          log.get(payload.input_id)!.state = payload.state ?? '';
        }
      }
      return [...log.values()].sort((a, b) => a.seq - b.seq);
    },
    async eventsHead(scope) {
      const range = await json<{ head: string }>(`/api/v1/events?scope=${encodeURIComponent(scope)}&after=0&limit=1`);
      return range.head;
    },
    async events(scope, after, limit = 100_000) {
      // Диапазон отдаётся пачками до 256 событий; идём по курсору до головы.
      const out: WireEvent[] = [];
      let cursor = after;
      while (out.length < limit) {
        const range = await json<{ events: WireEvent[]; head: string }>(`/api/v1/events?scope=${encodeURIComponent(scope)}&after=${cursor}&limit=256`);
        out.push(...range.events);
        const last = range.events.at(-1);
        if (!last || BigInt(last.scope_seq) >= BigInt(range.head)) break;
        cursor = last.scope_seq;
      }
      return out;
    },
    async machineId(name = stack.machineName) {
      const machines = await json<{ id: string; name: string }[]>('/api/v1/machines');
      const found = machines.find((machine) => machine.name === name);
      if (!found) throw new Error(`machine ${name} not found`);
      return found.id;
    },
    async createConversation(opts = {}) {
      const body = {
        machine_id: await client.machineId(),
        harness: opts.harness ?? 'synthetic',
        workdir: stack.workRoot,
        model: opts.model ?? 'synthetic-large',
        effort: opts.effort ?? 'medium',
        title: opts.title ?? null,
        start: opts.start ?? true
      };
      const created = await json<{ conversation_id: string }>('/api/v1/conversations', { method: 'POST', body: JSON.stringify(body) });
      return created.conversation_id;
    },
    async postInput(conversationId, text, key = randomUUID()) {
      return json(`/api/v1/conversations/${conversationId}/inputs`, {
        method: 'POST',
        headers: { 'Idempotency-Key': key },
        body: JSON.stringify({ content: [{ type: 'text', text }] })
      });
    },
    async postAndFinish(conversationId, text, timeoutMs = 600_000) {
      const scope = `conversation:${conversationId}`;
      let cursor = await client.eventsHead(scope);
      const { input_id: inputId } = await client.postInput(conversationId, text);
      const deadline = Date.now() + timeoutMs;
      for (;;) {
        const events = await client.events(scope, cursor);
        if (events.some((event) => event.kind === 'run_finished' && (event.payload as { input_id?: string }).input_id === inputId)) return inputId;
        if (events.length > 0) cursor = events.at(-1)!.scope_seq;
        if (Date.now() > deadline) throw new Error(`ввод ${inputId} не завершился за ${timeoutMs} мс`);
        await new Promise((r) => setTimeout(r, 250));
      }
    },
    detail(conversationId) {
      return json(`/api/v1/conversations/${conversationId}`);
    },
    async waitIdle(conversationId, timeoutMs = 60_000) {
      const deadline = Date.now() + timeoutMs;
      let last = '';
      while (Date.now() < deadline) {
        const detail = await client.detail(conversationId);
        last = `${detail.activity.state}/${detail.queue.queued}`;
        if (detail.activity.state === 'idle' && detail.queue.queued === 0) return;
        await new Promise((r) => setTimeout(r, 200));
      }
      throw new Error(`conversation ${conversationId} did not become idle: ${last}`);
    }
  };
  return client;
}

export async function sendChaos(control: string | number, command: Record<string, unknown>): Promise<Record<string, unknown>> {
  const res = await fetch(`http://127.0.0.1:${control}/chaos`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(command)
  });
  if (!res.ok) throw new Error(`chaos ${JSON.stringify(command)} -> ${res.status}`);
  return (await res.json()) as Record<string, unknown>;
}

export interface FrameLogItem {
  direction: 'client_to_hub' | 'hub_to_client';
  timestamp: number;
  delivered: boolean;
  op?: string;
  scope?: string;
  after?: string;
  event?: WireEvent;
}

export async function fetchFrames(control: string | number): Promise<FrameLogItem[]> {
  const res = await fetch(`http://127.0.0.1:${control}/frames`);
  if (!res.ok) throw new Error(`frames -> ${res.status}`);
  return (await res.json()) as FrameLogItem[];
}

export const composerOf = (page: Page) => page.getByRole('textbox', { name: 'Сообщение' });
export const sendButtonOf = (page: Page) => page.getByRole('button', { name: 'Отправить', exact: true });

/** Включает пробу клиента (ledger, отметки) до загрузки страницы. */
export async function enableProbe(page: Page): Promise<void> {
  await page.addInitScript(() => localStorage.setItem('canterlot.probe', '1'));
}

export async function createSessionUI(page: Page, stack: Stack, opts: { agent?: string; crunch?: boolean; resumeText?: string } = {}): Promise<string> {
  await page.goto(`${stack.baseUrl}/new`);
  await expect(page.locator('[data-new-session]')).toBeVisible({ timeout: 15_000 });
  await page.getByRole('combobox', { name: 'Машина' }).selectOption({ label: stack.machineName });
  await page.getByRole('textbox', { name: 'Рабочая папка' }).fill(stack.workRoot);
  await page.getByRole('radio', { name: opts.agent ?? 'Синтетический', exact: true }).check();
  await page.getByRole('combobox', { name: 'Модель' }).selectOption('synthetic-large');
  await page.getByRole('combobox', { name: 'Усилие' }).selectOption('medium');
  if (opts.crunch) {
    await page.getByRole('switch', { name: 'Кранч' }).click();
    if (opts.resumeText) await page.getByRole('textbox', { name: 'Текст продолжения' }).fill(opts.resumeText);
  }
  await page.getByRole('button', { name: 'Создать' }).click();
  await expect(page).toHaveURL(/\/c\/[0-9a-f-]{36}/, { timeout: 15_000 });
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
  return /\/c\/([0-9a-f-]{36})/.exec(page.url())![1]!;
}

export async function openSession(page: Page, stack: Stack, conversationId: string): Promise<void> {
  await page.goto(`${stack.baseUrl}/c/${conversationId}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
}

/** Печатает скрипт синтетического адаптера в поле ввода и нажимает «Отправить». */
export async function sendScript(page: Page, script: string): Promise<void> {
  await composerOf(page).fill(script);
  await sendButtonOf(page).click();
}

/** Текст ответа агента из ленты: только текстовые блоки карточек, без пузыря владельца с самим скриптом. */
export async function answerText(page: Page): Promise<string> {
  return (await page.locator('article.card [data-block]').allTextContents()).join('').trim();
}

export interface LedgerReport { ok: boolean; holes: [string, string][]; doubled: [string, string][] }

/** Сверка журнала приёма клиента с головой области на хабе; нужен enableProbe до загрузки страницы. */
export async function verifyLedger(page: Page, hub: HubClient, conversationId: string): Promise<LedgerReport> {
  const scope = `conversation:${conversationId}`;
  const head = await hub.eventsHead(scope);
  return page.evaluate(([name, end]) => window.__canterlot_probe!.ledger.verify(name!, end!), [scope, head]);
}
