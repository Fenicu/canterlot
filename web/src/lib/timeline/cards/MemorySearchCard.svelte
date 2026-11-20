<script lang="ts">
  import { resolve } from '$app/paths';
  import { fmt } from '#lib/format/ru.js';
  import CardShell from './CardShell.svelte';
  import { kindWord, levelWord, memoryHref, type MemorySearchData } from './defs/memory';
  import type { CardProps } from './types';

  let { call, data, line, word, grows, env }: CardProps<MemorySearchData> = $props();
  const status = $derived(word?.word ?? '');
</script>

<!-- Поиск по памяти: строка называет запрос и итог, раскрытие — найденные записи; каждая ведёт на экран «Память». -->
<CardShell {call} {env} {line} {grows} {status} tone={word?.tone ?? 'muted'} reveal="найденное">
  {#snippet fold()}
    {#if data.results.length > 0}
      <ul class="rows" data-role="memory-rows">
        {#each data.results as row (row.id)}
          <li data-expired={row.expired ? '' : undefined}>
            <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
            <a class="row" href={resolve(memoryHref(row.id) as '/')} data-memory-row={row.id}>
              <span class="subject"><span class="kind">{kindWord(row.kind)}</span> {row.subject}</span>
              <span class="meta">
                <span>{[levelWord(row.level, row.projectLabel), row.updatedAt !== null ? fmt.shortDate(row.updatedAt) : ''].filter(Boolean).join(' · ')}</span>
                {#if row.expired}<span class="stale" data-role="memory-stale">устарела</span>{/if}
              </span>
            </a>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="none">{data.refusal ? line : 'Подходящих записей нет'}</p>
    {/if}
  {/snippet}
</CardShell>

<style>
  .rows { display: flex; flex-direction: column; margin: var(--s0); padding: var(--s0); list-style: none; }
  .row { display: flex; flex-direction: column; justify-content: center; gap: var(--s1); min-height: var(--target); min-width: 0; padding-block: var(--s2); color: inherit; text-decoration: none; }
  .subject { min-width: 0; color: var(--text); overflow-wrap: anywhere; }
  .kind { color: var(--secondary); }
  .meta { display: flex; flex-wrap: wrap; gap: var(--s0) var(--s3); color: var(--muted); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .stale { color: var(--warning); }
  li[data-expired] .subject, li[data-expired] .kind { color: var(--muted); }
  .row:hover .subject, .row:focus-visible .subject { text-decoration: underline; text-underline-offset: var(--s1); }
  .none { margin: var(--s0); color: var(--muted); }
</style>
