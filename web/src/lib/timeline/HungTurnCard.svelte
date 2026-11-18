<script lang="ts">
  import { interrupt, sendNow as apiSendNow } from '#lib/api/client.js';
  import { intents } from '#lib/ops/intent.js';
  import Button from '#lib/ui/Button.svelte';
  import type { HubEvent } from '#lib/timeline/hubEvent.js';

  let {
    conversationId,
    event
  }: { conversationId: string; event: HubEvent } = $props();

  let busy = $state<'steer' | 'interrupt' | null>(null);
  let done = $state<string | null>(null);
  let failed = $state<string | null>(null);

  const queuedId = $derived(event.queued_input_id ?? null);
  const canSteer = $derived(typeof queuedId === 'string' && queuedId.length > 0);

  async function steer() {
    if (!canSteer || busy) return;
    busy = 'steer';
    failed = null;
    try {
      await intents.run(`hung-steer:${queuedId}`, (key) =>
        apiSendNow(conversationId, queuedId as string, key),
      );
      done = 'Влито в ход';
    } catch {
      failed = 'Не удалось влить: сообщение уйдёт следующим ходом';
    } finally {
      busy = null;
    }
  }

  async function stop() {
    if (busy) return;
    busy = 'interrupt';
    failed = null;
    try {
      await intents.run(`hung-interrupt:${conversationId}`, (key) =>
        interrupt(conversationId, key),
      );
      done = 'Ход прерван';
    } catch {
      failed = 'Не удалось прервать ход';
    } finally {
      busy = null;
    }
  }
</script>

<article class="hung" data-event-kind="turn_hung" data-role="hung-card">
  <p class="title">Ход завис</p>
  <p class="line">Генерация не отвечает, а процесс простаивает.</p>
  <div class="actions">
    {#if canSteer}
      <span data-action="steer">
        <Button
          variant="primary"
          busy={busy === 'steer'}
          onclick={() => void steer()}
        >Влить</Button>
      </span>
    {/if}
    <span data-action="interrupt">
      <Button
        busy={busy === 'interrupt'}
        onclick={() => void stop()}
      >Прервать</Button>
    </span>
  </div>
  {#if done}
    <p class="status" data-role="hung-done" data-tone="quiet">{done}</p>
  {/if}
  {#if failed}
    <p class="status" data-role="hung-failed" data-tone="warning">{failed}</p>
  {/if}
</article>

<style>
  .hung {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    min-width: 0;
    padding: var(--s5) var(--s6);
    border-radius: var(--radius-card);
    background: var(--warning-soft);
  }
  .title {
    margin: 0;
    color: var(--warning);
    font-size: var(--fs-body);
    font-weight: var(--weight-semibold);
    line-height: var(--lh-tight);
  }
  .line {
    margin: 0;
    color: var(--secondary);
    font-size: var(--fs-sm);
    overflow-wrap: anywhere;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s3);
  }
  .status {
    margin: 0;
    font-size: var(--fs-sm);
  }
  .status[data-tone='quiet'] {
    color: var(--muted);
  }
  .status[data-tone='warning'] {
    color: var(--warning);
  }
</style>
