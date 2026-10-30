<script lang="ts">
  import type { BlockModel } from '#lib/stores/blocks.js';
  import Icon from '#lib/ui/Icon.svelte';
  let { block }: { block: BlockModel } = $props();
  const title = $derived(block.kind === 'error' ? 'Ошибка' : 'Предупреждение');
</script>

<aside class="notice" data-kind={block.kind}>
  <p class="title"><Icon name="warning" />{title}</p>
  <p>{block.text || block.incompleteReason || title}</p>
  {#if block.state === 'incomplete' && block.incompleteReason}
    <p>Неполный ответ: {block.incompleteReason}</p>
  {/if}
</aside>

<style>
  .notice { display: flex; flex-direction: column; gap: var(--s2); padding: var(--s4) var(--s6); border-radius: var(--radius-sm); background: var(--warning-soft); }
  .notice[data-kind='error'] { background: var(--danger-soft); }
  .title { display: flex; align-items: center; gap: var(--s3); color: var(--warning); font-size: var(--fs-sm); font-weight: var(--weight-semibold); }
  .title :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  [data-kind='error'] .title { color: var(--danger); }
  p { margin: 0; overflow-wrap: anywhere; }
</style>
