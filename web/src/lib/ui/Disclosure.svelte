<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  import type { IconName } from './icons';
  let { open = $bindable(false), title, icon, flat = false, children }: { open?: boolean; title: string; icon?: IconName; flat?: boolean; children: Snippet } = $props();
</script>
<details bind:open class:flat><summary>{#if icon}<Icon name={icon} />{/if}<span>{title}</span><span class="chevron"><Icon name="chevron" /></span></summary><div>{@render children()}</div></details>
<style>
  details { background: var(--surface); border-radius: var(--radius-card); padding: var(--s2) var(--s6); }
  .flat { background: var(--transparent); padding: var(--s0); }
  summary { min-height: var(--target); display: flex; align-items: center; gap: var(--s6); border-radius: var(--radius-pill); list-style: none; }
  summary::-webkit-details-marker { display: none; }
  summary::marker { content: ''; }
  summary > span:first-of-type { flex: 1; min-width: 0; }
  .chevron { color: var(--muted); transition: transform var(--duration-fast) var(--ease); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  details[open] .chevron { transform: rotate(90deg); }
  div { padding-block: var(--s4) var(--s6); }
</style>
