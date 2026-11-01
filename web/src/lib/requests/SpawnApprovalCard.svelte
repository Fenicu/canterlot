<script lang="ts">
  import type { RequestItem } from '#lib/stores/requests.svelte.js';
  import type { DecisionBody } from '#lib/api/client.js';
  import { spawnApprovalView } from './spawnApprovalView';

  let {
    request,
    onDecision,
    disabled = false
  }: {
    request: RequestItem;
    onDecision: (d: DecisionBody) => Promise<void>;
    disabled?: boolean;
  } = $props();

  const view = $derived(spawnApprovalView(request.payload));
  const isDisabled = $derived(disabled || request.state !== 'open');

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

<div class="spawn-card" data-spawn-approval data-disabled={isDisabled ? '' : undefined}>
  <h3 class="title">Агент просит запустить сессию</h3>

  <dl class="facts">
    {#each view.facts as fact (fact.label)}
      <div class="fact">
        <dt>{fact.label}</dt>
        <dd>{fact.value}</dd>
      </div>
    {/each}
  </dl>

  {#if view.brief}
    <p class="meta">Задача</p>
    <!-- Прокручиваемая область берёт фокус, иначе с клавиатуры её не прочесть -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <pre class="brief" tabindex="0"><code>{view.brief}</code></pre>
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
  .spawn-card {
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

  .meta {
    margin: var(--s0);
    font-size: var(--fs-meta);
    color: var(--muted);
  }

  .facts {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    margin: var(--s0);
  }

  .fact {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s1) var(--s3);
    min-width: 0;
  }

  dt {
    flex: none;
    font-size: var(--fs-meta);
    color: var(--muted);
  }

  dd {
    margin: var(--s0);
    min-width: 0;
    font-size: var(--fs-body);
    overflow-wrap: anywhere;
  }

  .brief {
    margin: var(--s0);
    padding: var(--s3);
    border-radius: var(--radius-sm);
    background: var(--code);
    max-height: calc(var(--fs-sm) * var(--lh-body) * 12);
    overflow: auto;
  }

  .brief:focus-visible {
    outline: var(--focus-width) solid var(--accent);
  }

  .brief code {
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    color: var(--text);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s3);
    margin-top: var(--s2);
  }

  .btn {
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
