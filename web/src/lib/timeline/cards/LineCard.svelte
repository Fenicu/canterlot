<script lang="ts" generics="D">
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import type { CardProps } from './types';

  let { call, line, link, word, grows, quiet, env }: CardProps<D> = $props();
</script>

<!-- Карточка в одну строку: сводка, ссылка в ней и слово состояния; раскрытие показывает сырой вызов. -->
<CardShell {call} {env} {line} {grows} {quiet} status={word?.word ?? ''} tone={word?.tone ?? 'muted'}>
  {#snippet head()}<CardLine {line} {link} />{/snippet}
</CardShell>
