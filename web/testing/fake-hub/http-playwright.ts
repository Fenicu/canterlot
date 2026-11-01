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
      if (api.deleted.has(id)) {
        await route.fulfill({
          status: 404,
          headers,
          json: { error: { code: 'conversation_deleted', message: 'Сессия удалена', scope: 'browser', retry: 'never', correlation_id: '00000000-0000-4000-8000-0000000000e5' } }
        });
        return;
      }
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
      const body = route.request().postDataJSON() as { machine_id?: string; workdir?: string; harness?: string; model?: string; effort?: string; permission_mode?: string };
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
        permission_mode: body.permission_mode ?? 'bypass',
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

  const deletions = new Map<string, string>();
  const refuse = (status: number, code: string, message: string) => ({
    status,
    headers: headers(),
    json: { error: { code, message, scope: 'browser', retry: 'never', correlation_id: '00000000-0000-4000-8000-0000000000e4' } }
  });
  await page.route('**/api/v1/conversations/*', async (route) => {
    if (route.request().method() === 'DELETE') {
      const id = new URL(route.request().url()).pathname.split('/').at(-1) ?? '';
      const key = route.request().headers()['idempotency-key'] ?? '';
      if (deletions.get(key) === id) return route.fulfill({ status: 204, headers: headers() });
      if (api.deleted.has(id)) return route.fulfill(refuse(404, 'conversation_deleted', 'Сессия удалена'));
      if (!api.list.has(id)) return route.fulfill(refuse(404, 'not_found', 'Не найдено'));
      const execution = api.details.get(id)?.execution as { state?: string } | null | undefined;
      if (execution && !['stopped', 'failed'].includes(execution.state ?? '')) {
        return route.fulfill({ ...refuse(409, 'conversation_live', 'Сначала остановите сессию'), status: 409 });
      }
      deletions.set(key, id);
      api.deleteConversation(id);
      await route.fulfill({ status: 204, headers: headers() });
      return;
    }
    if (route.request().method() === 'GET') {
      const id = new URL(route.request().url()).pathname.split('/').at(-1) ?? '';
      const detail = api.details.get(id);
      if (!detail) return route.fulfill(refuse(404, 'not_found', 'Не найдено'));
      const execution = detail.execution as { state?: string } | null | undefined;
      const live = execution && !['stopped', 'failed'].includes(execution.state ?? '') ? execution : null;
      await route.fulfill({ headers: headers(), json: { ...detail, execution: live, activity: detail.activity ?? { state: 'idle' } } });
      return;
    }
    if (route.request().method() !== 'PATCH') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-1) ?? '';
    const body = route.request().postDataJSON() as { title?: string; color?: string; pinned?: boolean; archived?: boolean; force?: boolean };
    const row = api.list.get(id);
    if (!row) return route.fallback();
    const turn = ['working', 'waiting_for_user', 'stalled'].includes((row.activity as { state?: string } | undefined)?.state ?? '') && Boolean(api.details.get(id)?.execution);
    if (body.archived && !body.force && (row.crunch.enabled || turn)) {
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
            details: { reasons: [...(row.crunch.enabled ? ['crunch'] : []), ...(turn ? ['live_execution'] : [])] }
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
    const body = route.request().postDataJSON() as { model?: string; effort?: string; permission_mode?: string; instruction_profile?: string };
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
    if (body.instruction_profile !== undefined) {
      const known = body.instruction_profile === 'clean' || body.instruction_profile === 'owner' || api.presets.has(body.instruction_profile);
      if (!known) {
        await route.fulfill({
          status: 422,
          headers: headers(),
          json: {
            error: {
              code: 'profile_unknown',
              message: 'Пресет не найден',
              scope: 'delivery',
              retry: 'explicit_new_operation',
              correlation_id: '00000000-0000-4000-8000-0000000000e9'
            }
          }
        });
        return;
      }
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
    const appliedPreset = (ref: string | undefined): Record<string, unknown> | undefined => {
      if (ref === undefined) return undefined;
      if (ref === 'clean') return { id: 'clean', kind: 'clean', title: 'Без инструкций', revision: null, current_revision: null, deleted: false };
      if (ref === 'owner') return { id: 'owner', kind: 'owner', title: 'Как у владельца', revision: null, current_revision: null, deleted: false };
      const preset = api.presets.get(ref);
      if (!preset) return { id: ref, kind: 'custom', title: ref, revision: null, current_revision: null, deleted: true };
      return { id: preset.id, kind: 'custom', title: preset.title, revision: preset.revision, current_revision: preset.revision, deleted: false };
    };
    const previous = detail?.settings as { effective?: { instruction_profile?: string; instruction_preset?: unknown }; pending?: { instruction_profile?: string | null } } | undefined;
    const effectiveRef = previous?.effective?.instruction_profile ?? 'clean';
    const pendingRef = body.instruction_profile !== undefined ? body.instruction_profile : (previous?.pending?.instruction_profile ?? null);
    const effective = {
      model: row?.model ?? body.model ?? 'opus-5.5',
      effort: row?.effort ?? body.effort ?? 'medium',
      permission_mode: row?.permission_mode ?? body.permission_mode ?? 'owner',
      instruction_profile: effectiveRef,
      instruction_preset: appliedPreset(effectiveRef)
    };
    const pending = {
      model: body.model ?? null,
      effort: body.effort ?? null,
      permission_mode: body.permission_mode ?? null,
      instruction_profile: pendingRef
    };
    if (detail) {
      detail.settings = { effective, pending, rev: 1 };
    }

    hub.append(`conversation:${id}`, {
      kind: 'conversation_settings_changed',
      conversation_id: id,
      payload: { effective, pending, rev: 1 }
    });
    hub.flush();

    await route.fulfill({
      status: 200,
      headers: headers(),
      json: { effective, pending, rev: 1 }
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
    const live = api.details.get(id);
    if (live) live.execution = { state: 'stopped', generation: 1 };
    await route.fulfill({ status: 200, headers: headers(), json: { status: 'stopping' } });
  });

  await page.route('**/api/v1/conversations/*/resume', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-2) ?? '';
    const body = route.request().postDataJSON() as { continue_queue?: boolean } | null;
    const held = hub.log(`conversation:${id}`).findLast((event) => event.kind === 'queue_state_changed')?.payload as { pause_reasons?: unknown[] } | undefined;

    hub.append(`conversation:${id}`, {
      kind: 'execution_state_changed',
      conversation_id: id,
      payload: { generation: 1, outcome: null, state: 'running' }
    });
    hub.append(`conversation:${id}`, {
      kind: 'queue_state_changed',
      conversation_id: id,
      payload: {
        pause_reasons: body?.continue_queue ? [] : (held?.pause_reasons ?? []),
        hold_reasons: [],
        run_slot: 'free'
      }
    });
    hub.flush();
    await route.fulfill({ status: 200, headers: headers(), json: {} });
  });

  await page.route('**/api/v1/conversations/*/compact', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const id = new URL(route.request().url()).pathname.split('/').at(-2) ?? '';
    hub.append(`conversation:${id}`, {
      kind: 'queue_state_changed',
      conversation_id: id,
      payload: {
        pause_reasons: [{ reason: 'session_stuck', compaction: 'requested' }],
        hold_reasons: [],
        run_slot: 'free'
      }
    });
    hub.flush();
    await route.fulfill({
      status: 200,
      headers: headers(),
      json: { requested: { command_id: crypto.randomUUID() } }
    });
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

  await page.route('**/api/v1/machines', async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    await route.fulfill({ headers: headers(), json: [...api.machines.values()] });
  });

  await page.route('**/api/v1/spawn-whitelist', async (route) => {
    const method = route.request().method();
    if (method === 'GET') {
      await route.fulfill({ headers: headers(), json: api.spawnWhitelistView() });
      return;
    }
    if (method !== 'PUT') return route.fallback();
    const key = route.request().headers()['idempotency-key'];
    if (!key) {
      await route.fulfill({ status: 400, headers: headers(), json: errorBody('invalid_request', 'Нужен Idempotency-Key') });
      return;
    }
    api.spawnWhitelistKeys.push(key);
    const body = route.request().postDataJSON() as { entries: { harness?: string; model?: string }[] | null };
    const entries = body.entries;
    const seen = new Set<string>();
    const valid = entries === null || (Array.isArray(entries) && entries.length <= 64 && entries.every((entry) => {
      if (!entry.harness || !entry.model || entry.model.length > 256) return false;
      const token = `${entry.harness}/${entry.model}`;
      if (seen.has(token)) return false;
      seen.add(token);
      return true;
    }));
    if (!valid) {
      await route.fulfill({ status: 422, headers: headers(), json: errorBody('spawn_whitelist_invalid', 'Некорректный белый список') });
      return;
    }
    api.spawnWhitelist = entries as { harness: string; model: string }[] | null;
    await route.fulfill({ headers: headers(), json: api.spawnWhitelistView() });
  });

  await page.route('**/api/v1/presets', async (route) => {
    const method = route.request().method();
    if (method === 'GET') {
      await route.fulfill({ headers: headers(), json: api.presetList() });
      return;
    }
    if (method === 'POST') {
      const body = route.request().postDataJSON() as { title?: string; description?: string | null; text?: string };
      const error = validatePresetBody(body, api, null);
      if (error) {
        await route.fulfill({ status: error.status, headers: headers(), json: errorBody(error.code, error.message, error.details) });
        return;
      }
      const id = api.id();
      const text = body.text!.trim();
      const record = {
        id,
        kind: 'custom' as const,
        title: body.title!.trim(),
        description: (body.description ?? '').trim(),
        revision: 1,
        text_bytes: new TextEncoder().encode(text).length,
        text,
        created: { by: 'owner', conversation_id: null, conversation_title: null, at: new Date().toISOString() },
        last_edit: null,
        used_by: 0
      };
      api.presets.set(id, record);
      hub.append('conversations', { kind: 'instruction_preset_changed', payload: { preset: api.presetSummary(record), deleted: false } });
      hub.flush();
      await route.fulfill({ status: 201, headers: headers(), json: { ...api.presetSummary(record), text } });
      return;
    }
    await route.fallback();
  });

  await page.route(/\/api\/v1\/presets\/[^/]+$/, async (route) => {
    const id = new URL(route.request().url()).pathname.split('/').at(-1) ?? '';
    const method = route.request().method();
    const stored = api.presets.get(id) ?? (id === 'clean' || id === 'owner' ? builtinPreset(api, id) : undefined);
    if (method === 'GET') {
      if (!stored) {
        await route.fulfill({ status: 404, headers: headers(), json: errorBody('preset_not_found', 'Пресет не найден') });
        return;
      }
      await route.fulfill({ headers: headers(), json: { ...api.presetSummary(stored), text: stored.text } });
      return;
    }
    if (!stored || stored.kind !== 'custom') {
      await route.fulfill({ status: stored ? 409 : 404, headers: headers(), json: stored ? errorBody('preset_builtin', 'Встроенный пресет нельзя изменить или удалить') : errorBody('preset_not_found', 'Пресет не найден') });
      return;
    }
    if (method === 'DELETE') {
      api.applyPresetDeletion(id);
      await route.fulfill({ status: 204, headers: headers() });
      return;
    }
    if (method === 'PATCH') {
      const body = route.request().postDataJSON() as { title?: string; description?: string | null; text?: string; base_revision?: number };
      if (body.base_revision !== stored.revision) {
        await route.fulfill({ status: 409, headers: headers(), json: errorBody('preset_revision_conflict', 'Пресет уже изменён', { current_revision: stored.revision }) });
        return;
      }
      const merged = {
        title: body.title !== undefined ? body.title : stored.title,
        description: body.description !== undefined ? (body.description ?? '') : stored.description,
        text: body.text !== undefined ? body.text : stored.text ?? ''
      };
      const error = validatePresetBody(merged, api, id);
      if (error) {
        await route.fulfill({ status: error.status, headers: headers(), json: errorBody(error.code, error.message, error.details) });
        return;
      }
      const text = merged.text.trim();
      stored.title = merged.title.trim();
      stored.description = (merged.description ?? '').trim();
      stored.text = text;
      stored.text_bytes = new TextEncoder().encode(text).length;
      stored.revision = (stored.revision ?? 0) + 1;
      hub.append('conversations', { kind: 'instruction_preset_changed', payload: { preset: api.presetSummary(stored), deleted: false } });
      hub.flush();
      await route.fulfill({ headers: headers(), json: { ...api.presetSummary(stored), text } });
      return;
    }
    await route.fallback();
  });

  await page.route('**/api/v1/conversations/*/runs/*', async (route) => {    if (route.request().method() !== 'GET') return route.fallback();
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

  await page.route('**/api/v1/machines/*/workdirs/suggest?*', async (route) => {
    const path = new URL(route.request().url()).searchParams.get('path') ?? '';
    await route.fulfill({ headers: headers(), json: api.suggestWorkdirs(path) });
  });

  const decisions = api.decisions;

  await page.route('**/api/v1/conversations/*/requests/*/decision', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const url = new URL(route.request().url());
    const parts = url.pathname.split('/');
    const convId = parts[4] ?? '';
    const reqId = parts[6] ?? '';
    const key = route.request().headers()['idempotency-key'] ?? '';
    const body = route.request().postDataJSON() as Record<string, unknown>;

    let isClosed = false;
    let closedReason = 'closed';
    for (const ev of hub.log(`conversation:${convId}`)) {
      const p = ev.payload as Record<string, unknown>;
      if (String(p?.request_id ?? '') === reqId && ev.kind === 'request_closed') {
        isClosed = true;
        closedReason = String(p.reason ?? 'closed');
      }
    }

    if (isClosed) {
      await route.fulfill({
        status: 409,
        headers: headers(),
        json: {
          error: {
            code: 'request_not_open',
            message: 'Запрос уже закрыт',
            scope: 'delivery',
            retry: 'explicit_new_operation',
            correlation_id: '00000000-0000-4000-8000-0000000000c2',
            details: {
              current_state: 'closed_without_decision',
              reason: closedReason
            }
          }
        }
      });
      return;
    }

    const existing = decisions.get(reqId);
    if (existing) {
      if (JSON.stringify(existing.decision) === JSON.stringify(body)) {
        await route.fulfill({
          status: 200,
          headers: headers(),
          json: {
            request_id: reqId,
            winner_decision_id: existing.winnerId,
            state: 'decision_saved',
            decision: existing.decision
          }
        });
        return;
      } else {
        await route.fulfill({
          status: 409,
          headers: headers(),
          json: {
            error: {
              code: 'decision_conflict',
              message: 'Решение уже принято другим участником',
              scope: 'delivery',
              retry: 'explicit_new_operation',
              correlation_id: '00000000-0000-4000-8000-0000000000c1',
              details: {
                winner_decision_id: existing.winnerId,
                winner_decision: existing.decision,
                current_state: 'decision_saved'
              }
            }
          }
        });
        return;
      }
    }

    const winnerId = crypto.randomUUID();
    decisions.set(reqId, { winnerId, decision: body, key });

    hub.append(`conversation:${convId}`, {
      kind: 'request_decided',
      conversation_id: convId,
      payload: {
        request_id: reqId,
        winner_decision_id: winnerId,
        decision: body
      }
    });
    hub.flush();

    if (api.decisionAutoProgress === true) {
      setTimeout(() => {
        hub.append(`conversation:${convId}`, {
          kind: 'request_transmitting',
          conversation_id: convId,
          payload: { request_id: reqId }
        });
        hub.flush();

        setTimeout(() => {
          hub.append(`conversation:${convId}`, {
            kind: 'request_closed',
            conversation_id: convId,
            payload: { request_id: reqId, reason: 'applied' }
          });
          const detail = api.details.get(convId);
          if (detail) {
            detail.activity = {
              state: 'idle',
              evidence: {},
              since: new Date().toISOString()
            };
          }
          hub.flush();
        }, 120);
      }, 120);
    } else if (api.decisionAutoProgress === 'transmitting') {
      // Пауза держит «сохранено хабом» видимым: за 120 мс нагруженная страница
      // применяет оба события в одном кадре и промежуточный этап не отрисовывается.
      setTimeout(() => {
        hub.append(`conversation:${convId}`, {
          kind: 'request_transmitting',
          conversation_id: convId,
          payload: { request_id: reqId }
        });
        hub.flush();
      }, 1500);
    }

    await route.fulfill({
      status: 200,
      headers: headers(),
      json: {
        request_id: reqId,
        winner_decision_id: winnerId,
        state: 'decision_saved',
        decision: body
      }
    });
  });
}

function errorBody(code: string, message: string, details?: Record<string, unknown>): unknown {
  return {
    error: {
      code,
      message,
      scope: 'delivery',
      retry: 'explicit_new_operation',
      correlation_id: '00000000-0000-4000-8000-0000000000e8',
      ...(details ? { details } : {})
    }
  };
}

function builtinPreset(api: FakeHubApi, id: string) {
  return api.presets.get(id);
}

function validatePresetBody(
  body: { title?: string; description?: string | null; text?: string },
  api: FakeHubApi,
  selfId: string | null
): { status: number; code: string; message: string; details?: Record<string, unknown> } | null {
  const title = (body.title ?? '').trim();
  if (!title || title.length > 80 || /[\u0000-\u001f\u007f]/.test(title)) {
    return { status: 422, code: 'preset_invalid', message: 'Некорректный пресет', details: { field: 'title' } };
  }
  if ((body.description ?? '').length > 500) {
    return { status: 422, code: 'preset_invalid', message: 'Некорректный пресет', details: { field: 'description' } };
  }
  const text = (body.text ?? '').trim();
  if (!text) {
    return { status: 422, code: 'preset_invalid', message: 'Некорректный пресет', details: { field: 'text' } };
  }
  if (new TextEncoder().encode(text).length > 65_536) {
    return { status: 422, code: 'preset_invalid', message: 'Некорректный пресет', details: { field: 'text' } };
  }
  const clash = [...api.presets.values()].some(
    (preset) => preset.id !== selfId && preset.title.toLowerCase() === title.toLowerCase()
  );
  if (clash) {
    return { status: 409, code: 'preset_title_taken', message: 'Пресет с таким названием уже есть' };
  }
  if (selfId === null) {
    const customs = [...api.presets.values()].filter((preset) => preset.kind === 'custom').length;
    if (customs >= 100) {
      return { status: 409, code: 'preset_limit_reached', message: 'Слишком много пресетов' };
    }
  }
  return null;
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
