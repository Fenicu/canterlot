<script lang="ts">
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import { entryMeta, type MemoryWriteData } from './defs/memory';
  import MemoryBody from './MemoryBody.svelte';
  import type { CardProps } from './types';

  let { call, data, line, link, word, grows, env }: CardProps<MemoryWriteData> = $props();
  const meta = $derived(data.refusal ? '' : entryMeta({ ...data, version: null }));
</script>

<!-- Запись в память: строка называет тему, под ней вид и уровень; отказ сказан словами в самой строке. -->
<CardShell {call} {env} {line} {grows} status={word?.word ?? ''} tone={word?.tone ?? 'muted'} reveal="записанное">
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if meta}<p class="meta" data-role="memory-meta">{meta}</p>{/if}
  {#snippet fold()}
    {#if data.refusal}<p class="subject" data-role="memory-subject">{data.subject}</p>{/if}
    {#if data.body}<MemoryBody body={data.body} cut={data.bodyCut} folds={env.folds} foldKey={`memory:${call.block.id}`} />{/if}
  {/snippet}
</CardShell>

<style>
  .meta { margin: var(--s0); color: var(--secondary); overflow-wrap: anywhere; }
  .subject { margin: var(--s0); color: var(--text); overflow-wrap: anywhere; }
</style>
