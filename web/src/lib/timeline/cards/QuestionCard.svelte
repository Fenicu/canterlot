<script lang="ts">
  import CardShell from './CardShell.svelte';
  import type { QuestionData } from './defs/tools';
  import type { CardProps } from './types';

  let { call, data, line, word, env }: CardProps<QuestionData> = $props();
  const answered = $derived(data.questions.filter((item) => item.answer));
</script>

<!-- Вопрос без своей карточки запроса: в истории ответ владельца живёт только в ответе инструмента. -->
<CardShell {call} {env} {line} status={word?.word ?? (answered.length > 0 ? 'отвечено' : '')} tone={word?.tone ?? 'success'}>
  {#if answered.length > 0}
    <dl class="answers">
      {#each answered as item (item.question)}
        <div><dt>{item.question}</dt><dd>{item.answer}</dd></div>
      {/each}
    </dl>
  {/if}
</CardShell>

<style>
  .answers { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); }
  dt { color: var(--secondary); overflow-wrap: anywhere; }
  dd { margin: var(--s0); color: var(--text); overflow-wrap: anywhere; }
</style>
