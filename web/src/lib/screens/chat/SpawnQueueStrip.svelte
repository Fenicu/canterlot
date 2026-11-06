<script lang="ts">
  import { cancelSpawnQueue, startSpawnQueueNow } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { intents } from '#lib/ops/intent.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import { START_NOW_WARNING, actionNote, itemExpiry, itemReason, pendingItems, queueSummary } from '#lib/routing/queue.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import Button from '#lib/ui/Button.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import { blockFold, rowFold } from '#lib/ui/fold.js';
  import { onMount } from 'svelte';

  let { chat }: { chat: ChatStore } = $props();

  let open = $state(false);
  let confirming = $state<string | null>(null);
  let notes = $state<Record<string, string>>({});
  const listId = $props.id();

  onMount(() => ticker.retain());
  const now = $derived.by(() => ticker.second * 1000);
  const items = $derived(pendingItems(chat.spawnQueue));
  const summary = $derived(queueSummary(chat.spawnQueue, now));

  async function cancel(id: string) {
    notes = { ...notes, [id]: '' };
    try {
      const answer = await intents.run(`spawn-queue-cancel:${id}`, (key) => cancelSpawnQueue(chat.id, id, key));
      const note = actionNote(answer.data.status);
      if (note) notes = { ...notes, [id]: note };
    } catch (err) {
      notes = { ...notes, [id]: errorText(err, 'Отменить не удалось') };
    }
  }

  async function startNow(id: string) {
    notes = { ...notes, [id]: '' };
    try {
      const answer = await intents.run(`spawn-queue-start:${id}`, (key) => startSpawnQueueNow(chat.id, id, key));
      const note = actionNote(answer.data.status);
      if (note) notes = { ...notes, [id]: note };
    } catch (err) {
      notes = { ...notes, [id]: errorText(err, 'Запустить не удалось') };
    }
  }
</script>

{#if items.length > 0}
  <section class="queue" data-role="spawn-queue" aria-label="Очередь запуска">
    <button type="button" class="head" aria-expanded={open} aria-controls={listId} onclick={() => (open = !open)}>
      <Icon name="clock" />
      <span class="summary" data-role="spawn-queue-summary">{summary}</span>
      <span class="chevron" data-open={open ? '' : undefined}><Icon name="chevron" /></span>
    </button>
    {#if open}
      <div class="fold" transition:blockFold>
        <ul id={listId}>
          {#each items as item, index (item.id)}
            <li data-queue-item={item.id} data-state={item.state} transition:rowFold>
              <p class="title">Запуск {index + 1}{item.state === 'starting' ? ' · запускается' : ''}</p>
              <p class="reason" data-role="queue-reason">{itemReason(item, now)}</p>
              <p class="expiry">{itemExpiry(item, now)}</p>
              {#if item.state === 'queued'}
                <div class="actions">
                  {#if intents.busy(`spawn-queue-start:${item.id}`)}
                    <Button variant="primary" disabled reason="Запускаю…">Запустить сейчас</Button>
                  {:else}
                    <Button variant="primary" onclick={() => (confirming = item.id)}>Запустить сейчас</Button>
                  {/if}
                  {#if intents.busy(`spawn-queue-cancel:${item.id}`)}
                    <Button disabled reason="Отменяю…">Отменить</Button>
                  {:else}
                    <Button onclick={() => void cancel(item.id)}>Отменить</Button>
                  {/if}
                </div>
              {/if}
              {#if notes[item.id]}<p class="note" role="alert">{notes[item.id]}</p>{/if}
            </li>
          {/each}
        </ul>
      </div>
    {/if}
  </section>
{/if}

{#if confirming}
  <ConfirmDialog
    title="Запустить сейчас?"
    body={START_NOW_WARNING}
    confirmLabel="Запустить сейчас"
    onconfirm={() => { const id = confirming; confirming = null; if (id) void startNow(id); }}
    onclose={() => (confirming = null)}
  />
{/if}

<style>
  .queue { display: flex; flex-direction: column; padding: var(--s2) var(--gutter) var(--s0); }
  .head { display: flex; align-items: center; gap: var(--s4); min-height: var(--target); padding: var(--s0) var(--s6); border-radius: var(--radius-pill); background: var(--surface); color: var(--secondary); font-size: var(--fs-sm); text-align: left; }
  .head :global(svg) { width: var(--icon-small); height: var(--icon-small); flex: none; }
  .summary { flex: 1; min-width: 0; overflow: hidden; color: var(--text); text-overflow: ellipsis; white-space: nowrap; }
  .chevron { display: flex; color: var(--muted); transition: transform var(--duration-fast) var(--ease); }
  .chevron[data-open] { transform: rotate(90deg); }
  .fold { display: grid; grid-template-rows: 1fr; }
  ul { display: flex; flex-direction: column; gap: var(--s2); min-height: 0; margin: var(--s0); padding: var(--s2) var(--s0) var(--s0); overflow: hidden; list-style: none; }
  li { display: flex; flex-direction: column; gap: var(--s1); padding: var(--s4) var(--s6); border-radius: var(--radius-card); background: var(--surface); }
  p { margin: var(--s0); overflow-wrap: anywhere; }
  .title { color: var(--text); font-size: var(--fs-meta); font-weight: var(--weight-medium); }
  .reason { color: var(--secondary); font-size: var(--fs-sm); }
  .expiry, .note { color: var(--muted); font-size: var(--fs-sm); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s2); margin-top: var(--s2); }
  .actions :global(.reason) { margin-block: var(--s0); }
</style>
