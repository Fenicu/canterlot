<script lang="ts">
  import CardShell from './CardShell.svelte';
  import type { ReadData } from './defs/explore';
  import { fileDir, fileName } from './defs/files';
  import type { CardProps } from './types';

  let { call, data, line, word, grows, quiet, env }: CardProps<ReadData> = $props();
</script>

<!-- Чтение файла — бледная строка: имя файла, папка и окно строк; раскрытие показывает прочитанное. -->
<CardShell {call} {env} {line} {grows} {quiet} status={word?.word ?? ''} tone={word?.tone ?? 'muted'} reveal="прочитанное">
  {#snippet head()}
    <span class="name" data-role="read-name">{fileName(data.path)}</span>
    {#if fileDir(data.path)}<span class="dir" data-role="read-dir">{fileDir(data.path)}</span>{/if}
    {#if data.range}<span class="range" data-role="read-range">{data.range}</span>{/if}
  {/snippet}
</CardShell>

<style>
  .name { flex: 0 1 auto; min-width: 0; overflow: hidden; color: var(--text); text-overflow: ellipsis; white-space: nowrap; }
  .dir { flex: 1 1 auto; min-width: 0; overflow: hidden; color: var(--muted); text-overflow: ellipsis; white-space: nowrap; }
  .range { flex: none; margin-inline-start: auto; color: var(--muted); white-space: nowrap; }
</style>
