<script lang="ts">
  import CardShell from './CardShell.svelte';
  import { foundWord, type FindData } from './defs/explore';
  import type { CardProps } from './types';

  let { call, data, line, word, grows, quiet, env }: CardProps<FindData> = $props();
  const found = $derived(foundWord(data));
</script>

<!-- Поиск по коду — бледная строка: шаблон моноширинным, где искали и сколько нашлось. -->
<CardShell {call} {env} {line} {grows} {quiet} status={word?.word ?? ''} tone={word?.tone ?? 'muted'} reveal="найденное">
  {#snippet head()}
    <span class="kind">{data.kind === 'list' ? 'Список' : 'Поиск'}</span>
    {#if data.pattern}<code class="pattern" data-role="find-pattern">{data.pattern}</code>{/if}
    {#if data.where}<span class="where" data-role="find-where">{data.kind === 'list' ? data.where : `в ${data.where}`}</span>{/if}
    {#if found}<span class="found" data-role="find-count">{found}</span>{/if}
  {/snippet}
</CardShell>

<style>
  .kind { flex: none; color: var(--muted); }
  .pattern { flex: 0 1 auto; min-width: 0; overflow: hidden; color: var(--text); font-family: var(--font-mono); font-size: var(--fs-sm); text-overflow: ellipsis; white-space: nowrap; }
  .where { flex: 1 1 auto; min-width: 0; overflow: hidden; color: var(--muted); text-overflow: ellipsis; white-space: nowrap; }
  .found { flex: none; margin-inline-start: auto; color: var(--secondary); white-space: nowrap; }
</style>
