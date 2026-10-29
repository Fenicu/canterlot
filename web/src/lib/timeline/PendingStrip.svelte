<script lang="ts">
  import { ApiError } from '$lib/api/http';
  import { deleteScheduled, interrupt, patchScheduled } from '$lib/api/client';
  import { getRuntime } from '$lib/app/runtime';
  import { cancelInput } from '$lib/timeline/QueueActions';
  import { CANCEL_REQUESTED_LABEL } from '$lib/stores/inputs';
  import type { ChatStore } from '$lib/stores/chat.svelte';
  import { deliveryLabel, scheduledLabel } from '$lib/timeline/present';
  import Sheet from '$lib/ui/Sheet.svelte';

  let { chat }: { chat: ChatStore } = $props();
  const items = $derived(chat.strip());
  const scheduled = $derived(
    chat.scheduled as {
      id?: string;
      run_at?: string;
      send_at?: string;
      text?: string;
      state?: string;
      content?: { type: string; text?: string }[];
    }[]
  );
  let lost = $state<Record<string, boolean>>({});
  let notes = $state<Record<string, string>>({});
  let editId = $state<string | null>(null);
  let editText = $state('');
  let editAt = $state('');

  function textOf(item: (typeof scheduled)[number]): string {
    if (item.text) return item.text;
    return (item.content ?? [])
      .filter((block) => block.type === 'text')
      .map((block) => block.text ?? '')
      .join('');
  }

  function atOf(item: (typeof scheduled)[number]): string {
    const raw = item.run_at ?? item.send_at ?? '';
    const parsed = Date.parse(raw);
    return Number.isFinite(parsed) ? scheduledLabel(parsed) : 'Отправится позже';
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
  {#each items as input (input.key)}
    {@const op = getRuntime().journal.ops.get(input.key)}
    <div data-state={input.state}>
      <p>{deliveryLabel(input.state)} {input.text}</p>
      {#if input.state === 'queued' || input.state === 'on_machine'}
        <button type="button" onclick={() => cancel(input.key)}>Отменить</button>
      {/if}
      {#if input.cancelRequested}<p>{CANCEL_REQUESTED_LABEL}</p>{/if}
      {#if lost[input.key]}
        <p>Передача уже началась, обычная отмена невозможна</p>
        <button type="button" onclick={() => interrupt(chat.id, crypto.randomUUID())}>Прервать исполнение</button>
      {/if}
      {#if op?.state === 'unknown'}
        <p>Отправка не подтверждена, повторяю</p>
        <button type="button" onclick={() => retryUnknown(input.key)}>Повторить сейчас</button>
      {/if}
    </div>
  {/each}
  {#each scheduled as item, index (item.id ?? index)}
    <div>
      <p>{atOf(item)} {textOf(item)}</p>
      {#if item.id && notes[item.id]}
        <p>{notes[item.id]}</p>
      {:else if item.id}
        <button type="button" onclick={() => openEdit(item.id ?? '', textOf(item), item.run_at ?? item.send_at ?? '')}>Изменить</button>
        <button type="button" onclick={() => cancelScheduled(item.id ?? '')}>Отменить</button>
      {/if}
    </div>
  {/each}
</div>

{#if editId !== null}
  <Sheet open title="Изменить отправку" onclose={() => (editId = null)}>
    <label>Текст <textarea bind:value={editText} aria-label="Текст отложенного сообщения"></textarea></label>
    <label>Время <input type="datetime-local" bind:value={editAt} aria-label="Время отложенного сообщения" /></label>
    <button type="button" onclick={saveEdit}>Сохранить</button>
  </Sheet>
{/if}

<style>
  .strip { display: flex; flex-direction: column; gap: var(--s2); }
  p { margin: 0; }
  button { min-height: var(--target); border: var(--s0); background: var(--transparent); color: var(--secondary); font-size: var(--fs-meta); }
  textarea, input { min-height: var(--target); width: 100%; border: var(--s0); background: var(--bg); color: var(--text); }
</style>
