<script lang="ts">
  import { untrack } from 'svelte';
  import { deleteAlarm } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import type { AlarmView, ChatStore } from '#lib/stores/chat.svelte.js';
  import { alarmChip, alarmDetail, alarmSummary } from '#lib/timeline/alarm.js';
  import { readAlarmsOpen, writeAlarmsOpen } from '#lib/timeline/alarmsOpen.js';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';

  let { chat }: { chat: ChatStore } = $props();

  const visible = $derived(
    chat.alarms.filter((alarm) => alarm.state === 'armed' || alarm.state === 'failed')
  );
  const listId = $props.id();
  let detail = $state<string | null>(null);
  let busy = $state<string | null>(null);
  let error = $state('');
  let asking = $state<AlarmView | null>(null);
  // Выбор владельца живёт отдельно от числа будильников: новое событие не раскрывает полосу.
  let chosen = $state<boolean | null>(untrack(() => readAlarmsOpen(chat.id)));
  const collapsed = $derived(visible.length >= 2 && chosen !== true);
  const summary = $derived(alarmSummary(visible));

  function toggleList() {
    chosen = collapsed;
    writeAlarmsOpen(chat.id, chosen);
  }

  function toggle(id: string) {
    detail = detail === id ? null : id;
  }

  async function cancel(alarm: AlarmView) {
    asking = null;
    busy = alarm.alarm_id;
    error = '';
    try {
      const response = await deleteAlarm(chat.id, alarm.alarm_id, crypto.randomUUID());
      const updated = response.data;
      chat.alarms = chat.alarms
        .map((item) => (item.alarm_id === updated.alarm_id ? updated : item))
        .filter((item) => item.state !== 'cancelled');
    } catch (err) {
      error = errorText(err, 'Не удалось отменить будильник');
    } finally {
      busy = null;
    }
  }
</script>

{#if asking}
  <ConfirmDialog
    title="Отменить будильник?"
    body={alarmChip(asking)}
    confirmLabel="Отменить"
    danger
    onconfirm={() => void cancel(asking!)}
    onclose={() => (asking = null)}
  />
{/if}

{#if visible.length > 0}
  <div class="strip" role="region" aria-label="Будильники">
    {#if visible.length >= 2}
      <button
        type="button"
        class="summary"
        aria-expanded={!collapsed}
        aria-controls={listId}
        onclick={toggleList}
      >
        <span class="summary-text">{summary}</span>
        <span class="chevron" class:open={!collapsed}><Icon name="chevron" /></span>
      </button>
    {/if}
    {#if !collapsed}
      <div class="list" id={listId}>
        {#each visible as alarm (alarm.alarm_id)}
          <div class="item">
            <div class="pill" class:failed={alarm.state === 'failed'} data-alarm-state={alarm.state}>
              <button
                type="button"
                class="chip"
                aria-expanded={detail === alarm.alarm_id}
                onclick={() => toggle(alarm.alarm_id)}
              >{alarmChip(alarm)}</button>
              {#if alarm.state === 'armed'}
                <IconButton
                  icon="close"
                  label="Отменить: {alarmChip(alarm)}"
                  inert={busy === alarm.alarm_id}
                  onclick={() => (asking = alarm)}
                />
              {/if}
            </div>
            {#if detail === alarm.alarm_id}
              {#each alarmDetail(alarm) as line, index (index)}
                <p class="note">{line}</p>
              {/each}
            {/if}
          </div>
        {/each}
      </div>
    {/if}
    {#if error}<p class="note" role="alert">{error}</p>{/if}
  </div>
{/if}

<style>
  .strip { display: flex; flex-direction: column; gap: var(--s3); padding: var(--s3) var(--gutter); }
  .summary {
    display: flex;
    align-items: center;
    gap: var(--s4);
    width: 100%;
    min-height: var(--target);
    padding: var(--s0) var(--s6);
    border-radius: var(--radius-pill);
    background: var(--surface);
    color: var(--text);
    font: inherit;
    font-size: var(--fs-sm);
    text-align: start;
  }
  .summary-text { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chevron {
    flex: none;
    display: grid;
    place-items: center;
    color: var(--muted);
    transition: transform var(--duration-fast) var(--ease);
  }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .chevron.open { transform: rotate(90deg); }
  .list { display: flex; flex-direction: column; gap: var(--s3); }
  .item { display: flex; flex-direction: column; gap: var(--s3); min-width: 0; }
  .pill {
    display: flex;
    align-items: center;
    gap: var(--s4);
    min-width: 0;
    min-height: var(--target);
    padding: var(--s0) var(--s1) var(--s0) var(--s6);
    border-radius: var(--radius-pill);
    background: var(--surface);
    font-size: var(--fs-sm);
  }
  .failed { background: var(--danger-soft); }
  .chip {
    flex: 1;
    min-width: 0;
    padding: 0;
    border: 0;
    background: none;
    color: var(--text);
    font: inherit;
    text-align: start;
    text-overflow: ellipsis;
    white-space: nowrap;
    overflow: hidden;
  }
  .note { margin: 0; padding-inline: var(--s6); color: var(--muted); font-size: var(--fs-sm); }
</style>
