<script lang="ts">
  import type { Snippet } from 'svelte';
  let { title, subtitle, leading, actions }: { title: string; subtitle?: string; leading?: Snippet; actions?: Snippet } = $props();
</script>
<header class="topbar">{@render leading?.()}<div><h1>{title}</h1>{#if subtitle}<p>{subtitle}</p>{/if}</div>{@render actions?.()}</header>
<style>
  header { display: flex; align-items: center; gap: var(--s3); width: calc(100% - 2 * var(--s8) - 2 * var(--icon-hang)); max-width: calc(var(--reading-max) - 2 * var(--icon-hang)); padding-block: var(--s4); margin: var(--s5) auto var(--s3); flex-shrink: 0; }
  div { flex: 1; min-width: 0; }
  h1 { font-size: var(--fs-title); letter-spacing: var(--tracking-normal); overflow-wrap: anywhere; }
  p { color: var(--muted); font-size: var(--fs-xs); margin-top: var(--s2); }
</style>
