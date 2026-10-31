<script lang="ts">
  import { getRuntime } from '#lib/app/runtime.js';
  import { rejectionLabel } from '#lib/format/labels.js';
  import { fmt } from '#lib/format/ru.js';
  import Button from '#lib/ui/Button.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import StageTrack from '#lib/ui/StageTrack.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import type { Stage } from '#lib/ui/stage-track.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import type { InputView } from '#lib/stores/inputs.js';
  import { retryAsNew, returnToDraft } from '#lib/timeline/QueueActions.js';
  import { deliveryIndex, deliveryLabel } from '#lib/timeline/present.js';

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
  // Дорожка нужна, только пока доставка идёт; в истории она превратилась бы в самый громкий повторяющийся элемент.
  const settled = $derived(
    input.state === 'rejected' ||
      input.state === 'cancelled' ||
      input.state === 'acceptance_unconfirmed' ||
      (input.state === 'accepted' && !agentWorking)
  );
  const stamp = $derived(input.state === 'accepted' && input.createdAt !== null ? ` · ${fmt.clock(input.createdAt)}` : '');
</script>

<article class="bubble" data-state={input.state}>
  <p class="text">{input.text}</p>
  {#if input.attachments.length > 0}
    <ul class="files">
      {#each input.attachments as file (file.attachment_id)}
        <li><Icon name="attach" /><span>{file.name}</span></li>
      {/each}
    </ul>
  {/if}
  {#if settled}
    <p class="status">{label}{stamp}</p>
  {:else}
    <StageTrack {stages} {index} state="active" form="compact" label="Доставка сообщения" status={label} />
  {/if}
  {#if input.state === 'acceptance_unconfirmed'}
    <div class="actions">
      <Button
        onclick={() =>
          retryAsNew(input, {
            ops: getRuntime().journal,
            chat,
            conversationId: chat.id,
            confirm
          })}
      >Отправить повторно как новое</Button>
    </div>
  {/if}
  {#if input.state === 'rejected'}
    {#if input.rejection}<p class="reason">{rejectionLabel(input.rejection)}</p>{/if}
    <div class="actions">
      <Button
        onclick={() =>
          returnToDraft(input, {
            drafts: getRuntime().drafts,
            conversationId: chat.id,
            chat
          })}
      >Вернуть в черновик</Button>
    </div>
  {/if}
</article>
{#if ask}
  <ConfirmDialog title="Повторная отправка" body={ask} confirmLabel="Отправить" onconfirm={() => answer(true)} onclose={() => answer(false)} />
{/if}

<style>
  .bubble {
    --st-surface: var(--accent-soft);
    width: fit-content;
    min-width: 0;
    max-width: var(--bubble-max);
    margin-left: auto;
    background: var(--accent-soft);
    border-radius: var(--radius-card);
    padding: var(--s5) var(--s6);
    display: flex;
    flex-direction: column;
    gap: var(--s3);
  }
  .bubble[data-state='rejected'] { background: var(--danger-soft); }
  p { margin: 0; overflow-wrap: anywhere; }
  .text { white-space: pre-wrap; }
  .status, .reason { color: var(--secondary); font-size: var(--fs-sm); }
  [data-state='rejected'] .status { color: var(--danger); font-weight: var(--weight-medium); }
  [data-state='acceptance_unconfirmed'] .status { color: var(--warning); }
  .files { display: flex; flex-wrap: wrap; gap: var(--s2); margin: 0; padding: 0; list-style: none; }
  li { display: inline-flex; align-items: center; gap: var(--s2); min-width: 0; max-width: 100%; padding: var(--s1) var(--s4); border-radius: var(--radius-pill); background: var(--raised); font-size: var(--fs-sm); }
  li :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  li span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s2); }
</style>
