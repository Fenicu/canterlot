<script lang="ts">
  import Icon from './Icon.svelte';
  import type { IconName } from './icons';
  import type { MouseEventHandler } from 'svelte/elements';
  let { icon, label, onclick }: { icon: IconName; label: string; onclick?: MouseEventHandler<HTMLButtonElement> } = $props();
</script>
<button type="button" aria-label={label} {onclick}><Icon name={icon} /></button>
<style>
  button { min-width: var(--target); min-height: var(--target); display: inline-flex; align-items: center; justify-content: center; border: var(--s0); background: var(--transparent); padding: var(--s0); flex-shrink: 0; border-radius: var(--radius-round); }
</style>
