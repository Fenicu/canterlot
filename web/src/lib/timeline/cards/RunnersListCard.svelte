<script lang="ts">
  import { resolve } from '$app/paths';
  import CardShell from './CardShell.svelte';
  import type { RunnerRow, RunnersListData } from './defs/jobs';
  import type { CardProps } from './types';

  let { call, data, line, word, env }: CardProps<RunnersListData> = $props();
  const SHOWN = 6;
  const shown = $derived(data.rows.slice(0, SHOWN));

  /** Доступность словами хаба; пауза владельца важнее: на неё задачи не попадут, как бы раннер ни был на связи. */
  function state(row: RunnerRow): { text: string; tone: 'success' | 'danger' | 'warning' | 'muted' } {
    if (row.paused) return { text: 'пауза', tone: 'warning' };
    if (row.availability === 'online') return { text: 'на связи', tone: 'success' };
    if (row.availability === 'offline') return { text: 'недоступен', tone: 'danger' };
    return { text: 'неизвестно', tone: 'muted' };
  }
</script>

<!-- Раннеры несущей: имя, доступность и занятые места из всех. -->
<CardShell {call} {env} {line} status={word?.word ?? ''} tone={word?.tone ?? 'muted'}>
  {#if shown.length > 0}
    <ul class="list" data-role="runners-list">
      {#each shown as row (row.name)}
        {@const now = state(row)}
        <li data-runner-row={row.name}>
          <span class="name">{row.name}</span>
          <span class="slots" data-role="runner-slots">{row.slotsUsed}/{row.slotsMax}<span class="sr-only"> мест занято</span></span>
          <span class="word" data-tone={now.tone} data-role="runner-state">{now.text}</span>
        </li>
      {/each}
      {#if data.rows.length > SHOWN}<li class="more">ещё {data.rows.length - SHOWN}</li>{/if}
    </ul>
    <a class="open" href={resolve('/machines')}>Открыть машины</a>
  {/if}
</CardShell>

<style>
  .list { display: flex; flex-direction: column; gap: var(--s1); margin: var(--s0); padding: var(--s0); list-style: none; }
  li { display: flex; align-items: center; gap: var(--s3); min-width: 0; min-height: var(--target); }
  .name { flex: 0 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .slots { flex: 1 1 0; min-width: 0; color: var(--muted); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .word { flex: none; margin-inline-start: auto; color: var(--secondary); white-space: nowrap; }
  .word[data-tone='success'] { color: var(--success); }
  .word[data-tone='warning'] { color: var(--warning); }
  .word[data-tone='danger'] { color: var(--danger); }
  .word[data-tone='muted'], .more { color: var(--muted); }
  .open { align-self: flex-start; display: inline-flex; align-items: center; min-height: var(--target); color: var(--accent-link); text-decoration: none; }
  .open:hover, .open:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
</style>
