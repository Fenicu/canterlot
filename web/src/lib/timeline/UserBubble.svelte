<script lang="ts">
  import { getRuntime } from '#lib/app/runtime.js';
  import { durationMs } from '#lib/motion.js';
  import { rejectionLabel } from '#lib/format/labels.js';
  import { fmt } from '#lib/format/ru.js';
  import Button from '#lib/ui/Button.svelte';
  import AttachmentTile from '#lib/timeline/AttachmentTile.svelte';
  import { attachmentKind, attachmentUrl } from '#lib/timeline/attachments.js';
  import type { Component } from 'svelte';
  import StageTrack from '#lib/ui/StageTrack.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import type { Stage } from '#lib/ui/stage-track.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import type { InputView } from '#lib/stores/inputs.js';
  import { retryAsNew, returnToDraft } from '#lib/timeline/QueueActions.js';
  import { deliveryIndex, deliveryLabel } from '#lib/timeline/present.js';
  import { originLabel } from '#lib/timeline/origin.js';

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
  // Просмотрщик грузится при первом открытии: в ленте он нужен редко.
  let viewing = $state<number | null>(null);
  let Viewer = $state<Component<{ shots: { media_id: string; src: string; mime: string; caption: string }[]; start?: number; title: string; heading: string; onclose: () => void }> | null>(null);
  const images = $derived(input.attachments.filter((file) => attachmentKind(file) === 'image'));
  const shots = $derived(
    images.map((file) => ({
      media_id: file.attachment_id,
      src: attachmentUrl(file.attachment_id),
      mime: file.mime,
      caption: [file.name, file.size > 0 ? fmt.bytes(file.size) : ''].filter(Boolean).join(' · ')
    }))
  );

  async function view(attachmentId: string): Promise<void> {
    Viewer ??= (await import('#lib/timeline/MediaViewer.svelte')).default;
    viewing = Math.max(0, images.findIndex((file) => file.attachment_id === attachmentId));
  }
  const unsure = $derived(getRuntime().journal.ops.get(input.key)?.state === 'unknown');

  // Входит только пузырь, который владелец отправил сейчас: ряды, вернувшиеся в окно при прокрутке, не оживают.
  function enter(node: HTMLElement): void {
    if (!input.optimistic || typeof node.animate !== 'function') return;
    if (durationMs('--duration-enter') <= 0) return;
    const root = getComputedStyle(document.documentElement);
    node.animate(
      [{ opacity: 0, transform: `translateY(${root.getPropertyValue('--shift-enter').trim() || '0px'})` }, { opacity: 1, transform: 'none' }],
      { duration: durationMs('--duration-enter'), easing: root.getPropertyValue('--ease-enter').trim() || 'ease-out' }
    );
  }
  const from = $derived(originLabel(input.origin));
  const stamp = $derived(input.state === 'accepted' && input.createdAt !== null ? ` · ${fmt.clock(input.createdAt)}` : '');
</script>

<article class="bubble" data-state={input.state} data-origin={input.origin?.kind} data-optimistic={input.optimistic ? '' : undefined} use:enter>
  {#if from}<p class="origin">{from}</p>{/if}
  <p class="text">{input.text}</p>
  {#if input.attachments.length > 0}
    <ul class="files">
      {#each input.attachments as file (file.attachment_id)}
        <li><AttachmentTile {file} onopen={() => void view(file.attachment_id)} /></li>
      {/each}
    </ul>
  {/if}
  {#if settled}
    <p class="status">{label}{stamp}</p>
  {:else}
    <StageTrack {stages} {index} state="active" form="compact" label="Доставка сообщения" status={label} />
  {/if}
  {#if unsure}
    <p class="reason">Отправка не подтверждена, повторяю</p>
    <div class="actions"><Button onclick={() => getRuntime().journal.retryNow(input.key)}>Повторить сейчас</Button></div>
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
{#if viewing !== null && Viewer}
  <Viewer {shots} start={viewing} title={input.text.slice(0, 80) || 'Сообщение'} heading="Вложения" onclose={() => { viewing = null; }} />
{/if}
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
  .bubble[data-origin] { --st-surface: var(--raised); background: var(--raised); }
  .origin { color: var(--secondary); font-size: var(--fs-sm); font-weight: var(--weight-medium); }
  .bubble[data-state='rejected'] { background: var(--danger-soft); }
  p { margin: 0; overflow-wrap: anywhere; }
  .text { white-space: pre-wrap; }
  .status, .reason { color: var(--secondary); font-size: var(--fs-sm); }
  [data-state='rejected'] .status { color: var(--danger); font-weight: var(--weight-medium); }
  [data-state='acceptance_unconfirmed'] .status { color: var(--warning); }
  .files { display: flex; flex-wrap: wrap; align-items: flex-start; gap: var(--s2); margin: 0; padding: 0; list-style: none; }
  li { display: flex; min-width: 0; max-width: 100%; }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s2); }
</style>
