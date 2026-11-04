<script lang="ts">
  import { errorText } from '#lib/api/errors.js';
  import { ApiError } from '#lib/api/http.js';
  import { intents } from '#lib/ops/intent.js';
  import { deleteScheduled, interrupt, patchScheduled } from '#lib/api/client.js';
  import { getRuntime } from '#lib/app/runtime.js';
  import { unconfirmed } from '#lib/ops/journal.js';
  import { returnToComposer } from '#lib/composer/returned.js';
  import { cancelInput } from '#lib/timeline/QueueActions.js';
  import { STEER_RETURNED_LABEL, editInput, editInputCaption, sendNow, sendNowCaption } from '#lib/timeline/QueueActions.js';
  import type { InputView } from '#lib/stores/inputs.js';
  import AttachmentTile from '#lib/timeline/AttachmentTile.svelte';
  import { turnIsLive } from '#lib/composer/sendRules.js';
  import { CANCEL_REQUESTED_LABEL } from '#lib/stores/inputs.js';
  import type { ChatStore, ScheduledMessage } from '#lib/stores/chat.svelte.js';
  import { deliveryLabel, scheduledLabel } from '#lib/timeline/present.js';
  import Button from '#lib/ui/Button.svelte';
  import Field from '#lib/ui/Field.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import Textarea from '#lib/ui/Textarea.svelte';

  let { chat }: { chat: ChatStore } = $props();
  const items = $derived(chat.strip());
  // Хаб отдаёт и завершённые задания: снимок держит их до 30 дней.
  const scheduled = $derived(chat.scheduled.filter((item) => item.state === 'pending'));
  const failed = $derived(chat.scheduled.filter((item) => item.state === 'failed'));
  let lost = $state<Record<string, boolean>>({});
  let notes = $state<Record<string, string>>({});
  let editId = $state<string | null>(null);
  let editText = $state('');
  let editAt = $state('');
  let expanded = $state(false);

  const COLLAPSE_ABOVE = 3;
  const VISIBLE = 2;
  const total = $derived(items.length + scheduled.length + failed.length);
  const collapsed = $derived(!expanded && total > COLLAPSE_ABOVE);
  // Сначала показываем очередь: одно скрытое сообщение заняло бы столько же места, сколько строка «Ещё».
  const shownItems = $derived(collapsed ? items.slice(0, VISIBLE) : items);
  const shownScheduled = $derived(collapsed ? scheduled.slice(0, Math.max(0, VISIBLE - shownItems.length)) : scheduled);
  const shownFailed = $derived(collapsed ? failed.slice(0, Math.max(0, VISIBLE - shownItems.length - shownScheduled.length)) : failed);
  const hidden = $derived(total - shownItems.length - shownScheduled.length - shownFailed.length);

  // Подпись кнопки называет сообщение: одинаковые «Изменить» в соседних строках не различить на слух.
  const short = (text: string): string => (text.length > 32 ? `${text.slice(0, 32).trimEnd()}…` : text);

  function textOf(item: ScheduledMessage): string {
    return item.content.map((block) => (block.type === 'text' ? block.text : '')).join('');
  }

  function atOf(item: ScheduledMessage): string {
    const parsed = Date.parse(item.run_at);
    return Number.isFinite(parsed) ? scheduledLabel(parsed) : 'Отправится позже';
  }

  function takeBackFailed(item: ScheduledMessage) {
    chat.scheduled = chat.scheduled.filter((job) => job.id !== item.id);
    void returnToComposer(
      chat.id,
      { text: textOf(item), attachments: item.attachment_ids.map((id) => ({ id })) },
      getRuntime().drafts
    );
  }

  let cancelNotes = $state<Record<string, string>>({});
  let editError = $state('');
  let savingScheduled = $state(false);
  let interruptError = $state('');

  async function cancel(key: string) {
    const input = chat.inputs.get(key);
    if (!input) return;
    cancelNotes = Object.fromEntries(Object.entries(cancelNotes).filter(([noted]) => noted !== key));
    const outcome = await cancelInput(input);
    // Прервать ход предлагаем только по ответу хаба «передача началась», а не по обрыву связи.
    if (outcome === 'lost') lost = { ...lost, [key]: true };
    else if (typeof outcome === 'object') cancelNotes = { ...cancelNotes, [key]: outcome.failed };
  }

  async function interruptRun() {
    interruptError = '';
    try {
      const id = chat.id;
      await intents.run(`interrupt:${id}`, (key) => interrupt(id, key));
    } catch (err) {
      interruptError = errorText(err, 'Прервать не удалось');
    }
  }

  function openEdit(id: string, text: string, raw: string) {
    editId = id;
    editText = text;
    const parsed = Date.parse(raw);
    if (!Number.isFinite(parsed)) {
      editAt = '';
      return;
    }
    const local = new Date(parsed);
    const pad = (n: number) => String(n).padStart(2, '0');
    editAt = `${local.getFullYear()}-${pad(local.getMonth() + 1)}-${pad(local.getDate())}T${pad(local.getHours())}:${pad(local.getMinutes())}`;
  }

  async function saveEdit() {
    const id = editId;
    if (!id) return;
    const at = Date.parse(editAt);
    const body = {
      content: [{ type: 'text' as const, text: editText }],
      run_at: Number.isFinite(at) ? new Date(at).toISOString() : undefined
    };
    editError = '';
    savingScheduled = true;
    try {
      await intents.run(`scheduled-edit:${id}:${JSON.stringify(body)}`, (key) => patchScheduled(id, body, key));
      editId = null;
    } catch (err) {
      if (err instanceof ApiError && err.code === 'not_pending') {
        notes = { ...notes, [id]: 'Уже отправлено' };
        editId = null;
      } else editError = errorText(err, 'Сохранить не удалось');
    } finally {
      savingScheduled = false;
    }
  }

  async function cancelScheduled(id: string) {
    try {
      await intents.run(`scheduled-cancel:${id}`, (key) => deleteScheduled(id, key));
    } catch (err) {
      notes = {
        ...notes,
        [id]: err instanceof ApiError && err.code === 'not_pending' ? 'Уже отправлено' : errorText(err, 'Отменить не удалось')
      };
    }
  }

  function retryUnknown(key: string) {
    getRuntime().journal.retryNow(key);
  }

  const turnLive = $derived(turnIsLive(chat.activity, chat.queue.run_slot));
  const harnessId = $derived((chat.detail?.harness as { id?: string } | undefined)?.id ?? '');
  const steerSupported = $derived(harnessId === 'claude_code' || harnessId === 'codex' || harnessId === 'opencode' || harnessId === 'antigravity');
  let sending = $state<Record<string, boolean>>({});
  let steerNotes = $state<Record<string, string | null>>({});
  let steered = $state<Record<string, boolean>>({});

  async function doSendNow(input: InputView) {
    if (sending[input.key] || !input.id) return;
    sending = { ...sending, [input.key]: true };
    try {
      const outcome = await sendNow(input, chat.id);
      if (outcome !== 'requested' && outcome !== 'noop') {
        steerNotes = { ...steerNotes, [input.key]: sendNowCaption(outcome.reason) };
      } else if (outcome === 'requested') {
        steered = { ...steered, [input.key]: true };
      }
    } finally {
      sending = { ...sending, [input.key]: false };
    }
  }

  let editing = $state<Record<string, boolean>>({});
  let editTexts = $state<Record<string, string>>({});
  let editKept = $state<Record<string, string[]>>({});
  let editRevisions = $state<Record<string, number>>({});
  let editBusy = $state<Record<string, boolean>>({});
  let editErrors = $state<Record<string, string | null>>({});
  let editClosed = $state<Record<string, string | null>>({});

  function startEdit(input: InputView) {
    editTexts = { ...editTexts, [input.key]: input.text };
    editKept = { ...editKept, [input.key]: input.attachments.map((file) => file.attachment_id) };
    editRevisions = { ...editRevisions, [input.key]: input.revision };
    editErrors = { ...editErrors, [input.key]: null };
    editClosed = { ...editClosed, [input.key]: null };
    editing = { ...editing, [input.key]: true };
  }

  function dropEditAttachment(input: InputView, attachmentId: string) {
    editKept = {
      ...editKept,
      [input.key]: (editKept[input.key] ?? []).filter((id) => id !== attachmentId)
    };
  }

  async function saveQueuedEdit(input: InputView) {
    if (editBusy[input.key] || !input.id) return;
    editBusy = { ...editBusy, [input.key]: true };
    editErrors = { ...editErrors, [input.key]: null };
    try {
      const outcome = await editInput(
        input,
        chat.id,
        editTexts[input.key] ?? input.text,
        editKept[input.key] ?? input.attachments.map((file) => file.attachment_id),
        editRevisions[input.key] ?? input.revision,
      );
      if (outcome === 'saved' || outcome === 'noop') {
        editing = { ...editing, [input.key]: false };
      } else {
        editErrors = { ...editErrors, [input.key]: editInputCaption(outcome.reason) };
        if (outcome.reason === 'revision_conflict') {
          // Показываем текст другой сессии и продолжаем с её ревизией.
          editTexts = { ...editTexts, [input.key]: input.text };
          editKept = {
            ...editKept,
            [input.key]: input.attachments.map((file) => file.attachment_id)
          };
          editRevisions = { ...editRevisions, [input.key]: input.revision };
        }
        if (outcome.reason === 'input_not_queued') {
          editClosed = { ...editClosed, [input.key]: editErrors[input.key] ?? null };
          editing = { ...editing, [input.key]: false };
        }
      }
    } finally {
      editBusy = { ...editBusy, [input.key]: false };
    }
  }
</script>

<div class="strip">
  {#each shownItems as input (input.key)}
    {@const op = getRuntime().journal.ops.get(input.key)}
    <div class="item" data-state={input.state}>
      <span class="pill">
        <span class="label accent">{deliveryLabel(input.state)}</span>
        <span class="text" title={input.text}>{input.text}</span>
        {#if input.state === 'queued' && input.id && !editing[input.key]}
          <IconButton icon="edit" label="Изменить: {short(input.text)}" onclick={() => startEdit(input)} />
        {/if}
        {#if input.state === 'queued' && input.id && turnLive && steerSupported}
          {#if sending[input.key]}
            <IconButton icon="play" label="Отправляется: {short(input.text)}" inert={true} />
          {:else}
            <IconButton icon="play" label="Отправить сейчас: {short(input.text)}" onclick={() => void doSendNow(input)} />
          {/if}
        {/if}
        {#if input.state === 'queued' || input.state === 'on_machine'}
          {#if input.id && intents.busy(`cancel:${input.id}`)}
            <IconButton icon="close" label="Отменяется…" inert={true} />
          {:else}
            <IconButton icon="close" label="Отменить: {short(input.text)}" onclick={() => cancel(input.key)} />
          {/if}
        {/if}
      </span>
      {#if cancelNotes[input.key]}<p class="note" role="alert">{cancelNotes[input.key]}</p>{/if}
      {#if input.cancelRequested}<p class="note">{CANCEL_REQUESTED_LABEL}</p>{/if}
      {#if input.revision > 1}<p class="note">Изменено</p>{/if}
      {#if editClosed[input.key] && !editing[input.key] && input.state === 'queued'}<p class="note">{editClosed[input.key]}</p>{/if}
      {#if editing[input.key]}
        <div data-unsaved-edit><Textarea label="Текст сообщения" id="strip-edit-{input.key}" bind:value={editTexts[input.key]} /></div>
        {#if (editKept[input.key] ?? []).length > 0}
          <ul class="files">
            {#each input.attachments.filter((file) => (editKept[input.key] ?? []).includes(file.attachment_id)) as file (file.attachment_id)}
              <li class="edit-file">
                <AttachmentTile {file} onopen={() => {}} />
                <IconButton icon="close" label="Убрать вложение {file.name}" onclick={() => dropEditAttachment(input, file.attachment_id)} />
              </li>
            {/each}
          </ul>
        {/if}
        {#if editErrors[input.key]}<p class="note">{editErrors[input.key]}</p>{/if}
        <span class="actions">
          {#if editBusy[input.key]}
            <Button variant="primary" disabled={true} reason="Сохраняется…">Сохранить</Button>
          {:else}
            <Button variant="primary" onclick={() => void saveQueuedEdit(input)}>Сохранить</Button>
          {/if}
          <Button onclick={() => { editing = { ...editing, [input.key]: false }; }}>Отмена</Button>
        </span>
      {/if}
      {#if input.state === 'queued' && input.id && turnLive && steerSupported && (harnessId === 'opencode' || harnessId === 'antigravity')}
        <p class="note">Прервёт текущий шаг</p>
      {/if}
      {#if steerNotes[input.key]}<p class="note">{steerNotes[input.key]}</p>{/if}
      {#if input.state === 'queued' && input.steer === 'returned'}<p class="note">{STEER_RETURNED_LABEL}</p>
      {:else if input.state === 'queued' && (steered[input.key] || input.steer === 'pending')}<p class="note">Вливается в идущий ход</p>{/if}
      {#if lost[input.key]}
        <p class="note">Передача уже началась, обычная отмена невозможна</p>
        <span class="actions">
          {#if intents.busy(`interrupt:${chat.id}`)}
            <Button disabled={true} reason="Прерываю…">Прервать исполнение</Button>
          {:else}
            <Button onclick={() => void interruptRun()}>Прервать исполнение</Button>
          {/if}
        </span>
        {#if interruptError}<p class="note" role="alert">{interruptError}</p>{/if}
      {/if}
      {#if unconfirmed(op)}
        <p class="note">Отправка не подтверждена, повторяю</p>
        <span class="actions"><Button onclick={() => retryUnknown(input.key)}>Повторить сейчас</Button></span>
      {/if}
    </div>
  {/each}
  {#each shownScheduled as item (item.id)}
    <div class="item">
      <span class="pill">
        <span class="label">{atOf(item)}</span>
        <span class="text" title={textOf(item)}>{textOf(item)}</span>
        {#if notes[item.id] !== 'Уже отправлено'}
          <IconButton icon="edit" label="Изменить: {short(textOf(item))}" onclick={() => openEdit(item.id, textOf(item), item.run_at)} />
          {#if intents.busy(`scheduled-cancel:${item.id}`)}
            <IconButton icon="close" label="Отменяется…" inert={true} />
          {:else}
            <IconButton icon="close" label="Отменить: {short(textOf(item))}" onclick={() => cancelScheduled(item.id)} />
          {/if}
        {/if}
      </span>
      {#if notes[item.id]}<p class="note">{notes[item.id]}</p>{/if}
    </div>
  {/each}
  {#each shownFailed as item (item.id)}
    <div class="item">
      <span class="pill failed">
        <span class="label danger">Не отправлено</span>
        <span class="text" title={textOf(item)}>{textOf(item)}</span>
      </span>
      <span class="actions"><Button onclick={() => takeBackFailed(item)}>Вернуть в черновик</Button></span>
    </div>
  {/each}
  {#if hidden > 0}
    <button type="button" class="more" onclick={() => { expanded = true; }}>Ещё {hidden} в очереди</button>
  {:else if expanded && total > COLLAPSE_ABOVE}
    <button type="button" class="more" onclick={() => { expanded = false; }}>Свернуть</button>
  {/if}
</div>

{#if editId !== null}
  <Sheet open title="Изменить отправку" onclose={() => (editId = null)}>
    <div class="edit">
      <Field label="Текст">
        {#snippet children({ id })}<textarea {id} bind:value={editText} aria-label="Текст отложенного сообщения"></textarea>{/snippet}
      </Field>
      <Field label="Время">
        {#snippet children({ id })}<input {id} type="datetime-local" bind:value={editAt} aria-label="Время отложенного сообщения" />{/snippet}
      </Field>
      {#if editError}<p class="note" role="alert">{editError}</p>{/if}
      {#if savingScheduled}
        <Button variant="primary" full disabled={true} reason="Сохраняется…">Сохранить</Button>
      {:else}
        <Button variant="primary" full onclick={saveEdit}>Сохранить</Button>
      {/if}
    </div>
  </Sheet>
{/if}

<style>
  .strip { display: flex; flex-direction: column; gap: var(--s2); padding-inline: var(--gutter); }
  .item { display: flex; flex-direction: column; gap: var(--s2); min-width: 0; }
  .pill { display: flex; align-items: center; gap: var(--s4); min-width: 0; min-height: var(--target); padding: var(--s0) var(--s1) var(--s0) var(--s6); border-radius: var(--radius-pill); background: var(--surface); font-size: var(--fs-sm); }
  .failed { background: var(--danger-soft); }
  .label { flex: none; color: var(--secondary); }
  .label.accent { color: var(--accent-link); }
  .label.danger { color: var(--danger); }
  .text { flex: 1; min-width: 0; overflow: hidden; color: var(--text); text-overflow: ellipsis; white-space: nowrap; }
  .pill :global(button) { color: var(--secondary); }
  .note { margin: 0; padding-inline: var(--s6); color: var(--muted); font-size: var(--fs-sm); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s2); padding-inline: var(--s2); }
  .files { display: flex; flex-wrap: wrap; gap: var(--s2); margin: 0; padding: 0; list-style: none; }
  .edit-file { display: flex; align-items: center; gap: var(--s2); }
  .more { align-self: flex-start; min-height: var(--target); padding-inline: var(--s6); background: var(--transparent); color: var(--accent-link); font-size: var(--fs-sm); }
  .edit { display: flex; flex-direction: column; gap: var(--s6); }
</style>
