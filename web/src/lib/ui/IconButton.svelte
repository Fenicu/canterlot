<script lang="ts">
  import Icon from './Icon.svelte';
  import type { IconName } from './icons';
  import type { MouseEventHandler } from 'svelte/elements';
  let { icon, label, onclick, inert = false, filled = false, expanded }: { icon: IconName; label: string; onclick?: MouseEventHandler<HTMLButtonElement>; inert?: boolean; filled?: boolean; expanded?: boolean } = $props();
  // Кнопка остаётся в порядке табуляции: неактивное действие объясняет себя подписью, а не исчезает с клавиатуры.
  const state = $derived(inert ? { 'aria-disabled': true } : {});
</script>
<button type="button" aria-label={label} aria-expanded={expanded} {...state} class:filled onclick={inert ? undefined : onclick}><Icon name={icon} /></button>
<style>
  button { min-width: var(--target); min-height: var(--target); display: inline-flex; align-items: center; justify-content: center; border: var(--s0); background: var(--transparent); color: inherit; padding: var(--s0); flex-shrink: 0; border-radius: var(--radius-round); }
  .filled { background: var(--chat-bg); }
  button[aria-disabled='true'] { background: var(--transparent); color: var(--muted); cursor: default; }
</style>
