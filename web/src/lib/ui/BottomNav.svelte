<script lang="ts">
  import { resolve } from '$app/paths';
  let { items, label = 'Разделы' }: { items: { href: string; label: string; current?: boolean }[]; label?: string } = $props();
</script>
<nav aria-label={label}>{#each items as item (item.href)}<a href={resolve(item.href as '/')} aria-current={item.current ? 'page' : undefined}>{item.label}</a>{/each}</nav>
<style>
  nav { display: flex; justify-content: space-around; margin: var(--s4) var(--s6) var(--s6); padding: var(--s2); border-radius: var(--radius-pill); background: var(--raised); flex-shrink: 0; }
  a { display: flex; align-items: center; justify-content: center; min-height: var(--target); min-width: var(--target); padding: var(--s4) var(--s8); font-size: var(--fs-sm); color: var(--muted); border-radius: var(--radius-pill); overflow-wrap: anywhere; }
  a[aria-current=page] { color: var(--accent-link); background: var(--selected); }
</style>
