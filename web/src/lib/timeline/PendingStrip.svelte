<script lang="ts">
  import { ApiError } from '#lib/api/http.js';
  import { deleteScheduled, interrupt, patchScheduled } from '#lib/api/client.js';
  import { getRuntime } from '#lib/app/runtime.js';
  import { returnToComposer } from '#lib/composer/returned.js';
  import { cancelInput } from '#lib/timeline/QueueActions.js';
  import { CANCEL_REQUESTED_LABEL } from '#lib/stores/inputs.js';
  import type { ChatStore, ScheduledMessage } from '#lib/stores/chat.svelte.js';
  import { deliveryLabel, scheduledLabel } from '#lib/timeline/present.js';
  import Button from '#lib/ui/Button.svelte';
  import Field from '#lib/ui/Field.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';

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

  async function cancel(key: string) {
    const input = chat.inputs.get(key);
    if (!input) return;
    const outcome = await cancelInput(input);
    if (outcome === 'lost') lost = { ...lost, [key]: true };
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
    try {
      await patchScheduled(
        id,
        {
          content: [{ type: 'text', text: editText }],
          run_at: Number.isFinite(at) ? new Date(at).toISOString() : undefined
        },
        crypto.randomUUID()
      );
      editId = null;
    } catch (err) {
      if (err instanceof ApiError && err.code === 'not_pending') notes = { ...notes, [id]: 'Уже отправлено' };
    }
  }

  async function cancelScheduled(id: string) {
    try {
      await deleteScheduled(id, crypto.randomUUID());
    } catch (err) {
      if (err instanceof ApiError && err.code === 'not_pending') notes = { ...notes, [id]: 'Уже отправлено' };
    }
  }

  function retryUnknown(key: string) {
    getRuntime().journal.retryNow(key);
  }
</script>

<div class="strip">
  {#each shownItems as input (input.key)}
    {@const op = getRuntime().journal.ops.get(input.key)}
    <div class="item" data-state={input.state}>
      <span class="pill">
        <span class="label accent">{deliveryLabel(input.state)}</span>
        <span class="text">{input.text}</span>
        {#if input.state === 'queued' || input.state === 'on_machine'}
          <IconButton icon="close" label="Отменить" onclick={() => cancel(input.key)} />
        {/if}
      </span>
      {#if input.cancelRequested}<p class="note">{CANCEL_REQUESTED_LABEL}</p>{/if}
      {#if lost[input.key]}
        <p class="note">Передача уже началась, обычная отмена невозможна</p>
        <span class="actions"><Button onclick={() => interrupt(chat.id, crypto.randomUUID())}>Прервать исполнение</Button></span>
      {/if}
      {#if op?.state === 'unknown'}
        <p class="note">Отправка не подтверждена, повторяю</p>
        <span class="actions"><Button onclick={() => retryUnknown(input.key)}>Повторить сейчас</Button></span>
      {/if}
    </div>
  {/each}
  {#each shownScheduled as item (item.id)}
    <div class="item">
      <span class="pill">
        <span class="label">{atOf(item)}</span>
        <span class="text">{textOf(item)}</span>
        {#if !notes[item.id]}
          <IconButton icon="edit" label="Изменить" onclick={() => openEdit(item.id, textOf(item), item.run_at)} />
          <IconButton icon="close" label="Отменить" onclick={() => cancelScheduled(item.id)} />
        {/if}
      </span>
      {#if notes[item.id]}<p class="note">{notes[item.id]}</p>{/if}
    </div>
  {/each}
  {#each shownFailed as item (item.id)}
    <div class="item">
      <span class="pill failed">
        <span class="label danger">Не отправлено</span>
        <span class="text">{textOf(item)}</span>
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
      <Button variant="primary" full onclick={saveEdit}>Сохранить</Button>
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
  .more { align-self: flex-start; min-height: var(--target); padding-inline: var(--s6); background: var(--transparent); color: var(--accent-link); font-size: var(--fs-sm); }
  .edit { display: flex; flex-direction: column; gap: var(--s6); }
</style>
