<script lang="ts">
  import StageTrack from '#lib/ui/StageTrack.svelte';
  import type { Stage, StageState } from '#lib/ui/stage-track.js';
  import PermissionCard from './PermissionCard.svelte';
  import QuestionCard from './QuestionCard.svelte';
  import type { RequestItem } from '#lib/stores/requests.svelte.js';
  import type { DecisionBody } from '#lib/api/client.js';

  let {
    request,
    onDecision
  }: {
    request: RequestItem;
    onDecision: (d: DecisionBody) => Promise<void>;
  } = $props();

  const stages: Stage[] = [
    { id: 'sending', label: 'отправка' },
    { id: 'saved', label: 'сохранено хабом' },
    { id: 'transmitting', label: 'передано агенту' },
    { id: 'applied', label: 'применено' }
  ];

  let expanded = $state(false);

  function translateReason(reason?: string): string {
    if (!reason) return 'не применено';
    switch (reason) {
      case 'interrupted':
        return 'агент прерван';
      case 'rejected_by_policy':
      case 'policy_rejected':
      case 'cli_policy':
        return 'отклонено политикой';
      case 'not_applied':
        return 'не применено';
      case 'cancelled':
      case 'canceled':
        return 'отменено';
      default:
        return reason;
    }
  }

  const trackIndex = $derived.by(() => {
    switch (request.state) {
      case 'sending':
        return 0;
      case 'saved':
        return 1;
      case 'transmitting':
        return 2;
      case 'applied':
      case 'closed':
      case 'not_applied':
        return 3;
      default:
        return 0;
    }
  });

  const trackState = $derived.by((): StageState => {
    if (request.state === 'applied') return 'done';
    if (request.state === 'closed' || request.state === 'not_applied') return 'error';
    return 'active';
  });

  const trackReason = $derived.by(() => {
    if (trackState === 'error') {
      return translateReason(request.closedReason);
    }
    return undefined;
  });

  const trackStatus = $derived.by(() => {
    if (request.state === 'applied') return 'применено';
    return undefined;
  });

  const showTrack = $derived(request.state !== 'open');

  const summaryText = $derived.by(() => {
    const winner = request.winnerDecision as Record<string, unknown> | undefined;
    if (request.kind === 'permission') {
      if (winner?.behavior === 'allow') return 'Разрешено';
      if (winner?.behavior === 'deny') return 'Отклонено';
      return 'Разрешено';
    }

    const payload = (request.payload ?? {}) as Record<string, unknown>;
    const questionsList = Array.isArray(payload.questions)
      ? (payload.questions as Array<Record<string, unknown>>)
      : Array.isArray((payload.payload as Record<string, unknown>)?.questions)
        ? ((payload.payload as Record<string, unknown>).questions as Array<Record<string, unknown>>)
        : [];

    const hasSecret =
      Boolean(payload.is_secret) ||
      questionsList.some((q) => Boolean(q.is_secret || q.isSecret));

    if (hasSecret) {
      return 'Ответ: Ответ скрыт';
    }

    const answers = winner?.answers as Record<string, unknown> | undefined;
    if (answers && typeof answers === 'object') {
      const parts: string[] = [];
      for (const val of Object.values(answers)) {
        if (Array.isArray(val)) {
          parts.push(val.join(', '));
        } else if (val !== null && val !== undefined && String(val).trim()) {
          parts.push(String(val));
        }
      }
      if (parts.length > 0) {
        return `Ответ: ${parts.join('; ')}`;
      }
    }

    const single = winner?.answer;
    if (single !== undefined && single !== null && String(single).trim()) {
      return `Ответ: ${String(single)}`;
    }

    return 'Ответ дан';
  });
</script>

{#if request.state === 'applied' && !expanded}
  <div
    class="card-root applied-collapsed"
    data-collapsed="true"
    role="button"
    tabindex="0"
    onclick={() => { expanded = true; }}
    onkeydown={(e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        expanded = true;
      }
    }}
  >
    <div class="summary-line">
      <span class="summary-badge">{summaryText}</span>
      <span class="toggle-hint">Подробнее</span>
    </div>
  </div>
{:else if request.state === 'applied' && expanded}
  <div class="card-root applied-expanded" data-expanded="true">
    <div class="card-top-bar">
      <StageTrack
        {stages}
        index={trackIndex}
        state={trackState}
        form="compact"
        label="Доставка решения"
        status={trackStatus}
        reason={trackReason}
      />
      <button
        type="button"
        class="collapse-btn"
        data-toggle-collapse
        onclick={() => { expanded = false; }}
      >
        Свернуть
      </button>
    </div>

    {#if request.kind === 'permission'}
      <PermissionCard {request} {onDecision} disabled={true} />
    {:else}
      <QuestionCard {request} {onDecision} disabled={true} />
    {/if}
  </div>
{:else}
  <div class="card-root" data-state={request.state}>
    {#if showTrack}
      <div class="track-wrapper">
        <StageTrack
          {stages}
          index={trackIndex}
          state={trackState}
          form="compact"
          label="Доставка решения"
          status={trackStatus}
          reason={trackReason}
        />
      </div>
    {/if}

    {#if request.error === 'decision_conflict'}
      <div class="conflict-notice" role="alert" data-conflict="true">
        Решение принято на другом устройстве
      </div>
    {/if}

    {#if request.kind === 'permission'}
      <PermissionCard {request} {onDecision} disabled={request.state !== 'open'} />
    {:else}
      <QuestionCard {request} {onDecision} disabled={request.state !== 'open'} />
    {/if}
  </div>
{/if}

<style>
  .card-root {
    padding: var(--s4);
    border-radius: var(--radius-card);
    background: var(--surface);
    border: var(--hairline) solid var(--line);
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    color: var(--text);
    box-sizing: border-box;
    width: 100%;
  }

  .conflict-notice {
    padding: var(--s2) var(--s3);
    border-radius: var(--radius-sm);
    background: var(--surface-warning, var(--badge));
    color: var(--text);
    font-size: var(--font-small);
  }

  .applied-collapsed {
    padding: var(--s3) var(--s4);
    cursor: pointer;
    transition: background var(--duration-fast) var(--ease), border-color var(--duration-fast) var(--ease);
  }

  .applied-collapsed:hover {
    background: var(--raised);
    border-color: var(--control-line);
  }

  .applied-collapsed:focus-visible {
    outline: var(--focus-width) solid var(--accent);
    outline-offset: var(--s1);
  }

  .summary-line {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--s2);
  }

  .summary-badge {
    font-size: var(--fs-body);
    font-weight: var(--weight-medium);
    color: var(--text);
  }

  .toggle-hint {
    font-size: var(--fs-sm);
    color: var(--secondary);
  }

  .card-top-bar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--s3);
    padding-bottom: var(--s2);
    border-bottom: var(--hairline) solid var(--line);
  }

  .collapse-btn {
    min-height: var(--target);
    padding: var(--s1) var(--s3);
    border-radius: var(--radius-pill);
    background: var(--raised);
    color: var(--secondary);
    border: var(--hairline) solid var(--control-line);
    font-size: var(--fs-sm);
    cursor: pointer;
    transition: background var(--duration-fast) var(--ease), color var(--duration-fast) var(--ease);
  }

  .collapse-btn:focus-visible {
    outline: var(--focus-width) solid var(--accent);
    outline-offset: var(--s1);
  }

  .collapse-btn:hover {
    background: var(--hover);
    color: var(--text);
  }

  .track-wrapper {
    padding-bottom: var(--s2);
    border-bottom: var(--hairline) solid var(--line);
  }
</style>
