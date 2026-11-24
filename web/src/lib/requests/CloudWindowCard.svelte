<script lang="ts">
  import type { RequestItem } from '#lib/stores/requests.svelte.js';
  import type { DecisionBody } from '#lib/api/client.js';
  import { cloudCardLifetime, cloudOutcome, cloudWindowView } from './cloudWindowView';

  let {
    request,
    onDecision,
    disabled = false,
    now = Date.now()
  }: {
    request: RequestItem;
    onDecision: (d: DecisionBody) => Promise<void>;
    disabled?: boolean;
    now?: number;
  } = $props();

  const view = $derived(cloudWindowView(request.payload));
  const outcome = $derived(cloudOutcome(request.closedReason));
  const isDisabled = $derived(disabled || request.state !== 'open');
  const isTerminal = $derived(
    request.state === 'applied' || request.state === 'closed' ||
    request.state === 'not_applied' || request.state === 'unconfirmed'
  );
  const lifetime = $derived(isTerminal ? '' : cloudCardLifetime(view.expiresAt, now));

  function disableIf(node: HTMLButtonElement, val: boolean) {
    node.disabled = val;
    return {
      update(v: boolean) {
        node.disabled = v;
      }
    };
  }

  async function handleAllow() {
    if (isDisabled) return;
    await onDecision({ kind: 'permission', behavior: 'allow' });
  }

  async function handleDeny() {
    if (isDisabled) return;
    await onDecision({ kind: 'permission', behavior: 'deny' });
  }
</script>

<div
  class="cloud-card"
  data-cloud-window
  data-over-budget={view.overBudget ? '' : undefined}
  data-disabled={isDisabled ? '' : undefined}
>
  <h3 class="title">{view.title}</h3>
  <p class="source">{view.sourceText}</p>

  {#if view.reason}
    <p class="meta">Причина</p>
    <p class="reason">{view.reason}</p>
  {/if}

  <dl class="facts">
    <dt>Часы</dt><dd>{view.hoursText}</dd>
    <dt>Цена часа</dt><dd>{view.hourlyText}</dd>
    <dt>Резерв</dt><dd>{view.reservationText}</dd>
    <dt>Остаток бюджета</dt><dd>{view.remainingText}</dd>
  </dl>

  {#if view.prepare}
    <div class="prepare" data-cloud-prepare>
      <p class="meta">Подготовка машины</p>
      {#if view.prepare.packages.length}
        <p class="note" data-cloud-prepare-packages>Пакеты: {view.prepare.packages.join(", ")}</p>
      {/if}
      {#if view.prepare.script}
        <pre class="script" data-cloud-prepare-script>{view.prepare.script}</pre>
      {/if}
    </div>
  {/if}

  {#if view.overBudget}
    <p class="over" data-over-budget-mark>Сверх бюджета</p>
    <p class="note">Резерв больше остатка месяца. Одобрение разрешит ровно эту сумму сверх бюджета.</p>
  {/if}

  {#if lifetime}<p class="note" data-cloud-card-lifetime>{lifetime}</p>{/if}

  {#if isTerminal}
    <p class="outcome" data-outcome={outcome.word} data-tone={outcome.tone}>{outcome.word}</p>
    {#if outcome.note}<p class="note">{outcome.note}</p>{/if}
  {/if}

  <div class="actions">
    <button type="button" class="btn btn-allow" data-action="allow" use:disableIf={isDisabled} onclick={handleAllow}>
      Запустить
    </button>
    <button type="button" class="btn btn-deny" data-action="deny" use:disableIf={isDisabled} onclick={handleDeny}>
      Отклонить
    </button>
  </div>
</div>

<style>
  .cloud-card {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    min-width: 0;
    color: var(--text);
  }

  .title {
    margin: var(--s0);
    font-size: var(--fs-title);
    font-weight: var(--weight-semibold);
    line-height: var(--lh-tight);
  }

  .source {
    margin: var(--s0);
    color: var(--muted);
    font-size: var(--fs-meta);
  }

  .meta {
    margin: var(--s0);
    font-size: var(--fs-meta);
    color: var(--muted);
  }

  .reason {
    margin: var(--s0);
    font-size: var(--fs-body);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .facts {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: var(--s2) var(--s8);
    margin: var(--s2) var(--s0) var(--s0);
    padding: var(--s4) var(--s2);
    border-radius: var(--radius-sm);
    background: var(--code);
  }

  dt {
    color: var(--muted);
    font-size: var(--fs-meta);
  }

  dd {
    margin: var(--s0);
    font-variant-numeric: tabular-nums;
  }

  .over {
    margin: var(--s0);
    color: var(--warning);
    font-weight: var(--weight-medium);
  }

  .note {
    margin: var(--s0);
    color: var(--muted);
    font-size: var(--fs-meta);
  }

  .prepare {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    min-width: 0;
  }

  .script {
    margin: var(--s0);
    padding: var(--s3);
    max-height: calc(var(--fs-sm) * var(--lh-code) * 12);
    overflow: auto;
    border-radius: var(--radius-sm);
    background: var(--code);
    font-size: var(--fs-sm);
    line-height: var(--lh-code);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .outcome {
    margin: var(--s0);
    font-size: var(--fs-body);
    font-weight: var(--weight-medium);
    overflow-wrap: anywhere;
  }

  .outcome[data-tone='done'] {
    color: var(--success);
  }

  .outcome[data-tone='warning'] {
    color: var(--warning);
  }

  .outcome[data-tone='danger'] {
    color: var(--danger);
  }

  .outcome[data-tone='quiet'] {
    color: var(--muted);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s3);
    margin-top: var(--s2);
  }

  .btn {
    white-space: nowrap;
    flex: 1 1 auto;
    min-width: 0;
    max-width: 100%;
    min-height: var(--target);
    padding: var(--s2) var(--s6);
    border-radius: var(--radius-pill);
    font-size: var(--fs-body);
    font-weight: var(--weight-medium);
    cursor: pointer;
    transition: background var(--duration-fast) var(--ease);
  }

  .btn:focus-visible {
    outline: var(--focus-width) solid var(--accent);
    outline-offset: var(--s1);
  }

  .btn:disabled {
    cursor: not-allowed;
  }

  .btn-allow {
    background: var(--accent);
    color: var(--accent-ink);
  }

  .btn-deny {
    background: var(--raised);
    color: var(--danger);
  }

  .btn-allow:disabled {
    background: var(--accent-soft);
    color: var(--muted);
  }

  .btn-deny:disabled {
    background: var(--raised);
    color: var(--muted);
  }

  .btn-deny:not(:disabled):hover {
    background: var(--danger-soft);
  }
</style>
