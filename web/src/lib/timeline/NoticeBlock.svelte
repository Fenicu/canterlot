<script lang="ts">
  import type { BlockModel } from '#lib/stores/blocks.js';
  let { block }: { block: BlockModel } = $props();
  const title = $derived(block.kind === 'error' ? 'Ошибка' : 'Предупреждение');
</script>

<aside class="notice" data-kind={block.kind}>
  <p class="title">{title}</p>
  <p>{block.text || block.incompleteReason || title}</p>
  {#if block.state === 'incomplete' && block.incompleteReason}
    <p>Неполный ответ: {block.incompleteReason}</p>
  {/if}
</aside>

<style>
  .notice { display: flex; flex-direction: column; gap: var(--s2); }
  .title { margin: 0; color: var(--danger, var(--text)); }
  p { margin: 0; }
</style>
