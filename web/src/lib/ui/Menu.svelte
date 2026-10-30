<script lang="ts">
  import type { Snippet } from 'svelte';
  let { label, children }: { label: string; children: Snippet } = $props();
</script>
<div class="menu" role="menu" aria-label={label}>{@render children()}</div>
<style>
  .menu { display: flex; flex-direction: column; min-width: var(--preview-height); max-width: 100%; padding: var(--s3); border-radius: var(--radius-card); background: var(--popover); color: var(--text); animation: menu-enter var(--duration-fast) var(--ease); transform-origin: top right; }
  @keyframes menu-enter { from { opacity: 0; transform: scale(var(--scale-press)); } to { opacity: 1; transform: none; } }
</style>
