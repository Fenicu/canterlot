<script lang="ts">
  import { purgeConversation, restoreConversation } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { cachedConfig } from '#lib/api/config.js';
  import { fmt } from '#lib/format/ru.js';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import type { ListRow } from '#lib/stores/list.svelte.js';

  let {
    items,
    now = Date.now(),
    more = false,
    loading = false,
    onmore
  }: {
    items: ListRow[];
    now?: number;
    more?: boolean;
    loading?: boolean;
    onmore?: () => void;
  } = $props();

  let busy = $state('');
  let error = $state('');
  let confirming = $state<ListRow | null>(null);

  function retentionMs(): number {
    const seconds = cachedConfig()?.sessions?.trash_retention_secs ?? 86_400;
    return Math.max(0, seconds) * 1000;
  }

  function left(item: ListRow): string {
    const trashed = item.trashed_at ? Date.parse(item.trashed_at) : now;
    return fmt.durationCoarse(Math.max(0, trashed + retentionMs() - now));
  }

  async function restore(item: ListRow): Promise<void> {
    busy = item.id;
    error = '';
    try {
      await restoreConversation(item.id, crypto.randomUUID());
    } catch (failure) {
      error = errorText(failure, 'Не удалось вернуть сессию');
    } finally {
      busy = '';
    }
  }

  async function purge(item: ListRow): Promise<void> {
    busy = item.id;
    error = '';
    try {
      await purgeConversation(item.id, crypto.randomUUID());
      confirming = null;
    } catch (failure) {
      error = errorText(failure, 'Не удалось удалить сессию');
    } finally {
      busy = '';
    }
  }
</script>

<section class="trash">
  <h2>Корзина <span class="count">{items.length}</span></h2>
  {#each items as item (item.id)}
    <article class="row" data-session={item.id}>
      <div class="body">
        <span class="title" title={item.title}>{item.title}</span>
        <span class="meta">удалится через {left(item)}</span>
      </div>
      <div class="actions">
        <button type="button" onclick={() => { if (!busy) void restore(item); }}>Восстановить</button>
        <button type="button" class="danger" onclick={() => { if (!busy) confirming = item; }}>Удалить навсегда</button>
      </div>
    </article>
  {/each}
  {#if more}
    <button type="button" class="more" aria-busy={loading} onclick={() => { if (!loading) onmore?.(); }}>
      {loading ? 'Загружаю' : 'Показать ещё'}
    </button>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

{#if confirming}
  <ConfirmDialog
    title="Удалить сессию навсегда?"
    body={`Сессия «${confirming.title}» будет удалена вместе с историей и вложениями. Это нельзя отменить.`}
    confirmLabel="Удалить навсегда"
    danger
    onconfirm={() => void purge(confirming!)}
    onclose={() => (confirming = null)}
  />
{/if}

<style>
  .trash { padding-bottom: var(--s8); }
  h2 {
    display: flex;
    align-items: center;
    gap: var(--s2);
    min-height: var(--target);
    margin: 0;
    padding: var(--s2) var(--gutter);
    color: var(--secondary);
    font-size: var(--fs-meta);
    font-weight: var(--weight-medium);
  }
  .count { color: var(--muted); font-variant-numeric: tabular-nums; }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--s3);
    margin-inline: var(--s3);
    padding: var(--s4) var(--s3) var(--s4) var(--s6);
    border-radius: var(--radius-card);
  }
  .body { flex: 1 1 auto; min-width: 0; display: flex; flex-direction: column; gap: var(--s1); }
  .title { overflow-wrap: anywhere; }
  .meta { color: var(--secondary); font-size: var(--fs-sm); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s2); }
  button {
    min-height: var(--target);
    padding-inline: var(--s4);
    border: var(--s0);
    border-radius: var(--radius-pill);
    background: var(--chip);
    color: var(--chip-text);
  }
  button.danger { background: var(--danger-soft); color: var(--danger); }
  button.more { display: flex; margin: var(--s3) auto 0; }
  .error { margin: var(--s2) var(--gutter); color: var(--danger); font-size: var(--fs-sm); }
</style>
