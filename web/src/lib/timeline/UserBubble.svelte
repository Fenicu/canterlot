<script lang="ts">
  import { getRuntime } from '#lib/app/runtime.js';
  import { unconfirmed } from '#lib/ops/journal.js';
  import { durationMs } from '#lib/motion.js';
  import { rejectionLabel } from '#lib/format/labels.js';
  import { fmt } from '#lib/format/ru.js';
  import Button from '#lib/ui/Button.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import Textarea from '#lib/ui/Textarea.svelte';
  import AttachmentTile from '#lib/timeline/AttachmentTile.svelte';
  import { attachmentKind, attachmentUrl } from '#lib/timeline/attachments.js';
  import type { Component } from 'svelte';
  import StageTrack from '#lib/ui/StageTrack.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import type { Stage } from '#lib/ui/stage-track.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import type { InputView } from '#lib/stores/inputs.js';
  import { STEER_RETURNED_LABEL, retryAsNew, returnToDraft, sendNow, sendNowCaption } from '#lib/timeline/QueueActions.js';
  import { editInput, editInputCaption } from '#lib/timeline/QueueActions.js';
  import { deliveryIndex, deliveryLabel } from '#lib/timeline/present.js';
  import { turnIsLive } from '#lib/composer/sendRules.js';
  import { originLabel } from '#lib/timeline/origin.js';
  import { timeTip } from '#lib/ui/time-tip.js';

  let { input, chat, agentWorking = false, highlight = false }: { input: InputView; chat: ChatStore; agentWorking?: boolean; highlight?: boolean } = $props();
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
  const unsure = $derived(unconfirmed(getRuntime().journal.ops.get(input.key)));

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
  // Разделитель собирается в скрипте: Svelte срезает пробелы в начале и конце блока.
  const stampSeparator = ' · ';
  const stampAt = $derived(input.state === 'accepted' && input.createdAt !== null ? input.createdAt : null);
  const harnessId = $derived((chat.detail?.harness as { id?: string } | undefined)?.id ?? '');
  const steerSupported = $derived(harnessId === 'claude_code' || harnessId === 'codex' || harnessId === 'opencode' || harnessId === 'antigravity');
  const turnLive = $derived(turnIsLive(chat.activity, chat.queue.run_slot));
  // Кнопка видна, пока сообщение ждёт в очереди, а ход идёт: по умолчанию оно
  // ушло бы следующим ходом, «Отправить сейчас» вливает его в текущий.
  const showSendNow = $derived(input.state === 'queued' && input.id !== null && turnLive && steerSupported);
  let sendingNow = $state(false);
  let sendNowReason = $state<string | null>(null);
  let sendNowSent = $state(false);

  // Правка — в самом пузыре: текст уже на месте, черновик композера не трогаем.
  let editing = $state(false);
  let editText = $state('');
  let editKept = $state<string[]>([]);
  let editRevision = $state(1);
  let editBusy = $state(false);
  let editError = $state<string | null>(null);
  let editClosedNote = $state<string | null>(null);

  function startEdit() {
    editText = input.text;
    editKept = input.attachments.map((file) => file.attachment_id);
    editRevision = input.revision;
    editError = null;
    editClosedNote = null;
    editing = true;
  }

  function dropEditAttachment(attachmentId: string) {
    editKept = editKept.filter((id) => id !== attachmentId);
  }

  async function saveEdit() {
    if (editBusy || !input.id) return;
    editBusy = true;
    editError = null;
    try {
      const outcome = await editInput(input, chat.id, editText, editKept, editRevision);
      if (outcome === 'saved' || outcome === 'noop') {
        editing = false;
      } else {
        editError = editInputCaption(outcome.reason);
        if (outcome.reason === 'revision_conflict') {
          // Показываем текст, который уже сохранила другая сессия, и продолжаем
          // правку с её ревизией: иначе повтор снова упрётся в конфликт.
          editText = input.text;
          editKept = input.attachments.map((file) => file.attachment_id);
          editRevision = input.revision;
        }
        if (outcome.reason === 'input_not_queued') {
          editClosedNote = editError;
          editing = false;
        }
      }
    } finally {
      editBusy = false;
    }
  }

  async function doSendNow() {
    if (sendingNow) return;
    sendingNow = true;
    sendNowReason = null;
    try {
      const outcome = await sendNow(input, chat.id);
      if (outcome !== 'requested' && outcome !== 'noop') {
        sendNowReason = sendNowCaption(outcome.reason);
      } else if (outcome === 'requested') {
        sendNowSent = true;
      }
    } finally {
      sendingNow = false;
    }
  }
</script>

<!-- Программный фокус после прыжка: сообщение получает -1, чтобы экранный диктор прочитал его. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<article class="bubble" data-state={input.state} data-origin={input.origin?.kind} data-optimistic={input.optimistic ? '' : undefined} data-jump={highlight ? '' : undefined} tabindex={highlight ? -1 : undefined} use:enter>
  {#if from}<p class="origin">{from}</p>{/if}
  {#if editing}
    <div data-unsaved-edit><Textarea label="Текст сообщения" id="edit-{input.key}" bind:value={editText} /></div>
    {#if editKept.length > 0}
      <ul class="files">
        {#each input.attachments.filter((file) => editKept.includes(file.attachment_id)) as file (file.attachment_id)}
          <li class="edit-file">
            <AttachmentTile {file} onopen={() => void view(file.attachment_id)} />
            <IconButton icon="close" label="Убрать вложение {file.name}" onclick={() => dropEditAttachment(file.attachment_id)} />
          </li>
        {/each}
      </ul>
    {/if}
    {#if editError}<p class="reason">{editError}</p>{/if}
    <div class="actions">
      {#if editBusy}
        <Button variant="primary" disabled={true} reason="Сохраняется…">Сохранить</Button>
      {:else}
        <Button variant="primary" onclick={() => void saveEdit()}>Сохранить</Button>
      {/if}
      <Button onclick={() => { editing = false; }}>Отмена</Button>
    </div>
  {:else}
  <p class="text">{input.text}</p>
  {#if input.attachments.length > 0}
    <ul class="files">
      {#each input.attachments as file (file.attachment_id)}
        <li><AttachmentTile {file} onopen={() => void view(file.attachment_id)} /></li>
      {/each}
    </ul>
  {/if}
  {/if}
  {#if input.revision > 1 && !editing}<p class="edited">Изменено</p>{/if}
  {#if editClosedNote && !editing && input.state === 'queued'}<p class="reason">{editClosedNote}</p>{/if}
  {#if settled}
    <p class="status">{label}{#if stampAt !== null}{stampSeparator}<time datetime={new Date(stampAt).toISOString()} use:timeTip={stampAt}>{fmt.clock(stampAt)}</time>{/if}</p>
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
  {#if input.state === 'queued' && input.id !== null && !editing}
    <div class="actions">
      <Button onclick={() => startEdit()}>Изменить</Button>
      {#if showSendNow}
        {#if sendingNow}
          <Button disabled={true} reason="Отправляется…">Отправить сейчас</Button>
        {:else}
          <Button onclick={() => void doSendNow()}>Отправить сейчас</Button>
        {/if}
      {/if}
    </div>
    {#if (harnessId === 'opencode' || harnessId === 'antigravity') && showSendNow}<p class="reason">Прервёт текущий шаг</p>{/if}
    {#if sendNowReason}<p class="reason">{sendNowReason}</p>{/if}
    {#if input.steer === 'returned'}<p class="reason">{STEER_RETURNED_LABEL}</p>
    {:else if sendNowSent || input.steer === 'pending'}<p class="reason">Вливается в идущий ход</p>{/if}
  {:else if sendNowReason && input.state === 'queued'}
    <p class="reason">{sendNowReason}</p>
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
    --jump-rest: var(--accent-soft);
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
  .bubble[data-origin] { --st-surface: var(--raised); --jump-rest: var(--raised); background: var(--raised); }
  /* Цель прыжка вспыхивает тоном акцента сразу, держится миг и гаснет до своего фона: глаз находит её, движения строки нет. */
  .bubble[data-jump] {
    --jump-peak: color-mix(in srgb, var(--accent) var(--jump-glow-share), var(--jump-rest));
    animation: jump-glow var(--duration-jump-glow) var(--ease-jump-glow) both;
  }
  .bubble[data-jump]:focus {
    outline: var(--focus-width) solid var(--accent);
    outline-offset: var(--s1);
  }
  @keyframes jump-glow {
    0%, 15% { background-color: var(--jump-peak); }
    100% { background-color: var(--jump-rest); }
  }
  /* Без движения подсветка стоит ровным тоном, пока лента держит цель, и снимается сразу. */
  @media (prefers-reduced-motion: reduce) {
    .bubble[data-jump] { animation: none; background-color: var(--jump-peak); }
  }
  :global(:root[data-motion='off']) .bubble[data-jump] { background-color: var(--jump-peak); }
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
  .edit-file { align-items: center; gap: var(--s2); }
  .edited { color: var(--secondary); font-size: var(--fs-sm); }
</style>
