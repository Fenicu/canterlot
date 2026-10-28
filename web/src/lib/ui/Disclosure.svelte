<script lang="ts">
  import type { Snippet } from 'svelte';
  let { open = $bindable(false), title, children }: { open?: boolean; title: string; children: Snippet } = $props();
</script>
<details bind:open><summary>{title}</summary><div>{@render children()}</div></details>
<style>
  details { background: var(--surface); border-radius: var(--radius-card); padding: var(--s2) var(--s6); }
  summary { min-height: var(--target); display: flex; align-items: center; font-size: var(--fs-meta); border-radius: var(--radius-pill); }
  div { padding-block: var(--s4) var(--s6); }
</style>
