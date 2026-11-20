<script lang="ts">
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import { entryMeta, type MemoryGetData } from './defs/memory';
  import MemoryBody from './MemoryBody.svelte';
  import type { CardProps } from './types';

  let { call, data, line, link, word, grows, env }: CardProps<MemoryGetData> = $props();
  const meta = $derived(entryMeta(data));
</script>

<!-- Чтение записи памяти: строка называет тему, раскрытие показывает прочитанный текст. -->
<CardShell {call} {env} {line} {grows} status={word?.word ?? ''} tone={word?.tone ?? 'muted'} reveal="прочитанное">
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#snippet fold()}
    {#if meta}<p class="meta" data-role="memory-meta">{meta}</p>{/if}
    {#if data.body}<MemoryBody body={data.body} folds={env.folds} foldKey={`memory:${call.block.id}`} />{/if}
  {/snippet}
</CardShell>

<style>
  .meta { margin: var(--s0); color: var(--secondary); font-size: var(--fs-sm); overflow-wrap: anywhere; }
</style>
