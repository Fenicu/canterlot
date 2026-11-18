<script lang="ts">
  import type { RequestItem } from '#lib/stores/requests.svelte.js';
  import type { DecisionBody } from '#lib/api/client.js';
  import { settingsChangeView, settingsOutcome } from './settingsChangeView';

  let {
    request,
    onDecision,
    disabled = false
  }: {
    request: RequestItem;
    onDecision: (d: DecisionBody) => Promise<void>;
    disabled?: boolean;
  } = $props();

  const view = $derived(settingsChangeView(request.payload));
  const outcome = $derived(settingsOutcome(request.closedReason));
  const isDisabled = $derived(disabled || request.state !== 'open');
  const isTerminal = $derived(
    request.state === 'applied' || request.state === 'closed' ||
    request.state === 'not_applied' || request.state === 'unconfirmed'
  );

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
  class="settings-card"
  data-settings-change
  data-setting={view.setting}
  data-disabled={isDisabled ? '' : undefined}
>
  <h3 class="title">Агент предлагает изменить настройку</h3>
  <p class="setting-name">{view.title}</p>

  {#if view.reason}
    <p class="meta">Причина</p>
    <p class="reason">{view.reason}</p>
  {/if}

  <p class="meta">Что изменится</p>
  {#if view.lines.length}
    <!-- Прокручиваемая область берёт фокус, иначе длинную разницу не прочесть с клавиатуры. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div class="diff" role="region" aria-label="Разница настроек" tabindex="0">
      {#each view.lines as line, index (index)}
        <div class="diff-line" data-mark={line.mark}>{line.text}</div>
      {/each}
    </div>
  {:else}
    <p class="empty">Изменений в значении не найдено</p>
  {/if}
  {#if view.unchanged > 0}
    <p class="unchanged">Без изменений: {view.unchanged}</p>
  {/if}

  {#if isTerminal}
    <p class="outcome" data-outcome={outcome.word} data-tone={outcome.tone}>{outcome.word}</p>
  {/if}

  <div class="actions">
    <button type="button" class="btn btn-allow" data-action="allow" use:disableIf={isDisabled} onclick={handleAllow}>
      Разрешить
    </button>
    <button type="button" class="btn btn-deny" data-action="deny" use:disableIf={isDisabled} onclick={handleDeny}>
      Отклонить
    </button>
  </div>
</div>

<style>
  .settings-card {
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

  .setting-name {
    margin: var(--s0);
    font-size: var(--fs-body);
    font-weight: var(--weight-medium);
    overflow-wrap: anywhere;
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

  .diff {
    margin: var(--s0);
    padding: var(--s3);
    border-radius: var(--radius-sm);
    background: var(--code);
    max-height: calc(var(--fs-sm) * var(--lh-body) * 12);
    overflow: auto;
    overscroll-behavior: contain;
  }

  .diff:focus-visible {
    outline: var(--focus-width) solid var(--accent);
  }

  .diff-line {
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    line-height: var(--lh-body);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .diff-line[data-mark='added'] {
    color: var(--success);
  }

  .diff-line[data-mark='removed'] {
    color: var(--danger);
  }

  .diff-line[data-mark='changed'] {
    color: var(--warning);
  }

  .diff-line[data-mark='context'] {
    color: var(--muted);
  }

  .empty,
  .unchanged {
    margin: var(--s0);
    font-size: var(--fs-meta);
    color: var(--muted);
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
