<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { ColorKey } from './types';
  let { color = 'neutral', children }: { color?: ColorKey; children: Snippet } = $props();
</script>
<span data-color={color}>{@render children()}</span>
<style>
  span { display: inline-flex; align-items: center; gap: var(--s3); padding: var(--s2) var(--s4); border-radius: var(--radius-pill); background: var(--chip); color: var(--chip-text); font-size: var(--fs-xs); }
  [data-color=accent] { background: var(--accent-soft); color: var(--accent-link); }
  [data-color=blue] { background: var(--surface); color: var(--blue); }
  [data-color=amber] { background: var(--warning-soft); color: var(--warning); }
  [data-color=success] { background: var(--surface); color: var(--success); }
  [data-color=danger] { background: var(--danger-soft); color: var(--danger); }
</style>
