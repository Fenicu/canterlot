<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import { CHANGE_WORD, levelWord, kindWord, type MemoryUpdateData } from './defs/memory';
  import MemoryBody from './MemoryBody.svelte';
  import type { CardProps } from './types';

  let { call, data, line, link, word, grows, env }: CardProps<MemoryUpdateData> = $props();
  // Что поменялось, словами: «тема, текст»; новые значения короткие поля называют сами.
  const summary = $derived(data.changes.map((change) => CHANGE_WORD[change]).join(', '));
  const details = $derived(
    [
      data.changes.includes('subject') ? `Тема: ${data.subject}` : '',
      data.changes.includes('kind') ? `Вид: ${kindWord(data.kind)}` : '',
      data.changes.includes('level') ? `Уровень: ${levelWord(data.level, '')}` : '',
      data.changes.includes('expiry') ? (data.expiryCleared ? 'Срок снят' : data.validUntil !== null ? `Срок: до ${fmt.shortDate(data.validUntil)}` : '') : ''
    ].filter(Boolean)
  );
</script>

<!-- Правка записи памяти: строка называет запись и версии, раскрытие — что изменилось и новый текст. -->
<CardShell {call} {env} {line} {grows} status={word?.word ?? ''} tone={word?.tone ?? 'muted'} reveal="изменения">
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if summary && !data.refusal}<p class="meta" data-role="memory-changes">Изменено: {summary}</p>{/if}
  {#snippet fold()}
    {#if details.length > 0}
      <ul class="details" data-role="memory-details">{#each details as item (item)}<li>{item}</li>{/each}</ul>
    {/if}
    {#if data.changes.includes('text') && data.body}<MemoryBody body={data.body} cut={data.bodyCut} folds={env.folds} foldKey={`memory:${call.block.id}`} />{/if}
  {/snippet}
</CardShell>

<style>
  .meta { margin: var(--s0); color: var(--secondary); overflow-wrap: anywhere; }
  .details { display: flex; flex-direction: column; gap: var(--s1); margin: var(--s0); padding: var(--s0); color: var(--secondary); list-style: none; }
  li { overflow-wrap: anywhere; }
</style>
