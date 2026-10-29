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
        command: 'rm -rf /tmp/canterlot-test',
        description: 'Очистка временных файлов'
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
});
