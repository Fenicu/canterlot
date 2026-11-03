<script lang="ts">
  import StepHead from '#lib/timeline/StepHead.svelte';
  import StageTrack from '#lib/ui/StageTrack.svelte';
  import type { Stage, StageState } from '#lib/ui/stage-track.js';
  import PermissionCard from './PermissionCard.svelte';
  import QuestionCard from './QuestionCard.svelte';
  import SpawnApprovalCard from './SpawnApprovalCard.svelte';
  import { spawnApprovalView } from './spawnApprovalView';
  import type { RequestItem } from '#lib/stores/requests.svelte.js';
  import type { DecisionBody } from '#lib/api/client.js';

  let {
    request,
    onDecision,
    open,
    onToggle,
    harnessId = '',
    permissionMode = ''
  }: {
    request: RequestItem;
    onDecision: (d: DecisionBody) => Promise<void>;
    /** Лента хранит раскрытое сама: строка снимается с экрана и возвращается. */
    open?: boolean;
    onToggle?: () => void;
    harnessId?: string;
    permissionMode?: string;
  } = $props();

  const stages: Stage[] = [
    { id: 'sending', label: 'отправка' },
    { id: 'saved', label: 'сохранено хабом' },
    { id: 'transmitting', label: 'передано агенту' },
    { id: 'applied', label: 'применено' }
  ];

  let expanded = $state(false);
  const shown = $derived(open ?? expanded);

  function toggle() {
    if (onToggle) onToggle();
    else expanded = !expanded;
  }

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
      case 'execution_ended':
        return 'Исполнение завершилось';
      case 'owner_lost':
        return 'Потерян владелец исполнения';
      case 'generation_advanced':
        return 'Исполнение сменилось';
      default:
        return 'Запрос закрыт';
    }
  }

  const trackIndex = $derived.by(() => {
    switch (request.state) {
      case 'sending':
        return 0;
      case 'saved':
        return 1;
      case 'transmitting':
      case 'unconfirmed':
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
    if (trackState !== 'error') return undefined;
    const reason = translateReason(request.closedReason);
    if (request.state !== 'not_applied') return reason;
    return reason === 'Запрос закрыт' || reason === 'не применено' ? 'не применено' : `не применено: ${reason}`;
  });

  const trackStatus = $derived.by(() => {
    if (request.state === 'applied') return 'применено';
    if (request.state === 'unconfirmed') return 'Передано агенту, подтверждения нет';
    return undefined;
  });

  const showTrack = $derived(request.state !== 'open');

  const subject = $derived.by(() => {
    const payload = (request.payload ?? {}) as Record<string, unknown>;
    if (request.kind === 'permission') return String(payload.tool_name ?? '');
    if (request.kind === 'spawn_approval') return spawnApprovalView(payload).facts.slice(0, 2).map((fact) => fact.value).join(' · ');
    const nested = payload.payload as Record<string, unknown> | undefined;
    const first = ((payload.questions ?? nested?.questions) as Array<Record<string, unknown>> | undefined)?.[0];
    return String(first?.text ?? first?.question ?? payload.text ?? payload.question ?? '');
  });

  const answerText = $derived.by(() => {
    const winner = request.winnerDecision as Record<string, unknown> | undefined;
    if (request.kind === 'permission' || request.kind === 'spawn_approval') {
      if (winner?.behavior === 'allow') return 'Разрешено';
      if (winner?.behavior === 'deny') return 'Отклонено';
      return 'Решение применено';
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
      return 'Ответ скрыт';
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
        return parts.join('; ');
      }
    }

    const single = winner?.answer;
    if (single !== undefined && single !== null && String(single).trim()) {
      return String(single);
    }

    return 'Ответ дан';
  });
</script>

{#if request.state === 'applied'}
  <div class="applied" data-collapsed={shown ? undefined : 'true'} data-expanded={shown ? 'true' : undefined}>
    <StepHead name={request.kind === 'permission' ? 'Разрешение' : request.kind === 'spawn_approval' ? 'Запуск сессии' : 'Вопрос'} detail={subject} status={answerText} state="answer" open={shown} ontoggle={toggle} />
    {#if shown}
      <div class="applied-body">
        {#if request.kind === 'permission'}
          <PermissionCard {request} {onDecision} {harnessId} {permissionMode} disabled={true} />
        {:else if request.kind === 'spawn_approval'}
          <SpawnApprovalCard {request} {onDecision} disabled={true} />
        {:else}
          <QuestionCard {request} {onDecision} disabled={true} readonly />
        {/if}
      </div>
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
    {:else if request.error}
      <div class="error-notice" role="alert" data-decision-error>
        {request.error}
      </div>
    {/if}

    {#if request.kind === 'permission'}
      <PermissionCard {request} {onDecision} {harnessId} {permissionMode} disabled={request.state !== 'open'} />
    {:else if request.kind === 'spawn_approval'}
      <SpawnApprovalCard {request} {onDecision} disabled={request.state !== 'open'} />
    {:else}
      <QuestionCard {request} {onDecision} disabled={request.state !== 'open'} />
    {/if}
  </div>
{/if}

<style>
  .card-root {
    --card-pad-block: var(--s5);
    flex: 0 1 auto;
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
    scroll-padding-block: var(--s4) calc(var(--target) + var(--s8));
    padding: var(--card-pad-block) var(--s6);
    border-radius: var(--radius-card);
    background: var(--surface);
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
    background: var(--warning-soft);
    color: var(--text);
    font-size: var(--fs-meta);
  }

  .error-notice {
    padding: var(--s2) var(--s3);
    border-radius: var(--radius-sm);
    background: var(--danger-soft);
    color: var(--text);
    font-size: var(--fs-meta);
  }

  .applied { display: flex; flex-direction: column; min-width: 0; border-radius: var(--radius-card); background: var(--raised); }
  .applied-body { padding: var(--s2) var(--s6) var(--s4); }

  .track-wrapper {
    padding-bottom: var(--s2);
  }
</style>
