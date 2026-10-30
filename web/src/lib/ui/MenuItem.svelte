<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  import type { IconName } from './icons';
  let { icon, href, danger = false, onclick, children }: { icon?: IconName; href?: string; danger?: boolean; onclick?: () => void; children: Snippet } = $props();
</script>
{#if href}
  <!-- Адрес уже приведён к базовому пути вызывающей стороной. -->
  <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
  <a role="menuitem" class="item" class:danger {href} {onclick}>{#if icon}<Icon name={icon} />{/if}<span>{@render children()}</span></a>
{:else}
  <button type="button" role="menuitem" class="item" class:danger {onclick}>{#if icon}<Icon name={icon} />{/if}<span>{@render children()}</span></button>
{/if}
<style>
  .item { display: flex; align-items: center; gap: var(--s5); min-height: var(--target); width: 100%; padding: var(--s2) var(--s5); border: var(--s0); border-radius: var(--radius-sm); background: var(--transparent); color: var(--text); font-size: var(--fs-body); text-align: left; }
  @media (hover: hover) and (pointer: fine) { .item:hover { background: var(--popover-hover); } }
  .danger { color: var(--danger); }
</style>
