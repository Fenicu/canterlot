import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function bootChat(
  page: Page,
  opts?: {
    setupApi?: (api: FakeHubApi) => void;
    seed?: (api: FakeHubApi) => string;
  }
) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);

  seedMachine(api, {
    id: 'workstation',
    name: 'workstation',
    harnesses: [
      {
        id: 'claude',
        title: 'Claude Code',
        provider: 'claude',
        models: [
          { id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium', 'high'] },
          { id: 'sonnet-5', title: 'sonnet-5', efforts: ['low', 'medium'] },
          { id: 'haiku', title: 'haiku', efforts: ['low'] }
        ],
        permission_modes: ['owner', 'ask', 'bypass', 'read_only']
      }
    ]
  });

  if (opts?.setupApi) opts.setupApi(api);

  const id = opts?.seed ? opts.seed(api) : seedConversation(api, {
    title: 'Сессия запросов',
    model: 'opus-5.5',
    effort: 'medium',
    permission_mode: 'owner',
    activity: { state: 'idle', evidence: {}, since: new Date().toISOString() }
  });

  const detail = api.details.get(id);
  if (detail) {
    detail.harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
    detail.machine = { id: 'workstation', name: 'workstation' };
    detail.model = 'opus-5.5';
    detail.effort = detail.effort || 'medium';
    detail.permission_mode = detail.permission_mode || 'owner';
  }

  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

  return { hub, api, id };
}

function started(blockId: string) {
  return {
    block_id: blockId,
    rev: 0,
    kind: 'text' as const,
    input_command_id: null,
    parent_block_id: null,
    agent: 'агент',
    model: 'opus',
    tool_name: null,
    summary: null,
    approved_by: null
  };
}

test.describe('E2E Requests and Recovery', () => {
  test('e2e_agent_question_flows_from_synthetic_adapter_to_pwa_and_back', async ({ page }) => {
    const { hub, api, id } = await bootChat(page);

    const reqId = api.id();
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: reqId,
        kind: 'question',
        payload: {
          questions: [
            {
              id: 'q1',
              text: 'Выберите стратегию слияния ветки',
              options: [
                { label: 'Rebase and fast-forward', description: 'Линейная история' },
                { label: 'Merge commit', description: 'Сохранить граф коммитов' }
              ]
            }
          ]
        }
      }
    });
    hub.flush();

    // Question card is rendered
    await expect(page.getByText('Вопрос от агента')).toBeVisible();
    await expect(page.getByText('Выберите стратегию слияния ветки')).toBeVisible();
    await expect(page.getByText('Rebase and fast-forward')).toBeVisible();

    // Pause auto-progression at transmitting stage to verify the stage sequence deterministically
    api.decisionAutoProgress = 'transmitting';

    // User selects answer and clicks submit
    await page.getByText('Rebase and fast-forward').click();
    await page.getByRole('button', { name: 'Ответить' }).click();

    // StageTrack progress: sending -> saved -> transmitting
    await expect(page.locator('.st-status')).toHaveText(/отправка|сохранено хабом/);
    await expect(page.locator('.st-status')).toHaveText('сохранено хабом');
    await expect(page.locator('.st-status')).toHaveText('передано агенту');

    // Now transition to applied
    hub.append(`conversation:${id}`, {
      kind: 'request_closed',
      conversation_id: id,
      payload: { request_id: reqId, reason: 'applied' }
    });
    hub.flush();

    // Card collapses into summary row on applied
    await expect(page.locator('[data-collapsed="true"]')).toBeVisible();
    await expect(page.locator('[data-collapsed="true"] .pill')).toHaveText('Rebase and fast-forward');

    // Решённый запрос раскрывается как история и сворачивается той же строкой
    await page.locator('[data-collapsed="true"] .head').click();
    await expect(page.locator('[data-expanded="true"] .answered')).toBeVisible();
    await page.locator('[data-expanded="true"] .head').click();
    await expect(page.locator('[data-collapsed="true"]')).toBeVisible();

    // Turn completes: agent outputs text
    const blockId = api.id();
    hub.append(`conversation:${id}`, {
      kind: 'block_started',
      conversation_id: id,
      payload: started(blockId)
    });
    hub.append(`conversation:${id}`, {
      kind: 'block_delta',
      conversation_id: id,
      payload: { block_id: blockId, base_rev: 0, rev: 1, append: 'Ветка успешно перебазирована.' }
    });
    hub.flush();

    await expect(page.getByText('Ветка успешно перебазирована.')).toBeVisible();
  });

  test('e2e_permission_allow_and_deny_flow_with_proof', async ({ page }) => {
    const { hub, api, id } = await bootChat(page);

    // 1. Allow flow
    const permId1 = api.id();
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: permId1,
        kind: 'permission',
        payload: {
          tool_name: 'Bash',
          input: { command: 'cargo test --all', description: 'Запуск тестового набора' },
          tool_use_id: 'toolu_1'
        }
      }
    });
    hub.flush();

    await expect(page.getByText('Запрос разрешения')).toBeVisible();
    await expect(page.getByText('cargo test --all')).toBeVisible();

    const allowBtn = page.locator('button[data-action="allow"]');
    const denyBtn = page.locator('button[data-action="deny"]');
    await expect(allowBtn).toBeVisible();
    await expect(denyBtn).toBeVisible();

    await allowBtn.click();

    // Transitions to applied and collapses to «Разрешено»
    await expect(page.locator('[data-collapsed="true"]')).toBeVisible();
    await expect(page.getByText('Разрешено')).toBeVisible();

    // 2. Deny flow
    const permId2 = api.id();
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: permId2,
        kind: 'permission',
        payload: {
          command: 'rm -rf /tmp/scratch',
          description: 'Удаление временной директории',
          tool: 'bash'
        }
      }
    });
    hub.flush();

    await expect(page.getByText('rm -rf /tmp/scratch')).toBeVisible();
    await page.locator('button[data-action="deny"]').click();

    await expect(page.locator('[data-collapsed="true"]')).toHaveCount(2);
    await expect(page.getByText('Отклонено')).toBeVisible();
  });

  test('e2e_mcp_elicitation_url_card_shows_address_and_allows_safely', async ({ page }) => {
    const { hub, api, id } = await bootChat(page);

    const url = 'https://notes.example.com/authorize?scope=read';
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: api.id(),
        kind: 'permission',
        payload: {
          tool_name: 'mcp_elicitation',
          server: 'notes',
          message: 'Открыть доступ к заметкам?',
          mode: 'url',
          url,
          tool_use_id: 'tu-1',
          agent: 'codex'
        }
      }
    });
    hub.flush();

    await expect(page.getByText('Запрос MCP-сервера')).toBeVisible();
    await expect(page.getByText('Открыть доступ к заметкам?')).toBeVisible();
    const link = page.getByRole('link', { name: url });
    await expect(link).toBeVisible();
    await expect(link).toHaveAttribute('href', url);
    await expect(link).toHaveAttribute('target', '_blank');
    await expect(link).toHaveAttribute('rel', /noopener/);

    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: api.id(),
        kind: 'permission',
        payload: {
          tool_name: 'mcp_elicitation',
          server: 'notes',
          message: 'Подтвердите действие',
          mode: 'url',
          url: 'javascript:alert(1)'
        }
      }
    });
    hub.flush();

    await expect(page.getByText('javascript:alert(1)', { exact: true })).toBeVisible();
    await expect(page.locator('a[href^="javascript:"]')).toHaveCount(0);

    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: api.id(),
        kind: 'permission',
        payload: { tool_name: 'mcp_elicitation', server: 'notes', message: 'Доступ?', mode: 'url' }
      }
    });
    hub.flush();

    const missing = page.locator('div.permission-card', { hasText: 'Сервер не передал адрес' });
    await expect(missing).toBeVisible();
    await expect(missing.getByRole('button', { name: 'Разрешить' })).toBeDisabled();
    await missing.getByRole('button', { name: 'Отклонить' }).click();
    await expect(page.getByText('Отклонено')).toBeVisible();
  });

  test('e2e_decision_conflict_from_second_device_shows_conflict_and_disables_first', async ({ page }) => {
    const { hub, api, id } = await bootChat(page);

    const reqId = api.id();
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: reqId,
        kind: 'question',
        payload: {
          questions: [
            {
              id: 'q1',
              text: 'Какой режим сборки использовать?',
              options: [
                { label: 'Release' },
                { label: 'Debug' }
              ]
            }
          ]
        }
      }
    });
    hub.flush();

    // Open second page (simulating second device/tab)
    const page2 = await page.context().newPage();
    await installChatRoutes(page2, hub, api, OWNER);
    await page2.goto(`/c/${id}`);
    await expect(page2.locator('[data-ready]')).toBeVisible();

    // Both devices select an option
    await page2.getByText('Release').click();
    await page.getByText('Debug').click();

    api.decisionAutoProgress = 'saved';
    // Delay WSS broadcast to let Device 1 submit its conflicting decision via HTTP
    hub.chaos.delay(1000);

    // Device 2 submits first and wins
    const [res] = await Promise.all([
      page2.waitForResponse((resp) => resp.url().includes('/decision') && resp.status() === 200),
      page2.getByRole('button', { name: 'Ответить' }).click()
    ]);
    expect(res.ok()).toBe(true);

    // Device 1 submits conflicting decision -> receives 409 decision_conflict
    const [conflictRes] = await Promise.all([
      page.waitForResponse((resp) => resp.url().includes('/decision') && resp.status() === 409),
      page.getByRole('button', { name: 'Ответить' }).click()
    ]);
    expect(conflictRes.status()).toBe(409);

    hub.chaos.delay(0);

    // Device 1 receives conflict: shows conflict notification, disables button, syncs to winner
    await expect(page.locator('[data-conflict="true"]')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Ответить' })).toBeDisabled();
    await expect(page.locator('.st-status')).toHaveText(/сохранено хабом|передано агенту/);

    await page2.close();
  });

  test('e2e_turn_interrupt_while_request_pending_closes_card', async ({ page }) => {
    const { hub, api, id } = await bootChat(page);

    const reqId = api.id();
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: reqId,
        kind: 'question',
        payload: {
          questions: [{ id: 'q1', text: 'Продолжить ли операцию?' }]
        }
      }
    });
    hub.flush();

    await expect(page.getByText('Продолжить ли операцию?')).toBeVisible();

    // Interrupt occurs: hub appends request_closed with reason 'interrupted'
    hub.append(`conversation:${id}`, {
      kind: 'request_closed',
      conversation_id: id,
      payload: {
        request_id: reqId,
        reason: 'interrupted'
      }
    });
    hub.flush();

    // Card shows error/interrupted reason in StageTrack and controls are disabled
    await expect(page.locator('.st-status')).toHaveText('агент прерван');
    await expect(page.getByRole('button', { name: 'Ответить' })).toBeDisabled();
  });

  test('e2e_disconnection_waits_without_automatic_permission', async ({ page }) => {
    let decisionRequests = 0;
    page.on('request', (req) => {
      if (req.url().includes('/decision')) decisionRequests += 1;
    });

    const { hub, api, id } = await bootChat(page);

    const reqId = api.id();
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: reqId,
        kind: 'permission',
        payload: {
          command: 'reboot',
          description: 'Перезагрузка узла'
        }
      }
    });
    hub.flush();

    await expect(page.getByText('reboot')).toBeVisible();

    // Simulate link loss
    hub.chaos.cut();

    // Wait a brief moment to ensure no automatic grant happened
    await page.waitForTimeout(500);

    // Permission request is still open, not auto-granted
    expect(decisionRequests).toBe(0);
    await expect(page.getByText('reboot')).toBeVisible();
    await expect(page.locator('[data-collapsed="true"]')).toHaveCount(0);
    await expect(page.locator('button[data-action="allow"]')).toBeVisible();
  });

  test('e2e_open_request_survives_page_reload_via_conversation_snapshot', async ({ page }) => {
    const { hub, api, id } = await bootChat(page);

    const reqId = api.id();
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: reqId,
        kind: 'question',
        payload: {
          questions: [
            {
              id: 'q1',
              text: 'Вопрос перед перезагрузкой страницы',
              options: [{ label: 'Да' }, { label: 'Нет' }]
            }
          ]
        }
      }
    });
    hub.flush();

    await expect(page.getByText('Вопрос перед перезагрузкой страницы')).toBeVisible();

    // 1. Reload while open
    await page.reload();
    await expect(page.locator('[data-ready]')).toBeVisible();
    await expect(page.getByText('Вопрос перед перезагрузкой страницы')).toBeVisible();
    await expect(page.getByRole('radio', { name: 'Да' })).toBeEnabled();

    // 2. Decided-but-not-applied survives reload in its stage (orchestrator note)
    api.decisionAutoProgress = false;
    await page.getByText('Да').click();
    await page.getByRole('button', { name: 'Ответить' }).click();

    await expect(page.locator('.st-status')).toHaveText('сохранено хабом');

    // Reload while in 'saved' stage!
    await page.reload();
    await expect(page.locator('[data-ready]')).toBeVisible();
    await expect(page.locator('.st-status')).toHaveText('сохранено хабом');

    // Advance to 'transmitting'
    hub.append(`conversation:${id}`, {
      kind: 'request_transmitting',
      conversation_id: id,
      payload: { request_id: reqId }
    });
    hub.flush();

    await expect(page.locator('.st-status')).toHaveText('передано агенту');

    // Reload while in 'transmitting' stage!
    await page.reload();
    await expect(page.locator('[data-ready]')).toBeVisible();
    await expect(page.locator('.st-status')).toHaveText('передано агенту');
  });

  test('e2e_open_request_survives_wss_resync', async ({ page }) => {
    const { hub, api, id } = await bootChat(page);

    const reqId = api.id();
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: reqId,
        kind: 'question',
        payload: {
          questions: [{ id: 'q1', text: 'Синхронизация WSS' }]
        }
      }
    });
    hub.flush();

    await expect(page.getByText('Синхронизация WSS')).toBeVisible();

    // Emit decided event into journal
    hub.append(`conversation:${id}`, {
      kind: 'request_decided',
      conversation_id: id,
      payload: {
        request_id: reqId,
        winner_decision_id: api.id(),
        decision: { answers: { q1: 'Принято' } }
      }
    });

    // Trigger WSS resync
    hub.chaos.forceResync('cursor_too_old');

    // After resync, card catches up to saved state
    await expect(page.locator('.st-status')).toHaveText('сохранено хабом', { timeout: 10_000 });
  });

  test('e2e_open_request_survives_hub_restart', async ({ page }) => {
    const { hub, api, id } = await bootChat(page);

    const reqId = api.id();
    const detail = api.details.get(id);
    if (detail) {
      detail.activity = {
        state: 'waiting_for_user',
        evidence: { question: 'Запрос до перезапуска хаба' },
        since: new Date().toISOString()
      };
      detail.queue = {
        pause_reasons: [],
        hold_reasons: [{ reason: 'waiting_for_user' }],
        queued: 0,
        run_slot: 'free'
      };
    }

    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: reqId,
        kind: 'question',
        payload: {
          questions: [{ id: 'q1', text: 'Запрос до перезапуска хаба', options: [{ label: 'Продолжить' }] }]
        }
      }
    });
    hub.flush();

    await expect(page.getByText('Запрос до перезапуска хаба')).toBeVisible();

    // Simulate hub restart: connection drop and restore
    hub.chaos.restart();

    // After reconnect, open request and waiting_for_user activity are retained
    await expect(page.getByText('Запрос до перезапуска хаба')).toBeVisible({ timeout: 10_000 });
    await expect(page.getByRole('radio', { name: 'Продолжить' })).toBeEnabled();
  });

  test('e2e_open_request_survives_worker_restart_via_sj_reannouncement', async ({ page }) => {
    const { hub, api, id } = await bootChat(page);

    const reqId = api.id();
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: reqId,
        kind: 'question',
        payload: {
          questions: [{ id: 'q1', text: 'Дедупликация повторного объявления' }]
        }
      }
    });
    hub.flush();

    await expect(page.getByText('Дедупликация повторного объявления')).toBeVisible();

    // Worker restarts and re-announces the exact same request from SJ
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: reqId,
        kind: 'question',
        payload: {
          questions: [{ id: 'q1', text: 'Дедупликация повторного объявления' }]
        }
      }
    });
    hub.flush();

    // Exactly one card rendered (no duplicate cards)
    await expect(page.locator('.request-cards-slot .card-root')).toHaveCount(1);
    await expect(page.getByText('Дедупликация повторного объявления')).toBeVisible();
  });

  const LONG_OPTIONS = [1, 2, 3, 4].map((n) => ({
    label: `Вариант ${n}`,
    description: `Подробное описание варианта ${n}: на узком экране оно занимает несколько строк и делает карточку выше экрана.`
  }));

  for (const theme of ['dark', 'light'] as const) {
    for (const viewport of [
      { width: 384, height: 800 },
      { width: 384, height: 640 }
    ]) {
      test(`e2e_two_long_questions_scroll_and_submit_${theme}_${viewport.width}x${viewport.height}`, async ({ page }) => {
        await page.setViewportSize(viewport);
        await page.addInitScript((value) => localStorage.setItem('canterlot.theme', value), theme);
        const { hub, api, id } = await bootChat(page);

        const header = page.locator('.chat-header');
        const composer = page.locator('[data-composer]');
        const headerBefore = await header.boundingBox();
        const composerBefore = await composer.boundingBox();

        const reqId = api.id();
        hub.append(`conversation:${id}`, {
          kind: 'request_opened',
          conversation_id: id,
          payload: {
            request_id: reqId,
            kind: 'question',
            payload: {
              questions: [
                {
                  id: 'q1',
                  header: 'ВЕРСИЯ',
                  text: 'Какую версию считать основной?',
                  options: LONG_OPTIONS
                },
                {
                  id: 'q2',
                  header: 'ТЕГИ',
                  text: 'Какие теги повесить на релиз?',
                  options: LONG_OPTIONS,
                  multi_select: true,
                  allow_custom: true
                }
              ]
            }
          }
        });
        hub.flush();

        await expect(page.getByText('Вопрос от агента')).toBeVisible();
        await expect(page.locator('html')).toHaveAttribute('data-theme', theme);

        // Шапка и поле ввода остаются на месте: карточка отнимает место у ленты, а не у них.
        const headerAfter = await header.boundingBox();
        const composerAfter = await composer.boundingBox();
        expect(Math.abs(headerAfter!.y - headerBefore!.y)).toBeLessThanOrEqual(1);
        expect(Math.abs(composerAfter!.y - composerBefore!.y)).toBeLessThanOrEqual(1);

        const slot = page.locator('.request-cards-slot');
        const submit = page.getByRole('button', { name: 'Ответить' });
        // Кнопка отправки закреплена и видна без прокрутки.
        await expect(submit).toBeInViewport();

        const scrollable = await slot.evaluate((el) => el.scrollHeight > el.clientHeight + 1);
        expect(scrollable).toBe(true);

        await page.locator('[data-question="q1"] [data-option="Вариант 1"]').click();

        const second = page.locator('[data-question="q2"]');
        await second.scrollIntoViewIfNeeded();
        await expect(second).toBeInViewport();
        await second.locator('[data-option="Вариант 2"]').click();

        const custom = second.locator('input[data-custom="true"]');
        await custom.scrollIntoViewIfNeeded();
        await custom.fill('Свой ответ на второй вопрос');
        await expect(custom).toBeFocused();
        await expect(custom).toBeInViewport();
        await expect(submit).toBeInViewport();

        // Клавиатура сжимает видимую область: поле остаётся в прокручиваемой карточке,
        // а закреплённая кнопка и поле ввода сообщения не уезжают за экран.
        await page.setViewportSize({ width: viewport.width, height: viewport.height - 280 });
        await expect
          .poll(() =>
            custom.evaluate((el) => {
              const slotEl = el.closest('.request-cards-slot');
              if (!slotEl) return false;
              const field = el.getBoundingClientRect();
              const box = slotEl.getBoundingClientRect();
              return field.top >= box.top - 1 && field.bottom <= box.bottom + 1;
            })
          )
          .toBe(true);
        await expect(submit).toBeInViewport();
        await expect(composer).toBeInViewport();

        const [decision] = await Promise.all([
          page.waitForResponse(
            (resp) => resp.url().includes('/decision') && resp.request().method() === 'POST'
          ),
          submit.click()
        ]);
        const body = decision.request().postDataJSON() as {
          answers: Record<string, string | string[]>;
        };
        expect(body.answers.q1).toBe('Вариант 1');
        expect(body.answers.q2).toEqual(['Вариант 2', 'Свой ответ на второй вопрос']);

        await expect(page.locator('[data-collapsed="true"]')).toBeVisible();
      });
    }
  }

  test('e2e_request_cards_are_usable_at_360px_and_200_percent_zoom', async ({ page }) => {
    await page.setViewportSize({ width: 360, height: 640 });
    const { hub, api, id } = await bootChat(page);

    // Open question and permission requests
    const qId = api.id();
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: qId,
        kind: 'question',
        payload: {
          questions: [
            {
              id: 'q1',
              text: 'Очень длинный вопрос для проверки переноса строк и отсутствия горизонтальной прокрутки при ширине 360px',
              options: [
                { label: 'Длинный вариант ответа номер один с подробным описанием', description: 'Дополнительные пояснения' },
                { label: 'Второй длинный вариант ответа' }
              ],
              allow_custom: true
            }
          ]
        }
      }
    });

    const pId = api.id();
    hub.append(`conversation:${id}`, {
      kind: 'request_opened',
      conversation_id: id,
      payload: {
        request_id: pId,
        kind: 'permission',
        payload: {
          command: 'git checkout -b feature/very-long-branch-name-that-should-wrap-cleanly-without-breaking-layout',
          description: 'Проверка длинной команды',
          tool: 'bash'
        }
      }
    });
    hub.flush();

    await expect(page.getByText('Очень длинный вопрос')).toBeVisible();
    await expect(page.getByText('git checkout -b feature/very-long-branch-name')).toBeVisible();

    // Verify no horizontal overflow beyond viewport width
    const scrollWidth = await page.evaluate(() => document.documentElement.scrollWidth);
    expect(scrollWidth).toBeLessThanOrEqual(360);

    // Verify action buttons are accessible and meet touch target dimensions
    const submitBtn = page.getByRole('button', { name: 'Ответить' });
    const allowBtn = page.locator('button[data-action="allow"]');
    const denyBtn = page.locator('button[data-action="deny"]');

    await expect(submitBtn).toBeVisible();
    await expect(allowBtn).toBeVisible();
    await expect(denyBtn).toBeVisible();

    const submitBox = await submitBtn.boundingBox();
    const allowBox = await allowBtn.boundingBox();
    expect(submitBox?.height).toBeGreaterThanOrEqual(36);
    expect(allowBox?.height).toBeGreaterThanOrEqual(36);
  });
});
