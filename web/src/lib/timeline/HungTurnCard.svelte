<script lang="ts">
  import { hungEpisode, hungInterrupt, hungSteer } from '#lib/api/client.js';
  import { ApiError } from '#lib/api/http.js';
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

  const episodeId = $derived(event.turn_hung_episode ?? null);
  const queuedId = $derived(event.queued_input_id ?? null);
  // Карточка действует, пока открыт её эпизод: после продолжения хода, его конца или
  // действия владельца хаб отвечает отказом, и кнопки гаснут.
  let resolved = $state<string | null>(null);
  const live = $derived(episodeId !== null && resolved === null);
  const canSteer = $derived(live && typeof queuedId === 'string' && queuedId.length > 0);

  const RESOLVED_TEXT: Record<string, string> = {
    owner_interrupt: 'Ход уже прерван',
    owner_steer: 'Сообщение уже влито',
    progress: 'Ход продолжился',
    turn_ended: 'Ход закончился',
    superseded: 'Карточка устарела'
  };

  $effect(() => {
    const episode = episodeId;
    if (!episode) return;
    let cancelled = false;
    hungEpisode(conversationId, episode)
      .then((response) => {
        const state = response.data.state;
        if (!cancelled && state !== 'open') resolved = state;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  });

  function closedBy(error: unknown): boolean {
    if (error instanceof ApiError && error.details?.reason === 'hung_episode_resolved') {
      resolved = 'progress';
      return true;
    }
    return false;
  }

  async function steer() {
    if (!canSteer || busy || !episodeId) return;
    busy = 'steer';
    failed = null;
    try {
      await intents.run(`hung-steer:${episodeId}`, (key) =>
        hungSteer(conversationId, episodeId, key),
      );
      done = 'Влито в ход';
      resolved = 'owner_steer';
    } catch (error) {
      if (!closedBy(error)) failed = 'Не удалось влить: сообщение уйдёт следующим ходом';
    } finally {
      busy = null;
    }
  }

  async function stop() {
    if (busy || !live || !episodeId) return;
    busy = 'interrupt';
    failed = null;
    try {
      await intents.run(`hung-interrupt:${episodeId}`, (key) =>
        hungInterrupt(conversationId, episodeId, key),
      );
      done = 'Ход прерван';
      resolved = 'owner_interrupt';
    } catch (error) {
      if (!closedBy(error)) failed = 'Не удалось прервать ход';
    } finally {
      busy = null;
    }
  }
</script>

<article class="hung" data-event-kind="turn_hung" data-role="hung-card">
  <p class="title">Ход завис</p>
  <p class="line">Генерация не отвечает, а процесс простаивает.</p>
  {#if resolved !== null && !done}
    <p class="status" data-role="hung-resolved" data-tone="quiet">
      {RESOLVED_TEXT[resolved] ?? 'Карточка устарела'}
    </p>
  {/if}
  <div class="actions" data-live={live}>
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
      {#if live}
        <Button
          busy={busy === 'interrupt'}
          onclick={() => void stop()}
        >Прервать</Button>
      {:else}
        <Button disabled reason="Карточка устарела">Прервать</Button>
      {/if}
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
