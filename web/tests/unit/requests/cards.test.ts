import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import RequestCard from '#lib/requests/RequestCard.svelte';
import PermissionCard from '#lib/requests/PermissionCard.svelte';
import QuestionCard from '#lib/requests/QuestionCard.svelte';
import ToolBlock from '#lib/timeline/ToolBlock.svelte';
import { BlockModel } from '#lib/stores/blocks.js';
import { RequestsModel, createRequestItem, type RequestItem } from '#lib/stores/requests.svelte.js';
import type { HubEvent } from '#lib/sync/wire.js';

beforeEach(() => {
  vi.stubGlobal('matchMedia', (query: string) => ({
    media: query,
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    onchange: null,
    dispatchEvent: vi.fn(),
    addListener: vi.fn(),
    removeListener: vi.fn()
  }));
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.body.textContent = '';
});

describe('PWA Request Cards', () => {
  describe('truncated_payload_note', () => {
    const NOTE = 'Показано начало: решение относится к полному тексту';
    const QUESTION = { questions: [{ id: 'q0', text: 'Что делать?', options: ['Да', 'Нет'] }] };
    const PERMISSION = { tool_name: 'Bash', command: 'cargo test' };

    function cardText(component: typeof PermissionCard | typeof QuestionCard, kind: 'permission' | 'question', payload: unknown): string {
      const target = document.createElement('div');
      document.body.appendChild(target);
      const request: RequestItem = { id: 'req', conversationId: 'conv-1', kind, state: 'open', payload };
      const instance = mount(component, { target, props: { request, onDecision: vi.fn() } });
      flushSync();
      const text = target.textContent ?? '';
      void unmount(instance);
      target.remove();
      return text;
    }

    it.each([
      ['permission', PermissionCard, PERMISSION],
      ['question', QuestionCard, QUESTION]
    ] as const)('shows_the_note_on_a_%s_card_only_for_literal_true', (kind, component, base) => {
      expect(cardText(component, kind, { ...base, truncated: true })).toContain(NOTE);
      expect(cardText(component, kind, base)).not.toContain(NOTE);
      for (const value of [false, 'true', 1, null, {}, []]) {
        expect(cardText(component, kind, { ...base, truncated: value })).not.toContain(NOTE);
      }
    });
  });

  function permissionText(payload: unknown): string {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const request: RequestItem = { id: 'req', conversationId: 'conv-1', kind: 'permission', state: 'open', payload };
    const instance = mount(PermissionCard, { target, props: { request, onDecision: vi.fn() } });
    flushSync();
    const text = target.textContent ?? '';
    void unmount(instance);
    target.remove();
    return text;
  }

  it('not_applied_card_says_the_answer_did_not_reach_the_agent', () => {
    const cases: Array<[string, string]> = [
      ['unsupported_command', 'не применено'],
      ['execution_ended', 'не применено: Исполнение завершилось']
    ];
    for (const [reason, label] of cases) {
      const target = document.createElement('div');
      document.body.appendChild(target);
      const request = createRequestItem({
        id: 'req-not-applied',
        conversationId: 'conv-1',
        kind: 'permission',
        state: 'not_applied',
        payload: {},
        closedReason: reason
      });
      const instance = mount(RequestCard, { target, props: { request, onDecision: vi.fn() } });
      flushSync();
      expect(target.textContent).toContain(label);
      expect(target.textContent).not.toContain('Запрос закрыт');
      void unmount(instance);
    }
  });

  it('secret_custom_answer_keeps_its_spaces', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request: RequestItem = {
      id: 'req-secret',
      conversationId: 'conv-1',
      kind: 'question',
      state: 'open',
      payload: { questions: [{ id: 'q1', text: 'Пароль', allow_custom: true, is_secret: true }] }
    };
    const instance = mount(QuestionCard, { target, props: { request, onDecision } });
    flushSync();
    const input = target.querySelector<HTMLInputElement>('input[data-custom]')!;

    input.value = '   ';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    expect(target.querySelector<HTMLButtonElement>('button[type=submit]')!.disabled).toBe(true);

    input.value = ' s3cret ';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    target.querySelector<HTMLButtonElement>('button[type=submit]')!.click();
    await vi.waitFor(() => expect(onDecision).toHaveBeenCalled());

    expect(JSON.stringify(onDecision.mock.calls[0]![0])).toContain('" s3cret "');
    void unmount(instance);
  });

  it('permission_card_shows_what_claude_code_writes_or_edits', () => {
    const write = permissionText({
      tool_name: 'Write',
      input: { file_path: '/srv/app/config.toml', content: 'строка 1\nстрока 2' },
      tool_use_id: 'toolu_1'
    });
    expect(write).toContain('/srv/app/config.toml');
    expect(write).toContain('строка 1\nстрока 2');

    const edit = permissionText({
      tool_name: 'Edit',
      input: { file_path: '/srv/app/main.rs', old_string: 'было', new_string: 'стало' },
      tool_use_id: 'toolu_2'
    });
    expect(edit).toContain('/srv/app/main.rs');
    expect(edit).toContain('было');
    expect(edit).toContain('стало');
    const grep = permissionText({ tool_name: 'Grep', input: { pattern: 'TODO', limit: 5 } });
    expect(grep).toContain('pattern');
    expect(grep).toContain('TODO');
  });

  it('permission_card_shows_a_codex_shell_request_and_warns_about_the_deny', () => {
    const text = permissionText({
      tool_name: 'shell',
      command: "/bin/bash -lc 'touch probe.txt'",
      cwd: '/tmp/canterlot-stage0/codex/permissions',
      reason: 'May I run `touch probe.txt` in the current directory as requested?',
      command_actions: [{ command: 'touch probe.txt', type: 'unknown' }],
      available_decisions: ['accept', 'cancel'],
      network: { host: 'example.org', protocol: 'https' },
      input: { command: "/bin/bash -lc 'touch probe.txt'", cwd: '/tmp/canterlot-stage0/codex/permissions' },
      tool_use_id: 'exec-1',
      agent: 'orchestrator',
      deny_also_interrupts: true
    });
    expect(text).toContain("/bin/bash -lc 'touch probe.txt'");
    expect(text).toContain('/tmp/canterlot-stage0/codex/permissions');
    expect(text).toContain('May I run');
    expect(text).toContain('example.org');
    expect(text).toContain('Отказ также прервёт ход');
    expect(text.split("/bin/bash -lc 'touch probe.txt'")).toHaveLength(2);
  });

  it('permission_card_lists_codex_file_changes_and_the_requested_root', () => {
    const text = permissionText({
      tool_name: 'apply_patch',
      changes: [{ diff: 'hi\n', kind: { type: 'add' }, path: '/tmp/run-1/note.txt' }],
      reason: null,
      grant_root: '/tmp/run-1',
      tool_use_id: 'exec-2',
      agent: 'orchestrator',
      deny_also_interrupts: false
    });
    expect(text).toContain('/tmp/run-1/note.txt');
    expect(text).toContain('Новый файл');
    expect(text).toContain('hi');
    expect(text).toContain('Доступ к папке');
    expect(text).not.toContain('Отказ также прервёт ход');
  });

  it('permission_card_shows_which_permissions_codex_requests', () => {
    const text = permissionText({
      tool_name: 'permissions',
      permissions: { sandbox: 'read-only' },
      reason: 'Need network?',
      cwd: '/tmp/work',
      tool_use_id: 'perm-1',
      agent: 'orchestrator',
      deny_also_interrupts: false
    });
    expect(text).toContain('"sandbox": "read-only"');
    expect(text).toContain('Need network?');
    expect(text).toContain('/tmp/work');
  });

  it('permission_card_renders_command_and_allow_deny_buttons', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request: RequestItem = {
      id: 'req-perm-1',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'open',
      payload: {
        tool_name: 'Bash',
        input: { command: 'rm -rf /tmp/canterlot-test', description: 'Очистка временных файлов' },
        tool_use_id: 'toolu_01GkVeqnQwNz9hnzGsJfE8yM'
      }
    };

    const instance = mount(PermissionCard, {
      target,
      props: {
        request,
        onDecision
      }
    });
    flushSync();

    expect(target.textContent).toContain('rm -rf /tmp/canterlot-test');
    expect(target.textContent).toContain('Очистка временных файлов');

    const allowBtn = target.querySelector<HTMLButtonElement>('button[data-action="allow"]');
    const denyBtn = target.querySelector<HTMLButtonElement>('button[data-action="deny"]');

    expect(allowBtn).toBeTruthy();
    expect(denyBtn).toBeTruthy();
    expect(allowBtn!.textContent).toContain('Разрешить');
    expect(denyBtn!.textContent).toContain('Отклонить');

    allowBtn!.click();
    flushSync();
    expect(onDecision).toHaveBeenCalledWith({ kind: 'permission', behavior: 'allow' });

    denyBtn!.click();
    flushSync();
    expect(onDecision).toHaveBeenCalledWith({ kind: 'permission', behavior: 'deny' });

    unmount(instance);
  });

  it('question_card_renders_raw_agent_options_with_russian_shell', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request: RequestItem = {
      id: 'req-q-1',
      conversationId: 'conv-1',
      kind: 'question',
      state: 'open',
      payload: {
        questions: [
          {
            id: 'q1',
            text: 'Which database engine should be used for integration testing?',
            header: 'Database choice',
            options: [
              { label: 'PostgreSQL 18', description: 'Standard containerized PostgreSQL 18.6' },
              { label: 'SQLite in-memory', description: 'Fast ephemeral testing' }
            ]
          }
        ]
      }
    };

    const instance = mount(QuestionCard, {
      target,
      props: {
        request,
        onDecision
      }
    });
    flushSync();

    // Raw agent options & descriptions must be rendered without translation
    expect(target.textContent).toContain('Which database engine should be used for integration testing?');
    expect(target.textContent).toContain('PostgreSQL 18');
    expect(target.textContent).toContain('Standard containerized PostgreSQL 18.6');
    expect(target.textContent).toContain('SQLite in-memory');
    expect(target.textContent).toContain('Fast ephemeral testing');

    // Russian shell controls
    expect(target.textContent).toContain('Выберите один вариант');
    const submitBtn = target.querySelector<HTMLButtonElement>('button[type="submit"]');
    expect(submitBtn).toBeTruthy();
    expect(submitBtn!.textContent).toContain('Ответить');

    unmount(instance);
  });

  it('question_card_supports_up_to_four_questions_with_single_and_multi_select', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request: RequestItem = {
      id: 'req-q-multi',
      conversationId: 'conv-1',
      kind: 'question',
      state: 'open',
      payload: {
        questions: [
          { id: 'q1', text: 'Select profile', options: ['fast', 'thorough'] },
          { id: 'q2', text: 'Features', multi_select: true, options: ['cache', 'metrics', 'tracing'] },
          { id: 'q3', text: 'Custom note', allow_custom: true, options: ['Default note'] },
          { id: 'q4', text: 'Confirm run', options: ['yes', 'no'] }
        ]
      }
    };

    const instance = mount(QuestionCard, {
      target,
      props: {
        request,
        onDecision
      }
    });
    flushSync();

    // Select q1 -> thorough
    const optQ1 = target.querySelector<HTMLElement>('[data-question="q1"] [data-option="thorough"]');
    optQ1?.click();
    flushSync();

    // Select q2 (multi) -> cache & tracing
    const optQ2Cache = target.querySelector<HTMLElement>('[data-question="q2"] [data-option="cache"]');
    const optQ2Tracing = target.querySelector<HTMLElement>('[data-question="q2"] [data-option="tracing"]');
    optQ2Cache?.click();
    optQ2Tracing?.click();
    flushSync();

    // Fill q3 custom input
    const customInput = target.querySelector<HTMLInputElement>('[data-question="q3"] input[data-custom="true"]');
    expect(customInput).toBeTruthy();
    customInput!.value = 'Custom benchmark setting';
    customInput!.dispatchEvent(new Event('input'));
    flushSync();

    // Select q4 -> yes
    const optQ4 = target.querySelector<HTMLElement>('[data-question="q4"] [data-option="yes"]');
    optQ4?.click();
    flushSync();

    const submitBtn = target.querySelector<HTMLButtonElement>('button[type="submit"]');
    submitBtn?.click();
    flushSync();

    expect(onDecision).toHaveBeenCalledWith({
      kind: 'question',
      answers: {
        q1: 'thorough',
        q2: ['cache', 'tracing'],
        q3: 'Custom benchmark setting',
        q4: 'yes'
      }
    });

    unmount(instance);
  });

  it('question_card_blocks_submit_until_answered_and_sends_custom_text_for_multi_select', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request: RequestItem = {
      id: 'req-gate',
      conversationId: 'conv-1',
      kind: 'question',
      state: 'open',
      payload: {
        questions: [
          { id: 'q1', text: 'Профиль', options: ['быстрый', 'полный'] },
          { id: 'q2', text: 'Что включить', multi_select: true, allow_custom: true, options: ['кэш', 'метрики'] }
        ]
      }
    };
    const instance = mount(QuestionCard, { target, props: { request, onDecision } });
    flushSync();
    const submit = target.querySelector<HTMLButtonElement>('button[type="submit"]')!;
    expect(submit.disabled).toBe(true);
    submit.click();
    flushSync();
    expect(onDecision).not.toHaveBeenCalled();

    target.querySelector<HTMLElement>('[data-question="q1"] [data-option="быстрый"]')!.click();
    flushSync();
    expect(submit.disabled).toBe(true);

    const custom = target.querySelector<HTMLInputElement>('[data-question="q2"] input[data-custom="true"]')!;
    custom.value = 'трассировка';
    custom.dispatchEvent(new Event('input'));
    flushSync();
    expect(submit.disabled).toBe(false);
    submit.click();
    flushSync();
    expect(onDecision).toHaveBeenCalledWith({
      kind: 'question',
      answers: { q1: 'быстрый', q2: ['трассировка'] }
    });
    unmount(instance);
  });

  it('refused_decision_shows_the_error_on_an_editable_card', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request = createRequestItem({
      id: 'req-refused',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'open',
      payload: { command: 'cargo test' },
      error: 'Ответ не подходит к вопросу'
    });
    const instance = mount(RequestCard, { target, props: { request, onDecision } });
    flushSync();
    expect(target.querySelector('[role="alert"]')?.textContent).toContain('Ответ не подходит к вопросу');
    const allow = target.querySelector<HTMLButtonElement>('[data-action="allow"]')!;
    expect(allow.disabled).toBe(false);
    allow.click();
    flushSync();
    expect(onDecision).toHaveBeenCalledWith({ kind: 'permission', behavior: 'allow' });
    unmount(instance);
  });

  it('applied_permission_without_a_known_decision_reads_neutral_not_allowed', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const request = createRequestItem({
      id: 'req-unknown',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'applied',
      payload: { command: 'rm -rf build' }
    });
    const instance = mount(RequestCard, { target, props: { request, onDecision: vi.fn() } });
    flushSync();
    expect(target.textContent).toContain('Решение применено');
    expect(target.textContent).not.toContain('Разрешено');
    unmount(instance);
  });

  it('custom_answer_fields_are_labelled_by_their_question', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const request: RequestItem = {
      id: 'req-labels',
      conversationId: 'conv-1',
      kind: 'question',
      state: 'open',
      payload: {
        questions: [
          { id: 'q1', text: 'Имя ветки', allow_custom: true },
          { id: 'q2', text: 'Токен доступа', allow_custom: true, is_secret: true }
        ]
      }
    };
    const instance = mount(QuestionCard, { target, props: { request, onDecision: vi.fn() } });
    flushSync();
    const inputs = [...target.querySelectorAll<HTMLInputElement>('input[data-custom="true"]')];
    expect(inputs).toHaveLength(2);
    const names = inputs.map((input) => {
      const ids = (input.getAttribute('aria-labelledby') ?? '').split(' ').filter(Boolean);
      return ids.map((id) => document.getElementById(id)?.textContent?.trim()).join(' ');
    });
    expect(names).toEqual(['Имя ветки', 'Токен доступа']);
    unmount(instance);
  });

  it('request_card_renders_compact_stage_track_without_check_glyphs', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request = createRequestItem({
      id: 'req-st-1',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'sending',
      payload: { command: 'systemctl restart' }
    });

    const instance = mount(RequestCard, {
      target,
      props: {
        request,
        onDecision
      }
    });
    flushSync();

    // StageTrack compact should be present
    const track = target.querySelector('.st--compact');
    expect(track).toBeTruthy();
    expect(target.textContent).toContain('отправка');

    // Strictly NO check glyphs anywhere in card
    expect(target.textContent).not.toContain('✓');
    expect(target.textContent).not.toContain('✔');
    expect(target.textContent).not.toContain('✗');

    // Advance to saved
    request.state = 'saved';
    flushSync();
    expect(target.textContent).toContain('сохранено хабом');
    expect(target.textContent).not.toContain('✓');

    // Advance to transmitting
    request.state = 'transmitting';
    flushSync();
    expect(target.textContent).toContain('передано агенту');

    // Terminal closed with interrupted
    request.state = 'closed';
    request.closedReason = 'interrupted';
    flushSync();
    expect(target.textContent).toContain('агент прерван');
    expect(target.textContent).not.toContain('✓');

    unmount(instance);
  });

  it('applied_decision_collapses_card_into_expandable_summary_line', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request: RequestItem = {
      id: 'req-app-1',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'applied',
      payload: { command: 'make check' },
      winnerDecision: { kind: 'permission', behavior: 'allow' }
    };

    const instance = mount(RequestCard, {
      target,
      props: {
        request,
        onDecision
      }
    });
    flushSync();

    // Collapsed summary line visible
    const summaryRow = target.querySelector<HTMLElement>('[data-collapsed="true"]');
    expect(summaryRow).toBeTruthy();
    expect(summaryRow!.textContent).toContain('Разрешено');

    // Clicking toggles expanded read-only view
    summaryRow!.click();
    flushSync();

    expect(target.querySelector('[data-expanded="true"]')).toBeTruthy();
    expect(target.textContent).toContain('make check');

    // Clicking again collapses
    const collapseBtn = target.querySelector<HTMLElement>('[data-toggle-collapse]');
    collapseBtn?.click();
    flushSync();
    expect(target.querySelector('[data-collapsed="true"]')).toBeTruthy();

    unmount(instance);
  });

  it('disabled_state_prevents_clicks_when_decision_already_saved', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request: RequestItem = {
      id: 'req-dis-1',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'saved',
      payload: { command: 'rm -f db.sqlite' },
      winnerDecision: { kind: 'permission', behavior: 'allow' }
    };

    const instance = mount(RequestCard, {
      target,
      props: {
        request,
        onDecision
      }
    });
    flushSync();

    const allowBtn = target.querySelector<HTMLButtonElement>('button[data-action="allow"]');
    const denyBtn = target.querySelector<HTMLButtonElement>('button[data-action="deny"]');

    if (allowBtn) {
      expect(allowBtn.disabled).toBe(true);
      allowBtn.click();
      flushSync();
      expect(onDecision).not.toHaveBeenCalled();
    }
    if (denyBtn) {
      expect(denyBtn.disabled).toBe(true);
    }

    unmount(instance);
  });

  it('tool_row_shows_cli_policy_auto_approved_mark_without_revoke_button', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const block = new BlockModel('b-tool-1', 'tool_call');
    block.toolName = 'ReadFile';
    block.summary = 'read /etc/os-release';
    (block as unknown as { approved_by: string }).approved_by = 'cli_policy';

    const instance = mount(ToolBlock, {
      target,
      props: {
        block,
        now: Date.now(),
        load: vi.fn()
      }
    });
    flushSync();

    expect(target.textContent).toContain('Разрешено правилами CLI');
    // Ensure no revoke button
    const buttons = [...target.querySelectorAll('button')];
    const hasRevoke = buttons.some((btn) => btn.textContent?.includes('Отозвать') || btn.textContent?.includes('Revoke'));
    expect(hasRevoke).toBe(false);

    unmount(instance);
  });

  it('question_card_meets_accessibility_and_contrast_requirements', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request: RequestItem = {
      id: 'req-a11y-1',
      conversationId: 'conv-1',
      kind: 'question',
      state: 'open',
      payload: {
        questions: [
          {
            id: 'q-a11y',
            text: 'Choose environment',
            options: ['Staging', 'Production']
          }
        ]
      }
    };

    const instance = mount(QuestionCard, {
      target,
      props: {
        request,
        onDecision
      }
    });
    flushSync();

    const group = target.querySelector('[role="radiogroup"], [role="group"]');
    expect(group).toBeTruthy();
    expect(group!.getAttribute('aria-label')).toBeTruthy();

    const options = target.querySelectorAll('[role="radio"], input[type="radio"], button[data-option]');
    expect(options.length).toBe(2);

    unmount(instance);
  });

  it('secret_question_input_is_masked_and_summary_hides_answer', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request: RequestItem = {
      id: 'req-secret-1',
      conversationId: 'conv-1',
      kind: 'question',
      state: 'open',
      payload: {
        questions: [
          {
            id: 'q_token',
            text: 'Enter authentication token',
            allow_custom: true,
            is_secret: true
          }
        ]
      }
    };

    const instance = mount(QuestionCard, {
      target,
      props: {
        request,
        onDecision
      }
    });
    flushSync();

    // Input must be masked (type="password")
    const secretInput = target.querySelector<HTMLInputElement>('input[type="password"]');
    expect(secretInput).toBeTruthy();
    secretInput!.value = 'super-secret-token-123';
    secretInput!.dispatchEvent(new Event('input'));
    flushSync();

    const submitBtn = target.querySelector<HTMLButtonElement>('button[type="submit"]');
    submitBtn?.click();
    flushSync();

    expect(onDecision).toHaveBeenCalledWith({
      kind: 'question',
      answers: { q_token: 'super-secret-token-123' }
    });
    unmount(instance);

    // Now test collapsed summary with secret question applied
    const appliedSecret: RequestItem = {
      id: 'req-secret-applied',
      conversationId: 'conv-1',
      kind: 'question',
      state: 'applied',
      payload: {
        questions: [
          {
            id: 'q_token',
            text: 'Enter authentication token',
            is_secret: true
          }
        ]
      },
      winnerDecision: {
        kind: 'question',
        answers: { q_token: 'super-secret-token-123' }
      }
    };

    const cardInstance = mount(RequestCard, {
      target,
      props: {
        request: appliedSecret,
        onDecision: vi.fn()
      }
    });
    flushSync();

    // Summary must hide the secret and display "Ответ скрыт"
    expect(target.textContent).toContain('Ответ скрыт');
    expect(target.textContent).not.toContain('super-secret-token-123');

    unmount(cardInstance);
  });

  it('request_transmitting_event_moves_card_to_transmitted_stage', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request = createRequestItem({
      id: 'req-tx-1',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'saved',
      payload: { command: 'deploy' },
      winnerDecision: { kind: 'permission', behavior: 'allow' }
    });

    const instance = mount(RequestCard, {
      target,
      props: {
        request,
        onDecision
      }
    });
    flushSync();

    expect(target.textContent).toContain('сохранено хабом');

    // Simulate model receiving request_transmitting event
    const model = new RequestsModel('conv-1');
    model.items.set(request.id, request);
    model.applyTransmitting({
      kind: 'request_transmitting' as unknown as 'request_opened',
      payload: { request_id: 'req-tx-1' }
    } as unknown as HubEvent);

    flushSync();
    expect(request.state).toBe('transmitting');
    expect(target.textContent).toContain('передано агенту');

    // Should never move backwards if applied or closed
    request.state = 'applied';
    model.applyTransmitting({
      kind: 'request_transmitting' as unknown as 'request_opened',
      payload: { request_id: 'req-tx-1' }
    } as unknown as HubEvent);
    expect(request.state).toBe('applied');

    request.state = 'closed';
    model.applyTransmitting({
      kind: 'request_transmitting' as unknown as 'request_opened',
      payload: { request_id: 'req-tx-1' }
    } as unknown as HubEvent);
    expect(request.state).toBe('closed');

    unmount(instance);
  });

  it('permission_card_shows_mcp_elicitation_message_server_and_safe_url', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request: RequestItem = {
      id: 'req-elicit-1',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'open',
      payload: {
        tool_name: 'mcp_elicitation',
        server: 'notes',
        message: 'Открыть доступ к заметкам?',
        mode: 'url',
        url: 'https://notes.example.com/authorize?scope=read',
        schema: { type: 'object' },
        tool_use_id: 'tu-1',
        agent: 'codex'
      }
    };

    const instance = mount(PermissionCard, { target, props: { request, onDecision } });
    flushSync();

    expect(target.textContent).toContain('Запрос MCP-сервера');
    expect(target.textContent).toContain('Открыть доступ к заметкам?');
    expect(target.textContent).toContain('notes');
    expect(target.textContent).toContain('https://notes.example.com/authorize?scope=read');

    const link = target.querySelector<HTMLAnchorElement>('a[href="https://notes.example.com/authorize?scope=read"]');
    expect(link).toBeTruthy();
    expect(link!.target).toBe('_blank');
    expect(link!.rel).toContain('noopener');

    const allowBtn = target.querySelector<HTMLButtonElement>('button[data-action="allow"]');
    expect(allowBtn!.disabled).toBe(false);

    unmount(instance);
  });

  it('permission_card_never_links_unsafe_schemes_and_ignores_non_strings', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request: RequestItem = {
      id: 'req-elicit-2',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'open',
      payload: {
        tool_name: 'mcp_elicitation',
        server: 'notes',
        message: 'Подтвердите действие',
        mode: 'url',
        url: 'javascript:alert(document.cookie)'
      }
    };

    const instance = mount(PermissionCard, { target, props: { request, onDecision } });
    flushSync();

    expect(target.querySelector('a')).toBeNull();
    expect(target.textContent).toContain('javascript:alert(document.cookie)');

    unmount(instance);
    document.body.textContent = '';

    const target2 = document.createElement('div');
    document.body.appendChild(target2);
    const request2: RequestItem = {
      id: 'req-elicit-3',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'open',
      payload: {
        tool_name: 'mcp_elicitation',
        server: 42,
        message: { text: 'не строка' },
        mode: 'url',
        url: { href: 'https://notes.example.com/' }
      }
    };
    const instance2 = mount(PermissionCard, { target: target2, props: { request: request2, onDecision } });
    flushSync();

    expect(target2.querySelector('a')).toBeNull();
    expect(target2.textContent).not.toContain('https://notes.example.com/');
    expect(target2.textContent).not.toContain('не строка');

    unmount(instance2);
  });

  it('permission_card_disables_allow_when_url_mode_has_no_address', async () => {
    for (const url of [undefined, '']) {
      document.body.textContent = '';
      const target = document.createElement('div');
      document.body.appendChild(target);

      const onDecision = vi.fn().mockResolvedValue(undefined);
      const request: RequestItem = {
        id: 'req-elicit-4',
        conversationId: 'conv-1',
        kind: 'permission',
        state: 'open',
        payload: { tool_name: 'mcp_elicitation', server: 'notes', message: 'Доступ?', mode: 'url', ...(url === undefined ? {} : { url }) }
      };

      const instance = mount(PermissionCard, { target, props: { request, onDecision } });
      flushSync();

      const allowBtn = target.querySelector<HTMLButtonElement>('button[data-action="allow"]');
      const denyBtn = target.querySelector<HTMLButtonElement>('button[data-action="deny"]');
      expect(allowBtn!.disabled).toBe(true);
      expect(target.textContent).toContain('Сервер не передал адрес');

      allowBtn!.click();
      flushSync();
      expect(onDecision).not.toHaveBeenCalled();

      expect(denyBtn!.disabled).toBe(false);
      denyBtn!.click();
      flushSync();
      expect(onDecision).toHaveBeenCalledWith({ kind: 'permission', behavior: 'deny' });

      unmount(instance);
    }
  });

  it('request_card_shows_unconfirmed_as_its_own_state', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const onDecision = vi.fn().mockResolvedValue(undefined);
    const request = createRequestItem({
      id: 'req-unconfirmed-1',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'unconfirmed',
      payload: { tool_name: 'mcp_elicitation' }
    });

    const instance = mount(RequestCard, { target, props: { request, onDecision } });
    flushSync();

    expect(target.textContent).toContain('Передано агенту, подтверждения нет');
    expect(target.querySelector('[data-collapsed="true"]')).toBeNull();
    const allowBtn = target.querySelector<HTMLButtonElement>('button[data-action="allow"]');
    expect(allowBtn!.disabled).toBe(true);

    unmount(instance);
  });

  it('request_card_translates_closed_reasons_to_human_labels', async () => {
    const cases: Array<[string | undefined, string, string[]]> = [
      ['execution_ended', 'Исполнение завершилось', []],
      ['owner_lost', 'Потерян владелец исполнения', []],
      ['generation_advanced', 'Исполнение сменилось', []],
      ['frobnicated', 'Запрос закрыт', ['frobnicated']]
    ];
    for (const [reason, label, absent] of cases) {
      document.body.textContent = '';
      const target = document.createElement('div');
      document.body.appendChild(target);

      const onDecision = vi.fn().mockResolvedValue(undefined);
      const request = createRequestItem({
        id: 'req-closed-1',
        conversationId: 'conv-1',
        kind: 'permission',
        state: 'closed',
        payload: {},
        closedReason: reason
      });

      const instance = mount(RequestCard, { target, props: { request, onDecision } });
      flushSync();

      expect(target.textContent).toContain(label);
      for (const raw of absent) {
        expect(target.textContent).not.toContain(raw);
      }

      unmount(instance);
    }
  });
});
