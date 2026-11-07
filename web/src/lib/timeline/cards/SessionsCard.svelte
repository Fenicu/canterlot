<script lang="ts">
  import { resolve } from '$app/paths';
  import CardShell from './CardShell.svelte';
  import { sessionWord } from './copy';
  import type { ListData } from './defs/sessions';
  import type { CardProps } from './types';

  let { call, data, line, word, env }: CardProps<ListData> = $props();
  const SHOWN = 4;
  // Сначала занятые и требующие внимания: ждущие сообщения сессии в конце.
  const order = ['waiting_for_user', 'stalled', 'failed', 'quota_paused', 'working', 'idle', 'stopped'];
  const rows = $derived([...data.sessions].sort((a, b) => order.indexOf(a.state) - order.indexOf(b.state)));
  const shown = $derived(rows.slice(0, SHOWN));
</script>

<CardShell {call} {env} {line} status={word?.word ?? ''} tone={word?.tone ?? 'muted'}>
  {#if shown.length > 0}
    <ul class="list">
      {#each shown as row (row.id)}
        {@const state = sessionWord(row.state)}
        <li><a href={resolve(`/c/${row.id}` as '/')}>«{call.session(row.id)?.title || row.title || row.id.slice(0, 8)}»</a><span class="word" data-tone={state.tone}>{state.word}</span></li>
      {/each}
      {#if rows.length > SHOWN}<li class="more">ещё {rows.length - SHOWN}</li>{/if}
    </ul>
  {/if}
</CardShell>

<style>
  .list { display: flex; flex-wrap: wrap; gap: var(--s0) var(--s6); margin: var(--s0); padding: var(--s0); list-style: none; }
  li { display: inline-flex; align-items: center; gap: var(--s3); min-width: 0; max-width: 100%; }
  a { display: inline-flex; align-items: center; min-width: 0; min-height: var(--target); overflow: hidden; color: var(--accent-link); text-decoration: none; text-overflow: ellipsis; white-space: nowrap; }
  .word { flex: none; color: var(--secondary); white-space: nowrap; }
  .word[data-tone='accent'] { color: var(--accent-link); }
  .word[data-tone='success'] { color: var(--success); }
  .word[data-tone='warning'] { color: var(--warning); }
  .word[data-tone='danger'] { color: var(--danger); }
  .word[data-tone='muted'], .more { color: var(--muted); }
</style>
