<script lang="ts">
  import type { BlockModel } from '#lib/stores/blocks.js';
  import TextBlock from '#lib/timeline/TextBlock.svelte';
  import Icon from '#lib/ui/Icon.svelte';

  let { block, open, onToggle }: { block: BlockModel; open: boolean; onToggle: () => void } = $props();
</script>

<div class="reasoning" data-open={open ? '' : undefined}>
  <button type="button" class="toggle" aria-expanded={open} onclick={onToggle}>
    <span class="chevron"><Icon name="chevron" /></span>
    <span>Рассуждение</span>
  </button>
  {#if open}
    <div class="fold"><div class="body"><TextBlock {block} /></div></div>
  {/if}
</div>

<style>
  .reasoning { display: flex; flex-direction: column; min-width: 0; margin-block: calc(var(--s2) * -1); color: var(--secondary); }
  .toggle { display: flex; align-items: center; gap: var(--s3); width: fit-content; min-height: var(--target); padding: var(--s0) var(--s4); border-radius: var(--radius-pill); background: var(--transparent); color: var(--secondary); font-size: var(--fs-sm); }
  .chevron { color: var(--muted); transition: transform var(--duration-expand) var(--ease-expand); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  [data-open] .chevron { transform: rotate(90deg); }
  .fold { display: grid; grid-template-rows: 1fr; animation: fold var(--duration-expand) var(--ease-expand); }
  .body { min-height: 0; overflow: hidden; font-size: var(--fs-meta); }
  @keyframes fold { from { grid-template-rows: 0fr; } to { grid-template-rows: 1fr; } }
</style>
