import type { Page } from '@playwright/test';
import type { FakeHub } from './hub';
import type { FakeHubApi } from './api';

const PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
  'base64'
);

export async function installChatRoutes(page: Page, hub: FakeHub, api: FakeHubApi, ownerId: string): Promise<void> {
  await hub.wire.playwright(page);
  hub.routes.set('GET /api/v1/me', async (route) => {
    await route.fulfill({
      json: { login: 'owner', owner_id: ownerId },
      headers: { 'X-Server-Time-Ms': String(Date.now()) }
    });
  });
  await page.route('**/api/v1/conversations/**', async (route) => {
    const url = new URL(route.request().url());
    const parts = url.pathname.split('/');
    const id = parts[4] ?? '';
    const kind = parts[5] ?? '';
    const headers = { 'X-Server-Time-Ms': String(Date.now()) };
    if (route.request().method() === 'GET' && kind === 'snapshot') {
      await route.fulfill({ json: hub.snapshot(`conversation:${id}`), headers });
      return;
    }
    if (route.request().method() === 'GET' && kind === 'history') {
      const limit = url.searchParams.get('limit');
      await route.fulfill({
        json: await api.history(id, {
          before: url.searchParams.get('before') ?? undefined,
          after: url.searchParams.get('after') ?? undefined,
          limit: limit ? Number(limit) : undefined
        }),
        headers
      });
      return;
    }
    if (route.request().method() === 'GET' && kind === 'blocks') {
      const blockId = parts[6] ?? '';
      const offset = Number(url.searchParams.get('offset') ?? 0);
      await route.fulfill({ json: await api.block(id, blockId, offset), headers });
      return;
    }
    if (route.request().method() === 'POST' && kind === 'read') {
      await route.fulfill({ status: 204, headers });
      return;
    }
    await route.fallback();
  });
  await page.route('**/api/v1/media/**', async (route) => {
    await route.fulfill({ status: 200, contentType: 'image/png', body: PNG });
  });

  const inputs = new Map<string, string>();
  const scheduled = new Map<string, string>();
  const attachments = new Map<string, { size: number; offset: number; key: string }>();
  const headers = () => ({ 'X-Server-Time-Ms': String(Date.now()), 'cache-control': 'no-store' });
  const failure = (status: number, code: string, message: string, extra?: Record<string, string>) => ({
    status,
    headers: { ...headers(), ...extra },
    json: {
      error: {
        code,
        message,
        scope: 'delivery',
        retry: 'never',
        correlation_id: '00000000-0000-4000-8000-0000000000e1'
      }
    }
  });

  await page.route('**/api/v1/config', async (route) => {
    await route.fulfill({
      headers: headers(),
      json: {
        schema_version: 1,
        auth: { logout_url: null },
        machines: { disk_warn_pct: 85, metrics_stale_after_secs: 90 },
        limits: {
          max_attachment_bytes: 104_857_600,
          max_attachments_per_input: 10,
          max_input_meta_bytes: 65_536,
          max_input_text_bytes: 262_144,
          queue_limit: 100,
          upload_chunk_bytes: 8_388_608
        },
        defaults: { crunch_resume_text: 'продолжи с места остановки' },
        limit_thresholds: { stale_after_secs: 2100, warn_pcts: [80, 95], weekly_reserve_pct: 95 }
      }
    });
  });

  await page.route('**/api/v1/conversations/*/inputs', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const key = route.request().headers()['idempotency-key'] ?? '';
    const prior = key ? inputs.get(key) : undefined;
    const id = prior ?? crypto.randomUUID();
    if (key && !prior) inputs.set(key, id);
    await route.fulfill({ headers: headers(), json: { id, idempotency_key: key } });
  });

  await page.route('**/api/v1/conversations/*/scheduled', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const key = route.request().headers()['idempotency-key'] ?? '';
    const prior = key ? scheduled.get(key) : undefined;
    const id = prior ?? crypto.randomUUID();
    if (key && !prior) scheduled.set(key, id);
    const body = route.request().postDataJSON() as { run_at?: string; content?: unknown };
    await route.fulfill({
      headers: headers(),
      json: {
        id,
        conversation_id: new URL(route.request().url()).pathname.split('/')[4] ?? '',
        run_at: body.run_at ?? new Date().toISOString(),
        content: body.content ?? [],
        attachment_ids: [],
        state: 'pending',
        input_id: null,
        attempts: 0
      }
    });
  });

  await page.route('**/api/v1/conversations/*/interrupt', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    await route.fulfill({ headers: headers(), json: { status: 'sent_to_agent' } });
  });

  await page.route('**/api/v1/inputs/*/cancel', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    await route.fulfill({ headers: headers(), json: 'cancelled' });
  });

  await page.route('**/api/v1/scheduled/**', async (route) => {
    const method = route.request().method();
    if (method !== 'PATCH' && method !== 'DELETE') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-1) ?? '';
    await route.fulfill({
      headers: headers(),
      json: {
        id,
        conversation_id: id,
        run_at: new Date().toISOString(),
        content: [],
        attachment_ids: [],
        state: method === 'DELETE' ? 'cancelled' : 'pending',
        input_id: null,
        attempts: 0
      }
    });
  });

  await page.route(/\/api\/v1\/attachments(?:\/|$)/, async (route) => {
    const url = new URL(route.request().url());
    const method = route.request().method();
    const parts = url.pathname.split('/');
    if (method === 'POST' && parts.length === 4) {
      const key = route.request().headers()['idempotency-key'] ?? crypto.randomUUID();
      const existing = [...attachments.entries()].find(([, item]) => item.key === key);
      if (existing) {
        await route.fulfill({
          headers: headers(),
          json: { id: existing[0], state: 'uploading', upload: { upload_chunk_bytes: 8_388_608 } }
        });
        return;
      }
      const body = route.request().postDataJSON() as { size?: number };
      const id = crypto.randomUUID();
      attachments.set(id, { size: body.size ?? 0, offset: 0, key });
      await route.fulfill({
        headers: headers(),
        json: { id, state: 'uploading', upload: { upload_chunk_bytes: 8_388_608 } }
      });
      return;
    }
    const id = parts[4] ?? '';
    const tail = parts[5] ?? '';
    const record = attachments.get(id);
    if (method === 'HEAD' && tail === 'content') {
      await route.fulfill({
        status: 200,
        headers: { ...headers(), 'Upload-Offset': String(record?.offset ?? 0), 'Upload-Length': String(record?.size ?? 0) },
        body: ''
      });
      return;
    }
    if (method === 'PATCH' && tail === 'content') {
      const offset = Number(route.request().headers()['upload-offset'] ?? '0');
      const current = record?.offset ?? 0;
      if (!record || offset !== current) {
        await route.fulfill(failure(409, 'offset_mismatch', 'Смещение не совпадает', { 'Upload-Offset': String(current) }));
        return;
      }
      record.offset += route.request().postDataBuffer()?.length ?? 0;
      await route.fulfill({
        status: 200,
        headers: { ...headers(), 'Upload-Offset': String(record.offset) },
        body: ''
      });
      return;
    }
    if (method === 'POST' && tail === 'complete') {
      await route.fulfill({ headers: headers(), json: { id, state: 'ready' } });
      return;
    }
    if (method === 'DELETE') {
      attachments.delete(id);
      await route.fulfill({ status: 204, headers: headers() });
      return;
    }
    await route.fallback();
  });

  const keys = new Map<string, unknown>();

  await page.route(/\/api\/v1\/conversations(?:\?|#|$)/, async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname !== '/api/v1/conversations') return route.fallback();
    if (route.request().method() === 'GET') {
      const q = url.searchParams.get('q') ?? '';
      const items = [...api.list.values()].filter((item) => item.title.includes(q));
      await route.fulfill({ headers: headers(), json: { counts: api.counts, items, next_cursor: null } });
      return;
    }
    if (route.request().method() === 'POST') {
      const key = route.request().headers()['idempotency-key'] ?? '';
      const prior = key ? keys.get(key) : undefined;
      if (prior) {
        await route.fulfill({ status: 201, headers: headers(), json: prior });
        return;
      }
      const body = route.request().postDataJSON() as { machine_id?: string; workdir?: string; harness?: string; model?: string; effort?: string };
      const id = api.id();
      const created = { conversation_id: id };
      if (key) keys.set(key, created);
      const row = {
        id,
        title: 'Новая сессия',
        title_source: 'auto',
        color: null,
        pinned: false,
        archived: false,
        groups: [],
        machine: { id: body.machine_id ?? api.id(), name: 'workstation' },
        harness: { id: body.harness ?? 'claude', title: 'Claude Code', provider: 'claude' },
        workdir: body.workdir ?? '/',
        created_at: new Date().toISOString(),
        model: body.model ?? '',
        effort: body.effort ?? '',
        permission_mode: 'default',
        activity: { state: 'idle' as const, evidence: {}, since: new Date().toISOString() },
        unread_count: 0,
        queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
        crunch: { enabled: false },
        last_activity_at: null
      };
      api.list.set(id, row);
      api.details.set(id, { ...row });
      api.bindConversation(id);
      await route.fulfill({ status: 201, headers: headers(), json: created });
      return;
    }
    await route.fallback();
  });

  await page.route('**/api/v1/conversations/*', async (route) => {
    if (route.request().method() !== 'PATCH') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-1) ?? '';
    const body = route.request().postDataJSON() as { title?: string; color?: string; pinned?: boolean; archived?: boolean; force?: boolean };
    const row = api.list.get(id);
    if (!row) return route.fallback();
    if (body.archived && !body.force && row.crunch.enabled) {
      await route.fulfill({
        status: 409,
        headers: headers(),
        json: {
          error: {
            code: 'archive_active',
            message: 'Сессия ещё работает',
            scope: 'delivery',
            retry: 'explicit_new_operation',
            correlation_id: '00000000-0000-4000-8000-0000000000e3',
            details: { reasons: ['crunch'] }
          }
        }
      });
      return;
    }
    if (body.title !== undefined && body.title !== null) {
      row.title = body.title;
      row.title_source = 'user';
    }
    if (body.color !== undefined) row.color = body.color;
    if (body.pinned !== undefined && body.pinned !== null) row.pinned = body.pinned;
    if (body.archived !== undefined && body.archived !== null) row.archived = body.archived;
    api.counts = recount(api);
    hub.append('conversations', {
      kind: 'conversation_updated',
      conversation_id: id,
      payload: {
        id: row.id,
        title: row.title,
        title_source: row.title_source === 'auto' ? 'auto' : 'user',
        color: row.color,
        pinned: row.pinned,
        archived: row.archived,
        groups: row.groups,
        machine: row.machine,
        harness: row.harness,
        workdir: row.workdir,
        created_at: row.created_at,
        counts: api.counts
      }
    });
    hub.flush();
    await route.fulfill({ headers: headers(), json: row });
  });

  await page.route('**/api/v1/conversations/*/settings', async (route) => {
    if (route.request().method() !== 'PATCH') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-2) ?? '';
    const body = route.request().postDataJSON() as { model?: string; effort?: string; permission_mode?: string };
    const row = api.list.get(id);
    const detail = api.details.get(id) as (Record<string, unknown> & { settings_midsession?: boolean }) | undefined;

    if (body.model === 'unknown-model' || body.model === 'invalid') {
      await route.fulfill({
        status: 422,
        headers: headers(),
        json: {
          error: {
            code: 'model_unknown',
            message: 'Модель неизвестна или недоступна на этой машине',
            scope: 'delivery',
            retry: 'explicit_new_operation',
            correlation_id: '00000000-0000-4000-8000-0000000000e4'
          }
        }
      });
      return;
    }
    if (body.effort === 'forbidden-effort' || (body.model === 'haiku' && body.effort === 'high')) {
      await route.fulfill({
        status: 422,
        headers: headers(),
        json: {
          error: {
            code: 'effort_not_allowed',
            message: 'Усилие не поддерживается для этой модели',
            scope: 'delivery',
            retry: 'explicit_new_operation',
            correlation_id: '00000000-0000-4000-8000-0000000000e5'
          }
        }
      });
      return;
    }
    if (body.permission_mode === 'invalid-permission') {
      await route.fulfill({
        status: 422,
        headers: headers(),
        json: {
          error: {
            code: 'permission_mode_unknown',
            message: 'Режим разрешений не поддерживается',
            scope: 'delivery',
            retry: 'explicit_new_operation',
            correlation_id: '00000000-0000-4000-8000-0000000000e6'
          }
        }
      });
      return;
    }
    if (detail?.settings_midsession === false) {
      await route.fulfill({
        status: 422,
        headers: headers(),
        json: {
          error: {
            code: 'settings_midsession_unsupported',
            message: 'Настройка посреди сессии не поддерживается',
            scope: 'delivery',
            retry: 'explicit_new_operation',
            correlation_id: '00000000-0000-4000-8000-0000000000e7'
          }
        }
      });
      return;
    }

    if (row) {
      if (body.model) row.model = body.model;
      if (body.effort) row.effort = body.effort;
      if (body.permission_mode) row.permission_mode = body.permission_mode;
    }
    if (detail) {
      if (body.model) detail.model = body.model;
      if (body.effort) detail.effort = body.effort;
      if (body.permission_mode) detail.permission_mode = body.permission_mode;
    }

    hub.append(`conversation:${id}`, {
      kind: 'conversation_settings_changed',
      conversation_id: id,
      payload: {
        effective: {
          model: row?.model ?? body.model ?? 'opus-5.5',
          effort: row?.effort ?? body.effort ?? 'medium',
          permission_mode: row?.permission_mode ?? body.permission_mode ?? 'default'
        },
        pending: {
          model: body.model ?? null,
          effort: body.effort ?? null,
          permission_mode: body.permission_mode ?? null
        },
        rev: 1
      }
    });
    hub.flush();

    await route.fulfill({
      status: 200,
      headers: headers(),
      json: {
        effective: {
          model: row?.model ?? body.model ?? 'opus-5.5',
          effort: row?.effort ?? body.effort ?? 'medium',
          permission_mode: row?.permission_mode ?? body.permission_mode ?? 'default'
        },
        pending: {
          model: body.model ?? null,
          effort: body.effort ?? null,
          permission_mode: body.permission_mode ?? null
        },
        rev: 1
      }
    });
  });

  await page.route('**/api/v1/conversations/*/crunch', async (route) => {
    if (route.request().method() !== 'PUT') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-2) ?? '';
    const body = route.request().postDataJSON() as { enabled: boolean; resume_text?: string | null };
    const row = api.list.get(id);
    const detail = api.details.get(id) as (Record<string, unknown> & { settings_midsession?: boolean }) | undefined;
    if (row) row.crunch = { enabled: body.enabled };
    if (detail) detail.crunch = { enabled: body.enabled, resume_pending: false, resume_text: body.resume_text };

    hub.append(`conversation:${id}`, {
      kind: 'crunch_changed',
      conversation_id: id,
      payload: {
        enabled: body.enabled,
        resume_text: body.resume_text ?? null,
        resume_pending: false,
        reserve_override_until: null
      }
    });
    hub.flush();
    await route.fulfill({ status: 200, headers: headers(), json: { enabled: body.enabled, resume_text: body.resume_text } });
  });

  await page.route('**/api/v1/conversations/*/spend-reserve', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-2) ?? '';
    const detail = api.details.get(id) as (Record<string, unknown> & { _spendOutcome?: string; queue?: { hold_reasons?: { reason?: string }[] } }) | undefined;
    const outcome = detail?._spendOutcome ?? 'continued';

    if (outcome === 'continued') {
      if (detail?.queue?.hold_reasons) {
        detail.queue.hold_reasons = detail.queue.hold_reasons.filter(
          (h) => h.reason !== 'weekly_reserve'
        );
      }
      hub.append(`conversation:${id}`, {
        kind: 'queue_state_changed',
        conversation_id: id,
        payload: {
          pause_reasons: [],
          hold_reasons: [],
          run_slot: 'free'
        }
      });
      hub.flush();
    }

    await route.fulfill({ status: 200, headers: headers(), json: outcome });
  });

  await page.route('**/api/v1/conversations/*/stop', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-2) ?? '';
    const body = route.request().postDataJSON() as { force?: boolean } | null;
    const force = Boolean(body?.force);

    hub.append(`conversation:${id}`, {
      kind: 'execution_state_changed',
      conversation_id: id,
      payload: { generation: 1, outcome: null, state: 'stopping' }
    });
    hub.append(`conversation:${id}`, {
      kind: 'queue_state_changed',
      conversation_id: id,
      payload: {
        pause_reasons: [{ reason: 'user_stop' }],
        hold_reasons: force ? [{ reason: 'no_live_execution' }] : [],
        run_slot: 'free'
      }
    });
    if (force) {
      hub.append(`conversation:${id}`, {
        kind: 'execution_state_changed',
        conversation_id: id,
        payload: { generation: 1, state: 'stopped', outcome: 'force_stopped' }
      });
    }
    hub.flush();
    await route.fulfill({ status: 200, headers: headers(), json: { status: 'stopping' } });
  });

  await page.route('**/api/v1/conversations/*/resume', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-2) ?? '';
    const body = route.request().postDataJSON() as { continue_queue?: boolean } | null;
    void body;

    hub.append(`conversation:${id}`, {
      kind: 'execution_state_changed',
      conversation_id: id,
      payload: { generation: 1, outcome: null, state: 'running' }
    });
    hub.append(`conversation:${id}`, {
      kind: 'queue_state_changed',
      conversation_id: id,
      payload: {
        pause_reasons: [],
        hold_reasons: [],
        run_slot: 'free'
      }
    });
    hub.flush();
    await route.fulfill({ status: 200, headers: headers(), json: {} });
  });

  await page.route('**/api/v1/conversations/*/queue/continue', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-3) ?? '';

    hub.append(`conversation:${id}`, {
      kind: 'queue_state_changed',
      conversation_id: id,
      payload: {
        pause_reasons: [],
        hold_reasons: [],
        run_slot: 'free'
      }
    });
    hub.flush();
    await route.fulfill({ status: 200, headers: headers(), json: { remaining: [], removed: [{ reason: 'user_stop' }] } });
  });

  await page.route('**/api/v1/conversations/*/executions', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-2) ?? '';

    hub.append(`conversation:${id}`, {
      kind: 'execution_state_changed',
      conversation_id: id,
      payload: { generation: 1, outcome: null, state: 'running' }
    });
    hub.append(`conversation:${id}`, {
      kind: 'queue_state_changed',
      conversation_id: id,
      payload: {
        pause_reasons: [],
        hold_reasons: [],
        run_slot: 'free'
      }
    });
    hub.flush();
    await route.fulfill({
      status: 201,
      headers: headers(),
      json: {
        command_id: api.id(),
        execution_id: api.id(),
        generation: 1
      }
    });
  });

  await page.route('**/api/v1/conversations/*/groups', async (route) => {
    if (route.request().method() !== 'PUT') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-2) ?? '';
    const body = route.request().postDataJSON() as { group_ids?: string[] };
    const row = api.list.get(id);
    if (!row) return route.fallback();
    row.groups = body.group_ids ?? [];
    api.counts = recount(api);
    hub.append('conversations', {
      kind: 'conversation_updated',
      conversation_id: id,
      payload: {
        id: row.id,
        title: row.title,
        title_source: 'user',
        color: row.color,
        pinned: row.pinned,
        archived: row.archived,
        groups: row.groups,
        machine: row.machine,
        harness: row.harness,
        workdir: row.workdir,
        created_at: row.created_at,
        counts: api.counts
      }
    });
    hub.flush();
    await route.fulfill({ headers: headers(), json: { group_ids: row.groups } });
  });

  await page.route('**/api/v1/groups', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const body = route.request().postDataJSON() as { name?: string };
    const group = { id: api.id(), name: body.name ?? 'Группа', position: api.groups.length, created_at: new Date().toISOString() };
    api.groups = [...api.groups, group];
    hub.append('conversations', { kind: 'group_changed', payload: { group, groups: api.groups, counts: api.counts } });
    hub.flush();
    await route.fulfill({ status: 201, headers: headers(), json: group });
  });

  await page.route('**/api/v1/groups/*', async (route) => {
    const id = new URL(route.request().url()).pathname.split('/').at(-1) ?? '';
    if (route.request().method() === 'PATCH') {
      const body = route.request().postDataJSON() as { name?: string | null; position?: number | null };
      api.groups = api.groups.map((group) => group.id === id ? { ...group, name: body.name ?? group.name, position: body.position ?? group.position } : group);
    } else if (route.request().method() === 'DELETE') {
      api.groups = api.groups.filter((group) => group.id !== id);
    } else {
      return route.fallback();
    }
    hub.append('conversations', { kind: 'group_changed', payload: { group: null, groups: api.groups, counts: api.counts } });
    hub.flush();
    await route.fulfill({ headers: headers(), json: { id } });
  });

  await page.route('**/api/v1/conversations/*/runs/*', async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    const runId = new URL(route.request().url()).pathname.split('/').at(-1) ?? '';
    const info = api.runs.get(runId);
    if (!info) {
      await route.fulfill({
        status: 404,
        headers: headers(),
        json: { error: { code: 'not_found', message: 'Run not found' } }
      });
      return;
    }
    await route.fulfill({ headers: headers(), json: info });
  });

  await page.route('**/api/v1/conversations/*/delegation', async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-2) ?? '';
    const list = api.delegations.get(id) ?? [];
    await route.fulfill({ headers: headers(), json: list });
  });

  await page.route('**/api/v1/machines/*/workdirs/recent', async (route) => {
    await route.fulfill({ headers: headers(), json: { workdirs: ['/srv/canterlot', '/srv/notes'] } });
  });
}

function recount(api: FakeHubApi) {
  const items = [...api.list.values()];
  return {
    all: items.filter((item) => !item.archived).length,
    pinned: items.filter((item) => item.pinned && !item.archived).length,
    archived: items.filter((item) => item.archived).length,
    groups: api.counts.groups.map((group) => ({
      ...group,
      count: items.filter((item) => !item.archived && item.groups.includes(group.id)).length
    }))
  };
}

/** Числа макетов 09/10, даты относительно дня запуска. */
export async function seedSecondaryScreens(page: Page, api: FakeHubApi, ids: { machine: string; pinned: string }, logout: string | null) {
  const metrics = { machine_time_ms: Date.now(), cpu_cores: 8, cpu_permille: 240, mem_total_bytes: 32 * 1024 ** 3, mem_available_bytes: Math.round(19.2 * 1024 ** 3), load1_centi: 184, load5_centi: 162, load15_centi: 141, disks: [{ label: 'data', total_bytes: 1000, free_bytes: 120 }, { label: 'root_0', total_bytes: 1000, free_bytes: 600 }], executions: [] };
  const machine = api.machines.get(ids.machine)!;
  Object.assign(machine, { os: 'Linux · x86_64', availability: 'available', heartbeat_age_ms: 2000, availability_since: new Date(Date.now() - 3_600_000).toISOString(), metrics: { ...metrics, observed_at: new Date().toISOString() }, warnings: ['disk_usage_high'] });
  const historyRequests: URLSearchParams[] = [];
  await page.route('**/api/v1/config', r => r.fulfill({ json: { schema_version: 1, auth: { logout_url: logout }, machines: { disk_warn_pct: 85, metrics_stale_after_secs: 90 }, limits: { max_attachment_bytes: 104_857_600, max_attachments_per_input: 10, upload_chunk_bytes: 8_388_608, max_input_text_bytes: 262_144, queue_limit: 100 }, defaults: { crunch_resume_text: 'продолжи с места остановки' }, limit_thresholds: { stale_after_secs: 2100, warn_pcts: [80, 95], weekly_reserve_pct: 95 } } }));
  await page.route('**/api/v1/limits/history?*', r => {
    const query = new URL(r.request().url()).searchParams; historyRequests.push(query);
    const provider = query.get('provider') ?? 'claude';
    const start = new Date(query.get('from')!);
    const days = Array.from({ length: 7 }, (_, n) => { const date = new Date(start); date.setDate(date.getDate() + n); return date; });
    return r.fulfill({ json: {
      series: [{ provider, five_hour: days.map((date, n) => ({ observed_at: date.toISOString(), used_permille: 180 + n * 30, used_pct: 18 + n * 3 })), weekly: days.map((date, n) => ({ observed_at: date.toISOString(), used_permille: 560 + n * 60, used_pct: 56 + n * 6 })) }],
      usage: [{ day: new Intl.DateTimeFormat('en-CA', { timeZone: query.get('tz') ?? 'UTC' }).format(days.at(-1)!), conversation_id: ids.pinned, provider, agent: 'main', model: 'opus', input_tokens: 126000, output_tokens: 8400, cache_read_tokens: 1000, cache_write_tokens: 2000, weekly_delta_permille: 32, runs: 2, overlapped_runs: 0 }, { day: new Intl.DateTimeFormat('en-CA', { timeZone: query.get('tz') ?? 'UTC' }).format(days.at(-1)!), conversation_id: ids.pinned, provider, agent: 'agent-1', model: 'haiku', input_tokens: 18000, output_tokens: 1200, cache_read_tokens: 0, cache_write_tokens: 0, weekly_delta_permille: 2, runs: 1, overlapped_runs: 1 }]
    } });
  });
  return { metrics, historyRequests };
}
