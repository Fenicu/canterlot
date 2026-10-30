<script lang="ts">
  import type { RequestItem } from '#lib/stores/requests.svelte.js';
  import type { DecisionBody } from '#lib/api/client.js';
  import { permissionView } from './permissionView';
  import TruncatedNote from './TruncatedNote.svelte';

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
  const view = $derived(permissionView(payload));
  const toolName = $derived(
    typeof payload.tool_name === 'string'
      ? payload.tool_name
      : typeof payload.toolName === 'string'
        ? payload.toolName
        : ''
  );

  const isElicitation = $derived(toolName === 'mcp_elicitation');
  const elicitationMessage = $derived(
    isElicitation && typeof payload.message === 'string' ? payload.message : ''
  );
  const elicitationServer = $derived(
    isElicitation && typeof payload.server === 'string' ? payload.server : ''
  );
  const elicitationMode = $derived(
    isElicitation && typeof payload.mode === 'string' ? payload.mode : ''
  );
  const elicitationUrl = $derived(
    isElicitation && typeof payload.url === 'string' ? payload.url : ''
  );

  // Безопасная ссылка только на http(s), остальное — plain text. URL-парсер, не regex.
  function safeHttpUrl(value: string): string | null {
    try {
      const parsed = new URL(value);
      if (parsed.protocol === 'http:' || parsed.protocol === 'https:') return value;
    } catch {
      // не URL — обычным текстом
    }
    return null;
  }

  const safeUrl = $derived(safeHttpUrl(elicitationUrl));
  const urlHost = $derived(safeUrl ? new URL(safeUrl).host : '');
  const opensLink = $derived(elicitationMode === 'url' && safeUrl !== null);
  const missingUrl = $derived(elicitationMode === 'url' && elicitationUrl.trim().length === 0);

  const isDisabled = $derived(disabled || request.state !== 'open');
  const isAllowDisabled = $derived(isDisabled || missingUrl);

  function disableIf(node: HTMLButtonElement, val: boolean) {
    node.disabled = val;
    return {
      update(v: boolean) {
        node.disabled = v;
      }
    };
  }

  async function handleAllow() {
    if (isAllowDisabled) return;
    // Для запроса со ссылкой «Открыть» обязана открыть её, иначе подпись обещает то, чего не делает.
    if (opensLink && safeUrl) window.open(safeUrl, '_blank', 'noopener,noreferrer');
    await onDecision({ kind: 'permission', behavior: 'allow' });
  }

  async function handleDeny() {
    if (isDisabled) return;
    await onDecision({ kind: 'permission', behavior: 'deny' });
  }
</script>

<div class="permission-card" data-disabled={isDisabled ? '' : undefined}>
  <div class="card-head">
    {#if isElicitation}
      <span class="meta">Запрос MCP-сервера</span>
    {:else}
      <span class="meta">Запрос разрешения</span>
      {#if toolName}<span class="tool-tag">{toolName}</span>{/if}
    {/if}
  </div>

  {#if opensLink && elicitationServer}
    <h3 class="title">Сервер {elicitationServer} просит открыть ссылку</h3>
  {:else if elicitationServer}
    <p class="meta-line">Сервер: {elicitationServer}</p>
  {/if}

  {#if elicitationMessage}
    <p class="description">{elicitationMessage}</p>
  {/if}

  {#if view.description}
    <p class="description">{view.description}</p>
  {/if}

  {#if view.reason}
    <p class="description">{view.reason}</p>
  {/if}

  {#if elicitationUrl}
    {#if safeUrl}
      <!-- Внешняя ссылка сервера: resolve() здесь не нужен -->
      <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
      <a class="url" href={safeUrl} target="_blank" rel="noopener noreferrer"><strong>{urlHost}</strong><span class="full">{elicitationUrl}</span></a>
    {:else}
      <p class="url-text">{elicitationUrl}</p>
    {/if}
  {/if}

  {#if missingUrl}
    <p class="missing-url">Сервер не передал адрес</p>
  {/if}

  {#if view.command}
    <pre class="command-block"><code>{view.command}</code></pre>
  {/if}

  {#each view.facts as fact (fact.label)}
    <p class="meta-line">{fact.label}: {fact.value}</p>
  {/each}

  {#each view.changes as change, index (index)}
    <p class="meta-line">{change.kind}: {change.path}</p>
    {#if change.diff}
      <pre class="command-block detail"><code>{change.diff}</code></pre>
    {/if}
  {/each}

  {#each view.fields as field (field.label)}
    <p class="meta-line">{field.label}</p>
    <pre class="command-block detail"><code>{field.value}</code></pre>
  {/each}

  {#if view.denyInterrupts}
    <p class="warning" role="note">Отказ также прервёт ход</p>
  {/if}

  <TruncatedNote payload={request.payload} />

  <div class="actions">
    <button
      type="button"
      class="btn btn-allow"
      data-action="allow"
      use:disableIf={isAllowDisabled}
      onclick={handleAllow}
    >
      {opensLink ? 'Открыть' : 'Разрешить'}
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

  .meta-line {
    margin: var(--s0);
    font-size: var(--fs-meta);
    color: var(--muted);
    overflow-wrap: anywhere;
  }

  .warning {
    margin: var(--s0);
    padding: var(--s2) var(--s3);
    border-radius: var(--radius-sm);
    background: var(--warning-soft);
    color: var(--text);
    font-size: var(--fs-meta);
  }

  .url,
  .url-text {
    margin: var(--s0);
    font-size: var(--fs-body);
    overflow-wrap: anywhere;
  }

  .url {
    min-height: var(--target);
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    color: var(--accent-link);
    font-weight: var(--weight-medium);
  }

  .full {
    display: -webkit-box;
    overflow: hidden;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    color: var(--muted);
    font-size: var(--fs-sm);
    font-weight: var(--weight-normal);
  }

  .title {
    margin: var(--s0);
    font-size: var(--fs-title);
    font-weight: var(--weight-semibold);
    line-height: var(--lh-tight);
  }

  .url:focus-visible {
    outline: var(--focus-width) solid var(--accent);
  }

  .missing-url {
    margin: var(--s0);
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
    overflow-x: auto;
  }

  .detail {
    max-height: calc(var(--fs-sm) * var(--lh-body) * 12);
    overflow-y: auto;
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
