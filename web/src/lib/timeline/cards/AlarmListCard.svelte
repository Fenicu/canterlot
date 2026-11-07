<script lang="ts">
  import CardShell from './CardShell.svelte';
  import { alarmListLine, type AlarmListData } from './defs/alarms';
  import type { CardProps } from './types';

  let { call, data, line, word, env }: CardProps<AlarmListData> = $props();
  const armed = $derived(data.alarms.filter((alarm) => alarm.state === 'armed').slice(0, 5));
</script>

<CardShell {call} {env} {line} status={word?.word ?? ''} tone={word?.tone ?? 'muted'}>
  {#if armed.length > 0}
    <ul class="list">
      {#each armed as alarm (alarm.alarm_id)}<li>{alarmListLine(call, alarm)}</li>{/each}
    </ul>
  {/if}
</CardShell>

<style>
  .list { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); padding: var(--s0); color: var(--secondary); list-style: none; }
  li { overflow-wrap: anywhere; }
</style>
