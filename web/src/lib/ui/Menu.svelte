<script lang="ts">
  import type { Snippet } from 'svelte';
  import { leave } from './exit';
  let { label, children }: { label: string; children: Snippet } = $props();
</script>
<div class="menu" role="menu" aria-label={label} out:leave|global>{@render children()}</div>
<style>
  .menu { display: flex; flex-direction: column; min-width: var(--preview-height); max-width: 100%; padding: var(--s3); border-radius: var(--radius-card); background: var(--popover); color: var(--text); animation: menu-enter var(--duration-fast) var(--ease); }
  /* Уход зеркалит вход: меню поднимается и гаснет за то же время и по кривой ухода (`ui/exit.ts`). Меню уже закрыто, уходящее ничего не принимает. */
  .menu:global([data-leaving]) { animation: none; opacity: 0; transform: translateY(calc(var(--shift-label) * -1)); pointer-events: none; transition: opacity var(--duration-fast) var(--ease-exit), transform var(--duration-fast) var(--ease-exit); }
  @keyframes menu-enter { from { opacity: 0; transform: translateY(calc(var(--shift-label) * -1)); } to { opacity: 1; transform: none; } }
</style>
