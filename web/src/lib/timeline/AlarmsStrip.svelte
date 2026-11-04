<script lang="ts">
  import { deleteAlarm } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import type { AlarmView, ChatStore } from '#lib/stores/chat.svelte.js';
  import { alarmChip, alarmDetail } from '#lib/timeline/alarm.js';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';

  let { chat }: { chat: ChatStore } = $props();

  const visible = $derived(
    chat.alarms.filter((alarm) => alarm.state === 'armed' || alarm.state === 'failed')
  );
  let expanded = $state<string | null>(null);
  let busy = $state<string | null>(null);
  let error = $state('');
  let asking = $state<AlarmView | null>(null);

  function toggle(id: string) {
    expanded = expanded === id ? null : id;
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
    {#each visible as alarm (alarm.alarm_id)}
      <div class="item">
        <div class="pill" class:failed={alarm.state === 'failed'} data-alarm-state={alarm.state}>
          <button
            type="button"
            class="chip"
            aria-expanded={expanded === alarm.alarm_id}
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
        {#if expanded === alarm.alarm_id}
          {#each alarmDetail(alarm) as line, index (index)}
            <p class="note">{line}</p>
          {/each}
        {/if}
      </div>
    {/each}
    {#if error}<p class="note" role="alert">{error}</p>{/if}
  </div>
{/if}

<style>
  .strip { display: flex; flex-direction: column; gap: var(--s2); padding-inline: var(--gutter); }
  .item { display: flex; flex-direction: column; gap: var(--s2); min-width: 0; }
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
