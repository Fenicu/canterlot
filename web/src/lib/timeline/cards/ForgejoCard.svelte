<script lang="ts">
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import { CI_WORD, type ForgejoData } from './defs/forgejo';
  import type { CardProps } from './types';

  let { call, data, line, link, word, grows, quiet, env }: CardProps<ForgejoData> = $props();
  const SHOWN = 5;
  const key = $derived(`forgejo-more:${call.block.id}`);
  const all = $derived(env.folds.isOpen(key));
  const rows = $derived(all ? data.rows : data.rows.slice(0, SHOWN));
</script>

<!-- Запуски и задания CI: состояние цветным словом, имя, ветка и длительность; имя ведёт в запуск. -->
<!-- Адрес запуска внешний: маршрутизатор в нём не участвует. -->
<!-- eslint-disable svelte/no-navigation-without-resolve -->
<CardShell {call} {env} {line} {grows} {quiet} status={word?.word ?? ''} tone={word?.tone ?? 'muted'}>
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if data.error}<p class="danger" data-role="forgejo-error">{data.error}</p>{/if}
  {#if rows.length > 0}
    <ul class="rows" data-role="forgejo-rows">
      {#each rows as row, index (index)}
        <li class="row" data-status={row.status}>
          <span class="status" data-tone={CI_WORD[row.status].tone} data-role="ci-status">{CI_WORD[row.status].word || row.raw || '—'}</span>
          <span class="name">{#if row.url}<a href={row.url} target="_blank" rel="noopener noreferrer">{row.name}{#if row.number}&nbsp;{row.number}{/if}</a>{:else}{row.name}{#if row.number}&nbsp;{row.number}{/if}{/if}</span>
          {#if row.ref}<span class="ref" data-role="ci-ref">{row.ref}</span>{/if}
          {#if row.took}<span class="took" data-role="ci-took">{row.took}</span>{/if}
        </li>
      {/each}
    </ul>
    {#if data.rows.length > SHOWN}
      <button type="button" class="more" aria-expanded={all} onclick={() => env.folds.toggle(key)}>{all ? 'Свернуть' : `ещё ${data.rows.length - SHOWN}`}</button>
    {/if}
  {/if}
</CardShell>
<!-- eslint-enable svelte/no-navigation-without-resolve -->

<style>
  p { margin: var(--s0); }
  .danger { color: var(--danger); overflow-wrap: anywhere; }
  .rows { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); padding: var(--s0); list-style: none; }
  .row { display: flex; align-items: baseline; gap: var(--s4); min-width: 0; }
  .status { flex: none; color: var(--secondary); }
  .status[data-tone='success'] { color: var(--success); }
  .status[data-tone='danger'] { color: var(--danger); }
  .status[data-tone='accent'] { color: var(--accent-link); }
  .status[data-tone='warning'] { color: var(--warning); }
  .status[data-tone='muted'] { color: var(--muted); }
  .name { flex: 0 1 auto; min-width: 0; overflow: hidden; color: var(--text); text-overflow: ellipsis; white-space: nowrap; }
  .name a { color: var(--accent-link); text-decoration: none; }
  .name a:hover, .name a:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
  .ref { flex: 1 1 auto; min-width: 0; overflow: hidden; color: var(--muted); font-family: var(--font-mono); font-size: var(--fs-sm); text-overflow: ellipsis; white-space: nowrap; }
  .took { flex: none; margin-inline-start: auto; color: var(--secondary); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .more { align-self: flex-start; min-width: var(--target); min-height: var(--target); margin-block: calc(var(--s2) * -1); padding: var(--s0); background: var(--transparent); color: var(--accent-link); }
</style>
