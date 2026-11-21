<script lang="ts">
  import { resolve } from '$app/paths';
  import { durationWord, jobHref, jobLabel, jobWord } from '#lib/jobs/model.js';
  import CardShell from './CardShell.svelte';
  import type { JobListData } from './defs/jobs';
  import type { CardProps } from './types';

  let { call, data, line, word, env }: CardProps<JobListData> = $props();
  const SHOWN = 5;
  const shown = $derived(data.rows.slice(0, SHOWN));
</script>

<!-- Список задач: каждая строка ведёт в лист задачи. -->
<CardShell {call} {env} {line} status={word?.word ?? ''} tone={word?.tone ?? 'muted'}>
  {#if shown.length > 0}
    <ul class="list" data-role="jobs-list">
      {#each shown as row (row.jobId)}
        {@const state = jobWord({ state: row.state, exitCode: row.exitCode, cancelRequested: false })}
        {@const took = durationWord(row.durationMs)}
        <li>
          <a href={resolve(jobHref(row.jobId) as '/')}>{jobLabel(row.title, row.shell) || row.jobId.slice(0, 8)}</a>
          <span class="meta">{[row.machine, took].filter(Boolean).join(' · ')}</span>
          <span class="word" data-tone={state.tone}>{state.word}</span>
        </li>
      {/each}
      {#if data.rows.length > SHOWN}<li class="more">ещё {data.rows.length - SHOWN}</li>{/if}
    </ul>
  {/if}
</CardShell>

<style>
  .list { display: flex; flex-direction: column; gap: var(--s1); margin: var(--s0); padding: var(--s0); list-style: none; }
  li { display: flex; align-items: center; gap: var(--s3); min-width: 0; min-height: var(--target); }
  a { flex: 0 1 auto; min-width: 0; overflow: hidden; color: var(--accent-link); text-decoration: none; text-overflow: ellipsis; white-space: nowrap; }
  .meta { flex: 1 1 0; min-width: 0; overflow: hidden; color: var(--muted); text-overflow: ellipsis; white-space: nowrap; }
  .word { flex: none; margin-inline-start: auto; color: var(--secondary); white-space: nowrap; }
  .word[data-tone='accent'] { color: var(--accent-link); }
  .word[data-tone='success'] { color: var(--success); }
  .word[data-tone='warning'] { color: var(--warning); }
  .word[data-tone='danger'] { color: var(--danger); }
  .word[data-tone='muted'], .more { color: var(--muted); }
</style>
