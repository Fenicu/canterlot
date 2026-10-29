<script lang="ts">
  import { getRuntime } from '$lib/app/runtime';
  import { restoreComposerText } from '$lib/composer/restore';
  import StageTrack from '$lib/ui/StageTrack.svelte';
  import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
  import type { Stage } from '$lib/ui/stage-track';
  import type { ChatStore } from '$lib/stores/chat.svelte';
  import type { InputView } from '$lib/stores/inputs';
  import { retryAsNew, returnToDraft } from '$lib/timeline/QueueActions';
  import { deliveryIndex, deliveryLabel } from '$lib/timeline/present';

  let { input, chat, agentWorking = false }: { input: InputView; chat: ChatStore; agentWorking?: boolean } = $props();
  let ask = $state<string | null>(null);
  let decided = $state<((ok: boolean) => void) | null>(null);

  function confirm(message: string): Promise<boolean> {
    ask = message;
    return new Promise((resolve) => {
      decided = resolve;
    });
  }

  function answer(ok: boolean) {
    decided?.(ok);
    decided = null;
    ask = null;
  }
  const stages: Stage[] = [
    { id: 'saved', label: 'сохранено' },
    { id: 'machine', label: 'на машине' },
    { id: 'sending', label: 'передаётся' },
    { id: 'accepted', label: 'принято' },
    { id: 'working', label: 'агент работает' }
  ];
  const label = $derived(deliveryLabel(input.state, agentWorking));
  const index = $derived(deliveryIndex(input.state));
  const trackState = $derived(input.state === 'rejected' ? 'error' : input.state === 'accepted' && !agentWorking ? 'done' : 'active');
</script>

<article class="bubble" data-state={input.state}>
  <p>{input.text}</p>
  <StageTrack {stages} {index} state={trackState} form="compact" label="Доставка сообщения" status={label} />
  <p class="mark">{label}</p>
  {#if input.state === 'acceptance_unconfirmed'}
    <p>Приём не подтверждён</p>
    <button
      type="button"
      onclick={() =>
        retryAsNew(input, {
          ops: getRuntime().journal,
          chat,
          conversationId: chat.id,
          confirm
        })}
    >Отправить повторно как новое</button>
  {/if}
  {#if input.state === 'rejected'}
    {#if input.rejection}<p>{input.rejection}</p>{/if}
    <button
      type="button"
      onclick={() =>
        returnToDraft(input, {
          drafts: getRuntime().drafts,
          conversationId: chat.id,
          chat,
          onReturn: restoreComposerText
        })}
    >Вернуть в черновик</button>
  {/if}
</article>
{#if ask}
  <ConfirmDialog title="Повторная отправка" body={ask} confirmLabel="Отправить" onconfirm={() => answer(true)} onclose={() => answer(false)} />
{/if}

<style>
  .bubble {
    background: var(--accent-soft);
    border-radius: var(--radius-card);
    padding: var(--s3);
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }
  .mark { margin: 0; color: var(--text); }
  p { margin: 0; overflow-wrap: anywhere; }
  button { min-height: var(--target); border: var(--s0); background: var(--transparent); color: var(--secondary); font-size: var(--fs-meta); }
</style>
