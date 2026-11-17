<script lang="ts">
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import type { MeetingData } from './defs/meeting';
  import type { CardProps } from './types';

  let { call, data, line, link, word, grows, quiet, env }: CardProps<MeetingData> = $props();
</script>

<!-- Встреча: что искали и сколько нашлось, и до трёх строк превью; полный ответ — в раскрытии. -->
<CardShell {call} {env} {line} {grows} {quiet} status={word?.word ?? ''} tone={word?.tone ?? 'muted'} reveal="ответ">
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if data.error}<p class="danger" data-role="meeting-error">{data.error}</p>{/if}
  {#if data.preview.length > 0}
    <ul class="rows" data-role="meeting-preview">
      {#each data.preview as row, index (index)}
        <li class="row">{#if row.lead}<span class="lead">{row.lead}</span>{/if}<span class="text">{row.text}</span></li>
      {/each}
    </ul>
  {/if}
  {#if data.note}<p class="note">{data.note}</p>{/if}
</CardShell>

<style>
  p { margin: var(--s0); }
  .danger { color: var(--danger); overflow-wrap: anywhere; }
  .note { color: var(--warning); }
  .rows { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); padding: var(--s0); list-style: none; }
  .row { display: flex; align-items: baseline; gap: var(--s4); min-width: 0; }
  .lead { flex: none; color: var(--muted); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .text { min-width: 0; overflow: hidden; color: var(--secondary); overflow-wrap: anywhere; display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; }
</style>
