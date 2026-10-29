<script lang="ts">
  import type { RequestItem } from '$lib/stores/requests.svelte';
  import type { DecisionBody } from '$lib/api/client';

  let {
    request,
    onDecision,
    disabled = false
  }: {
    request: RequestItem;
    onDecision: (d: DecisionBody) => Promise<void>;
    disabled?: boolean;
  } = $props();

  const payload = $derived((request.payload ?? {}) as Record<string, unknown>);
  const command = $derived(typeof payload.command === 'string' ? payload.command : '');
  const description = $derived(typeof payload.description === 'string' ? payload.description : '');
  const toolName = $derived(
    typeof payload.tool_name === 'string'
      ? payload.tool_name
      : typeof payload.toolName === 'string'
        ? payload.toolName
        : ''
  );

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

<div class="permission-card" data-disabled={isDisabled ? '' : undefined}>
  <div class="card-head">
    <span class="meta">Запрос разрешения</span>
    {#if toolName}
      <span class="tool-tag">{toolName}</span>
    {/if}
  </div>

  {#if description}
    <p class="description">{description}</p>
  {/if}

  {#if command}
    <pre class="command-block"><code>{command}</code></pre>
  {/if}

  <div class="actions">
    <button
      type="button"
      class="btn btn-allow"
      data-action="allow"
      use:disableIf={isDisabled}
      onclick={handleAllow}
    >
      Разрешить
    </button>
    <button
      type="button"
      class="btn btn-deny"
      data-action="deny"
      use:disableIf={isDisabled}
      onclick={handleDeny}
    >
      Отклонить
    </button>
  </div>
</div>

<style>
  .permission-card {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    color: var(--text);
  }

  .card-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--s2);
  }

  .meta {
    font-size: var(--fs-meta);
    color: var(--muted);
  }

  .tool-tag {
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    padding: var(--s1) var(--s2);
    border-radius: var(--radius-sm);
    background: var(--raised);
    color: var(--secondary);
    border: var(--hairline) solid var(--control-line);
  }

  .description {
    margin: var(--s0);
    font-size: var(--fs-body);
    line-height: var(--lh-body);
    color: var(--text);
  }

  .command-block {
    margin: var(--s0);
    padding: var(--s3);
    border-radius: var(--radius-sm);
    background: var(--code);
    border: var(--hairline) solid var(--line);
    overflow-x: auto;
  }

  .command-block code {
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    color: var(--text);
    white-space: pre-wrap;
    word-break: break-all;
  }

  .actions {
    display: flex;
    gap: var(--s3);
    margin-top: var(--s2);
  }

  .btn {
    min-height: var(--target);
    padding: var(--s2) var(--s6);
    border-radius: var(--radius-pill);
    font-size: var(--fs-body);
    font-weight: var(--weight-medium);
    cursor: pointer;
    border: var(--hairline) solid var(--transparent);
    transition: background var(--duration-fast) var(--ease), opacity var(--duration-fast) var(--ease);
  }

  .btn:focus-visible {
    outline: var(--focus-width) solid var(--accent);
    outline-offset: var(--s1);
  }

  .btn:disabled {
    opacity: var(--opacity-low);
    cursor: not-allowed;
  }

  .btn-allow {
    background: var(--accent);
    color: var(--accent-ink);
  }

  .btn-deny {
    background: var(--surface);
    color: var(--danger);
    border-color: var(--control-line);
  }

  .btn-deny:not(:disabled):hover {
    background: var(--danger-soft);
  }
</style>
